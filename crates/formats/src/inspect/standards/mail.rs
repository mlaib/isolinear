//! Internet mail: single messages (RFC 5322 `.eml`, MIME per RFC 2045–2047 and 2231) and
//! mailboxes (`.mbox`, mboxo/mboxrd). Headers are unfolded and encoded words decoded; the MIME
//! tree is walked under depth and part limits; base64 and quoted-printable are decoded here,
//! bounded. HTML bodies become plain text with the tags dropped: nothing is fetched or run.
use super::doc_common::{
    self, base64, clip, decode_charset, human, is_picture, push_row, quoted_printable, table,
};
use crate::inspect::{
    Handler, Input, Inspection, MAX_ROWS, OutlineNode, Probe, Stage, State, Visual,
};
use std::ops::Range;

/// MIME nesting and parts read at most per message.
const MAX_DEPTH: u16 = 16;
const MAX_PARTS: usize = 1000;
/// Header block read at most, and header fields kept.
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_HEADERS: usize = 10_000;
/// Decoded bytes per part, and text kept for the body.
const MAX_DECODED: usize = 64 * 1024 * 1024;
const MAX_BODY_TEXT: usize = 4 * 1024 * 1024;
const MAX_BODY_LINES: usize = 20_000;
/// Pictures shown, and decoded picture bytes in all.
const MAX_PICTURES: usize = 10;
const MAX_PICTURE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Header {
    pub name: String,
    /// Unfolded, not decoded.
    pub value: String,
    /// Bytes of the field (name to the end of its last line), relative to the parsed slice.
    pub span: Range<usize>,
}

/// Header fields of a message or part, and where its body starts (relative).
pub fn parse_headers(bytes: &[u8]) -> (Vec<Header>, usize) {
    let mut out: Vec<Header> = vec![];
    let mut at = 0usize;
    let limit = bytes.len().min(MAX_HEADER_BYTES);
    while at < limit {
        let end = memchr::memchr(b'\n', &bytes[at..]).map_or(bytes.len(), |i| at + i + 1);
        let line = &bytes[at..end];
        let content = trim_eol(line);
        if content.is_empty() {
            return (out, end);
        }
        if matches!(content[0], b' ' | b'\t') {
            if let Some(last) = out.last_mut() {
                last.value.push(' ');
                last.value
                    .push_str(String::from_utf8_lossy(content).trim_start());
                last.span.end = end;
            }
        } else {
            let Some(colon) = memchr::memchr(b':', content) else {
                // Not a header line: the body starts here.
                return (out, at);
            };
            let name = &content[..colon];
            if name.is_empty() || name.iter().any(|&c| c <= b' ' || c >= 127) {
                return (out, at);
            }
            if out.len() >= MAX_HEADERS {
                return (out, at);
            }
            out.push(Header {
                name: String::from_utf8_lossy(name).into_owned(),
                value: String::from_utf8_lossy(&content[colon + 1..])
                    .trim()
                    .to_owned(),
                span: at..end,
            });
        }
        at = end;
    }
    (out, at.min(bytes.len()))
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let mut l = line;
    while let [rest @ .., b'\n' | b'\r'] = l {
        l = rest;
    }
    l
}

pub fn header<'a>(headers: &'a [Header], name: &str) -> Option<&'a Header> {
    headers.iter().find(|h| h.name.eq_ignore_ascii_case(name))
}
fn headers_named<'a>(headers: &'a [Header], name: &'a str) -> impl Iterator<Item = &'a Header> {
    headers
        .iter()
        .filter(move |h| h.name.eq_ignore_ascii_case(name))
}

