//! FITS (Flexible Image Transport System): every header-data unit's cards, 2D images (the
//! first plane of cubes) with BZERO/BSCALE/BLANK applied, 1D arrays as a plot, and binary and
//! ASCII tables. Data is big-endian and laid out in 2880-byte blocks.
use super::sci_common::{self, Cur};
use crate::inspect::{Channel, Handler, Input, Inspection, Probe, Stage, State, Visual};

pub struct Fits;

const BLOCK: usize = 2880;
const CARD: usize = 80;
/// Header-data units read.
const MAX_HDUS: usize = 1000;
/// Cards listed per header, and header blocks read before giving up on END.
const MAX_CARDS: usize = 10_000;
const MAX_HEADER_BLOCKS: usize = 10_000;
/// Table rows shown per extension.
const MAX_TABLE_ROWS: usize = 10_000;
/// Images and plots made.
const MAX_VISUALS: usize = 16;
const MAX_PIXELS: usize = 64 * 1024 * 1024;
const MAX_PLOT: usize = 1_000_000;
/// Values shown per table cell for repeated fields.
const CELL_VALUES: usize = 8;

struct Card {
    key: String,
    value: String,
    comment: String,
}

fn card(b: &[u8]) -> Card {
    let text = sci_common::latin1(b);
    let key = text.get(..8).unwrap_or(&text).trim_end().to_owned();
    if text.get(8..10) != Some("= ") {
        return Card {
            key,
            value: text.get(8..).unwrap_or("").trim_end().to_owned(),
            comment: String::new(),
        };
    }
    let rest = text.get(10..).unwrap_or("").trim_start();
    if let Some(s) = rest.strip_prefix('\'') {
        // A string: '' is a quote inside it.
        let mut value = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    value.push('\'');
                    continue;
                }
                break;
            }
            value.push(c);
        }
        let after: String = chars.collect();
        let comment = after
            .split_once('/')
            .map_or("", |(_, c)| c)
            .trim()
            .to_owned();
        return Card {
            key,
            value: value.trim_end().to_owned(),
            comment,
        };
    }
    let (v, c) = rest.split_once('/').unwrap_or((rest, ""));
    Card {
        key,
        value: v.trim().to_owned(),
        comment: c.trim().to_owned(),
    }
}

struct Header {
    cards: Vec<Card>,
}
impl Header {
    fn get(&self, key: &str) -> Option<&str> {
        self.cards
            .iter()
            .find(|c| c.key == key)
            .map(|c| c.value.as_str())
    }
    fn int(&self, key: &str) -> Option<i64> {
        self.get(key)?.parse().ok()
    }
    fn float(&self, key: &str) -> Option<f64> {
        self.get(key)?.replace(['D', 'd'], "E").parse().ok()
    }
}

fn bitpix_name(b: i64) -> &'static str {
    match b {
        8 => "8-bit unsigned integer",
        16 => "16-bit integer",
        32 => "32-bit integer",
        64 => "64-bit integer",
        -32 => "32-bit float",
        -64 => "64-bit float",
        _ => "invalid",
    }
}

/// One value of an image or table cell, big-endian.
fn value(c: &mut Cur, bitpix: i64) -> Option<f64> {
    Some(match bitpix {
        8 => c.u8()? as f64,
        16 => c.i16()? as f64,
        32 => c.i32()? as f64,
        64 => c.i64()? as f64,
        -32 => c.f32()? as f64,
        -64 => c.f64()?,
        _ => return None,
    })
}

/// A binary table column: repeat count, type letter, byte width.
struct Field {
    name: String,
    repeat: usize,
    kind: u8,
    offset: usize,
    width: usize,
    scale: f64,
    zero: f64,
}

fn tform(s: &str) -> Option<(usize, u8)> {
    let s = s.trim();
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let repeat = if digits == 0 {
        1
    } else {
        s[..digits].parse().ok()?
    };
    let kind = *s.as_bytes().get(digits)?;
    Some((repeat, kind))
}

fn field_width(repeat: usize, kind: u8) -> Option<usize> {
    let size = match kind {
        b'L' | b'B' | b'A' => 1,
        b'X' => return Some(repeat.div_ceil(8)),
        b'I' => 2,
        b'J' | b'E' => 4,
        b'K' | b'D' | b'C' | b'P' => 8,
        b'M' | b'Q' => 16,
        _ => return None,
    };
    repeat.checked_mul(size)
}

