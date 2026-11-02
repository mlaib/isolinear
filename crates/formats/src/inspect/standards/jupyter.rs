//! Jupyter notebooks (`.ipynb`, nbformat 4, and 3 in part): cells, their sources and the
//! outputs stored in the file. Nothing is executed or rendered as HTML; stored PNG and JPEG
//! outputs are shown as pictures.
use super::records::{Rows, base64, clip, first_line, join, nodes, outline, s, span};
use crate::inspect::{
    Inspection, Visual,
    jsondoc::{Node, Value},
    spec::JsonSpec,
};

/// Pictures kept, and the most bytes one may decode to.
const MAX_PICTURES: usize = 20;
const MAX_PICTURE_BYTES: usize = 32 * 1024 * 1024;

pub static JUPYTER: JsonSpec = JsonSpec {
    family: "Jupyter",
    kind: "Notebook",
    matches: |keys, head| {
        keys.iter().any(|k| k == "nbformat")
            || (keys
                .first()
                .is_some_and(|k| k == "cells" || k == "worksheets")
                && head.contains("\"cell_type\""))
    },
    version: |root| {
        let (major, minor) = (s(root, "nbformat"), s(root, "nbformat_minor"));
        (!major.is_empty()).then(|| {
            if minor.is_empty() {
                format!("nbformat {major}")
            } else {
                format!("nbformat {major}.{minor}")
            }
        })
    },
    facts: &[],
    tables: &[],
    custom: Some(custom),
};

