//! The Overview of a file in an industry standard: what the standard reader found, shaped for the
//! page and capped so a large file never sends more than the page can show. Every fact, row and
//! outline entry keeps its byte range in the file, and, for text files, its place in the text.

use std::path::Path;

use isolinear_core::encoding::{self, Encoding};
use isolinear_formats::inspect::{self, Geometry, Inspection, Visual};
use isolinear_formats::tree::Offsets;
use serde::Serialize;

const ROWS_SHOWN: usize = 5000;
const OUTLINE_SHOWN: usize = 20_000;
const POINTS_PER_CHANNEL: usize = 4000;
const PIXELS_MAX: usize = 16 * 1024 * 1024;
const TRIANGLES_MAX: usize = 400_000;
/// Text files up to this size get positions in the text, so a fact can be shown in Source.
const TEXT_MAX: u64 = 16 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    family: String,
    kind: String,
    version: Option<String>,
    checks: Vec<CheckDto>,
    facts: Vec<FactDto>,
    tables: Vec<TableDto>,
    outline: Vec<OutlineDto>,
    outline_total: usize,
    visuals: Vec<VisualDto>,
    notes: Vec<String>,
}

#[derive(Serialize, Clone, Copy)]
pub struct Source {
    /// Byte range in the file.
    from: u64,
    to: u64,
    /// UTF-16 range in the decoded text, for text files.
    text: Option<(u32, u32)>,
}

#[derive(Serialize)]
pub struct CheckDto {
    stage: &'static str,
    state: &'static str,
    detail: String,
}

#[derive(Serialize)]
pub struct FactDto {
    group: String,
    label: String,
    value: String,
    source: Option<Source>,
}

#[derive(Serialize)]
pub struct TableDto {
    name: String,
    columns: Vec<String>,
    rows: Vec<RowDto>,
    /// Rows in the file (more than were kept, or than are shown).
    total: usize,
}

#[derive(Serialize)]
pub struct RowDto {
    cells: Vec<String>,
    source: Option<Source>,
}

#[derive(Serialize)]
pub struct OutlineDto {
    depth: u16,
    label: String,
    value: String,
    source: Option<Source>,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VisualDto {
    Map {
        features: Vec<FeatureDto>,
        geographic: bool,
    },
    Plot {
        channels: Vec<ChannelDto>,
    },
    Drawing {
        paths: Vec<Vec<[f32; 2]>>,
        unit: String,
    },
    Picture {
        uri: String,
        caption: String,
    },
    Image {
        width: u32,
        height: u32,
        /// RGBA bytes, base64.
        rgba: String,
        caption: String,
    },
    Timeline {
        rows: Vec<SpanDto>,
        phases: Vec<String>,
        unit: String,
    },
    Mesh {
        /// x, y, z per vertex.
        vertices: Vec<f32>,
        /// Three vertex indices per triangle.
        triangles: Vec<u32>,
        shown: usize,
        total: usize,
    },
    /// A visual that could not be shown, and why.
    Note {
        text: String,
    },
}

#[derive(Serialize)]
pub struct FeatureDto {
    name: String,
    /// `points`, `lines` or `polygons`.
    shape: &'static str,
    parts: Vec<Vec<[f64; 2]>>,
}

#[derive(Serialize)]
pub struct ChannelDto {
    name: String,
    unit: String,
    /// (seconds, value), downsampled to keep the minimum and maximum of every stretch.
    points: Vec<[f32; 2]>,
    samples: usize,
}

#[derive(Serialize)]
pub struct SpanDto {
    label: String,
    start: f64,
    parts: Vec<f64>,
    source: Option<Source>,
}

/// Maps byte ranges of the file to UTF-16 ranges of its decoded text.
struct TextMap {
    text: String,
    offsets: Offsets,
    encoding: Encoding,
    bom: bool,
}

impl TextMap {
    fn open(path: &Path, format: &str) -> Option<TextMap> {
        let spec = isolinear_formats::registry::by_id(format)?;
        if spec.kind != isolinear_formats::registry::Kind::Text
            || std::fs::metadata(path).ok()?.len() > TEXT_MAX
        {
            return None;
        }
        let d = encoding::decode(&std::fs::read(path).ok()?);
        Some(TextMap {
            offsets: Offsets::new(&d.text),
            text: d.text,
            encoding: d.encoding,
            bom: d.bom,
        })
    }

