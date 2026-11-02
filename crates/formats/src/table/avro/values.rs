use super::display::quote;
use super::*;

pub(super) fn cell(c: &mut Cursor<'_>, mut s: &S, t: &Types) -> Result<Option<String>, String> {
    let mut depth = 0;
    while let S::Union(branches) = s {
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(damaged("values are nested too deeply"));
        }
        s = c.branch(branches)?;
    }
    if matches!(s, S::Null) {
        c.count(1)?;
        return Ok(None);
    }
    let mut out = Text::default();
    if matches!(s, S::Bytes) {
        super::display::hex(c.bytes()?, &mut out, false);
    } else {
        text(c, s, t, &mut out, depth)?;
    }
    Ok(Some(out.finish()))
}

fn text(c: &mut Cursor<'_>, s: &S, t: &Types, out: &mut Text, depth: usize) -> Result<(), String> {
    enum Task<'a> {
        Value(&'a S, bool, usize),
        Record(&'a [(String, S)], usize, usize),
        Collection {
            schema: &'a S,
            map: bool,
            remaining: u64,
            first: bool,
            depth: usize,
            end: Option<usize>,
        },
    }
    let mut stack = vec![Task::Value(s, false, depth)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Record(fields, index, depth) => {
                if let Some((name, schema)) = fields.get(index) {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    quote(name, out);
                    out.push_str(": ");
                    stack.push(Task::Record(fields, index + 1, depth));
                    stack.push(Task::Value(schema, true, depth + 1));
                } else {
                    out.push('}');
                }
            }
            Task::Collection {
                schema,
                map,
                mut remaining,
                mut first,
                depth,
                mut end,
            } => {
                if remaining == 0 {
                    if end.is_some_and(|end| end != c.i) {
                        return Err(damaged(
                            "an array or map block does not match its declared size",
                        ));
                    }
                    (remaining, end) = c.block()?;
                    if remaining == 0 {
                        out.push(if map { '}' } else { ']' });
                        continue;
                    }
                }
                if !first {
                    out.push_str(", ");
                }
                first = false;
                if map {
                    super::display::utf8(c.bytes()?, out, true);
                    out.push_str(": ");
                }
                stack.push(Task::Collection {
                    schema,
                    map,
                    remaining: remaining - 1,
                    first,
                    depth,
                    end,
                });
                stack.push(Task::Value(schema, true, depth + 1));
            }
            Task::Value(s, nested, depth) => {
                if depth > MAX_DEPTH {
                    return Err(damaged("values are nested too deeply"));
                }
                c.count(1)?;
                match s {
                    S::Null => out.push_str("null"),
                    S::Bool => out.push_str(if c.bool()? { "true" } else { "false" }),
                    S::Int => {
                        let _ = write!(out, "{}", c.int()?);
                    }
                    S::Long => {
                        let _ = write!(out, "{}", c.long()?);
                    }
                    S::Float => {
                        let v = c.float()?;
                        if nested {
                            let _ = write!(out, "{v}");
                        } else {
                            let _ = write!(out, "{v:?}");
                        }
                    }
                    S::Double => {
                        let v = c.double()?;
                        if nested {
                            let _ = write!(out, "{v}");
                        } else {
                            let _ = write!(out, "{v:?}");
                        }
                    }
                    S::Bytes => hex(c.bytes()?, out),
                    S::Str => super::display::utf8(c.bytes()?, out, nested),
                    S::Named(i) => match t
                        .named
                        .get(*i)
                        .ok_or_else(|| damaged("a named type does not exist"))?
                    {
                        Named::Record { fields, .. } => {
                            out.push('{');
                            stack.push(Task::Record(fields, 0, depth));
                        }
                        Named::Enum { symbols, .. } => {
                            let symbol = usize::try_from(c.long()?)
                                .ok()
                                .and_then(|i| symbols.get(i))
                                .ok_or_else(|| damaged("an enum symbol does not exist"))?;
                            if nested {
                                quote(symbol, out);
                            } else {
                                out.push_str(symbol);
                            }
                        }
                        Named::Fixed { size, .. } => hex(c.take(*size)?, out),
                    },
                    S::Array(schema) | S::Map(schema) => {
                        let map = matches!(s, S::Map(_));
                        out.push(if map { '{' } else { '[' });
                        stack.push(Task::Collection {
                            schema,
                            map,
                            remaining: 0,
                            first: true,
                            depth,
                            end: None,
                        });
                    }
                    S::Union(branches) => {
                        stack.push(Task::Value(c.branch(branches)?, nested, depth + 1))
                    }
                    S::Logical(base, l) => match (l, base.as_ref()) {
                        (Logical::Date, _) => out.push_str(&date(i64::from(c.int()?))),
                        (Logical::TimeMillis, _) => {
                            out.push_str(&time_of_day(i64::from(c.int()?), 1000))
                        }
                        (Logical::TimeMicros, _) => {
                            out.push_str(&time_of_day(c.long()?, 1_000_000))
                        }
                        (Logical::Timestamp(unit), _) => out.push_str(&timestamp(c.long()?, *unit)),
                        (Logical::Decimal(scale), S::Named(i)) => match t.named.get(*i) {
                            Some(Named::Fixed { size, .. }) => decimal(c.take(*size)?, *scale, out),
                            _ => return Err(damaged("a decimal is not bytes or fixed")),
                        },
                        (Logical::Decimal(scale), _) => decimal(c.bytes()?, *scale, out),
                    },
                }
            }
        }
    }
    Ok(())
}

// Gregorian date conversion from the previous reader's UTC formatter.
pub(super) fn utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86400);
    let secs = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}
