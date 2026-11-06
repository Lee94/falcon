//! Filter outer-terminal query replies before forwarding Tern viewer input to the PTY.
//!
//! Only seven-bit (`ESC`-introduced) control sequences are interpreted: treating C1 bytes as
//! controls would corrupt UTF-8 input. Unknown sequences, TSP APC / OSC 877 messages, focus,
//! mouse and keyboard events pass through unchanged. Bracketed paste is entirely literal.
//!
//! CPR and modified F3 share the same encoding. Call [`ReplyFilter::observe_output`] before
//! actually writing output/replay to the viewer; CPR is filtered only while a corresponding
//! `CSI 6 n` / `CSI ? 6 n` query is outstanding. An F3 event arriving during that short query
//! window is inherently indistinguishable from its reply. Bound that window with the same
//! short input deadline used for Escape (for example, 30 ms): whenever [`ReplyFilter::has_pending`]
//! is true, arrange a deadline and call [`ReplyFilter::flush_pending`] on expiry. Forward its
//! returned bytes too, including at EOF. A deadline must not be continually postponed by
//! incoming data, otherwise an unanswered query or lone Escape could stay pending forever.
//!
//! Known string replies are classified by their protocol prefix and discarded through their
//! terminator, without retaining their payload. Other strings stream through immediately.
//! Incomplete or overlong unclassified sequences fail open. This is a wire-syntax filter,
//! not a mechanism for authenticating which bytes were physically typed by a person.

const ESC: u8 = 0x1b;
const MAX_PENDING: usize = 256;
const MAX_CPR_QUERIES: u8 = 32;
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringKind {
    Osc,
    Dcs,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    Csi,
    StringPrefix(StringKind),
    String { kind: StringKind, discard: bool, escaped: bool },
    CsiPass,
    Mouse(u8),
    Paste(usize),
}

#[derive(Clone, Copy, Debug, Default)]
enum OutputState {
    #[default]
    Ground,
    Esc,
    Csi,
    String { kind: StringKind, escaped: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    Pending,
    Reply,
    Other,
}

/// Stateful, bounded filter shared by native Tern attach and WebSocket viewers.
///
/// Create one instance per viewer input stream and retain it across reads. No allocation is
/// performed until bytes need to be buffered or returned. Ordinary input is copied only once
/// into the required return value; string payloads are never accumulated.
#[derive(Debug, Default)]
pub struct ReplyFilter {
    state: State,
    pending: Vec<u8>,
    output_state: OutputState,
    output_csi: Vec<u8>,
    cpr_queries: [u8; 2],
}

impl ReplyFilter {
    /// Observe the exact bytes about to be sent to the outer terminal, including replay.
    ///
    /// Only cursor-position queries are tracked, because their replies collide with keyboard
    /// events. Queries embedded in control-string payloads are not interpreted. Tracking is
    /// bounded to 32 normal and 32 private queries; excess queries are coalesced at that limit.
    pub fn observe_output(&mut self, bytes: &[u8]) {
        if matches!(self.output_state, OutputState::Ground) && !bytes.contains(&ESC) {
            return;
        }
        for &byte in bytes {
            match self.output_state {
                OutputState::Ground => {
                    if byte == ESC {
                        self.output_state = OutputState::Esc;
                    }
                }
                OutputState::Esc => match byte {
                    b'[' => {
                        self.output_csi.clear();
                        self.output_state = OutputState::Csi;
                    }
                    b']' | b'P' | b'_' | b'^' | b'X' => {
                        self.output_state = OutputState::String {
                            kind: string_kind(byte),
                            escaped: false,
                        };
                    }
                    ESC => {}
                    _ => self.output_state = OutputState::Ground,
                },
                OutputState::Csi => {
                    if byte == ESC {
                        self.output_csi.clear();
                        self.output_state = OutputState::Esc;
                    } else if (0x40..=0x7e).contains(&byte) {
                        if byte == b'n' {
                            let index = match self.output_csi.as_slice() {
                                b"6" => Some(0),
                                b"?6" => Some(1),
                                _ => None,
                            };
                            if let Some(index) = index {
                                self.cpr_queries[index] =
                                    self.cpr_queries[index].saturating_add(1).min(MAX_CPR_QUERIES);
                            }
                        }
                        self.output_csi.clear();
                        self.output_state = OutputState::Ground;
                    } else if (0x20..=0x3f).contains(&byte) && self.output_csi.len() < MAX_PENDING {
                        self.output_csi.push(byte);
                    } else {
                        self.output_csi.clear();
                        self.output_state = OutputState::Ground;
                    }
                }
                OutputState::String { kind, escaped } => {
                    self.output_state = if string_end(kind, escaped, byte) {
                        OutputState::Ground
                    } else {
                        OutputState::String { kind, escaped: byte == ESC }
                    };
                }
            }
        }
    }

