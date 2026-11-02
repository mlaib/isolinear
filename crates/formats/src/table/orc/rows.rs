use super::super::avro::{check_output, display};
use super::*;

type Row = Vec<Option<String>>;

pub(super) fn decode_stripe(
    file: &mut File,
    tail: &Tail,
    index: usize,
    from: u64,
    to: u64,
) -> Result<Vec<Row>, String> {
    let types = &tail.footer.types;
    let stripe = tail
        .footer
        .stripes
        .get(index)
        .ok_or("The ORC stripe does not exist")?;
    let limits = Limits::default();
    let sf = read_stripe_footer(file, tail, stripe, &limits)?;
    let data_end = stripe
        .offset
        .checked_add(stripe.index)
        .and_then(|n| n.checked_add(stripe.data))
        .ok_or_else(|| damaged("a stripe offset is too large"))?;
    let mut at = stripe.offset;
    let mut table = HashMap::new();
    for &(kind, column, len) in &sf.streams {
        let start = at;
        at = at
            .checked_add(len)
            .filter(|end| *end <= data_end)
            .ok_or_else(|| damaged("a stream runs past its stripe"))?;
        if column >= types.len() {
            return Err(damaged("a stream names a column that does not exist"));
        }
        if table.insert((column, kind), (start, len)).is_some() {
            return Err(damaged("a column has duplicate streams of the same kind"));
        }
    }
    let mut streams = Streams {
        file,
        table,
        codec: tail.codec,
        block: tail.block,
        budget: limits.batch_bytes as u64,
        read: 0,
    };
    let mut needed = vec![false; types.len()];
    let mut stack = vec![0];
    while let Some(col) = stack.pop() {
        if let Some(flag) = needed.get_mut(col)
            && !*flag
        {
            *flag = true;
            stack.extend(types.get(col).map_or(&[][..], |t| t.children.as_slice()));
        }
    }
    let mut readers = Vec::with_capacity(types.len());
    for (col, &needed) in needed.iter().enumerate() {
        readers.push(if needed {
            Some(Reader::new(&mut streams, types, &sf.encodings, col)?)
        } else {
            None
        });
    }
    let mut budget = Budget {
        items: ITEMS_BASE.saturating_add(streams.read.saturating_mul(ITEMS_PER_BYTE)),
        text: 0,
        cap: limits.batch_bytes,
    };
    let root_struct = types.first().is_some_and(|t| t.kind == Kind::Struct);
    let mut rows = vec![];
    for row in 0..to {
        budget.item()?;
        let present = if root_struct {
            readers
                .first_mut()
                .and_then(Option::as_mut)
                .ok_or("The root ORC column is missing")?
                .present()?
        } else {
            true
        };
        let mut cells = vec![];
        for (name, col) in &tail.roots {
            let value = if present {
                cell(&mut readers, *col, &mut budget)
                    .map_err(|e| format!("{e} (column {name}, row {row} of the stripe)"))?
            } else {
                None
            };
            if row >= from {
                budget.text(value.as_ref().map_or(0, String::len))?;
                cells.push(value);
            }
        }
        if row >= from {
            rows.push(cells);
        }
    }
    Ok(rows)
}

