use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use falcon_proto::tsp::{Parser, State, Token};
use ghostty_vt::Terminal;

use super::VtHandler;
use crate::vtproto::FromVt;

const HELLO_GRACE: Duration = Duration::from_secs(1);
const SNAPSHOT_MAGIC: &[u8] = b"falcon-vt-tsp\0\x01";

/// Native documents belong to the persistent VT, rather than an attached viewer.
/// DA1 sentinels after a TSP hello wait together: later sentinels must never
/// overtake the probe's sentinel in the application's capability-query FIFO.
#[derive(Default)]
pub(super) struct NativeState {
    parser: Parser,
    document: State,
    error: Option<String>,
    hello: bool,
    deferred: Option<(Instant, Vec<u8>)>,
}

impl NativeState {
    pub(super) fn feed(&mut self, term: &mut Terminal<VtHandler>, bytes: &[u8], replay: bool) {
        let Self { parser, document, error, hello, deferred } = self;
        let mut at = 0;
        parser.push(bytes, |end, token| {
            let capture = !replay && *hello && matches!(&token, Token::Da1);
            if capture {
                // Only the final c executes DA1. Earlier bytes in the same Feed
                // may contain independent queries whose replies must remain immediate.
                let before = end.saturating_sub(1).max(at);
                term.vt_write(&bytes[at..before]);
                at = before;
                let held = deferred.as_mut().map(|(_, bytes)| std::mem::take(bytes)).unwrap_or_default();
                term.handler_mut().held_reply = Some(held);
            }
            term.vt_write(&bytes[at..end]);
            at = end;
            if capture {
                let reply = term.handler_mut().held_reply.take().unwrap_or_default();
                let deadline = deferred.as_ref().map(|(at, _)| *at).unwrap_or_else(|| Instant::now() + HELLO_GRACE);
                if reply.len() > 65_536 {
                    term.handler_mut().push(FromVt::PtyWrite(reply));
                    *deferred = None;
                    *hello = false;
                } else {
                    *deferred = Some((deadline, reply));
                }
            }
            match token {
                Token::Tsp(message) => {
                    if message.verb == b'q' {
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&message.body)
                            && value["q"] == "hello"
                        {
                            *hello = !replay;
                            if *hello && deferred.is_none() {
                                *deferred = Some((Instant::now() + HELLO_GRACE, Vec::new()));
                            }
                        }
                    } else if message.verb == b'o' {
                        // Hello is confirmed. Release every real sentinel in order,
                        // including the probe's: the application still owns its FIFO slot.
                        if let Some((_, bytes)) = deferred.take() && !bytes.is_empty() {
                            term.handler_mut().push(FromVt::PtyWrite(bytes));
                        }
                        *hello = false;
                    }
                    if let Err(problem) = document.apply(&message)
                        && document.replay().is_err()
                    {
                        // Tern skips individual invalid ops. Only failed retained
                        // state makes the whole native snapshot unavailable.
                        error.get_or_insert_with(|| problem.to_string());
                    }
                }
                Token::Reset => {
                    *document = State::default();
                    *error = None;
                    *hello = false;
                    *deferred = None;
                }
                Token::Error(_) => {} // Interrupted/expired chunks are discarded by Tern too.
                Token::Da1 => {}
            }
        });
        term.vt_write(&bytes[at..]);
        if let Some(problem) = parser.failure() {
            error.get_or_insert_with(|| problem.to_string());
        }
    }

    pub(super) fn timeout(&self) -> Option<Duration> {
        self.deferred.as_ref().map(|(at, _)| at.saturating_duration_since(Instant::now()))
    }

    pub(super) fn expire(&mut self, term: &mut Terminal<VtHandler>) {
        if self.deferred.as_ref().is_some_and(|(at, _)| *at <= Instant::now()) {
            let (_, bytes) = self.deferred.take().expect("checked deadline");
            self.hello = false;
            if !bytes.is_empty() { term.handler_mut().push(FromVt::PtyWrite(bytes)); }
        }
    }


    pub(super) fn replay_with_limit(&self, limit: usize) -> Result<Vec<u8>> {
        if let Some(error) = &self.error { bail!("{error}"); }
        self.document.replay_with_limit(limit).map_err(Into::into)
    }

    pub(super) fn continuation(&self) -> Result<Vec<u8>> {
        self.parser.replay_pending().map_err(Into::into)
    }

    pub(super) fn snapshot_continuation(&self, grid: Vec<u8>) -> Result<Vec<u8>> {
        let mut native = self.continuation()?;
        if native.is_empty() { return Ok(grid); }
        if !self.parser.pending_is_tsp() && !native.ends_with(&grid) {
            native.extend_from_slice(&grid);
        }
        Ok(native)
    }

    pub(super) fn encode(&self, grid: &[u8]) -> Vec<u8> {
        let native = if let Some(error) = &self.error {
            Err(anyhow::anyhow!("{error}"))
        } else {
            self.document.checkpoint().map_err(Into::into)
        }.and_then(|mut bytes| {
            bytes.extend_from_slice(&self.continuation()?);
            Ok(bytes)
        });
        let (failed, bytes) = match native {
            Ok(bytes) => (false, bytes),
            Err(error) => (true, error.to_string().into_bytes()),
        };
        let mut out = Vec::with_capacity(SNAPSHOT_MAGIC.len() + 9 + grid.len() + bytes.len());
        out.extend_from_slice(SNAPSHOT_MAGIC);
        out.push(u8::from(failed));
        out.extend_from_slice(&(grid.len() as u64).to_le_bytes());
        out.extend_from_slice(grid);
        out.extend_from_slice(&bytes);
        out
    }

    /// Old binary snapshots remain readable; the holder treats this envelope as opaque.
    pub(super) fn restore<'a>(&mut self, bytes: &'a [u8]) -> Result<&'a [u8]> {
        *self = Self::default();
        let Some(body) = bytes.strip_prefix(SNAPSHOT_MAGIC) else { return Ok(bytes) };
        let (&failed, rest) = body.split_first().context("truncated native VT snapshot")?;
        let length = rest.get(..8).context("truncated native VT grid length")?;
        let length = usize::try_from(u64::from_le_bytes(length.try_into()?))?;
        let grid = rest.get(8..8usize.checked_add(length).context("invalid native VT grid length")?)
            .context("truncated native VT grid")?;
        let native = &rest[8 + length..];
        if failed != 0 {
            self.error = Some(String::from_utf8(native.to_vec()).context("invalid native VT error")?);
        } else {
            let Self { parser, document, error, .. } = self;
            parser.push(native, |_, token| match token {
                Token::Tsp(message) => {
                    if let Err(problem) = document.apply(&message)
                        && document.replay().is_err()
                    {
                        error.get_or_insert_with(|| problem.to_string());
                    }
                }
                Token::Error(_) => {}
                Token::Reset => *document = State::default(),
                Token::Da1 => {}
            });
            if let Some(problem) = parser.failure() {
                error.get_or_insert_with(|| problem.to_string());
            }
        }
        Ok(grid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtproto::read_frame;

    fn terminal() -> Terminal<VtHandler> {
        Terminal::new(80, 24, VtHandler {
            out: Vec::new(), suppress: false, scheme: None,
            version: "falcon native test".into(), held_reply: None,
        }).unwrap()
    }

    fn replies(term: &mut Terminal<VtHandler>) -> Vec<u8> {
        let frames = std::mem::take(&mut term.handler_mut().out);
        let mut input = std::io::Cursor::new(frames);
        let mut bytes = Vec::new();
        while let Some((kind, payload)) = read_frame(&mut input).unwrap() {
            if let FromVt::PtyWrite(reply) = FromVt::decode(kind, payload).unwrap() {
                bytes.extend_from_slice(&reply);
            }
        }
        bytes
    }

    const HELLO: &[u8] = b"\x1b_tsp;q;{\"q\":\"hello\",\"v\":[1],\"app\":\"omp\"}\x1b\\\x1b[c";
    const OPEN: &[u8] = b"\x1b_tsp;o;{\"id\":\"s:0\",\"mode\":\"inline\"}\x1b\\";

    #[test]
    fn native_hello_preserves_the_da1_fifo_until_open() {
        let mut native = NativeState::default();
        let mut term = terminal();
        native.feed(&mut term, b"\x1b[c", false);
        let real_reply = replies(&mut term);
        assert!(real_reply.starts_with(b"\x1b[?"));
        for byte in HELLO { native.feed(&mut term, &[*byte], false); }
        native.feed(&mut term, b"\x1b[c\x1b[0c", false);
        assert!(replies(&mut term).is_empty());
        native.feed(&mut term, OPEN, false);
        assert!(native.timeout().is_none());
        assert_eq!(replies(&mut term), real_reply.repeat(3));
        native.feed(&mut term, b"\x1b[c", false);
        assert_eq!(replies(&mut term), real_reply);
    }

    #[test]
    fn hello_does_not_defer_a_cursor_reply_in_the_same_feed() {
        let mut native = NativeState::default();
        let mut term = terminal();
        let hello = &HELLO[..HELLO.len() - 3];
        native.feed(&mut term, hello, false);
        native.feed(&mut term, b"\x1b[6n\x1b[c", false);
        assert_eq!(replies(&mut term), b"\x1b[1;1R");
        native.feed(&mut term, OPEN, false);
        native.expire(&mut term);
        assert!(replies(&mut term).starts_with(b"\x1b[?"));
    }

    #[test]
    fn absent_native_terminal_eventually_gets_the_real_vt_reply() {
        let mut native = NativeState::default();
        let mut term = terminal();
        native.feed(&mut term, b"\x1b[c", false);
        let real_reply = replies(&mut term);
        native.feed(&mut term, HELLO, false);
        assert!(replies(&mut term).is_empty());
        native.deferred.as_mut().unwrap().0 = Instant::now();
        native.expire(&mut term);
        assert_eq!(replies(&mut term), real_reply);
        native.expire(&mut term);
        assert!(replies(&mut term).is_empty());
    }

    #[test]
    fn hot_restore_keeps_native_document_and_partial_frame() {
        let mut native = NativeState::default();
        let mut term = terminal();
        native.feed(&mut term, OPEN, false);
        native.feed(&mut term, b"\x1b_tsp;f;{\"sf\":\"s:0\",\"s\":1,\"ops\":[[\"add\",\"main\",\"s:0\",null,{\"id\":\"main\",\"k\":\"col\"}]]}\x1b\\", false);
        let next = b"\x1b_tsp;f;{\"sf\":\"s:0\",\"s\":2,\"ops\":[[\"add\",\"answer\",\"main\",null,{\"id\":\"answer\",\"k\":\"text\",\"p\":{\"text\":\"kept\"}}]]}\x1b\\";
        let split = next.len() / 2;
        native.feed(&mut term, &next[..split], false);
        let encoded = native.encode(b"old-grid");
        let mut restored = NativeState::default();
        assert_eq!(restored.restore(&encoded).unwrap(), b"old-grid");
        restored.feed(&mut term, &next[split..], false);
        let replay = String::from_utf8(restored.replay_with_limit(falcon_proto::tsp::MAX_SNAPSHOT_BYTES).unwrap()).unwrap();
        assert!(replay.contains("kept"));
        assert!(replay.contains("answer"));
        assert_eq!(restored.restore(b"legacy-grid").unwrap(), b"legacy-grid");
        assert!(restored.replay_with_limit(falcon_proto::tsp::MAX_SNAPSHOT_BYTES).unwrap().is_empty());
    }
}