/// RFC 2047 encoded words decoded (`=?utf-8?Q?caf=C3=A9?=`); whitespace between two encoded
/// words is dropped, as the RFC says.
pub fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let (before, candidate) = rest.split_at(start);
        match encoded_word(candidate) {
            Some((text, len)) => {
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&text);
                rest = &candidate[len..];
                last_was_word = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &candidate[2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One encoded word at the start of `s`: its text and length.
fn encoded_word(s: &str) -> Option<(String, usize)> {
    let inner = s.strip_prefix("=?")?;
    let q1 = inner.find('?')?;
    let charset = inner[..q1].split('*').next().unwrap_or("");
    let enc = inner.get(q1 + 1..q1 + 2)?;
    if inner.get(q1 + 2..q1 + 3) != Some("?") {
        return None;
    }
    let text_start = q1 + 3;
    let end = inner[text_start..].find("?=")? + text_start;
    let text = &inner[text_start..end];
    if text.contains(' ') {
        return None;
    }
    let bytes = match enc {
        "B" | "b" => base64(text.as_bytes(), 64 * 1024).0,
        "Q" | "q" => quoted_printable(text.as_bytes(), 64 * 1024, true).0,
        _ => return None,
    };
    Some((decode_charset(&bytes, charset), end + 4))
}

/// `type/subtype; a=b; c="d"` → lower-case type and the parameters (RFC 2231 continuations and
/// `*`-encoded values joined and decoded).
pub fn content_params(value: &str) -> (String, Vec<(String, String)>) {
    let mut parts = split_unquoted(value, ';').into_iter();
    let kind = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let mut raw: Vec<(String, String)> = vec![];
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            let v = v.trim();
            let v = if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
                v[1..v.len() - 1]
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\")
            } else {
                v.to_owned()
            };
            raw.push((k.trim().to_ascii_lowercase(), v));
        }
    }
    // RFC 2231: name*0, name*1*, name* ...
    let mut params: Vec<(String, String)> = vec![];
    let mut extended: Vec<(String, u32, bool, String)> = vec![];
    for (k, v) in raw {
        if let Some((base, rest)) = k.split_once('*') {
            let encoded = rest.ends_with('*') || rest.is_empty();
            let index = rest.trim_end_matches('*').parse().unwrap_or(0);
            extended.push((base.to_owned(), index, encoded, v));
        } else {
            params.push((k, v));
        }
    }
    extended.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let mut joined: Vec<(String, Vec<u8>, String)> = vec![];
    for (base, index, encoded, v) in extended {
        let (charset, text) = if encoded && index == 0 {
            let mut it = v.splitn(3, '\'');
            match (it.next(), it.next(), it.next()) {
                (Some(cs), Some(_lang), Some(t)) => (cs.to_owned(), t.to_owned()),
                _ => (String::new(), v.clone()),
            }
        } else {
            (String::new(), v.clone())
        };
        let bytes = if encoded {
            percent_decode(&text)
        } else {
            text.into_bytes()
        };
        match joined.iter_mut().find(|(b, _, _)| *b == base) {
            Some((_, acc, _)) => acc.extend(bytes),
            None => joined.push((base, bytes, charset)),
        }
    }
    for (base, bytes, charset) in joined {
        let cs = if charset.is_empty() {
            "utf-8"
        } else {
            &charset
        };
        params.retain(|(k, _)| *k != base);
        params.push((base, decode_charset(&bytes, cs)));
    }
    (kind, params)
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Split on `sep` outside double quotes, angle brackets and parentheses.
fn split_unquoted(s: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let (mut quoted, mut angle, mut paren) = (false, 0i32, 0i32);
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => {
                escaped = true;
                current.push(c);
                continue;
            }
            '"' => quoted = !quoted,
            '<' if !quoted => angle += 1,
            '>' if !quoted => angle -= 1,
            '(' if !quoted => paren += 1,
            ')' if !quoted => paren -= 1,
            c if c == sep && !quoted && angle <= 0 && paren <= 0 => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    out.push(current);
    out
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The mail addresses in an address list (`"Ann" <ann@x.org>, bob@y.org`), lower case.
pub fn addresses(value: &str) -> Vec<String> {
    split_unquoted(value, ',')
        .iter()
        .filter_map(|a| {
            let a = a.trim();
            let addr = match (a.rfind('<'), a.rfind('>')) {
                (Some(s), Some(e)) if s < e => &a[s + 1..e],
                _ => a.split_whitespace().find(|w| w.contains('@')).unwrap_or(""),
            };
            let addr = addr.trim().trim_matches(|c| c == '"' || c == '\'');
            addr.contains('@').then(|| addr.to_ascii_lowercase())
        })
        .take(10_000)
        .collect()
}

/// An RFC 5322 date (`Tue, 3 Nov 2026 10:15:00 +0100`): seconds since 1970 (UTC) and the
/// date as `2026-11-03 10:15:00 +0100`.
pub fn parse_date(value: &str) -> Option<(i64, String)> {
    let v = value.split('(').next().unwrap_or(value);
    let v = match v.find(',') {
        Some(i) => &v[i + 1..],
        None => v,
    };
    let words: Vec<&str> = v.split_whitespace().collect();
    if words.len() < 4 {
        return None;
    }
    let day: u32 = words[0].parse().ok()?;
    let month = doc_common::month_number(words[1])?;
    let mut year: i64 = words[2].parse().ok()?;
    if words[2].len() == 2 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let time: Vec<u32> = words[3]
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let (h, mi, s) = match time.as_slice() {
        [h, m] => (*h, *m, 0),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    if !doc_common::valid_date(year, month, day) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let zone = words.get(4).copied().unwrap_or("+0000");
    let offset_min: i64 = match zone {
        z if (z.starts_with('+') || z.starts_with('-')) && z.len() == 5 => {
            let n: i64 = z[1..].parse().ok()?;
            let m = n / 100 * 60 + n % 100;
            if z.starts_with('-') { -m } else { m }
        }
        "UT" | "UTC" | "GMT" | "Z" => 0,
        "EST" => -300,
        "EDT" => -240,
        "CST" => -360,
        "CDT" => -300,
        "MST" => -420,
        "MDT" => -360,
        "PST" => -480,
        "PDT" => -420,
        _ => 0,
    };
    let secs = doc_common::days_from_civil(year, month, day) * 86_400
        + i64::from(h) * 3600
        + i64::from(mi) * 60
        + i64::from(s)
        - offset_min * 60;
    let sign = if offset_min < 0 { '-' } else { '+' };
    let z = offset_min.abs();
    Some((
        secs,
        format!(
            "{year:04}-{month:02}-{day:02} {h:02}:{mi:02}:{s:02} {sign}{:02}{:02}",
            z / 60,
            z % 60
        ),
    ))
}

// ----- MIME -----

#[derive(Clone, Debug, Default)]
pub struct Part {
    pub content_type: String,
    pub params: Vec<(String, String)>,
    pub encoding: String,
    pub disposition: String,
    pub filename: Option<String>,
    pub content_id: Option<String>,
    pub depth: u16,
    /// From the part's headers to the end of its body, relative to the message.
    pub span: Range<usize>,
    pub body: Range<usize>,
    pub children: usize,
}
impl Part {
    pub fn multipart(&self) -> bool {
        self.content_type.starts_with("multipart/")
    }
    pub fn charset(&self) -> &str {
        param(&self.params, "charset").unwrap_or("utf-8")
    }
    /// An attachment: named, marked as one, or a leaf that is neither text nor multipart.
    pub fn attachment(&self) -> bool {
        !self.multipart()
            && (self.disposition == "attachment"
                || self.filename.is_some()
                || !(self.content_type.starts_with("text/")
                    || self.content_type.is_empty()
                    || self.content_type == "message/delivery-status"))
    }
}

#[derive(Default)]
pub struct Mime {
    pub parts: Vec<Part>,
    /// Depth or part limit reached.
    pub limited: bool,
    /// A multipart without its closing boundary.
    pub unclosed: bool,
}

/// The MIME tree of a message, flattened in document order (`depth` gives the nesting).
pub fn parse_mime(msg: &[u8], headers: &[Header], body_start: usize) -> Mime {
    let mut mime = Mime::default();
    walk_part(
        msg,
        headers,
        0..msg.len(),
        body_start,
        0,
        &mut mime,
        "text/plain",
    );
    mime
}

fn walk_part(
    msg: &[u8],
    headers: &[Header],
    span: Range<usize>,
    body_start: usize,
    depth: u16,
    mime: &mut Mime,
    default_type: &str,
) {
    if mime.parts.len() >= MAX_PARTS || depth > MAX_DEPTH {
        mime.limited = true;
        return;
    }
    let (mut kind, params) = header(headers, "Content-Type")
        .map(|h| content_params(&h.value))
        .unwrap_or_else(|| (default_type.to_owned(), vec![]));
    if !kind.contains('/') {
        kind = default_type.to_owned();
    }
    let (disposition, dparams) = header(headers, "Content-Disposition")
        .map(|h| content_params(&h.value))
        .unwrap_or_default();
    let filename = param(&dparams, "filename")
        .or_else(|| param(&params, "name"))
        .map(decode_words);
    let part = Part {
        encoding: header(headers, "Content-Transfer-Encoding")
            .map(|h| h.value.trim().to_ascii_lowercase())
            .unwrap_or_default(),
        disposition,
        filename,
        content_id: header(headers, "Content-ID").map(|h| {
            h.value
                .trim()
                .trim_matches(|c| c == '<' || c == '>')
                .to_owned()
        }),
        depth,
        span: span.clone(),
        body: body_start.min(span.end)..span.end,
        children: 0,
        content_type: kind,
        params,
    };
    let index = mime.parts.len();
    let boundary = param(&part.params, "boundary").map(str::to_owned);
    let multipart = part.multipart();
    let child_default = if part.content_type == "multipart/digest" {
        "message/rfc822"
    } else {
        "text/plain"
    };
    let body = part.body.clone();
    mime.parts.push(part);
    if !multipart {
        return;
    }
    let Some(boundary) = boundary.filter(|b| !b.is_empty() && b.len() <= 200) else {
        return;
    };
    let (children, closed) = split_multipart(msg, body, &boundary);
    if !closed {
        mime.unclosed = true;
    }
    for child in children {
        if mime.parts.len() >= MAX_PARTS {
            mime.limited = true;
            break;
        }
        let (h, start) = parse_headers(&msg[child.clone()]);
        let h: Vec<Header> = h
            .into_iter()
            .map(|mut x| {
                x.span = x.span.start + child.start..x.span.end + child.start;
                x
            })
            .collect();
        mime.parts[index].children += 1;
        walk_part(
            msg,
            &h,
            child.clone(),
            child.start + start,
            depth + 1,
            mime,
            child_default,
        );
    }
}

/// The byte ranges of the parts of a multipart body, and whether the closing delimiter was seen.
fn split_multipart(msg: &[u8], body: Range<usize>, boundary: &str) -> (Vec<Range<usize>>, bool) {
    let delimiter = format!("--{boundary}");
    let finder = memchr::memmem::Finder::new(delimiter.as_bytes());
    let data = &msg[body.clone()];
    let mut parts = vec![];
    let mut current: Option<usize> = None;
    let mut search = 0usize;
    while let Some(i) = finder.find(&data[search..]).map(|i| i + search) {
        search = i + delimiter.len();
        if i > 0 && data[i - 1] != b'\n' {
            continue;
        }
        let after = &data[i + delimiter.len()..];
        let closing = after.starts_with(b"--");
        let line_end =
            memchr::memchr(b'\n', after).map_or(data.len(), |n| i + delimiter.len() + n + 1);
        // The CRLF before a delimiter belongs to the delimiter.
        let mut end = i;
        if end > 0 && data[end - 1] == b'\n' {
            end -= 1;
            if end > 0 && data[end - 1] == b'\r' {
                end -= 1;
            }
        }
        if let Some(start) = current.take()
            && start <= end
        {
            parts.push(body.start + start..body.start + end);
        }
        if closing {
            return (parts, true);
        }
        current = Some(line_end.min(data.len()));
        if parts.len() >= MAX_PARTS {
            return (parts, false);
        }
    }
    if let Some(start) = current {
        parts.push(body.start + start..body.end);
    }
    (parts, false)
}

/// A part's body with its transfer encoding undone, at most `limit` bytes.
pub fn decoded(msg: &[u8], part: &Part, limit: usize) -> (Vec<u8>, bool) {
    let raw = &msg[part.body.clone()];
    match part.encoding.as_str() {
        "base64" => base64(raw, limit),
        "quoted-printable" => quoted_printable(raw, limit, false),
        _ => (raw[..raw.len().min(limit)].to_vec(), raw.len() > limit),
    }
}

/// Plain text of HTML: tags dropped (script, style and head content too), block elements on
/// their own lines, entities resolved. Nothing referenced by the HTML is loaded.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(MAX_BODY_TEXT) / 2);
    let mut rest = &html[..floor(html, MAX_BODY_TEXT)];
    let mut skip_until: Option<&str> = None;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(i) => {
                if skip_until.is_none() {
                    push_text(&mut out, &rest[..i]);
                }
                rest = &rest[i..];
                if rest.starts_with("<!--") {
                    rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
                    continue;
                }
                let (tag, end) = match rest.find('>') {
                    Some(e) => (&rest[1..e], e + 1),
                    None => (&rest[1..], rest.len()),
                };
                let closing = tag.starts_with('/');
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                match (skip_until, closing) {
                    (Some(s), true) if s == name => skip_until = None,
                    (Some(_), _) => {}
                    (None, false)
                        if matches!(name.as_str(), "script" | "style" | "head" | "title") =>
                    {
                        skip_until = Some(match name.as_str() {
                            "script" => "script",
                            "style" => "style",
                            "head" => "head",
                            _ => "title",
                        })
                    }
                    (None, _) => {
                        if matches!(
                            name.as_str(),
                            "br" | "p"
                                | "div"
                                | "tr"
                                | "li"
                                | "h1"
                                | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                                | "table"
                                | "ul"
                                | "ol"
                                | "blockquote"
                                | "hr"
                                | "pre"
                        ) {
                            if name == "li" && !closing {
                                out.push_str("\n• ");
                            } else if !out.ends_with('\n') {
                                out.push('\n');
                            }
                        } else if matches!(name.as_str(), "td" | "th") && !closing {
                            out.push('\t');
                        }
                    }
                }
                rest = &rest[end.min(rest.len())..];
            }
            None => {
                if skip_until.is_none() {
                    push_text(&mut out, rest);
                }
                break;
            }
        }
    }
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let mut text = String::new();
    let mut blank = 0;
    for l in lines {
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(&l);
        text.push('\n');
    }
    text.trim().to_owned()
}

