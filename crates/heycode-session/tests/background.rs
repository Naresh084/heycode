//! IPC boundary checks independent of any provider account or terminal UI.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};

use heycode_session::background::{self as wire, Request, Response};

#[test]
fn frames_are_bounded_before_allocation_and_preserve_input_sequence() {
    let bytes = wire::encode(&Request::Input {
        sequence: 17,
        bytes: b"accepted\r".to_vec(),
    })
    .unwrap();
    match wire::read_frame::<Request>(&mut Cursor::new(bytes)).unwrap() {
        Request::Input { sequence, bytes } => {
            assert_eq!(sequence, 17);
            assert_eq!(bytes, b"accepted\r");
        }
        _ => panic!("wrong frame"),
    }
    let mut oversized = Cursor::new(((wire::MAX_FRAME + 1) as u32).to_be_bytes());
    assert!(wire::read_frame::<Request>(&mut oversized).is_err());
    assert_eq!(oversized.position(), 4);
    assert!(
        wire::encode(&Request::Input {
            sequence: 1,
            bytes: vec![255; wire::MAX_FRAME]
        })
        .is_err()
    );
}

#[test]
fn partial_frame_is_never_interpreted_as_accepted_input() {
    let mut bytes = wire::encode(&Request::Input {
        sequence: 1,
        bytes: b"prompt\r".to_vec(),
    })
    .unwrap();
    bytes.pop();
    assert!(wire::read_frame::<Request>(&mut Cursor::new(bytes)).is_err());
}

#[test]
fn private_registry_refuses_symlinks_and_permissive_directories() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    wire::private_directory(&target).unwrap();
    let link = root.path().join("link");
    symlink(&target, &link).unwrap();
    assert!(wire::private_directory(&link).is_err());
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(wire::private_directory(&target).is_err());
}

#[test]
fn durable_record_is_private_and_cannot_follow_a_final_symlink() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("status.json");
    wire::write_private_json(&file, &serde_json::json!({"state":"detached"})).unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let read: serde_json::Value = wire::read_private_json(&file).unwrap();
    assert_eq!(read["state"], "detached");
    let alias = root.path().join("alias.json");
    symlink(&file, &alias).unwrap();
    assert!(wire::read_private_json::<serde_json::Value>(&alias).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn local_peer_and_socket_namespace_are_checked() {
    let (left, right) = UnixStream::pair().unwrap();
    wire::check_peer(&left).unwrap();
    wire::check_peer(&right).unwrap();
    assert!(wire::connect(std::path::Path::new("/tmp/arbitrary.sock")).is_err());
    assert!(!wire::valid_id("../../session"));
    assert!(!wire::valid_id("00000000000000000000000000000000"));
}

#[test]
fn real_socket_control_roundtrip_and_stale_socket_are_distinct() {
    let socket = wire::socket_root()
        .unwrap()
        .join(format!("{}.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        wire::check_peer(&stream).unwrap();
        assert!(matches!(
            wire::read_frame::<Request>(&mut stream).unwrap(),
            Request::Stop
        ));
        let frame = wire::encode(&Response::Ok).unwrap();
        // Deliberate transport fragmentation.
        for byte in frame {
            stream.write_all(&[byte]).unwrap();
        }
        let mut eof = [0];
        let _ = stream.read(&mut eof);
    });
    assert!(matches!(
        wire::request(&socket, &Request::Stop).unwrap(),
        Response::Ok
    ));
    server.join().unwrap();
    assert!(wire::request(&socket, &Request::Status).is_err());
    fs::remove_file(socket).unwrap();
}

#[test]
fn conversation_fork_never_inherits_parent_runtime_ownership() {
    use heycode_session::{ForkBoundary, Session, SessionEventKind};
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create(root.path()).unwrap();
    parent
        .append(SessionEventKind::RuntimeLinked {
            runtime: "native".into(),
            runtime_session_id: parent.id().to_string(),
        })
        .unwrap();
    let parent_id = parent.id().to_string();
    let mut child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    assert!(child.events().iter().any(|event| matches!(&event.kind, SessionEventKind::RuntimeLinked { runtime_session_id, .. } if runtime_session_id == &parent_id)));
    assert!(
        child.runtime_link().is_none(),
        "inherited provenance does not own the parent's runtime"
    );
    let child_id = child.id().to_string();
    child
        .append(SessionEventKind::RuntimeLinked {
            runtime: "native".into(),
            runtime_session_id: child_id.clone(),
        })
        .unwrap();
    assert_eq!(child.runtime_link(), Some(("native", child_id.as_str())));
    assert_eq!(parent.runtime_link(), Some(("native", parent_id.as_str())));
    let path = child.path().parent().unwrap().to_path_buf();
    drop(child);
    let child = Session::open(path).unwrap();
    assert_eq!(child.runtime_link(), Some(("native", child_id.as_str())));
    let grandchild = child.fork(root.path(), ForkBoundary::Latest).unwrap();
    assert!(grandchild.runtime_link().is_none());
}
