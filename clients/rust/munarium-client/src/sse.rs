// SPDX-License-Identifier: Apache-2.0
//! A minimal incremental Server-Sent-Events parser for the streaming turn
//! plane (`POST /v1/sessions/{id}/turns/stream`). Hand-rolled by design —
//! the server's emitter is simple (named events, single-line JSON data,
//! comment keep-alives) and a dependency would be heavier than the format.
//!
//! The parser is PURE: feed it byte chunks as they arrive (chunk boundaries
//! may fall anywhere, including mid-line and mid-UTF-8-codepoint) and it
//! yields complete events. Per the SSE grammar it handles `event:`/`data:`
//! fields, multi-line data accumulation, `\n`/`\r\n`/`\r` line endings,
//! comment lines (leading `:`, the keep-alive form), and ignores fields it
//! does not know (`id:`, `retry:`).
//!
//! Retention is bounded: a stream that never terminates its lines or events
//! cannot grow client memory without limit — past [`MAX_EVENT_BYTES`] the
//! parser reports overflow and the caller ends the stream with a typed
//! error instead of buffering toward an OOM kill.

/// Upper bound on one event's buffered bytes (pending line + accumulated
/// data). A terminal `done` event carries a whole TurnResponse — hits text
/// included — so the cap is generous; anything past it is a misbehaving
/// peer, not a real event.
pub(crate) const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// One dispatched SSE event: the event name (empty = the spec's default
/// "message") and the joined data payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
}

/// The parser refused to buffer further — the peer sent more than
/// [`MAX_EVENT_BYTES`] without completing an event.
#[derive(Debug)]
pub(crate) struct SseOverflow;

#[derive(Default)]
pub(crate) struct SseParser {
    /// Undelivered raw bytes (a partial line, possibly a partial codepoint).
    /// Cleared — not reallocated — per line, so capacity is reused.
    buf: Vec<u8>,
    /// Accumulated `data:` lines for the event being built.
    data: Vec<String>,
    /// Bytes across `data` (tracked so the cap is O(1) to enforce).
    data_bytes: usize,
    /// The pending `event:` name.
    event: String,
    /// True when the previous byte was CR — a following LF is the same
    /// line ending, not an extra blank line.
    saw_cr: bool,
    /// The cap was exceeded on an earlier push: events completed in that
    /// push were still delivered (a `done` beside oversized trailing bytes
    /// is a real result), and the NEXT push reports the overflow.
    poisoned: bool,
}

impl SseParser {
    /// Feed one chunk; returns every event completed by it, in order.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseOverflow> {
        if self.poisoned {
            return Err(SseOverflow);
        }
        let mut out = Vec::new();
        for &b in chunk {
            match b {
                b'\n' if self.saw_cr => self.saw_cr = false, // CRLF: LF consumed
                b'\r' | b'\n' => {
                    self.saw_cr = b == b'\r';
                    // A free function over the fields sidesteps the borrow
                    // conflict without `mem::take`, so `buf` keeps its
                    // capacity across lines instead of reallocating per line.
                    line(
                        &self.buf,
                        &mut self.event,
                        &mut self.data,
                        &mut self.data_bytes,
                        &mut out,
                    );
                    self.buf.clear();
                }
                _ => {
                    self.saw_cr = false;
                    self.buf.push(b);
                }
            }
        }
        if self.buf.len() + self.data_bytes > MAX_EVENT_BYTES {
            self.poisoned = true;
            if out.is_empty() {
                return Err(SseOverflow);
            }
        }
        Ok(out)
    }
}