fn floor(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn push_text(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[..floor(rest, 12)].find(';');
        match end.and_then(|e| entity(&rest[1..e]).map(|c| (c, e))) {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "euro" => '€',
        "copy" => '©',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "uuml" => 'ü',
        "ouml" => 'ö',
        "auml" => 'ä',
        "szlig" => 'ß',
        "ndash" => '–',
        "mdash" => '—',
        n if n.starts_with("#x") || n.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&n[2..], 16).ok()?)?
        }
        n if n.starts_with('#') => char::from_u32(n[1..].parse().ok()?)?,
        _ => return None,
    })
}

// ----- one message -----

/// Read a message (`bytes`, which start at `base` in the file) into `out`.
pub fn inspect_message(
    bytes: &[u8],
    base: u64,
    out: &mut Inspection,
    cancelled: &dyn Fn() -> bool,
) {
    let at = |r: &Range<usize>| Some(base + r.start as u64..base + r.end as u64);
    let (headers, body_start) = parse_headers(bytes);
    let get = |name: &str| header(&headers, name);
    let fact = |out: &mut Inspection, group: &str, label: &str, name: &str| {
        if let Some(h) = get(name) {
            out.fact(group, label, decode_words(&h.value), at(&h.span));
        }
    };
    fact(out, "Message", "Subject", "Subject");
    if let Some(h) = get("Date") {
        let shown = parse_date(&h.value).map_or_else(|| h.value.clone(), |(_, d)| d);
        out.fact("Message", "Date", shown, at(&h.span));
    }
    fact(out, "Message", "Message ID", "Message-ID");
    fact(out, "Message", "In reply to", "In-Reply-To");
    if let Some(h) = get("References") {
        let n = h.value.matches('<').count();
        out.fact(
            "Message",
            "References",
            format!("{n} earlier messages"),
            at(&h.span),
        );
    }
    fact(out, "Message", "MIME version", "MIME-Version");
    fact(out, "Message", "Mail program", "X-Mailer");
    fact(out, "Message", "Mail program", "User-Agent");
    for (label, name) in [
        ("From address", "From"),
        ("Sender address", "Sender"),
        ("Reply-To address", "Reply-To"),
        ("To addresses", "To"),
        ("Cc addresses", "Cc"),
        ("Bcc addresses", "Bcc"),
        ("Return-Path address", "Return-Path"),
        ("Delivered-To address", "Delivered-To"),
    ] {
        fact(out, "Addresses", label, name);
    }

    // Transport: the Received chain, newest first, and what receiving servers reported.
    let received: Vec<&Header> = headers_named(&headers, "Received").collect();
    if !received.is_empty() {
        out.fact(
            "Transport",
            "Received hops",
            received.len().to_string(),
            at(&received[0].span),
        );
        let mut t = table("Received chain", &["Hop", "From", "By", "With", "Date"]);
        for (i, h) in received.iter().enumerate() {
            let (from, by, with, date) = received_parts(&h.value);
            push_row(
                &mut t,
                vec![(i + 1).to_string(), from, by, with, date],
                at(&h.span),
            );
        }
        out.tables.push(t);
    }
    let mut auth = vec![];
    for h in headers_named(&headers, "Authentication-Results").take(20) {
        let summary = auth_summary(&h.value);
        if !summary.is_empty() {
            auth.push((summary, h.span.clone()));
        }
    }
    if let Some((s, span)) = auth.first() {
        out.fact("Transport", "Authentication results", s.clone(), at(span));
    }
    let dkim: Vec<&Header> = headers_named(&headers, "DKIM-Signature").collect();
    for h in dkim.iter().take(5) {
        let domain = h
            .value
            .split(';')
            .filter_map(|kv| kv.trim().strip_prefix("d="))
            .next()
            .unwrap_or("?")
            .trim()
            .to_owned();
        out.fact("Transport", "DKIM signing domain", domain, at(&h.span));
    }
    let required: Vec<&str> = ["From", "Date"]
        .into_iter()
        .filter(|n| get(n).is_none())
        .collect();
    out.check(
        Stage::Semantic,
        if required.is_empty() {
            State::Passed
        } else {
            State::Failed
        },
        if required.is_empty() {
            "Required header fields From and Date are present (RFC 5322)".to_owned()
        } else {
            format!("Missing required header fields: {}", required.join(", "))
        },
    );

    // MIME structure, body text, attachments.
    let mime = parse_mime(bytes, &headers, body_start);
    if cancelled() {
        return;
    }
    for p in &mime.parts {
        if out.outline.len() >= crate::inspect::MAX_ITEMS {
            break;
        }
        let mut value = vec![];
        if p.content_type.starts_with("text/") {
            value.push(format!("charset {}", p.charset()));
        }
        if !p.encoding.is_empty() {
            value.push(p.encoding.clone());
        }
        if !p.multipart() {
            value.push(human(p.body.len() as u64));
        }
        if let Some(f) = &p.filename {
            value.push(format!("“{f}”"));
        }
        if !p.disposition.is_empty() {
            value.push(p.disposition.clone());
        }
        out.outline.push(OutlineNode {
            depth: p.depth,
            label: p.content_type.clone(),
            value: value.join(", "),
            source: at(&p.span),
        });
    }
    let body = mime
        .parts
        .iter()
        .find(|p| {
            p.content_type == "text/plain" && !p.attachment() && p.disposition != "attachment"
        })
        .map(|p| (p, false))
        .or_else(|| {
            mime.parts
                .iter()
                .find(|p| p.content_type == "text/html" && p.disposition != "attachment")
                .map(|p| (p, true))
        });
    let mut remote = 0usize;
    if let Some((p, html)) = body {
        let (bytes_out, cut) = decoded(bytes, p, MAX_BODY_TEXT);
        let mut text = decode_charset(&bytes_out, p.charset());
        if html {
            text = html_to_text(&text);
        }
        out.fact(
            "Content",
            "Body",
            if html {
                "text/html, shown as plain text (tags removed)"
            } else {
                "text/plain"
            },
            at(&p.span),
        );
        let mut t = table("Body text", &["Text"]);
        for line in text.lines().take(MAX_BODY_LINES) {
            push_row(&mut t, vec![line.trim_end().to_owned()], at(&p.body));
        }
        if text.lines().count() > MAX_BODY_LINES || cut {
            out.notes.push(format!(
                "The body text is shown up to {MAX_BODY_LINES} lines or {}",
                human(MAX_BODY_TEXT as u64)
            ));
        }
        out.tables.push(t);
    }
    for p in mime
        .parts
        .iter()
        .filter(|p| p.content_type == "text/html")
        .take(20)
    {
        let (html, _) = decoded(bytes, p, MAX_BODY_TEXT);
        remote += remote_references(&decode_charset(&html, p.charset()));
    }
    if remote > 0 {
        out.notes.push(format!(
            "The HTML refers to {remote} remote {} (images, styles); nothing was loaded",
            if remote == 1 { "resource" } else { "resources" }
        ));
    }
    let mut attachments = table(
        "Attachments",
        &[
            "Filename",
            "Type",
            "Disposition",
            "Encoding",
            "Decoded size",
            "Content ID",
        ],
    );
    let mut pictures = 0usize;
    let mut picture_bytes = 0usize;
    for p in &mime.parts {
        if !p.attachment() || body.is_some_and(|(b, _)| std::ptr::eq(b, p)) {
            continue;
        }
        let (data, cut) = decoded(bytes, p, MAX_DECODED);
        let size = if cut {
            format!("more than {}", human(MAX_DECODED as u64))
        } else {
            data.len().to_string()
        };
        if p.content_type.starts_with("image/")
            && pictures < MAX_PICTURES
            && !cut
            && is_picture(&data)
            && picture_bytes + data.len() <= MAX_PICTURE_BYTES
        {
            pictures += 1;
            picture_bytes += data.len();
            out.visuals.push(Visual::Picture {
                caption: p
                    .filename
                    .clone()
                    .or_else(|| p.content_id.clone())
                    .unwrap_or_else(|| p.content_type.clone()),
                bytes: data,
            });
        }
        push_row(
            &mut attachments,
            vec![
                p.filename.clone().unwrap_or_default(),
                p.content_type.clone(),
                if p.disposition.is_empty() {
                    "inline".into()
                } else {
                    p.disposition.clone()
                },
                if p.encoding.is_empty() {
                    "7bit".into()
                } else {
                    p.encoding.clone()
                },
                size,
                p.content_id.clone().unwrap_or_default(),
            ],
            at(&p.span),
        );
    }
    out.fact("Content", "MIME parts", mime.parts.len().to_string(), None);
    out.fact(
        "Content",
        "Attachments",
        attachments.rows.len().to_string(),
        None,
    );
    if !attachments.rows.is_empty() {
        out.tables.push(attachments);
    }
    let signed = !dkim.is_empty()
        || mime.parts.iter().any(|p| {
            p.content_type == "multipart/signed"
                || p.content_type.contains("pkcs7-signature")
                || p.content_type == "application/pgp-signature"
                || p.content_type.contains("pkcs7-mime")
        });
    if mime.limited {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("Only {MAX_PARTS} MIME parts, nested at most {MAX_DEPTH} deep, were read"),
        );
    } else if mime.unclosed {
        out.check(
            Stage::Parsed,
            State::Partial,
            "A multipart body ends without its closing boundary (cut message?): its parts so far are shown",
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Passed,
            "Header fields and MIME structure read",
        );
    }
    out.not_validated();
    if signed {
        out.check(
            Stage::Signature,
            State::NotDone,
            "Signatures (DKIM, S/MIME, OpenPGP) were not verified",
        );
    }
    if !auth.is_empty() {
        out.notes.push(
            "Authentication results are what receiving servers wrote into the message; Isolinear did not check them".into(),
        );
    }
}

