//! Only this module emits markup. Input strings are always escaped, including attributes.
use super::{
    model::{Block, Cell, Span},
    reader::{self, OfficeDocument},
    zip::Listing,
};
use std::io::{Read, Seek};

pub(super) const TEXT_LIMIT: usize = 16 * 1024 * 1024;
pub(super) const IMAGE_LIMIT: u64 = 32 * 1024 * 1024;
const CUT: &str =
    "<p><span class=\"note\">The preview stops at the 16 MiB HTML text limit.</span></p>";

struct Html {
    out: String,
    used: usize,
    limit: usize,
    open: Vec<String>,
    closing: usize,
    cut: bool,
}
impl Html {
    fn new(limit: usize) -> Self {
        Self {
            out: String::new(),
            used: 0,
            limit,
            open: vec![],
            closing: 0,
            cut: false,
        }
    }
    fn atom(&mut self, s: &str) -> bool {
        if self.cut {
            return false;
        }
        if s.len()
            .saturating_add(self.used)
            .saturating_add(self.closing)
            .saturating_add(CUT.len())
            > self.limit
        {
            self.cut = true;
            return false;
        }
        self.used += s.len();
        self.out.push_str(s);
        true
    }
    fn start(&mut self, tag: &str, end: &str) {
        if self.cut {
            return;
        }
        self.closing += end.len();
        if self.atom(tag) {
            self.open.push(end.to_owned());
        } else {
            self.closing -= end.len();
        }
    }
    fn end(&mut self) {
        if let Some(end) = self.open.pop() {
            self.closing -= end.len();
            self.used += end.len();
            self.out.push_str(&end);
        }
    }
    fn text(&mut self, text: &str) {
        for c in text.chars() {
            let mut buf = [0; 4];
            let s = match c {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                '\n' => "<br>",
                _ => c.encode_utf8(&mut buf),
            };
            if !self.atom(s) {
                break;
            }
        }
    }
    fn finish(mut self) -> String {
        while !self.open.is_empty() {
            self.end();
        }
        if self.cut {
            self.out.push_str(CUT);
        }
        self.out
    }
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn safe_link(link: &str) -> bool {
    // Reject controls and whitespace rather than allowing the browser to normalize a scheme.
    !link.chars().any(|c| c.is_control() || c.is_whitespace())
        && link.split_once(':').is_some_and(|(scheme, rest)| {
            !rest.is_empty()
                && ["http", "https", "mailto"]
                    .iter()
                    .any(|s| scheme.eq_ignore_ascii_case(s))
        })
}

struct Images<'a, R> {
    file: &'a mut R,
    listing: &'a Listing,
    remaining: u64,
}
impl<R: Read + Seek> Images<'_, R> {
    fn emit(&mut self, span: &Span, html: &mut Html) -> bool {
        let Some(name) = &span.style.image_part else {
            return false;
        };
        let Some(entry) = self.listing.find(name) else {
            return false;
        };
        if entry.size > self.remaining {
            html.text("[image omitted: 32 MiB image limit]");
            return true;
        }
        // Charge every attempted read, including damaged or unsupported images.
        self.remaining -= entry.size;
        let Ok(Some(bytes)) = reader::part(self.file, self.listing, name) else {
            return false;
        };
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else {
            return false;
        };
        let prefix = format!(
            "<img alt=\"{}\" src=\"data:{mime};base64,",
            escape(&span.text)
        );
        // Count markup as HTML text, but account for base64 separately under the image budget.
        if prefix
            .len()
            .saturating_add(2)
            .saturating_add(html.used)
            .saturating_add(html.closing)
            .saturating_add(CUT.len())
            > html.limit
        {
            html.cut = true;
            return true;
        }
        if html.atom(&prefix) {
            html.out.push_str(&crate::base64(&bytes));
            html.out.push_str("\">");
            html.used += 2;
        }
        true
    }
}

fn spans<R: Read + Seek>(spans: &[Span], html: &mut Html, images: &mut Images<R>) {
    for span in spans {
        if html.cut {
            break;
        }
        if span.style.image && images.emit(span, html) {
            continue;
        }
        let depth = html.open.len();
        let s = &span.style;
        if let Some(link) = s.link.as_deref().filter(|l| safe_link(l)) {
            html.start(&format!("<a href=\"{}\">", escape(link)), "</a>");
        }
        for (on, start, end) in [
            (s.note, "<span class=\"note\">", "</span>"),
            (s.strong, "<strong>", "</strong>"),
            (s.emphasis, "<em>", "</em>"),
            (s.underline, "<u>", "</u>"),
            (s.strike, "<s>", "</s>"),
            (s.position > 0, "<sup>", "</sup>"),
            (s.position < 0, "<sub>", "</sub>"),
        ] {
            if on {
                html.start(start, end);
            }
        }
        html.text(&span.text);
        while html.open.len() > depth {
            html.end();
        }
    }
}

