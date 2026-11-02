//! KML 2.x (Google Earth, OGC 07-147) and KMZ (KML zipped with its images): the document and
//! folder outline, placemarks with their geometries on a map, and the archive's other entries.
//! Network links and remote images are listed, never fetched.
use super::geo::{Bounds, MAX_NESTING, Map, Parts, TypeCounts};
use crate::inspect::{
    Handler, Input, Inspection, MAX_ITEMS, MAX_ROWS, OutlineNode, Probe, Row, Stage, State, Table,
    spec::{F, XmlSpec, run_xml, select},
    xmldoc::Doc,
};

pub static KML: XmlSpec = XmlSpec {
    family: "KML",
    kind: "KML — placemarks and overlays (Google Earth)",
    matches: |root, ns| {
        root == "kml"
            && (ns.is_empty()
                || ns.starts_with("http://www.opengis.net/kml/")
                || ns.starts_with("http://earth.google.com/kml/"))
    },
    version: |doc| {
        let ns = &doc.root().ns;
        ns.rsplit('/')
            .next()
            .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(|v| format!("KML {v}"))
    },
    facts: &[
        F("Document", "Name", "Document/name | Folder/name"),
        F("Document", "Description", "Document/description"),
        F("Document", "Author", "Document/author/name"),
        F("Content", "Placemarks", "count:**/Placemark"),
        F("Content", "Folders", "count:**/Folder"),
        F("Content", "Styles", "count:**/Style"),
        F("Content", "Ground overlays", "count:**/GroundOverlay"),
    ],
    tables: &[],
    custom: Some(custom),
};

/// Text of a description without markup: tags removed, common entities decoded.
fn plain(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let words: Vec<&str> = out.split_whitespace().collect();
    let joined = words.join(" ");
    match joined.char_indices().nth(500) {
        Some((i, _)) => format!("{}…", &joined[..i]),
        None => joined,
    }
}

/// KML coordinate tuples ("lon,lat[,alt]" separated by white space; spaces next to commas,
/// as some writers put them, are tolerated). The second value tells whether one was unreadable.
pub fn coordinates(text: &str) -> (Vec<[f64; 2]>, bool) {
    let mut squeezed = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && c != ',' && !squeezed.ends_with(',') && !squeezed.is_empty() {
            squeezed.push(' ');
        }
        pending_space = false;
        squeezed.push(c);
    }
    let mut bad = false;
    let mut out = vec![];
    for tuple in squeezed.split(' ').filter(|t| !t.is_empty()) {
        let mut v = tuple.split(',').map(|x| x.trim().parse::<f64>());
        match (v.next(), v.next()) {
            (Some(Ok(lon)), Some(Ok(lat))) => out.push([lon, lat]),
            _ => bad = true,
        }
    }
    (out, bad)
}

fn child(doc: &Doc, e: u32, name: &str) -> Option<u32> {
    doc.get(e)
        .children
        .iter()
        .copied()
        .find(|&c| doc.get(c).name == name)
}
fn child_text(doc: &Doc, e: u32, name: &str) -> String {
    child(doc, e, name).map(|c| doc.text(c)).unwrap_or_default()
}

const GEOMETRY: &[&str] = &[
    "Point",
    "LineString",
    "LinearRing",
    "Polygon",
    "MultiGeometry",
    "Track",
    "MultiTrack",
    "Model",
];

/// Parts of one geometry element; problems are counted.
fn geometry(doc: &Doc, e: u32, depth: usize, parts: &mut Parts, bad: &mut usize) {
    if depth > MAX_NESTING {
        *bad += 1;
        return;
    }
    let coords = |e: u32, bad: &mut usize| {
        let (c, broken) = coordinates(&child_text(doc, e, "coordinates"));
        *bad += usize::from(broken);
        c
    };
    match doc.get(e).name.as_str() {
        "Point" => parts.points.extend(coords(e, bad)),
        "LineString" => parts.lines.push(coords(e, bad)),
        "LinearRing" => parts.rings.push(coords(e, bad)),
        "Polygon" => {
            for b in ["outerBoundaryIs", "innerBoundaryIs"] {
                for boundary in doc
                    .get(e)
                    .children
                    .iter()
                    .filter(|&&c| doc.get(c).name == b)
                {
                    if let Some(r) = child(doc, *boundary, "LinearRing") {
                        parts.rings.push(coords(r, bad));
                    }
                }
            }
        }
        "Track" => {
            // gx:Track: one "lon lat alt" per gx:coord.
            let mut line = vec![];
            for &c in &doc.get(e).children {
                if doc.get(c).name == "coord" {
                    let t = doc.text(c);
                    let mut v = t.split_whitespace().map(|x| x.parse::<f64>());
                    match (v.next(), v.next()) {
                        (Some(Ok(lon)), Some(Ok(lat))) => line.push([lon, lat]),
                        _ => *bad += 1,
                    }
                }
            }
            parts.lines.push(line);
        }
        "Model" => {
            if let Some(l) = child(doc, e, "Location") {
                let lon = child_text(doc, l, "longitude").parse::<f64>();
                let lat = child_text(doc, l, "latitude").parse::<f64>();
                if let (Ok(lon), Ok(lat)) = (lon, lat) {
                    parts.points.push([lon, lat]);
                }
            }
        }
        _ => {
            // MultiGeometry and gx:MultiTrack.
            for &c in &doc.get(e).children {
                if GEOMETRY.contains(&doc.get(c).name.as_str()) {
                    geometry(doc, c, depth + 1, parts, bad);
                }
            }
        }
    }
}