/// "from A by B with C; date" of a Received field.
fn received_parts(value: &str) -> (String, String, String, String) {
    let (route, date) = match value.rfind(';') {
        Some(i) => (&value[..i], value[i + 1..].trim()),
        None => (value, ""),
    };
    let lower = route.to_ascii_lowercase();
    let word = |key: &str| -> String {
        let key = format!("{key} ");
        let start = if lower.starts_with(&key) {
            Some(0)
        } else {
            lower.find(&format!(" {key}")).map(|i| i + 1)
        };
        let Some(start) = start else {
            return String::new();
        };
        let rest = &route[start + key.len()..];
        let rest_lower = &lower[start + key.len()..];
        let end = [" by ", " with ", " id ", " for ", " via "]
            .iter()
            .filter_map(|k| rest_lower.find(k))
            .min()
            .unwrap_or(rest.len());
        clip(rest[..end].trim(), 120)
    };
    let date = parse_date(date).map_or_else(|| date.to_owned(), |(_, d)| d);
    (word("from"), word("by"), word("with"), date)
}

/// `spf=pass, dkim=pass, dmarc=pass` from an Authentication-Results field.
fn auth_summary(value: &str) -> String {
    let mut out: Vec<String> = vec![];
    for token in value.split([';', ' ', '\t']) {
        let token = token.trim();
        if let Some((k, v)) = token.split_once('=') {
            let k = k.to_ascii_lowercase();
            if matches!(
                k.as_str(),
                "spf" | "dkim" | "dmarc" | "arc" | "auth" | "iprev" | "bimi"
            ) && out.len() < 12
            {
                out.push(format!("{k}={}", v.to_ascii_lowercase()));
            }
        }
    }
    out.join(", ")
}

