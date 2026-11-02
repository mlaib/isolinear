//! EDF and EDF+ (European Data Format, 16-bit samples) and BDF (BioSemi, 24-bit samples)
//! physiological recordings: the fixed-width ASCII header, the signal table, EDF+ annotations
//! (time-stamped annotation lists) and a plot of the first minute in physical units.
use super::sci_common::{self, field};
use crate::inspect::{Channel, Handler, Input, Inspection, Probe, Stage, State, Visual};

pub struct Edf;

/// Signals in a header (the format allows 9999; real files have a few hundred at most).
const MAX_SIGNALS: usize = 4096;
/// Plotted signals and seconds.
const PLOT_SIGNALS: usize = 16;
const PLOT_SECONDS: f64 = 60.0;
/// Annotation rows kept.
const MAX_ANNOTATIONS: usize = 100_000;
/// Samples per record and signal accepted.
const MAX_SAMPLES: usize = 10_000_000;

struct Signal {
    label: String,
    transducer: String,
    dim: String,
    pmin: f64,
    pmax: f64,
    dmin: f64,
    dmax: f64,
    prefilter: String,
    samples: usize,
    /// Byte offset inside a data record.
    offset: usize,
}
impl Signal {
    fn annotations(&self) -> bool {
        self.label == "EDF Annotations" || self.label == "BDF Annotations"
    }
    fn physical(&self, d: f64) -> f64 {
        let span = self.dmax - self.dmin;
        if span == 0.0 {
            return d;
        }
        (d - self.dmin) * (self.pmax - self.pmin) / span + self.pmin
    }
}

fn int(b: &[u8]) -> Option<i64> {
    field(b).parse().ok()
}
fn dec(b: &[u8]) -> Option<f64> {
    field(b).parse().ok()
}

/// dd.mm.yy with the EDF 1985 clipping year.
fn start_date(d: &str, recording: &str) -> String {
    let parts: Vec<&str> = d.split('.').collect();
    if parts.len() != 3 {
        return d.to_owned();
    }
    // EDF+ puts the full date in the recording field: "Startdate 02-MAR-2002 ...".
    let year = recording
        .strip_prefix("Startdate ")
        .and_then(|r| r.split(' ').next())
        .and_then(|s| s.rsplit('-').next())
        .filter(|y| y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
        .unwrap_or_else(|| match parts[2].parse::<u32>() {
            Ok(y) if y >= 85 => format!("19{:02}", y),
            Ok(y) => format!("20{:02}", y),
            Err(_) => parts[2].to_owned(),
        });
    format!("{year}-{}-{}", parts[1], parts[0])
}

fn duration_text(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return String::new();
    }
    let t = s.round() as u64;
    format!(
        "{:02}:{:02}:{:02} ({} s)",
        t / 3600,
        t / 60 % 60,
        t % 60,
        sci_common::num(s)
    )
}

fn header_ok(h: &[u8]) -> bool {
    if h.len() < 256 {
        return false;
    }
    let edf = &h[..8] == b"0       ";
    let bdf = h[0] == 0xFF && &h[1..8] == b"BIOSEMI";
    if !edf && !bdf {
        return false;
    }
    let (Some(bytes), Some(ns)) = (int(&h[184..192]), int(&h[252..256])) else {
        return false;
    };
    ns >= 0 && bytes == 256 * (ns + 1) && h[168..176].iter().filter(|&&b| b == b'.').count() == 2
}

