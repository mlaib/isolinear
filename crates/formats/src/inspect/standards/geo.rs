//! Shared pieces of the geographic handlers: bounds, distances on the sphere, the common
//! coordinate reference systems, well-known binary (WKB) geometry, and the map with its limits.
use crate::inspect::{Feature, Geometry, Visual};

/// Features kept for the map; the rest are counted in a note.
pub const MAX_FEATURES: usize = 200_000;
/// Coordinates kept for the map, all features together.
pub const MAX_POINTS: usize = 4_000_000;
/// Nesting of geometry collections and multi-geometries.
pub const MAX_NESTING: usize = 32;
/// Mean Earth radius (IUGG), metres.
pub const EARTH_RADIUS: f64 = 6_371_008.8;

/// Smallest rectangle around coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f64; 2],
    pub max: [f64; 2],
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            min: [f64::INFINITY; 2],
            max: [f64::NEG_INFINITY; 2],
        }
    }
}
impl Bounds {
    pub fn add(&mut self, p: [f64; 2]) {
        if p[0].is_finite() && p[1].is_finite() {
            self.min = [self.min[0].min(p[0]), self.min[1].min(p[1])];
            self.max = [self.max[0].max(p[0]), self.max[1].max(p[1])];
        }
    }
    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0]
    }
    /// Whether every coordinate could be a longitude/latitude.
    pub fn fits_lonlat(&self) -> bool {
        !self.is_empty()
            && self.min[0] >= -180.0
            && self.max[0] <= 360.0
            && self.min[1] >= -90.0
            && self.max[1] <= 90.0
    }
    /// "min x, min y — max x, max y".
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        format!(
            "{}, {} — {}, {}",
            num(self.min[0]),
            num(self.min[1]),
            num(self.max[0]),
            num(self.max[1])
        )
    }
    /// For longitude/latitude bounds: "west …, south … — east …, north …".
    pub fn describe_lonlat(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        format!(
            "west {}, south {}, east {}, north {}",
            num(self.min[0]),
            num(self.min[1]),
            num(self.max[0]),
            num(self.max[1])
        )
    }
}

/// A number with up to 7 decimals (about 1 cm in degrees), trailing zeros removed.
pub fn num(x: f64) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    let s = format!("{x:.7}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Great-circle distance in metres between two longitude/latitude points (haversine formula).
pub fn haversine(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (la1, la2) = (a[1].to_radians(), b[1].to_radians());
    let dlat = la2 - la1;
    let dlon = (b[0] - a[0]).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS * h.sqrt().min(1.0).asin()
}

/// Points, lines and polygon rings of one feature, gathered from any geometry type.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parts {
    pub points: Vec<[f64; 2]>,
    pub lines: Vec<Vec<[f64; 2]>>,
    pub rings: Vec<Vec<[f64; 2]>>,
}
impl Parts {
    pub fn coordinates(&self) -> usize {
        self.points.len()
            + self.lines.iter().map(Vec::len).sum::<usize>()
            + self.rings.iter().map(Vec::len).sum::<usize>()
    }
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.lines.is_empty() && self.rings.is_empty()
    }
    pub fn bounds(&self, b: &mut Bounds) {
        for p in self
            .points
            .iter()
            .chain(self.lines.iter().flatten())
            .chain(self.rings.iter().flatten())
        {
            b.add(*p);
        }
    }
    pub fn append(&mut self, other: Parts) {
        self.points.extend(other.points);
        self.lines.extend(other.lines);
        self.rings.extend(other.rings);
    }
}

