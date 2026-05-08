//! Minimal SSE wire-format parser.
//!
//! Only handles the fields Ferra emits: `event:`, `id:`, `data:`. Comments
//! (`:` lines) and unknown fields are ignored. `data:` accumulates across
//! multiple lines (joined with `\n`). Events terminate on a blank line.

use bytes::Bytes;

#[derive(Debug, Default, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

pub struct SseParser {
    buf: Vec<u8>,
    cur: SseEvent,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: SseEvent::default(),
        }
    }

    /// Feed bytes from the network and emit any complete events that
    /// resulted. The returned events are owned and the parser retains its
    /// internal partial-line state.
    pub fn feed(&mut self, chunk: &Bytes) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            // Find end of next line: \n (handles CRLF too via \r trim).
            let Some(pos) = self.buf.iter().position(|b| *b == b'\n') else {
                break;
            };
            let raw_line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line_with_lf = String::from_utf8_lossy(&raw_line);
            let line = line_with_lf.trim_end_matches(['\n', '\r']);

            if line.is_empty() {
                // Event terminator.
                if let Some(ev) = self.flush() {
                    out.push(ev);
                }
                continue;
            }
            if line.starts_with(':') {
                // Comment.
                continue;
            }
            let (field, value) = match line.find(':') {
                Some(i) => (&line[..i], line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..])),
                None => (line, ""),
            };
            match field {
                "event" => self.cur.event = Some(value.to_string()),
                "id" => self.cur.id = Some(value.to_string()),
                "data" => {
                    if !self.cur.data.is_empty() {
                        self.cur.data.push('\n');
                    }
                    self.cur.data.push_str(value);
                }
                _ => {} // ignore unknown fields (incl. retry:)
            }
        }
        out
    }

    fn flush(&mut self) -> Option<SseEvent> {
        if self.cur.event.is_none() && self.cur.id.is_none() && self.cur.data.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn feed_str(p: &mut SseParser, s: &str) -> Vec<SseEvent> {
        p.feed(&Bytes::copy_from_slice(s.as_bytes()))
    }

    #[test]
    fn parses_simple_event() {
        let mut p = SseParser::new();
        let evs = feed_str(&mut p, "event: kv_changed\nid: 7\ndata: hello\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("kv_changed"));
        assert_eq!(evs[0].id.as_deref(), Some("7"));
        assert_eq!(evs[0].data, "hello");
    }

    #[test]
    fn handles_split_chunks() {
        let mut p = SseParser::new();
        let mut evs = feed_str(&mut p, "event: kv_chan");
        assert!(evs.is_empty());
        evs = feed_str(&mut p, "ged\nid: 1\ndata: ");
        assert!(evs.is_empty());
        evs = feed_str(&mut p, "x\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("kv_changed"));
        assert_eq!(evs[0].data, "x");
    }

    #[test]
    fn parses_two_events_in_one_chunk() {
        let mut p = SseParser::new();
        let evs = feed_str(
            &mut p,
            "event: heartbeat\ndata: {}\n\nevent: kv_changed\nid: 2\ndata: {\"k\":1}\n\n",
        );
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event.as_deref(), Some("heartbeat"));
        assert_eq!(evs[1].event.as_deref(), Some("kv_changed"));
        assert_eq!(evs[1].data, "{\"k\":1}");
    }

    #[test]
    fn ignores_comments_and_crlf() {
        let mut p = SseParser::new();
        let evs = feed_str(
            &mut p,
            ": this is a comment\r\nevent: heartbeat\r\ndata: {}\r\n\r\n",
        );
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event.as_deref(), Some("heartbeat"));
    }

    #[test]
    fn multiline_data() {
        let mut p = SseParser::new();
        let evs = feed_str(&mut p, "event: x\ndata: line1\ndata: line2\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "line1\nline2");
    }
}