    fn utf16(&self, byte: u64) -> u32 {
        match self.encoding {
            Encoding::Utf8 => {
                let b = byte.saturating_sub(if self.bom { 3 } else { 0 });
                self.offsets
                    .to_utf16(&self.text, b.min(u32::MAX as u64) as u32)
            }
            // One byte per character, all in the Basic Multilingual Plane.
            Encoding::Windows1252 => byte.min(u32::MAX as u64) as u32,
            Encoding::Utf16Le | Encoding::Utf16Be => (byte.saturating_sub(2) / 2) as u32,
        }
    }
}

fn source(map: Option<&TextMap>, r: &Option<std::ops::Range<u64>>) -> Option<Source> {
    let r = r.as_ref()?;
    Some(Source {
        from: r.start,
        to: r.end,
        text: map.map(|m| (m.utf16(r.start), m.utf16(r.end))),
    })
}

fn stage(s: inspect::Stage) -> &'static str {
    s.name()
}

fn state(s: inspect::State) -> &'static str {
    match s {
        inspect::State::Passed => "passed",
        inspect::State::Partial => "partial",
        inspect::State::Failed => "failed",
        inspect::State::NotDone => "notDone",
    }
}

fn picture_uri(bytes: &[u8]) -> Option<String> {
    let mime = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'B', b'M', ..] => "image/bmp",
        _ => return None,
    };
    Some(format!(
        "data:{mime};base64,{}",
        isolinear_formats::base64(bytes)
    ))
}

/// Keeps the minimum and maximum of each stretch, so peaks survive downsampling.
fn downsample(samples: &[f32], rate: f64) -> Vec<[f32; 2]> {
    let rate = if rate > 0.0 { rate } else { 1.0 };
    if samples.len() <= POINTS_PER_CHANNEL {
        return samples
            .iter()
            .enumerate()
            .map(|(i, &v)| [(i as f64 / rate) as f32, v])
            .collect();
    }
    let buckets = POINTS_PER_CHANNEL / 2;
    let step = samples.len().div_ceil(buckets);
    let mut out = Vec::with_capacity(POINTS_PER_CHANNEL);
    for (b, chunk) in samples.chunks(step).enumerate() {
        let base = b * step;
        let (mut lo, mut hi) = (0, 0);
        for (i, v) in chunk.iter().enumerate() {
            if *v < chunk[lo] {
                lo = i;
            }
            if *v > chunk[hi] {
                hi = i;
            }
        }
        for i in if lo <= hi { [lo, hi] } else { [hi, lo] } {
            out.push([((base + i) as f64 / rate) as f32, chunk[i]]);
        }
    }
    out
}

fn visual(v: Visual, map: Option<&TextMap>) -> VisualDto {
    match v {
        Visual::Map {
            features,
            geographic,
        } => VisualDto::Map {
            features: features
                .into_iter()
                .map(|f| {
                    let (shape, parts) = match f.geometry {
                        Geometry::Points(p) => ("points", vec![p]),
                        Geometry::Lines(l) => ("lines", l),
                        Geometry::Polygons(p) => ("polygons", p),
                    };
                    FeatureDto {
                        name: f.name,
                        shape,
                        parts,
                    }
                })
                .collect(),
            geographic,
        },
        Visual::Plot { channels } => VisualDto::Plot {
            channels: channels
                .into_iter()
                .map(|c| ChannelDto {
                    points: downsample(&c.samples, c.rate),
                    samples: c.samples.len(),
                    name: c.name,
                    unit: c.unit,
                })
                .collect(),
        },
        Visual::Mesh {
            vertices,
            triangles,
        } => {
            let total = triangles.len();
            VisualDto::Mesh {
                vertices: vertices.iter().flatten().copied().collect(),
                triangles: triangles
                    .iter()
                    .take(TRIANGLES_MAX)
                    .flatten()
                    .copied()
                    .collect(),
                shown: total.min(TRIANGLES_MAX),
                total,
            }
        }
        Visual::Drawing { paths, unit } => VisualDto::Drawing { paths, unit },
        Visual::Picture { bytes, caption } => match picture_uri(&bytes) {
            Some(uri) => VisualDto::Picture { uri, caption },
            None => VisualDto::Note {
                text: format!("{caption}: this picture's format cannot be shown"),
            },
        },
        Visual::Image {
            width,
            height,
            grey,
            rgba,
            caption,
        } => {
            let pixels = width as usize * height as usize;
            if pixels == 0 || pixels > PIXELS_MAX {
                return VisualDto::Note {
                    text: format!("{caption}: {width} × {height} pixels is too large to show"),
                };
            }
            let bytes = match (grey, rgba) {
                (_, Some(rgba)) if rgba.len() == pixels * 4 => rgba,
                (Some(grey), _) if grey.len() == pixels => {
                    // Window: the full range of values, stretched to 0–255.
                    let (lo, hi) = grey
                        .iter()
                        .filter(|v| v.is_finite())
                        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
                    let span = if hi > lo { hi - lo } else { 1.0 };
                    grey.iter()
                        .flat_map(|&v| {
                            let g = (((v - lo) / span) * 255.0).clamp(0.0, 255.0) as u8;
                            [g, g, g, 255]
                        })
                        .collect()
                }
                _ => {
                    return VisualDto::Note {
                        text: format!("{caption}: the pixel data does not match its size"),
                    };
                }
            };
            VisualDto::Image {
                width,
                height,
                rgba: isolinear_formats::base64(&bytes),
                caption,
            }
        }
        Visual::Timeline { rows, phases, unit } => VisualDto::Timeline {
            rows: rows
                .into_iter()
                .take(ROWS_SHOWN)
                .map(|s| SpanDto {
                    source: source(map, &s.source),
                    label: s.label,
                    start: s.start,
                    parts: s.parts,
                })
                .collect(),
            phases,
            unit,
        },
    }
}