/// The features of a map, within [`MAX_FEATURES`] and [`MAX_POINTS`].
#[derive(Default)]
pub struct Map {
    pub features: Vec<Feature>,
    points: usize,
    /// Features (or parts) left out because of the limits.
    pub left_out: usize,
}
impl Map {
    pub fn add(&mut self, name: &str, parts: Parts) {
        let n = parts.coordinates();
        if n == 0 {
            return;
        }
        if self.features.len() >= MAX_FEATURES || self.points + n > MAX_POINTS {
            self.left_out += 1;
            return;
        }
        self.points += n;
        if !parts.points.is_empty() {
            self.features.push(Feature {
                name: name.into(),
                geometry: Geometry::Points(parts.points),
            });
        }
        if !parts.lines.is_empty() {
            self.features.push(Feature {
                name: name.into(),
                geometry: Geometry::Lines(parts.lines),
            });
        }
        if !parts.rings.is_empty() {
            self.features.push(Feature {
                name: name.into(),
                geometry: Geometry::Polygons(parts.rings),
            });
        }
    }
    /// The map visual, and a note when features were left out.
    pub fn finish(self, geographic: bool, visuals: &mut Vec<Visual>, notes: &mut Vec<String>) {
        if self.left_out > 0 {
            notes.push(format!(
                "The map shows the first {} features ({} coordinates); {} more are not drawn",
                self.features.len(),
                self.points,
                self.left_out
            ));
        }
        if !self.features.is_empty() {
            visuals.push(Visual::Map {
                features: self.features,
                geographic,
            });
        }
    }
}

