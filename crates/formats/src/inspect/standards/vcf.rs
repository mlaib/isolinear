//! Variant Call Format (VCF 4.x), plain or gzip/BGZF-compressed: meta-information lines
//! (contigs, INFO/FORMAT/FILTER definitions), the variants with per-sample genotypes, and
//! counts of SNVs, indels and chromosomes.
use super::sci_common::{self, latin1};
use crate::inspect::{Handler, Input, Inspection, MAX_INPUT, Probe, Stage, State, Table};
use std::borrow::Cow;
use std::collections::HashSet;

pub struct Vcf;

/// Variant rows kept (all variants are counted).
const MAX_VARIANTS: usize = 100_000;
/// Samples with a genotype column.
const MAX_SAMPLES: usize = 10;
/// Decompressed text read from a compressed file.
const MAX_TEXT: usize = MAX_INPUT as usize;

/// The decompressed (or plain) text of a possibly gzip-compressed text file.
pub(crate) struct Text<'a> {
    pub bytes: Cow<'a, [u8]>,
    pub compressed: bool,
    /// What was not read, in words, when the text is partial.
    pub partial: Option<String>,
}

/// Plain bytes are copied; gzip (including BGZF) is expanded up to [`MAX_TEXT`] bytes.
pub(crate) fn text_of<'a>(b: &'a [u8], cancelled: &dyn Fn() -> bool) -> Text<'a> {
    if !sci_common::is_gzip(b) {
        return Text {
            bytes: Cow::Borrowed(b),
            compressed: false,
            partial: None,
        };
    }
    let out = sci_common::gunzip(b, MAX_TEXT, cancelled);
    let partial = if out.limited {
        Some(format!(
            "only the first {} MiB of decompressed text were read",
            MAX_TEXT >> 20
        ))
    } else {
        out.error
            .map(|e| format!("the compressed stream ends early or is damaged ({e})"))
    };
    Text {
        bytes: Cow::Owned(out.bytes),
        compressed: true,
        partial,
    }
}

/// The first decompressed bytes of a probe head (plain heads as they are).
pub(crate) fn head_text(h: &[u8]) -> Cow<'_, [u8]> {
    if sci_common::is_gzip(h) {
        Cow::Owned(sci_common::gunzip_head(h, 16 * 1024))
    } else {
        Cow::Borrowed(h)
    }
}

/// `ID=DP,Number=1,Type=Integer,Description="Total depth"` as key/value pairs.
fn structured(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut key = String::new();
    let mut val = String::new();
    let mut in_key = true;
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    val.push(n);
                }
            }
            '=' if in_key => in_key = false,
            ',' if !quoted => {
                out.push((std::mem::take(&mut key), std::mem::take(&mut val)));
                in_key = true;
            }
            _ if in_key => key.push(c),
            _ => val.push(c),
        }
        if out.len() > 64 {
            break;
        }
    }
    if !key.is_empty() {
        out.push((key, val));
    }
    out
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

#[derive(PartialEq)]
enum Kind {
    Snv,
    Indel,
    Other,
}
fn classify(reference: &str, alts: &[&str]) -> Kind {
    let base = |s: &str| s.len() == 1 && s.bytes().all(|b| b"ACGTNacgtn".contains(&b));
    let alts: Vec<&str> = alts
        .iter()
        .copied()
        .filter(|a| *a != "." && *a != "*")
        .collect();
    if alts.is_empty() {
        return Kind::Other;
    }
    if base(reference) && alts.iter().all(|a| base(a)) {
        return Kind::Snv;
    }
    let symbolic = |a: &str| a.starts_with('<') || a.contains('[') || a.contains(']');
    if alts.iter().any(|a| symbolic(a)) {
        return Kind::Other;
    }
    if alts.iter().any(|a| a.len() != reference.len()) {
        Kind::Indel
    } else {
        Kind::Other
    }
}

