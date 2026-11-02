//! TIFF pictures for the Preview, which the web view cannot show: the first image is decoded in
//! Rust (strips or tiles; none, LZW, Deflate, PackBits, JPEG and fax compression), reduced to fit
//! the display limits, and written as PNG. Palette pictures, which that decoder refuses, are
//! read in `palette`. Only the file's own bytes are read.

use std::io::Cursor;
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

/// Largest decoded image held in memory, in bytes.
const DECODE_MAX: usize = 256 * 1024 * 1024;
/// Largest side and pixel count shown; bigger pictures are reduced (nearest pixel).
const SHOWN_SIDE: u32 = 8192;
const SHOWN_PIXELS: u64 = 40_000_000;

/// A decoded picture: straight RGBA, 8 bits a sample, row after row.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Width and height in the file, before any reduction.
    pub original: (u32, u32),
}

/// The first image of a TIFF file as PNG.
pub fn to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let p = decode(bytes)?;
    // `decode` keeps both sides within 8192, so they fit the encoder.
    crate::pdf::png::encode(p.width as u16, p.height as u16, &p.rgba)
}

/// The first image of a TIFF file, reduced to fit the display limits.
pub fn decode(bytes: &[u8]) -> Result<Picture, String> {
    std::panic::catch_unwind(|| decode_unguarded(bytes))
        .unwrap_or_else(|_| Err("The TIFF picture cannot be decoded: it is damaged".into()))
}

#[derive(Clone, Copy, PartialEq)]
enum Model {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
    Cmyk,
    Cmyka,
    YCbCr,
    /// Several bands of measurements: the first is shown in grey.
    Bands,
}

fn err(e: tiff::TiffError) -> String {
    format!("The TIFF picture cannot be decoded: {e}")
}

