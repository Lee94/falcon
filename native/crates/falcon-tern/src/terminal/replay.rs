use std::io::{self, Write};

// A native terminal answers device/cursor/color queries itself. Historical queries
// must not be replayed into its input stream and then forwarded into the live PTY.
// Keep all other bytes verbatim, including invalid UTF-8 and ordinary OSC titles.
pub(super) fn write(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let mut start = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != 0x1b {
            cursor += 1;
            continue;
        }
        let Some(&kind) = bytes.get(cursor + 1) else { break };
        let (end, query) = match kind {
            b'Z' => (cursor + 2, true),
            b'[' => {
                let body = cursor + 2;
                let Some(last) = (body..bytes.len()).find(|index| (0x40..=0x7e).contains(&bytes[*index])) else { break };
                (last + 1, csi_query(&bytes[body..last], bytes[last]))
            }
            b']' | b'P' => {
                let body = cursor + 2;
                let mut last = body;
                while last < bytes.len() {
                    if kind == b']' && bytes[last] == 7 { break; }
                    if bytes[last] == 0x1b && bytes.get(last + 1) == Some(&b'\\') { break; }
                    last += 1;
                }
                if last == bytes.len() { break; }
                let payload = &bytes[body..last];
                let query = if kind == b']' {
                    payload.ends_with(b";?")
                } else {
                    payload.starts_with(b"$q") || payload.starts_with(b"+q")
                };
                (last + if bytes[last] == 7 { 1 } else { 2 }, query)
            }
            _ => (cursor + 2, false),
        };
        if query {
            output.write_all(&bytes[start..cursor])?;
            start = end;
        }
        cursor = end;
    }
    output.write_all(&bytes[start..])
}

fn csi_query(parameters: &[u8], final_byte: u8) -> bool {
    match final_byte {
        b'c' | b'n' | b'x' => true,
        b'p' => parameters.contains(&b'$'),
        b'u' => parameters == b"?",
        b'q' => parameters == b">",
        b'y' => parameters.contains(&b'*'),
        b't' => matches!(parameters.split(|byte| *byte == b';').next(),
            Some(b"11" | b"13" | b"14" | b"15" | b"16" | b"18" | b"19" | b"20" | b"21")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_keeps_painting_and_strips_historical_queries() {
        let bytes = b"\x1b[31mhello\xff\x1b[0m\x1b[6n\x1b[c\x1b[18t\x1b[?2004$p\x1b]10;?\x07\x1bP$qm\x1b\\\x1b]0;title\x07\x1b[Hworld";
        let mut output = Vec::new();
        write(&mut output, bytes).unwrap();
        assert_eq!(output, b"\x1b[31mhello\xff\x1b[0m\x1b]0;title\x07\x1b[Hworld");
    }

    #[test]
    fn string_escape_payload_is_not_parsed_as_csi() {
        let bytes = b"\x1b]0;literal \x1b[6n\x07\x1b[!p\x1b[8;24;80t\x1b[?2004h";
        let mut output = Vec::new();
        write(&mut output, bytes).unwrap();
        assert_eq!(output, bytes);
    }
}
