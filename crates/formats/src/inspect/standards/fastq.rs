//! FASTQ sequencing reads, plain or gzip-compressed: the first reads are sampled for lengths,
//! qualities (Phred offset 33 or 64 detected) and a per-position quality profile, and the record
//! structure (`@` header, sequence, `+` line, quality of the same length) is checked.
use super::sci_common::{self, latin1};
use super::vcf::{head_text, text_of};
use crate::inspect::{Channel, Handler, Input, Inspection, Probe, Stage, State, Visual};

pub struct Fastq;

const EXTENSIONS: &[&str] = &["fq", "fastq"];
/// Reads sampled.
const SAMPLE: usize = 200_000;
/// Positions in the quality profile.
const MAX_POSITIONS: usize = 10_000;
/// Bases shown per read.
const PREVIEW: usize = 40;

struct Read<'a> {
    id: String,
    seq: &'a [u8],
    qual: &'a [u8],
    start: usize,
    end: usize,
}

impl Handler for Fastq {
    fn name(&self) -> &'static str {
        "FASTQ"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        let named = probe.ext_is(EXTENSIONS)
            || (probe.ext_is(&["gz", "bgz"])
                && probe
                    .inner_ext
                    .as_deref()
                    .is_some_and(|e| EXTENSIONS.contains(&e)));
        named && head_text(probe.head).first() == Some(&b'@')
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = text_of(input.bytes, cancelled);
        let b: &[u8] = &text.bytes;
        let mut i = Inspection::new("FASTQ", "FASTQ — sequencing reads with qualities");
        let mut lines = sci_common::lines(b).peekable();
        let mut reads: Vec<Read> = Vec::new();
        let mut malformed = 0u64;
        let mut first_bad: Option<(usize, String)> = None;
        let mut record_no = 0usize;
        let mut consumed = 0usize;
        while reads.len() < SAMPLE {
            if record_no.is_multiple_of(65_536) && cancelled() {
                return Err("Cancelled".into());
            }
            let Some((start, header)) = lines.next() else {
                break;
            };
            if header.is_empty() {
                consumed = start + 1;
                continue;
            }
            record_no += 1;
            let mut bad = |why: String| {
                malformed += 1;
                if first_bad.is_none() {
                    first_bad = Some((record_no, why));
                }
            };
            if header[0] != b'@' {
                bad("the header line does not start with '@'".into());
                consumed = start + header.len() + 1;
                // Look for the next header.
                while lines.peek().is_some_and(|(_, l)| l.first() != Some(&b'@')) {
                    lines.next();
                }
                continue;
            }
            let (seq, plus, qual) = (lines.next(), lines.next(), lines.next());
            let (Some((_, seq)), Some((_, plus)), Some((qs, qual))) = (seq, plus, qual) else {
                bad("the file ends inside the record".into());
                break;
            };
            consumed = qs + qual.len() + 1;
            if plus.first() != Some(&b'+') {
                bad("the third line does not start with '+'".into());
                continue;
            }
            if seq.len() != qual.len() {
                bad(format!(
                    "sequence length {} differs from quality length {}",
                    seq.len(),
                    qual.len()
                ));
                continue;
            }
            let h = latin1(&header[1..]);
            reads.push(Read {
                id: h.split([' ', '\t']).next().unwrap_or("").to_owned(),
                seq,
                qual,
                start,
                end: qs + qual.len(),
            });
        }
        if reads.is_empty() && first_bad.is_none() {
            return Err("No FASTQ records were found".into());
        }
        // Reads after the sample: four lines each.
        let rest = b.get(consumed.min(b.len())..).unwrap_or(&[]);
        let more_lines = memchr::memchr_iter(b'\n', rest).count()
            + usize::from(rest.last().is_some_and(|&c| c != b'\n'));
        let sampled_all = more_lines == 0;
        let complete = sampled_all || (text.partial.is_none() && input.complete);