fn cell(f: &Field, row: &[u8]) -> String {
    let Some(b) = f
        .offset
        .checked_add(f.width)
        .and_then(|end| row.get(f.offset..end))
    else {
        return String::new();
    };
    let mut c = Cur::new(b, true);
    let scaled = |v: f64| sci_common::num(v * f.scale + f.zero);
    let one = |c: &mut Cur| -> Option<String> {
        Some(match f.kind {
            b'L' => match c.u8()? {
                b'T' => "T".into(),
                b'F' => "F".into(),
                _ => String::new(),
            },
            b'B' => scaled(c.u8()? as f64),
            b'I' => scaled(c.i16()? as f64),
            b'J' => scaled(c.i32()? as f64),
            b'K' => scaled(c.i64()? as f64),
            b'E' => scaled(c.f32()? as f64),
            b'D' => scaled(c.f64()?),
            b'C' => format!("{}{:+}i", sci_common::num(c.f32()? as f64), c.f32()?),
            b'M' => format!("{}{:+}i", sci_common::num(c.f64()?), c.f64()?),
            _ => return None,
        })
    };
    match f.kind {
        b'A' => sci_common::field(b),
        b'X' => b
            .iter()
            .map(|v| format!("{v:08b}"))
            .collect::<Vec<_>>()
            .join(" "),
        b'P' => {
            let n = c.i32().unwrap_or(0);
            format!("variable array ({n} elements)")
        }
        b'Q' => {
            let n = c.i64().unwrap_or(0);
            format!("variable array ({n} elements)")
        }
        _ if f.repeat == 1 => one(&mut c).unwrap_or_default(),
        _ => {
            let vals: Vec<String> = (0..f.repeat.min(CELL_VALUES))
                .map_while(|_| one(&mut c))
                .collect();
            let more = if f.repeat > CELL_VALUES {
                format!(", … ({})", f.repeat)
            } else {
                String::new()
            };
            format!("[{}{more}]", vals.join(", "))
        }
    }
}

