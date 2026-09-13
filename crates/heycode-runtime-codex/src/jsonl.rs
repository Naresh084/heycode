//! Strict byte-exact JSONL frame decoding.

use crate::config::WIRE_LINE_LIMIT;
use crate::{CodexAppServerError, CodexAppServerErrorCode};

#[derive(Default)]
pub(crate) struct RawJsonLineDecoder {
    pending: Vec<u8>,
}

impl RawJsonLineDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, CodexAppServerError> {
        let mut frames = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                frames.push(std::mem::take(&mut self.pending));
            } else {
                if self.pending.len() >= WIRE_LINE_LIMIT {
                    return Err(protocol());
                }
                self.pending.push(*byte);
            }
        }
        Ok(frames)
    }

    pub(crate) fn finish(self) -> Result<(), CodexAppServerError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(protocol())
        }
    }
}

impl std::fmt::Debug for RawJsonLineDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawJsonLineDecoder")
            .field("pending_bytes", &self.pending.len())
            .finish()
    }
}

fn protocol() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Protocol)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn fragmentation_crlf_and_many_cumulative_frames_are_independent() {
        let mut decoder = RawJsonLineDecoder::default();
        assert!(decoder.push(b"{\"id\":1").unwrap().is_empty());
        assert_eq!(
            decoder.push(b",\"result\":{}}\r\nnext\n").unwrap(),
            [b"{\"id\":1,\"result\":{}}".to_vec(), b"next".to_vec()]
        );
        for _ in 0..2_000 {
            assert_eq!(decoder.push(b"{}\n").unwrap(), [b"{}".to_vec()]);
        }
        decoder.finish().unwrap();
    }

    #[test]
    fn oversized_or_unterminated_frames_fail_without_body_projection() {
        let mut decoder = RawJsonLineDecoder::default();
        let error = decoder.push(&vec![b'x'; WIRE_LINE_LIMIT + 1]).unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);

        let mut decoder = RawJsonLineDecoder::default();
        decoder.push(b"private-canary").unwrap();
        let error = decoder.finish().unwrap_err();
        assert!(!error.to_string().contains("private-canary"));
    }
}