/// Counts of geometry types, in order of first appearance: "Point × 3, LineString × 1".
#[derive(Default)]
pub struct TypeCounts(Vec<(String, u64)>);
impl TypeCounts {
    pub fn add(&mut self, name: &str) {
        if let Some((_, c)) = self.0.iter_mut().find(|(n, _)| n == name) {
            *c += 1;
        } else if self.0.len() < 64 {
            self.0.push((name.into(), 1));
        }
    }
    pub fn describe(&self) -> String {
        self.0
            .iter()
            .map(|(n, c)| format!("{n} × {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ----- coordinate reference systems -----

/// Names of common EPSG coordinate reference systems.
pub fn epsg_name(code: u32) -> Option<String> {
    let fixed = match code {
        4326 => "WGS 84",
        4979 => "WGS 84 (3D)",
        4978 => "WGS 84 (geocentric)",
        4269 => "NAD83",
        4267 => "NAD27",
        4258 => "ETRS89",
        4230 => "ED50",
        4283 => "GDA94",
        7844 => "GDA2020",
        4612 => "JGD2000",
        6668 => "JGD2011",
        4490 => "China Geodetic Coordinate System 2000",
        4674 => "SIRGAS 2000",
        4148 => "Hartebeesthoek94",
        4277 => "OSGB36",
        4181 => "Luxembourg 1930",
        4313 => "Belge 1972",
        4314 => "DHDN",
        4149 => "CH1903",
        4150 => "CH1903+",
        3857 | 900913 | 3785 | 102100 => "WGS 84 / Pseudo-Mercator",
        3395 => "WGS 84 / World Mercator",
        4087 => "WGS 84 / World Equidistant Cylindrical",
        3035 => "ETRS89-extended / LAEA Europe",
        3034 => "ETRS89-extended / LCC Europe",
        2154 => "RGF93 v1 / Lambert-93",
        27700 => "OSGB36 / British National Grid",
        2056 => "CH1903+ / LV95",
        21781 => "CH1903 / LV03",
        31370 => "Belge 1972 / Belgian Lambert 72",
        3812 => "ETRS89 / Belgian Lambert 2008",
        2169 => "Luxembourg 1930 / Gauss",
        28992 => "Amersfoort / RD New",
        3006 => "SWEREF99 TM",
        25833 => "ETRS89 / UTM zone 33N",
        31467 => "DHDN / 3-degree Gauss-Kruger zone 3",
        5070 => "NAD83 / Conus Albers",
        3577 => "GDA94 / Australian Albers",
        2193 => "NZGD2000 / New Zealand Transverse Mercator 2000",
        3031 => "WGS 84 / Antarctic Polar Stereographic",
        3413 => "WGS 84 / NSIDC Sea Ice Polar Stereographic North",
        _ => "",
    };
    if !fixed.is_empty() {
        return Some(fixed.into());
    }
    let zone = |base: u32| code - base;
    match code {
        32601..=32660 => Some(format!("WGS 84 / UTM zone {}N", zone(32600))),
        32701..=32760 => Some(format!("WGS 84 / UTM zone {}S", zone(32700))),
        25828..=25838 => Some(format!("ETRS89 / UTM zone {}N", zone(25800))),
        26901..=26923 => Some(format!("NAD83 / UTM zone {}N", zone(26900))),
        26701..=26722 => Some(format!("NAD27 / UTM zone {}N", zone(26700))),
        23028..=23038 => Some(format!("ED50 / UTM zone {}N", zone(23000))),
        _ => None,
    }
}

/// Whether an EPSG code is a geographic (longitude/latitude) system.
pub fn epsg_geographic(code: u32) -> bool {
    matches!(code, 4001..=4999 | 7844 | 6668) && code != 4978
}

/// "EPSG:32632 — WGS 84 / UTM zone 32N".
pub fn epsg_label(code: u32) -> String {
    match epsg_name(code) {
        Some(n) => format!("EPSG:{code} — {n}"),
        None => format!("EPSG:{code}"),
    }
}

/// Longitude/latitude of projected coordinates, for the projections that need no parameters
/// beyond the EPSG code: Web Mercator and UTM zones (WGS 84, ETRS89, NAD83).
pub fn to_lonlat(code: u32, x: f64, y: f64) -> Option<[f64; 2]> {
    if epsg_geographic(code) {
        return Some([x, y]);
    }
    let utm = |zone: u32, south: bool| utm_inverse(zone, south, x, y);
    match code {
        3857 | 900913 | 3785 | 102100 => {
            let lon = (x / 6_378_137.0).to_degrees();
            let lat =
                (2.0 * (y / 6_378_137.0).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
            Some([lon, lat])
        }
        32601..=32660 => utm(code - 32600, false),
        32701..=32760 => utm(code - 32700, true),
        25828..=25838 => utm(code - 25800, false),
        26901..=26923 => utm(code - 26900, false),
        _ => None,
    }
    .filter(|p| p[0].is_finite() && p[1].is_finite())
}

/// Inverse transverse Mercator for a UTM zone on the WGS 84 ellipsoid (GRS 80 differs by
/// less than a millimetre), after Snyder, "Map Projections — A Working Manual", p. 63.
fn utm_inverse(zone: u32, south: bool, x: f64, y: f64) -> Option<[f64; 2]> {
    let a: f64 = 6_378_137.0;
    let f: f64 = 1.0 / 298.257_223_563;
    let k0 = 0.9996;
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);
    let x = x - 500_000.0;
    let y = if south { y - 10_000_000.0 } else { y };
    let m = y / k0;
    let mu = m / (a * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0));
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();
    let (s, c) = phi1.sin_cos();
    if c.abs() < 1e-12 {
        return None;
    }
    let t = (s / c).powi(2);
    let cc = ep2 * c * c;
    let n = a / (1.0 - e2 * s * s).sqrt();
    let r = a * (1.0 - e2) / (1.0 - e2 * s * s).powf(1.5);
    let d = x / (n * k0);
    let lat = phi1
        - (n * s / c / r)
            * (d * d / 2.0
                - (5.0 + 3.0 * t + 10.0 * cc - 4.0 * cc * cc - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t + 298.0 * cc + 45.0 * t * t - 252.0 * ep2 - 3.0 * cc * cc)
                    * d.powi(6)
                    / 720.0);
    let lon0 = (f64::from(zone) * 6.0 - 183.0).to_radians();
    let lon = lon0
        + (d - (1.0 + 2.0 * t + cc) * d.powi(3) / 6.0
            + (5.0 - 2.0 * cc + 28.0 * t - 3.0 * cc * cc + 8.0 * ep2 + 24.0 * t * t) * d.powi(5)
                / 120.0)
            / c;
    Some([lon.to_degrees(), lat.to_degrees()])
}

/// What a WKT coordinate reference system says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wkt {
    pub name: String,
    /// GEOGCS / GEOGCRS / GEOGRAPHICCRS (longitude/latitude).
    pub geographic: bool,
    pub projected: bool,
    /// The system's own EPSG code (the last authority at the top level).
    pub epsg: Option<u32>,
}

/// Read the name, kind and EPSG code of a WKT (version 1 or 2) definition, without evaluating it.
pub fn wkt(text: &str) -> Option<Wkt> {
    let text = text.trim();
    let open = text.find(['[', '('])?;
    let keyword = text[..open].trim().to_ascii_uppercase();
    let rest = &text[open + 1..];
    let name = rest
        .trim_start()
        .strip_prefix('"')
        .and_then(|r| r.split('"').next())
        .unwrap_or("")
        .to_owned();
    // Authority or ID at depth 1 (the system's own, after its parts).
    let mut epsg = None;
    let mut depth = 0usize;
    let b = text.as_bytes();
    let mut i = open;
    let mut in_str = false;
    while i < b.len() {
        match b[i] {
            b'"' => in_str = !in_str,
            b'[' | b'(' if !in_str => depth += 1,
            b']' | b')' if !in_str => depth = depth.saturating_sub(1),
            b'A' | b'I' if !in_str && depth == 1 => {
                let tail = &text[i..];
                let upper = tail.get(..10).unwrap_or(tail).to_ascii_uppercase();
                if upper.starts_with("AUTHORITY[") || upper.starts_with("ID[") {
                    let inner = tail.split(']').next().unwrap_or("");
                    let mut parts = inner
                        .split_once('[')
                        .map(|(_, p)| p)
                        .unwrap_or("")
                        .split(',');
                    let org = parts.next().unwrap_or("").trim().trim_matches('"');
                    let code = parts.next().unwrap_or("").trim().trim_matches('"');
                    if org.eq_ignore_ascii_case("EPSG") {
                        epsg = code.parse().ok();
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    Some(Wkt {
        name,
        geographic: matches!(
            keyword.as_str(),
            "GEOGCS" | "GEOGCRS" | "GEOGRAPHICCRS" | "GEODCRS" | "GEODETICCRS"
        ) && !text.to_ascii_uppercase().contains("CARTESIAN"),
        projected: matches!(keyword.as_str(), "PROJCS" | "PROJCRS" | "PROJECTEDCRS"),
        epsg,
    })
}

// ----- well-known binary -----

struct Wkb<'a> {
    b: &'a [u8],
    at: usize,
}
impl Wkb<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.at).ok_or("The geometry is cut short")?;
        self.at += 1;
        Ok(v)
    }
    fn u32(&mut self, le: bool) -> Result<u32, String> {
        let s: [u8; 4] = self
            .b
            .get(self.at..self.at + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or("The geometry is cut short")?;
        self.at += 4;
        Ok(if le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    }
    fn f64(&mut self, le: bool) -> Result<f64, String> {
        let s: [u8; 8] = self
            .b
            .get(self.at..self.at + 8)
            .and_then(|s| s.try_into().ok())
            .ok_or("The geometry is cut short")?;
        self.at += 8;
        Ok(if le {
            f64::from_le_bytes(s)
        } else {
            f64::from_be_bytes(s)
        })
    }
    /// A count of items of at least `size` bytes each, checked against what is left.
    fn count(&mut self, le: bool, size: usize) -> Result<usize, String> {
        let n = self.u32(le)? as usize;
        if n.saturating_mul(size) > self.b.len() - self.at {
            return Err("A geometry declares more coordinates than it holds".into());
        }
        Ok(n)
    }
    fn coords(&mut self, le: bool, dims: usize) -> Result<Vec<[f64; 2]>, String> {
        let n = self.count(le, dims * 8)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let p = [self.f64(le)?, self.f64(le)?];
            for _ in 2..dims {
                self.f64(le)?;
            }
            out.push(p);
        }
        Ok(out)
    }
    fn geometry(&mut self, depth: usize, out: &mut Parts) -> Result<&'static str, String> {
        if depth > MAX_NESTING {
            return Err("Geometries nest too deeply".into());
        }
        let le = match self.u8()? {
            0 => false,
            1 => true,
            _ => return Err("Unknown byte order in a geometry".into()),
        };
        let raw = self.u32(le)?;
        // Extended WKB (PostGIS) flags, then ISO WKB's thousands for Z, M and ZM.
        let (mut z, mut m) = (raw & 0x8000_0000 != 0, raw & 0x4000_0000 != 0);
        if raw & 0x2000_0000 != 0 {
            self.u32(le)?; // SRID
        }
        let code = raw & 0x0fff_ffff;
        match code / 1000 {
            1 => z = true,
            2 => m = true,
            3 => (z, m) = (true, true),
            _ => {}
        }
        let dims = 2 + usize::from(z) + usize::from(m);
        Ok(match code % 1000 {
            1 => {
                let p = [self.f64(le)?, self.f64(le)?];
                for _ in 2..dims {
                    self.f64(le)?;
                }
                // An empty point is written as NaN coordinates.
                if p[0].is_finite() && p[1].is_finite() {
                    out.points.push(p);
                }
                "Point"
            }
            2 => {
                out.lines.push(self.coords(le, dims)?);
                "LineString"
            }
            3 => {
                let rings = self.count(le, 4)?;
                for _ in 0..rings {
                    out.rings.push(self.coords(le, dims)?);
                }
                "Polygon"
            }
            kind @ 4..=7 => {
                let n = self.count(le, 5)?;
                for _ in 0..n {
                    self.geometry(depth + 1, out)?;
                }
                match kind {
                    4 => "MultiPoint",
                    5 => "MultiLineString",
                    6 => "MultiPolygon",
                    _ => "GeometryCollection",
                }
            }
            other => {
                return Err(format!(
                    "Geometry type {other} (curves, surfaces or TIN) is not drawn"
                ));
            }
        })
    }
}

/// Decode a well-known binary geometry (ISO, OGC or PostGIS extended): its type name, its
/// parts, and the bytes it used.
pub fn wkb(b: &[u8]) -> Result<(&'static str, Parts, usize), String> {
    let mut r = Wkb { b, at: 0 };
    let mut parts = Parts::default();
    let kind = r.geometry(0, &mut parts)?;
    Ok((kind, parts, r.at))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distances_projections_and_wkt() {
        // Paris to London, about 343.5 km.
        let d = haversine([2.3522, 48.8566], [-0.1276, 51.5072]);
        assert!((d - 343_500.0).abs() < 1_000.0, "{d}");
        let p = to_lonlat(3857, 1_113_194.9, 0.0).unwrap();
        assert!((p[0] - 10.0).abs() < 1e-6 && p[1].abs() < 1e-9);
        // UTM 32N: 500000 E, 5540000 N is 9°E, about 50.01°N.
        let p = to_lonlat(32632, 500_000.0, 5_540_000.0).unwrap();
        assert!((p[0] - 9.0).abs() < 1e-9, "{p:?}");
        assert!((p[1] - 50.0127).abs() < 1e-3, "{p:?}");
        let w = wkt(r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4326"]]"#).unwrap();
        assert_eq!(
            (w.name.as_str(), w.geographic, w.epsg),
            ("WGS 84", true, Some(4326))
        );
        let w = wkt(r#"PROJCRS["WGS 84 / UTM zone 32N",BASEGEOGCRS["WGS 84"],ID["EPSG",32632]]"#)
            .unwrap();
        assert!(w.projected && !w.geographic);
        assert_eq!(w.epsg, Some(32632));
        assert_eq!(epsg_label(32733), "EPSG:32733 — WGS 84 / UTM zone 33S");
        assert_eq!(num(1.50), "1.5");
        assert_eq!(num(-0.00000001), "0");
    }
    #[test]
    fn well_known_binary() {
        // MultiLineString (LE) with one LineString (BE) of two points, ISO Z.
        let mut b = vec![1u8];
        b.extend(1005u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.push(0);
        b.extend(1002u32.to_be_bytes());
        b.extend(2u32.to_be_bytes());
        for v in [1.0f64, 2.0, 3.0, 4.0, 5.0, 6.0] {
            b.extend(v.to_be_bytes());
        }
        let (kind, parts, used) = wkb(&b).unwrap();
        assert_eq!(kind, "MultiLineString");
        assert_eq!(parts.lines, vec![vec![[1.0, 2.0], [4.0, 5.0]]]);
        assert_eq!(used, b.len());
        assert!(wkb(&b[..b.len() - 1]).is_err());
        // A huge declared count is refused before allocating.
        let mut bad = vec![1u8];
        bad.extend(2u32.to_le_bytes());
        bad.extend(u32::MAX.to_le_bytes());
        assert!(wkb(&bad).is_err());
    }
}
