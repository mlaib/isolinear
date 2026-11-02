//! Small helpers shared by the binary and scientific handlers: a bounds-checked byte cursor,
//! bounded gzip/BGZF decompression, tables with a row cap, and images scaled to a drawable size.
use crate::inspect::{MAX_ROWS, Row, Table, Visual};
use std::io::Read;
use std::ops::Range;

/// Reads numbers from a byte slice in one byte order; every read is checked.
#[derive(Clone)]
pub struct Cur<'a> {
    pub b: &'a [u8],
    pub at: usize,
    /// Big-endian when true.
    pub be: bool,
}

macro_rules! num {
    ($name:ident, $t:ty, $n:literal) => {
        pub fn $name(&mut self) -> Option<$t> {
            let s: [u8; $n] = self.take($n)?.try_into().ok()?;
            Some(if self.be {
                <$t>::from_be_bytes(s)
            } else {
                <$t>::from_le_bytes(s)
            })
        }
    };
}

impl<'a> Cur<'a> {
    pub fn new(b: &'a [u8], be: bool) -> Self {
        Self { b, at: 0, be }
    }
    pub fn at(b: &'a [u8], at: usize, be: bool) -> Self {
        Self { b, at, be }
    }
    pub fn left(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    num!(u16, u16, 2);
    num!(i16, i16, 2);
    num!(u32, u32, 4);
    num!(i32, i32, 4);
    num!(u64, u64, 8);
    num!(i64, i64, 8);
    num!(f32, f32, 4);
    num!(f64, f64, 8);
}

/// Text of a byte field: UTF-8 when valid, otherwise Latin-1; control characters dropped.
pub fn latin1(b: &[u8]) -> String {
    let s = match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    };
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// Trimmed ASCII/Latin-1 field (fixed-width headers padded with spaces or NULs).
pub fn field(b: &[u8]) -> String {
    latin1(b)
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_owned()
}

/// At most `n` characters, with an ellipsis when cut.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// A compact decimal: integers without a fraction, others with up to 6 significant digits.
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let a = v.abs();
    if !(1e-4..1e9).contains(&a) {
        return format!("{v:.5e}");
    }
    let digits = (6 - a.log10().floor() as i32 - 1).clamp(0, 10) as usize;
    let s = format!("{v:.digits$}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_owned()
}

pub fn table(name: &str, columns: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        rows: Vec::new(),
        total: None,
    }
}

/// Adds a row while the table holds fewer than `cap` (at most [`MAX_ROWS`]) rows; rows past
/// the cap are counted in `total`.
pub fn push(t: &mut Table, cap: usize, cells: Vec<String>, source: Option<Range<u64>>) {
    if t.total.is_none() && t.rows.len() < cap.min(MAX_ROWS) {
        t.rows.push(Row { cells, source });
    } else {
        t.total = Some(t.total.unwrap_or(t.rows.len() as u64) + 1);
    }
}

/// Records how many rows the file has when it is known without visiting them all.
pub fn set_total(t: &mut Table, total: u64) {
    t.total = (total > t.rows.len() as u64).then_some(total);
}

/// Whether [`push`] would keep another row.
pub fn has_room(t: &Table, cap: usize) -> bool {
    t.total.is_none() && t.rows.len() < cap.min(MAX_ROWS)
}

/// Largest side of an image handed to the app; larger pictures are sampled down.
pub const MAX_SIDE: usize = 4096;

/// A greyscale image, sampled down (nearest pixel) when a side exceeds [`MAX_SIDE`].
pub fn grey_image(width: usize, height: usize, values: Vec<f32>, caption: String) -> Visual {
    let step = width.max(height).div_ceil(MAX_SIDE).max(1);
    let (w, h, grey) = if step == 1 {
        (width, height, values)
    } else {
        let (w, h) = (width.div_ceil(step), height.div_ceil(step));
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                out.push(
                    values
                        .get(y * step * width + x * step)
                        .copied()
                        .unwrap_or(0.0),
                );
            }
        }
        (w, h, out)
    };
    Visual::Image {
        width: w as u32,
        height: h as u32,
        grey: Some(grey),
        rgba: None,
        caption: if step == 1 {
            caption
        } else {
            format!("{caption} (shown at 1/{step} scale)")
        },
    }
}

