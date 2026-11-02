//! What a file is and how it can be shown.
//!
//! [`detect`] names the format, the programming language when there is one, the views the
//! format offers and the one to open first. Every reader in this crate has explicit limits
//! and never executes, fetches or writes anything.

pub mod bibtex;
pub mod binary;
pub mod csv;
pub mod decoded;
pub mod detect;
pub mod document;
pub mod inspect;
pub mod language;
pub mod markdown;
pub mod notebook;
pub mod pdf;
pub mod pretty;
pub mod registry;
pub mod signature;
pub mod table;
pub mod tiff;
pub mod tree;

pub use detect::{Detection, detect};
pub use registry::{Spec, View};

/// Standard base64, for `data:` URIs built from bytes.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        for (input, out) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
            (&[0xFF, 0xFE, 0x00], "//4A"),
        ] {
            assert_eq!(super::base64(input), out);
        }
    }
}
