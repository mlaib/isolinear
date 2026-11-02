//! GPX 1.0 and 1.1 (GPS exchange format): metadata, waypoints, routes and tracks with
//! distances (haversine, on a sphere of the mean Earth radius), raw elevation gain and loss,
//! times and durations, a map, and the elevation profile of the first track.
use super::geo::{Bounds, Map, Parts, haversine, num};
use crate::inspect::{
    Channel, Inspection, MAX_ROWS, Row, Stage, State, Table, Visual,
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

/// Samples of the elevation profile at most.
const PROFILE_SAMPLES: f64 = 4000.0;

pub static GPX: XmlSpec = XmlSpec {
    family: "GPX",
    kind: "GPX — GPS waypoints, routes and tracks",
    matches: |root, ns| {
        root == "gpx" && (ns.is_empty() || ns.starts_with("http://www.topografix.com/GPX/1/"))
    },
    version: |doc| doc.attr(0, "version").map(|v| format!("GPX {v}")),
    facts: &[
        F("Document", "Creator", "@creator"),
        F("Document", "Name", "metadata/name | name"),
        F("Document", "Description", "metadata/desc | desc"),
        F("Document", "Author", "metadata/author/name | author"),
        F("Document", "Time", "metadata/time | time"),
        F("Document", "Keywords", "metadata/keywords | keywords"),
        F("Content", "Waypoints", "count:wpt"),
        F("Content", "Routes", "count:rte"),
        F("Content", "Tracks", "count:trk"),
    ],
    tables: &[T {
        name: "Waypoints",
        rows: "wpt",
        columns: &[
            ("Name", "name"),
            ("Latitude", "@lat"),
            ("Longitude", "@lon"),
            ("Elevation (m)", "ele"),
            ("Time", "time"),
            ("Symbol", "sym"),
            ("Description", "desc | cmt"),
        ],
    }],
    custom: Some(custom),
};

/// Seconds since 1970 of an ISO 8601 / XML Schema date-time ("2026-05-01T08:00:00.5Z",
/// "2026-05-01T10:00:00+02:00"); a time without zone is read as UTC.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 24 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.bytes().take_while(u8::is_ascii_digit).count();
        frac = format!("0.{}", &r[..digits]).parse().unwrap_or(0.0);
        rest = &r[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        z if z.len() == 6 && (z.starts_with('+') || z.starts_with('-')) => {
            let hh: i64 = z.get(1..3)?.parse().ok()?;
            let mm: i64 = z.get(4..6)?.parse().ok()?;
            let o = hh * 3600 + mm * 60;
            if z.starts_with('-') { -o } else { o }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset) as f64 + frac)
}

/// "1 h 02 min 05 s", "12 min 30 s", "45 s".
pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return String::new();
    }
    let t = seconds.round() as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    match (h, m) {
        (0, 0) => format!("{s} s"),
        (0, _) => format!("{m} min {s:02} s"),
        _ => format!("{h} h {m:02} min {s:02} s"),
    }
}

/// One point of a route or track.
struct Point {
    at: [f64; 2],
    ele: Option<f64>,
    time: Option<f64>,
}

fn point(doc: &Doc, e: u32, problems: &mut Vec<String>) -> Option<Point> {
    let lat: f64 = doc.attr(e, "lat")?.trim().parse().ok()?;
    let lon: f64 = doc.attr(e, "lon")?.trim().parse().ok()?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        problems.push(format!("a point is outside the valid range ({lat}, {lon})"));
    }
    let child = |name: &str| {
        doc.get(e)
            .children
            .iter()
            .find(|&&c| doc.get(c).name == name)
            .map(|&c| doc.text(c))
    };
    Some(Point {
        at: [lon, lat],
        ele: child("ele").and_then(|t| t.trim().parse().ok()),
        time: child("time").and_then(|t| parse_time(&t)),
    })
}

