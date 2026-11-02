//! glTF 2.0 scenes: `.gltf` (JSON) and `.glb` (binary container with a JSON and a BIN chunk).
//! Shows the asset, scenes and node tree, meshes and primitives, materials, textures, images,
//! cameras, animations, skins, buffers and extensions, and draws triangle primitives of the
//! default scene with node transforms applied. Geometry is read from the GLB BIN chunk or from
//! base64 `data:` URIs only: external files are listed, never opened or fetched.
use super::cad::{Mesh, Rows, clip, num};
use crate::inspect::{
    Handler, Input, Inspection, MAX_ITEMS, OutlineNode, Probe, Stage, State, Visual,
    jsondoc::{self, Node, Value},
};
use std::{borrow::Cow, collections::HashMap, ops::Range, rc::Rc};

pub struct Gltf;

/// Node visits while walking the scene (instancing can repeat nodes).
const MAX_VISITS: usize = 200_000;
/// Elements read from one accessor.
const MAX_ACCESSOR: usize = 50_000_000;
/// Pictures handed to the app, and their total size.
const MAX_PICTURES: usize = 16;
const MAX_PICTURE_BYTES: usize = 64 * 1024 * 1024;

impl Handler for Gltf {
    fn name(&self) -> &'static str {
        "glTF"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        if probe.head.starts_with(b"glTF") {
            return true;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        if !text.trim_start().starts_with('{') {
            return false;
        }
        let keys = jsondoc::top_keys(text);
        keys.iter().any(|k| k == "asset")
            && (probe.ext_is(&["gltf"])
                || keys
                    .iter()
                    .any(|k| matches!(k.as_str(), "accessors" | "meshes" | "bufferViews")))
    }
    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = input.bytes;
        if b.starts_with(b"glTF") {
            glb(b, input.size, cancelled)
        } else {
            let text = input.text()?;
            let root = jsondoc::parse(&text)?;
            let mut out = Inspection::new("glTF", "3D scene (glTF JSON)");
            out.check(Stage::Parsed, State::Passed, "Read as JSON");
            Scene::new(&root, 0, None, false).run(&mut out, cancelled);
            Ok(out)
        }
    }
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn glb(b: &[u8], size: u64, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
    let mut out = Inspection::new("glTF", "3D scene (GLB binary)");
    let version = u32_at(b, 4).ok_or("Too short for a GLB header")?;
    let length = u32_at(b, 8).ok_or("Too short for a GLB header")?;
    out.fact("Container", "GLB version", version.to_string(), Some(4..8));
    out.fact(
        "Container",
        "Declared length",
        length.to_string(),
        Some(8..12),
    );
    if version != 2 {
        out.check(
            Stage::Parsed,
            State::NotDone,
            format!("GLB version {version} is not supported (only version 2)"),
        );
        return Ok(out);
    }
    let mut at = 12usize;
    // The JSON chunk's data range and header offset; the BIN chunk.
    let mut json: Option<(Range<usize>, usize)> = None;
    let mut bin: Option<&[u8]> = None;
    let mut problems = vec![];
    let end = b.len().min(length as usize);
    let mut chunks = 0;
    while at + 8 <= end && chunks < 1024 {
        chunks += 1;
        let len = u32_at(b, at).unwrap_or(0) as usize;
        let kind = u32_at(b, at + 4).unwrap_or(0);
        let data = at + 8;
        let Some(stop) = data.checked_add(len).filter(|&s| s <= b.len()) else {
            problems.push(format!("chunk at byte {at} runs past the end of the file"));
            if kind == 0x4E4F_534A && json.is_none() {
                json = Some((data..b.len(), at));
            }
            break;
        };
        match kind {
            0x4E4F_534A if json.is_none() => json = Some((data..stop, at)),
            0x004E_4942 if bin.is_none() => bin = Some(&b[data..stop]),
            _ => {}
        }
        at = stop;
    }
    if u64::from(length) != size {
        problems.push(format!("declared length {length}, file size {size}"));
    }
    let (range, chunk_at) = json.ok_or("The GLB file has no JSON chunk")?;
    out.fact(
        "Container",
        "JSON chunk",
        format!("{} bytes", range.len()),
        Some(chunk_at as u64..range.end as u64),
    );
    if let Some(bin) = bin {
        out.fact(
            "Container",
            "BIN chunk",
            format!("{} bytes", bin.len()),
            None,
        );
    }
    let text = String::from_utf8_lossy(&b[range.clone()]);
    let root = jsondoc::parse(text.trim_end_matches(['\0', ' ']))?;
    if problems.is_empty() {
        out.check(
            Stage::Parsed,
            State::Passed,
            "GLB header, JSON and BIN chunks read",
        );
    } else {
        out.check(
            Stage::Parsed,
            State::Partial,
            format!("GLB container: {}", problems.join("; ")),
        );
    }
    Scene::new(&root, range.start as u64, bin, true).run(&mut out, cancelled);
    Ok(out)
}