/// How many `src`/`href` values in the HTML point at the network.
fn remote_references(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    [
        "src=\"http",
        "src='http",
        "src=http",
        "url(http",
        "background=\"http",
    ]
    .iter()
    .map(|p| lower.matches(p).count())
    .sum()
}

// ----- handlers -----

/// Whether the text starts with RFC 5322 header fields that make it a message.
fn looks_like_message(text: &str, strict: bool) -> bool {
    let (headers, _) = parse_headers(text.as_bytes());
    if headers.len() < 2
        || !text
            .as_bytes()
            .first()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    let has = |n: &str| headers.iter().any(|h| h.name.eq_ignore_ascii_case(n));
    let transport =
        has("Received") || has("Return-Path") || has("MIME-Version") || has("Message-ID");
    if strict {
        has("From") && (has("Date") || has("Subject")) && transport
    } else {
        has("From") || has("To") || has("Subject") || transport
    }
}

pub struct Eml;
impl Handler for Eml {
    fn name(&self) -> &'static str {
        "Internet mail message"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if probe.head.starts_with(b"From ") {
            return false;
        }
        let text = String::from_utf8_lossy(probe.head);
        if probe.ext_is(&["eml", "mht", "mhtml"]) {
            looks_like_message(&text, false)
        } else {
            matches!(probe.format, "text" | "binary") && looks_like_message(&text, true)
        }
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let mut out = Inspection::new("Internet mail", "Mail message (RFC 5322 / MIME)");
        let (headers, _) = parse_headers(input.bytes);
        if headers.is_empty() {
            return Err("No mail header fields at the start of the file".into());
        }
        out.version = header(&headers, "MIME-Version").map(|h| format!("MIME {}", h.value));
        inspect_message(input.bytes, 0, &mut out, cancelled);
        Ok(out)
    }
}

