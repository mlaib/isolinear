//! Check uncompressed dictionaries before hayro can try the empty password or build pages.
//! Encryption dictionaries and trailer keys cannot be inside encrypted object streams.
use hayro::hayro_syntax::object::{Dict, FromBytes, Name};

const MAX_DICTIONARY: usize = 1024 * 1024;
const MAX_DICTIONARIES: usize = 1_000_000;

pub(super) fn check(data: &[u8]) -> Result<(), String> {
    if !data
        .get(..data.len().min(2000))
        .unwrap_or_default()
        .windows(5)
        .any(|w| w == b"%PDF-")
    {
        return Err("Expected a PDF header near the start of the file".into());
    }
    let mut at = 0;
    let mut dictionaries = 0;
    let mut pages = 0;
    while let Some(&byte) = data.get(at) {
        match byte {
            b'%' => {
                at += 1;
                while data.get(at).is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
                    at += 1;
                }
            }
            b'(' => {
                at += 1;
                let mut depth = 1usize;
                while let Some(&b) = data.get(at) {
                    at += 1;
                    match b {
                        b'\\' => at = (at + 1).min(data.len()),
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'<' if data.get(at + 1) == Some(&b'<') => {
                dictionaries += 1;
                if dictionaries > MAX_DICTIONARIES {
                    return Err("The PDF exceeds the limit of 1,000,000 dictionaries".into());
                }
                let end = at.saturating_add(MAX_DICTIONARY).min(data.len());
                let Some(dict) = data.get(at..end).and_then(Dict::from_bytes) else {
                    return Err("Expected a complete PDF dictionary no larger than 1 MiB".into());
                };
                if dict.contains_key(b"Encrypt") {
                    return Err(super::PROTECTED.into());
                }
                match dict.get::<Name<'_>>(b"Type").as_deref() {
                    Some(b"Pages") => {
                        if dict
                            .get::<u32>(b"Count")
                            .is_some_and(|n| n as usize > super::MAX_PAGES)
                        {
                            return Err(
                                "The PDF declares more than the limit of 20,000 pages".into()
                            );
                        }
                    }
                    Some(b"Page") => {
                        pages += 1;
                        if pages > super::MAX_PAGES {
                            return Err(
                                "The PDF exceeds the limit of 20,000 page dictionaries".into()
                            );
                        }
                    }
                    _ => {}
                }
                at += dict.data().len();
                whitespace(data, &mut at);
                if data
                    .get(at..)
                    .is_some_and(|tail| tail.starts_with(b"stream"))
                {
                    at += 6;
                    // A stream begins after CR, LF or CRLF. Binary bytes are not PDF tokens.
                    if data.get(at) == Some(&b'\r') {
                        at += 1;
                    }
                    if data.get(at) == Some(&b'\n') {
                        at += 1;
                    }
                    let direct_end = dict
                        .get::<u32>(b"Length")
                        .and_then(|len| at.checked_add(len as usize))
                        .filter(|&end| end <= data.len());
                    let mut end = direct_end.unwrap_or(at);
                    whitespace(data, &mut end);
                    if data
                        .get(end..)
                        .is_some_and(|tail| tail.starts_with(b"endstream"))
                    {
                        at = end + 9;
                    } else {
                        // Indirect/missing lengths: the same recovery boundary used by PDF readers.
                        let tail = data.get(at..).unwrap_or_default();
                        let Some(offset) = tail.windows(9).position(|w| w == b"endstream") else {
                            return Err("Expected the end of the PDF stream".into());
                        };
                        at += offset + 9;
                    }
                }
            }
            b'<' => {
                at += 1;
                while data.get(at).is_some_and(|b| *b != b'>') {
                    at += 1;
                }
                if at < data.len() {
                    at += 1;
                }
            }
            _ => at += 1,
        }
    }
    Ok(())
}

fn whitespace(data: &[u8], at: &mut usize) {
    loop {
        while data
            .get(*at)
            .is_some_and(|b| matches!(b, 0 | 9 | 10 | 12 | 13 | 32))
        {
            *at += 1;
        }
        if data.get(*at) != Some(&b'%') {
            break;
        }
        while data.get(*at).is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
            *at += 1;
        }
    }
}
