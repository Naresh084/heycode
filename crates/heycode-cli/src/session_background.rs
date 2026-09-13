//! Unix whole-session PTY host. Only the child opens the durable session log.
//! The broker owns terminal attachment; losing a client never cancels a turn.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use heycode_session::background::{self as wire, HostStatus, Request, Response};
use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

const MAX_CONNECTIONS: usize = 16;
const MAX_PENDING_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Launch {
    args: Vec<String>,
    cwd: PathBuf,
    home: PathBuf,
    host_id: String,
    token: String,
    rows: u16,
    cols: u16,
}

fn dimensions(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(2, 500),
        cols: cols.clamp(2, 1000),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Handle private host dispatch or explicit `heycode sessions` operations before
/// normal argument parsing. These paths never compose an inference runtime.
pub(super) fn dispatch(args: &[String]) -> anyhow::Result<Option<i32>> {
    match args.first().map(String::as_str) {
        Some("__session-host") => {
            let [_, path] = args else {
                anyhow::bail!("invalid private session host invocation");
            };
            // This is a freshly spawned executable, before any runtime/threads.
            nix::unistd::setsid()?;
            let path = Path::new(path);
            let launch: Launch = wire::read_private_json(path)?;
            if !wire::valid_id(&launch.host_id)
                || !wire::valid_id(&launch.token)
                || path != wire::registry(&launch.home)?.join(format!("{}.launch", launch.host_id))
            {
                anyhow::bail!("invalid private session launch record");
            }
            fs::remove_file(path)?;
            host(launch)?;
            Ok(Some(0))
        }
        Some("sessions") => {
            let home = heycode_cli::heycode_home()?;
            fs::create_dir_all(&home)?;
            match &args[1..] {
                [] => print_hosts(&home)?,
                [operation] if operation == "list" => print_hosts(&home)?,
                [operation, id] if operation == "attach" => {
                    let status = wire::find_host(&home, id)?.ok_or_else(|| anyhow::anyhow!("no live session host; use --resume <session-id> to recover the durable conversation"))?;
                    return attach(&status.socket).map(Some);
                }
                [operation, id] if operation == "stop" => {
                    let status = wire::find_host(&home, id)?
                        .ok_or_else(|| anyhow::anyhow!("no live session host"))?;
                    wire::request(&status.socket, &Request::Stop)?;
                    let until = Instant::now() + Duration::from_secs(10);
                    loop {
                        match wire::request(&status.socket, &Request::Status) {
                            Ok(Response::Status { status }) if status.exit_code.is_some() => {
                                println!(
                                    "session stopped (exit {})",
                                    status.exit_code.unwrap_or(1)
                                );
                                break;
                            }
                            Err(_) => {
                                let record: HostStatus = wire::read_private_json(
                                    &wire::registry(&home)?
                                        .join(format!("{}.json", status.host_id)),
                                )?;
                                if record.exit_code.is_some() {
                                    println!("session stopped");
                                    break;
                                }
                                anyhow::bail!(
                                    "host disconnected; stop settlement is unconfirmed; inspect the durable session"
                                );
                            }
                            _ => {}
                        }
                        if Instant::now() >= until {
                            anyhow::bail!(
                                "stop requested; settlement is unconfirmed after 10 seconds; host remains owned"
                            );
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                _ => anyhow::bail!(
                    "usage: heycode sessions [list | attach <session-or-host-id> | stop <session-or-host-id>]"
                ),
            }
            Ok(Some(0))
        }
        _ => Ok(None),
    }
}

fn print_hosts(home: &Path) -> anyhow::Result<()> {
    let hosts = wire::live_hosts(home)?;
    if hosts.is_empty() {
        println!("No live whole-session hosts.");
    }
    for status in hosts {
        println!(
            "{}  {}  {}",
            status.session_id.as_deref().unwrap_or(&status.host_id),
            if status.stopping {
                "stopping"
            } else if status.attached {
                "attached"
            } else {
                "detached"
            },
            status.cwd.display()
        );
    }
    Ok(())
}

/// Start the broker for an interactive invocation; a live durable resume target
/// attaches to its existing owner before any new world is composed.
pub(super) fn interactive(args: &[String], cli: &super::Cli) -> anyhow::Result<Option<i32>> {
    if cli.no_background
        || wire::child_connection().is_some()
        || !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || cli.mode_run
        || cli.prompt.is_some()
        || cli.mode_setup
        || cli.mode_acp
        || cli.mode_app_server
        || cli.mode_doctor
        || cli.mode_mcp
        || cli.mode_plugin
        || cli.mode_config
        || cli.mode_release
    {
        return Ok(None);
    }
    let home = heycode_cli::heycode_home()?;
    fs::create_dir_all(&home)?;
    let target = cli.resume.clone().or_else(|| {
        cli.resume_latest
            .then(|| {
                heycode_cli::find_latest_session_in(
                    &home.join("sessions"),
                    &std::env::current_dir().ok()?,
                )
                .ok()
                .flatten()
            })
            .flatten()
    });
    if let Some(target) = target {
        let id = resume_target_id(&home, &target);
        if let Some(id) = id
            && let Some(status) = wire::find_host(&home, &id)?
        {
            return attach(&status.socket).map(Some);
        }
    }
    let (cols, rows) = crossterm::terminal::size().unwrap_or((100, 30));
    let status = launch_host(
        args.to_vec(),
        std::env::current_dir()?,
        home,
        rows,
        cols,
        None,
    )?;
    attach(&status.socket).map(Some)
}

fn resume_target_id(home: &Path, target: &Path) -> Option<String> {
    if wire::valid_id(&target.to_string_lossy()) && !target.exists() {
        return Some(target.to_string_lossy().into_owned());
    }
    let directory = if target.is_dir() {
        target
    } else {
        target.parent()?
    };
    let id = directory
        .file_name()?
        .to_str()
        .filter(|id| wire::valid_id(id))?;
    // A same-looking UUID from another store must not redirect a path resume
    // to this home's live owner.
    let expected = fs::canonicalize(home.join("sessions").join(id)).ok()?;
    (fs::canonicalize(directory).ok()? == expected).then(|| id.to_owned())
}

fn launch_host(
    args: Vec<String>,
    cwd: PathBuf,
    home: PathBuf,
    rows: u16,
    cols: u16,
    expected_session: Option<&str>,
) -> anyhow::Result<HostStatus> {
    let launch = Launch {
        args,
        cwd,
        home,
        host_id: uuid::Uuid::new_v4().to_string(),
        token: uuid::Uuid::new_v4().to_string(),
        rows,
        cols,
    };
    let root = wire::registry(&launch.home)?;
    let path = root.join(format!("{}.launch", launch.host_id));
    wire::write_private_json(&path, &launch)?;
    let mut child = match Command::new(std::env::current_exe()?)
        .arg("__session-host")
        .arg(&path)
        .env_remove(wire::HOST_ENV)
        .env_remove(wire::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
    };
    let socket = wire::socket_root()?.join(format!("{}.sock", launch.host_id));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Response::Status { status }) = wire::request(&socket, &Request::Status)
            && status.session_id.is_some()
            && status.exit_code.is_none()
        {
            if expected_session
                .is_some_and(|expected| status.session_id.as_deref() != Some(expected))
            {
                let _ = wire::request(&socket, &Request::Stop);
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                anyhow::bail!(
                    "background startup selected another session; stop requested for that host; the saved fork remains resumable"
                );
            }
            // Reap the broker if this launching process stays alive (forks).
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(status);
        }
        if let Some(status) = child.try_wait()? {
            let record = root.join(format!("{}.json", launch.host_id));
            let detail = wire::read_private_json::<HostStatus>(&record)
                .ok()
                .and_then(|status| status.startup_error)
                .unwrap_or_else(|| status.to_string());
            anyhow::bail!("session host failed to start: {detail}");
        }
        if Instant::now() >= until {
            // The direct Child is still ours and not reaped; no stored PID lookup.
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(path);
            anyhow::bail!("session host did not become ready");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// Streaming attachments retain partial frames across readiness notifications.
// A control request's short timeout is not a terminal-session deadline.
fn read_available_frames<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
    pending: &mut Vec<u8>,
) -> io::Result<(Vec<T>, bool)> {
    let mut messages = Vec::new();
    let mut buffer = [0_u8; 8192];
    for _ in 0..64 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok((messages, true)),
            Ok(count) => pending.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        while pending.len() >= 4 {
            let length =
                u32::from_be_bytes(pending[..4].try_into().map_err(io::Error::other)?) as usize;
            if length > wire::MAX_FRAME {
                return Err(io::Error::other("terminal frame exceeds bound"));
            }
            if pending.len() < length + 4 {
                break;
            }
            messages
                .push(serde_json::from_slice(&pending[4..length + 4]).map_err(io::Error::other)?);
            pending.drain(..length + 4);
        }
    }
    Ok((messages, false))
}

fn flush_pending(writer: &mut impl Write, pending: &mut VecDeque<u8>) -> io::Result<()> {
    while !pending.is_empty() {
        let (bytes, _) = pending.as_slices();
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                pending.drain(..count);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct PtyInput {
    owner: Instant,
    sequence: u64,
    bytes: Vec<u8>,
}

type InputReceipt = (Instant, u64, io::Result<()>);

fn write_pty_inputs(
    mut writer: impl Write,
    input_rx: mpsc::Receiver<PtyInput>,
    receipt_tx: mpsc::Sender<InputReceipt>,
) {
    let mut failure: Option<String> = None;
    for input in input_rx {
        let result = if let Some(message) = &failure {
            Err(io::Error::other(message.clone()))
        } else {
            writer.write_all(&input.bytes).and_then(|()| writer.flush())
        };
        if let Err(error) = &result {
            failure = Some(error.to_string());
        }
        // Once the writer fails, settle every queued/new owner with an
        // error too; reattachment must never wait for a lost receipt.
        if receipt_tx
            .send((input.owner, input.sequence, result))
            .is_err()
        {
            break;
        }
    }
}

struct Connection {
    stream: UnixStream,
    input: Vec<u8>,
    input_ended: bool,
    input_pending: usize,
    output: VecDeque<u8>,
    attached: bool,
    next_input: u64,
    close: bool,
    close_since: Option<Instant>,
    born: Instant,
    peer_pid: Option<u32>,
    pending_response: Option<mpsc::Receiver<Response>>,
}

impl Connection {
    fn send(&mut self, response: &Response) -> io::Result<()> {
        let bytes = wire::encode(response)?;
        if self.output.len() + bytes.len() > MAX_PENDING_OUTPUT {
            return Err(io::Error::other("terminal output consumer fell behind"));
        }
        self.output.extend(bytes);
        Ok(())
    }
    fn read(&mut self) -> io::Result<Vec<Request>> {
        if self.input_ended {
            return Ok(Vec::new());
        }
        let (messages, ended) = read_available_frames(&mut self.stream, &mut self.input)?;
        // EOF ends admission, not already decoded requests or their receipts.
        // A peer may half-close its write side while still reading responses.
        self.input_ended = ended;
        Ok(messages)
    }
    fn close_after_input_end(&mut self) {
        if self.input_ended && self.input_pending == 0 && self.pending_response.is_none() {
            self.close = true;
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        flush_pending(&mut self.stream, &mut self.output)
    }

    fn flush_or_expire(&mut self) -> bool {
        if !self.attached
            && !self.close
            && self.pending_response.is_none()
            && self.born.elapsed() > Duration::from_secs(3)
        {
            return false;
        }
        if self.close
            && self.close_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(2)
        {
            return false;
        }
        self.flush().is_ok() && !(self.close && self.output.is_empty())
    }
}

struct ChildOutputDrain {
    code: u32,
    deadline: Instant,
}

fn drain_child_output(
    receiver: &mpsc::Receiver<Vec<u8>>,
    screen: &mut vt100::Parser,
    connections: &mut [Connection],
) -> bool {
    for _ in 0..64 {
        let bytes = match receiver.try_recv() {
            Ok(bytes) => bytes,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => return true,
        };
        screen.process(&bytes);
        for connection in &mut *connections {
            if connection.attached
                && !connection.close
                && connection
                    .send(&Response::Output {
                        bytes: bytes.clone(),
                    })
                    .is_err()
            {
                connection.close = true;
                connection.output.clear();
            }
        }
    }
    false
}

fn finish_child_output(
    pending: &mut Option<ChildOutputDrain>,
    output_ended: bool,
    connections: &mut [Connection],
    now: Instant,
) -> bool {
    if !pending
        .as_ref()
        .is_some_and(|exit| output_ended || now >= exit.deadline)
    {
        return false;
    }
    if let Some(exit) = pending.take() {
        for connection in connections {
            let _ = connection.send(&Response::Exited { code: exit.code });
            connection.close = true;
        }
    }
    true
}

fn host(launch: Launch) -> anyhow::Result<()> {
    let socket = wire::socket_root()?.join(format!("{}.sock", launch.host_id));
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let record = wire::registry(&launch.home)?.join(format!("{}.json", launch.host_id));
    let mut status = HostStatus {
        host_id: launch.host_id.clone(),
        session_id: None,
        socket: socket.clone(),
        cwd: launch.cwd.clone(),
        attached: false,
        stopping: false,
        exit_code: None,
        startup_error: None,
    };
    wire::write_private_json(&record, &status)?;
    let pair = portable_pty::native_pty_system().openpty(dimensions(launch.rows, launch.cols))?;
    let mut command = CommandBuilder::new(std::env::current_exe()?);
    command.args(&launch.args);
    command.cwd(&launch.cwd);
    command.env(wire::HOST_ENV, &socket);
    command.env(wire::TOKEN_ENV, &launch.token);
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let writer = pair.master.take_writer()?;
    // A full child input buffer must never prevent draining its output or
    // serving detach/stop. The receipt is emitted only after the write succeeds.
    let (input_tx, input_rx) = mpsc::sync_channel::<PtyInput>(32);
    let (receipt_tx, receipt_rx) = mpsc::channel();
    let _input_thread = std::thread::spawn(move || write_pty_inputs(writer, input_rx, receipt_tx));
    let mut reader = pair.master.try_clone_reader()?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(32);
    let _output_thread = std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 || tx.send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut connections: Vec<Connection> = Vec::new();
    let initial_size = dimensions(launch.rows, launch.cols);
    let mut screen = vt100::Parser::new(initial_size.rows, initial_size.cols, 0);
    let mut restore_size: Option<(Instant, PtySize)> = None;
    let mut exit_at = None;
    let mut child_output_drain = None;
    let mut output_ended = false;
    loop {
        if let Ok((stream, _)) = listener.accept()
            && connections.len() < MAX_CONNECTIONS
            && wire::check_peer(&stream).is_ok()
        {
            stream.set_nonblocking(true)?;
            let peer_pid = wire::peer_pid(&stream)?;
            connections.push(Connection {
                stream,
                input: Vec::new(),
                input_ended: false,
                input_pending: 0,
                output: VecDeque::new(),
                attached: false,
                next_input: 1,
                close: false,
                close_since: None,
                born: Instant::now(),
                peer_pid,
                pending_response: None,
            });
        }
        if let Some((until, size)) = restore_size
            && Instant::now() >= until
        {
            pair.master.resize(size)?;
            screen.set_size(size.rows, size.cols);
            restore_size = None;
        }
        output_ended |= drain_child_output(&rx, &mut screen, &mut connections);
        for (owner, sequence, result) in receipt_rx.try_iter() {
            if let Some(connection) = connections
                .iter_mut()
                .find(|connection| connection.born == owner)
            {
                connection.input_pending = connection.input_pending.saturating_sub(1);
                let response = match result {
                    Ok(()) => Response::InputAccepted { sequence },
                    Err(error) => {
                        connection.close = true;
                        Response::Error {
                            message: format!(
                                "terminal input write failed: {error}; delivery may be partial; input was not replayed"
                            ),
                        }
                    }
                };
                if connection.send(&response).is_err() {
                    connection.close = true;
                }
                connection.close_after_input_end();
            }
        }
        let mut detach = false;
        let mut switch_to = None;
        for index in 0..connections.len() {
            if let Some(pending) = &connections[index].pending_response {
                let response = match pending.try_recv() {
                    Ok(response) => Some(response),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(Response::Error {
                        message: "background startup worker stopped; inspect the saved session before retrying".to_owned(),
                    }),
                };
                if let Some(response) = response {
                    let connection = &mut connections[index];
                    connection.pending_response = None;
                    if connection.send(&response).is_err() {
                        connection.output.clear();
                    }
                    connection.close = true;
                }
                continue;
            }
            let messages = match connections[index].read() {
                Ok(messages) => messages,
                Err(_) => {
                    connections[index].close = true;
                    connections[index].output.clear();
                    continue;
                }
            };
            for message in messages {
                let peer_pid = connections[index].peer_pid;
                let child_authorized = |token: &str| {
                    token == launch.token
                        && child.process_id().is_some_and(|pid| Some(pid) == peer_pid)
                };
                let result: anyhow::Result<Option<Response>> = (|| {
                    Ok(Some(match message {
                        Request::Status => Response::Status {
                            status: status.clone(),
                        },
                        Request::Attach { rows, cols } => {
                            if detach
                                || status.attached
                                || status.stopping
                                || status.exit_code.is_some()
                            {
                                anyhow::bail!("session already attached or stopping");
                            }
                            connections[index].attached = true;
                            status.attached = true;
                            connections[index].send(&Response::Ok)?;
                            // Reconstruct display state; never replay historical OSC
                            // clipboard writes, terminal queries, or arbitrary output.
                            let mut replay = Vec::new();
                            if screen.screen().alternate_screen() {
                                replay.extend_from_slice(b"\x1b[?1049h\x1b[?1004h");
                            }
                            replay.extend(screen.screen().contents_formatted());
                            replay.extend(screen.screen().input_mode_formatted());
                            for bytes in replay.chunks(8192) {
                                connections[index].send(&Response::Output {
                                    bytes: bytes.to_vec(),
                                })?;
                            }
                            let size = dimensions(rows, cols);
                            let interim =
                                dimensions(rows, if cols > 2 { cols - 1 } else { cols + 1 });
                            pair.master.resize(interim)?;
                            screen.set_size(interim.rows, interim.cols);
                            restore_size =
                                Some((Instant::now() + Duration::from_millis(160), size));
                            wire::write_private_json(&record, &status)?;
                            Response::Ok
                        }
                        Request::Input { sequence, bytes } => {
                            if !connections[index].attached
                                || connections[index].close
                                || detach
                                || status.stopping
                                || status.exit_code.is_some()
                            {
                                anyhow::bail!("terminal does not own input");
                            }
                            if sequence != connections[index].next_input || bytes.len() > 8192 {
                                anyhow::bail!(
                                    "invalid terminal input sequence or size; input was not replayed"
                                );
                            }
                            input_tx
                                .try_send(PtyInput {
                                    owner: connections[index].born,
                                    sequence,
                                    bytes,
                                })
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "terminal input queue unavailable; input was not admitted"
                                    )
                                })?;
                            connections[index].next_input += 1;
                            connections[index].input_pending += 1;
                            // Admission is separate from the writer's delivery receipt.
                            Response::Ok
                        }
                        Request::Resize { rows, cols } => {
                            if !connections[index].attached || connections[index].close || detach {
                                anyhow::bail!("terminal does not own resize");
                            }
                            let size = dimensions(rows, cols);
                            pair.master.resize(size)?;
                            screen.set_size(size.rows, size.cols);
                            restore_size = None;
                            Response::Ok
                        }
                        Request::Detach => {
                            detach = true;
                            Response::Ok
                        }
                        Request::Switch { token, socket } => {
                            if !child_authorized(&token)
                                || !status.attached
                                || socket == status.socket
                            {
                                anyhow::bail!("invalid terminal handoff");
                            }
                            match wire::request(&socket, &Request::Status)? {
                                Response::Status { status }
                                    if !status.attached
                                        && !status.stopping
                                        && status.exit_code.is_none() => {}
                                _ => {
                                    anyhow::bail!("target session is already attached or stopping")
                                }
                            }
                            switch_to = Some(socket);
                            detach = true;
                            Response::Ok
                        }
                        Request::Stop => {
                            status.stopping = true;
                            wire::write_private_json(&record, &status)?;
                            Response::Ok
                        }
                        Request::Register {
                            token,
                            session_id,
                            cwd,
                        } => {
                            if !child_authorized(&token) || !wire::valid_id(&session_id) {
                                anyhow::bail!("invalid session owner registration");
                            }
                            status.session_id = Some(session_id);
                            status.cwd = cwd;
                            wire::write_private_json(&record, &status)?;
                            Response::Ok
                        }
                        Request::Poll { token } => {
                            if !child_authorized(&token) {
                                anyhow::bail!("invalid session owner token");
                            }
                            if status.stopping {
                                Response::StopRequested
                            } else {
                                Response::Ok
                            }
                        }
                        Request::Fork {
                            token,
                            session_id,
                            cwd,
                            options,
                        } => {
                            if !child_authorized(&token)
                                || !wire::valid_id(&session_id)
                                || status.stopping
                                || connections
                                    .iter()
                                    .any(|connection| connection.pending_response.is_some())
                            {
                                anyhow::bail!("invalid background fork request");
                            }
                            if options.provider.is_empty()
                                || options.provider.len() > 128
                                || options.model.is_empty()
                                || options.model.len() > 4096
                                || !matches!(
                                    options.approval.as_str(),
                                    "ask"
                                        | "deny"
                                        | "plan"
                                        | "full_access"
                                        | "accepted_edits"
                                        | "auto"
                                )
                            {
                                anyhow::bail!("unsupported background fork controls");
                            }
                            let mut args = super::session_restart_args(
                                &launch.args,
                                &launch.home.join("sessions"),
                                &heycode_core::SessionId::from_raw(&session_id),
                            );
                            // A fork is an exact durable child, never the startup picker.
                            args.retain(|arg| arg != "--resume-picker");
                            args.extend([
                                "--provider".to_owned(),
                                options.provider,
                                "--model".to_owned(),
                                options.model,
                                "--approval".to_owned(),
                                options.approval,
                            ]);
                            let home = launch.home.clone();
                            let (rows, cols) = (launch.rows, launch.cols);
                            let (tx, rx) = mpsc::sync_channel(1);
                            // Startup can perform slow composition. The owner loop
                            // must keep servicing health checks, stop and detach.
                            std::thread::spawn(move || {
                                let response = match launch_host(
                                    args,
                                    cwd,
                                    home,
                                    rows,
                                    cols,
                                    Some(&session_id),
                                ) {
                                    Ok(fork) => Response::ForkStarted {
                                        host_id: fork.host_id,
                                    },
                                    Err(error) => Response::Error {
                                        message: error.to_string(),
                                    },
                                };
                                let _ = tx.send(response);
                            });
                            connections[index].pending_response = Some(rx);
                            return Ok(None);
                        }
                    }))
                })();
                let response = match result {
                    Ok(Some(response)) => response,
                    Ok(None) => continue,
                    Err(error) => Response::Error {
                        message: error.to_string(),
                    },
                };
                if connections[index].send(&response).is_err() {
                    connections[index].close = true;
                    connections[index].output.clear();
                }
                if !connections[index].attached {
                    connections[index].close = true;
                }
            }
            connections[index].close_after_input_end();
        }
        if detach {
            for connection in &mut connections {
                if connection.attached {
                    let response =
                        switch_to
                            .as_ref()
                            .map_or(Response::Detached, |socket| Response::Switch {
                                socket: socket.clone(),
                            });
                    let _ = connection.send(&response);
                    connection.attached = false;
                    connection.close = true;
                }
            }
        }
        if status.exit_code.is_none()
            && let Some(exit) = child.try_wait()?
        {
            status.exit_code = Some(exit.exit_code());
            if status.session_id.is_none() && !exit.success() {
                status.startup_error =
                    Some(screen.screen().contents().chars().take(4096).collect());
            }
            wire::write_private_json(&record, &status)?;
            // Process settlement can precede the reader's final PTY bytes.
            // A descendant retaining the slave must not keep this host alive.
            child_output_drain = Some(ChildOutputDrain {
                code: exit.exit_code(),
                deadline: Instant::now() + Duration::from_secs(2),
            });
        }
        if finish_child_output(
            &mut child_output_drain,
            output_ended,
            &mut connections,
            Instant::now(),
        ) {
            exit_at = Some(Instant::now() + Duration::from_secs(2));
        }
        connections.retain_mut(Connection::flush_or_expire);
        let attached = connections
            .iter()
            .any(|connection| connection.attached && !connection.close);
        if status.attached != attached {
            status.attached = attached;
            wire::write_private_json(&record, &status)?;
        }
        if exit_at.is_some_and(|until| connections.is_empty() || Instant::now() >= until) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(input_tx);
    drop(pair.master);
    drop(rx);
    // A misbehaving descendant may retain the PTY slave after the TUI exits.
    // Do not let its read thread keep this broker alive; process exit closes it.
    fs::remove_file(socket)?;
    Ok(())
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Stop terminal-generated input before restoring canonical echo.
        // This also runs when a socket/output operation fails.
        // Restore client terminal modes on detach as well as normal child exit.
        let reset = b"\x1b[?2004l\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[<u\x1b[?25h\x1b[0m\x1b[?1049l";
        // Bounded nonblocking output always yields to OS raw-mode restoration,
        // including when the original failure was terminal backpressure.
        let _ = heycode_tui::terminal::restore_terminal_output(reset);
        // Discard pending mouse/key protocol reports from this attachment while
        // raw mode is still owned; they must not become shell commands.
        let _ = nix::sys::termios::tcflush(std::io::stdin(), nix::sys::termios::FlushArg::TCIFLUSH);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn attach(socket: &Path) -> anyhow::Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("attaching requires an interactive terminal");
    }
    let mut stream = wire::connect(socket)?;
    let mut size = crossterm::terminal::size().unwrap_or((100, 30));
    stream.write_all(&wire::encode(&Request::Attach {
        rows: size.1,
        cols: size.0,
    })?)?;
    match wire::read_frame::<Response>(&mut stream)? {
        Response::Ok => {}
        Response::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("invalid terminal attachment response"),
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    stream.set_nonblocking(true)?;
    crossterm::terminal::enable_raw_mode()?;
    let guard = TerminalGuard;
    let mut terminal_output = fs::OpenOptions::new()
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open("/dev/tty")?;
    let mut pending_output = VecDeque::new();
    let mut ending = None;
    let mut ending_since = None;
    let mut received = Vec::new();
    let mut outgoing = VecDeque::new();
    let mut awaiting_input = None;
    let mut deferred_input = VecDeque::new();
    let mut detaching = false;
    let mut sequence = 1;
    let mut input = [0_u8; 4096];
    let mut stdin = std::io::stdin();
    loop {
        use nix::poll::{PollFd, PollFlags, poll};
        let (read_input, read_output) = {
            let mut fds = [
                PollFd::new(
                    stdin.as_fd(),
                    if !detaching && ending.is_none() {
                        PollFlags::POLLIN
                    } else {
                        PollFlags::empty()
                    },
                ),
                PollFd::new(
                    stream.as_fd(),
                    PollFlags::POLLIN
                        | if outgoing.is_empty() {
                            PollFlags::empty()
                        } else {
                            PollFlags::POLLOUT
                        },
                ),
                PollFd::new(
                    terminal_output.as_fd(),
                    if pending_output.is_empty() {
                        PollFlags::empty()
                    } else {
                        PollFlags::POLLOUT
                    },
                ),
            ];
            match poll(&mut fds, 100_u16) {
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
            (
                fds[0]
                    .revents()
                    .is_some_and(|flags| flags.intersects(PollFlags::POLLIN | PollFlags::POLLHUP)),
                fds[1]
                    .revents()
                    .is_some_and(|flags| flags.intersects(PollFlags::POLLIN | PollFlags::POLLHUP)),
            )
        };
        if read_output && ending.is_none() {
            let (responses, ended) = read_available_frames::<Response>(&mut stream, &mut received)
                .map_err(|error| anyhow::anyhow!("terminal attachment receive failed: {error}"))?;
            for response in responses {
                match response {
                    Response::Output { bytes } => {
                        if pending_output.len() + bytes.len() > MAX_PENDING_OUTPUT {
                            anyhow::bail!(
                                "terminal display consumer fell behind; session remains running; reattach to recover its screen"
                            );
                        }
                        pending_output.extend(bytes);
                    }
                    response @ (Response::Detached
                    | Response::Switch { .. }
                    | Response::Exited { .. }) => {
                        ending = Some(response);
                        ending_since = Some(Instant::now());
                    }
                    Response::Error { message } => anyhow::bail!(message),
                    Response::InputAccepted { sequence: accepted }
                        if awaiting_input == Some(accepted) =>
                    {
                        awaiting_input = None
                    }
                    _ => {}
                }
            }
            if ended && ending.is_none() {
                anyhow::bail!(
                    "terminal attachment disconnected; the session may still be running; use heycode sessions list"
                );
            }
        }
        if read_input && !detaching && ending.is_none() {
            let count = stdin.read(&mut input)?;
            if count == 0 {
                return Ok(0);
            }
            // Keep reading the host-owned detach chord even if the child is
            // not consuming input. Unadmitted keys are bounded independently.
            if input[..count].contains(&0x1d) {
                deferred_input.clear();
                outgoing.extend(wire::encode(&Request::Detach)?);
                detaching = true;
            } else {
                if deferred_input.len() + count > 64 * 1024 {
                    anyhow::bail!(
                        "terminal input buffer filled while the child was not reading; session remains running; unadmitted input was not replayed"
                    );
                }
                deferred_input.extend(&input[..count]);
            }
        }
        if awaiting_input.is_none() && !detaching && ending.is_none() && !deferred_input.is_empty()
        {
            let count = deferred_input.len().min(4096);
            let bytes = deferred_input.drain(..count).collect();
            outgoing.extend(wire::encode(&Request::Input { sequence, bytes })?);
            awaiting_input = Some(sequence);
            sequence += 1;
        }
        if let Ok(current) = crossterm::terminal::size()
            && size != current
        {
            size = current;
            // Coalesce resize storms while a previous frame is still queued.
            if outgoing.is_empty() {
                outgoing.extend(wire::encode(&Request::Resize {
                    rows: size.1,
                    cols: size.0,
                })?);
            } else {
                size = (0, 0);
            }
        }
        if ending.is_none() {
            flush_pending(&mut stream, &mut outgoing).map_err(|error| {
                anyhow::anyhow!("terminal attachment send failed: {error}; input was not replayed")
            })?;
        }
        flush_pending(&mut terminal_output, &mut pending_output).map_err(|error| {
            anyhow::anyhow!("terminal display write failed: {error}; session may still be running")
        })?;
        if ending.is_some() && pending_output.is_empty() {
            match ending.take() {
                Some(Response::Detached) => {
                    drop(guard);
                    let _ = heycode_tui::terminal::restore_terminal_output(
                        b"Session detached. Reattach with dshx sessions attach <session-id>.\n",
                    );
                    return Ok(0);
                }
                Some(Response::Switch { socket: target }) => {
                    drop(guard);
                    return match attach(&target) {
                        Ok(code) => Ok(code),
                        Err(error) => {
                            let _ = heycode_tui::terminal::restore_terminal_output(format!("Could not attach target: {error}; returning to previous session.\n").as_bytes());
                            attach(socket)
                        }
                    };
                }
                Some(Response::Exited { code }) => return Ok(code as i32),
                _ => {}
            }
        }
        if ending_since.is_some_and(|start| start.elapsed() > Duration::from_secs(2)) {
            anyhow::bail!(
                "terminal display did not drain during exit; some terminal output could not be displayed"
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn connected_attachment() -> (Connection, UnixStream) {
        let (server, peer) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        (
            Connection {
                stream: server,
                input: Vec::new(),
                input_ended: false,
                input_pending: 0,
                output: VecDeque::new(),
                attached: true,
                next_input: 1,
                close: false,
                close_since: None,
                born: Instant::now(),
                peer_pid: Some(std::process::id()),
                pending_response: None,
            },
            peer,
        )
    }

    #[test]
    fn half_closed_input_returns_complete_requests_once_and_retains_receipts() {
        let (mut connection, mut peer) = connected_attachment();
        peer.write_all(
            &wire::encode(&Request::Input {
                sequence: 1,
                bytes: b"one input".to_vec(),
            })
            .unwrap(),
        )
        .unwrap();
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        let requests = connection.read().unwrap();
        assert!(
            matches!(&requests[..], [Request::Input { sequence: 1, bytes }] if bytes == b"one input")
        );
        assert!(connection.input_ended);
        assert!(
            !connection.close,
            "decoded input must remain eligible for admission"
        );
        assert!(
            connection.read().unwrap().is_empty(),
            "EOF never replays decoded input"
        );
        connection.input_pending = 1;
        connection.close_after_input_end();
        assert!(!connection.close, "delivery receipt must survive input EOF");
        connection
            .send(&Response::InputAccepted { sequence: 1 })
            .unwrap();
        connection.input_pending -= 1;
        connection.close_after_input_end();
        assert!(!connection.flush_or_expire());
        assert!(matches!(
            wire::read_frame::<Response>(&mut peer).unwrap(),
            Response::InputAccepted { sequence: 1 }
        ));
    }

    #[test]
    fn child_exit_waits_for_late_output_and_queues_exit_after_every_byte() {
        let (connection, mut peer) = connected_attachment();
        let mut connections = vec![connection];
        let (sender, receiver) = mpsc::channel();
        let mut screen = vt100::Parser::new(24, 80, 0);
        let now = Instant::now();
        let mut exit = Some(ChildOutputDrain {
            code: 7,
            deadline: now + Duration::from_secs(2),
        });
        sender.send(b"first".to_vec()).unwrap();
        assert!(!drain_child_output(
            &receiver,
            &mut screen,
            &mut connections
        ));
        assert!(!finish_child_output(
            &mut exit,
            false,
            &mut connections,
            now
        ));
        assert!(!connections[0].close);
        sender.send(b"last".to_vec()).unwrap();
        drop(sender);
        assert!(drain_child_output(&receiver, &mut screen, &mut connections));
        assert!(finish_child_output(&mut exit, true, &mut connections, now));
        assert!(
            !finish_child_output(&mut exit, true, &mut connections, now),
            "exit is delivered once"
        );
        assert!(!connections[0].flush_or_expire());
        assert!(
            matches!(wire::read_frame::<Response>(&mut peer).unwrap(), Response::Output { bytes } if bytes == b"first")
        );
        assert!(
            matches!(wire::read_frame::<Response>(&mut peer).unwrap(), Response::Output { bytes } if bytes == b"last")
        );
        assert!(matches!(
            wire::read_frame::<Response>(&mut peer).unwrap(),
            Response::Exited { code: 7 }
        ));
    }

    #[test]
    fn child_exit_drain_is_bounded_when_a_descendant_retains_the_pty() {
        let (connection, mut peer) = connected_attachment();
        let mut connections = vec![connection];
        let (_retained_sender, receiver) = mpsc::channel();
        let mut screen = vt100::Parser::new(24, 80, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut exit = Some(ChildOutputDrain { code: 0, deadline });
        assert!(!drain_child_output(
            &receiver,
            &mut screen,
            &mut connections
        ));
        assert!(!finish_child_output(
            &mut exit,
            false,
            &mut connections,
            deadline - Duration::from_millis(1)
        ));
        assert!(finish_child_output(
            &mut exit,
            false,
            &mut connections,
            deadline
        ));
        assert!(!connections[0].flush_or_expire());
        assert!(matches!(
            wire::read_frame::<Response>(&mut peer).unwrap(),
            Response::Exited { code: 0 }
        ));
    }

    #[test]
    fn failed_pty_writer_settles_queued_inputs_from_every_attachment() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (input_tx, input_rx) = mpsc::sync_channel(2);
        let (receipt_tx, receipt_rx) = mpsc::channel();
        let first = Instant::now();
        let second = first + Duration::from_nanos(1);
        input_tx
            .send(PtyInput {
                owner: first,
                sequence: 1,
                bytes: b"old".to_vec(),
            })
            .unwrap();
        input_tx
            .send(PtyInput {
                owner: second,
                sequence: 1,
                bytes: b"new".to_vec(),
            })
            .unwrap();
        drop(input_tx);
        write_pty_inputs(FailedWriter, input_rx, receipt_tx);
        let receipts: Vec<_> = receipt_rx.into_iter().collect();
        assert_eq!(receipts.len(), 2);
        assert_eq!((receipts[0].0, receipts[1].0), (first, second));
        assert!(
            receipts
                .iter()
                .all(|(_, sequence, result)| *sequence == 1 && result.is_err())
        );
    }

    #[test]
    fn attachment_preserves_partial_frames_across_readiness_and_eof() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let bytes = wire::encode(&Response::Output {
            bytes: b"screen bytes".to_vec(),
        })
        .unwrap();
        let mut pending = Vec::new();
        for byte in &bytes[..bytes.len() - 1] {
            sender.write_all(&[*byte]).unwrap();
            let (messages, ended) =
                read_available_frames::<Response>(&mut receiver, &mut pending).unwrap();
            assert!(messages.is_empty());
            assert!(!ended);
        }
        sender.write_all(&bytes[bytes.len() - 1..]).unwrap();
        sender
            .write_all(&wire::encode(&Response::Exited { code: 0 }).unwrap())
            .unwrap();
        drop(sender);
        let (messages, ended) =
            read_available_frames::<Response>(&mut receiver, &mut pending).unwrap();
        assert!(ended);
        assert!(pending.is_empty());
        assert!(
            matches!(&messages[..], [Response::Output { bytes }, Response::Exited { code: 0 }] if bytes == b"screen bytes")
        );
    }

    #[test]
    fn attachment_rejects_oversized_frame_before_allocating_its_body() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender
            .write_all(&((wire::MAX_FRAME + 1) as u32).to_be_bytes())
            .unwrap();
        let mut pending = Vec::new();
        assert!(read_available_frames::<Response>(&mut receiver, &mut pending).is_err());
        assert_eq!(pending.len(), 4);
    }

    #[test]
    fn attachment_backpressure_retains_exact_unwritten_suffix() {
        struct PausingWriter {
            bytes: Vec<u8>,
            allowance: usize,
        }
        impl Write for PausingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.allowance == 0 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let count = bytes.len().min(self.allowance);
                self.bytes.extend_from_slice(&bytes[..count]);
                self.allowance -= count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let expected = wire::encode(&Request::Input {
            sequence: 1,
            bytes: b"\x1b[<65;64;32Mhello".to_vec(),
        })
        .unwrap();
        let mut pending: VecDeque<u8> = expected.iter().copied().collect();
        let mut writer = PausingWriter {
            bytes: Vec::new(),
            allowance: 3,
        };
        flush_pending(&mut writer, &mut pending).unwrap();
        assert_eq!(pending.len(), expected.len() - 3);
        for _ in 0..expected.len() {
            writer.allowance = 1;
            flush_pending(&mut writer, &mut pending).unwrap();
        }
        assert!(pending.is_empty());
        assert_eq!(writer.bytes, expected);
    }

    #[test]
    fn aged_attachment_flushes_its_final_handoff_frame() {
        let (server, mut peer) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = Connection {
            stream: server,
            input: Vec::new(),
            input_ended: false,
            input_pending: 0,
            output: VecDeque::new(),
            attached: false,
            next_input: 2,
            close: true,
            close_since: None,
            born: Instant::now() - Duration::from_secs(60),
            peer_pid: Some(std::process::id()),
            pending_response: None,
        };
        connection.send(&Response::Detached).unwrap();
        assert!(!connection.flush_or_expire());
        assert!(matches!(
            wire::read_frame::<Response>(&mut peer).unwrap(),
            Response::Detached
        ));
    }

    #[test]
    fn pending_startup_survives_the_short_control_handshake_deadline() {
        let (server, _peer) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let (_tx, rx) = mpsc::sync_channel(1);
        let mut connection = Connection {
            stream: server,
            input: Vec::new(),
            input_ended: false,
            input_pending: 0,
            output: VecDeque::new(),
            attached: false,
            next_input: 1,
            close: false,
            close_since: None,
            born: Instant::now() - Duration::from_secs(5),
            peer_pid: Some(std::process::id()),
            pending_response: Some(rx),
        };
        assert!(connection.flush_or_expire());
        connection.pending_response = None;
        assert!(!connection.flush_or_expire());
    }

    #[test]
    fn resume_attachment_resolves_ids_directories_and_logs_but_not_another_store() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let directory = home.path().join("sessions").join(&id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("session.jsonl"), "").unwrap();
        assert_eq!(
            resume_target_id(home.path(), Path::new(&id)),
            Some(id.clone())
        );
        assert_eq!(resume_target_id(home.path(), &directory), Some(id.clone()));
        assert_eq!(
            resume_target_id(home.path(), &directory.join("session.jsonl")),
            Some(id.clone())
        );
        let other_session = other.path().join(&id);
        fs::create_dir(&other_session).unwrap();
        assert!(resume_target_id(home.path(), &other_session).is_none());
    }

    #[test]
    fn terminal_reconstruction_excludes_past_clipboard_side_effects() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"hello\x1b]52;c;c2VjcmV0\x07\x1b[?2004h");
        let mut reconstructed = parser.screen().contents_formatted();
        reconstructed.extend(parser.screen().input_mode_formatted());
        let reconstructed = String::from_utf8(reconstructed).unwrap();
        assert!(reconstructed.contains("hello"));
        assert!(reconstructed.contains("2004h"));
        assert!(!reconstructed.contains("52;c"));
        assert!(!reconstructed.contains("c2VjcmV0"));
    }
}
