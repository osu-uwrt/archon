//! Turns the raw bytes of a PTY or serial port into display text. ANSI escape sequences are stripped, and a UTF-8
//! sequence split across two reads is held back until the rest of it arrives.

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    /// After ESC
    Esc,
    /// After `ESC (` or `ESC )`, which select a character set with one more byte
    Charset,
    /// Inside `ESC [ ...`, which ends at a byte in 0x40..=0x7e
    Csi,
    /// Inside `ESC ] ...`, which ends at BEL or `ESC \`
    Osc,
    /// After ESC inside an OSC sequence
    OscEsc,
}

#[derive(Default)]
pub struct TextDecoder {
    escape: Escape,
    pending: Vec<u8>,
}

impl TextDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> String {
        let mut kept = std::mem::take(&mut self.pending);
        for &b in bytes {
            self.escape = match self.escape {
                Escape::None => match b {
                    0x1b => Escape::Esc,
                    // Keep line structure and tabs; drop the other control bytes (bells, backspaces)
                    b'\n' | b'\r' | b'\t' => {
                        kept.push(b);
                        Escape::None
                    }
                    0x00..=0x1f | 0x7f => Escape::None,
                    _ => {
                        kept.push(b);
                        Escape::None
                    }
                },
                Escape::Esc => match b {
                    b'[' => Escape::Csi,
                    b']' => Escape::Osc,
                    b'(' | b')' => Escape::Charset,
                    _ => Escape::None,
                },
                Escape::Charset => Escape::None,
                Escape::Csi if (0x40..=0x7e).contains(&b) => Escape::None,
                Escape::Csi => Escape::Csi,
                Escape::Osc => match b {
                    0x07 => Escape::None,
                    0x1b => Escape::OscEsc,
                    _ => Escape::Osc,
                },
                Escape::OscEsc if b == b'\\' => Escape::None,
                Escape::OscEsc => Escape::Osc,
            };
        }

        // Hold back an incomplete UTF-8 sequence at the end for the next read. Invalid bytes elsewhere become U+FFFD.
        let complete = match std::str::from_utf8(&kept) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => kept.len(),
        };
        self.pending = kept.split_off(complete);
        String::from_utf8_lossy(&kept).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_codes() {
        let mut d = TextDecoder::new();
        assert_eq!(d.feed(b"\x1b[31m[ERROR]\x1b[0m bad\n"), "[ERROR] bad\n");
    }

    #[test]
    fn escape_split_across_reads() {
        let mut d = TextDecoder::new();
        assert_eq!(d.feed(b"a\x1b[3"), "a");
        assert_eq!(d.feed(b"3mb"), "b");
    }

    #[test]
    fn osc_title_is_dropped() {
        let mut d = TextDecoder::new();
        assert_eq!(d.feed(b"\x1b]0;title\x07x\x1b]2;t\x1b\\y"), "xy");
    }

    #[test]
    fn utf8_split_across_reads() {
        let mut d = TextDecoder::new();
        let bytes = "°C".as_bytes();
        assert_eq!(d.feed(&bytes[..1]), "");
        assert_eq!(d.feed(&bytes[1..]), "°C");
    }

    #[test]
    fn invalid_bytes_are_replaced() {
        let mut d = TextDecoder::new();
        assert_eq!(d.feed(b"a\xffb"), "a\u{fffd}b");
    }
}
