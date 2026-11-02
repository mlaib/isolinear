//! A file as one self-contained HTML page, for the quick previews of the file managers (Quick
//! Look on macOS, the preview pane of Explorer on Windows): the Overview of a standard, the first
//! rows of a table, a document or a PDF page, or the text. Styles are inline, there is no script,
//! and every value from the file is escaped; pictures are `data:` URIs. Nothing is fetched.

use isolinear_core::encoding;
use isolinear_formats::inspect::{self, Inspection, State};
use isolinear_formats::registry::Kind;
use isolinear_formats::{base64, binary, decoded, detect, document, notebook, pdf, table, tiff};
use std::fmt::Write;
use std::path::Path;

/// Rows of a table shown.
const ROWS: u64 = 200;
/// Text shown, in bytes.
const TEXT: usize = 512 * 1024;
/// Bytes read for a text or decoded preview.
const READ: u64 = 16 * 1024 * 1024;

/// The preview of the file at `path`. Never fails: a file that cannot be read is described.
pub fn html(path: &Path) -> String {
    std::panic::catch_unwind(|| page(path)).unwrap_or_else(|_| {
        shell(
            &name_of(path),
            "File",
            "",
            "<p class=note>Isolinear could not read this file.</p>",
        )
    })
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn page(path: &Path) -> String {
    let name = name_of(path);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let head = read_head(path, READ.min(size));
    let probe = &head[..head.len().min(64 * 1024)];
    let d = detect(&name, probe);
    let format = d.format.id;
    let size_text = human(size);

    let mut body = String::new();
    if let Some(Ok(i)) = inspect::inspect_file(path, format, &|| false) {
        overview(&mut body, &i);
    }
    match format {
        "pdf" => pdf_page(&mut body, path),
        "docx" | "odt" => {
            if let Ok(doc) = document::to_html(format, path) {
                // The reader writes `<a href="…">` for links (escaped); shown, not followed.
                let doc = doc
                    .replace("<a href=", "<span class=link title=")
                    .replace("</a>", "</span>");
                let _ = write!(body, "<section class=doc>{doc}</section>");
            }
        }
        "tiff" if (size as usize) == head.len() => {
            if let Ok(png) = tiff::to_png(&head) {
                picture(&mut body, &png, "image/png", "");
            }
        }
        f if decoded::supports(f) && (size as usize) == head.len() => {
            if let Some(Ok((json, note))) = decoded::to_json(f, &head, &|| false) {
                let _ = write!(body, "<p class=note>{}</p>", esc(&note));
                pre(&mut body, &json);
            }
        }
        "notebook" => notebook_cells(&mut body, &head),
        _ => {}
    }
    if table::supports(format) {
        first_rows(&mut body, format, path);
    } else if d.format.kind == Kind::Text && format != "notebook" {
        let text = encoding::decode(&head).text;
        if format == "markdown" {
            markdown(&mut body, &text);
        } else {
            pre(&mut body, &text);
        }
        if size > head.len() as u64 {
            let _ = write!(
                body,
                "<p class=note>The first {} are shown.</p>",
                human(head.len() as u64)
            );
        }
    }
    if body.is_empty() {
        summary(&mut body, probe);
    }
    shell(&name, d.format.name, &size_text, &body)
}

fn read_head(path: &Path, n: u64) -> Vec<u8> {
    use std::io::Read;
    let mut bytes = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(n).read_to_end(&mut bytes);
    }
    bytes
}

fn shell(name: &str, format: &str, size: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8>\
         <meta name=color-scheme content=\"light dark\">\
         <meta http-equiv=Content-Security-Policy content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\">\
         <title>{name}</title><style>{CSS}</style></head>\
         <body><header><h1>{name}</h1><span class=chip>{format}</span><span class=muted>{size}</span>\
         <span class=brand>Isolinear</span></header><main>{body}</main></body></html>",
        name = esc(name),
        format = esc(format),
        size = esc(size),
    )
}