fn line(
    raw: &[u8],
    event: &mut String,
    data: &mut Vec<String>,
    data_bytes: &mut usize,
    out: &mut Vec<SseEvent>,
) {
    if raw.is_empty() {
        // Blank line = dispatch. An event with no data lines is dropped per
        // the SSE spec (this is what makes comment keep-alives free).
        if !data.is_empty() {
            let payload = if data.len() == 1 {
                data.pop().expect("len checked") // the dominant case: no join copy
            } else {
                data.join("\n")
            };
            out.push(SseEvent {
                event: std::mem::take(event),
                data: payload,
            });
            data.clear();
        } else {
            event.clear();
        }
        *data_bytes = 0;
        return;
    }
    if raw[0] == b':' {
        return; // comment / keep-alive
    }
    let text = String::from_utf8_lossy(raw);
    let (field, value) = match text.split_once(':') {
        Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
        None => (text.as_ref(), ""),
    };
    match field {
        "event" => *event = value.to_string(),
        "data" => {
            *data_bytes += value.len();
            data.push(value.to_string());
        }
        _ => {} // id / retry / unknown fields: ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(parser: &mut SseParser, bytes: &[u8]) -> Vec<SseEvent> {
        parser.push(bytes).expect("no overflow in tests")
    }

    #[test]
    fn parses_named_events_and_keepalives() {
        let mut p = SseParser::default();
        let evs = one(
            &mut p,
            b": keep-alive\n\nevent: progress\ndata: {\"stage\":\"merge\"}\n\n",
        );
        assert_eq!(
            evs,
            vec![SseEvent {
                event: "progress".into(),
                data: "{\"stage\":\"merge\"}".into()
            }]
        );
    }

    #[test]
    fn survives_arbitrary_chunk_boundaries() {
        // The transport may split anywhere — including mid-line and between
        // CR and LF. Byte-by-byte is the adversarial version of that.
        let wire = b"event: progress\r\ndata: {\"n\":1}\r\n\r\nevent: done\ndata: {}\n\n";
        let mut p = SseParser::default();
        let mut evs = Vec::new();
        for b in wire.iter() {
            evs.extend(p.push(std::slice::from_ref(b)).expect("no overflow"));
        }
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event, "progress");
        assert_eq!(evs[0].data, "{\"n\":1}");
        assert_eq!(evs[1].event, "done");
    }

    #[test]
    fn multi_line_data_joins_with_newlines() {
        let mut p = SseParser::default();
        let evs = one(&mut p, b"data: a\ndata: b\n\n");
        assert_eq!(evs[0].data, "a\nb");
        assert_eq!(evs[0].event, "", "default event name is empty");
    }

    #[test]
    fn event_without_data_is_dropped_not_dispatched() {
        let mut p = SseParser::default();
        assert!(one(&mut p, b"event: progress\n\n").is_empty());
        // ...and the stale name does not leak into the next event.
        let evs = one(&mut p, b"data: x\n\n");
        assert_eq!(evs[0].event, "");
    }

    #[test]
    fn overflow_never_drops_events_completed_in_the_same_chunk() {
        // A terminal `done` followed by oversized trailing bytes in ONE
        // chunk: the done is delivered, and the overflow surfaces on the
        // NEXT push — never a lost result.
        let mut p = SseParser::default();
        let mut chunk = b"event: done\ndata: {}\n\n".to_vec();
        chunk.extend(std::iter::repeat_n(b'x', MAX_EVENT_BYTES + 1));
        let evs = p.push(&chunk).expect("completed events survive overflow");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event, "done");
        assert!(p.push(b"more").is_err(), "poisoned after overflow");
    }

    #[test]
    fn unknown_fields_and_no_colon_lines_are_ignored() {
        let mut p = SseParser::default();
        let evs = one(&mut p, b"id: 7\nretry: 100\nnonsense\ndata: ok\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "ok");
    }

    #[test]
    fn a_neverending_event_overflows_instead_of_growing_forever() {
        // No newline at all: the pending line buffer hits the cap.
        let mut p = SseParser::default();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut overflowed = false;
        for _ in 0..20 {
            if p.push(&chunk).is_err() {
                overflowed = true;
                break;
            }
        }
        assert!(overflowed, "unterminated line must trip MAX_EVENT_BYTES");

        // Data lines that never dispatch (no blank line) also count.
        let mut p = SseParser::default();
        let line = [b"data: ".as_slice(), &vec![b'y'; 1024 * 1024], b"\n"].concat();
        let mut overflowed = false;
        for _ in 0..20 {
            if p.push(&line).is_err() {
                overflowed = true;
                break;
            }
        }
        assert!(overflowed, "undispatched data must trip MAX_EVENT_BYTES");
    }
}
