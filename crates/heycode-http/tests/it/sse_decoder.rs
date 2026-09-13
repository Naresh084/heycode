//! Standards-oriented SSE framing under arbitrary network fragmentation.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_http::{SseDecodeError, SseDecoder, SseEvent};

fn decode(fragments: &[&[u8]]) -> Result<Vec<SseEvent>, SseDecodeError> {
    let mut decoder = SseDecoder::new();
    let mut events = Vec::new();
    for fragment in fragments {
        events.extend(decoder.feed(fragment)?);
    }
    events.extend(decoder.finish()?);
    Ok(events)
}

#[test]
fn every_byte_split_preserves_utf8_multidata_ids_retry_and_comments() {
    let raw = concat!(
        "\u{feff}id: 7\r\n",
        "event: token\r\n",
        "data: hé\r\n",
        "data: llo\r\n",
        "retry: 1500\r\n",
        "\r\n",
        ": keepalive\n",
        "\n",
        "data: [DONE]\n\n",
    )
    .as_bytes();
    let expected = vec![
        SseEvent {
            event: "token".to_owned(),
            data: "hé\nllo".to_owned(),
            id: Some("7".to_owned()),
            retry_ms: Some(1_500),
        },
        SseEvent {
            event: "message".to_owned(),
            data: "[DONE]".to_owned(),
            id: Some("7".to_owned()),
            retry_ms: None,
        },
    ];

    for split in 0..=raw.len() {
        assert_eq!(
            decode(&[&raw[..split], &raw[split..]]).unwrap(),
            expected,
            "split at byte {split}"
        );
    }
    let one_byte: Vec<&[u8]> = raw.chunks(1).collect();
    assert_eq!(decode(&one_byte).unwrap(), expected);
}

#[test]
fn eof_dispatches_a_complete_unterminated_event() {
    let events = decode(&[b"event: final\ndata: tail"]).unwrap();
    assert_eq!(
        events,
        [SseEvent {
            event: "final".to_owned(),
            data: "tail".to_owned(),
            id: None,
            retry_ms: None,
        }]
    );
}

#[test]
fn invalid_utf8_and_oversized_event_fail_terminally() {
    let mut invalid = SseDecoder::new();
    assert!(matches!(
        invalid.feed(b"data: \xff\n\n"),
        Err(SseDecodeError::InvalidUtf8)
    ));
    assert!(invalid.feed(b"data: later\n\n").is_err());

    let mut oversized = SseDecoder::with_max_event_bytes(8);
    assert!(matches!(
        oversized.feed(b"data: 123456789\n\n"),
        Err(SseDecodeError::EventTooLarge { max_bytes: 8 })
    ));
}