fn overview(out: &mut String, i: &Inspection) {
    let _ = write!(
        out,
        "<section><span class=family>{}</span><h2 class=kind>{}</h2>",
        esc(&i.family),
        esc(&i.kind)
    );
    if let Some(v) = &i.version {
        let _ = write!(out, "<p class=muted>{}</p>", esc(v));
    }
    out.push_str("<div class=panel><h3>What was checked</h3><ul class=checks>");
    for c in &i.checks {
        let (mark, class) = match c.state {
            State::Passed => ("✓", "passed"),
            State::Partial => ("!", "partial"),
            State::Failed => ("✗", "failed"),
            State::NotDone => ("–", "notdone"),
        };
        let _ = write!(
            out,
            "<li class={class}><span class=mark>{mark}</span><b>{}</b> <span class=muted>{}</span></li>",
            esc(c.stage.name()),
            esc(&c.detail)
        );
    }
    out.push_str("</ul></div>");
    let mut group: Option<&str> = None;
    for f in &i.facts {
        if group != Some(&f.group) {
            if group.is_some() {
                out.push_str("</dl></div>");
            }
            let _ = write!(out, "<div class=panel><h3>{}</h3><dl>", esc(&f.group));
            group = Some(&f.group);
        }
        let _ = write!(out, "<dt>{}</dt><dd>{}</dd>", esc(&f.label), esc(&f.value));
    }
    if group.is_some() {
        out.push_str("</dl></div>");
    }
    for v in &i.visuals {
        if let inspect::Visual::Picture { bytes, caption } = v
            && let Some(mime) = image_mime(bytes)
        {
            picture(out, bytes, mime, caption);
        }
    }
    for t in &i.tables {
        let total = t.total.map(|n| format!(" ({n} rows)")).unwrap_or_default();
        let _ = write!(out, "<h3>{}{}</h3>", esc(&t.name), esc(&total));
        let rows: Vec<Vec<Option<String>>> = t
            .rows
            .iter()
            .take(ROWS as usize)
            .map(|r| r.cells.iter().cloned().map(Some).collect())
            .collect();
        grid(out, &t.columns, &rows);
    }
    if !i.notes.is_empty() {
        out.push_str("<div class=notes>");
        for n in &i.notes {
            let _ = write!(out, "<p>{}</p>", esc(n));
        }
        out.push_str("</div>");
    }
    out.push_str("</section>");
}

fn first_rows(out: &mut String, format: &str, path: &Path) {
    let Some(Ok(mut t)) = table::open(format, path, None) else {
        return;
    };
    let schema = t.schema().clone();
    let columns: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
    let rows = t.rows(0, ROWS as usize).unwrap_or_default();
    let total = schema.rows.unwrap_or(rows.len() as u64);
    // The table's name, when the file holds several.
    let name = match schema.tables.len() {
        0 | 1 => "",
        _ => schema
            .tables
            .get(schema.table)
            .map_or("", |t| t.name.as_str()),
    };
    let _ = write!(
        out,
        "<h3>{}{} rows × {} columns</h3>",
        if name.is_empty() {
            String::new()
        } else {
            format!("{} · ", esc(name))
        },
        total,
        columns.len()
    );
    grid(out, &columns, &rows);
    if total > rows.len() as u64 {
        let _ = write!(
            out,
            "<p class=note>The first {} rows are shown.</p>",
            rows.len()
        );
    }
}