/// Totals of a run of points: distance, raw elevation gain and loss, first and last time.
#[derive(Default)]
struct Totals {
    points: usize,
    distance: f64,
    gain: f64,
    loss: f64,
    start: Option<f64>,
    end: Option<f64>,
    out_of_order: bool,
}
impl Totals {
    fn add(&mut self, pts: &[Point]) {
        self.points += pts.len();
        let mut last_ele: Option<f64> = None;
        let mut last_time: Option<f64> = None;
        for (i, p) in pts.iter().enumerate() {
            if i > 0 {
                self.distance += haversine(pts[i - 1].at, p.at);
            }
            if let Some(e) = p.ele {
                if let Some(l) = last_ele {
                    let d = e - l;
                    if d > 0.0 {
                        self.gain += d;
                    } else {
                        self.loss -= d;
                    }
                }
                last_ele = Some(e);
            }
            if let Some(t) = p.time {
                if last_time.is_some_and(|l| t < l) {
                    self.out_of_order = true;
                }
                last_time = Some(t);
                self.start = Some(self.start.map_or(t, |s| s.min(t)));
                self.end = Some(self.end.map_or(t, |s| s.max(t)));
            }
        }
    }
    fn cells(&self) -> Vec<String> {
        let ele = |v: f64| {
            if self.gain + self.loss > 0.0 {
                format!("{v:.0}")
            } else {
                String::new()
            }
        };
        let time = |t: Option<f64>| t.map(iso).unwrap_or_default();
        vec![
            self.points.to_string(),
            format!("{:.3}", self.distance / 1000.0),
            ele(self.gain),
            ele(self.loss),
            time(self.start),
            time(self.end),
            match (self.start, self.end) {
                (Some(s), Some(e)) => duration(e - s),
                _ => String::new(),
            },
        ]
    }
}

/// Seconds since 1970 as "2026-05-01 08:00:00 UTC".
fn iso(t: f64) -> String {
    let secs = t.floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

const TOTAL_COLUMNS: [&str; 7] = [
    "Points",
    "Distance (km)",
    "Elevation gain (m)",
    "Elevation loss (m)",
    "Start",
    "End",
    "Duration",
];

fn name_of(doc: &Doc, e: u32) -> String {
    doc.get(e)
        .children
        .iter()
        .find(|&&c| doc.get(c).name == "name")
        .map(|&c| doc.text(c))
        .unwrap_or_default()
}

/// The elevation of the track sampled every `step` metres along it (linear interpolation).
fn profile(pts: &[Point]) -> Option<(Vec<f32>, f64)> {
    let mut along = vec![];
    let mut d = 0.0;
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            d += haversine(pts[i - 1].at, p.at);
        }
        if let Some(e) = p.ele {
            along.push((d, e));
        }
    }
    let total = along.last()?.0 - along.first()?.0;
    if along.len() < 2 || total <= 0.0 {
        return None;
    }
    let step = (total / PROFILE_SAMPLES).max(1.0);
    let start = along[0].0;
    let mut samples = vec![];
    let mut j = 0;
    let mut x = start;
    while x <= start + total && samples.len() <= PROFILE_SAMPLES as usize + 1 {
        while j + 1 < along.len() && along[j + 1].0 < x {
            j += 1;
        }
        let (d0, e0) = along[j];
        let (d1, e1) = along[(j + 1).min(along.len() - 1)];
        let e = if d1 > d0 {
            e0 + (e1 - e0) * ((x - d0) / (d1 - d0)).clamp(0.0, 1.0)
        } else {
            e0
        };
        samples.push(e as f32);
        x += step;
    }
    Some((samples, step))
}