enum Task<'a> {
    Block(&'a Block),
    Cells(&'a [Cell], bool),
    Spans(&'a [Span]),
    Open(&'static str, &'static str),
    Close,
}

pub(super) fn render(
    document: &OfficeDocument,
    file: &mut (impl Read + Seek),
    listing: &Listing,
) -> Result<String, String> {
    let mut html = Html::new(TEXT_LIMIT);
    let mut images = Images {
        file,
        listing,
        remaining: IMAGE_LIMIT,
    };
    // Macro notice comes before all other content, including preview limitations.
    for warning in document
        .warnings
        .iter()
        .filter(|w| w.contains("macros"))
        .chain(document.warnings.iter().filter(|w| !w.contains("macros")))
    {
        html.start("<p>", "</p>");
        html.start("<span class=\"note\">", "</span>");
        html.text(warning);
        html.end();
        html.end();
    }
    let mut stack: Vec<_> = document
        .content
        .blocks
        .iter()
        .rev()
        .map(Task::Block)
        .collect();
    while let Some(task) = stack.pop() {
        if html.cut {
            break;
        }
        match task {
            Task::Close => html.end(),
            Task::Open(start, end) => html.start(start, end),
            Task::Spans(s) => spans(s, &mut html, &mut images),
            Task::Cells(cells, header) => {
                html.start("<tr>", "</tr>");
                for cell in cells.iter().filter(|c| !c.covered) {
                    if html.cut {
                        break;
                    }
                    let tag = if header { "th" } else { "td" };
                    html.start(
                        &format!(
                            "<{tag} colspan=\"{}\" rowspan=\"{}\">",
                            cell.colspan, cell.rowspan
                        ),
                        if header { "</th>" } else { "</td>" },
                    );
                    spans(&cell.spans, &mut html, &mut images);
                    html.end();
                }
                html.end();
            }
            Task::Block(block) => match block {
                Block::Heading { level, spans } => {
                    let level = level.clamp(&1, &6);
                    html.start(&format!("<h{level}>"), &format!("</h{level}>"));
                    stack.extend([Task::Close, Task::Spans(spans)]);
                }
                Block::Paragraph { spans } => {
                    html.start("<p>", "</p>");
                    stack.extend([Task::Close, Task::Spans(spans)]);
                }
                Block::Code { spans } => {
                    html.start("<pre>", "</pre>");
                    html.start("<code>", "</code>");
                    stack.extend([Task::Close, Task::Close, Task::Spans(spans)]);
                }
                Block::Quote { blocks } => {
                    html.start("<blockquote>", "</blockquote>");
                    stack.push(Task::Close);
                    stack.extend(blocks.iter().rev().map(Task::Block));
                }
                Block::List { start, items } => {
                    if start.is_some() {
                        html.start("<ol>", "</ol>");
                    } else {
                        html.start("<ul>", "</ul>");
                    }
                    stack.push(Task::Close);
                    for item in items.iter().rev() {
                        stack.push(Task::Close);
                        stack.extend(item.blocks.iter().rev().map(Task::Block));
                        stack.push(Task::Open("<li>", "</li>"));
                    }
                }
                Block::Table { header, rows } => {
                    html.start("<table>", "</table>");
                    stack.push(Task::Close);
                    stack.extend(rows.iter().rev().map(|r| Task::Cells(r, false)));
                    stack.push(Task::Cells(header, true));
                }
            },
        }
    }
    Ok(html.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cut_escaped_text_without_breaking_markup_or_utf8() {
        let mut html = Html::new(200);
        html.start("<p>", "</p>");
        html.start("<strong>", "</strong>");
        html.text(&"<&😀".repeat(100));
        let out = html.finish();
        assert!(out.len() <= 200);
        assert!(out.ends_with(&format!("</strong></p>{CUT}")));
        assert!(!out.contains("<&"));
    }
    #[test]
    fn only_explicit_safe_schemes() {
        for good in [
            "https://example.org",
            "HTTP://example.org",
            "mailto:a@example.org",
        ] {
            assert!(safe_link(good));
        }
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "//example.org",
            "java\nscript:x",
            " https://example.org",
            "https:",
        ] {
            assert!(!safe_link(bad));
        }
    }
}
