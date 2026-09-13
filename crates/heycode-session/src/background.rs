//! Owner-only local IPC for whole interactive sessions. A host keeps one PTY
//! and one runtime alive; connecting a terminal never opens the session log.
//! Stale host records are observations, never authority to signal a PID.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Maximum encoded IPC message, including bounded terminal input/output.
pub const MAX_FRAME: usize = 128 * 1024;
/// Child-only socket location. Ordinary terminal clients do not set this.
pub const HOST_ENV: &str = "HEYCODE_SESSION_HOST";
/// Child control credential, regenerated for each host process.
pub const TOKEN_ENV: &str = "HEYCODE_SESSION_HOST_TOKEN";

/// Explicit current controls carried to a new native conversation owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOptions {
    /// Active inference provider, rather than the host's original CLI override.
    pub provider: String,
    /// Active native model ID.
    pub model: String,
    /// Active built-in approval policy. Custom policies cannot be copied.
    pub approval: String,
}

/// Client or live TUI request. The host admits only one terminal at a time.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Inspect the live owner without attaching or opening the log.
    Status,
    /// Exclusively attach one terminal.
    Attach {
        /// Terminal rows.
        rows: u16,
        /// Terminal columns.
        cols: u16,
    },
    /// Input belongs to this attached connection; never replayed on reconnect.
    Input {
        /// Monotonically increasing connection-local input sequence.
        sequence: u64,
        /// Exact terminal bytes.
        bytes: Vec<u8>,
    },
    /// Resize the exclusively attached terminal.
    Resize {
        /// Rows.
        rows: u16,
        /// Columns.
        cols: u16,
    },
    /// Detach the current terminal, preserving the TUI and active work.
    Detach,
    /// Move the attached terminal to another live owner; this owner stays alive.
    Switch {
        /// Child control credential.
        token: String,
        /// Other live owner socket.
        socket: PathBuf,
    },
    /// Request orderly TUI exit. No timeout escalates to an unowned PID kill.
    Stop,
    /// Register the current durable session after composition/recomposition.
    Register {
        /// Child control credential.
        token: String,
        /// Current durable session ID.
        session_id: String,
        /// Actual composed workspace.
        cwd: PathBuf,
    },
    /// Child polls for orderly stop, without reading or answering approvals.
    Poll {
        /// Child control credential.
        token: String,
    },
    /// Start an already-created durable fork using the current host arguments.
    Fork {
        /// Child control credential.
        token: String,
        /// Newly created child session.
        session_id: String,
        /// Effective workspace for the fork.
        cwd: PathBuf,
        /// Current route and permission controls.
        options: ForkOptions,
    },
}

/// Live owner state. `stopped` is published only after its child is reaped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostStatus {
    /// Unique process-generation identity, not a PID.
    pub host_id: String,
    /// Registered durable session, absent during startup.
    pub session_id: Option<String>,
    /// Live host socket in the private per-user runtime directory.
    pub socket: PathBuf,
    /// Workspace of this composition.
    pub cwd: PathBuf,
    /// Whether one terminal currently owns input.
    pub attached: bool,
    /// An orderly stop has been requested.
    pub stopping: bool,
    /// Reaped child exit code; absent while running.
    pub exit_code: Option<u32>,
    /// Bounded startup failure, captured only before any interactive input.
    #[serde(default)]
    pub startup_error: Option<String>,
}

/// Bounded response or terminal event.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Response {
    /// Request succeeded, with no inferred model completion.
    Ok,
    /// Request was refused before admission.
    Error {
        /// Safe operational explanation.
        message: String,
    },
    /// Actual host state.
    Status {
        /// Current process observation.
        status: HostStatus,
    },
    /// Exact PTY output.
    Output {
        /// Output bytes.
        bytes: Vec<u8>,
    },
    /// Input written once to the PTY; model acceptance is separately durable.
    InputAccepted {
        /// Exact connection-local sequence.
        sequence: u64,
    },
    /// Terminal has detached; the session remains alive.
    Detached,
    /// Reconnect the terminal to another owner, preserving this session.
    Switch {
        /// Other live owner socket.
        socket: PathBuf,
    },
    /// TUI should follow its existing cancellation/shutdown path.
    StopRequested,
    /// Child exited and was reaped.
    Exited {
        /// Child exit status.
        code: u32,
    },
    /// Fork host was started; durable session remains independently resumable.
    ForkStarted {
        /// New host identity.
        host_id: String,
    },
}

/// Validate a canonical UUID before using it in a registry filename.
pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

/// Create/validate a private directory without following a final symlink.
pub fn private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "background directory must be owned by this user with mode 0700",
        ));
    }
    Ok(())
}

/// Short Unix-socket directory; independent of potentially long workspace paths.
pub fn socket_root() -> io::Result<PathBuf> {
    let root = PathBuf::from(format!("/tmp/dshx-bg-{}", nix::unistd::geteuid()));
    private_directory(&root)?;
    Ok(root)
}

