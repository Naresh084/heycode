#![no_main]

use std::path::{Path, PathBuf};

use heycode_session::{Session, derive_messages, project_repair, project_usage};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fn write_stream(root: &Path, name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let directory = root.join(name);
    std::fs::create_dir(&directory).ok()?;
    std::fs::write(directory.join("session.jsonl"), bytes).ok()?;
    Some(directory)
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let Ok(root) = tempfile::tempdir() else {
        return;
    };
    let Some(input_dir) = write_stream(root.path(), "input", input) else {
        return;
    };
    let Ok(session) = Session::open(&input_dir) else {
        return;
    };

    for (index, event) in session.events().iter().enumerate() {
        let expected = u64::try_from(index).unwrap_or(u64::MAX);
        assert!(
            event.seq == expected,
            "accepted root session sequence must be contiguous"
        );
    }

    let messages = derive_messages(session.events());
    let repair = project_repair(session.events());
    let usage = project_usage(session.events());
    assert!(
        messages == derive_messages(session.events()),
        "session message projection must be deterministic"
    );
    assert!(
        repair == project_repair(session.events()),
        "session repair projection must be deterministic"
    );
    assert!(
        usage == project_usage(session.events()),
        "session usage projection must be deterministic"
    );

    let mut canonical = Vec::new();
    for event in session.events() {
        let Ok(mut line) = serde_json::to_vec(event) else {
            panic!("an accepted session event must serialize");
        };
        canonical.append(&mut line);
        canonical.push(b'\n');
    }
    assert!(
        canonical.len() <= MAX_INPUT_BYTES.saturating_mul(4),
        "canonical session representation must remain bounded"
    );

    let Some(roundtrip_dir) = write_stream(root.path(), "roundtrip", &canonical) else {
        return;
    };
    let Ok(roundtrip) = Session::open(roundtrip_dir) else {
        panic!("canonical accepted session must reopen");
    };
    assert!(
        session.events() == roundtrip.events(),
        "accepted session semantics must survive serialize and reopen"
    );
    assert!(
        messages == derive_messages(roundtrip.events()),
        "message projection must survive session round trip"
    );
    assert!(
        repair == project_repair(roundtrip.events()),
        "repair projection must survive session round trip"
    );
});
