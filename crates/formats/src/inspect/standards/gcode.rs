//! G-code (RS-274) programs for 3D printers and CNC machines: words (G, M, T, F, S, X, Y, Z,
//! I, J, K, R, E, N, P), comments (`;` and `( )`), line numbers and RepRap checksums (`*`).
//! Modal state is followed for G0–G3, G17–G19, G20/G21, G90/G91, M82/M83 and G92; the feed
//! toolpath (G1, G2/G3 arcs sampled) is drawn in XY. Other codes are shown, not interpreted.
use super::cad::{Drawing, Rows, arc, clip, lines, num};
use crate::inspect::{Handler, Input, Inspection, Probe, Stage, State};
use std::collections::{BTreeMap, BTreeSet};

pub struct Gcode;

impl Handler for Gcode {
    fn name(&self) -> &'static str {
        "G-code"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if !probe.ext_is(&["gcode", "nc", "ngc", "tap"]) {
            return false;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        // Several lines that start with a G or M word (after an optional line number).
        let mut hits = 0;
        for line in text.lines().take(2000) {
            let l = line.trim_start();
            let l = l.strip_prefix(['N', 'n']).map_or(l, |r| {
                r.trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start()
            });
            let mut c = l.chars();
            if matches!(c.next(), Some('G' | 'g' | 'M' | 'm'))
                && c.next().is_some_and(|d| d.is_ascii_digit())
            {
                hits += 1;
                if hits >= 3 {
                    return true;
                }
            }
        }
        hits > 0 && text.lines().count() < 3
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        inspect(&text, cancelled)
    }
}

fn meaning(code: &str) -> &'static str {
    match code {
        "G0" => "Rapid move",
        "G1" => "Linear move",
        "G2" => "Clockwise arc",
        "G3" => "Counter-clockwise arc",
        "G4" => "Dwell",
        "G10" => "Set offsets / retract",
        "G11" => "Unretract",
        "G17" => "XY plane",
        "G18" => "ZX plane",
        "G19" => "YZ plane",
        "G20" => "Inches",
        "G21" => "Millimetres",
        "G28" => "Home",
        "G29" => "Bed levelling",
        "G40" => "Cutter compensation off",
        "G41" => "Cutter compensation left",
        "G42" => "Cutter compensation right",
        "G43" => "Tool length offset",
        "G49" => "Tool length offset off",
        "G53" => "Machine coordinates",
        "G54" | "G55" | "G56" | "G57" | "G58" | "G59" => "Work offset",
        "G80" => "Cancel canned cycle",
        "G81" => "Drilling cycle",
        "G82" => "Drilling cycle with dwell",
        "G83" => "Peck drilling cycle",
        "G84" => "Tapping cycle",
        "G90" => "Absolute positioning",
        "G91" => "Relative positioning",
        "G92" => "Set position",
        "G94" => "Feed per minute",
        "G95" => "Feed per revolution",
        "M0" => "Program stop",
        "M1" => "Optional stop",
        "M2" => "Program end",
        "M3" => "Spindle on, clockwise",
        "M4" => "Spindle on, counter-clockwise",
        "M5" => "Spindle stop",
        "M6" => "Tool change",
        "M7" => "Mist coolant on",
        "M8" => "Flood coolant on",
        "M9" => "Coolant off",
        "M30" => "Program end and rewind",
        "M82" => "Extruder absolute",
        "M83" => "Extruder relative",
        "M84" => "Motors off",
        "M104" => "Set hotend temperature",
        "M105" => "Report temperatures",
        "M106" => "Fan on",
        "M107" => "Fan off",
        "M109" => "Wait for hotend temperature",
        "M117" => "Display message",
        "M140" => "Set bed temperature",
        "M190" => "Wait for bed temperature",
        _ => "",
    }
}

/// Codes that may share a line with axis words that still move in the current motion mode
/// (unit, plane, distance mode, offsets, compensation; and M codes).
fn keeps_motion(code: &str) -> bool {
    code.starts_with('M')
        || matches!(
            code,
            "G17"
                | "G18"
                | "G19"
                | "G20"
                | "G21"
                | "G40"
                | "G41"
                | "G42"
                | "G43"
                | "G49"
                | "G54"
                | "G55"
                | "G56"
                | "G57"
                | "G58"
                | "G59"
                | "G61"
                | "G64"
                | "G90"
                | "G91"
                | "G94"
                | "G95"
        )
}

