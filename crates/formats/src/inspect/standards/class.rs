//! Java class files (JVM specification, chapter 4): Java version, constant pool, the class and
//! its super class and interfaces, fields and methods with readable types and modifiers, and
//! the simple attributes (source file, annotation names). Never loaded or run. The magic
//! 0xCAFEBABE is shared with universal Mach-O binaries, which have a small architecture count
//! where class files have their version (major 45 or more).
use super::util::{Bytes, bytes_text, clip, flags, printable, span};
use crate::inspect::{Handler, Input, Inspection, MAX_ROWS, Probe, Row, Stage, State, Table};

pub struct JavaClass;

/// Whether the first bytes are a class file rather than a universal Mach-O binary.
pub fn is_class(head: &[u8]) -> bool {
    let b = Bytes::new(head, false);
    b.u32(0) == Some(0xcafe_babe)
        && b.u16(6).is_some_and(|major| (45..=200).contains(&major))
        && b.u32(4).is_some_and(|w| w >= 45)
        && b.u16(8).is_some_and(|count| count > 0)
}

fn java_version(major: u16, minor: u16) -> String {
    match major {
        45 if minor >= 3 => "Java 1.1".into(),
        45 => "Java 1.0".into(),
        46..=48 => format!("Java 1.{}", major - 44),
        49.. => {
            let v = major - 44;
            if minor == 0xffff {
                format!("Java {v} (preview features)")
            } else {
                format!("Java {v}")
            }
        }
        _ => "unknown".into(),
    }
}

#[derive(Clone, Debug)]
enum Const {
    Empty,
    Utf8(String),
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    Str(u16),
    Ref(u8, u16, u16),
    NameType(u16, u16),
    Handle(u8, u16),
    MethodType(u16),
    Dynamic(u8, u16, u16),
    Module(u16),
    Package(u16),
}

struct Pool {
    items: Vec<(Const, usize, usize)>,
}
impl Pool {
    fn utf8(&self, i: u16) -> String {
        match self.items.get(i as usize).map(|c| &c.0) {
            Some(Const::Utf8(s)) => s.clone(),
            _ => String::new(),
        }
    }
    fn class(&self, i: u16) -> String {
        match self.items.get(i as usize).map(|c| &c.0) {
            Some(Const::Class(n)) => self.utf8(*n).replace('/', "."),
            _ => String::new(),
        }
    }
    /// Readable value of an entry, one level of references resolved.
    fn show(&self, c: &Const, depth: u8) -> String {
        let at = |i: u16| -> String {
            if depth > 3 {
                return format!("#{i}");
            }
            self.items
                .get(i as usize)
                .map(|e| self.show(&e.0, depth + 1))
                .unwrap_or_default()
        };
        match c {
            Const::Empty => String::new(),
            Const::Utf8(s) => clip(s, 300),
            Const::Int(v) => v.to_string(),
            Const::Float(v) => v.to_string(),
            Const::Long(v) => v.to_string(),
            Const::Double(v) => v.to_string(),
            Const::Class(n) => self.utf8(*n).replace('/', "."),
            Const::Str(n) => format!("\"{}\"", clip(&self.utf8(*n), 300)),
            Const::Ref(_, c, nt) => format!("{}.{}", at(*c), at(*nt)),
            Const::NameType(n, d) => format!("{}:{}", self.utf8(*n), self.utf8(*d)),
            Const::Handle(k, r) => format!("kind {k} {}", at(*r)),
            Const::MethodType(d) => self.utf8(*d),
            Const::Dynamic(_, b, nt) => format!("bootstrap {b} {}", at(*nt)),
            Const::Module(n) | Const::Package(n) => self.utf8(*n),
        }
    }
}

