//! Minimal RGBA8 PNG writer: filter zero, zlib via the existing flate2 dependency.
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

pub(crate) fn encode(width: u16, height: u16, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let stride = usize::from(width) * 4;
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > super::MAX_PIXELS
        || rgba.len() != stride * usize::from(height)
    {
        return Err("The picture has an invalid raster size".into());
    }
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
    for row in rgba.chunks_exact(stride) {
        zlib.write_all(&[0])
            .and_then(|()| zlib.write_all(row))
            .map_err(|e| format!("The picture could not be encoded as PNG: {e}"))?;
    }
    let compressed = zlib
        .finish()
        .map_err(|e| format!("The picture could not be encoded as PNG: {e}"))?;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&u32::from(width).to_be_bytes());
    header.extend_from_slice(&u32::from(height).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    for bytes in compressed.chunks(64 * 1024) {
        chunk(&mut out, b"IDAT", bytes);
    }
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], bytes: &[u8]) {
    // All callers use fixed headers or chunks bounded to 64 KiB.
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(bytes);
    let mut crc = !0u32;
    for &byte in kind.iter().chain(bytes) {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 255) as usize];
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};
