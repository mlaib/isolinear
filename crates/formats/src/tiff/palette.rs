//! Palette (indexed colour) TIFF pictures, which the `tiff` decoder refuses: its tags are read
//! through it, the strips are decompressed here (none, PackBits, LZW, Deflate), each into a
//! buffer of exactly the size the picture needs, and the indices go through the colour map.

use super::{Picture, reduction};
use std::io::{Cursor, Read};
use tiff::decoder::Decoder;
use tiff::tags::Tag;

fn err(e: tiff::TiffError) -> String {
    format!("The TIFF picture cannot be decoded: {e}")
}

/// Whether the first image of the file is an indexed-colour picture.
pub(super) fn is_palette(bytes: &[u8]) -> bool {
    Decoder::new(Cursor::new(bytes)).ok().and_then(|mut d| {
        d.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)
            .ok()
            .flatten()
    }) == Some(3)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Picture, String> {
    let mut d = Decoder::new(Cursor::new(bytes)).map_err(err)?;
    let (w, h) = d.dimensions().map_err(err)?;
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return Err("The TIFF picture is empty".into());
    }
    let tag = |d: &mut Decoder<Cursor<&[u8]>>, t: Tag| d.find_tag_unsigned::<u32>(t).map_err(err);
    let bits = tag(&mut d, Tag::BitsPerSample)?.unwrap_or(1) as usize;
    if !matches!(bits, 1 | 2 | 4 | 8) {
        return Err(format!(
            "Palette TIFF pictures of {bits} bits are not shown"
        ));
    }
    if tag(&mut d, Tag::SamplesPerPixel)?.unwrap_or(1) != 1 {
        return Err("Palette TIFF pictures with extra samples are not shown".into());
    }
    if tag(&mut d, Tag::Predictor)?.unwrap_or(1) != 1 {
        return Err("Palette TIFF pictures with a predictor are not shown".into());
    }
    if tag(&mut d, Tag::TileWidth)?.is_some() {
        return Err("Tiled palette TIFF pictures are not shown".into());
    }
    let reversed = tag(&mut d, Tag::FillOrder)? == Some(2);
    let compression = tag(&mut d, Tag::Compression)?.unwrap_or(1);
    let map = d
        .find_tag_unsigned_vec::<u16>(Tag::ColorMap)
        .map_err(err)?
        .ok_or("The TIFF palette is missing")?;
    let entries = 1usize << bits;
    if map.len() < 3 * entries {
        return Err("The TIFF palette is shorter than its bit depth needs".into());
    }
    let offsets = d
        .find_tag_unsigned_vec::<u64>(Tag::StripOffsets)
        .map_err(err)?
        .ok_or("The TIFF picture has no strips")?;
    let counts = d
        .find_tag_unsigned_vec::<u64>(Tag::StripByteCounts)
        .map_err(err)?
        .ok_or("The TIFF picture has no strip sizes")?;
    let per_strip = (tag(&mut d, Tag::RowsPerStrip)?.unwrap_or(h as u32) as usize).clamp(1, h);
    let row_bytes = (w * bits).div_ceil(8);
    let total = row_bytes
        .checked_mul(h)
        .filter(|&n| n <= super::DECODE_MAX)
        .ok_or("The TIFF picture is too large")?;

    // Indices, row after row, each row padded to a byte as the strips store them.
    let mut data = vec![0u8; total];
    for (s, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
        let first = s * per_strip;
        if first >= h {
            break;
        }
        let rows = per_strip.min(h - first);
        let out = &mut data[first * row_bytes..(first + rows) * row_bytes];
        let start = usize::try_from(offset).map_err(|_| "strip offset overflow")?;
        let end = start
            .checked_add(usize::try_from(count).map_err(|_| "strip size overflow")?)
            .filter(|&e| e <= bytes.len())
            .ok_or("A strip lies past the end of the file")?;
        let strip = &bytes[start..end];
        match compression {
            1 => {
                let n = strip.len().min(out.len());
                out[..n].copy_from_slice(&strip[..n]);
            }
            32773 => packbits(strip, out),
            5 => lzw(strip, out),
            8 | 32946 => {
                let mut z = flate2::read::ZlibDecoder::new(strip);
                let mut filled = 0;
                while filled < out.len() {
                    match z.read(&mut out[filled..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => filled += n,
                    }
                }
            }
            c => return Err(format!("Palette TIFF compression {c} is not supported")),
        }
    }
    if reversed {
        for b in &mut data {
            *b = b.reverse_bits();
        }
    }

    let step = reduction(w as u64, h as u64);
    let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        for ox in 0..ow {
            let (x, y) = (ox * step, oy * step);
            let bit = x * bits;
            let byte = data[y * row_bytes + bit / 8];
            let i = usize::from((byte >> (8 - bits - bit % 8)) & ((1u16 << bits) - 1) as u8);
            rgba.extend_from_slice(&[
                (map[i] >> 8) as u8,
                (map[entries + i] >> 8) as u8,
                (map[2 * entries + i] >> 8) as u8,
                255,
            ]);
        }
    }
    Ok(Picture {
        width: ow as u32,
        height: oh as u32,
        rgba,
        original: (w as u32, h as u32),
    })
}

/// PackBits runs into `out`, stopping when it is full or the data ends.
fn packbits(mut data: &[u8], out: &mut [u8]) {
    let mut o = 0;
    while let Some((&n, rest)) = data.split_first() {
        data = rest;
        if o >= out.len() {
            break;
        }
        let n = n as i8;
        if n >= 0 {
            let len = (n as usize + 1).min(data.len()).min(out.len() - o);
            out[o..o + len].copy_from_slice(&data[..len]);
            o += len;
            data = &data[(n as usize + 1).min(data.len())..];
        } else if n != -128 {
            let Some((&v, rest)) = data.split_first() else {
                break;
            };
            data = rest;
            let len = ((1 - n as isize) as usize).min(out.len() - o);
            out[o..o + len].fill(v);
            o += len;
        }
    }
}

/// TIFF's LZW (most significant bit first, early code size change) into `out`.
fn lzw(data: &[u8], out: &mut [u8]) {
    let mut decoder = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let (mut i, mut o) = (0, 0);
    while o < out.len() && i <= data.len() {
        let r = decoder.decode_bytes(&data[i..], &mut out[o..]);
        i += r.consumed_in;
        o += r.consumed_out;
        match r.status {
            Ok(weezl::LzwStatus::Ok) if r.consumed_in + r.consumed_out > 0 => {}
            _ => break,
        }
    }
}