fn tag_name(c: &Const) -> &'static str {
    match c {
        Const::Empty => "",
        Const::Utf8(_) => "Utf8",
        Const::Int(_) => "Integer",
        Const::Float(_) => "Float",
        Const::Long(_) => "Long",
        Const::Double(_) => "Double",
        Const::Class(_) => "Class",
        Const::Str(_) => "String",
        Const::Ref(9, ..) => "Fieldref",
        Const::Ref(10, ..) => "Methodref",
        Const::Ref(..) => "InterfaceMethodref",
        Const::NameType(..) => "NameAndType",
        Const::Handle(..) => "MethodHandle",
        Const::MethodType(_) => "MethodType",
        Const::Dynamic(17, ..) => "Dynamic",
        Const::Dynamic(..) => "InvokeDynamic",
        Const::Module(_) => "Module",
        Const::Package(_) => "Package",
    }
}

/// Modified UTF-8 (class files): NUL as two bytes, supplementary characters as surrogates.
fn modified_utf8(b: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            units.push(u16::from(c));
            i += 1;
        } else if c & 0xe0 == 0xc0 && i + 1 < b.len() {
            units.push((u16::from(c & 0x1f) << 6) | u16::from(b[i + 1] & 0x3f));
            i += 2;
        } else if c & 0xf0 == 0xe0 && i + 2 < b.len() {
            units.push(
                (u16::from(c & 0x0f) << 12)
                    | (u16::from(b[i + 1] & 0x3f) << 6)
                    | u16::from(b[i + 2] & 0x3f),
            );
            i += 3;
        } else {
            units.push(0xfffd);
            i += 1;
        }
    }
    printable(&String::from_utf16_lossy(&units))
}

/// One field type from a descriptor at `*i`: "I" -> "int", "[Ljava/lang/String;" -> "java.lang.String[]".
fn field_type(d: &[u8], i: &mut usize) -> Option<String> {
    let mut dims = 0;
    while d.get(*i) == Some(&b'[') {
        dims += 1;
        *i += 1;
        if dims > 255 {
            return None;
        }
    }
    let c = *d.get(*i)?;
    *i += 1;
    let base = match c {
        b'B' => "byte".into(),
        b'C' => "char".into(),
        b'D' => "double".into(),
        b'F' => "float".into(),
        b'I' => "int".into(),
        b'J' => "long".into(),
        b'S' => "short".into(),
        b'Z' => "boolean".into(),
        b'V' => "void".into(),
        b'L' => {
            let end = d.get(*i..)?.iter().position(|&x| x == b';')? + *i;
            let name = String::from_utf8_lossy(&d[*i..end]).replace('/', ".");
            *i = end + 1;
            name
        }
        _ => return None,
    };
    Some(format!("{base}{}", "[]".repeat(dims)))
}

/// A field or method descriptor as Java: "(ILjava/lang/String;)V" -> "void (int, java.lang.String)".
pub fn descriptor(d: &str) -> String {
    let b = d.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'(') {
        i = 1;
        let mut params = vec![];
        while b.get(i).is_some_and(|&c| c != b')') {
            match field_type(b, &mut i) {
                Some(t) => params.push(t),
                None => return d.to_owned(),
            }
            if params.len() > 255 {
                return d.to_owned();
            }
        }
        i += 1;
        let ret = field_type(b, &mut i).unwrap_or_default();
        return format!("{ret} ({})", params.join(", "));
    }
    field_type(b, &mut i).unwrap_or_else(|| d.to_owned())
}

const CLASS_FLAGS: &[(u64, &str)] = &[
    (0x0001, "public"),
    (0x0010, "final"),
    (0x0020, "super"),
    (0x0200, "interface"),
    (0x0400, "abstract"),
    (0x1000, "synthetic"),
    (0x2000, "annotation"),
    (0x4000, "enum"),
    (0x8000, "module"),
];
const FIELD_FLAGS: &[(u64, &str)] = &[
    (0x0001, "public"),
    (0x0002, "private"),
    (0x0004, "protected"),
    (0x0008, "static"),
    (0x0010, "final"),
    (0x0040, "volatile"),
    (0x0080, "transient"),
    (0x1000, "synthetic"),
    (0x4000, "enum"),
];
const METHOD_FLAGS: &[(u64, &str)] = &[
    (0x0001, "public"),
    (0x0002, "private"),
    (0x0004, "protected"),
    (0x0008, "static"),
    (0x0010, "final"),
    (0x0020, "synchronized"),
    (0x0040, "bridge"),
    (0x0080, "varargs"),
    (0x0100, "native"),
    (0x0400, "abstract"),
    (0x0800, "strictfp"),
    (0x1000, "synthetic"),
];