fn num_of(n: &Node) -> Option<f64> {
    match &n.value {
        Value::Number(s) => s.parse().ok().filter(|v: &f64| v.is_finite()),
        _ => None,
    }
}
fn idx(n: &Node, key: &str) -> Option<usize> {
    n.get(key)
        .and_then(num_of)
        .filter(|v| *v >= 0.0 && v.fract() == 0.0)
        .map(|v| v as usize)
}
fn s<'a>(n: &'a Node, key: &str) -> &'a str {
    n.get(key).and_then(Node::str).unwrap_or("")
}
fn floats(n: Option<&Node>) -> Vec<f64> {
    n.map(|a| a.items().iter().filter_map(num_of).collect())
        .unwrap_or_default()
}

/// Decode base64 (standard alphabet, padding and whitespace allowed).
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The bytes of a `data:` URI, if it is base64.
fn data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let rest = uri.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let mime = head.strip_suffix(";base64")?;
    Some((mime.to_owned(), base64(data)?))
}

type Mat = [[f64; 4]; 4];
const IDENTITY: Mat = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut m = [[0.0; 4]; 4];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    m
}

/// A node's local matrix: `matrix` (column-major) or translation × rotation × scale.
fn local(node: &Node) -> Mat {
    let m = floats(node.get("matrix"));
    if m.len() == 16 {
        let mut out = [[0.0; 4]; 4];
        for (c, col) in m.chunks(4).enumerate() {
            for (r, v) in col.iter().enumerate() {
                out[r][c] = *v;
            }
        }
        return out;
    }
    let t = floats(node.get("translation"));
    let q = floats(node.get("rotation"));
    let sc = floats(node.get("scale"));
    let (tx, ty, tz) = if t.len() == 3 {
        (t[0], t[1], t[2])
    } else {
        (0.0, 0.0, 0.0)
    };
    let (x, y, z, w) = if q.len() == 4 {
        (q[0], q[1], q[2], q[3])
    } else {
        (0.0, 0.0, 0.0, 1.0)
    };
    let (sx, sy, sz) = if sc.len() == 3 {
        (sc[0], sc[1], sc[2])
    } else {
        (1.0, 1.0, 1.0)
    };
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    [
        [r[0][0] * sx, r[0][1] * sy, r[0][2] * sz, tx],
        [r[1][0] * sx, r[1][1] * sy, r[1][2] * sz, ty],
        [r[2][0] * sx, r[2][1] * sy, r[2][2] * sz, tz],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn transform_summary(node: &Node) -> String {
    if node.get("matrix").is_some() {
        return "matrix".into();
    }
    let mut parts = vec![];
    for (key, label) in [("translation", "T"), ("rotation", "R"), ("scale", "S")] {
        let v = floats(node.get(key));
        if !v.is_empty() {
            parts.push(format!(
                "{label}({})",
                v.iter().map(|x| num(*x)).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    parts.join(" ")
}

struct Scene<'a> {
    root: &'a Node,
    /// Byte offset of the JSON text in the file (GLB), for sources.
    offset: u64,
    bin: Option<&'a [u8]>,
    glb: bool,
    buffers: Vec<Option<Cow<'a, [u8]>>>,
    problems: Vec<String>,
    positions: HashMap<usize, Rc<Vec<[f32; 3]>>>,
    indices: HashMap<usize, Rc<Vec<u32>>>,
}

impl<'a> Scene<'a> {
    fn new(root: &'a Node, offset: u64, bin: Option<&'a [u8]>, glb: bool) -> Self {
        Scene {
            root,
            offset,
            bin,
            glb,
            buffers: vec![],
            problems: vec![],
            positions: HashMap::new(),
            indices: HashMap::new(),
        }
    }
    fn list(&self, key: &str) -> &'a [Node] {
        self.root.get(key).map(Node::items).unwrap_or(&[])
    }
    fn span(&self, n: &Node) -> Option<Range<u64>> {
        Some(n.span.start + self.offset..n.span.end + self.offset)
    }
    fn name(&self, key: &str, i: usize) -> String {
        match self.list(key).get(i) {
            Some(n) if !s(n, "name").is_empty() => s(n, "name").to_owned(),
            Some(_) => format!("#{i}"),
            None => format!("#{i} (missing)"),
        }
    }
    fn problem(&mut self, p: String) {
        if self.problems.len() < 100 {
            self.problems.push(p);
        }
    }

    fn run(mut self, out: &mut Inspection, cancelled: &dyn Fn() -> bool) {
        let root = self.root;
        let asset = root.get("asset");
        let a = "Asset";
        if let Some(asset) = asset {
            let span = self.span(asset);
            out.fact(a, "glTF version", s(asset, "version"), span.clone());
            out.fact(a, "Minimum version", s(asset, "minVersion"), span.clone());
            out.fact(
                a,
                "Generator",
                clip(s(asset, "generator"), 200),
                span.clone(),
            );
            out.fact(a, "Copyright", clip(s(asset, "copyright"), 200), span);
            out.version = Some(format!("glTF {}", s(asset, "version")));
        } else {
            self.problem("there is no \"asset\" object".into());
        }
        if !s(asset.unwrap_or(root), "version").starts_with('2') && asset.is_some() {
            out.notes
                .push("Only glTF 2.0 is described; this file declares another version.".into());
        }
        let used: Vec<String> = root
            .get("extensionsUsed")
            .map(|e| e.items().iter().map(Node::text).collect())
            .unwrap_or_default();
        let required: Vec<String> = root
            .get("extensionsRequired")
            .map(|e| e.items().iter().map(Node::text).collect())
            .unwrap_or_default();
        out.fact(
            a,
            "Extensions used",
            used.join(", "),
            root.get("extensionsUsed").and_then(|n| self.span(n)),
        );
        out.fact(
            a,
            "Extensions required",
            required.join(", "),
            root.get("extensionsRequired").and_then(|n| self.span(n)),
        );
        let c = "Contents";
        for (key, label) in [
            ("scenes", "Scenes"),
            ("nodes", "Nodes"),
            ("meshes", "Meshes"),
            ("materials", "Materials"),
            ("textures", "Textures"),
            ("images", "Images"),
            ("cameras", "Cameras"),
            ("animations", "Animations"),
            ("skins", "Skins"),
            ("accessors", "Accessors"),
            ("buffers", "Buffers"),
        ] {
            let n = self.list(key).len();
            if n > 0 {
                out.fact(c, label, n.to_string(), None);
            }
        }
        self.load_buffers(out);
        self.outline(out);
        self.tables(out);
        self.pictures(out);
        self.geometry(out, cancelled, &required);
        if self.problems.is_empty() {
            out.check(
                Stage::Semantic,
                State::Passed,
                "References: indices between nodes, meshes, accessors, buffer views and buffers are in range",
            );
        } else {
            out.check(
                Stage::Semantic,
                State::Failed,
                format!(
                    "References: {} problems (first: {})",
                    self.problems.len(),
                    self.problems[0]
                ),
            );
        }
        out.not_validated();
    }

    fn load_buffers(&mut self, out: &mut Inspection) {
        let mut rows = Rows::new("Buffers", &["Buffer", "Name", "Byte length", "Source"]);
        for (i, b) in self.list("buffers").iter().enumerate() {
            let declared = idx(b, "byteLength").unwrap_or(0);
            let uri = s(b, "uri");
            let (data, source): (Option<Cow<[u8]>>, String) = if uri.is_empty() {
                match (self.glb && i == 0, self.bin) {
                    (true, Some(bin)) => (Some(Cow::Borrowed(bin)), "GLB BIN chunk".into()),
                    _ => (None, "missing (no URI and no BIN chunk)".into()),
                }
            } else if uri.starts_with("data:") {
                match data_uri(uri) {
                    Some((_, bytes)) => {
                        let n = bytes.len();
                        (
                            Some(Cow::Owned(bytes)),
                            format!("embedded data URI ({n} bytes)"),
                        )
                    }
                    None => (None, "data URI that is not base64".into()),
                }
            } else {
                (
                    None,
                    format!("external file “{}” (not opened)", clip(uri, 200)),
                )
            };
            if let Some(d) = &data
                && d.len() < declared
            {
                self.problem(format!(
                    "buffer {i} has {} bytes, declares {declared}",
                    d.len()
                ));
            }
            rows.push(
                vec![
                    i.to_string(),
                    s(b, "name").to_owned(),
                    declared.to_string(),
                    source,
                ],
                self.span(b),
            );
            self.buffers.push(data);
        }
        rows.finish(out);
    }

    fn outline(&self, out: &mut Inspection) {
        let nodes = self.list("nodes");
        let default = idx(self.root, "scene");
        for (si, scene) in self.list("scenes").iter().enumerate() {
            out.outline.push(OutlineNode {
                depth: 0,
                label: if s(scene, "name").is_empty() {
                    format!("Scene {si}")
                } else {
                    s(scene, "name").to_owned()
                },
                value: if Some(si) == default || (default.is_none() && si == 0) {
                    "scene (default)".into()
                } else {
                    "scene".into()
                },
                source: self.span(scene),
            });
            let mut stack: Vec<(usize, u16)> = scene
                .get("nodes")
                .map(|n| {
                    n.items()
                        .iter()
                        .rev()
                        .filter_map(|x| num_of(x).map(|v| (v as usize, 1)))
                        .collect()
                })
                .unwrap_or_default();
            let mut visits = 0;
            while let Some((ni, depth)) = stack.pop() {
                visits += 1;
                if visits > MAX_VISITS || out.outline.len() >= MAX_ITEMS {
                    break;
                }
                let Some(node) = nodes.get(ni) else { continue };
                let mut value = vec![];
                if let Some(m) = idx(node, "mesh") {
                    value.push(format!("mesh {}", self.name("meshes", m)));
                }
                if let Some(c) = idx(node, "camera") {
                    value.push(format!("camera {}", self.name("cameras", c)));
                }
                if let Some(sk) = idx(node, "skin") {
                    value.push(format!("skin {}", self.name("skins", sk)));
                }
                let t = transform_summary(node);
                if !t.is_empty() {
                    value.push(t);
                }
                out.outline.push(OutlineNode {
                    depth,
                    label: self.name("nodes", ni),
                    value: value.join(" · "),
                    source: self.span(node),
                });
                if depth < 64 {
                    for child in node
                        .get("children")
                        .map(Node::items)
                        .unwrap_or(&[])
                        .iter()
                        .rev()
                    {
                        if let Some(c) = num_of(child) {
                            stack.push((c as usize, depth + 1));
                        }
                    }
                }
            }
        }
    }

    fn tables(&mut self, out: &mut Inspection) {
        let mut rows = Rows::new(
            "Nodes",
            &[
                "Node",
                "Name",
                "Mesh",
                "Camera",
                "Skin",
                "Children",
                "Transform",
            ],
        );
        for (i, n) in self.list("nodes").iter().enumerate() {
            rows.push(
                vec![
                    i.to_string(),
                    s(n, "name").to_owned(),
                    idx(n, "mesh")
                        .map(|m| self.name("meshes", m))
                        .unwrap_or_default(),
                    idx(n, "camera")
                        .map(|m| self.name("cameras", m))
                        .unwrap_or_default(),
                    idx(n, "skin")
                        .map(|m| self.name("skins", m))
                        .unwrap_or_default(),
                    n.get("children")
                        .map(|c| c.items().len().to_string())
                        .unwrap_or_default(),
                    transform_summary(n),
                ],
                self.span(n),
            );
        }
        rows.finish(out);
        let mode_name = |m: usize| match m {
            0 => "points",
            1 => "lines",
            2 => "line loop",
            3 => "line strip",
            4 => "triangles",
            5 => "triangle strip",
            6 => "triangle fan",
            _ => "unknown",
        };
        let accessors = self.list("accessors");
        let count = |a: Option<usize>| {
            a.and_then(|a| accessors.get(a))
                .and_then(|a| idx(a, "count"))
                .map(|c| c.to_string())
                .unwrap_or_default()
        };
        let mut rows = Rows::new(
            "Primitives",
            &[
                "Mesh",
                "Name",
                "Primitive",
                "Mode",
                "Attributes",
                "Vertices",
                "Indices",
                "Material",
                "Morph targets",
            ],
        );
        for (mi, m) in self.list("meshes").iter().enumerate() {
            for (pi, p) in m
                .get("primitives")
                .map(Node::items)
                .unwrap_or(&[])
                .iter()
                .enumerate()
            {
                let attrs = match p.get("attributes").map(|a| &a.value) {
                    Some(Value::Object(f)) => f
                        .iter()
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    _ => String::new(),
                };
                let mut mode = mode_name(idx(p, "mode").unwrap_or(4)).to_owned();
                if p.get("extensions")
                    .and_then(|e| e.get("KHR_draco_mesh_compression"))
                    .is_some()
                {
                    mode.push_str(" (Draco-compressed)");
                }
                rows.push(
                    vec![
                        mi.to_string(),
                        s(m, "name").to_owned(),
                        pi.to_string(),
                        mode,
                        attrs,
                        count(p.get("attributes").and_then(|a| idx(a, "POSITION"))),
                        count(idx(p, "indices")),
                        idx(p, "material")
                            .map(|x| self.name("materials", x))
                            .unwrap_or_default(),
                        p.get("targets")
                            .map(|t| t.items().len().to_string())
                            .unwrap_or_default(),
                    ],
                    self.span(p),
                );
            }
        }
        rows.finish(out);
        let mut rows = Rows::new(
            "Materials",
            &[
                "Material",
                "Name",
                "Base colour",
                "Metallic",
                "Roughness",
                "Alpha mode",
                "Double-sided",
                "Textures",
            ],
        );
        for (i, m) in self.list("materials").iter().enumerate() {
            let pbr = m.get("pbrMetallicRoughness");
            let factor = |k: &str| {
                pbr.and_then(|p| p.get(k))
                    .and_then(num_of)
                    .map(num)
                    .unwrap_or_default()
            };
            let colour = floats(pbr.and_then(|p| p.get("baseColorFactor")));
            let mut textures = vec![];
            for (holder, key) in [
                (pbr, "baseColorTexture"),
                (pbr, "metallicRoughnessTexture"),
                (Some(m), "normalTexture"),
                (Some(m), "occlusionTexture"),
                (Some(m), "emissiveTexture"),
            ] {
                if let Some(t) = holder
                    .and_then(|h| h.get(key))
                    .and_then(|t| idx(t, "index"))
                {
                    textures.push(format!("{key} {t}"));
                }
            }
            rows.push(
                vec![
                    i.to_string(),
                    s(m, "name").to_owned(),
                    colour
                        .iter()
                        .map(|c| num(*c))
                        .collect::<Vec<_>>()
                        .join(", "),
                    factor("metallicFactor"),
                    factor("roughnessFactor"),
                    s(m, "alphaMode").to_owned(),
                    m.get("doubleSided").map(Node::text).unwrap_or_default(),
                    textures.join(", "),
                ],
                self.span(m),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new("Textures", &["Texture", "Name", "Image", "Sampler"]);
        for (i, t) in self.list("textures").iter().enumerate() {
            rows.push(
                vec![
                    i.to_string(),
                    s(t, "name").to_owned(),
                    idx(t, "source")
                        .map(|x| self.name("images", x))
                        .unwrap_or_default(),
                    idx(t, "sampler").map(|x| x.to_string()).unwrap_or_default(),
                ],
                self.span(t),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new("Images", &["Image", "Name", "MIME type", "Source"]);
        for (i, im) in self.list("images").iter().enumerate() {
            let uri = s(im, "uri");
            let source = if let Some(bv) = idx(im, "bufferView") {
                format!("embedded, buffer view {bv}")
            } else if uri.starts_with("data:") {
                "embedded data URI".into()
            } else if !uri.is_empty() {
                format!("external file “{}” (not opened)", clip(uri, 200))
            } else {
                "none".into()
            };
            let mime = if s(im, "mimeType").is_empty() && uri.starts_with("data:") {
                uri[5..].split([';', ',']).next().unwrap_or("").to_owned()
            } else {
                s(im, "mimeType").to_owned()
            };
            rows.push(
                vec![i.to_string(), s(im, "name").to_owned(), mime, source],
                self.span(im),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new("Cameras", &["Camera", "Name", "Type", "Parameters"]);
        for (i, c) in self.list("cameras").iter().enumerate() {
            let kind = s(c, "type");
            let params = c
                .get(kind)
                .and_then(|p| match &p.value {
                    Value::Object(f) => Some(
                        f.iter()
                            .map(|(k, v)| format!("{k} {}", v.text()))
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            rows.push(
                vec![
                    i.to_string(),
                    s(c, "name").to_owned(),
                    kind.to_owned(),
                    params,
                ],
                self.span(c),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new(
            "Animations",
            &[
                "Animation",
                "Name",
                "Channels",
                "Samplers",
                "Targets",
                "Duration (s)",
            ],
        );
        for (i, an) in self.list("animations").iter().enumerate() {
            let channels = an.get("channels").map(Node::items).unwrap_or(&[]);
            let mut targets: Vec<String> = channels
                .iter()
                .filter_map(|ch| ch.get("target"))
                .map(|t| {
                    let node = idx(t, "node")
                        .map(|n| self.name("nodes", n))
                        .unwrap_or_default();
                    format!("{node}.{}", s(t, "path"))
                })
                .collect();
            targets.sort();
            targets.dedup();
            let samplers = an.get("samplers").map(Node::items).unwrap_or(&[]);
            let duration = samplers
                .iter()
                .filter_map(|sm| idx(sm, "input"))
                .filter_map(|a| accessors.get(a))
                .filter_map(|a| floats(a.get("max")).first().copied())
                .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
            rows.push(
                vec![
                    i.to_string(),
                    s(an, "name").to_owned(),
                    channels.len().to_string(),
                    samplers.len().to_string(),
                    clip(&targets.join(", "), 200),
                    duration.map(num).unwrap_or_default(),
                ],
                self.span(an),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new("Skins", &["Skin", "Name", "Joints", "Skeleton root"]);
        for (i, sk) in self.list("skins").iter().enumerate() {
            rows.push(
                vec![
                    i.to_string(),
                    s(sk, "name").to_owned(),
                    sk.get("joints")
                        .map(|j| j.items().len().to_string())
                        .unwrap_or_default(),
                    idx(sk, "skeleton")
                        .map(|n| self.name("nodes", n))
                        .unwrap_or_default(),
                ],
                self.span(sk),
            );
        }
        rows.finish(out);
        let mut rows = Rows::new(
            "Accessors",
            &[
                "Accessor",
                "Type",
                "Component",
                "Count",
                "Buffer view",
                "Min",
                "Max",
            ],
        );
        for (i, a) in accessors.iter().enumerate() {
            let comp = match idx(a, "componentType") {
                Some(5120) => "byte",
                Some(5121) => "unsigned byte",
                Some(5122) => "short",
                Some(5123) => "unsigned short",
                Some(5125) => "unsigned int",
                Some(5126) => "float",
                _ => "?",
            };
            let show = |k: &str| {
                floats(a.get(k))
                    .iter()
                    .map(|v| num(*v))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            rows.push(
                vec![
                    i.to_string(),
                    s(a, "type").to_owned(),
                    comp.to_owned(),
                    idx(a, "count").map(|c| c.to_string()).unwrap_or_default(),
                    idx(a, "bufferView")
                        .map(|c| c.to_string())
                        .unwrap_or_default(),
                    show("min"),
                    show("max"),
                ],
                self.span(a),
            );
        }
        rows.finish(out);
    }

    /// The bytes of a buffer view, if its buffer is available and it lies inside it.
    fn view(&self, bv: usize) -> Result<(&[u8], usize), String> {
        let views = self.list("bufferViews");
        let v = views
            .get(bv)
            .ok_or(format!("buffer view {bv} does not exist"))?;
        if v.get("extensions")
            .and_then(|e| e.get("EXT_meshopt_compression"))
            .is_some()
        {
            return Err("meshopt-compressed buffer views are not decoded".into());
        }
        let buffer = idx(v, "buffer").ok_or(format!("buffer view {bv} has no buffer"))?;
        let start = idx(v, "byteOffset").unwrap_or(0);
        let len = idx(v, "byteLength").unwrap_or(0);
        let stride = idx(v, "byteStride").unwrap_or(0);
        let data = match self.buffers.get(buffer) {
            Some(Some(d)) => d,
            Some(None) => {
                return Err(format!(
                    "buffer {buffer} is not available (external or missing)"
                ));
            }
            None => return Err(format!("buffer {buffer} does not exist")),
        };
        let end = start
            .checked_add(len)
            .filter(|&e| e <= data.len())
            .ok_or(format!("buffer view {bv} runs past buffer {buffer}"))?;
        Ok((&data[start..end], stride))
    }

    /// Elements of an accessor as f64 components: (values, components per element).
    fn read(&self, a: usize, allowed: &[&str]) -> Result<(Vec<f64>, usize), String> {
        let acc = self
            .list("accessors")
            .get(a)
            .ok_or(format!("accessor {a} does not exist"))?;
        let ty = s(acc, "type");
        if !allowed.contains(&ty) {
            return Err(format!("accessor {a} has type {ty}"));
        }
        let comps = match ty {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return Err(format!("accessor {a} has type {ty}")),
        };
        let ct = idx(acc, "componentType").unwrap_or(0);
        let size = match ct {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(format!("accessor {a} has component type {ct}")),
        };
        let count = idx(acc, "count").unwrap_or(0);
        if count > MAX_ACCESSOR {
            return Err(format!(
                "accessor {a} has more than {MAX_ACCESSOR} elements"
            ));
        }
        let normalized = acc.get("normalized").map(Node::text).as_deref() == Some("true");
        let offset = idx(acc, "byteOffset").unwrap_or(0);
        let Some(bv) = idx(acc, "bufferView") else {
            // No buffer view: all zeros (sparse values are not applied).
            return Ok((vec![0.0; count * comps], comps));
        };
        let (bytes, stride) = self.view(bv)?;
        let elem = size * comps;
        let stride = if stride == 0 { elem } else { stride };
        let need = if count == 0 {
            0
        } else {
            offset + (count - 1) * stride + elem
        };
        if need > bytes.len() {
            return Err(format!("accessor {a} runs past buffer view {bv}"));
        }
        let mut values = Vec::with_capacity(count * comps);
        for i in 0..count {
            let base = offset + i * stride;
            for k in 0..comps {
                let at = base + k * size;
                let b = &bytes[at..at + size];
                let v = match ct {
                    5120 => {
                        let x = f64::from(b[0] as i8);
                        if normalized { (x / 127.0).max(-1.0) } else { x }
                    }
                    5121 => {
                        let x = f64::from(b[0]);
                        if normalized { x / 255.0 } else { x }
                    }
                    5122 => {
                        let x = f64::from(i16::from_le_bytes([b[0], b[1]]));
                        if normalized {
                            (x / 32767.0).max(-1.0)
                        } else {
                            x
                        }
                    }
                    5123 => {
                        let x = f64::from(u16::from_le_bytes([b[0], b[1]]));
                        if normalized { x / 65535.0 } else { x }
                    }
                    5125 => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    _ => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                };
                values.push(v);
            }
        }
        Ok((values, comps))
    }

    fn positions(&mut self, a: usize) -> Result<Rc<Vec<[f32; 3]>>, String> {
        if let Some(p) = self.positions.get(&a) {
            return Ok(p.clone());
        }
        let (v, _) = self.read(a, &["VEC3"])?;
        let p: Rc<Vec<[f32; 3]>> = Rc::new(
            v.chunks_exact(3)
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect(),
        );
        self.positions.insert(a, p.clone());
        Ok(p)
    }
    fn indices(&mut self, a: usize) -> Result<Rc<Vec<u32>>, String> {
        if let Some(p) = self.indices.get(&a) {
            return Ok(p.clone());
        }
        let (v, _) = self.read(a, &["SCALAR"])?;
        let p: Rc<Vec<u32>> = Rc::new(v.into_iter().map(|x| x as u32).collect());
        self.indices.insert(a, p.clone());
        Ok(p)
    }

    fn pictures(&mut self, out: &mut Inspection) {
        let mut total = 0usize;
        let images = self.list("images");
        for (i, im) in images.iter().enumerate() {
            if out.visuals.len() >= MAX_PICTURES {
                break;
            }
            let uri = s(im, "uri");
            let bytes: Option<Vec<u8>> = if let Some(bv) = idx(im, "bufferView") {
                self.view(bv).ok().map(|(b, _)| b.to_vec())
            } else if uri.starts_with("data:") {
                data_uri(uri).map(|(_, b)| b)
            } else {
                None
            };
            let Some(bytes) = bytes.filter(|b| !b.is_empty()) else {
                continue;
            };
            if total + bytes.len() > MAX_PICTURE_BYTES {
                break;
            }
            total += bytes.len();
            let name = if s(im, "name").is_empty() {
                format!("Image {i}")
            } else {
                s(im, "name").to_owned()
            };
            out.visuals.push(Visual::Picture {
                bytes,
                caption: name,
            });
        }
    }

    fn geometry(
        &mut self,
        out: &mut Inspection,
        cancelled: &dyn Fn() -> bool,
        required: &[String],
    ) {
        let mut mesh = Mesh::new();
        let (mut skipped_modes, mut draco) = (0u64, 0u64);
        let mut errors: Vec<String> = vec![];
        let nodes = self.list("nodes");
        let scenes = self.list("scenes");
        let default = idx(self.root, "scene").unwrap_or(0);
        // (mesh, matrix) instances to draw.
        let mut instances: Vec<(usize, Mat)> = vec![];
        let local_only = scenes.is_empty();
        if let Some(scene) = scenes.get(default) {
            let mut stack: Vec<(usize, Mat, u16)> = scene
                .get("nodes")
                .map(|n| {
                    n.items()
                        .iter()
                        .rev()
                        .filter_map(|x| num_of(x).map(|v| (v as usize, IDENTITY, 0)))
                        .collect()
                })
                .unwrap_or_default();
            let mut visits = 0;
            while let Some((ni, parent, depth)) = stack.pop() {
                visits += 1;
                if visits > MAX_VISITS {
                    errors.push(format!("the scene has more than {MAX_VISITS} node visits"));
                    break;
                }
                let Some(node) = nodes.get(ni) else {
                    self.problem(format!("node {ni} does not exist"));
                    continue;
                };
                let m = mul(&parent, &local(node));
                if let Some(mi) = idx(node, "mesh") {
                    instances.push((mi, m));
                }
                if depth < 64 {
                    for child in node
                        .get("children")
                        .map(Node::items)
                        .unwrap_or(&[])
                        .iter()
                        .rev()
                    {
                        if let Some(c) = num_of(child) {
                            stack.push((c as usize, m, depth + 1));
                        }
                    }
                }
            }
        } else if local_only {
            instances = (0..self.list("meshes").len())
                .map(|m| (m, IDENTITY))
                .collect();
        } else {
            self.problem(format!("default scene {default} does not exist"));
        }
        for (n, (mi, m)) in instances.iter().enumerate() {
            if n % 256 == 0 && cancelled() {
                return;
            }
            let Some(mesh_node) = self.list("meshes").get(*mi) else {
                self.problem(format!("mesh {mi} does not exist"));
                continue;
            };
            for p in mesh_node.get("primitives").map(Node::items).unwrap_or(&[]) {
                let mode = idx(p, "mode").unwrap_or(4);
                if p.get("extensions")
                    .and_then(|e| e.get("KHR_draco_mesh_compression"))
                    .is_some()
                {
                    draco += 1;
                    continue;
                }
                if !matches!(mode, 4..=6) {
                    skipped_modes += 1;
                    continue;
                }
                let Some(pa) = p.get("attributes").and_then(|a| idx(a, "POSITION")) else {
                    continue;
                };
                let pos = match self.positions(pa) {
                    Ok(p) => p,
                    Err(e) => {
                        if errors.len() < 20 {
                            errors.push(e);
                        }
                        continue;
                    }
                };
                let ind: Rc<Vec<u32>> = match idx(p, "indices") {
                    Some(ia) => match self.indices(ia) {
                        Ok(i) => i,
                        Err(e) => {
                            if errors.len() < 20 {
                                errors.push(e);
                            }
                            continue;
                        }
                    },
                    None => Rc::new((0..pos.len() as u32).collect()),
                };
                let tf = |v: [f32; 3]| -> [f32; 3] {
                    let v = [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])];
                    [0, 1, 2].map(|r| {
                        (m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3]) as f32
                    })
                };
                let corner = |k: usize| -> Option<[f32; 3]> {
                    pos.get(*ind.get(k)? as usize).map(|&v| tf(v))
                };
                let tris = match mode {
                    4 => ind.len() / 3,
                    _ => ind.len().saturating_sub(2),
                };
                let mut bad = 0u64;
                for t in 0..tris {
                    let (a, b, c) = match mode {
                        4 => (3 * t, 3 * t + 1, 3 * t + 2),
                        5 if t % 2 == 0 => (t, t + 1, t + 2),
                        5 => (t + 1, t, t + 2),
                        _ => (0, t + 1, t + 2),
                    };
                    match (corner(a), corner(b), corner(c)) {
                        (Some(a), Some(b), Some(c)) => mesh.triangle(a, b, c),
                        _ => bad += 1,
                    }
                    if mesh.seen.is_multiple_of(1 << 20) && cancelled() {
                        return;
                    }
                }
                if bad > 0 {
                    self.problem(format!(
                        "{bad} triangles of mesh {mi} use vertex indices past the end"
                    ));
                }
            }
        }
        if draco > 0 {
            out.notes.push(format!(
                "{draco} Draco-compressed primitives are not decoded and not drawn."
            ));
        }
        if skipped_modes > 0 {
            out.notes.push(format!(
                "{skipped_modes} point and line primitives are not drawn."
            ));
        }
        if !errors.is_empty() {
            out.notes.push(format!(
                "Some geometry could not be read: {}.",
                clip(&errors.join("; "), 400)
            ));
        }
        let unsupported: Vec<&String> = required
            .iter()
            .filter(|e| {
                !matches!(
                    e.as_str(),
                    "KHR_mesh_quantization"
                        | "KHR_texture_transform"
                        | "KHR_materials_unlit"
                        | "KHR_texture_basisu"
                )
            })
            .collect();
        if !unsupported.is_empty() {
            out.notes.push(format!(
                "Required extensions that Isolinear does not apply: {}; the view may be incomplete.",
                unsupported.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        if mesh.seen > 0 {
            out.notes.push(if local_only {
                "The file has no scene: meshes are drawn in their own coordinates, without node transforms.".into()
            } else {
                "The 3D view shows the default scene's triangles with node transforms applied; glTF units are metres.".into()
            });
            mesh.finish(out, if local_only { None } else { Some("m") });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_and_transforms() {
        assert_eq!(base64("SGVsbG8=").unwrap(), b"Hello");
        assert_eq!(base64("SGVsbG8").unwrap(), b"Hello");
        assert!(base64("SGV*").is_none());
        let (mime, bytes) = data_uri("data:application/octet-stream;base64,AAEC").unwrap();
        assert_eq!(
            (mime.as_str(), bytes.as_slice()),
            ("application/octet-stream", &[0u8, 1, 2][..])
        );
        let n = jsondoc::parse(
            r#"{"translation":[1,2,3],"rotation":[0,0,0.7071068,0.7071068],"scale":[2,2,2]}"#,
        )
        .unwrap();
        let m = local(&n);
        // (1,0,0) scaled to (2,0,0), turned 90° about z to (0,2,0), moved to (1,4,3).
        let p = [0, 1, 2].map(|r| m[r][0] + m[r][3]);
        assert!(
            (p[0] - 1.0).abs() < 1e-6 && (p[1] - 4.0).abs() < 1e-6 && (p[2] - 3.0).abs() < 1e-6
        );
    }
}