        // Phred offset: any quality character below '@' (64) means offset 33.
        let min_q = reads
            .iter()
            .flat_map(|r| r.qual.iter().copied())
            .min()
            .unwrap_or(b'I');
        let offset: u8 = if min_q < 64 { 33 } else { 64 };
        let mut table =
            sci_common::table("Reads", &["ID", "Length", "Mean quality", "First bases"]);
        let mut pos_sum: Vec<f64> = Vec::new();
        let mut pos_n: Vec<u32> = Vec::new();
        let (mut bases, mut qsum, mut gc, mut acgt) = (0u64, 0f64, 0u64, 0u64);
        let (mut min_len, mut max_len) = (usize::MAX, 0usize);
        for r in &reads {
            let mut s = 0f64;
            for (p, &q) in r.qual.iter().enumerate() {
                let v = q.saturating_sub(offset) as f64;
                s += v;
                if p < MAX_POSITIONS {
                    if pos_sum.len() <= p {
                        pos_sum.push(0.0);
                        pos_n.push(0);
                    }
                    pos_sum[p] += v;
                    pos_n[p] += 1;
                }
            }
            for &c in r.seq {
                match c.to_ascii_uppercase() {
                    b'G' | b'C' => {
                        gc += 1;
                        acgt += 1;
                    }
                    b'A' | b'T' => acgt += 1,
                    _ => {}
                }
            }
            bases += r.seq.len() as u64;
            qsum += s;
            min_len = min_len.min(r.seq.len());
            max_len = max_len.max(r.seq.len());
            let mean = if r.qual.is_empty() {
                String::new()
            } else {
                format!("{:.1}", s / r.qual.len() as f64)
            };
            let source = (!text.compressed).then(|| r.start as u64..r.end as u64);
            sci_common::push(
                &mut table,
                SAMPLE,
                vec![
                    r.id.clone(),
                    r.seq.len().to_string(),
                    mean,
                    String::from_utf8_lossy(&r.seq[..r.seq.len().min(PREVIEW)]).into_owned(),
                ],
                source,
            );
        }
        let n = reads.len();
        let g = "Reads";
        i.fact(g, "Reads read", n.to_string(), None);
        if complete {
            let total = n as u64 + more_lines as u64 / 4;
            i.fact(g, "Reads in file", total.to_string(), None);
        } else {
            i.fact(
                g,
                "Reads in file",
                "not known (the file was not read completely)",
                None,
            );
        }
        i.fact(
            g,
            "Phred offset",
            if offset == 33 {
                "33 (Sanger, Illumina 1.8+)"
            } else {
                "64 (Illumina 1.3–1.7)"
            },
            None,
        );
        if n > 0 {
            i.fact(g, "Bases read", bases.to_string(), None);
            i.fact(
                g,
                "Mean length",
                format!("{:.1}", bases as f64 / n as f64),
                None,
            );
            i.fact(g, "Shortest", min_len.to_string(), None);
            i.fact(g, "Longest", max_len.to_string(), None);
            if bases > 0 {
                i.fact(
                    g,
                    "Mean quality",
                    format!("{:.2}", qsum / bases as f64),
                    None,
                );
            }
            if acgt > 0 {
                i.fact(
                    g,
                    "GC %",
                    format!("{:.2}", gc as f64 * 100.0 / acgt as f64),
                    None,
                );
            }
        }
        if !pos_sum.is_empty() {
            let samples = pos_sum
                .iter()
                .zip(&pos_n)
                .map(|(s, &c)| (s / c.max(1) as f64) as f32)
                .collect();
            i.visuals.push(Visual::Plot {
                channels: vec![Channel {
                    name: "Mean quality per position".into(),
                    unit: "Phred".into(),
                    rate: 1.0,
                    samples,
                }],
            });
            i.notes.push(
                "In the quality plot, the horizontal axis is the base position (one per second of the axis)."
                    .into(),
            );
        }
        if text.compressed {
            i.notes.push(
                "The file is compressed; rows are not linked to byte positions in it.".into(),
            );
        }
        if sampled_all && text.partial.is_none() {
            i.check(Stage::Parsed, State::Passed, format!("All {n} reads read"));
        } else {
            let why = text
                .partial
                .clone()
                .unwrap_or_else(|| format!("the first {SAMPLE} reads were sampled"));
            i.check(
                Stage::Parsed,
                State::Partial,
                format!("{n} reads read; {why}"),
            );
        }
        match first_bad {
            None => i.check(
                Stage::Semantic,
                State::Passed,
                "Every sampled record has '@' and '+' lines and equal sequence and quality lengths",
            ),
            Some((rec, why)) => i.check(
                Stage::Semantic,
                State::Failed,
                format!("{malformed} malformed record(s); record {rec}: {why}"),
            ),
        }
        i.not_validated();
        i.tables.push(table);
        Ok(i)
    }
}