    /// Filter one input chunk. Sequences may be split at any byte, including UTF-8 boundaries.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.state == State::Ground && !bytes.contains(&ESC) {
            return bytes.to_vec();
        }
        let mut output = Vec::with_capacity(bytes.len() + self.pending.len());
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            match self.state {
                State::Ground => {
                    let end = bytes[index..].iter().position(|&b| b == ESC)
                        .map_or(bytes.len(), |offset| index + offset);
                    output.extend_from_slice(&bytes[index..end]);
                    index = end;
                    if index == bytes.len() {
                        break;
                    }
                    self.pending.push(ESC);
                    self.state = State::Esc;
                }
                State::Esc => match byte {
                    b'[' => {
                        self.pending.push(byte);
                        self.state = State::Csi;
                    }
                    b']' | b'P' => {
                        self.pending.push(byte);
                        self.state = State::StringPrefix(string_kind(byte));
                    }
                    b'_' | b'^' | b'X' => {
                        self.pending.push(byte);
                        self.release(&mut output);
                        self.state = State::String {
                            kind: StringKind::Other,
                            discard: false,
                            escaped: false,
                        };
                    }
                    ESC => {
                        self.release(&mut output);
                        self.pending.push(ESC);
                    }
                    _ => {
                        self.pending.push(byte);
                        self.release(&mut output);
                        self.state = State::Ground;
                    }
                },
                State::Csi => {
                    if byte == ESC {
                        self.release(&mut output);
                        self.pending.push(ESC);
                        self.state = State::Esc;
                    } else {
                        self.pending.push(byte);
                        if (0x40..=0x7e).contains(&byte) {
                            let paste = self.pending == b"\x1b[200~";
                            let mouse = self.pending == b"\x1b[M";
                            if self.is_csi_reply() {
                                self.pending.clear();
                            } else {
                                self.release(&mut output);
                            }
                            self.state = if paste {
                                State::Paste(0)
                            } else if mouse {
                                State::Mouse(3)
                            } else {
                                State::Ground
                            };
                        } else if !(0x20..=0x3f).contains(&byte) {
                            self.release(&mut output);
                            self.state = State::Ground;
                        } else if self.pending.len() == MAX_PENDING {
                            self.release(&mut output);
                            self.state = State::CsiPass;
                        }
                    }
                }
                State::StringPrefix(kind) => {
                    self.pending.push(byte);
                    if string_end(kind, false, byte) {
                        self.release(&mut output);
                        self.state = State::Ground;
                    } else {
                        let prefix = string_prefix(kind, &self.pending[2..]);
                        if prefix != Prefix::Pending {
                            if prefix == Prefix::Reply {
                                self.pending.clear();
                            } else {
                                self.release(&mut output);
                            }
                            self.state = State::String {
                                kind,
                                discard: prefix == Prefix::Reply,
                                escaped: byte == ESC,
                            };
                        }
                    }
                }
                State::String { kind, discard, escaped } => {
                    if !discard {
                        output.push(byte);
                    }
                    self.state = if string_end(kind, escaped, byte) {
                        State::Ground
                    } else {
                        State::String { kind, discard, escaped: byte == ESC }
                    };
                }
                State::CsiPass => {
                    if byte == ESC {
                        self.pending.push(ESC);
                        self.state = State::Esc;
                    } else {
                        output.push(byte);
                        if !(0x20..=0x3f).contains(&byte) {
                            self.state = State::Ground;
                        }
                    }
                }
                State::Mouse(remaining) => {
                    output.push(byte);
                    self.state = if remaining == 1 { State::Ground } else { State::Mouse(remaining - 1) };
                }
                State::Paste(matched) => {
                    output.push(byte);
                    let matched = if byte == PASTE_END[matched] {
                        matched + 1
                    } else {
                        usize::from(byte == ESC)
                    };
                    self.state = if matched == PASTE_END.len() { State::Ground } else { State::Paste(matched) };
                }
            }
            index += 1;
        }
        output
    }

    /// Whether an Escape/prefix or cursor-query window needs the caller's short deadline.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() || self.cpr_queries != [0, 0]
    }

    /// Expire cursor queries and release any incomplete input prefix literally.
    ///
    /// Call on the input deadline and EOF, and forward the returned bytes. Flushing never
    /// leaves bracketed paste: an incomplete paste terminator was already forwarded and its
    /// matching state must survive a pause inside the user's paste. Streaming strings whose
    /// prefixes have already been classified likewise retain their framing state.
    pub fn flush_pending(&mut self) -> Vec<u8> {
        self.cpr_queries = [0, 0];
        if self.pending.is_empty() {
            return Vec::new();
        }
        self.state = State::Ground;
        let mut output = Vec::with_capacity(self.pending.len());
        self.release(&mut output);
        output
    }

    fn release(&mut self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.pending);
        self.pending.clear();
    }

    fn is_csi_reply(&mut self) -> bool {
        let sequence = &self.pending[2..];
        let (&final_byte, params) = sequence.split_last().expect("CSI includes a final byte");
        match final_byte {
            // Empty CSI fields mean the default parameter. Tern 0.9.12 sends
            // DA1 as CSI ?62;52;c, including its final empty field.
            b'c' => matches!(params.first(), Some(b'?' | b'>' | b'='))
                && params[1..].iter().all(|byte| byte.is_ascii_digit() || *byte == b';'),
            b'R' => {
                let private = params.first() == Some(&b'?');
                let coordinates = if private { &params[1..] } else { params };
                let fields = coordinates.split(|&b| b == b';');
                let count = fields.clone().count();
                if (count == 2 || (private && count == 3))
                    && fields.clone().all(positive_number)
                    && self.cpr_queries[usize::from(private)] != 0
                {
                    self.cpr_queries[usize::from(private)] -= 1;
                    true
                } else {
                    false
                }
            }
            b'y' => {
                let params = params.strip_prefix(b"?").unwrap_or(params);
                let Some(params) = params.strip_suffix(b"$") else { return false };
                let mut fields = params.split(|&b| b == b';');
                matches!((fields.next(), fields.next(), fields.next()),
                    (Some(mode), Some([b'0'..=b'4']), None) if number(mode))
            }
            b'n' => matches!(params, b"0" | b"3" | b"?10" | b"?11" | b"?13" | b"?20" | b"?21"
                | b"?50" | b"?53" | b"?57" | b"?58" | b"?70" | b"?71" | b"?73"
                | b"?80" | b"?81" | b"?83" | b"?997;1" | b"?997;2")
                || params.strip_prefix(b"?27;").is_some_and(numeric_list),
            b'u' => params.strip_prefix(b"?").is_some_and(number),
            b't' => {
                let mut fields = params.split(|&b| b == b';');
                matches!((fields.next(), fields.next(), fields.next(), fields.next()),
                    (Some(b"1" | b"2"), None, None, None)
                    | (Some(b"3" | b"4" | b"5" | b"6" | b"8" | b"9"), Some(_), Some(_), None))
                    && numeric_list(params)
            }
            _ => false,
        }
    }
}