impl Handler for Edf {
    fn name(&self) -> &'static str {
        "EDF/BDF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        header_ok(probe.head)
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if b.len() < 256 {
            return Err("The header is shorter than 256 bytes".into());
        }
        let bdf = b[0] == 0xFF;
        let bps = if bdf { 3 } else { 2 };
        let reserved = field(&b[192..236]);
        let plus = reserved.starts_with("EDF+") || reserved.starts_with("BDF+");
        let format = match (bdf, reserved.as_str()) {
            (false, r) if r.starts_with("EDF+C") => "EDF+ (continuous)",
            (false, r) if r.starts_with("EDF+D") => "EDF+ (discontinuous)",
            (false, _) => "EDF",
            (true, r) if r.starts_with("BDF+C") => "BDF+ (continuous)",
            (true, r) if r.starts_with("BDF+D") => "BDF+ (discontinuous)",
            (true, _) => "BDF",
        };
        let mut i = Inspection::new(
            if bdf { "BDF" } else { "EDF" },
            &format!("{format} — physiological signal recording"),
        );
        i.version = Some(if bdf {
            "BIOSEMI (24-bit)".into()
        } else {
            format!("version {}", field(&b[..8]))
        });
        let patient = field(&b[8..88]);
        let recording = field(&b[88..168]);
        let ns = int(&b[252..256]).ok_or("Unreadable number of signals")?;
        if !(0..=MAX_SIGNALS as i64).contains(&ns) {
            return Err(format!("Implausible number of signals: {ns}"));
        }
        let ns = ns as usize;
        let header_bytes = int(&b[184..192]).unwrap_or(-1);
        let need = 256 * (ns + 1);
        if b.len() < need {
            return Err(format!(
                "The header is cut: {ns} signals need {need} header bytes, the file has {}",
                b.len()
            ));
        }
        let records = int(&b[236..244]).unwrap_or(-1);
        let record_secs = dec(&b[244..252]).unwrap_or(0.0);
        // Signal fields are stored field by field for all signals.
        let col = |start: usize, width: usize, k: usize| -> &[u8] {
            let at = 256 + start * ns + width * k;
            &b[at..at + width]
        };
        let mut signals = Vec::with_capacity(ns);
        let mut offset = 0usize;
        let mut bad_range = Vec::new();
        for k in 0..ns {
            let samples = int(col(216, 8, k))
                .unwrap_or(0)
                .clamp(0, MAX_SAMPLES as i64) as usize;
            let s = Signal {
                label: field(col(0, 16, k)),
                transducer: field(col(16, 80, k)),
                dim: field(col(96, 8, k)),
                pmin: dec(col(104, 8, k)).unwrap_or(f64::NAN),
                pmax: dec(col(112, 8, k)).unwrap_or(f64::NAN),
                dmin: dec(col(120, 8, k)).unwrap_or(f64::NAN),
                dmax: dec(col(128, 8, k)).unwrap_or(f64::NAN),
                prefilter: field(col(136, 80, k)),
                samples,
                offset,
            };
            let ordered = s.dmin.partial_cmp(&s.dmax) == Some(std::cmp::Ordering::Less);
            if !ordered && !s.annotations() {
                bad_range.push(s.label.clone());
            }
            offset = offset.saturating_add(samples.saturating_mul(bps));
            signals.push(s);
        }
        let record_bytes = offset;
        let data = &b[need.min(b.len())..];
        let available = data.len().checked_div(record_bytes).unwrap_or(0);

