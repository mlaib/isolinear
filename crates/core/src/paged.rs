//! Text files too large to edit, read a page of lines at a time.
//!
//! [`LineIndex::build`] scans the file once (on a worker) and remembers a line start every
//! `every_lines` lines or `every_bytes` bytes, whichever comes first. Memory stays small for
//! any size, and every line start inside a block lies within `every_bytes` of the block start,
//! so reading a line never scans more than that. Lines become readable block by block while
//! the scan continues. Only the length the file had when the index started is read.
//!
//! UTF-16 files are indexed the same way: their line break is a two-byte unit at an even
//! offset, so a break found at an odd offset is two characters' halves, not a break.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use memchr::memmem;
use serde::Serialize;

use crate::encoding::Encoding;

/// Bytes of one line shown at most; the rest is counted, not read.
pub const LINE_CAP: usize = 32 * 1024;

/// Longest search text, in bytes.
pub const NEEDLE_MAX: usize = 4096;

const SEARCH_CHUNK: usize = 4 << 20;
const BLOCK_CACHE: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub every_lines: u64,
    pub every_bytes: u64,
    /// Read size while indexing.
    pub chunk: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            every_lines: 1024,
            every_bytes: 1 << 20,
            chunk: 1 << 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Checkpoint {
    line: u64,
    offset: u64,
}

#[derive(Debug, Default)]
struct State {
    /// Line starts. While scanning, the last one opens the block still being read; once
    /// complete, a final entry holds the line count and the end offset, as if the file ended
    /// with a newline.
    checkpoints: Vec<Checkpoint>,
    scanned: u64,
    complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// Lines that can be read now.
    pub lines: u64,
    pub scanned: u64,
    pub size: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub text: String,
    /// Length of the whole line in bytes, without its line break.
    pub bytes: u64,
    /// Only the first [`LINE_CAP`] bytes are in `text`.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub line: u64,
    /// Start in UTF-16 code units of the shown line text; `None` when the match lies beyond
    /// the part of the line that is shown.
    pub column: Option<usize>,
    /// Length in UTF-16 code units.
    pub length: usize,
    /// Where the match starts in the file; a next search starts one byte later.
    pub byte_offset: u64,
}

/// Where a search starts.
#[derive(Debug, Clone, Copy)]
pub enum From {
    Line(u64),
    Offset(u64),
}

pub struct LineIndex {
    path: PathBuf,
    size: u64,
    encoding: Encoding,
    bom: bool,
    params: Params,
    state: RwLock<State>,
    cancel: AtomicBool,
    reader: Mutex<Reader>,
}

struct Reader {
    file: File,
    /// Line starts per block, ending with the next block's offset.
    blocks: VecDeque<(usize, Arc<Vec<u64>>)>,
}

/// The line feed of an encoding: its bytes, which are also its alignment.
fn newline(encoding: Encoding) -> &'static [u8] {
    match encoding {
        Encoding::Utf16Le => &[0x0A, 0x00],
        Encoding::Utf16Be => &[0x00, 0x0A],
        _ => b"\n",
    }
}

/// Offsets (from `base`) of the line feeds in `buf`, which starts at file offset `base`.
fn breaks<'a>(nl: &'a [u8], buf: &'a [u8], base: u64) -> impl Iterator<Item = usize> + 'a {
    let width = nl.len() as u64;
    memmem::find_iter(buf, nl).filter(move |&i| (base + i as u64).is_multiple_of(width))
}

impl LineIndex {
    /// Prepares an index of the text at `path` in `encoding`.
    pub fn new(
        path: &Path,
        size: u64,
        encoding: Encoding,
        bom: bool,
        params: Params,
    ) -> io::Result<Self> {
        assert!(params.every_lines > 0 && params.every_bytes > 0 && params.chunk > 0);
        Ok(LineIndex {
            path: path.to_owned(),
            size,
            encoding,
            bom: bom && encoding != Encoding::Windows1252,
            params,
            state: RwLock::new(State {
                checkpoints: vec![Checkpoint { line: 0, offset: 0 }],
                ..State::default()
            }),
            cancel: AtomicBool::new(false),
            reader: Mutex::new(Reader {
                file: File::open(path)?,
                blocks: VecDeque::new(),
            }),
        })
    }