fn number(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

fn positive_number(bytes: &[u8]) -> bool {
    number(bytes) && bytes.iter().any(|&b| b != b'0')
}

fn numeric_list(bytes: &[u8]) -> bool {
    bytes.split(|&b| b == b';').all(number)
}

fn string_kind(byte: u8) -> StringKind {
    match byte {
        b']' => StringKind::Osc,
        b'P' => StringKind::Dcs,
        _ => StringKind::Other,
    }
}

fn string_end(kind: StringKind, escaped: bool, byte: u8) -> bool {
    (escaped && byte == b'\\') || (kind == StringKind::Osc && byte == 0x07)
        || byte == 0x18 || byte == 0x1a
}

fn string_prefix(kind: StringKind, bytes: &[u8]) -> Prefix {
    let prefixes: &[&[u8]] = match kind {
        StringKind::Osc => &[
            b"10;rgb:", b"11;rgb:", b"12;rgb:",
            b"10;rgba:", b"11;rgba:", b"12;rgba:",
            b"10;rgbi:", b"11;rgbi:", b"12;rgbi:",
            b"10;#", b"11;#", b"12;#",
        ],
        StringKind::Dcs => &[b"0+r", b"1+r", b"0$r", b"1$r", b">|", b"!|"],
        StringKind::Other => return Prefix::Other,
    };
    if prefixes.contains(&bytes) {
        Prefix::Reply
    } else if prefixes.iter().any(|prefix| prefix.starts_with(bytes)) {
        Prefix::Pending
    } else {
        Prefix::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_chunks(chunks: &[&[u8]], output: &[u8]) -> Vec<u8> {
        let mut filter = ReplyFilter::default();
        filter.observe_output(output);
        let mut result = Vec::new();
        for chunk in chunks {
            result.extend(filter.push(chunk));
        }
        result.extend(filter.flush_pending());
        result
    }

    fn every_split(input: &[u8], expected: &[u8], output: &[u8]) {
        for split in 0..=input.len() {
            assert_eq!(filter_chunks(&[&input[..split], &input[split..]], output), expected, "split {split}");
        }
        let chunks: Vec<_> = input.chunks(1).collect();
        assert_eq!(filter_chunks(&chunks, output), expected, "one-byte chunks");
    }

    #[test]
    fn tern_da1_with_an_omitted_parameter_cannot_end_native_negotiation() {
        let wire = b"\x1b[?62;52;c\x1b_tsp;r;{\"r\":\"hello\",\"v\":1}\x1b\\\x1b[?62;52;cok";
        every_split(wire, b"\x1b_tsp;r;{\"r\":\"hello\",\"v\":1}\x1b\\ok", b"");
        let pasted = b"\x1b[200~\x1b[?62;52;c\x1b[201~";
        every_split(pasted, pasted, b"");
    }

    #[test]
    fn ordinary_keyboard_mouse_focus_utf8_and_tsp_are_unchanged() {
        let inputs: &[&[u8]] = &[
            "typing 中文🦅\r\n\t\u{7f}".as_bytes(),
            b"\xc2\x9b\xc2\x9d\xc2\x90\xc2\x9f\x1b[12;34R",
            b"\x1bx\x1b[A\x1b[1;5D\x1b[1;2R\x1bOP\x1b[15;2~\x1b[97;5u",
            b"\x1b[<0;20;3M\x1b[<0;20;3m\x1b[M !!\x1b[I\x1b[O",
            b"\x1b_Greply\x1b[?1;2c\x1b\\\x1b]877;event;\x1b[1;2R\x07",
            b"\x1b]52;c;YWJj\x07\x1bPunknown\x1b[?1c\x1b\\",
            b"\x1b[?999n\x1b[?6n\x1b[6n\x1b[18t\x1b[c\x1b]10;?\x07",
        ];
        for input in inputs {
            every_split(input, input, b"");
        }
    }

    #[test]
    fn known_replies_are_removed_at_every_boundary() {
        let replies: &[&[u8]] = &[
            b"\x1b[?62;22c", b"\x1b[>0;100;0c", b"\x1b[=1;2c",
            b"\x1b[12;34R", b"\x1b[?12;34;1R", b"\x1b[?2004;1$y", b"\x1b[4;0$y",
            b"\x1b[0n", b"\x1b[3n", b"\x1b[?997;2n", b"\x1b[?27;1;0;0n",
            b"\x1b[?31u", b"\x1b[4;600;800t", b"\x1b[8;24;80t",
            b"\x1b]10;rgb:ffff/0000/abcd\x07", b"\x1b]11;rgb:0000/1111/2222\x1b\\",
            b"\x1b]12;#abcdef\x07", b"\x1bP1+r544e=7465726e\x1b\\",
            b"\x1bP0+r544e\x1b\\", b"\x1bP1$r0m\x1b\\", b"\x1bP0$r\x1b\\",
            b"\x1bP>|tern 1.0\x1b\\", b"\x1bP!|01234567\x1b\\",
        ];
        for reply in replies {
            let mut input = b"before".to_vec();
            input.extend_from_slice(reply);
            input.extend_from_slice(b"after");
            every_split(&input, b"beforeafter", b"\x1b[6n\x1b[?6n");
        }
    }

    #[test]
    fn paste_contents_are_literal_across_all_boundaries_and_deadlines() {
        let input = b"\x1b[200~\x1b[?62c\x1b[1;2R\x1b]10;rgb:aa/bb/cc\x07\x1bP1$r0m\x1b\\\x1b\x1b[201~\x1b[?62c";
        let expected = &input[..input.len() - b"\x1b[?62c".len()];
        every_split(input, expected, b"\x1b[6n");
        let mut filter = ReplyFilter::default();
        assert_eq!(filter.push(b"\x1b[200~paste\x1b[20"), b"\x1b[200~paste\x1b[20");
        assert!(filter.flush_pending().is_empty());
        assert_eq!(filter.push(b"1~\x1b[?1c"), b"1~");
    }

    #[test]
    fn cpr_requires_a_real_matching_query_and_expires() {
        let mut filter = ReplyFilter::default();
        assert_eq!(filter.push(b"\x1b[1;2R"), b"\x1b[1;2R");
        filter.observe_output(b"\x1b[?6n");
        assert_eq!(filter.push(b"\x1b[1;2R"), b"\x1b[1;2R");
        assert!(filter.push(b"\x1b[?1;2R").is_empty());
        assert_eq!(filter.push(b"\x1b[?1;2R"), b"\x1b[?1;2R");
        filter.observe_output(b"\x1b[6n");
        assert!(filter.has_pending());
        assert!(filter.flush_pending().is_empty());
        assert!(!filter.has_pending());
        assert_eq!(filter.push(b"\x1b[1;2R"), b"\x1b[1;2R");
    }

    #[test]
    fn output_queries_can_split_at_every_byte_but_not_inside_strings() {
        let query = b"\x1b[6n\x1b[?6n";
        for split in 0..=query.len() {
            let mut filter = ReplyFilter::default();
            filter.observe_output(&query[..split]);
            filter.observe_output(&query[split..]);
            assert!(filter.push(b"\x1b[1;2R\x1b[?3;4R").is_empty());
            assert!(!filter.has_pending());
        }
        let mut filter = ReplyFilter::default();
        let output = b"\x1b_G\x1b[6n\x1b\\\x1b]877;\x1b[?6n\x07\x1bP1+r\x1b[6n\x1b\\";
        for chunk in output.chunks(1) {
            filter.observe_output(chunk);
        }
        assert!(!filter.has_pending());
        assert_eq!(filter.push(b"\x1b[1;2R\x1b[?3;4R"), b"\x1b[1;2R\x1b[?3;4R");
    }

    #[test]
    fn escape_and_incomplete_prefixes_are_released_on_deadline() {
        for prefix in [b"\x1b".as_slice(), b"\x1b[", b"\x1b[12;", b"\x1b]10;rg", b"\x1bP1$"] {
            let mut filter = ReplyFilter::default();
            assert!(filter.push(prefix).is_empty());
            assert!(filter.has_pending());
            assert_eq!(filter.flush_pending(), prefix);
            assert!(!filter.has_pending());
            assert_eq!(filter.push(b"x"), b"x");
        }
        every_split(b"\x1b\x1bx", b"\x1b\x1bx", b"");
    }

    #[test]
    fn memory_is_bounded_for_long_sequences_and_payloads() {
        let mut filter = ReplyFilter::default();
        let mut csi = b"\x1b[".to_vec();
        csi.extend(std::iter::repeat_n(b'1', MAX_PENDING * 4));
        csi.push(b'c');
        assert_eq!(filter.push(&csi), csi);
        assert!(filter.pending.len() <= MAX_PENDING);
        assert!(filter.push(b"\x1b]10;rgb:").is_empty());
        let payload = vec![b'a'; MAX_PENDING * 8];
        assert!(filter.push(&payload).is_empty());
        assert!(filter.pending.is_empty());
        assert_eq!(filter.push(b"\x1b\\x"), b"x");
        assert_eq!(filter.push(b"\x1b_G"), b"\x1b_G");
        assert_eq!(filter.push(&payload), payload);
        assert!(filter.pending.is_empty());
        assert_eq!(filter.push(b"\x1b\\"), b"\x1b\\");
        for _ in 0..100 {
            filter.observe_output(b"\x1b[6n\x1b[?6n");
        }
        assert_eq!(filter.cpr_queries, [MAX_CPR_QUERIES; 2]);
    }
}