        // Header facts.
        i.fact("Recording", "Format", format, Some(0..8));
        i.fact("Patient", "Patient", patient.clone(), Some(8..88));
        if plus {
            let p: Vec<&str> = patient.split(' ').collect();
            let sub = |n: usize| {
                p.get(n)
                    .filter(|v| **v != "X")
                    .map(|v| v.replace('_', " "))
                    .unwrap_or_default()
            };
            i.fact("Patient", "Patient code", sub(0), Some(8..88));
            i.fact("Patient", "Patient sex", sub(1), Some(8..88));
            i.fact("Patient", "Patient birth date", sub(2), Some(8..88));
            i.fact("Patient", "Patient name", sub(3), Some(8..88));
            let r: Vec<&str> = recording.split(' ').collect();
            let sub = |n: usize| {
                r.get(n)
                    .filter(|v| **v != "X")
                    .map(|v| v.replace('_', " "))
                    .unwrap_or_default()
            };
            i.fact(
                "Recording",
                "Hospital administration code",
                sub(2),
                Some(88..168),
            );
            i.fact("Recording", "Technician name", sub(3), Some(88..168));
            i.fact("Recording", "Equipment", sub(4), Some(88..168));
        }
        i.fact("Recording", "Recording", recording.clone(), Some(88..168));
        i.fact(
            "Recording",
            "Start date",
            start_date(&field(&b[168..176]), &recording),
            Some(168..176),
        );
        i.fact(
            "Recording",
            "Start time",
            field(&b[176..184]).replace('.', ":"),
            Some(176..184),
        );
        i.fact(
            "Recording",
            "Data records",
            if records < 0 {
                format!("unknown in the header; {available} in the file")
            } else {
                records.to_string()
            },
            Some(236..244),
        );
        i.fact(
            "Recording",
            "Record duration (s)",
            sci_common::num(record_secs),
            Some(244..252),
        );
        let n_rec = if records >= 0 {
            records as f64
        } else {
            available as f64
        };
        i.fact(
            "Recording",
            "Duration",
            duration_text(n_rec * record_secs),
            None,
        );
        let data_signals = signals.iter().filter(|s| !s.annotations()).count();
        i.fact(
            "Recording",
            "Signals",
            data_signals.to_string(),
            Some(252..256),
        );
        if data_signals != ns {
            i.fact(
                "Recording",
                "Annotation signals",
                (ns - data_signals).to_string(),
                None,
            );
        }

        // Signal table.
        let mut t = sci_common::table(
            "Signals",
            &[
                "Label",
                "Transducer",
                "Physical dimension",
                "Physical min",
                "Physical max",
                "Digital min",
                "Digital max",
                "Prefiltering",
                "Samples per record",
                "Sampling rate (Hz)",
            ],
        );
        for s in &signals {
            let rate = if record_secs > 0.0 {
                sci_common::num(s.samples as f64 / record_secs)
            } else {
                String::new()
            };
            sci_common::push(
                &mut t,
                MAX_SIGNALS,
                vec![
                    s.label.clone(),
                    s.transducer.clone(),
                    s.dim.clone(),
                    sci_common::num(s.pmin),
                    sci_common::num(s.pmax),
                    sci_common::num(s.dmin),
                    sci_common::num(s.dmax),
                    s.prefilter.clone(),
                    s.samples.to_string(),
                    rate,
                ],
                None,
            );
        }
        i.tables.push(t);

        let sample = |s: &Signal, rec: usize, n: usize| -> Option<f64> {
            let at = need + rec * record_bytes + s.offset + n * bps;
            let v = b.get(at..at + bps)?;
            Some(if bdf {
                let raw = (v[0] as i32) | (v[1] as i32) << 8 | (v[2] as i32) << 16;
                ((raw << 8) >> 8) as f64
            } else {
                i16::from_le_bytes([v[0], v[1]]) as f64
            })
        };

        // Annotations (EDF+/BDF+).
        let mut annotations =
            sci_common::table("Annotations", &["Onset (s)", "Duration (s)", "Text"]);
        let mut record_onsets = Vec::new();
        for s in signals.iter().filter(|s| s.annotations()) {
            for rec in 0..available {
                if cancelled() {
                    return Err("Cancelled".into());
                }
                if !sci_common::has_room(&annotations, MAX_ANNOTATIONS) {
                    break;
                }
                let at = need + rec * record_bytes + s.offset;
                let Some(bytes) = b.get(at..at + s.samples * bps) else {
                    break;
                };
                let mut first = true;
                for tal in bytes.split(|&c| c == 0).filter(|t| !t.is_empty()) {
                    let mut parts = tal.split(|&c| c == 0x14);
                    let time = parts.next().unwrap_or_default();
                    let (onset, dur) = match time.iter().position(|&c| c == 0x15) {
                        Some(p) => (&time[..p], &time[p + 1..]),
                        None => (time, &[][..]),
                    };
                    let onset = sci_common::latin1(onset);
                    let texts: Vec<String> = parts
                        .map(sci_common::latin1)
                        .filter(|t| !t.is_empty())
                        .collect();
                    if texts.is_empty() {
                        if first {
                            record_onsets.push(onset.clone());
                        }
                    } else {
                        let src = Some(at as u64..(at + bytes.len()) as u64);
                        for text in texts {
                            sci_common::push(
                                &mut annotations,
                                MAX_ANNOTATIONS,
                                vec![
                                    onset.trim_start_matches('+').to_owned(),
                                    sci_common::latin1(dur),
                                    text,
                                ],
                                src.clone(),
                            );
                        }
                    }
                    first = false;
                }
            }
        }
        if !annotations.rows.is_empty() {
            i.fact(
                "Recording",
                "Annotations",
                annotations
                    .total
                    .unwrap_or(annotations.rows.len() as u64)
                    .to_string(),
                None,
            );
            i.tables.push(annotations);
        }
        if format.contains("discontinuous") {
            i.notes.push(
                "Discontinuous recording: the plot joins the data records without the gaps between them."
                    .into(),
            );
            let mut onsets: Vec<String> = record_onsets
                .iter()
                .take(10)
                .map(|o| o.trim_start_matches('+').to_owned())
                .collect();
            if record_onsets.len() > 10 {
                onsets.push("…".into());
            }
            i.fact(
                "Recording",
                "Data record onsets (s)",
                onsets.join(", "),
                None,
            );
        }