fn custom(doc: &Doc, out: &mut Inspection) {
    let mut map = Map::default();
    let mut bounds = Bounds::default();
    let mut problems = vec![];
    for w in super::super::spec::select(doc, 0, "wpt") {
        if let Some(p) = point(doc, w, &mut problems) {
            bounds.add(p.at);
            map.add(
                &name_of(doc, w),
                Parts {
                    points: vec![p.at],
                    ..Parts::default()
                },
            );
        }
    }
    let read = |e: u32, child: &str, problems: &mut Vec<String>| -> Vec<Point> {
        doc.get(e)
            .children
            .iter()
            .filter(|&&c| doc.get(c).name == child)
            .filter_map(|&c| point(doc, c, problems))
            .collect()
    };
    let mut routes = Table {
        name: "Routes".into(),
        columns: std::iter::once("Name")
            .chain(TOTAL_COLUMNS)
            .map(str::to_owned)
            .collect(),
        ..Table::default()
    };
    for r in super::super::spec::select(doc, 0, "rte")
        .into_iter()
        .take(MAX_ROWS)
    {
        let pts = read(r, "rtept", &mut problems);
        let mut t = Totals::default();
        t.add(&pts);
        let name = name_of(doc, r);
        let line: Vec<[f64; 2]> = pts.iter().map(|p| p.at).collect();
        line.iter().for_each(|p| bounds.add(*p));
        map.add(
            &name,
            Parts {
                lines: vec![line],
                ..Parts::default()
            },
        );
        routes.rows.push(Row {
            cells: std::iter::once(name).chain(t.cells()).collect(),
            source: Some(doc.get(r).span.clone()),
        });
    }
    let mut tracks = Table {
        name: "Tracks".into(),
        columns: ["Name", "Segments"]
            .into_iter()
            .chain(TOTAL_COLUMNS)
            .map(str::to_owned)
            .collect(),
        ..Table::default()
    };
    let mut first_profile: Option<(String, Vec<f32>, f64)> = None;
    let mut total_distance = 0.0;
    let mut out_of_order = false;
    for trk in super::super::spec::select(doc, 0, "trk")
        .into_iter()
        .take(MAX_ROWS)
    {
        let name = name_of(doc, trk);
        let mut t = Totals::default();
        let mut lines = vec![];
        let mut all = vec![];
        let segments = super::super::spec::select(doc, trk, "trkseg");
        for &seg in &segments {
            let pts = read(seg, "trkpt", &mut problems);
            // Distances add up within segments; a gap between segments is not travelled.
            t.add(&pts);
            let line: Vec<[f64; 2]> = pts.iter().map(|p| p.at).collect();
            line.iter().for_each(|p| bounds.add(*p));
            lines.push(line);
            all.extend(pts);
        }
        out_of_order |= t.out_of_order;
        total_distance += t.distance;
        if first_profile.is_none()
            && let Some((samples, step)) = profile(&all)
        {
            first_profile = Some((name.clone(), samples, step));
        }
        map.add(
            &name,
            Parts {
                lines,
                ..Parts::default()
            },
        );
        tracks.rows.push(Row {
            cells: [name, segments.len().to_string()]
                .into_iter()
                .chain(t.cells())
                .collect(),
            source: Some(doc.get(trk).span.clone()),
        });
    }
    if !tracks.rows.is_empty() {
        out.fact(
            "Content",
            "Track distance (total)",
            format!("{:.3} km", total_distance / 1000.0),
            None,
        );
    }
    out.fact("Content", "Bounds", bounds.describe_lonlat(), None);
    if !routes.rows.is_empty() {
        out.tables.push(routes);
    }
    if !tracks.rows.is_empty() {
        out.tables.push(tracks);
    }
    map.finish(true, &mut out.visuals, &mut out.notes);
    if let Some((name, samples, step)) = first_profile {
        out.visuals.push(Visual::Plot {
            channels: vec![Channel {
                name: format!(
                    "Elevation{}",
                    if name.is_empty() {
                        String::new()
                    } else {
                        format!(" — {name}")
                    }
                ),
                unit: "m".into(),
                rate: 1000.0 / step,
                samples,
            }],
        });
        out.notes.push(format!(
            "Elevation profile of the first track: sampled every {} m of distance along it (linear interpolation between points). On the plot's horizontal axis one second stands for one kilometre.",
            num((step * 10.0).round() / 10.0)
        ));
    }
    out.notes.push(
        "Distances use the haversine formula on a sphere of 6,371 km; elevation gain and loss add up every rise and fall between recorded points, without smoothing."
            .into(),
    );
    if problems.is_empty() && !out_of_order {
        out.check(
            Stage::Semantic,
            State::Passed,
            "Coordinates are within range and track times are in order",
        );
    } else {
        let first = problems
            .first()
            .cloned()
            .unwrap_or_else(|| "track times go backwards".into());
        out.check(
            Stage::Semantic,
            State::Failed,
            format!("{} problem(s); first: {first}", problems.len().max(1)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn times_and_durations() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800.0));
        assert_eq!(
            parse_time("2026-05-01T10:00:00+02:00"),
            parse_time("2026-05-01T08:00:00Z")
        );
        assert_eq!(parse_time("2026-05-01T08:00:00.5Z").unwrap().fract(), 0.5);
        assert_eq!(parse_time("yesterday"), None);
        assert_eq!(iso(951_868_800.0), "2000-03-01 00:00:00 UTC");
        assert_eq!(duration(3725.0), "1 h 02 min 05 s");
        assert_eq!(duration(750.0), "12 min 30 s");
    }
}