/// Create and validate the registry under an already-existing heycode home.
pub fn registry(home: &Path) -> io::Result<PathBuf> {
    let root = home.join("background");
    private_directory(&root)?;
    Ok(root)
}

/// Atomically publish an owner-only JSON record and sync its parent directory.
pub fn write_private_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Read a bounded regular owner-only registry file.
pub fn read_private_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_FRAME as u64
    {
        return Err(io::Error::other("unsafe or oversized background record"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    serde_json::from_reader(file.take(MAX_FRAME as u64 + 1)).map_err(io::Error::other)
}

/// Authenticate the OS identity of a connected local process.
pub fn check_peer(stream: &UnixStream) -> io::Result<()> {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let uid = nix::unistd::getpeereid(stream).map_err(io::Error::from)?.0;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let uid = nix::unistd::Uid::from_raw(
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .map_err(io::Error::from)?
            .uid(),
    );
    if uid != nix::unistd::geteuid() {
        return Err(io::Error::other("background peer is not this user"));
    }
    Ok(())
}

/// Kernel-reported client process identity on the supported local host platforms.
/// Used together with the per-generation token for child-only control requests.
pub fn peer_pid(stream: &UnixStream) -> io::Result<Option<u32>> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .map_err(io::Error::from)?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let pid = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .map_err(io::Error::from)?
        .pid();
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android"
    ))]
    return Ok(u32::try_from(pid).ok());
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android"
    )))]
    {
        let _ = stream;
        Ok(None)
    }
}

/// Connect only to a canonical socket in this user's private runtime directory.
pub fn connect(socket: &Path) -> io::Result<UnixStream> {
    let root = socket_root()?;
    if socket.parent() != Some(root.as_path())
        || !socket
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(valid_id)
        || socket
            .extension()
            .is_none_or(|extension| extension != "sock")
    {
        return Err(io::Error::other("invalid local background socket"));
    }
    let stream = UnixStream::connect(socket)?;
    check_peer(&stream)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(stream)
}

/// Encode one length-prefixed bounded frame.
pub fn encode(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    if data.len() > MAX_FRAME {
        return Err(io::Error::other("background frame exceeds bound"));
    }
    let mut encoded = (data.len() as u32).to_be_bytes().to_vec();
    encoded.extend(data);
    Ok(encoded)
}

/// Read exactly one frame; oversized lengths are rejected before allocation.
pub fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> io::Result<T> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::other("background frame exceeds bound"));
    }
    let mut data = vec![0_u8; length];
    stream.read_exact(&mut data)?;
    serde_json::from_slice(&data).map_err(io::Error::other)
}

/// One bounded control exchange, independent of terminal attachment.
pub fn request(socket: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = connect(socket)?;
    if matches!(request, Request::Fork { .. }) {
        // A fork confirms bounded child composition; ordinary owner health
        // checks remain short and are serviced concurrently by the host.
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    }
    stream.write_all(&encode(request)?)?;
    let response: Response = read_frame(&mut stream)?;
    if let Response::Error { message } = response {
        return Err(io::Error::other(message));
    }
    Ok(response)
}

/// Child control connection provided by its process host.
pub fn child_connection() -> Option<(PathBuf, String)> {
    Some((
        std::env::var_os(HOST_ENV)?.into(),
        std::env::var(TOKEN_ENV).ok()?,
    ))
}

/// List observed live session hosts. Dead records are retained as crash evidence;
/// they never justify reconnect success or signalling their old process IDs.
pub fn live_hosts(home: &Path) -> io::Result<Vec<HostStatus>> {
    let root = registry(home)?;
    let mut hosts = Vec::new();
    for entry in fs::read_dir(root)?.take(4096) {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(record) = read_private_json::<HostStatus>(&path) else {
            continue;
        };
        if let Ok(Response::Status { status }) = request(&record.socket, &Request::Status)
            && status.host_id == record.host_id
            && status.exit_code.is_none()
        {
            hosts.push(status);
        }
    }
    hosts.sort_by(|left, right| left.host_id.cmp(&right.host_id));
    Ok(hosts)
}

/// Locate a live host by durable session or unique host identity.
pub fn find_host(home: &Path, id: &str) -> io::Result<Option<HostStatus>> {
    if !valid_id(id) {
        return Err(io::Error::other("expected a canonical session or host id"));
    }
    let matches: Vec<_> = live_hosts(home)?
        .into_iter()
        .filter(|host| host.host_id == id || host.session_id.as_deref() == Some(id))
        .collect();
    if matches.len() > 1 {
        return Err(io::Error::other(
            "multiple hosts claim this session; refusing ambiguous ownership",
        ));
    }
    Ok(matches.into_iter().next())
}