impl Handler for Vcf {
    fn name(&self) -> &'static str {
        "VCF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        head_text(probe.head).starts_with(b"##fileformat=VCF")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = text_of(input.bytes, cancelled);
        let b = &text.bytes;
        let src = |start: usize, len: usize| {
            (!text.compressed).then(|| start as u64..(start + len) as u64)
        };
        let mut i = Inspection::new("VCF", "Variant Call Format — genomic variants");
        let mut contigs = sci_common::table("Contigs", &["ID", "Length", "Other"]);
        let mut info = sci_common::table("INFO fields", &["ID", "Number", "Type", "Description"]);
        let mut format =
            sci_common::table("FORMAT fields", &["ID", "Number", "Type", "Description"]);
        let mut filters = sci_common::table("Filters", &["ID", "Description"]);
        let mut other = sci_common::table("Other header lines", &["Key", "Value"]);
        let mut samples: Vec<String> = Vec::new();
        let mut header_seen = false;
        let mut variants: Option<Table> = None;
        let (mut n, mut snv, mut indel, mut odd, mut multi, mut pass) = (0u64, 0, 0, 0, 0, 0);
        let mut chroms: Vec<String> = Vec::new();
        let mut chrom_set: HashSet<String> = HashSet::new();
        let mut contig_ids: HashSet<String> = HashSet::new();
        let mut malformed = 0u64;
        let mut first_bad: Option<(usize, String)> = None;
        let mut unsorted: Option<(String, u64)> = None;
        let mut last: (String, u64) = (String::new(), 0);
        let mut line_no = 0usize;
        for (start, line) in sci_common::lines(b) {
            line_no += 1;
            if line_no.is_multiple_of(65_536) && cancelled() {
                return Err("Cancelled".into());
            }
            if line.is_empty() {
                continue;
            }
            if let Some(meta) = line.strip_prefix(b"##") {
                let meta = latin1(meta);
                let (key, value) = meta.split_once('=').unwrap_or((meta.as_str(), ""));
                let source = src(start, line.len());
                if let Some(inner) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) {
                    let kv = structured(inner);
                    let id = get(&kv, "ID").to_owned();
                    match key {
                        "contig" => {
                            contig_ids.insert(id.clone());
                            let rest: Vec<String> = kv
                                .iter()
                                .filter(|(k, _)| k != "ID" && k != "length")
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect();
                            sci_common::push(
                                &mut contigs,
                                MAX_VARIANTS,
                                vec![id, get(&kv, "length").to_owned(), rest.join(", ")],
                                source,
                            );
                        }
                        "INFO" | "FORMAT" => {
                            let t = if key == "INFO" {
                                &mut info
                            } else {
                                &mut format
                            };
                            sci_common::push(
                                t,
                                MAX_VARIANTS,
                                vec![
                                    id,
                                    get(&kv, "Number").to_owned(),
                                    get(&kv, "Type").to_owned(),
                                    get(&kv, "Description").to_owned(),
                                ],
                                source,
                            );
                        }
                        "FILTER" => sci_common::push(
                            &mut filters,
                            MAX_VARIANTS,
                            vec![id, get(&kv, "Description").to_owned()],
                            source,
                        ),
                        _ => sci_common::push(
                            &mut other,
                            MAX_VARIANTS,
                            vec![key.to_owned(), sci_common::clip(value, 300)],
                            source,
                        ),
                    }
                } else {
                    let group = "Header";
                    match key {
                        "fileformat" => i.version = Some(value.to_owned()),
                        "reference" => i.fact(group, "Reference", value, source),
                        "source" => i.fact(group, "Source", value, source),
                        "fileDate" => i.fact(group, "File date", value, source),
                        _ => sci_common::push(
                            &mut other,
                            MAX_VARIANTS,
                            vec![key.to_owned(), sci_common::clip(value, 300)],
                            source,
                        ),
                    }
                }
                continue;
            }
            if line.starts_with(b"#") {
                header_seen = true;
                let cols: Vec<String> = latin1(&line[1..]).split('\t').map(str::to_owned).collect();
                samples = cols.iter().skip(9).cloned().collect();
                let mut names: Vec<&str> =
                    vec!["CHROM", "POS", "ID", "REF", "ALT", "QUAL", "FILTER", "INFO"];
                names.extend(samples.iter().take(MAX_SAMPLES).map(String::as_str));
                variants = Some(sci_common::table("Variants", &names));
                continue;
            }
            // A data line.
            let line_text = latin1(line);
            let f: Vec<&str> = line_text.split('\t').collect();
            let expected = if samples.is_empty() {
                8
            } else {
                9 + samples.len()
            };
            let pos = f.get(1).and_then(|p| p.parse::<u64>().ok());
            let ok = header_seen
                && pos.is_some()
                && (f.len() == expected || (samples.is_empty() && f.len() >= 8));
            if !ok {
                malformed += 1;
                if first_bad.is_none() {
                    let why = if !header_seen {
                        "data before the #CHROM header line".to_owned()
                    } else if pos.is_none() {
                        "POS is not a number".to_owned()
                    } else {
                        format!("{} columns instead of {expected}", f.len())
                    };
                    first_bad = Some((line_no, why));
                }
                if f.len() < 8 {
                    continue;
                }
            }
            n += 1;
            let chrom = f[0];
            if !chrom_set.contains(chrom) {
                chrom_set.insert(chrom.to_owned());
                chroms.push(chrom.to_owned());
                last = (chrom.to_owned(), 0);
            }
            let p = pos.unwrap_or(0);
            if last.0 == chrom {
                if p < last.1 && unsorted.is_none() {
                    unsorted = Some((chrom.to_owned(), p));
                }
                last.1 = p;
            } else if unsorted.is_none() {
                // Back to a chromosome seen before: records are not grouped.
                unsorted = Some((chrom.to_owned(), p));
                last = (chrom.to_owned(), p);
            }
            let alts: Vec<&str> = f[4].split(',').collect();
            match classify(f[3], &alts) {
                Kind::Snv => snv += 1,
                Kind::Indel => indel += 1,
                Kind::Other => odd += 1,
            }
            if alts.len() > 1 {
                multi += 1;
            }
            if f[6] == "PASS" {
                pass += 1;
            }
            let Some(t) = variants.as_mut() else {
                continue;
            };
            if !sci_common::has_room(t, MAX_VARIANTS) {
                sci_common::push(t, MAX_VARIANTS, Vec::new(), None);
                continue;
            }
            let mut cells: Vec<String> = f[..7].iter().map(|c| sci_common::clip(c, 200)).collect();
            cells.push(sci_common::clip(f[7], 120));
            let gt = f
                .get(8)
                .and_then(|fmt| fmt.split(':').position(|k| k == "GT"));
            for s in 0..samples.len().min(MAX_SAMPLES) {
                let v = f.get(9 + s).copied().unwrap_or("");
                cells.push(match gt {
                    Some(k) => v.split(':').nth(k).unwrap_or("").to_owned(),
                    None => String::new(),
                });
            }
            sci_common::push(t, MAX_VARIANTS, cells, src(start, line.len()));
        }
        if !header_seen && n == 0 {
            return Err("No #CHROM header line and no variants".into());
        }
        let g = "Variants";
        i.fact("Header", "Samples", samples.len().to_string(), None);
        if !samples.is_empty() {
            let mut names = samples
                .iter()
                .take(MAX_SAMPLES)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if samples.len() > MAX_SAMPLES {
                names.push_str(&format!(", … ({} more)", samples.len() - MAX_SAMPLES));
            }
            i.fact("Header", "Sample names", names, None);
        }
        i.fact(g, "Variants", n.to_string(), None);
        i.fact(g, "Chromosomes", chroms.len().to_string(), None);
        let mut list = chroms
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if chroms.len() > 40 {
            list.push_str(", …");
        }
        i.fact(g, "Chromosome list", list, None);
        i.fact(g, "SNVs", snv.to_string(), None);
        i.fact(g, "Indels", indel.to_string(), None);
        i.fact(
            g,
            "Other (MNV, structural, reference)",
            odd.to_string(),
            None,
        );
        i.fact(g, "Multi-allelic sites", multi.to_string(), None);
        i.fact(g, "Passing all filters (PASS)", pass.to_string(), None);
        if samples.len() > MAX_SAMPLES {
            i.notes.push(format!(
                "Genotypes are shown for the first {MAX_SAMPLES} of {} samples.",
                samples.len()
            ));
        }
        if text.compressed {
            i.notes.push(
                "The file is compressed; rows are not linked to byte positions in it.".into(),
            );
        }
        match &text.partial {
            Some(p) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("{n} variants read; {p}"),
            ),
            None => i.check(
                Stage::Parsed,
                State::Passed,
                format!("{n} variants and the header read"),
            ),
        }
        match first_bad {
            None => i.check(
                Stage::Semantic,
                State::Passed,
                "Every data line has the declared columns and a numeric position",
            ),
            Some((line, why)) => i.check(
                Stage::Semantic,
                State::Failed,
                format!("{malformed} malformed data line(s); the first is line {line}: {why}"),
            ),
        }
        match unsorted {
            None => i.check(
                Stage::Semantic,
                State::Passed,
                "Records are grouped by chromosome and sorted by position",
            ),
            Some((c, p)) => i.check(
                Stage::Semantic,
                State::Failed,
                format!("Records are not sorted: {c}:{p} comes after a later position"),
            ),
        }
        if !contig_ids.is_empty() {
            let undeclared: Vec<&String> =
                chroms.iter().filter(|c| !contig_ids.contains(*c)).collect();
            if undeclared.is_empty() {
                i.check(
                    Stage::Semantic,
                    State::Passed,
                    "Every chromosome is declared in a ##contig line",
                );
            } else {
                i.check(
                    Stage::Semantic,
                    State::Failed,
                    format!(
                        "Chromosomes without a ##contig line: {}",
                        undeclared
                            .iter()
                            .take(10)
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        i.not_validated();
        if let Some(t) = variants {
            i.tables.push(t);
        }
        for t in [contigs, info, format, filters, other] {
            if !t.rows.is_empty() {
                i.tables.push(t);
            }
        }
        Ok(i)
    }
}
