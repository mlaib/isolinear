//! Jupyter notebooks (`.ipynb`, nbformat 4) read as cells and their saved outputs. Nothing is
//! executed: outputs are what the file saved. HTML and JavaScript outputs are shown as source,
//! widgets are named, never run. Markdown cells are rendered like the Markdown preview, with
//! images attached to the cell embedded.

use serde::Serialize;
use serde_json::Value;

use crate::markdown;

/// Characters of a text output shown; the rest is cut and counted.
const TEXT_MAX: usize = 100_000;
/// Bytes of an embedded image accepted (base64 text).
const IMAGE_MAX: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notebook {
    /// Language of the code cells (`python`, `r`, `julia`, …), from the kernel metadata.
    pub language: Option<String>,
    pub kernel: Option<String>,
    pub nbformat: String,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Cell {
    Code {
        source: String,
        execution_count: Option<u64>,
        outputs: Vec<Output>,
    },
    Markdown {
        /// Rendered HTML (raw HTML in the cell is shown as text).
        html: String,
    },
    Raw {
        source: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Output {
    /// Text written by the program: `stdout` or `stderr`.
    Stream {
        name: String,
        text: String,
    },
    Text {
        text: String,
    },
    /// A `data:` URI for an image.
    Image {
        uri: String,
        alt: String,
    },
    /// Markdown output, rendered.
    Html {
        html: String,
    },
    /// Content shown as source and never run (HTML, JavaScript), with its type.
    Source {
        mime: String,
        text: String,
    },
    Error {
        name: String,
        value: String,
        traceback: String,
    },
    /// Something that needs a running kernel or browser code to show.
    Unsupported {
        what: String,
    },
}

pub fn parse(text: &str) -> Result<Notebook, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let major = v.get("nbformat").and_then(Value::as_u64).unwrap_or(0);
    let minor = v.get("nbformat_minor").and_then(Value::as_u64).unwrap_or(0);
    if major != 4 {
        return Err(format!(
            "notebook format {major} is not supported (only version 4, written since 2015)"
        ));
    }
    let meta = v.get("metadata");
    let language = meta
        .and_then(|m| {
            m.pointer("/language_info/name")
                .or_else(|| m.pointer("/kernelspec/language"))
        })
        .and_then(Value::as_str)
        .map(str::to_lowercase);
    let kernel = meta
        .and_then(|m| m.pointer("/kernelspec/display_name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let cells = v
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("the notebook has no cells list")?
        .iter()
        .map(cell)
        .collect();
    Ok(Notebook {
        language,
        kernel,
        nbformat: format!("{major}.{minor}"),
        cells,
    })
}

/// Notebook text fields are a string or a list of lines.
fn joined(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn cut(mut s: String) -> String {
    if let Some((at, _)) = s.char_indices().nth(TEXT_MAX) {
        let total = s.chars().count();
        s.truncate(at);
        s.push_str(&format!(
            "\n… ({} more characters not shown)",
            total - TEXT_MAX
        ));
    }
    s
}

fn cell(c: &Value) -> Cell {
    let source = joined(c.get("source"));
    match c.get("cell_type").and_then(Value::as_str) {
        Some("code") => Cell::Code {
            source,
            execution_count: c.get("execution_count").and_then(Value::as_u64),
            outputs: c
                .get("outputs")
                .and_then(Value::as_array)
                .map(|o| o.iter().map(output).collect())
                .unwrap_or_default(),
        },
        Some("markdown") => {
            let attachments = c.get("attachments");
            let resolve = |dest: &str| -> Option<String> {
                let name = dest.strip_prefix("attachment:")?;
                let bundle = attachments?.get(name)?.as_object()?;
                bundle.iter().find_map(|(mime, data)| {
                    let data = joined(Some(data));
                    (mime.starts_with("image/") && data.len() <= IMAGE_MAX)
                        .then(|| format!("data:{mime};base64,{}", data.replace(['\n', ' '], "")))
                })
            };
            let html = markdown::render(&source, &resolve)
                .blocks
                .into_iter()
                .map(|b| b.html)
                .collect();
            Cell::Markdown { html }
        }
        _ => Cell::Raw { source },
    }
}

fn output(o: &Value) -> Output {
    match o.get("output_type").and_then(Value::as_str) {
        Some("stream") => Output::Stream {
            name: o
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("stdout")
                .to_owned(),
            text: cut(joined(o.get("text"))),
        },
        Some("error") => Output::Error {
            name: o
                .get("ename")
                .and_then(Value::as_str)
                .unwrap_or("Error")
                .to_owned(),
            value: o
                .get("evalue")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            traceback: cut(strip_ansi(
                &o.get("traceback")
                    .and_then(Value::as_array)
                    .map(|t| {
                        t.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            )),
        },
        Some("execute_result" | "display_data") => match o.get("data") {
            Some(Value::Object(data)) => bundle(data),
            _ => Output::Unsupported {
                what: "an output without data".into(),
            },
        },
        other => Output::Unsupported {
            what: format!("output type {}", other.unwrap_or("(none)")),
        },
    }
}

/// The richest representation of a MIME bundle that can be shown without running anything.
fn bundle(data: &serde_json::Map<String, Value>) -> Output {
    let get = |mime: &str| data.get(mime).map(|v| joined(Some(v)));
    for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        if let Some(b64) = get(mime)
            && b64.len() <= IMAGE_MAX
        {
            return Output::Image {
                uri: format!("data:{mime};base64,{}", b64.replace(['\n', ' '], "")),
                alt: get("text/plain").unwrap_or_default(),
            };
        }
    }
    if let Some(svg) = get("image/svg+xml")
        && svg.len() <= IMAGE_MAX
    {
        // In an <img>, SVG cannot run scripts or load anything.
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            crate::base64(svg.as_bytes())
        );
        return Output::Image {
            uri,
            alt: get("text/plain").unwrap_or_default(),
        };
    }
    if let Some(md) = get("text/markdown") {
        let html = markdown::render(&md, &|_| None)
            .blocks
            .into_iter()
            .map(|b| b.html)
            .collect();
        return Output::Html { html };
    }
    if data.contains_key("application/vnd.jupyter.widget-view+json") {
        return Output::Unsupported {
            what: "an interactive widget (it needs a running kernel)".into(),
        };
    }
    if let Some(text) = get("text/plain") {
        return Output::Text { text: cut(text) };
    }
    for mime in [
        "text/html",
        "application/javascript",
        "text/latex",
        "application/json",
    ] {
        if let Some(v) = data.get(mime) {
            let text = match v {
                Value::Object(_) | Value::Array(_) if mime == "application/json" => {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                }
                _ => joined(Some(v)),
            };
            return Output::Source {
                mime: mime.to_owned(),
                text: cut(text),
            };
        }
    }
    Output::Unsupported {
        what: format!(
            "output of type {}",
            data.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Terminal colour and cursor codes removed (tracebacks are written for terminals).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
 "nbformat": 4, "nbformat_minor": 5,
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python"},
              "language_info": {"name": "python"}},
 "cells": [
  {"cell_type": "markdown", "metadata": {}, "source": ["# Title\n", "Some ![pic](attachment:a.png)"],
   "attachments": {"a.png": {"image/png": "iVBORw0KGgo="}}},
  {"cell_type": "code", "execution_count": 3, "metadata": {}, "source": "print('hi')\n1/0",
   "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hi\n"]},
    {"output_type": "error", "ename": "ZeroDivisionError", "evalue": "division by zero",
     "traceback": ["\u001b[0;31mZeroDivisionError\u001b[0m: division by zero"]}
   ]},
  {"cell_type": "code", "execution_count": 4, "metadata": {}, "source": "df",
   "outputs": [
    {"output_type": "execute_result", "execution_count": 4, "metadata": {},
     "data": {"text/html": "<table onclick=x></table>", "text/plain": "   a\n0  1"}},
    {"output_type": "display_data", "metadata": {},
     "data": {"image/png": "iVBORw0KGgo=\n", "text/plain": "<Figure>"}},
    {"output_type": "display_data", "metadata": {},
     "data": {"application/javascript": "alert(1)"}},
    {"output_type": "display_data", "metadata": {},
     "data": {"application/vnd.jupyter.widget-view+json": {"model_id": "x"}, "text/plain": "IntSlider"}}
   ]},
  {"cell_type": "raw", "metadata": {}, "source": "raw text"}
 ]
}"##;

    #[test]
    fn cells_and_outputs() {
        let nb = parse(SAMPLE).unwrap();
        assert_eq!(nb.language.as_deref(), Some("python"));
        assert_eq!(nb.kernel.as_deref(), Some("Python 3"));
        assert_eq!(nb.nbformat, "4.5");
        assert_eq!(nb.cells.len(), 4);
        let Cell::Markdown { html } = &nb.cells[0] else {
            panic!()
        };
        assert!(html.contains("<h1"), "{html}");
        assert!(
            html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "{html}"
        );
        let Cell::Code {
            source,
            execution_count,
            outputs,
        } = &nb.cells[1]
        else {
            panic!()
        };
        assert_eq!(source, "print('hi')\n1/0");
        assert_eq!(*execution_count, Some(3));
        assert!(
            matches!(&outputs[0], Output::Stream { name, text } if name == "stdout" && text == "hi\n")
        );
        let Output::Error { traceback, .. } = &outputs[1] else {
            panic!()
        };
        assert_eq!(
            traceback, "ZeroDivisionError: division by zero",
            "colour codes removed"
        );
    }

    #[test]
    fn nothing_that_runs_is_passed_on_as_markup() {
        let nb = parse(SAMPLE).unwrap();
        let Cell::Code { outputs, .. } = &nb.cells[2] else {
            panic!()
        };
        assert!(
            matches!(&outputs[0], Output::Text { text } if text.contains("a")),
            "plain text is preferred to HTML: {:?}",
            outputs[0]
        );
        assert!(
            matches!(&outputs[1], Output::Image { uri, alt } if uri.ends_with("KGgo=") && alt == "<Figure>"),
            "{:?}",
            outputs[1]
        );
        assert!(
            matches!(&outputs[2], Output::Source { mime, .. } if mime == "application/javascript")
        );
        assert!(matches!(&outputs[3], Output::Unsupported { what } if what.contains("widget")));
    }

    #[test]
    fn refusals_and_odd_input() {
        assert!(parse("not json").unwrap_err().contains("not valid JSON"));
        assert!(
            parse(r#"{"nbformat": 3, "worksheets": []}"#)
                .unwrap_err()
                .contains("format 3")
        );
        assert!(parse(r#"{"nbformat": 4}"#).is_err());
        let nb = parse(r#"{"nbformat": 4, "cells": [{"cell_type": "code"}, {}, 5]}"#).unwrap();
        assert_eq!(nb.cells.len(), 3);
    }

    #[test]
    fn long_text_is_cut_and_counted() {
        let long = cut("x".repeat(TEXT_MAX + 5));
        assert!(long.ends_with("(5 more characters not shown)"));
    }
}
