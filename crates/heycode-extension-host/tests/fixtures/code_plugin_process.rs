//! Standalone std-only PL09 length-prefixed protocol fixture.

use std::io::{Read as _, Write as _};

const MAX_FRAME: usize = 1024 * 1024;

fn main() {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut prefix = [0_u8; 4];
        if input.read_exact(&mut prefix).is_err() {
            return;
        }
        let length = u32::from_be_bytes(prefix) as usize;
        if length == 0 || length > MAX_FRAME {
            return;
        }
        let mut request = vec![0_u8; length];
        if input.read_exact(&mut request).is_err() {
            return;
        }
        let Ok(request) = String::from_utf8(request) else {
            return;
        };
        if request.contains("\"operation\":\"oversized-frame\"") {
            let invalid = (MAX_FRAME as u32).saturating_add(1).to_be_bytes();
            let _ = output.write_all(&invalid);
            let _ = output.flush();
            std::thread::sleep(std::time::Duration::from_secs(20));
            return;
        }
        let response = if request.contains("\"kind\":\"initialize\"") {
            request
                .replace("\"kind\":\"initialize\"", "\"kind\":\"ready\"")
                .replace("\"granted_capabilities\":", "\"accepted_capabilities\":")
        } else if request.contains("\"kind\":\"invoke\"") {
            if request.contains("\"operation\":\"stream\"")
                && request.contains("\"model\":\"crash\"")
            {
                return;
            }
            if request.contains("\"operation\":\"block-descendant\"") {
                let Some(ready) = string_field(&request, "ready") else {
                    return;
                };
                let Some(survived) = string_field(&request, "survived") else {
                    return;
                };
                let command = format!("sleep 2; printf x > {}", shell_quote(survived));
                if std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(command)
                    .spawn()
                    .is_err()
                {
                    return;
                }
                if std::fs::write(ready, b"ready").is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(20));
            }
            let Some(session_id) = string_field(&request, "session_id") else {
                return;
            };
            let Some(request_id) = integer_field(&request, "request_id") else {
                return;
            };
            let output = if request.contains("\"operation\":\"stream\"") {
                r#"{"chunks":[{"kind":"text_delta","text":"installed"},{"kind":"usage","prompt_tokens":3,"completion_tokens":1},{"kind":"finish","reason":"stop"}]}"#
            } else {
                r#"{"ok":true}"#
            };
            format!(
                "{{\"protocol_version\":1,\"kind\":\"result\",\"session_id\":\"{session_id}\",\"request_id\":{request_id},\"output\":{output}}}"
            )
        } else {
            "{}".to_owned()
        };
        let Ok(length) = u32::try_from(response.len()) else {
            return;
        };
        if output.write_all(&length.to_be_bytes()).is_err() {
            return;
        }
        for byte in response.as_bytes() {
            if output.write_all(&[*byte]).is_err() || output.flush().is_err() {
                return;
            }
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn string_field<'a>(json: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("\"{field}\":\"");
    let tail = json.split_once(&prefix)?.1;
    tail.split_once('"').map(|(value, _)| value)
}

fn integer_field(json: &str, field: &str) -> Option<u64> {
    let prefix = format!("\"{field}\":");
    let tail = json.split_once(&prefix)?.1;
    let digits = tail
        .bytes()
        .take_while(u8::is_ascii_digit)
        .map(char::from)
        .collect::<String>();
    digits.parse().ok()
}
