//! Protobuf messages shown without their schema, as `protoc --decode_raw` would: JSON whose keys
//! are field numbers. Varints are numbers, fixed-width fields are hex with their float reading,
//! and length-delimited fields are guessed: a nested message, text, or bytes. Repeated fields
//! become lists in the order they appear.
use super::MAX_NODES;
use super::json_out::{JsonOut, bytes_label};

/// Nested messages tried at most this deep (each level is a guess).
const MAX_DEPTH: usize = 64;

enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
    Group(Vec<(u64, Value<'a>)>),
}

fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *b.get(*at)?;
        *at += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Fields of a message that fills `b` exactly; `None` when it is not one. `end_group` is the
/// field number of an open group, which ends at its END_GROUP tag.
fn message<'a>(
    b: &'a [u8],
    at: &mut usize,
    depth: usize,
    nodes: &mut usize,
    end_group: Option<u64>,
) -> Option<Vec<(u64, Value<'a>)>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut fields = vec![];
    while *at < b.len() {
        let key = varint(b, at)?;
        let (field, wire) = (key >> 3, key & 7);
        if field == 0 || field > (1 << 29) - 1 {
            return None;
        }
        *nodes += 1;
        if *nodes > MAX_NODES {
            return None;
        }
        let value = match wire {
            0 => Value::Varint(varint(b, at)?),
            1 => {
                let s = b.get(*at..*at + 8)?;
                *at += 8;
                Value::Fixed64(u64::from_le_bytes(s.try_into().ok()?))
            }
            5 => {
                let s = b.get(*at..*at + 4)?;
                *at += 4;
                Value::Fixed32(u32::from_le_bytes(s.try_into().ok()?))
            }
            2 => {
                let n = usize::try_from(varint(b, at)?).ok()?;
                let end = at.checked_add(n).filter(|&e| e <= b.len())?;
                let s = &b[*at..end];
                *at = end;
                Value::Bytes(s)
            }
            3 => Value::Group(message(b, at, depth + 1, nodes, Some(field))?),
            4 => {
                return (end_group == Some(field)).then_some(fields);
            }
            _ => return None,
        };
        fields.push((field, value));
    }
    // A group must end with its END_GROUP tag, not with the data.
    end_group.is_none().then_some(fields)
}

/// Mostly printable UTF-8 (letters, digits, punctuation, white space).
fn printable(b: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(b).ok()?;
    let bad = s
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        .count();
    (bad == 0).then_some(s)
}

struct Writer {
    out: JsonOut,
    nodes: usize,
}

impl Writer {
    fn fields(&mut self, fields: &[(u64, Value)], depth: usize) -> Result<(), String> {
        self.out.check()?;
        if fields.is_empty() {
            self.out.raw("{}");
            return Ok(());
        }
        // Field numbers in order of first appearance; repeated ones as lists.
        let mut order: Vec<u64> = vec![];
        for (f, _) in fields {
            if !order.contains(f) {
                order.push(*f);
            }
        }
        self.out.raw("{");
        for (i, f) in order.iter().enumerate() {
            if i > 0 {
                self.out.raw(",");
            }
            self.out.newline(depth + 1);
            self.out.string(&f.to_string());
            self.out.raw(": ");
            let values: Vec<&Value> = fields
                .iter()
                .filter(|(n, _)| n == f)
                .map(|(_, v)| v)
                .collect();
            if values.len() == 1 {
                self.value(values[0], depth + 1)?;
            } else {
                self.out.raw("[");
                for (j, v) in values.iter().enumerate() {
                    if j > 0 {
                        self.out.raw(",");
                    }
                    self.out.newline(depth + 2);
                    self.value(v, depth + 2)?;
                }
                self.out.newline(depth + 1);
                self.out.raw("]");
            }
        }
        self.out.newline(depth);
        self.out.raw("}");
        Ok(())
    }

    fn value(&mut self, v: &Value, depth: usize) -> Result<(), String> {
        match v {
            // Negative int32/int64 values are ten-byte varints.
            Value::Varint(n) if *n >= 1 << 63 => self.out.raw(&(*n as i64).to_string()),
            Value::Varint(n) => self.out.raw(&n.to_string()),
            Value::Fixed64(n) => self
                .out
                .string(&format!("0x{n:016x} ({:?} as double)", f64::from_bits(*n))),
            Value::Fixed32(n) => self
                .out
                .string(&format!("0x{n:08x} ({:?} as float)", f32::from_bits(*n))),
            Value::Group(fields) => self.fields(fields, depth)?,
            Value::Bytes(b) => {
                // Text that happens to parse as a message is still text; a message that is not
                // printable is a message; otherwise text or bytes.
                let text = printable(b).filter(|s| !s.is_empty());
                let mut at = 0;
                let nested = if text.is_none() && !b.is_empty() {
                    message(b, &mut at, depth / 2 + 1, &mut self.nodes, None)
                } else {
                    None
                };
                match (text, nested) {
                    (Some(s), _) => self.out.string(s),
                    (None, Some(fields)) if !fields.is_empty() => self.fields(&fields, depth)?,
                    _ if b.is_empty() => self.out.raw("\"\""),
                    _ => self.out.string(&bytes_label(b)),
                }
            }
        }
        Ok(())
    }
}

/// Decode a binary Protobuf message to JSON text and a note on how it was shown.
pub fn to_json(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<(String, String), String> {
    if cancelled() {
        return Err("Cancelled".into());
    }
    let mut nodes = 0;
    let mut at = 0;
    let fields = message(bytes, &mut at, 0, &mut nodes, None)
        .filter(|f| !f.is_empty() || bytes.is_empty())
        .ok_or("The file is not a Protobuf message (its fields do not parse)")?;
    let mut w = Writer {
        out: JsonOut::new(),
        nodes,
    };
    w.fields(&fields, 0)?;
    w.out.raw("\n");
    Ok((
        w.out.text,
        "Decoded from Protobuf without its schema, read-only: keys are field numbers; nested messages, text and bytes are guessed".into(),
    ))
}