/// Document, Folder and Placemark entries, indented.
fn outline(doc: &Doc, e: u32, depth: u16, out: &mut Vec<OutlineNode>) {
    if depth > 64 || out.len() >= MAX_ITEMS {
        return;
    }
    for &c in &doc.get(e).children {
        let el = doc.get(c);
        let container = matches!(el.name.as_str(), "Document" | "Folder");
        let leaf = matches!(
            el.name.as_str(),
            "Placemark" | "GroundOverlay" | "ScreenOverlay" | "PhotoOverlay" | "NetworkLink"
        );
        if container || leaf {
            out.push(OutlineNode {
                depth,
                label: el.name.clone(),
                value: child_text(doc, c, "name"),
                source: Some(el.span.clone()),
            });
        }
        if container {
            outline(doc, c, depth + 1, out);
        }
    }
}

fn custom(doc: &Doc, out: &mut Inspection) {
    outline(doc, 0, 0, &mut out.outline);
    let mut map = Map::default();
    let mut bounds = Bounds::default();
    let mut types = TypeCounts::default();
    let mut bad = 0usize;
    let mut table = Table {
        name: "Placemarks".into(),
        columns: [
            "Name",
            "Description",
            "Geometry",
            "Coordinates",
            "Style",
            "Folder",
        ]
        .map(str::to_owned)
        .to_vec(),
        ..Table::default()
    };
    // In document order (element indices), whatever their nesting.
    let mut placemarks = select(doc, 0, "**/Placemark");
    placemarks.sort_unstable();
    if placemarks.len() > MAX_ROWS {
        table.total = Some(placemarks.len() as u64);
    }
    for &p in &placemarks {
        let name = child_text(doc, p, "name");
        let mut parts = Parts::default();
        let mut kind = String::new();
        if let Some(g) = doc
            .get(p)
            .children
            .iter()
            .copied()
            .find(|&c| GEOMETRY.contains(&doc.get(c).name.as_str()))
        {
            kind = match doc.get(g).name.as_str() {
                "Track" => "gx:Track".into(),
                "MultiTrack" => "gx:MultiTrack".into(),
                n => n.into(),
            };
            geometry(doc, g, 0, &mut parts, &mut bad);
        }
        types.add(if kind.is_empty() {
            "no geometry"
        } else {
            &kind
        });
        let mut pb = Bounds::default();
        parts.bounds(&mut pb);
        if !pb.is_empty() && !pb.fits_lonlat() {
            bad += 1;
        }
        parts.bounds(&mut bounds);
        let count = parts.coordinates();
        map.add(&name, parts);
        if table.rows.len() < MAX_ROWS {
            let folder = doc
                .get(p)
                .parent
                .filter(|&f| matches!(doc.get(f).name.as_str(), "Folder" | "Document"))
                .map(|f| child_text(doc, f, "name"))
                .unwrap_or_default();
            table.rows.push(Row {
                cells: vec![
                    name,
                    plain(&child_text(doc, p, "description")),
                    kind,
                    count.to_string(),
                    child_text(doc, p, "styleUrl"),
                    folder,
                ],
                source: Some(doc.get(p).span.clone()),
            });
        }
    }
    out.fact("Content", "Geometry types", types.describe(), None);
    out.fact("Content", "Bounds", bounds.describe_lonlat(), None);
    if !table.rows.is_empty() {
        out.tables.push(table);
    }
    map.finish(true, &mut out.visuals, &mut out.notes);
    // Network links and remote images: listed, never fetched.
    let mut remote: Vec<String> = vec![];
    for h in select(doc, 0, "**/href") {
        let t = doc.text(h);
        let lower = t.to_ascii_lowercase();
        let in_link = doc
            .get(h)
            .parent
            .and_then(|l| doc.get(l).parent)
            .is_some_and(|n| doc.get(n).name == "NetworkLink");
        if (in_link
            || lower.starts_with("http:")
            || lower.starts_with("https:")
            || lower.starts_with("//"))
            && !remote.contains(&t)
        {
            remote.push(t);
        }
    }
    let links = select(doc, 0, "**/NetworkLink").len();
    if links > 0 {
        out.fact("Content", "Network links", links.to_string(), None);
    }
    if !remote.is_empty() {
        let shown: Vec<&str> = remote.iter().take(20).map(String::as_str).collect();
        out.notes.push(format!(
            "Not fetched (Isolinear never downloads): {}{}",
            shown.join(", "),
            if remote.len() > 20 {
                format!(" and {} more", remote.len() - 20)
            } else {
                String::new()
            }
        ));
    }
    if bad == 0 {
        out.check(
            Stage::Semantic,
            State::Passed,
            "Coordinates are readable longitude/latitude values",
        );
    } else {
        out.check(
            Stage::Semantic,
            State::Failed,
            format!("{bad} geometry(ies) with unreadable or out-of-range coordinates"),
        );
    }
}