/// Skips one annotation element value; false when damaged or too deep.
fn skip_value(b: &Bytes, pos: &mut usize, depth: u8) -> bool {
    if depth > 16 {
        return false;
    }
    let Some(tag) = b.u8(*pos) else {
        return false;
    };
    *pos += 1;
    match tag {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' | b'c' => {
            *pos += 2;
            true
        }
        b'e' => {
            *pos += 4;
            true
        }
        b'@' => skip_annotation(b, pos, depth + 1).is_some(),
        b'[' => {
            let Some(n) = b.u16(*pos) else {
                return false;
            };
            *pos += 2;
            (0..n).all(|_| skip_value(b, pos, depth + 1))
        }
        _ => false,
    }
}

/// Reads an annotation's type index and skips its elements.
fn skip_annotation(b: &Bytes, pos: &mut usize, depth: u8) -> Option<u16> {
    let ty = b.u16(*pos)?;
    let pairs = b.u16(*pos + 2)?;
    *pos += 4;
    for _ in 0..pairs {
        *pos += 2;
        if !skip_value(b, pos, depth) || *pos > b.b.len() {
            return None;
        }
    }
    Some(ty)
}

/// Attributes at `*pos`: (name, offset, length) each, and annotation names found.
/// Attributes as (name, content offset, length), and the annotation names found.
type Attributes = (Vec<(String, usize, usize)>, Vec<String>);

fn attributes(b: &Bytes, pos: &mut usize, pool: &Pool) -> Option<Attributes> {
    let count = b.u16(*pos)?;
    *pos += 2;
    let mut list = vec![];
    let mut annotations = vec![];
    for _ in 0..count {
        let name = pool.utf8(b.u16(*pos)?);
        let len = b.u32(*pos + 2)? as usize;
        let start = *pos + 6;
        b.get(start, len)?;
        if name == "RuntimeVisibleAnnotations" || name == "RuntimeInvisibleAnnotations" {
            let mut p = start + 2;
            for _ in 0..b.u16(start).unwrap_or(0).min(256) {
                match skip_annotation(b, &mut p, 0) {
                    Some(t) if p <= start + len => {
                        annotations.push(format!("@{}", descriptor(&pool.utf8(t))))
                    }
                    _ => break,
                }
            }
        }
        list.push((name, start, len));
        *pos = start + len;
    }
    Some((list, annotations))
}

fn table(name: &str, cols: &[&str]) -> Table {
    Table {
        name: name.into(),
        columns: cols.iter().map(|c| (*c).to_owned()).collect(),
        ..Table::default()
    }
}

