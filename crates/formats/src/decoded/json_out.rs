//! A small pretty JSON writer for binary formats shown as JSON (MessagePack, Protobuf). The text
//! it builds is what the JSON tree and source views show, so it must always be valid JSON.

/// Text written at most; decoding stops with an error beyond it.
pub(crate) const MAX_TEXT: usize = 256 * 1024 * 1024;

pub(crate) struct JsonOut {
    pub(crate) text: String,
}

impl JsonOut {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
        }
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.text.len() > MAX_TEXT {
            Err("The decoded text would exceed 256 MiB".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn newline(&mut self, depth: usize) {
        self.text.push('\n');
        for _ in 0..depth.min(64) {
            self.text.push_str("  ");
        }
    }
    pub(crate) fn string(&mut self, s: &str) {
        self.text.push('"');
        for c in s.chars() {
            match c {
                '"' => self.text.push_str("\\\""),
                '\\' => self.text.push_str("\\\\"),
                '\n' => self.text.push_str("\\n"),
                '\r' => self.text.push_str("\\r"),
                '\t' => self.text.push_str("\\t"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    self.text.push_str(&format!("\\u{:04x}", c as u32))
                }
                c => self.text.push(c),
            }
        }
        self.text.push('"');
    }
    /// A float as JSON: NaN and infinities are not numbers in JSON, so they become strings.
    pub(crate) fn float(&mut self, f: f64) {
        if f.is_finite() {
            self.text.push_str(&format!("{f:?}"));
        } else {
            self.string(&format!("{f}"));
        }
    }
    pub(crate) fn raw(&mut self, s: &str) {
        self.text.push_str(s);
    }
}

/// Bytes as a readable string: "<12 bytes: 0a0b…>" (the first 64 bytes in hex).
pub(crate) fn bytes_label(b: &[u8]) -> String {
    let mut s = format!("<{} bytes: ", b.len());
    for x in b.iter().take(64) {
        s.push_str(&format!("{x:02x}"));
    }
    if b.len() > 64 {
        s.push('…');
    }
    s.push('>');
    s
}
