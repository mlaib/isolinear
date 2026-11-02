use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    std::fs::read(dir.join(name)).unwrap()
}

fn pixel(p: &Picture, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * p.width + x) * 4) as usize;
    p.rgba[i..i + 4].try_into().unwrap()
}

/// Close enough for lossy compression and colour conversions.
fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 24)
}

#[test]
fn every_layout_decodes_to_the_same_picture() {
    // ImageMagick fixtures: a red-to-blue gradient from top to bottom, 64 × 48.
    for name in [
        "rgb-lzw.tif",
        "rgb16-deflate.tif",
        "jpeg.tif",
        "tiled.tif",
        "planar8.tif",
        "cmyk.tif",
    ] {
        let p = decode(&fixture(&format!("tiff/{name}"))).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((p.width, p.height), (64, 48), "{name}");
        assert!(
            near(pixel(&p, 10, 0), [255, 0, 0, 255]),
            "{name} top {:?}",
            pixel(&p, 10, 0)
        );
        assert!(
            near(pixel(&p, 10, 47), [0, 0, 255, 255]),
            "{name} bottom {:?}",
            pixel(&p, 10, 47)
        );
    }
    let p = decode(&fixture("tiff/rgba.tif")).unwrap();
    assert!(
        near(pixel(&p, 0, 0), [255, 0, 0, 128]),
        "{:?}",
        pixel(&p, 0, 0)
    );
}

#[test]
fn grey_fax_and_palette() {
    let p = decode(&fixture("tiff/gray.tif")).unwrap();
    assert!(near(pixel(&p, 0, 0), [255, 255, 255, 255]));
    assert!(near(pixel(&p, 0, 47), [0, 0, 0, 255]));
    // A checkerboard in Group 4, black is zero or white is zero: the same picture.
    let black = decode(&fixture("tiff/fax-g4.tif")).unwrap();
    let white = decode(&fixture("tiff/fax-white.tif")).unwrap();
    assert_eq!(black.rgba, white.rgba);
    assert!(black.rgba.chunks(4).any(|p| p[0] == 0) && black.rgba.chunks(4).any(|p| p[0] == 255));
    // Palette pictures: the gradient through 200 colours (LZW), and halves of green and white
    // through four colours at one bit (PackBits).
    let p = decode(&fixture("tiff/palette8.tif")).unwrap();
    assert_eq!((p.width, p.height), (64, 48));
    assert!(
        near(pixel(&p, 10, 0), [255, 0, 0, 255]),
        "{:?}",
        pixel(&p, 10, 0)
    );
    assert!(
        near(pixel(&p, 10, 47), [0, 0, 255, 255]),
        "{:?}",
        pixel(&p, 10, 47)
    );
    let p = decode(&fixture("tiff/palette.tif")).unwrap();
    assert_eq!(pixel(&p, 5, 5), [0, 255, 0, 255]);
    assert_eq!(pixel(&p, 60, 5), [255, 255, 255, 255]);
    for name in ["palette4-deflate.tif", "palette8-none.tif"] {
        let p = decode(&fixture(&format!("tiff/{name}"))).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            near(pixel(&p, 10, 0), [255, 0, 0, 255]),
            "{name} {:?}",
            pixel(&p, 10, 0)
        );
        assert!(
            near(pixel(&p, 10, 47), [0, 0, 255, 255]),
            "{name} {:?}",
            pixel(&p, 10, 47)
        );
    }
}

#[test]
fn png_output_and_geotiff_fixtures() {
    let png = to_png(&fixture("tiff/rgb-lzw.tif")).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    for name in ["plain.tif", "utm.tif", "geographic-bigtiff.tif"] {
        let p = decode(&fixture(&format!("standards/{name}")))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
    }
}

#[test]
fn large_pictures_are_reduced() {
    assert_eq!(reduction(64, 48), 1);
    assert_eq!(reduction(8192, 4000), 1);
    assert_eq!(reduction(8193, 10), 2);
    assert_eq!(reduction(100_000, 10), 13);
    assert_eq!(reduction(100_000, 100_000), 16);
    let step = reduction(20_000, 20_000);
    assert!(20_000u64.div_ceil(step as u64).pow(2) <= SHOWN_PIXELS);
}

#[test]
fn damaged_and_random_inputs_never_panic() {
    assert!(decode(b"").is_err());
    assert!(decode(b"II*\0\x08\0\0\0").is_err());
    let bases: Vec<Vec<u8>> = [
        "rgb-lzw.tif",
        "fax-g4.tif",
        "palette.tif",
        "jpeg.tif",
        "tiled.tif",
    ]
    .iter()
    .map(|n| fixture(&format!("tiff/{n}")))
    .collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..20_000 {
        let mut b = bases[i % bases.len()].clone();
        for _ in 0..(next() % 8 + 1) {
            let at = (next() % b.len() as u64) as usize;
            b[at] = next() as u8;
        }
        if next() % 5 == 0 {
            b.truncate((next() % b.len() as u64) as usize);
        }
        if let Ok(p) = decode(&b) {
            assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize, "case {i}");
        }
    }
}