/// Words of one line, with comments removed: (letter, number text).
fn words(code: &str) -> Vec<(char, &str)> {
    let mut out = vec![];
    let b = code.as_bytes();
    let mut i = 0;
    while i < b.len() && out.len() < 256 {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            let s = i + 1;
            let mut j = s;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let ns = j;
            while j < b.len() && matches!(b[j], b'0'..=b'9' | b'.' | b'-' | b'+') {
                j += 1;
            }
            out.push((c.to_ascii_uppercase(), &code[ns..j]));
            i = j.max(s);
        } else {
            i += 1;
        }
    }
    out
}

/// A G or M number written plainly: "G01" -> "G1", "G1.0" stays "G1.0" unless ".0".
fn code_name(letter: char, n: &str) -> String {
    let (int, frac) = n.split_once('.').unwrap_or((n, ""));
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if frac.trim_end_matches('0').is_empty() {
        format!("{letter}{int}")
    } else {
        format!("{letter}{int}.{frac}")
    }
}

fn inspect(text: &str, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("G-code", "Machine program (RS-274)");
    let mut table = Rows::new(
        "Commands",
        &[
            "Line", "N", "Command", "Meaning", "X", "Y", "Z", "E", "I", "J", "K/R", "F", "S",
            "Tool", "Comment",
        ],
    );
    let mut drawing = Drawing::default();
    let mut path: Vec<[f64; 2]> = vec![];
    // Modal state.
    let (mut absolute, mut ext_absolute, mut inches, mut plane) = (true, true, false, 17);
    let mut motion: Option<u32> = None;
    let mut pos = [0f64; 4]; // X Y Z E
    let (mut feed, mut feed_len, mut rapid_len, mut feed_time) = (0f64, 0f64, 0f64, 0f64);
    let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut tools: BTreeSet<String> = BTreeSet::new();
    let mut feeds: BTreeSet<String> = BTreeSet::new();
    let mut codes: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let (mut checksums, mut bad_checksums, mut first_bad) = (0u64, 0u64, None);
    let (mut extrusion, mut reprap_m, mut cnc_signs) = (false, false, 0u32);
    let mut flavour: Option<String> = None;
    let (mut units_seen, mut abs_seen, mut other_plane_arcs, mut commands) =
        (false, false, 0u64, 0u64);
    let mut lines_n = 0u64;
    let mut tool = String::new();
    for (n, (at, raw)) in lines(text).enumerate() {
        if n % 65536 == 0 && cancelled() {
            return Err("Cancelled".into());
        }
        lines_n += 1;
        // Comments: ';' to the end of the line, and '( … )'.
        let (code_part, mut comment) = match raw.find(';') {
            Some(i) => (&raw[..i], raw[i + 1..].trim().to_owned()),
            None => (raw, String::new()),
        };
        if flavour.is_none()
            && let Some(f) = comment.strip_prefix("FLAVOR:")
        {
            flavour = Some(f.trim().to_owned());
        }
        let mut code = String::with_capacity(code_part.len());
        let mut depth = 0;
        let mut paren = String::new();
        for c in code_part.chars() {
            match c {
                '(' => depth += 1,
                ')' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        paren.push(' ');
                    }
                }
                _ if depth > 0 => paren.push(c),
                _ => code.push(c),
            }
        }
        if !paren.trim().is_empty() {
            cnc_signs += 1;
            if comment.is_empty() {
                comment = paren.trim().to_owned();
            }
        }
        // Checksum: "*nn" at the end; XOR of every byte before '*'.
        let code_str = match code.rfind('*') {
            Some(i) => {
                let given: Option<u32> = code[i + 1..].trim().parse().ok();
                let computed = code_part.as_bytes()[..code_part.find('*').unwrap_or(0)]
                    .iter()
                    .fold(0u8, |a, &b| a ^ b);
                checksums += 1;
                if given != Some(u32::from(computed)) {
                    bad_checksums += 1;
                    first_bad.get_or_insert(n + 1);
                }
                code[..i].to_owned()
            }
            None => code,
        };
        let w = words(&code_str);
        if w.is_empty() {
            continue;
        }
        if code_str.trim_start().starts_with('%') || code_str.trim_start().starts_with('O') {
            cnc_signs += 1;
        }
        let get = |l: char| w.iter().find(|x| x.0 == l).map(|x| x.1);
        let val = |l: char| {
            get(l)
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let mut names: Vec<String> = vec![];
        let mut move_code: Option<u32> = None;
        let mut set_position = false;
        let mut home = false;
        for &(l, v) in &w {
            if l != 'G' && l != 'M' {
                continue;
            }
            let name = code_name(l, v);
            if codes.len() < 10_000 {
                *codes.entry(name.clone()).or_default() += 1;
            }
            if meaning(&name).is_empty() && unknown.len() < 1000 {
                unknown.insert(name.clone());
            }
            match name.as_str() {
                "G0" => move_code = Some(0),
                "G1" => move_code = Some(1),
                "G2" => move_code = Some(2),
                "G3" => move_code = Some(3),
                "G17" => plane = 17,
                "G18" => plane = 18,
                "G19" => plane = 19,
                "G20" => (inches, units_seen) = (true, true),
                "G21" => (inches, units_seen) = (false, true),
                "G90" => (absolute, ext_absolute, abs_seen) = (true, true, true),
                "G91" => (absolute, abs_seen) = (false, true),
                "M82" => ext_absolute = true,
                "M83" => ext_absolute = false,
                "G92" => set_position = true,
                "G28" => home = true,
                "M104" | "M109" | "M140" | "M190" | "M106" | "M107" => reprap_m = true,
                "M6" | "G43" | "G54" | "G55" | "G56" | "G57" | "G58" | "G59" => cnc_signs += 1,
                _ => {}
            }
            if names.len() < 8 {
                names.push(name);
            }
        }
        if let Some(t) = get('T') {
            tool = format!("T{t}");
            if tools.len() < 1000 {
                tools.insert(tool.clone());
            }
        }
        if let Some(f) = val('F') {
            feed = f;
            if feeds.len() < 1000 {
                feeds.insert(num(f));
            }
        }
        if get('E').is_some() {
            extrusion = true;
        }
        let axes = ['X', 'Y', 'Z', 'E'];
        let has_axis = axes.iter().any(|&a| get(a).is_some());
        if let Some(m) = move_code {
            motion = Some(m);
        }
        let mut moved = false;
        if set_position {
            for (k, &a) in axes.iter().enumerate() {
                if let Some(v) = val(a) {
                    pos[k] = v;
                }
            }
            if !has_axis {
                pos = [0.0; 4];
            }
            // The drawing continues from the new position.
            if path.len() >= 2 {
                drawing.path(std::mem::take(&mut path));
            }
            path.clear();
        } else if home {
            for (k, &a) in axes.iter().enumerate().take(3) {
                if get(a).is_some() || !has_axis {
                    pos[k] = 0.0;
                }
            }
            if path.len() >= 2 {
                drawing.path(std::mem::take(&mut path));
            }
            path.clear();
        } else if let (Some(m), true) =
            (motion, has_axis || get('I').is_some() || get('J').is_some())
            && (move_code.is_some() || names.iter().all(|c| keeps_motion(c)))
        {
            let from = pos;
            let mut to = pos;
            for (k, &a) in axes.iter().enumerate() {
                if let Some(v) = val(a) {
                    let relative = if k == 3 {
                        !ext_absolute || !absolute
                    } else {
                        !absolute
                    };
                    to[k] = if relative { pos[k] + v } else { v };
                }
            }
            let d3 =
                ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2))
                    .sqrt();
            moved = true;
            if m == 0 {
                rapid_len += d3;
                if path.len() >= 2 {
                    drawing.path(std::mem::take(&mut path));
                }
                path.clear();
            } else {
                for p in [from, to] {
                    for k in 0..3 {
                        min[k] = min[k].min(p[k]);
                        max[k] = max[k].max(p[k]);
                    }
                }
                if path.is_empty() {
                    path.push([from[0], from[1]]);
                }
                let same_xy = to[0] == from[0] && to[1] == from[1];
                let len = if same_xy && m == 1 {
                    d3
                } else if (m == 2 || m == 3) && plane == 17 {
                    let (pts, len) = arc_move(from, to, m == 2, val('I'), val('J'), val('R'));
                    for p in &pts {
                        for k in 0..2 {
                            min[k] = min[k].min(p[k]);
                            max[k] = max[k].max(p[k]);
                        }
                    }
                    path.extend(pts.into_iter().skip(1));
                    len
                } else {
                    if m == 2 || m == 3 {
                        other_plane_arcs += 1;
                    }
                    path.push([to[0], to[1]]);
                    d3
                };
                feed_len += len;
                if feed > 0.0 {
                    feed_time += len / feed;
                }
                if path.len() > 100_000 {
                    let last = *path.last().unwrap_or(&[0.0, 0.0]);
                    drawing.path(std::mem::take(&mut path));
                    path.push(last);
                }
            }
            pos = to;
        }
        commands += 1;
        if names.is_empty()
            && moved
            && let Some(m) = motion
        {
            names.push(format!("G{m}"));
        }
        let command = names.join(" ");
        let meaning_text = names
            .iter()
            .map(|c| match meaning(c) {
                "" => "(not interpreted)",
                m => m,
            })
            .collect::<Vec<_>>()
            .join(", ");
        let cell = |l: char| get(l).unwrap_or("").to_owned();
        let kr = match (get('K'), get('R')) {
            (Some(k), _) => k.to_owned(),
            (None, Some(r)) => format!("R{r}"),
            _ => String::new(),
        };
        table.push(
            vec![
                (n + 1).to_string(),
                cell('N'),
                command,
                meaning_text,
                cell('X'),
                cell('Y'),
                cell('Z'),
                cell('E'),
                cell('I'),
                cell('J'),
                kr,
                cell('F'),
                cell('S'),
                if get('T').is_some() {
                    tool.clone()
                } else {
                    String::new()
                },
                clip(&comment, 120),
            ],
            Some(at as u64..(at + raw.len()) as u64),
        );
    }
    if path.len() >= 2 {
        drawing.path(path);
    }
    if commands == 0 {
        return Err("No G-code words were found".into());
    }
    // Facts.
    let dialect = match &flavour {
        Some(f) => format!("{f} (declared by the slicer)"),
        None if extrusion || reprap_m => {
            "RepRap/Marlin style (3D printer: E axis, temperature codes)".into()
        }
        None if cnc_signs > 0 => {
            "Fanuc/LinuxCNC style (CNC: tool changes, offsets, ( ) comments)".into()
        }
        None => "Not determined".into(),
    };
    let unit = if inches { "inches" } else { "millimetres" };
    let g = "Program";
    out.fact(g, "Dialect (guess)", dialect, None);
    out.fact(g, "Lines", lines_n.to_string(), None);
    out.fact(g, "Commands", commands.to_string(), None);
    out.fact(
        g,
        "Units",
        format!(
            "{unit}{}",
            if units_seen {
                ""
            } else {
                " (no G20/G21; assumed)"
            }
        ),
        None,
    );
    out.fact(
        g,
        "Positioning",
        format!(
            "{}{}",
            if absolute {
                "absolute (G90)"
            } else {
                "relative (G91)"
            },
            if abs_seen {
                " at the end"
            } else {
                " (no G90/G91; assumed absolute)"
            }
        ),
        None,
    );
    out.fact(
        g,
        "Tools",
        tools.iter().cloned().collect::<Vec<_>>().join(", "),
        None,
    );
    let feed_list: Vec<String> = feeds.iter().take(20).cloned().collect();
    out.fact(
        g,
        "Feed rates",
        format!(
            "{}{}",
            feed_list.join(", "),
            if feeds.len() > 20 {
                format!(" … ({} values)", feeds.len())
            } else {
                String::new()
            }
        ),
        None,
    );
    let t = "Toolpath";
    if min[0] <= max[0] {
        out.fact(
            t,
            "Extent of feed moves (x × y × z)",
            format!(
                "{} × {} × {} {unit}",
                num(max[0] - min[0]),
                num(max[1] - min[1]),
                num(max[2] - min[2])
            ),
            None,
        );
        out.fact(
            t,
            "Feed bounding box",
            format!(
                "({}, {}, {}) to ({}, {}, {})",
                num(min[0]),
                num(min[1]),
                num(min[2]),
                num(max[0]),
                num(max[1]),
                num(max[2])
            ),
            None,
        );
    }
    out.fact(
        t,
        "Feed path length",
        format!("{} {unit}", num(feed_len)),
        None,
    );
    out.fact(
        t,
        "Rapid path length",
        format!("{} {unit}", num(rapid_len)),
        None,
    );
    if feed_time > 0.0 {
        out.fact(
            t,
            "Feed time at the programmed rates",
            format!("{} min (ignores acceleration and rapids)", num(feed_time)),
            None,
        );
    }
    let mut code_rows = Rows::new("Codes used", &["Code", "Meaning", "Lines"]);
    for (c, n) in &codes {
        code_rows.push(
            vec![
                c.clone(),
                match meaning(c) {
                    "" => "(not interpreted)".into(),
                    m => m.to_owned(),
                },
                n.to_string(),
            ],
            None,
        );
    }
    table.finish(&mut out);
    code_rows.finish(&mut out);
    // Checks.
    out.check(
        Stage::Parsed,
        State::Passed,
        format!("{commands} command lines read; modal state followed"),
    );
    if checksums > 0 {
        out.check(
            Stage::Semantic,
            if bad_checksums == 0 {
                State::Passed
            } else {
                State::Failed
            },
            if bad_checksums == 0 {
                format!("Checksums: all {checksums} line checksums match")
            } else {
                format!(
                    "Checksums: {bad_checksums} of {checksums} do not match (first on line {})",
                    first_bad.unwrap_or(0)
                )
            },
        );
    }
    out.notes.push(format!(
        "The drawing shows feed moves (G1, G2/G3 with arcs sampled) projected onto the XY plane; rapid moves (G0) are not drawn, and their length is reported separately.{}",
        if other_plane_arcs > 0 {
            format!(" {other_plane_arcs} arcs in the ZX or YZ plane are drawn as straight lines.")
        } else {
            String::new()
        }
    ));
    if !unknown.is_empty() {
        out.notes.push(format!(
            "Codes shown but not interpreted: {}.",
            clip(&unknown.iter().cloned().collect::<Vec<_>>().join(", "), 300)
        ));
    }
    out.notes.push(
        "Canned cycles, work offsets, cutter compensation and macros are listed, not simulated; the program is never run.".into(),
    );
    drawing.finish(&mut out, unit);
    out.not_validated();
    Ok(out)
}

