//! Streaming page extraction: fixed scan buffer and bounded retained bytes.
use super::{
    FileSystemError, FileSystemErrorCode, ReadFileOutput, ReadFilePage, ReadFileWindow, Stamp,
};
use std::io::Read;
use tokio_util::sync::CancellationToken;

/// Counts at most 64 MiB. File byte size remains exact beyond that boundary.
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn read_page(
    mut file: cap_std::fs::File,
    window: ReadFileWindow,
    max_bytes: usize,
    cancellation: CancellationToken,
    shutdown: CancellationToken,
) -> Result<(ReadFileOutput, Stamp), FileSystemError> {
    let before =
        Stamp::of_file(&file).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
    let total_bytes = file
        .metadata()
        .map_err(|error| super::from_io(&error))?
        .len();
    let mut buffer = [0_u8; 8192];
    let mut bytes = Vec::with_capacity(max_bytes.min(16 * 1024));
    let mut scanned = 0_u64;
    let mut line = 1_usize;
    let mut start_line = window.offset;
    let mut start_byte = total_bytes;
    let mut started = false;
    let mut last_byte = None;
    loop {
        if shutdown.is_cancelled() {
            return Err(FileSystemError::new(FileSystemErrorCode::ServiceStopped));
        }
        if cancellation.is_cancelled() {
            return Err(FileSystemError::new(FileSystemErrorCode::Cancelled));
        }
        let remaining = MAX_SCAN_BYTES
            .saturating_sub(scanned)
            .min(buffer.len() as u64) as usize;
        if remaining == 0 {
            break;
        }
        let count = file
            .read(&mut buffer[..remaining])
            .map_err(|error| super::from_io(&error))?;
        if count == 0 {
            break;
        }
        if buffer[..count].contains(&0) {
            return Err(FileSystemError::new(FileSystemErrorCode::Binary));
        }
        for byte in &buffer[..count] {
            let eligible = window
                .byte_offset
                .map_or(line >= window.offset, |offset| scanned >= offset);
            if !started && eligible {
                started = true;
                start_line = line;
                start_byte = scanned;
            }
            if started && line.saturating_sub(start_line) < window.limit && bytes.len() < max_bytes
            {
                bytes.push(*byte);
            }
            if *byte == b'\n' {
                line += 1;
            }
            scanned += 1;
            last_byte = Some(*byte);
        }
    }
    let scan_limited = scanned < total_bytes;
    if !started && scan_limited {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    // A byte ceiling may bisect a UTF-8 scalar. Return its valid prefix and
    // resume at that scalar, never introduce replacement characters.
    if let Err(error) = std::str::from_utf8(&bytes) {
        if error.error_len().is_none() && bytes.len() == max_bytes {
            bytes.truncate(error.valid_up_to());
        } else {
            return Err(FileSystemError::new(FileSystemErrorCode::InvalidUtf8));
        }
    }
    if started && bytes.is_empty() && max_bytes < 4 {
        return Err(FileSystemError::new(FileSystemErrorCode::InvalidSpec));
    }
    let after =
        Stamp::of_file(&file).ok_or_else(|| FileSystemError::new(FileSystemErrorCode::Io))?;
    if before != after {
        return Err(FileSystemError::new(FileSystemErrorCode::ChangedAtCommit));
    }
    let total_lines = (!scan_limited).then_some(if scanned == 0 {
        0
    } else {
        line - usize::from(last_byte == Some(b'\n'))
    });
    let lines_returned = std::str::from_utf8(&bytes)
        .map_err(|_| FileSystemError::new(FileSystemErrorCode::InvalidUtf8))?
        .lines()
        .count();
    let end = start_byte.saturating_add(bytes.len() as u64);
    let has_more = started && end < total_bytes;
    let partial_last_line = has_more && bytes.last() != Some(&b'\n');
    let next_offset = (has_more && !partial_last_line).then_some(start_line + lines_returned);
    let page = ReadFilePage {
        total_bytes,
        total_lines,
        start_line,
        start_byte,
        lines_returned,
        next_byte_offset: has_more.then_some(end),
        next_offset,
        partial_last_line,
        scan_limited,
        revision: after.revision(),
    };
    Ok((ReadFileOutput::new(bytes, has_more).with_page(page), after))
}
