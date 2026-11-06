/// stdin reads may split a UTF-8 code point. Keep only the unfinished suffix;
/// replace genuinely invalid bytes without corrupting a following valid character.
#[derive(Default)]
pub(super) struct Utf8Input {
    pending: Vec<u8>,
}

impl Utf8Input {
    pub(super) fn push(&mut self, mut bytes: Vec<u8>) -> String {
        if !self.pending.is_empty() {
            self.pending.append(&mut bytes);
            std::mem::swap(&mut bytes, &mut self.pending);
        }
        let mut bytes = match String::from_utf8(bytes) {
            Ok(text) => return text,
            Err(error) => error.into_bytes(),
        };
        let mut cursor = 0;
        while cursor < bytes.len() {
            match std::str::from_utf8(&bytes[cursor..]) {
                Ok(_) => break,
                Err(error) => {
                    cursor += error.valid_up_to();
                    match error.error_len() {
                        Some(size) => cursor += size,
                        None => {
                            self.pending.extend_from_slice(&bytes[cursor..]);
                            bytes.truncate(cursor);
                            break;
                        }
                    }
                }
            }
        }
        match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_boundary_preserves_utf8_and_escape_sequences() {
        let bytes = "a中🦅\x1b[A\x00".as_bytes();
        for boundary in 0..=bytes.len() {
            let mut input = Utf8Input::default();
            let mut text = input.push(bytes[..boundary].to_vec());
            text.push_str(&input.push(bytes[boundary..].to_vec()));
            assert_eq!(text.as_bytes(), bytes);
        }
        let mut input = Utf8Input::default();
        let mut text = String::new();
        for byte in bytes { text.push_str(&input.push(vec![*byte])); }
        assert_eq!(text.as_bytes(), bytes);
    }

    #[test]
    fn invalid_bytes_do_not_consume_following_valid_input() {
        let mut input = Utf8Input::default();
        assert_eq!(input.push(b"\xf0\x9f".to_vec()), "");
        assert_eq!(input.push(b"x\xffok".to_vec()), "�x�ok");
        assert_eq!(input.push("中".as_bytes().to_vec()), "中");
    }
}
