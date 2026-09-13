//! Incremental Server-Sent Events framing independent from provider payloads.

/// One complete SSE event. `data` is the newline-joined value of every
/// `data:` field; no provider JSON interpretation occurs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type, defaulting to `message`.
    pub event: String,
    /// Joined opaque event data.
    pub data: String,
    /// Last event id when one has been observed.
    pub id: Option<String>,
    /// Retry hint carried by this event, in milliseconds.
    pub retry_ms: Option<u64>,
}

/// Terminal SSE framing failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SseDecodeError {
    /// A completed SSE line was not valid UTF-8.
    #[error("SSE stream contains invalid UTF-8")]
    InvalidUtf8,
    /// One buffered event exceeded its defensive cap.
    #[error("SSE event exceeds the {max_bytes}-byte limit")]
    EventTooLarge {
        /// Configured cap.
        max_bytes: usize,
    },
    /// The decoder was already terminal after an earlier framing failure.
    #[error("SSE decoder is closed after a framing failure")]
    Closed,
}

/// Standards-oriented incremental SSE decoder.
#[derive(Debug)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    event_type: Option<String>,
    data_lines: Vec<String>,
    last_event_id: Option<String>,
    retry_ms: Option<u64>,
    event_bytes: usize,
    max_event_bytes: usize,
    first_line: bool,
    failed: bool,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SseDecoder {
    /// Build with a one-MiB per-event framing cap.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_event_bytes(1024 * 1024)
    }

    /// Build with an explicit positive per-event byte cap.
    #[must_use]
    pub fn with_max_event_bytes(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            event_type: None,
            data_lines: Vec::new(),
            last_event_id: None,
            retry_ms: None,
            event_bytes: 0,
            max_event_bytes: max_event_bytes.max(1),
            first_line: true,
            failed: false,
        }
    }

    /// Feed an arbitrary network fragment and return completed events.
    ///
    /// # Errors
    /// Invalid UTF-8, an oversized event, or reuse after terminal failure.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseDecodeError> {
        if self.failed {
            return Err(SseDecodeError::Closed);
        }
        self.buffer.extend_from_slice(bytes);
        self.decode_available(false)
    }

    /// Flush an optional unterminated final line/event at response EOF.
    ///
    /// # Errors
    /// Invalid UTF-8, an oversized event, or an earlier terminal failure.
    pub fn finish(mut self) -> Result<Vec<SseEvent>, SseDecodeError> {
        if self.failed {
            return Err(SseDecodeError::Closed);
        }
        let mut events = self.decode_available(true)?;
        if let Some(event) = self.dispatch() {
            events.push(event);
        }
        Ok(events)
    }

    fn decode_available(&mut self, eof: bool) -> Result<Vec<SseEvent>, SseDecodeError> {
        let mut events = Vec::new();
        loop {
            let Some((line, consumed)) = next_line(&self.buffer, eof) else {
                if self.buffer.len().saturating_add(self.event_bytes) > self.max_event_bytes {
                    return self.fail(SseDecodeError::EventTooLarge {
                        max_bytes: self.max_event_bytes,
                    });
                }
                break;
            };
            self.buffer.drain(..consumed);
            let mut line = line;
            if self.first_line {
                self.first_line = false;
                if line.starts_with(&[0xef, 0xbb, 0xbf]) {
                    line.drain(..3);
                }
            }
            let line = match std::str::from_utf8(&line) {
                Ok(line) => line,
                Err(_) => return self.fail(SseDecodeError::InvalidUtf8),
            };
            let line = self.process_line(line)?;
            if let Some(event) = line {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &str) -> Result<Option<SseEvent>, SseDecodeError> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line.len().saturating_add(self.event_bytes) > self.max_event_bytes {
            return self.fail(SseDecodeError::EventTooLarge {
                max_bytes: self.max_event_bytes,
            });
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "data" => {
                self.event_bytes = self.event_bytes.saturating_add(value.len());
                self.data_lines.push(value.to_owned());
            }
            "event" => {
                self.event_bytes = self.event_bytes.saturating_add(value.len());
                self.event_type = (!value.is_empty()).then(|| value.to_owned());
            }
            "id" if !value.contains('\0') => {
                self.last_event_id = Some(value.to_owned());
            }
            "retry" if value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry_ms = value.parse().ok();
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let has_data = !self.data_lines.is_empty();
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        self.event_bytes = 0;
        let event = self
            .event_type
            .take()
            .unwrap_or_else(|| "message".to_owned());
        let retry_ms = self.retry_ms.take();
        has_data.then(|| SseEvent {
            event,
            data,
            id: self.last_event_id.clone(),
            retry_ms,
        })
    }

    fn fail<T>(&mut self, error: SseDecodeError) -> Result<T, SseDecodeError> {
        self.failed = true;
        self.buffer.clear();
        Err(error)
    }
}

fn next_line(buffer: &[u8], eof: bool) -> Option<(Vec<u8>, usize)> {
    for (index, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some((buffer[..index].to_vec(), index + 1)),
            b'\r' if index + 1 < buffer.len() => {
                let consumed = index + if buffer[index + 1] == b'\n' { 2 } else { 1 };
                return Some((buffer[..index].to_vec(), consumed));
            }
            b'\r' if eof => return Some((buffer[..index].to_vec(), index + 1)),
            b'\r' => return None,
            _ => {}
        }
    }
    if eof && !buffer.is_empty() {
        Some((buffer.to_vec(), buffer.len()))
    } else {
        None
    }
}