fn decode_unguarded(bytes: &[u8]) -> Result<Picture, String> {
    if palette::is_palette(bytes) {
        return palette::decode(bytes);
    }
    let mut limits = Limits::default();
    limits.decoding_buffer_size = DECODE_MAX;
    let mut d = Decoder::new(Cursor::new(bytes))
        .map_err(err)?
        .with_limits(limits);
    let (w, h) = d.dimensions().map_err(err)?;
    if w == 0 || h == 0 {
        return Err("The TIFF picture is empty".into());
    }
    let (model, bits, channels) = match d.colortype().map_err(err)? {
        ColorType::Gray(b) => (Model::Gray, b, 1),
        ColorType::GrayA(b) => (Model::GrayAlpha, b, 2),
        ColorType::RGB(b) => (Model::Rgb, b, 3),
        ColorType::RGBA(b) => (Model::Rgba, b, 4),
        ColorType::CMYK(b) => (Model::Cmyk, b, 4),
        ColorType::CMYKA(b) => (Model::Cmyka, b, 5),
        ColorType::YCbCr(b) => (Model::YCbCr, b, 3),
        ColorType::Multiband {
            bit_depth,
            num_samples,
        } if num_samples > 0 => (Model::Bands, bit_depth, usize::from(num_samples)),
        other => return Err(format!("TIFF pictures in {other:?} are not shown yet")),
    };
    if bits == 0 || bits > 64 {
        return Err("The TIFF picture has an invalid bit depth".into());
    }
    let planar = d
        .find_tag_unsigned::<u16>(Tag::PlanarConfiguration)
        .map_err(err)?
        == Some(2);
    // Every plane of a planar image (`read_image` reads only the first); white-is-zero grey is
    // inverted by the decoder.
    let mut result = DecodingResult::U8(Vec::new());
    d.read_image_to_buffer(&mut result).map_err(err)?;
    let samples = Samples::new(result, bits);

    let (w, h, c) = (w as usize, h as usize, channels);
    // Packed samples (under 8 bits) are counted by row, padded to a byte.
    let packed = samples.bits < 8;
    let row_bytes = (w * c * usize::from(samples.bits)).div_ceil(8);
    let needed = if packed {
        row_bytes * h
    } else {
        w.checked_mul(h)
            .and_then(|n| n.checked_mul(c))
            .ok_or("The TIFF picture is too large")?
    };
    if samples.data.len() < needed {
        return Err("The TIFF picture data is shorter than its size says".into());
    }
    let get = |x: usize, y: usize, ch: usize| -> u8 {
        if packed {
            let b = usize::from(samples.bits);
            let bit = (x * c + ch) * b;
            let byte = samples.data[y * row_bytes + bit / 8];
            let v = (byte >> (8 - b - bit % 8)) & ((1u8 << b) - 1);
            (u16::from(v) * 255 / ((1u16 << b) - 1)) as u8
        } else if planar {
            samples.data[ch * w * h + y * w + x]
        } else {
            samples.data[(y * w + x) * c + ch]
        }
    };

    let step = reduction(w as u64, h as u64);
    let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        for ox in 0..ow {
            let (x, y) = (ox * step, oy * step);
            let pixel = match model {
                Model::Gray | Model::Bands => {
                    let g = get(x, y, 0);
                    [g, g, g, 255]
                }
                Model::GrayAlpha => {
                    let g = get(x, y, 0);
                    [g, g, g, get(x, y, 1)]
                }
                Model::Rgb => [get(x, y, 0), get(x, y, 1), get(x, y, 2), 255],
                Model::Rgba => [get(x, y, 0), get(x, y, 1), get(x, y, 2), get(x, y, 3)],
                Model::Cmyk | Model::Cmyka => {
                    let k = 255 - u16::from(get(x, y, 3));
                    let ink = |v: u8| ((255 - u16::from(v)) * k / 255) as u8;
                    let alpha = if model == Model::Cmyka {
                        get(x, y, 4)
                    } else {
                        255
                    };
                    [
                        ink(get(x, y, 0)),
                        ink(get(x, y, 1)),
                        ink(get(x, y, 2)),
                        alpha,
                    ]
                }
                Model::YCbCr => {
                    let (yy, cb, cr) = (
                        f32::from(get(x, y, 0)),
                        f32::from(get(x, y, 1)) - 128.0,
                        f32::from(get(x, y, 2)) - 128.0,
                    );
                    let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
                    [
                        clamp(yy + 1.402 * cr),
                        clamp(yy - 0.344_136 * cb - 0.714_136 * cr),
                        clamp(yy + 1.772 * cb),
                        255,
                    ]
                }
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    Ok(Picture {
        width: ow as u32,
        height: oh as u32,
        rgba,
        original: (w as u32, h as u32),
    })
}

/// The smallest whole step that brings the picture within the display limits.
fn reduction(w: u64, h: u64) -> usize {
    let mut step = 1u64;
    while w.div_ceil(step) > u64::from(SHOWN_SIDE)
        || h.div_ceil(step) > u64::from(SHOWN_SIDE)
        || w.div_ceil(step) * h.div_ceil(step) > SHOWN_PIXELS
    {
        step += 1;
    }
    step as usize
}

/// Samples brought to 8 bits, or kept packed when under 8 bits.
struct Samples {
    data: Vec<u8>,
    bits: u8,
}

impl Samples {
    fn new(result: DecodingResult, bits: u8) -> Self {
        fn spread<T: Copy>(v: &[T], f: impl Fn(T) -> f64) -> Vec<u8> {
            let (lo, hi) = v
                .iter()
                .map(|&x| f(x))
                .filter(|x| x.is_finite())
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
                    (lo.min(x), hi.max(x))
                });
            let range = if hi > lo { hi - lo } else { 1.0 };
            v.iter()
                .map(|&x| {
                    let x = f(x);
                    if x.is_finite() {
                        ((x - lo) / range * 255.0).round() as u8
                    } else {
                        0
                    }
                })
                .collect()
        }
        let data = match result {
            DecodingResult::U8(v) => {
                return Self {
                    data: v,
                    bits: bits.min(8),
                };
            }
            DecodingResult::U16(v) => v.iter().map(|&x| (x >> 8) as u8).collect(),
            DecodingResult::U32(v) => v.iter().map(|&x| (x >> 24) as u8).collect(),
            DecodingResult::U64(v) => v.iter().map(|&x| (x >> 56) as u8).collect(),
            DecodingResult::I8(v) => v.iter().map(|&x| (i16::from(x) + 128) as u8).collect(),
            DecodingResult::I16(v) => v
                .iter()
                .map(|&x| ((i32::from(x) + 32768) >> 8) as u8)
                .collect(),
            DecodingResult::I32(v) => spread(&v, f64::from),
            DecodingResult::I64(v) => spread(&v, |x| x as f64),
            DecodingResult::F16(v) => spread(&v, |x| f64::from(f32::from(x))),
            DecodingResult::F32(v) => spread(&v, f64::from),
            DecodingResult::F64(v) => spread(&v, |x| x),
        };
        Self { data, bits: 8 }
    }
}

mod palette;

#[cfg(test)]
mod tests;