/// Points of a G2/G3 arc in the XY plane and its length (helical Z included). The centre is
/// given by I/J offsets from the start, or by the radius R.
fn arc_move(
    from: [f64; 4],
    to: [f64; 4],
    clockwise: bool,
    i: Option<f64>,
    j: Option<f64>,
    r: Option<f64>,
) -> (Vec<[f64; 2]>, f64) {
    let (p, q) = ([from[0], from[1]], [to[0], to[1]]);
    let straight = || {
        let d = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
        (vec![p, q], d)
    };
    let centre = match (i, j, r) {
        (Some(_), _, _) | (_, Some(_), _) => [p[0] + i.unwrap_or(0.0), p[1] + j.unwrap_or(0.0)],
        (None, None, Some(r)) => {
            let d = [q[0] - p[0], q[1] - p[1]];
            let c = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if c == 0.0 || c > 2.0 * r.abs() + 1e-9 {
                return straight();
            }
            let h = (r * r - c * c / 4.0).max(0.0).sqrt();
            // Negative R: the longer arc. Clockwise puts the centre on the right.
            let side = if clockwise == (r > 0.0) { -1.0 } else { 1.0 };
            [
                (p[0] + q[0]) / 2.0 - side * d[1] / c * h,
                (p[1] + q[1]) / 2.0 + side * d[0] / c * h,
            ]
        }
        _ => return straight(),
    };
    let radius = ((p[0] - centre[0]).powi(2) + (p[1] - centre[1]).powi(2)).sqrt();
    if radius == 0.0 || !radius.is_finite() {
        return straight();
    }
    let a0 = (p[1] - centre[1]).atan2(p[0] - centre[0]);
    let mut a1 = (q[1] - centre[1]).atan2(q[0] - centre[0]);
    let tau = std::f64::consts::TAU;
    if clockwise {
        while a1 >= a0 - 1e-12 {
            a1 -= tau;
        }
    } else {
        while a1 <= a0 + 1e-12 {
            a1 += tau;
        }
    }
    let sweep = (a1 - a0).abs();
    let len = ((radius * sweep).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
    (arc(centre, radius, a0, a1), len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn words_codes_and_arcs() {
        assert_eq!(
            words("G01X10.5 Y-2"),
            [('G', "01"), ('X', "10.5"), ('Y', "-2")]
        );
        assert_eq!(code_name('G', "01"), "G1");
        assert_eq!(code_name('G', "00"), "G0");
        assert_eq!(code_name('G', "38.2"), "G38.2");
        // Counter-clockwise quarter circle of radius 10 around the origin.
        let (pts, len) = arc_move(
            [10.0, 0.0, 0.0, 0.0],
            [0.0, 10.0, 0.0, 0.0],
            false,
            Some(-10.0),
            Some(0.0),
            None,
        );
        assert!((len - 10.0 * std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert!((pts[pts.len() / 2][0] - 7.0710678).abs() < 1e-6);
        let (_, len_r) = arc_move(
            [10.0, 0.0, 0.0, 0.0],
            [0.0, 10.0, 0.0, 0.0],
            false,
            None,
            None,
            Some(10.0),
        );
        assert!((len_r - len).abs() < 1e-9);
    }
}