    /// Stops a running [`build`](Self::build) at its next chunk.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> Progress {
        let st = self.state.read().unwrap();
        Progress {
            lines: st.checkpoints.last().map_or(0, |c| c.line),
            scanned: st.scanned,
            size: self.size,
            complete: st.complete,
        }
    }

    /// Scans the file, publishing complete blocks as it goes; `progress` is called after
    /// every chunk and once at the end.
    pub fn build(&self, mut progress: impl FnMut(Progress)) -> io::Result<()> {
        let mut file = File::open(&self.path)?;
        let nl = newline(self.encoding);
        // Whole chunks of an even size, so a two-byte line feed never straddles two of them.
        let mut buf = vec![0u8; self.params.chunk.next_multiple_of(2)];
        let (mut pos, mut line, mut cp_line, mut cp_offset) = (0u64, 0u64, 0u64, 0u64);
        let mut last = Vec::new();
        let mut fresh = Vec::new();
        while pos < self.size {
            if self.cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let want = (self.size - pos).min(buf.len() as u64) as usize;
            let mut n = 0;
            while n < want {
                match file.read(&mut buf[n..want]) {
                    Ok(0) => break,
                    Ok(k) => n += k,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            if n == 0 {
                break;
            }
            for i in breaks(nl, &buf[..n], pos) {
                let next = pos + (i + nl.len()) as u64;
                line += 1;
                let due = line - cp_line >= self.params.every_lines
                    || next - cp_offset >= self.params.every_bytes;
                if due && next < self.size {
                    fresh.push(Checkpoint { line, offset: next });
                    cp_line = line;
                    cp_offset = next;
                }
            }
            last = buf[n.saturating_sub(nl.len())..n].to_vec();
            pos += n as u64;
            let mut st = self.state.write().unwrap();
            st.checkpoints.append(&mut fresh);
            st.scanned = pos;
            drop(st);
            progress(self.progress());
        }
        let ends_with_newline = pos > 0 && last == nl && pos.is_multiple_of(nl.len() as u64);
        let open_line = u64::from(!ends_with_newline);
        let mut st = self.state.write().unwrap();
        st.checkpoints.push(Checkpoint {
            line: line + open_line,
            offset: pos + open_line * nl.len() as u64,
        });
        st.complete = true;
        drop(st);
        progress(self.progress());
        Ok(())
    }

    /// Up to `count` lines from `start`, as far as the index reaches.
    pub fn lines(&self, start: u64, count: usize) -> io::Result<Vec<Line>> {
        let available = self.progress().lines;
        let end = start.saturating_add(count as u64).min(available);
        let mut out = Vec::with_capacity(end.saturating_sub(start) as usize);
        let mut reader = self.reader.lock().unwrap();
        for l in start..end {
            let (begin, stop) = self.line_range(&mut reader, l)?;
            let len = stop - begin;
            let shown = len.min(LINE_CAP as u64) as usize;
            let mut bytes = read_at(&mut reader.file, begin, shown)?;
            let truncated = len > LINE_CAP as u64;
            let cr = self.carriage_return();
            if !truncated && bytes.ends_with(cr) {
                bytes.truncate(bytes.len() - cr.len());
            }
            out.push(Line {
                text: self.decode(&bytes, l == 0),
                bytes: if truncated { len } else { bytes.len() as u64 },
                truncated,
            });
        }
        Ok(out)
    }

    /// Finds `needle` from `from` to the end of what is indexed, then from the start. Returns
    /// `None` when there is no match or when `cancelled` says to stop.
    pub fn find(
        &self,
        needle: &str,
        from: From,
        match_case: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Option<Found>> {
        let Some(mut pattern) = self.encode(needle) else {
            return Ok(None);
        };
        if pattern.is_empty() || pattern.len() > NEEDLE_MAX {
            return Ok(None);
        }
        if !match_case {
            pattern.make_ascii_lowercase();
        }
        let finder = memmem::Finder::new(&pattern);
        let limit = {
            let st = self.state.read().unwrap();
            st.checkpoints
                .last()
                .map_or(0, |c| c.offset)
                .min(st.scanned)
        };
        let start = match from {
            From::Offset(o) => o.min(limit),
            From::Line(l) => {
                let available = self.progress().lines;
                if available == 0 {
                    return Ok(None);
                }
                let mut reader = self.reader.lock().unwrap();
                self.line_range(&mut reader, l.min(available - 1))?.0
            }
        };
        let overlap = pattern.len() as u64 - 1;
        let hit = match self.scan(&finder, start, limit, match_case, cancelled)? {
            Some(hit) => Some(hit),
            None => self.scan(
                &finder,
                0,
                (start + overlap).min(limit),
                match_case,
                cancelled,
            )?,
        };
        match hit {
            Some(offset) => self.locate(offset, pattern.len()).map(Some),
            None => Ok(None),
        }
    }

    fn scan(
        &self,
        finder: &memmem::Finder,
        from: u64,
        to: u64,
        match_case: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Option<u64>> {
        let overlap = finder.needle().len() as u64 - 1;
        let mut pos = from;
        let mut reader = self.reader.lock().unwrap();
        while pos < to {
            if cancelled() {
                return Ok(None);
            }
            let want = (to - pos).min(SEARCH_CHUNK as u64) as usize;
            let mut buf = read_at(&mut reader.file, pos, want)?;
            if buf.is_empty() {
                break;
            }
            if !match_case {
                buf.make_ascii_lowercase();
            }
            let width = newline(self.encoding).len() as u64;
            if let Some(i) = finder
                .find_iter(&buf)
                .find(|&i| (pos + i as u64).is_multiple_of(width))
            {
                return Ok(Some(pos + i as u64));
            }
            let n = buf.len() as u64;
            if pos + n >= to || n <= overlap {
                break;
            }
            pos += n - overlap;
        }
        Ok(None)
    }

    /// Line and column of a match at `offset`.
    fn locate(&self, offset: u64, len: usize) -> io::Result<Found> {
        let mut reader = self.reader.lock().unwrap();
        let (block, first_line) = {
            let st = self.state.read().unwrap();
            let i = st.checkpoints.partition_point(|c| c.offset <= offset) - 1;
            (i, st.checkpoints[i].line)
        };
        let starts = self.block(&mut reader, block)?;
        let j = starts.partition_point(|&s| s <= offset) - 1;
        let line = first_line + j as u64;
        let line_start = starts[j];
        let within = offset - line_start;
        let matched = read_at(&mut reader.file, offset, len)?;
        let length = utf16_len(&self.decode(&matched, false));
        let column = if within + len as u64 <= LINE_CAP as u64 {
            let prefix = read_at(&mut reader.file, line_start, within as usize)?;
            Some(utf16_len(&self.decode(&prefix, line == 0)))
        } else {
            None
        };
        Ok(Found {
            line,
            column,
            length,
            byte_offset: offset,
        })
    }

    /// Byte range of line `l` without its line break (a trailing CR is still included).
    fn line_range(&self, reader: &mut Reader, l: u64) -> io::Result<(u64, u64)> {
        let (block, first_line) = {
            let st = self.state.read().unwrap();
            let i = st.checkpoints.partition_point(|c| c.line <= l) - 1;
            (i, st.checkpoints[i].line)
        };
        let starts = self.block(reader, block)?;
        let j = (l - first_line) as usize;
        Ok((
            starts[j],
            starts[j + 1] - newline(self.encoding).len() as u64,
        ))
    }

    /// Line starts of block `i`, read once and cached.
    fn block(&self, reader: &mut Reader, i: usize) -> io::Result<Arc<Vec<u64>>> {
        if let Some(pos) = reader.blocks.iter().position(|(b, _)| *b == i) {
            let hit = reader.blocks.remove(pos).unwrap();
            let starts = hit.1.clone();
            reader.blocks.push_front(hit);
            return Ok(starts);
        }
        let (here, next) = {
            let st = self.state.read().unwrap();
            match st.checkpoints.get(i + 1) {
                Some(next) => (st.checkpoints[i], *next),
                None => return Err(io::Error::other("line not indexed yet")),
            }
        };
        let lines = (next.line - here.line) as usize;
        let span = next.offset.min(self.size) - here.offset;
        let head = read_at(
            &mut reader.file,
            here.offset,
            span.min(self.params.every_bytes) as usize,
        )?;
        let mut starts = Vec::with_capacity(lines + 1);
        starts.push(here.offset);
        let nl = newline(self.encoding);
        for p in breaks(nl, &head, here.offset) {
            if starts.len() == lines {
                break;
            }
            starts.push(here.offset + (p + nl.len()) as u64);
        }
        if starts.len() != lines {
            return Err(io::Error::other("the file changed while it was open"));
        }
        starts.push(next.offset);
        let starts = Arc::new(starts);
        reader.blocks.push_front((i, starts.clone()));
        reader.blocks.truncate(BLOCK_CACHE);
        Ok(starts)
    }

    fn carriage_return(&self) -> &'static [u8] {
        match self.encoding {
            Encoding::Utf16Le => &[0x0D, 0x00],
            Encoding::Utf16Be => &[0x00, 0x0D],
            _ => b"\r",
        }
    }

    fn decode(&self, bytes: &[u8], first_line: bool) -> String {
        let mark: &[u8] = match self.encoding {
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            _ => &[0xEF, 0xBB, 0xBF],
        };
        let bytes = if first_line && self.bom {
            bytes.strip_prefix(mark).unwrap_or(bytes)
        } else {
            bytes
        };
        let codec = match self.encoding {
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252,
            Encoding::Utf16Le => encoding_rs::UTF_16LE,
            Encoding::Utf16Be => encoding_rs::UTF_16BE,
            _ => return String::from_utf8_lossy(bytes).into_owned(),
        };
        codec.decode_without_bom_handling(bytes).0.into_owned()
    }

    fn encode(&self, text: &str) -> Option<Vec<u8>> {
        match self.encoding {
            Encoding::Windows1252 => {
                let (bytes, _, bad) = encoding_rs::WINDOWS_1252.encode(text);
                (!bad).then(|| bytes.into_owned())
            }
            Encoding::Utf16Le => Some(text.encode_utf16().flat_map(u16::to_le_bytes).collect()),
            Encoding::Utf16Be => Some(text.encode_utf16().flat_map(u16::to_be_bytes).collect()),
            _ => Some(text.as_bytes().to_vec()),
        }
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Reads up to `len` bytes at `offset`; fewer at the end of the file.
fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "isolinear-paged-{name}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Fixture { dir }
        }

        fn index(&self, bytes: &[u8], encoding: Encoding, params: Params) -> LineIndex {
            let p = self.dir.join("f.txt");
            std::fs::write(&p, bytes).unwrap();
            let idx = LineIndex::new(&p, bytes.len() as u64, encoding, false, params).unwrap();
            idx.build(|_| {}).unwrap();
            idx
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const SMALL: Params = Params {
        every_lines: 3,
        every_bytes: 16,
        chunk: 7,
    };

    /// What a reader expects: lines split on LF, a CR before it dropped, no empty line after
    /// a final newline, and one empty line for an empty file.
    fn naive(text: &str) -> Vec<String> {
        let body = text.strip_suffix('\n').unwrap_or(text);
        body.split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
            .collect()
    }

    fn all(idx: &LineIndex) -> Vec<String> {
        let n = idx.progress().lines;
        idx.lines(0, n as usize)
            .unwrap()
            .into_iter()
            .map(|l| l.text)
            .collect()
    }

    #[test]
    fn lines_match_a_plain_split() {
        let f = Fixture::new("split");
        let long = "x".repeat(40);
        let cases = [
            String::new(),
            "one".into(),
            "one\n".into(),
            "\n\n\n".into(),
            "a\r\nb\r\nc".into(),
            format!("short\n{long}\nafter\n{long}"),
            (0..50).map(|i| format!("line {i}\n")).collect(),
            format!("{long}{long}{long}\n"),
        ];
        for text in cases {
            for params in [SMALL, Params::default()] {
                let idx = f.index(text.as_bytes(), Encoding::Utf8, params);
                assert!(idx.progress().complete);
                assert_eq!(all(&idx), naive(&text), "{text:?} with {params:?}");
            }
        }
    }

    #[test]
    fn pages_can_start_anywhere() {
        let f = Fixture::new("pages");
        let text: String = (0..100).map(|i| format!("{i}\n")).collect();
        let idx = f.index(text.as_bytes(), Encoding::Utf8, SMALL);
        let page = idx.lines(41, 5).unwrap();
        let got: Vec<_> = page.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(got, ["41", "42", "43", "44", "45"]);
        assert_eq!(idx.lines(98, 10).unwrap().len(), 2, "clamped at the end");
        assert!(idx.lines(500, 10).unwrap().is_empty());
    }

    #[test]
    fn long_lines_are_cut_and_measured() {
        let f = Fixture::new("long");
        let text = format!("a\n{}\nb\n", "y".repeat(LINE_CAP + 100));
        let idx = f.index(text.as_bytes(), Encoding::Utf8, SMALL);
        let lines = idx.lines(0, 3).unwrap();
        assert_eq!(lines[1].text.len(), LINE_CAP);
        assert!(lines[1].truncated);
        assert_eq!(lines[1].bytes, (LINE_CAP + 100) as u64);
        assert_eq!(lines[2].text, "b");
        assert!(!lines[0].truncated);
    }

    #[test]
    fn windows_1252_and_bom() {
        let f = Fixture::new("enc");
        let idx = f.index(b"caf\xE9\n", Encoding::Windows1252, SMALL);
        assert_eq!(all(&idx), ["café"]);
        let p = f.dir.join("bom.txt");
        std::fs::write(&p, b"\xEF\xBB\xBFfirst\nsecond\n").unwrap();
        let idx = LineIndex::new(&p, 16, Encoding::Utf8, true, SMALL).unwrap();
        idx.build(|_| {}).unwrap();
        assert_eq!(all(&idx), ["first", "second"]);
    }

    #[test]
    fn utf16_lines_and_search_in_both_byte_orders() {
        let f = Fixture::new("u16");
        // Characters whose units hold 0x0A or 0x0D bytes (U+0A0D, U+0D0A, U+010A) must not
        // break lines; a BOM is dropped from the first line only; CRLF and a missing final
        // line feed are handled as in 8-bit text.
        let text = "\u{feff}first \u{a0d} line\r\nsecond \u{d0a}\u{10a}\n\nlast é 𝄞";
        for (encoding, unit) in [
            (Encoding::Utf16Le, u16::to_le_bytes as fn(u16) -> [u8; 2]),
            (Encoding::Utf16Be, u16::to_be_bytes as fn(u16) -> [u8; 2]),
        ] {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(unit).collect();
            for params in [SMALL, Params::default()] {
                let p = f.dir.join("u.txt");
                std::fs::write(&p, &bytes).unwrap();
                let idx = LineIndex::new(&p, bytes.len() as u64, encoding, true, params).unwrap();
                idx.build(|_| {}).unwrap();
                assert_eq!(
                    all(&idx),
                    [
                        "first \u{a0d} line",
                        "second \u{d0a}\u{10a}",
                        "",
                        "last é 𝄞"
                    ],
                    "{encoding:?}"
                );
                let hit = idx
                    .find("LAST", From::Line(0), false, &|| false)
                    .unwrap()
                    .unwrap();
                assert_eq!((hit.line, hit.column, hit.length), (3, Some(0), 4));
                let hit = idx
                    .find("\u{d0a}", From::Line(0), true, &|| false)
                    .unwrap()
                    .unwrap();
                assert_eq!((hit.line, hit.column), (1, Some(7)));
                assert!(
                    idx.find("absent", From::Line(0), true, &|| false)
                        .unwrap()
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn progress_reports_complete_blocks_while_scanning() {
        let f = Fixture::new("progress");
        let text: String = (0..30).map(|i| format!("{i:02}\n")).collect();
        let p = f.dir.join("p.txt");
        std::fs::write(&p, &text).unwrap();
        let idx = LineIndex::new(&p, text.len() as u64, Encoding::Utf8, false, SMALL).unwrap();
        let mut seen = Vec::new();
        idx.build(|p| seen.push(p)).unwrap();
        assert!(seen.windows(2).all(|w| w[0].lines <= w[1].lines));
        assert!(
            seen.iter()
                .any(|p| !p.complete && p.lines > 0 && p.lines < 30)
        );
        assert_eq!(seen.last().unwrap().lines, 30);
        assert!(seen.last().unwrap().complete);
    }

    #[test]
    fn cancelled_build_stops() {
        let f = Fixture::new("cancel");
        let text = "line\n".repeat(1000);
        let p = f.dir.join("c.txt");
        std::fs::write(&p, &text).unwrap();
        let idx = LineIndex::new(&p, text.len() as u64, Encoding::Utf8, false, SMALL).unwrap();
        idx.cancel();
        idx.build(|_| {}).unwrap();
        assert!(!idx.progress().complete);
    }

    #[test]
    fn find_forward_wrapping_and_case() {
        let f = Fixture::new("find");
        let text = "alpha\nbeta Needle\ngamma\ndelta needle here\n";
        let idx = f.index(text.as_bytes(), Encoding::Utf8, SMALL);
        let never = || false;
        let hit = idx
            .find("needle", From::Line(0), false, &never)
            .unwrap()
            .unwrap();
        assert_eq!((hit.line, hit.column, hit.length), (1, Some(5), 6));
        let next = idx
            .find("needle", From::Offset(hit.byte_offset + 1), false, &never)
            .unwrap()
            .unwrap();
        assert_eq!((next.line, next.column), (3, Some(6)));
        let wrapped = idx
            .find("needle", From::Offset(next.byte_offset + 1), false, &never)
            .unwrap()
            .unwrap();
        assert_eq!(wrapped.line, 1, "wraps to the start");
        let exact = idx
            .find("needle", From::Line(0), true, &never)
            .unwrap()
            .unwrap();
        assert_eq!(exact.line, 3, "match case skips 'Needle'");
        assert!(
            idx.find("absent", From::Line(0), false, &never)
                .unwrap()
                .is_none()
        );
        assert!(
            idx.find("", From::Line(0), false, &never)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_across_search_chunks_and_in_unicode() {
        let f = Fixture::new("chunks");
        let mut text = "é😀 x\n".repeat(10);
        text.push_str(&"z".repeat(SEARCH_CHUNK - 3));
        text.push_str("target\n");
        let idx = f.index(text.as_bytes(), Encoding::Utf8, Params::default());
        let never = || false;
        let hit = idx
            .find("target", From::Line(0), false, &never)
            .unwrap()
            .unwrap();
        assert_eq!(hit.line, 10);
        assert_eq!(hit.column, None, "beyond the shown part of the line");
        let x = idx.find("x", From::Line(2), true, &never).unwrap().unwrap();
        assert_eq!(
            (x.line, x.column),
            (2, Some(4)),
            "UTF-16 units: é=1, 😀=2, space=1"
        );
    }

    #[test]
    fn find_stops_when_cancelled() {
        let f = Fixture::new("findcancel");
        let idx = f.index(b"a\nb\nneedle\n", Encoding::Utf8, SMALL);
        let always = || true;
        assert!(
            idx.find("needle", From::Line(0), false, &always)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_changed_file_is_reported_not_misread() {
        let f = Fixture::new("changed");
        let p = f.dir.join("f.txt");
        let text: String = (0..20).map(|i| format!("{i}\n")).collect();
        std::fs::write(&p, &text).unwrap();
        let idx = LineIndex::new(&p, text.len() as u64, Encoding::Utf8, false, SMALL).unwrap();
        idx.build(|_| {}).unwrap();
        std::fs::write(&p, "no newlines at all, same length..").unwrap();
        assert!(idx.lines(10, 3).is_err());
    }
}