/// A source or text field: a string or a list of strings (lines).
fn text_of(n: Option<&Node>) -> String {
    match n.map(|n| &n.value) {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(items)) => {
            let mut out = String::new();
            for i in items {
                if let Value::String(t) = &i.value {
                    out.push_str(t);
                }
                if out.len() > 4 * 1024 * 1024 {
                    break;
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn custom(root: &Node, out: &mut Inspection) {
    let meta = root.get("metadata");
    let m = |p: &str| meta.map(|m| s(m, p)).unwrap_or_default();
    out.fact(
        "Notebook",
        "Kernel",
        join(
            &[m("kernelspec/display_name"), {
                let n = m("kernelspec/name");
                if n.is_empty() { n } else { format!("({n})") }
            }],
            " ",
        ),
        meta.and_then(|m| m.get("kernelspec")).and_then(span),
    );
    let language = {
        let l = m("language_info/name");
        if l.is_empty() {
            m("kernelspec/language")
        } else {
            l
        }
    };
    out.fact(
        "Notebook",
        "Language",
        join(&[language.clone(), m("language_info/version")], " "),
        meta.and_then(|m| m.get("language_info")).and_then(span),
    );
    out.fact("Notebook", "Title", m("title"), None);
    let v3 = root.get("worksheets").is_some();
    let cells = if v3 {
        nodes(root, "worksheets/cells")
    } else {
        nodes(root, "cells")
    };
    let mut table = Rows::new(
        "Cells",
        &[
            "#",
            "Type",
            "Execution count",
            "First line",
            "Lines",
            "Outputs",
            "Output types",
        ],
    );
    let (mut code, mut markdown, mut raw, mut executed, mut outputs_total) = (0, 0, 0, 0, 0);
    let mut pictures = 0usize;
    let mut not_rendered = 0usize;
    for (i, c) in cells.iter().enumerate() {
        let n = i + 1;
        let ty = s(c, "cell_type");
        match ty.as_str() {
            "code" => code += 1,
            "markdown" => markdown += 1,
            _ => raw += 1,
        }
        let source = text_of(c.get("source").or(c.get("input")));
        let count = s(c, "execution_count | prompt_number");
        if !count.is_empty() {
            executed += 1;
        }
        let outputs = nodes(c, "outputs");
        outputs_total += outputs.len();
        let mut types: Vec<String> = vec![];
        let lang = if ty == "code" {
            if language.is_empty() {
                "code".to_owned()
            } else {
                language.clone()
            }
        } else {
            ty.clone()
        };
        outline(
            out,
            0,
            format!("Cell {n} ({lang})"),
            if count.is_empty() {
                String::new()
            } else {
                format!("[{count}]")
            },
            span(c),
        );
        for (ln, line) in source.lines().enumerate().take(500) {
            outline(out, 1, format!("{}", ln + 1), clip(line, 400), None);
        }
        for (oi, o) in outputs.iter().enumerate() {
            let ot = s(o, "output_type");
            let label = format!("Output {}", oi + 1);
            let data = o.get("data");
            let mime: Vec<String> = match data.map(|d| &d.value) {
                Some(Value::Object(fields)) => fields.iter().map(|(k, _)| k.clone()).collect(),
                _ => vec![],
            };
            types.push(if mime.is_empty() {
                ot.clone()
            } else {
                format!("{ot} ({})", mime.join(", "))
            });
            match ot.as_str() {
                "stream" => {
                    let t = text_of(o.get("text"));
                    outline(
                        out,
                        1,
                        format!("{label} ({})", s(o, "name")),
                        clip(&t.replace('\n', " ⏎ "), 400),
                        span(o),
                    );
                }
                "error" | "pyerr" => {
                    outline(
                        out,
                        1,
                        format!("{label} (error)"),
                        join(&[s(o, "ename"), s(o, "evalue")], ": "),
                        span(o),
                    );
                }
                _ => {
                    let d = data.unwrap_or(o);
                    // nbformat 3 keeps mime data on the output itself (png, jpeg, html).
                    let png = d.get("image/png").or(o.get("png"));
                    let jpeg = d.get("image/jpeg").or(o.get("jpeg"));
                    let picture = png.or(jpeg);
                    let plain = text_of(d.get("text/plain").or(o.get("text")));
                    let mut value = clip(&plain.replace('\n', " ⏎ "), 400);
                    if let Some(p) = picture {
                        if pictures < MAX_PICTURES {
                            let encoded = text_of(Some(p));
                            match base64(&encoded, MAX_PICTURE_BYTES) {
                                Some(bytes) if !bytes.is_empty() => {
                                    pictures += 1;
                                    out.visuals.push(Visual::Picture {
                                        bytes,
                                        caption: format!("Cell {n} output"),
                                    });
                                    value = join(&[format!("picture {pictures}"), value], " · ");
                                }
                                _ => {
                                    value = join(
                                        &["picture data could not be decoded".into(), value],
                                        " · ",
                                    )
                                }
                            }
                        } else {
                            value = join(
                                &[
                                    format!("picture not shown (more than {MAX_PICTURES})"),
                                    value,
                                ],
                                " · ",
                            );
                        }
                    }
                    for (key, what) in [
                        ("text/html", "HTML"),
                        ("html", "HTML"),
                        ("application/javascript", "JavaScript"),
                        ("javascript", "JavaScript"),
                        ("image/svg+xml", "SVG"),
                        ("svg", "SVG"),
                        ("application/vnd.jupyter.widget-view+json", "widget"),
                    ] {
                        if d.get(key).is_some() || o.get(key).is_some() {
                            not_rendered += 1;
                            value = join(
                                &[
                                    value,
                                    format!("{what} output not rendered (never executed)"),
                                ],
                                " · ",
                            );
                        }
                    }
                    outline(out, 1, format!("{label} ({ot})"), value, span(o));
                }
            }
        }
        table.push(
            vec![
                n.to_string(),
                ty,
                count,
                first_line(&source, 120),
                if source.is_empty() {
                    "0".into()
                } else {
                    source.lines().count().to_string()
                },
                outputs.len().to_string(),
                types.join("; "),
            ],
            span(c),
        );
    }
    out.fact("Cells", "Cells", cells.len().to_string(), None);
    out.fact("Cells", "Code cells", code.to_string(), None);
    out.fact("Cells", "Markdown cells", markdown.to_string(), None);
    if raw > 0 {
        out.fact("Cells", "Other cells", raw.to_string(), None);
    }
    out.fact(
        "Cells",
        "Code cells with an execution count",
        executed.to_string(),
        None,
    );
    out.fact("Cells", "Stored outputs", outputs_total.to_string(), None);
    out.fact("Cells", "Pictures shown", pictures.to_string(), None);
    table.finish(out);
    out.notes.push(
        "Nothing in the notebook is executed: the outputs shown are those stored in the file. HTML, JavaScript, SVG and widget outputs are listed, not rendered".into(),
    );
    if not_rendered > 0 {
        out.notes
            .push(format!("{not_rendered} rich outputs were not rendered"));
    }
    if v3 {
        out.notes.push(
            "nbformat 3 notebook: read in part (cells of the worksheets, stored outputs)".into(),
        );
    }
}