fn grid(out: &mut String, columns: &[String], rows: &[Vec<Option<String>>]) {
    out.push_str("<div class=table><table><thead><tr><th></th>");
    for c in columns {
        let _ = write!(out, "<th>{}</th>", esc(c));
    }
    out.push_str("</tr></thead><tbody>");
    for (n, row) in rows.iter().enumerate() {
        let _ = write!(out, "<tr><td class=n>{}</td>", n + 1);
        for cell in row {
            match cell {
                Some(v) => {
                    let _ = write!(out, "<td>{}</td>", esc(&clip(v, 300)));
                }
                None => out.push_str("<td class=null>null</td>"),
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
}

fn pdf_page(out: &mut String, path: &Path) {
    if pdf::page_count(path).is_ok_and(|n| n > 0)
        && let Ok(png) = pdf::render_page(path, 0, 1.5)
    {
        out.push_str("<div class=page>");
        picture(out, &png, "image/png", "");
        out.push_str("</div>");
    }
}

fn notebook_cells(out: &mut String, bytes: &[u8]) {
    let text = encoding::decode(bytes).text;
    let Ok(nb) = notebook::parse(&text) else {
        return;
    };
    for cell in nb.cells.iter().take(200) {
        match cell {
            notebook::Cell::Markdown { html } => {
                let _ = write!(out, "<div class=md>{html}</div>");
            }
            notebook::Cell::Code {
                source, outputs, ..
            } => {
                pre(out, source);
                for o in outputs {
                    match o {
                        notebook::Output::Stream { text, .. } | notebook::Output::Text { text } => {
                            let _ =
                                write!(out, "<pre class=output>{}</pre>", esc(&clip(text, TEXT)));
                        }
                        notebook::Output::Image { uri, alt } if uri.starts_with("data:image/") => {
                            let _ = write!(out, "<img src=\"{}\" alt=\"{}\">", esc(uri), esc(alt));
                        }
                        notebook::Output::Html { html } => {
                            let _ = write!(out, "<div class=md>{html}</div>");
                        }
                        notebook::Output::Error { name, value, .. } => {
                            let _ =
                                write!(out, "<pre class=error>{}: {}</pre>", esc(name), esc(value));
                        }
                        _ => {}
                    }
                }
            }
            notebook::Cell::Raw { source } => pre(out, source),
        }
    }
}

/// Markdown as HTML; HTML written in the file is shown as text, and images are not loaded.
fn markdown(out: &mut String, text: &str) {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let text = clip(text, TEXT);
    let events = Parser::new_ext(
        &text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
    .map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Image { .. }) => Event::Text("[Image: ".into()),
        // Links are shown, not followed: a preview loads nothing.
        Event::Start(Tag::Link { dest_url, .. }) => {
            Event::Html(format!("<span class=link title=\"{}\">", esc(&dest_url)).into())
        }
        Event::End(pulldown_cmark::TagEnd::Link) => Event::Html("</span>".into()),
        Event::End(pulldown_cmark::TagEnd::Image) => Event::Text("]".into()),
        e => e,
    });
    out.push_str("<div class=md>");
    pulldown_cmark::html::push_html(out, events);
    out.push_str("</div>");
}

fn summary(out: &mut String, probe: &[u8]) {
    out.push_str("<div class=panel><h3>Summary</h3><dl>");
    for (k, v) in binary::facts(probe) {
        let _ = write!(out, "<dt>{}</dt><dd>{}</dd>", esc(&k), esc(&v));
    }
    out.push_str("</dl></div><h3>First bytes</h3><pre>");
    for (row, chunk) in probe.chunks(16).take(32).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let text: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '·'
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "{:08X}  {:<48} {}",
            row * 16,
            hex.join(" "),
            esc(&text)
        );
    }
    out.push_str("</pre>");
}

fn pre(out: &mut String, text: &str) {
    let _ = write!(out, "<pre>{}</pre>", esc(&clip(text, TEXT)));
}

fn picture(out: &mut String, bytes: &[u8], mime: &str, caption: &str) {
    let _ = write!(
        out,
        "<figure><img src=\"data:{mime};base64,{}\" alt=\"{}\">{}</figure>",
        base64(bytes),
        esc(caption),
        if caption.is_empty() {
            String::new()
        } else {
            format!("<figcaption>{}</figcaption>", esc(caption))
        }
    );
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

/// At most `max` bytes of `s`, cut at a character boundary.
fn clip(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max {
        return s.into();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end]).into()
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn human(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1 << 20 => format!("{:.1} KiB", n as f64 / 1024.0),
        n if n < 1 << 30 => format!("{:.1} MiB", n as f64 / 1048576.0),
        n => format!("{:.2} GiB", n as f64 / 1073741824.0),
    }
}