/// An RGBA image, sampled down like [`grey_image`].
pub fn rgba_image(width: usize, height: usize, rgba: Vec<u8>, caption: String) -> Visual {
    let step = width.max(height).div_ceil(MAX_SIDE).max(1);
    let (w, h, px) = if step == 1 {
        (width, height, rgba)
    } else {
        let (w, h) = (width.div_ceil(step), height.div_ceil(step));
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let at = (y * step * width + x * step) * 4;
                out.extend_from_slice(rgba.get(at..at + 4).unwrap_or(&[0, 0, 0, 255]));
            }
        }
        (w, h, out)
    };
    Visual::Image {
        width: w as u32,
        height: h as u32,
        grey: None,
        rgba: Some(px),
        caption: if step == 1 {
            caption
        } else {
            format!("{caption} (shown at 1/{step} scale)")
        },
    }
}

pub fn is_gzip(b: &[u8]) -> bool {
    b.starts_with(&[0x1f, 0x8b])
}

/// What came out of a bounded decompression.
pub struct Inflated {
    pub bytes: Vec<u8>,
    /// The output limit was reached before the end of the stream.
    pub limited: bool,
    /// The stream was damaged or cut; what was decoded before is kept.
    pub error: Option<String>,
}

/// Decompresses gzip (including concatenated members such as BGZF blocks) up to `limit` bytes.
pub fn gunzip(b: &[u8], limit: usize, cancelled: &dyn Fn() -> bool) -> Inflated {
    let mut dec = flate2::read::MultiGzDecoder::new(b);
    read_bounded(&mut dec, limit, cancelled)
}

/// Decompresses a zlib stream up to `limit` bytes.
pub fn unzlib(b: &[u8], limit: usize, cancelled: &dyn Fn() -> bool) -> Inflated {
    let mut dec = flate2::read::ZlibDecoder::new(b);
    read_bounded(&mut dec, limit, cancelled)
}

/// Decompresses a raw deflate stream up to `limit` bytes.
pub fn inflate(b: &[u8], limit: usize, cancelled: &dyn Fn() -> bool) -> Inflated {
    let mut dec = flate2::read::DeflateDecoder::new(b);
    read_bounded(&mut dec, limit, cancelled)
}

fn read_bounded(r: &mut dyn Read, limit: usize, cancelled: &dyn Fn() -> bool) -> Inflated {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut error = None;
    let mut limited = false;
    loop {
        if cancelled() {
            error = Some("cancelled".into());
            break;
        }
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = limit - out.len();
                if n > room {
                    out.extend_from_slice(&buf[..room]);
                    limited = true;
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                if out.len() == limit {
                    // One more byte tells whether the stream ends here.
                    let mut one = [0u8; 1];
                    limited = matches!(r.read(&mut one), Ok(n) if n > 0);
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }
    Inflated {
        bytes: out,
        limited,
        error,
    }
}

/// Decompresses the start of a gzip file (for recognizing it): at most `limit` bytes.
pub fn gunzip_head(b: &[u8], limit: usize) -> Vec<u8> {
    gunzip(b, limit, &|| false).bytes
}

/// Splits text into lines with their byte offsets (line ending removed).
pub fn lines(text: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if at >= text.len() {
            return None;
        }
        let start = at;
        let end = memchr::memchr(b'\n', &text[at..]).map_or(text.len(), |i| at + i);
        at = end + 1;
        let mut line = &text[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Some((start, line))
    })
}

/// N50: the length such that sequences at least that long hold half of all bases.
pub fn n50(lengths: &mut [u64]) -> u64 {
    lengths.sort_unstable_by(|a, b| b.cmp(a));
    let total: u64 = lengths.iter().sum();
    let mut acc = 0u64;
    for &l in lengths.iter() {
        acc += l;
        if acc * 2 >= total {
            return l;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers_and_n50() {
        assert_eq!(num(3.0), "3");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(1.0 / 3.0), "0.333333");
        assert_eq!(n50(&mut [2, 3, 4, 5, 6, 7, 8, 9, 10]), 8);
        let mut c = Cur::new(&[1, 0, 0, 2], false);
        assert_eq!(c.u16(), Some(1));
        c.be = true;
        assert_eq!(c.u16(), Some(2));
        assert_eq!(c.u8(), None);
    }
}