/// Starts of messages in a mailbox: every line that begins with `From `.
fn separators(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Vec<usize>, String> {
    let mut starts = vec![];
    if bytes.starts_with(b"From ") {
        starts.push(0);
    }
    for (n, i) in memchr::memmem::find_iter(bytes, b"\nFrom ").enumerate() {
        if n % 65_536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        starts.push(i + 1);
        if starts.len() > MAX_ROWS {
            break;
        }
    }
    Ok(starts)
}

pub struct Mbox;
impl Handler for Mbox {
    fn name(&self) -> &'static str {
        "Mailbox (mbox)"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.head.starts_with(b"From ") {
            return false;
        }
        let first_end = memchr::memchr(b'\n', probe.head).unwrap_or(probe.head.len());
        let rest = &probe.head[(first_end + 1).min(probe.head.len())..];
        let (headers, _) = parse_headers(rest);
        probe.ext_is(&["mbox", "mbx"]) && !headers.is_empty()
            || headers.len() >= 2
                && headers.iter().any(|h| {
                    ["From", "Date", "Subject", "Received", "Return-Path"]
                        .iter()
                        .any(|n| h.name.eq_ignore_ascii_case(n))
                })
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let bytes = input.bytes;
        let starts = separators(bytes, cancelled)?;
        if starts.is_empty() {
            return Err("No \"From \" separator lines: not an mbox mailbox".into());
        }
        let mut out = Inspection::new("Internet mail", "Mailbox (mbox)");
        let mut t = table(
            "Messages",
            &[
                "No.",
                "Date",
                "From address",
                "To address",
                "Subject",
                "Size",
                "Attachments",
            ],
        );
        let mut first: Option<(i64, String)> = None;
        let mut last: Option<(i64, String)> = None;
        let mut senders: Vec<(String, usize)> = vec![];
        let mut unreadable = 0usize;
        let kept = starts.len().min(MAX_ROWS);
        for (n, &start) in starts.iter().take(kept).enumerate() {
            if n % 1024 == 0 && cancelled() {
                return Err("Cancelled".into());
            }
            let end = starts.get(n + 1).map_or(bytes.len(), |&e| e);
            let line_end = memchr::memchr(b'\n', &bytes[start..end]).map_or(end, |i| start + i + 1);
            let msg = &bytes[line_end..end];
            let (headers, body_start) = parse_headers(msg);
            if headers.is_empty() {
                unreadable += 1;
            }
            let get = |name: &str| header(&headers, name).map(|h| decode_words(&h.value));
            let date = header(&headers, "Date").and_then(|h| parse_date(&h.value));
            if let Some((secs, shown)) = &date {
                if first.as_ref().is_none_or(|(s, _)| secs < s) {
                    first = Some((*secs, shown.clone()));
                }
                if last.as_ref().is_none_or(|(s, _)| secs > s) {
                    last = Some((*secs, shown.clone()));
                }
            }
            let from = get("From").unwrap_or_default();
            if let Some(addr) = addresses(&from).into_iter().next() {
                match senders.iter().position(|(a, _)| *a == addr) {
                    Some(i) => senders[i].1 += 1,
                    None if senders.len() < 100_000 => senders.push((addr, 1)),
                    None => {}
                }
            }
            let mime = parse_mime(msg, &headers, body_start);
            let attachments = mime.parts.iter().filter(|p| p.attachment()).count();
            push_row(
                &mut t,
                vec![
                    (n + 1).to_string(),
                    date.map(|(_, d)| d)
                        .or_else(|| get("Date"))
                        .unwrap_or_default(),
                    from,
                    get("To").unwrap_or_default(),
                    get("Subject").unwrap_or_default(),
                    (end - start).to_string(),
                    attachments.to_string(),
                ],
                Some(start as u64..end as u64),
            );
        }
        let total = if starts.len() > MAX_ROWS {
            format!("more than {MAX_ROWS}")
        } else {
            starts.len().to_string()
        };
        out.fact("Mailbox", "Messages", total, None);
        if let Some((_, d)) = &first {
            out.fact("Mailbox", "First date", d.clone(), None);
        }
        if let Some((_, d)) = &last {
            out.fact("Mailbox", "Last date", d.clone(), None);
        }
        out.fact(
            "Mailbox",
            "Distinct senders",
            senders.len().to_string(),
            None,
        );
        senders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let top: Vec<String> = senders
            .iter()
            .take(5)
            .map(|(a, c)| format!("{a} ({c})"))
            .collect();
        out.fact(
            "Mailbox",
            "Most frequent sender addresses",
            top.join(", "),
            None,
        );
        let escaped = memchr::memmem::find_iter(bytes, b"\n>From ").count();
        let double = memchr::memmem::find_iter(bytes, b"\n>>From ").count();
        if escaped + double > 0 {
            out.fact(
                "Mailbox",
                "Escaped \">From\" lines",
                format!(
                    "{} ({})",
                    escaped + double,
                    if double > 0 {
                        "mboxrd"
                    } else {
                        "mboxo or mboxrd"
                    }
                ),
                None,
            );
        }
        out.tables.push(t);
        if starts.len() > MAX_ROWS {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("Only the first {MAX_ROWS} messages are listed"),
            );
        } else if unreadable > 0 {
            out.check(
                Stage::Parsed,
                State::Partial,
                format!("{unreadable} messages have no readable header fields"),
            );
        } else {
            out.check(
                Stage::Parsed,
                State::Passed,
                "Messages separated by their \"From \" lines; header fields read",
            );
        }
        out.not_validated();
        out.notes.push(
            "Each row points at its message in the file; open a single message as .eml to see its body and attachments".into(),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn words_params_dates() {
        assert_eq!(
            decode_words("=?utf-8?Q?caf=C3=A9?= =?ISO-8859-1?B?6Q==?= et lait"),
            "caféé et lait"
        );
        assert_eq!(decode_words("plain =? text"), "plain =? text");
        let (k, p) = content_params("Text/Plain; charset=\"UTF-8\"; name*=utf-8''caf%C3%A9.txt");
        assert_eq!(k, "text/plain");
        assert_eq!(param(&p, "charset"), Some("UTF-8"));
        assert_eq!(param(&p, "name"), Some("café.txt"));
        let (_, p) = content_params("attachment; filename*0=\"long\"; filename*1=\"name.pdf\"");
        assert_eq!(param(&p, "filename"), Some("longname.pdf"));
        assert_eq!(
            addresses("\"Doe, Ann\" <Ann@Example.org>, bob@example.net"),
            ["ann@example.org", "bob@example.net"]
        );
        let (secs, shown) = parse_date("Tue, 3 Nov 2026 10:15:00 +0100 (CET)").unwrap();
        assert_eq!(shown, "2026-11-03 10:15:00 +0100");
        assert_eq!(secs % 86_400, 9 * 3600 + 15 * 60);
        assert!(parse_date("yesterday").is_none());
    }
    #[test]
    fn html_and_multipart() {
        assert_eq!(
            html_to_text(
                "<html><head><title>x</title><style>p{}</style></head><body><p>A &amp; B</p><script>evil()</script><ul><li>one</li></ul></body></html>"
            ),
            "A & B\n\n• one"
        );
        // Cut tags and entities next to multi-byte characters.
        assert_eq!(html_to_text("x <é"), "x");
        assert_eq!(html_to_text("a &éééééééééé; b"), "a &éééééééééé; b");
        let msg = b"Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\npre\r\n--b1\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b1\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b1--\r\n";
        let (h, start) = parse_headers(msg);
        let mime = parse_mime(msg, &h, start);
        assert_eq!(mime.parts.len(), 3);
        assert!(!mime.unclosed);
        assert_eq!(&msg[mime.parts[1].body.clone()], b"hello");
        assert_eq!(decoded(msg, &mime.parts[2], 100).0, b"%PDF");
        assert!(mime.parts[2].attachment());
        // Nesting is bounded.
        let mut deep = String::new();
        for i in 0..40 {
            deep.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"b{i}\"\r\n\r\n--b{i}\r\n"
            ));
        }
        let (h, start) = parse_headers(deep.as_bytes());
        let mime = parse_mime(deep.as_bytes(), &h, start);
        assert!(mime.limited);
    }
}