const CSS: &str = r#"
:root{--bg:#fff;--fg:#1f2328;--muted:#656d76;--bar:#f6f8fa;--line:#d0d7de;--chip:#e7ebef;--accent:#0969da;--ok:#1a7f37;--warn:#9a6700;--bad:#cf222e}
@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#8d96a0;--bar:#161b22;--line:#30363d;--chip:#21262d;--accent:#2f81f7;--ok:#3fb950;--warn:#d29922;--bad:#ff7b72}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 -apple-system,"Segoe UI",system-ui,sans-serif}
header{display:flex;align-items:center;gap:10px;padding:10px 18px;border-bottom:1px solid var(--line);background:var(--bar);position:sticky;top:0}
header h1{font-size:15px;margin:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.md h1{font-size:24px;border-bottom:1px solid var(--line);padding-bottom:6px}.md h2{font-size:19px;border-bottom:1px solid var(--line);padding-bottom:4px}.md h3{font-size:16px;text-transform:none;letter-spacing:0;color:var(--fg)}
.chip,.family{padding:1px 8px;border-radius:999px;background:var(--chip);color:var(--muted);font-size:12px;white-space:nowrap}
.family{color:var(--accent);font-weight:600}
.brand{margin-left:auto;color:var(--muted);font-size:12px}
main{padding:14px 18px 40px;max-width:1100px}
h2.kind{font-size:20px;margin:6px 0 2px}
h3{font-size:11.5px;text-transform:uppercase;letter-spacing:.06em;color:var(--muted);margin:16px 0 8px}
.panel{border:1px solid var(--line);border-radius:10px;background:var(--bar);padding:4px 14px 12px;margin-top:12px}
.checks{list-style:none;padding:0;margin:0}.checks li{padding:4px 0;border-top:1px solid var(--line)}.checks li:first-child{border-top:none}
.mark{display:inline-block;width:1.4em;font-weight:700}.passed .mark{color:var(--ok)}.partial .mark{color:var(--warn)}.failed .mark{color:var(--bad)}.notdone .mark{color:var(--muted)}
dl{display:grid;grid-template-columns:minmax(120px,220px) 1fr;gap:4px 16px;margin:0}dt{color:var(--muted)}dd{margin:0;overflow-wrap:anywhere}
.muted,.note,.notes{color:var(--muted)}.note,.notes{font-size:12.5px}
.table{overflow:auto;border:1px solid var(--line);border-radius:8px;max-height:70vh}
table{border-collapse:collapse;font:12.5px ui-monospace,Menlo,Consolas,monospace;width:max-content;min-width:100%}
th,td{padding:3px 10px;border-bottom:1px solid var(--line);text-align:left;white-space:nowrap;max-width:420px;overflow:hidden;text-overflow:ellipsis}
th{position:sticky;top:0;background:var(--bar);font-family:-apple-system,"Segoe UI",system-ui,sans-serif}
td.n{color:var(--muted);text-align:right}td.null{color:var(--muted);font-style:italic}
pre{font:12.5px/1.5 ui-monospace,Menlo,Consolas,monospace;background:var(--bar);border:1px solid var(--line);border-radius:8px;padding:10px 12px;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere}
pre.output{background:transparent}pre.error{color:var(--bad)}
img{max-width:100%;height:auto}figure{margin:12px 0}figcaption{color:var(--muted);font-size:12px}
.page img{background:#fff;box-shadow:0 1px 4px rgb(0 0 0 / .3)}
.md table{font-family:inherit}.md td,.md th{white-space:normal}.link{color:var(--accent);text-decoration:underline}.md code{font-family:ui-monospace,Menlo,Consolas,monospace;background:var(--chip);padding:1px 4px;border-radius:4px}
.doc{background:var(--bar);border:1px solid var(--line);border-radius:10px;padding:8px 20px}
"#;

/// For the macOS Quick Look extension: the preview of the file at the UTF-8 path `path`, as a
/// UTF-8 C string to release with [`isolinear_preview_free`]; null if the path is not UTF-8.
///
/// # Safety
/// `path` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isolinear_preview_html(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a valid NUL-terminated string.
    let Ok(path) = (unsafe { std::ffi::CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let page = html(Path::new(path)).replace('\0', "");
    std::ffi::CString::new(page).map_or(std::ptr::null_mut(), std::ffi::CString::into_raw)
}

/// Releases a string from [`isolinear_preview_html`].
///
/// # Safety
/// `s` is null or came from [`isolinear_preview_html`] and was not released before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isolinear_preview_free(s: *mut std::ffi::c_char) {
    if !s.is_null() {
        // SAFETY: `s` came from `CString::into_raw` in `isolinear_preview_html`.
        drop(unsafe { std::ffi::CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests;
