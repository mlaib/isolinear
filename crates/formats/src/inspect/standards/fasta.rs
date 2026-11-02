//! FASTA sequence files (nucleotide or protein), plain or gzip-compressed: one row per
//! sequence with its length, GC content and composition, and assembly-style totals (N50).
use super::sci_common::{self, latin1};
use super::vcf::{head_text, text_of};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::HashSet;

pub struct Fasta;

const EXTENSIONS: &[&str] = &["fa", "fasta", "fna", "faa", "ffn", "frn", "fas", "mfa"];
/// Characters of each sequence shown in the table.
const PREVIEW: usize = 80;

#[derive(Default)]
struct Record {
    id: String,
    description: String,
    start: usize,
    end: usize,
    length: u64,
    /// A, C, G, T/U, N and other letters.
    acgtun: u64,
    gc: u64,
    at: u64,
    letters: u64,
    preview: String,
}
impl Record {
    fn nucleotide(&self) -> bool {
        self.letters > 0 && self.acgtun * 10 >= self.letters * 9
    }
}

fn is_fasta_name(p: &Probe) -> bool {
    p.ext_is(EXTENSIONS)
        || (p.ext_is(&["gz", "bgz"])
            && p.inner_ext
                .as_deref()
                .is_some_and(|e| EXTENSIONS.contains(&e)))
}

impl Handler for Fasta {
    fn name(&self) -> &'static str {
        "FASTA"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        is_fasta_name(probe) && {
            let h = head_text(probe.head);
            h.first() == Some(&b'>') || h.first() == Some(&b';')
        }
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = text_of(input.bytes, cancelled);
        let b = &text.bytes;
        let mut i = Inspection::new("FASTA", "FASTA — biological sequences");
        let mut table = sci_common::table(
            "Sequences",
            &[
                "ID",
                "Description",
                "Length",
                "GC %",
                "Type",
                "First 80 characters",
            ],
        );
        let mut lengths: Vec<u64> = Vec::new();
        let (mut total_gc, mut total_at, mut nucleotide, mut protein) = (0u64, 0u64, 0u64, 0u64);
        let mut empty = 0u64;
        let mut before_header = false;
        let mut ids: HashSet<String> = HashSet::new();
        let mut duplicate: Option<String> = None;
        let mut current: Option<Record> = None;
        let mut finish = |r: Record, table: &mut crate::inspect::Table| {
            lengths.push(r.length);
            if r.length == 0 {
                empty += 1;
            }
            let nuc = r.nucleotide();
            if nuc {
                nucleotide += 1;
                total_gc += r.gc;
                total_at += r.at;
            } else if r.letters > 0 {
                protein += 1;
            }
            let gc = if nuc && r.gc + r.at > 0 {
                format!("{:.1}", r.gc as f64 * 100.0 / (r.gc + r.at) as f64)
            } else {
                String::new()
            };
            let kind = match (nuc, r.letters) {
                (_, 0) => "",
                (true, _) => "nucleotide",
                (false, _) => "protein",
            };
            let source = (!text.compressed).then(|| r.start as u64..r.end as u64);
            sci_common::push(
                table,
                crate::inspect::MAX_ROWS,
                vec![
                    r.id,
                    r.description,
                    r.length.to_string(),
                    gc,
                    kind.into(),
                    r.preview,
                ],
                source,
            );
        };
        for (n, (start, line)) in sci_common::lines(b).enumerate() {
            if n.is_multiple_of(65_536) && cancelled() {
                return Err("Cancelled".into());
            }
            if let Some(header) = line.strip_prefix(b">") {
                if let Some(r) = current.take() {
                    finish(r, &mut table);
                }
                let h = latin1(header);
                let (id, desc) = h.split_once([' ', '\t']).unwrap_or((h.as_str(), ""));
                if !ids.insert(id.to_owned()) && duplicate.is_none() {
                    duplicate = Some(id.to_owned());
                }
                current = Some(Record {
                    id: id.to_owned(),
                    description: desc.trim().to_owned(),
                    start,
                    end: start + line.len(),
                    ..Record::default()
                });
                continue;
            }
            if line.starts_with(b";") {
                continue;
            }
            let Some(r) = current.as_mut() else {
                if line.iter().any(|c| !c.is_ascii_whitespace()) {
                    before_header = true;
                }
                continue;
            };
            r.end = start + line.len();
            for &c in line {
                if c.is_ascii_whitespace() {
                    continue;
                }
                r.length += 1;
                if r.preview.len() < PREVIEW {
                    r.preview.push(c as char);
                }
                if c.is_ascii_alphabetic() {
                    r.letters += 1;
                    match c.to_ascii_uppercase() {
                        b'G' | b'C' => {
                            r.gc += 1;
                            r.acgtun += 1;
                        }
                        b'A' | b'T' | b'U' => {
                            r.at += 1;
                            r.acgtun += 1;
                        }
                        b'N' => r.acgtun += 1,
                        _ => {}
                    }
                }
            }
        }
        if let Some(r) = current.take() {
            finish(r, &mut table);
        }
        if lengths.is_empty() {
            return Err("No sequence header ('>') was found".into());
        }
        let count = lengths.len() as u64;
        let total: u64 = lengths.iter().sum();
        let longest = lengths.iter().copied().max().unwrap_or(0);
        let shortest = lengths.iter().copied().min().unwrap_or(0);
        let n50 = sci_common::n50(&mut lengths);
        let g = "Sequences";
        i.fact(g, "Sequences", count.to_string(), None);
        i.fact(g, "Total length", total.to_string(), None);
        i.fact(g, "N50", n50.to_string(), None);
        i.fact(g, "Longest", longest.to_string(), None);
        i.fact(g, "Shortest", shortest.to_string(), None);
        i.fact(
            g,
            "Mean length",
            sci_common::num((total as f64 / count as f64 * 10.0).round() / 10.0),
            None,
        );
        let kind = match (nucleotide, protein) {
            (0, 0) => "empty",
            (_, 0) => "nucleotide",
            (0, _) => "protein",
            _ => "mixed nucleotide and protein",
        };
        i.fact(g, "Composition", kind, None);
        if total_gc + total_at > 0 {
            i.fact(
                g,
                "GC % (nucleotide sequences)",
                format!(
                    "{:.2}",
                    total_gc as f64 * 100.0 / (total_gc + total_at) as f64
                ),
                None,
            );
        }
        if text.compressed {
            i.notes.push(
                "The file is compressed; rows are not linked to byte positions in it.".into(),
            );
        }
        i.notes.push(
            "The composition is guessed from the letters: a sequence is nucleotide when at least 90% of its letters are A, C, G, T, U or N."
                .into(),
        );
        match &text.partial {
            Some(p) => i.check(
                Stage::Parsed,
                State::Partial,
                format!("{count} sequences read; {p}"),
            ),
            None => i.check(
                Stage::Parsed,
                State::Passed,
                format!("{count} sequences read"),
            ),
        }
        let mut problems = Vec::new();
        if before_header {
            problems.push("text before the first header".to_owned());
        }
        if empty > 0 {
            problems.push(format!("{empty} empty sequence(s)"));
        }
        if let Some(d) = duplicate {
            problems.push(format!("duplicate ID {d}"));
        }
        if problems.is_empty() {
            i.check(
                Stage::Semantic,
                State::Passed,
                "Every sequence has a unique ID and residues",
            );
        } else {
            i.check(Stage::Semantic, State::Failed, problems.join("; "));
        }
        i.not_validated();
        i.tables.push(table);
        Ok(i)
    }
}