        // Plot of the first minute.
        if record_secs > 0.0 && available > 0 {
            let recs = ((PLOT_SECONDS / record_secs).ceil() as usize).clamp(1, available);
            let mut channels = Vec::new();
            for s in signals
                .iter()
                .filter(|s| !s.annotations() && s.samples > 0)
                .take(PLOT_SIGNALS)
            {
                let rate = s.samples as f64 / record_secs;
                let keep = ((PLOT_SECONDS * rate).floor() as usize).max(1);
                let mut samples = Vec::with_capacity(keep.min(recs * s.samples));
                'records: for rec in 0..recs {
                    for n in 0..s.samples {
                        if samples.len() >= keep {
                            break 'records;
                        }
                        match sample(s, rec, n) {
                            Some(d) => samples.push(s.physical(d) as f32),
                            None => break 'records,
                        }
                    }
                }
                channels.push(Channel {
                    name: s.label.clone(),
                    unit: s.dim.clone(),
                    rate,
                    samples,
                });
            }
            if !channels.is_empty() {
                i.visuals.push(Visual::Plot { channels });
                if data_signals > PLOT_SIGNALS {
                    i.notes.push(format!(
                        "The plot shows the first {PLOT_SIGNALS} of {data_signals} signals."
                    ));
                }
            }
        }

        // Checks.
        let expected = (records.max(0) as u128) * record_bytes as u128 + need as u128;
        let actual = input.size as u128;
        if records < 0 {
            i.check(
                Stage::Semantic,
                State::Partial,
                format!(
                    "The header does not give the number of data records (-1); {available} complete records are in the file"
                ),
            );
        } else if expected == actual {
            i.check(
                Stage::Semantic,
                State::Passed,
                format!("File size matches the header ({records} records of {record_bytes} bytes)"),
            );
        } else {
            i.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "File size does not match the header: {records} records of {record_bytes} bytes need {expected} bytes, the file has {actual}"
                ),
            );
        }
        if header_bytes != need as i64 {
            i.check(
                Stage::Semantic,
                State::Failed,
                format!("Header size field says {header_bytes} bytes; {ns} signals need {need}"),
            );
        }
        if bad_range.is_empty() {
            i.check(
                Stage::Semantic,
                State::Passed,
                "Digital minimum is below the digital maximum for every signal",
            );
        } else {
            i.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "Digital minimum is not below the maximum for: {}",
                    bad_range.join(", ")
                ),
            );
        }
        if records >= 0 && (available as i64) < records {
            i.check(
                Stage::Parsed,
                State::Partial,
                format!("Only {available} of {records} data records are in the file"),
            );
        } else {
            i.check(
                Stage::Parsed,
                State::Passed,
                format!("Header, {ns} signal descriptions and {available} data records read"),
            );
        }
        i.not_validated();
        Ok(i)
    }
}