fn cell(
    readers: &mut [Option<Reader>],
    col: usize,
    b: &mut Budget,
) -> Result<Option<String>, String> {
    enum Task {
        Value(usize, bool),
        List(usize, usize, usize),
        Map(usize, usize, usize, usize),
        Struct(usize, usize),
        Literal(&'static str),
    }
    let mut tasks = vec![Task::Value(col, false)];
    let mut out = Text::default();
    let mut null = false;
    while let Some(task) = tasks.pop() {
        match task {
            Task::Literal(text) => out.push_str(text),
            Task::List(child, remaining, index) => {
                if remaining == 0 {
                    out.push(']');
                    continue;
                }
                if index > 0 {
                    out.push_str(", ");
                }
                tasks.push(Task::List(child, remaining - 1, index + 1));
                tasks.push(Task::Value(child, true));
            }
            Task::Map(keys, values, remaining, index) => {
                if remaining == 0 {
                    out.push('}');
                    continue;
                }
                if index > 0 {
                    out.push_str(", ");
                }
                tasks.push(Task::Map(keys, values, remaining - 1, index + 1));
                tasks.push(Task::Value(values, true));
                tasks.push(Task::Literal(": "));
                tasks.push(Task::Value(keys, true));
            }
            Task::Struct(col, index) => {
                let reader = readers
                    .get(col)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| damaged("a struct column is missing"))?;
                let Body::Struct { names, fields } = &reader.body else {
                    return Err(damaged("expected a struct column"));
                };
                if let Some(&child) = fields.get(index) {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    display::quote(names.get(index).map_or("?", String::as_str), &mut out);
                    out.push_str(": ");
                    tasks.push(Task::Struct(col, index + 1));
                    tasks.push(Task::Value(child, true));
                } else {
                    out.push('}');
                }
            }
            Task::Value(col, nested) => {
                b.item()?;
                let reader = readers
                    .get_mut(col)
                    .and_then(Option::as_mut)
                    .ok_or_else(|| damaged("a column reader is missing"))?;
                if !reader.present()? {
                    if !nested {
                        null = true;
                    }
                    out.push_str("null");
                    continue;
                }
                let kind = reader.kind;
                match &mut reader.body {
                    Body::Bool(d) => out.push_str(if d.next()? { "true" } else { "false" }),
                    Body::Byte(d) => {
                        let _ = write!(out, "{}", d.next()? as i8);
                    }
                    Body::Int(d) => {
                        let v = d.next()?;
                        if (kind == Kind::Short && i16::try_from(v).is_err())
                            || (kind == Kind::Int && i32::try_from(v).is_err())
                        {
                            return Err(damaged(
                                "an integer value is outside its declared type's range",
                            ));
                        }
                        let _ = write!(out, "{v}");
                    }
                    Body::Float(d) => {
                        let v = f32::from_le_bytes(d.take(4)?.try_into().map_err(|_| ended())?);
                        if nested {
                            let _ = write!(out, "{v}");
                        } else {
                            let _ = write!(out, "{v:?}");
                        }
                    }
                    Body::Double(d) => {
                        let v = f64::from_le_bytes(d.take(8)?.try_into().map_err(|_| ended())?);
                        if nested {
                            let _ = write!(out, "{v}");
                        } else {
                            let _ = write!(out, "{v:?}");
                        }
                    }
                    body @ (Body::Direct { .. } | Body::Dict { .. }) => {
                        let binary = matches!(body, Body::Direct { binary: true, .. });
                        let bytes = body.bytes()?;
                        if binary {
                            display::hex(bytes, &mut out, nested);
                        } else {
                            display::utf8(bytes, &mut out, nested);
                        }
                    }
                    Body::Decimal {
                        data,
                        scales,
                        scale,
                    } => decimal(decimal_varint(data)?, scales.next()?, *scale, &mut out)?,
                    Body::Date(d) => {
                        if nested {
                            out.push('"');
                        }
                        out.push_str(&date_text(d.next()?));
                        if nested {
                            out.push('"');
                        }
                    }
                    Body::Timestamp {
                        secs,
                        nanos,
                        instant,
                    } => {
                        if nested {
                            out.push('"');
                        }
                        timestamp(secs.next()?, nanos.next()? as u64, *instant, &mut out)?;
                        if nested {
                            out.push('"');
                        }
                    }
                    Body::List { lengths, child } => {
                        let n = lengths.index()?;
                        if n as u64 > b.items {
                            return Err(damaged("too many values in one stripe"));
                        }
                        out.push('[');
                        tasks.push(Task::List(*child, n, 0));
                    }
                    Body::Map {
                        lengths,
                        keys,
                        values,
                    } => {
                        let n = lengths.index()?;
                        if (n as u64).checked_mul(2).is_none_or(|n| n > b.items) {
                            return Err(damaged("too many values in one stripe"));
                        }
                        out.push('{');
                        tasks.push(Task::Map(*keys, *values, n, 0));
                    }
                    Body::Struct { .. } => {
                        out.push('{');
                        tasks.push(Task::Struct(col, 0));
                    }
                    Body::Union { tags, branches } => {
                        let tag = usize::from(tags.next()?);
                        tasks.push(Task::Value(
                            *branches
                                .get(tag)
                                .ok_or_else(|| damaged("a union tag does not exist"))?,
                            nested,
                        ));
                    }
                }
            }
        }
        // This counts consumed text, not retained text; malformed expansions are bounded too.
        check_output(out.len())?;
    }
    Ok((!null).then(|| out.finish()))
}