impl Handler for Fits {
    fn name(&self) -> &'static str {
        "FITS"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.head.starts_with(b"SIMPLE  =") && probe.head.len() >= CARD
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        let mut i = Inspection::new("FITS", "FITS — astronomical images and tables");
        let mut at = 0usize;
        let mut problem: Option<String> = None;
        let mut order_problem: Option<String> = None;
        let mut hdus = 0usize;
        while at < b.len() && hdus < MAX_HDUS {
            if cancelled() {
                return Err("Cancelled".into());
            }
            // Header: cards up to END.
            let start = at;
            let mut cards = Vec::new();
            let mut end_found = false;
            let mut blocks = 0usize;
            while !end_found && blocks < MAX_HEADER_BLOCKS {
                let Some(block) = b.get(at..at + BLOCK) else {
                    // A cut last block: read the cards that are there.
                    let rest = &b[at.min(b.len())..];
                    for c in rest.chunks_exact(CARD) {
                        cards.push(card(c));
                    }
                    at = b.len();
                    break;
                };
                for c in block.chunks_exact(CARD) {
                    let parsed = card(c);
                    if parsed.key == "END" {
                        end_found = true;
                        break;
                    }
                    cards.push(parsed);
                }
                at += BLOCK;
                blocks += 1;
            }
            if hdus > 0 && cards.first().is_none_or(|c| c.key != "XTENSION") {
                // Not another HDU: trailing bytes after the last one.
                if b[start..].iter().any(|&c| c != 0 && c != b' ') {
                    i.notes.push(format!(
                        "{} bytes after the last HDU are not a FITS extension.",
                        b.len() - start
                    ));
                }
                break;
            }
            let h = Header { cards };
            let n = hdus;
            hdus += 1;
            if !end_found {
                problem = Some(format!("The header of HDU {n} has no END card"));
            }
            // Mandatory keywords in order.
            let first = if n == 0 { "SIMPLE" } else { "XTENSION" };
            let naxis = h.int("NAXIS").unwrap_or(0);
            let mut expected = vec![first.to_owned(), "BITPIX".into(), "NAXIS".into()];
            for k in 1..=naxis.clamp(0, 999) {
                expected.push(format!("NAXIS{k}"));
            }
            if n > 0 {
                expected.push("PCOUNT".into());
                expected.push("GCOUNT".into());
            }
            let actual: Vec<&str> = h
                .cards
                .iter()
                .take(expected.len())
                .map(|c| c.key.as_str())
                .collect();
            if actual != expected.iter().map(String::as_str).collect::<Vec<_>>()
                && order_problem.is_none()
            {
                order_problem = Some(format!(
                    "HDU {n}: the mandatory keywords are not {}",
                    expected.join(", ")
                ));
            }
            let bitpix = h.int("BITPIX").unwrap_or(0);
            let kind = if n == 0 {
                "Primary".to_owned()
            } else {
                h.get("XTENSION").unwrap_or("").trim().to_owned()
            };
            let dims: Vec<i64> = (1..=naxis.clamp(0, 999))
                .map(|k| h.int(&format!("NAXIS{k}")).unwrap_or(0))
                .collect();
            let groups = h.get("GROUPS") == Some("T") && dims.first() == Some(&0);
            let pcount = h.int("PCOUNT").unwrap_or(0).max(0) as u128;
            let gcount = h.int("GCOUNT").unwrap_or(1).max(0) as u128;
            let elems: u128 = if dims.is_empty() {
                0
            } else {
                dims.iter()
                    .skip(usize::from(groups))
                    .fold(1u128, |a, &d| a.saturating_mul(d.max(0) as u128))
            };
            let data_bytes = (bitpix.unsigned_abs() as u128 / 8)
                .saturating_mul(gcount)
                .saturating_mul(pcount.saturating_add(elems));
            let data_start = at;
            let data_end = (data_start as u128).saturating_add(data_bytes);
            let padded = data_bytes.div_ceil(BLOCK as u128) * BLOCK as u128;
            let data = &b[data_start.min(b.len())..(data_end.min(b.len() as u128) as usize)];
            if data_end > b.len() as u128 {
                problem.get_or_insert(format!(
                    "HDU {n}'s data runs past the end of the file ({data_bytes} bytes declared)"
                ));
            }

            // Facts.
            let g = format!("HDU {n}");
            let name = h.get("EXTNAME").map(str::to_owned);
            i.fact(
                &g,
                "Type",
                kind.clone(),
                Some(start as u64..data_start as u64),
            );
            if let Some(name) = &name {
                i.fact(&g, "Extension name", name.clone(), None);
            }
            let dim_text = dims
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(" × ");
            i.fact(
                &g,
                "Dimensions",
                if dims.is_empty() {
                    "no data".into()
                } else {
                    dim_text.clone()
                },
                None,
            );
            i.fact(
                &g,
                "BITPIX",
                format!("{bitpix} ({})", bitpix_name(bitpix)),
                None,
            );
            if data_bytes > 0 {
                i.fact(
                    &g,
                    "Data bytes",
                    data_bytes.to_string(),
                    Some(data_start as u64..data_end.min(b.len() as u128) as u64),
                );
            }
            if n == 0 {
                for (key, label) in [
                    ("OBJECT", "Object"),
                    ("TELESCOP", "Telescope"),
                    ("INSTRUME", "Instrument"),
                    ("OBSERVER", "Observer name"),
                    ("DATE-OBS", "Observation date"),
                    ("EXPTIME", "Exposure time (s)"),
                    ("FILTER", "Filter"),
                    ("ORIGIN", "Origin"),
                    ("DATE", "File date"),
                    ("RADESYS", "Reference frame"),
                    ("EQUINOX", "Equinox"),
                    ("BUNIT", "Data unit"),
                ] {
                    if let Some(v) = h.get(key) {
                        i.fact("Observation", label, v, None);
                    }
                }
            }
            let mut t =
                sci_common::table(&format!("HDU {n} header"), &["Keyword", "Value", "Comment"]);
            for (k, c) in h.cards.iter().enumerate() {
                if c.key.is_empty() && c.value.is_empty() {
                    continue;
                }
                let card_at = (start + k * CARD) as u64;
                sci_common::push(
                    &mut t,
                    MAX_CARDS,
                    vec![c.key.clone(), c.value.clone(), c.comment.clone()],
                    Some(card_at..card_at + CARD as u64),
                );
            }
            i.tables.push(t);

            // Data.
            let image = n == 0 || kind == "IMAGE";
            let bscale = h.float("BSCALE").unwrap_or(1.0);
            let bzero = h.float("BZERO").unwrap_or(0.0);
            let blank = h.int("BLANK");
            if image && !groups && dims.len() >= 2 && bitpix != 0 && i.visuals.len() < MAX_VISUALS {
                let (w, hgt) = (dims[0].max(0) as usize, dims[1].max(0) as usize);
                let npx = w.saturating_mul(hgt);
                let bytes = bitpix.unsigned_abs() as usize / 8;
                if npx > MAX_PIXELS {
                    i.notes.push(format!(
                        "HDU {n}: the image is too large to show ({npx} pixels)."
                    ));
                } else if npx > 0 && data.len() >= npx * bytes {
                    let mut c = Cur::new(data, true);
                    let mut grey = vec![0f32; npx];
                    for y in 0..hgt {
                        // FITS rows go upwards: the first row is the bottom one.
                        let row = hgt - 1 - y;
                        for x in 0..w {
                            let v = value(&mut c, bitpix).unwrap_or(f64::NAN);
                            let v = if bitpix > 0 && blank.is_some_and(|bl| bl as f64 == v) {
                                f64::NAN
                            } else {
                                v * bscale + bzero
                            };
                            grey[row * w + x] = v as f32;
                        }
                    }
                    let plane = if dims.len() > 2 { " (first plane)" } else { "" };
                    let label = name.clone().unwrap_or_else(|| format!("HDU {n}"));
                    i.visuals.push(sci_common::grey_image(
                        w,
                        hgt,
                        grey,
                        format!("{label} — {w} × {hgt}{plane}"),
                    ));
                } else if npx > 0 {
                    i.notes.push(format!(
                        "HDU {n}: the image data is cut; no image is shown."
                    ));
                }
            } else if image && dims.len() == 1 && bitpix != 0 && i.visuals.len() < MAX_VISUALS {
                let len = (dims[0].max(0) as usize).min(MAX_PLOT);
                let mut c = Cur::new(data, true);
                let samples: Vec<f32> = (0..len)
                    .map_while(|_| value(&mut c, bitpix))
                    .map(|v| (v * bscale + bzero) as f32)
                    .collect();
                if !samples.is_empty() {
                    i.visuals.push(Visual::Plot {
                        channels: vec![Channel {
                            name: name.clone().unwrap_or_else(|| format!("HDU {n}")),
                            unit: h.get("BUNIT").unwrap_or("").to_owned(),
                            rate: 1.0,
                            samples,
                        }],
                    });
                }
            } else if kind == "BINTABLE" || kind == "TABLE" {
                let row_bytes = dims.first().copied().unwrap_or(0).max(0) as usize;
                let rows = dims.get(1).copied().unwrap_or(0).max(0) as u64;
                let nfields = h.int("TFIELDS").unwrap_or(0).clamp(0, 999) as usize;
                i.fact(&g, "Rows", rows.to_string(), None);
                i.fact(&g, "Columns", nfields.to_string(), None);
                let mut fields = Vec::new();
                let mut offset = 0usize;
                let mut bad = None;
                for k in 1..=nfields {
                    let form = h.get(&format!("TFORM{k}")).unwrap_or("");
                    let unit = h.get(&format!("TUNIT{k}")).unwrap_or("");
                    let mut name = h.get(&format!("TTYPE{k}")).unwrap_or("").to_owned();
                    if name.is_empty() {
                        name = format!("Column {k}");
                    }
                    if !unit.is_empty() {
                        name = format!("{name} [{unit}]");
                    }
                    let scale = h.float(&format!("TSCAL{k}")).unwrap_or(1.0);
                    let zero = h.float(&format!("TZERO{k}")).unwrap_or(0.0);
                    if kind == "TABLE" {
                        let col = h.int(&format!("TBCOL{k}")).unwrap_or(1).max(1) as usize - 1;
                        let width: usize = form
                            .trim()
                            .get(1..)
                            .and_then(|w| w.split('.').next())
                            .and_then(|w| w.parse().ok())
                            .unwrap_or(0);
                        fields.push(Field {
                            name,
                            repeat: 1,
                            kind: b'A',
                            offset: col,
                            width,
                            scale,
                            zero,
                        });
                    } else {
                        match tform(form).and_then(|(r, t)| Some((r, t, field_width(r, t)?))) {
                            Some((repeat, kind, width)) => {
                                fields.push(Field {
                                    name,
                                    repeat,
                                    kind,
                                    offset,
                                    width,
                                    scale,
                                    zero,
                                });
                                offset = offset.saturating_add(width);
                            }
                            None => {
                                bad.get_or_insert(format!("TFORM{k} = '{form}' is not understood"));
                                break;
                            }
                        }
                    }
                }
                if let Some(bad) = bad {
                    i.notes
                        .push(format!("HDU {n}: {bad}; the table is not shown."));
                } else if kind == "BINTABLE" && offset > row_bytes {
                    i.notes.push(format!(
                        "HDU {n}: the columns need {offset} bytes per row, NAXIS1 is {row_bytes}; the table is not shown."
                    ));
                } else if row_bytes > 0 {
                    let cols: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                    let title = match &name {
                        Some(e) => format!("HDU {n} table ({e})"),
                        None => format!("HDU {n} table"),
                    };
                    let mut t = sci_common::table(&title, &cols);
                    for (r, row) in data
                        .chunks_exact(row_bytes)
                        .take(MAX_TABLE_ROWS)
                        .enumerate()
                    {
                        let cells = fields.iter().map(|f| {
                            let v = cell(f, row);
                            if kind == "TABLE" {
                                v.trim().to_owned()
                            } else {
                                v
                            }
                        });
                        let row_at = (data_start + r * row_bytes) as u64;
                        sci_common::push(
                            &mut t,
                            MAX_TABLE_ROWS,
                            cells.collect(),
                            Some(row_at..row_at + row_bytes as u64),
                        );
                    }
                    sci_common::set_total(&mut t, rows);
                    i.tables.push(t);
                }
            }
            match usize::try_from((data_start as u128).saturating_add(padded)) {
                Ok(next) if next <= b.len() => at = next,
                _ => break,
            }
        }
        if hdus >= MAX_HDUS && at < b.len() {
            problem.get_or_insert(format!("Only the first {MAX_HDUS} HDUs were read"));
        }
        i.fact("File", "HDUs", hdus.to_string(), None);
        match &problem {
            None => i.check(
                Stage::Parsed,
                State::Passed,
                format!("{hdus} header-data unit(s) read"),
            ),
            Some(p) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("{hdus} HDU(s) read; {p}"),
            ),
        }
        match order_problem {
            None => i.check(
                Stage::Semantic,
                State::Passed,
                "Mandatory keywords present in the required order",
            ),
            Some(p) => i.check(Stage::Semantic, State::Failed, p),
        }
        if input.complete {
            if input.size.is_multiple_of(BLOCK as u64) {
                i.check(
                    Stage::Semantic,
                    State::Passed,
                    "File length is a whole number of 2880-byte blocks",
                );
            } else {
                i.check(
                    Stage::Semantic,
                    State::Failed,
                    format!(
                        "File length {} is not a whole number of 2880-byte blocks",
                        input.size
                    ),
                );
            }
        }
        i.not_validated();
        Ok(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cards_and_forms() {
        let c =
            card(b"OBJECT  = 'M31 ''x'''          / target                                       ");
        assert_eq!(
            (c.key.as_str(), c.value.as_str(), c.comment.as_str()),
            ("OBJECT", "M31 'x'", "target")
        );
        let c = card(
            b"NAXIS1  =                  100 / width                                         ",
        );
        assert_eq!(c.value, "100");
        assert_eq!(tform("16A"), Some((16, b'A')));
        assert_eq!(tform("E"), Some((1, b'E')));
        assert_eq!(field_width(3, b'D'), Some(24));
        assert_eq!(field_width(9, b'X'), Some(2));
    }
}