// ----- KMZ -----

/// Largest KML document read out of a KMZ.
const MAX_KML: u64 = 256 * 1024 * 1024;

pub struct Kmz;
impl Handler for Kmz {
    fn name(&self) -> &'static str {
        "KMZ"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        probe.ext_is(&["kmz"]) && probe.head.starts_with(b"PK\x03\x04")
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        use crate::table::archive::reader as archive;
        let mut cursor = std::io::Cursor::new(input.bytes);
        let listing = archive::list_zip(&mut cursor, cancelled)
            .map_err(|e| format!("The KMZ archive cannot be read: {e}"))?;
        let kml = |e: &&archive::Entry| !e.dir && e.name.to_ascii_lowercase().ends_with(".kml");
        let main = listing
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("doc.kml"))
            .or_else(|| listing.entries.iter().find(kml))
            .ok_or("The KMZ archive holds no KML document")?;
        if main.size > MAX_KML {
            return Err(format!(
                "{} is larger than {} MiB",
                main.name,
                MAX_KML >> 20
            ));
        }
        let mut bytes = vec![];
        archive::extract_zip(&mut cursor, main, &mut bytes)
            .map_err(|e| format!("{} cannot be read: {e}", main.name))?;
        let text = String::from_utf8_lossy(&bytes);
        let doc = Doc::parse(&text, cancelled)?;
        let root = doc.root();
        if !(KML.matches)(&root.name, &root.ns) {
            return Err(format!("{} is not a KML document", main.name));
        }
        let mut out = run_xml(&KML, &doc);
        out.kind = "KMZ — zipped KML with its files".into();
        // Spans point into the KML inside the archive, not into the KMZ file.
        for f in &mut out.facts {
            f.source = None;
        }
        for t in &mut out.tables {
            t.rows.iter_mut().for_each(|r| r.source = None);
        }
        out.outline.iter_mut().for_each(|o| o.source = None);
        out.fact("Archive", "KML document", main.name.clone(), None);
        out.fact(
            "Archive",
            "Entries",
            listing.entries.len().to_string(),
            None,
        );
        let mut others = Table {
            name: "Archive entries".into(),
            columns: ["Name", "Size", "Compressed", "Method", "Note"]
                .map(str::to_owned)
                .to_vec(),
            ..Table::default()
        };
        for e in listing
            .entries
            .iter()
            .filter(|e| e.name != main.name && !e.dir)
        {
            if others.rows.len() >= MAX_ROWS {
                others.total = Some(listing.entries.len() as u64);
                break;
            }
            others.rows.push(Row {
                cells: vec![
                    e.name.clone(),
                    e.size.to_string(),
                    e.packed.map(|p| p.to_string()).unwrap_or_default(),
                    e.method.clone(),
                    if e.unsafe_path {
                        "unsafe path (never extracted)".into()
                    } else if e.encrypted {
                        "encrypted".into()
                    } else {
                        String::new()
                    },
                ],
                source: Some(e.offset..e.offset + 30),
            });
        }
        if !others.rows.is_empty() {
            out.tables.push(others);
        }
        out.notes.extend(listing.warnings.iter().cloned());
        out.notes.push(format!(
            "Facts, placemarks and the outline come from {} inside the archive; their byte positions are not in this file",
            main.name
        ));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinate_tuples_and_descriptions() {
        let (c, bad) = coordinates(" 6.1,49.6,0\n 6.2 , 49.7  6.3,49.8,10 ");
        assert_eq!(c, vec![[6.1, 49.6], [6.2, 49.7], [6.3, 49.8]]);
        assert!(!bad);
        assert!(coordinates("6.1,x").1);
        assert_eq!(
            plain("<p>Caf&eacute; <b>open</b> &amp; busy</p>"),
            "Caf&eacute; open & busy"
        );
    }
}