impl Handler for JavaClass {
    fn name(&self) -> &'static str {
        "Java class"
    }
    fn recognize(&self, probe: &Probe) -> bool {
        is_class(probe.head)
    }
    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let b = Bytes::new(input.bytes, false);
        if !is_class(input.bytes) {
            return Err("Not a Java class file".into());
        }
        let minor = b.u16(4).unwrap_or(0);
        let major = b.u16(6).unwrap_or(0);
        let mut out = Inspection::new("Java", "Java class file");
        out.version = Some(format!("{major}.{minor}"));
        out.fact(
            "Class file",
            "Format version",
            format!("{major}.{minor}"),
            span(4, 4),
        );
        out.fact(
            "Class file",
            "Java version",
            java_version(major, minor),
            span(4, 4),
        );
        // Constant pool (u16 count: bounded by the format).
        let count = b.u16(8).unwrap_or(0);
        let mut pool = Pool {
            items: vec![(Const::Empty, 0, 0)],
        };
        let mut pos = 10usize;
        let mut damaged: Option<String> = None;
        let mut i = 1u32;
        while i < u32::from(count) {
            let at = pos;
            let Some(tag) = b.u8(pos) else {
                damaged = Some(format!("the constant pool ends early (entry {i})"));
                break;
            };
            let u16_at = |o: usize| b.u16(at + o);
            let entry = match tag {
                1 => u16_at(1).and_then(|len| {
                    let s = b.get(at + 3, len as usize)?;
                    pos = at + 3 + len as usize;
                    Some(Const::Utf8(modified_utf8(s)))
                }),
                3 => b.u32(at + 1).map(|v| {
                    pos = at + 5;
                    Const::Int(v as i32)
                }),
                4 => b.u32(at + 1).map(|v| {
                    pos = at + 5;
                    Const::Float(f32::from_bits(v))
                }),
                5 => b.u64(at + 1).map(|v| {
                    pos = at + 9;
                    Const::Long(v as i64)
                }),
                6 => b.u64(at + 1).map(|v| {
                    pos = at + 9;
                    Const::Double(f64::from_bits(v))
                }),
                7 | 8 | 16 | 19 | 20 => u16_at(1).map(|v| {
                    pos = at + 3;
                    match tag {
                        7 => Const::Class(v),
                        8 => Const::Str(v),
                        16 => Const::MethodType(v),
                        19 => Const::Module(v),
                        _ => Const::Package(v),
                    }
                }),
                9..=12 | 17 | 18 => u16_at(1).zip(u16_at(3)).map(|(x, y)| {
                    pos = at + 5;
                    match tag {
                        12 => Const::NameType(x, y),
                        17 | 18 => Const::Dynamic(tag, x, y),
                        _ => Const::Ref(tag, x, y),
                    }
                }),
                15 => b.u8(at + 1).zip(u16_at(2)).map(|(k, r)| {
                    pos = at + 4;
                    Const::Handle(k, r)
                }),
                _ => None,
            };
            let Some(entry) = entry else {
                damaged = Some(format!(
                    "constant pool entry {i} at byte {at} (tag {tag}) is not readable"
                ));
                break;
            };
            let wide = matches!(entry, Const::Long(_) | Const::Double(_));
            pool.items.push((entry, at, pos - at));
            if wide {
                pool.items.push((Const::Empty, pos, 0));
                i += 1;
            }
            i += 1;
        }
        let mut cp = table("Constant pool", &["Index", "Kind", "Value"]);
        for (n, (c, at, len)) in pool.items.iter().enumerate().skip(1).take(MAX_ROWS) {
            if matches!(c, Const::Empty) {
                continue;
            }
            cp.rows.push(Row {
                cells: vec![format!("#{n}"), tag_name(c).into(), pool.show(c, 0)],
                source: span(*at, *len),
            });
        }
        out.fact(
            "Class file",
            "Constant pool entries",
            (pool.items.len() - 1).to_string(),
            span(8, pos - 8),
        );
        let mut fields_t = table("Fields", &["Name", "Type", "Modifiers", "Attributes"]);
        let mut methods_t = table(
            "Methods",
            &["Name", "Signature", "Modifiers", "Code size", "Attributes"],
        );
        if damaged.is_none()
            && let Err(e) = members(&b, &mut pos, &pool, &mut out, &mut fields_t, &mut methods_t)
        {
            damaged = Some(e);
        }
        for t in [fields_t, methods_t, cp] {
            if !t.rows.is_empty() {
                out.tables.push(t);
            }
        }
        match damaged {
            None => out.check(
                Stage::Parsed,
                State::Passed,
                "Read the whole class file (bytecode is not disassembled)",
            ),
            Some(e) => out.check(Stage::Parsed, State::Partial, format!("Damaged: {e}")),
        }
        out.not_validated();
        out.notes.push(
            "The class is never loaded or run; bytecode is not verified or disassembled.".into(),
        );
        Ok(out)
    }
}