fn shape(i: Inspection, map: Option<&TextMap>) -> Overview {
    let outline_total = i.outline.len();
    Overview {
        family: i.family,
        kind: i.kind,
        version: i.version,
        checks: i
            .checks
            .into_iter()
            .map(|c| CheckDto {
                stage: stage(c.stage),
                state: state(c.state),
                detail: c.detail,
            })
            .collect(),
        facts: i
            .facts
            .into_iter()
            .map(|f| FactDto {
                source: source(map, &f.source),
                group: f.group,
                label: f.label,
                value: f.value,
            })
            .collect(),
        tables: i
            .tables
            .into_iter()
            .map(|t| TableDto {
                total: t.total.map_or(t.rows.len(), |n| n as usize),
                name: t.name,
                columns: t.columns,
                rows: t
                    .rows
                    .into_iter()
                    .take(ROWS_SHOWN)
                    .map(|r| RowDto {
                        source: source(map, &r.source),
                        cells: r.cells,
                    })
                    .collect(),
            })
            .collect(),
        outline: i
            .outline
            .into_iter()
            .take(OUTLINE_SHOWN)
            .map(|o| OutlineDto {
                source: source(map, &o.source),
                depth: o.depth,
                label: o.label,
                value: o.value,
            })
            .collect(),
        outline_total,
        visuals: i.visuals.into_iter().map(|v| visual(v, map)).collect(),
        notes: i.notes,
    }
}

/// The name of the standard the file is in, from its first bytes; `None` for none.
pub fn recognize(path: &Path, head: &[u8], size: u64, format: &str) -> Option<&'static str> {
    let probe = inspect::probe(path, head, size, format);
    inspect::recognize(&probe).map(|h| h.name())
}

#[tauri::command]
pub async fn inspect_document(path: String, format: String) -> Result<Option<Overview>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = Path::new(&path);
        let Some(result) = inspect::inspect_file(p, &format, &|| false) else {
            return Ok(None);
        };
        let inspection = result?;
        let map = TextMap::open(p, &format);
        Ok(Some(shape(inspection, map.as_ref())))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsampling_keeps_peaks() {
        let mut s = vec![0.0f32; 100_000];
        s[54_321] = 9.0;
        s[77_777] = -4.0;
        let d = downsample(&s, 1000.0);
        assert!(d.len() <= POINTS_PER_CHANNEL + 2);
        assert!(d.iter().any(|p| p[1] == 9.0));
        assert!(d.iter().any(|p| p[1] == -4.0));
        assert!((d.iter().find(|p| p[1] == 9.0).unwrap()[0] - 54.321).abs() < 1e-3);
    }

    #[test]
    fn pictures_need_a_known_signature() {
        assert!(
            picture_uri(b"\x89PNG\r\n\x1a\n")
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(
            picture_uri(b"II*\0").is_none(),
            "TIFF cannot be shown in the page"
        );
    }
}
