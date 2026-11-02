use super::*;

impl Types {
    pub(super) fn parse(&mut self, root: &Json) -> Result<S, String> {
        enum Task<'a> {
            Parse(&'a Json, String, usize),
            Array,
            Map,
            Union(usize),
            Record(usize, Vec<String>),
            Logical(&'a Json),
        }
        let mut tasks = vec![Task::Parse(root, String::new(), 0)];
        let mut values = Vec::new();
        let mut nodes = 0usize;
        while let Some(task) = tasks.pop() {
            match task {
                Task::Parse(j, ns, depth) => {
                    nodes += 1;
                    if nodes > 10_000 {
                        return Err("The Avro schema exceeds 10,000 type nodes".into());
                    }
                    if depth > MAX_DEPTH {
                        return Err("the schema is nested too deeply".into());
                    }
                    match j {
                        Json::String(name) => values.push(
                            primitive(name)
                                .or_else(|| self.lookup(name, &ns).map(S::Named))
                                .ok_or_else(|| format!("unknown type \"{name}\""))?,
                        ),
                        Json::Array(branches) => {
                            if branches.len() > 10_000 {
                                return Err("An Avro union exceeds 10,000 branches".into());
                            }
                            if branches.is_empty() {
                                return Err("a union has no branches".into());
                            }
                            tasks.push(Task::Union(branches.len()));
                            for branch in branches.iter().rev() {
                                tasks.push(Task::Parse(branch, ns.clone(), depth + 1));
                            }
                        }
                        Json::Object(_) => {
                            for key in ["name", "namespace"] {
                                if j.get(key)
                                    .and_then(Json::as_str)
                                    .is_some_and(|s| s.len() > 1024)
                                {
                                    return Err(format!("An Avro {key} exceeds 1,024 bytes"));
                                }
                            }
                            let kind = j.get("type").ok_or("a schema object has no \"type\"")?;
                            match kind.as_str() {
                                Some("record" | "error") => {
                                    let (index, ns) = self.define(
                                        j,
                                        &ns,
                                        Named::Record {
                                            name: String::new(),
                                            fields: vec![],
                                        },
                                    );
                                    let fields = j
                                        .get("fields")
                                        .and_then(Json::as_array)
                                        .ok_or("a record has no \"fields\" list")?;
                                    if fields.len() > 10_000 {
                                        return Err("An Avro record exceeds 10,000 fields".into());
                                    }
                                    let names = fields
                                        .iter()
                                        .map(|f| {
                                            f.get("name")
                                                .and_then(Json::as_str)
                                                .map(str::to_owned)
                                                .ok_or("a record field has no name".to_owned())
                                        })
                                        .collect::<Result<Vec<_>, _>>()?;
                                    tasks.push(Task::Record(index, names));
                                    for f in fields.iter().rev() {
                                        tasks.push(Task::Parse(
                                            f.get("type")
                                                .ok_or("a record field has no \"type\"")?,
                                            ns.clone(),
                                            depth + 1,
                                        ));
                                    }
                                }
                                Some("enum") => {
                                    let symbols = j
                                        .get("symbols")
                                        .and_then(Json::as_array)
                                        .ok_or("an enum has no \"symbols\" list")?
                                        .iter()
                                        .map(|s| {
                                            s.as_str()
                                                .map(str::to_owned)
                                                .ok_or("an enum symbol is not a string".to_owned())
                                        })
                                        .collect::<Result<Vec<_>, _>>()?;
                                    let name = j
                                        .get("name")
                                        .and_then(Json::as_str)
                                        .unwrap_or("enum")
                                        .to_owned();
                                    let (index, _) =
                                        self.define(j, &ns, Named::Enum { name, symbols });
                                    values.push(S::Named(index));
                                }
                                Some("fixed") => {
                                    let size = j
                                        .get("size")
                                        .and_then(Json::as_u64)
                                        .filter(|n| *n <= MAX_BLOCK)
                                        .ok_or("a fixed type has no valid \"size\"")?
                                        as usize;
                                    let name = j
                                        .get("name")
                                        .and_then(Json::as_str)
                                        .unwrap_or("fixed")
                                        .to_owned();
                                    let (index, _) =
                                        self.define(j, &ns, Named::Fixed { name, size });
                                    values.push(self.logical(j, S::Named(index)));
                                }
                                Some("array") => {
                                    tasks.push(Task::Array);
                                    tasks.push(Task::Parse(
                                        j.get("items").ok_or("an array has no \"items\"")?,
                                        ns,
                                        depth + 1,
                                    ));
                                }
                                Some("map") => {
                                    tasks.push(Task::Map);
                                    tasks.push(Task::Parse(
                                        j.get("values").ok_or("a map has no \"values\"")?,
                                        ns,
                                        depth + 1,
                                    ));
                                }
                                _ => {
                                    tasks.push(Task::Logical(j));
                                    tasks.push(Task::Parse(kind, ns, depth + 1));
                                }
                            }
                        }
                        _ => return Err("invalid schema".into()),
                    }
                }
                Task::Array => {
                    let s = values.pop().ok_or("missing array schema")?;
                    values.push(S::Array(Box::new(s)));
                }
                Task::Map => {
                    let s = values.pop().ok_or("missing map schema")?;
                    values.push(S::Map(Box::new(s)));
                }
                Task::Union(n) => {
                    let at = values.len().checked_sub(n).ok_or("missing union schemas")?;
                    let branches = values.split_off(at);
                    values.push(S::Union(branches));
                }
                Task::Record(index, names) => {
                    let at = values
                        .len()
                        .checked_sub(names.len())
                        .ok_or("missing field schemas")?;
                    let fields = names.into_iter().zip(values.split_off(at)).collect();
                    let name = self
                        .names
                        .iter()
                        .find(|(_, i)| *i == index)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_default();
                    *self.named.get_mut(index).ok_or("missing named schema")? =
                        Named::Record { name, fields };
                    values.push(S::Named(index));
                }
                Task::Logical(j) => {
                    let base = values.pop().ok_or("missing logical schema")?;
                    values.push(self.logical(j, base));
                }
            }
        }
        values.pop().ok_or("empty schema".into())
    }

    pub(super) fn describe(&self, s: &S) -> String {
        enum Part<'a> {
            Type(&'a S),
            Literal(&'static str),
        }
        let mut stack = vec![Part::Type(s)];
        let mut out = String::new();
        while let Some(part) = stack.pop() {
            let s = match part {
                Part::Literal(s) => {
                    out.push_str(s);
                    continue;
                }
                Part::Type(s) => s,
            };
            match s {
                S::Null => out.push_str("null"),
                S::Bool => out.push_str("boolean"),
                S::Int => out.push_str("int"),
                S::Long => out.push_str("long"),
                S::Float => out.push_str("float"),
                S::Double => out.push_str("double"),
                S::Bytes => out.push_str("bytes"),
                S::Str => out.push_str("string"),
                S::Named(i) => match self.named.get(*i) {
                    Some(Named::Record { name, .. }) => {
                        let _ = write!(out, "record {name}");
                    }
                    Some(Named::Enum { name, .. }) => {
                        let _ = write!(out, "enum {name}");
                    }
                    Some(Named::Fixed { name, size }) => {
                        let _ = write!(out, "fixed {name}({size})");
                    }
                    None => out.push('?'),
                },
                S::Array(item) | S::Map(item) => {
                    out.push_str(if matches!(s, S::Array(_)) {
                        "array<"
                    } else {
                        "map<"
                    });
                    stack.push(Part::Literal(">"));
                    stack.push(Part::Type(item));
                }
                S::Union(items) => {
                    out.push_str("union<");
                    stack.push(Part::Literal(">"));
                    for (i, item) in items.iter().enumerate().rev() {
                        stack.push(Part::Type(item));
                        if i > 0 {
                            stack.push(Part::Literal(", "));
                        }
                    }
                }
                S::Logical(_, logical) => match logical {
                    Logical::Date => out.push_str("date"),
                    Logical::TimeMillis => out.push_str("time-millis"),
                    Logical::TimeMicros => out.push_str("time-micros"),
                    Logical::Timestamp(1000) => out.push_str("timestamp-millis"),
                    Logical::Timestamp(1_000_000) => out.push_str("timestamp-micros"),
                    Logical::Timestamp(_) => out.push_str("timestamp-nanos"),
                    Logical::Decimal(scale) => {
                        let _ = write!(out, "decimal(scale {scale})");
                    }
                },
            }
        }
        out
    }
}