fn members(
    b: &Bytes,
    pos: &mut usize,
    pool: &Pool,
    out: &mut Inspection,
    fields_t: &mut Table,
    methods_t: &mut Table,
) -> Result<(), String> {
    let cut = |what: &str| format!("the file ends inside the {what}");
    let access = b.u16(*pos).ok_or_else(|| cut("class header"))?;
    let this = b.u16(*pos + 2).ok_or_else(|| cut("class header"))?;
    let sup = b.u16(*pos + 4).ok_or_else(|| cut("class header"))?;
    let header = span(*pos, 6);
    out.fact("Class", "Name", pool.class(this), header.clone());
    out.fact(
        "Class",
        "Super class",
        if sup == 0 {
            "(none)".into()
        } else {
            pool.class(sup)
        },
        header.clone(),
    );
    out.fact(
        "Class",
        "Modifiers",
        flags(u64::from(access), CLASS_FLAGS),
        header,
    );
    let n = b.u16(*pos + 6).ok_or_else(|| cut("interfaces"))?;
    *pos += 8;
    let mut ifaces = vec![];
    for _ in 0..n {
        ifaces.push(pool.class(b.u16(*pos).ok_or_else(|| cut("interfaces"))?));
        *pos += 2;
    }
    out.fact("Class", "Interfaces", ifaces.join(", "), None);
    for (is_method, t) in [(false, fields_t), (true, methods_t)] {
        let what = if is_method { "methods" } else { "fields" };
        let count = b.u16(*pos).ok_or_else(|| cut(what))?;
        *pos += 2;
        out.fact(
            "Class",
            if is_method { "Methods" } else { "Fields" },
            count.to_string(),
            None,
        );
        for _ in 0..count {
            let at = *pos;
            let acc = b.u16(at).ok_or_else(|| cut(what))?;
            let name = pool.utf8(b.u16(at + 2).ok_or_else(|| cut(what))?);
            let desc = pool.utf8(b.u16(at + 4).ok_or_else(|| cut(what))?);
            *pos += 6;
            let (attrs, annotations) = attributes(b, pos, pool).ok_or_else(|| cut(what))?;
            let mut names: Vec<String> = attrs.iter().map(|a| a.0.clone()).collect();
            names.extend(annotations);
            let row = if is_method {
                let code = attrs
                    .iter()
                    .find(|a| a.0 == "Code")
                    .and_then(|a| b.u32(a.1 + 4))
                    .map(|l| bytes_text(u64::from(l)))
                    .unwrap_or_default();
                vec![
                    name,
                    descriptor(&desc),
                    flags(u64::from(acc), METHOD_FLAGS),
                    code,
                    names.join(", "),
                ]
            } else {
                vec![
                    name,
                    descriptor(&desc),
                    flags(u64::from(acc), FIELD_FLAGS),
                    names.join(", "),
                ]
            };
            if t.rows.len() < MAX_ROWS {
                t.rows.push(Row {
                    cells: row,
                    source: span(at, *pos - at),
                });
            }
        }
    }
    let (attrs, annotations) = attributes(b, pos, pool).ok_or_else(|| cut("class attributes"))?;
    for (name, start, len) in &attrs {
        match name.as_str() {
            "SourceFile" => {
                let v = b.u16(*start).map(|i| pool.utf8(i)).unwrap_or_default();
                out.fact("Class", "Source file", v, span(*start, *len));
            }
            "Signature" => {
                let v = b.u16(*start).map(|i| pool.utf8(i)).unwrap_or_default();
                out.fact("Class", "Generic signature", v, span(*start, *len));
            }
            "Module" => out.fact("Class", "Module", "module-info", span(*start, *len)),
            _ => {}
        }
    }
    out.fact(
        "Class",
        "Attributes",
        attrs
            .iter()
            .map(|a| a.0.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        None,
    );
    out.fact("Class", "Annotations", annotations.join(", "), None);
    if *pos != b.b.len() {
        out.notes.push(format!(
            "{} bytes follow the end of the class structure",
            b.b.len().saturating_sub(*pos)
        ));
    }
    Ok(())
}
