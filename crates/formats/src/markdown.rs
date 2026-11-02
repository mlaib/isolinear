//! Markdown rendered for the preview: CommonMark with GitHub tables, task lists, strikethrough,
//! footnotes and math. Each top-level block is rendered on its own and its source range kept, so
//! the preview can follow the editor and the editor the preview.
//!
//! Nothing in the document runs or loads: raw HTML is shown as text, links are inert until the
//! user acts on them, and images go through `image` (the caller decides what to show; remote
//! images are never fetched). Math is passed to the UI as TeX for KaTeX.

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// A rendered document: one HTML fragment per top-level block.
pub struct Rendered {
    pub blocks: Vec<Block>,
}

pub struct Block {
    pub html: String,
    /// Byte range of the block in the source.
    pub span: Range<usize>,
}

/// What to show for an image: a `data:` URI the UI can display, or `None` for a placeholder.
pub type ImageResolver<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
}

pub fn render(source: &str, image: ImageResolver) -> Rendered {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut events: Vec<Event> = Vec::new();
    let mut start = 0usize;
    // Inside an image: its destination and title; its alt text is collected as plain text.
    let mut img: Option<(String, String, String)> = None;
    // Inside a heading without an id: its events, held until its text gives the id.
    let mut heading: Option<Vec<Event>> = None;
    let mut slugs: HashMap<String, usize> = HashMap::new();
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        if depth == 0 {
            start = range.start;
        }
        let opens = matches!(event, Event::Start(_));
        let closes = matches!(event, Event::End(_));
        if let Some((_, _, alt)) = &mut img {
            match event {
                Event::End(TagEnd::Image) => {
                    let (dest, title, alt) = img.take().unwrap();
                    events.push(Event::Html(CowStr::from(image_html(
                        &dest, &title, &alt, image,
                    ))));
                }
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                _ => {}
            }
            continue;
        }
        if let Some(held) = &mut heading {
            if let Event::End(TagEnd::Heading(_)) = event {
                let mut held = heading.take().unwrap();
                let text: String = held
                    .iter()
                    .filter_map(|e| match e {
                        Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                        _ => None,
                    })
                    .collect();
                if let Event::Start(Tag::Heading { id, .. }) = &mut held[0] {
                    *id = Some(CowStr::from(unique_slug(&text, &mut slugs)));
                }
                events.append(&mut held);
            } else {
                held.push(event);
                continue;
            }
        }
        match event {
            Event::Start(Tag::Heading { id: None, .. }) => {
                heading = Some(vec![event]);
                depth += 1;
                continue;
            }
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                img = Some((dest_url.into_string(), title.into_string(), String::new()));
                if depth == 0 {
                    // An image alone at the top level is still a block of its own.
                    depth += 1;
                }
                continue;
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                events.push(Event::Html(CowStr::from(format!(
                    "<span class=\"raw-html\">{}</span>",
                    escape(&h)
                ))));
            }
            Event::InlineMath(tex) => events.push(Event::Html(CowStr::from(format!(
                "<span class=\"math\" data-tex=\"{}\"></span>",
                escape(&tex)
            )))),
            Event::DisplayMath(tex) => events.push(Event::Html(CowStr::from(format!(
                "<span class=\"math display\" data-tex=\"{}\"></span>",
                escape(&tex)
            )))),
            other => events.push(other),
        }
        if opens {
            depth += 1;
        }
        if closes {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 && !events.is_empty() {
            let mut out = String::new();
            html::push_html(&mut out, events.drain(..));
            blocks.push(Block {
                html: out,
                span: start..range.end,
            });
        }
    }
    if !events.is_empty() {
        let mut out = String::new();
        html::push_html(&mut out, events.drain(..));
        blocks.push(Block {
            html: out,
            span: start..source.len(),
        });
    }
    Rendered { blocks }
}

/// GitHub's heading ids: lower case, spaces to hyphens, other punctuation dropped; repeats
/// numbered `-1`, `-2`, ….
fn unique_slug(text: &str, seen: &mut HashMap<String, usize>) -> String {
    let base: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect();
    let n = seen.entry(base.clone()).or_insert(0);
    let slug = if *n == 0 {
        base.clone()
    } else {
        format!("{base}-{n}")
    };
    *n += 1;
    slug
}

fn image_html(dest: &str, title: &str, alt: &str, image: ImageResolver) -> String {
    match image(dest) {
        Some(uri) => format!(
            "<img src=\"{}\" alt=\"{}\" title=\"{}\">",
            escape(&uri),
            escape(alt),
            escape(title)
        ),
        None => format!(
            "<span class=\"image-placeholder\" title=\"{}\">Image: {}</span>",
            escape(dest),
            escape(if alt.is_empty() { dest } else { alt })
        ),
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    fn html(source: &str) -> Vec<String> {
        render(source, &none)
            .blocks
            .into_iter()
            .map(|b| b.html)
            .collect()
    }

    #[test]
    fn one_fragment_per_block_with_source_ranges() {
        let s = "# Title\n\nSome *text*.\n\n- a\n- b\n";
        let r = render(s, &none);
        assert_eq!(r.blocks.len(), 3);
        assert_eq!(&s[r.blocks[0].span.clone()], "# Title\n");
        assert!(r.blocks[1].html.contains("<em>text</em>"));
        assert_eq!(&s[r.blocks[2].span.clone()], "- a\n- b\n");
    }

    #[test]
    fn raw_html_is_shown_as_text() {
        let h = html("<script>alert(1)</script>\n\nText <b onclick=x>bold</b>\n");
        assert!(
            h.iter()
                .all(|b| !b.contains("<script") && !b.contains("<b ")),
            "{h:?}"
        );
        assert!(h[0].contains("&lt;script&gt;"));
    }

    #[test]
    fn images_go_through_the_resolver() {
        let local = |dest: &str| (dest == "a.png").then(|| "data:image/png;base64,AAAA".to_owned());
        let r = render("![one](a.png) ![two](https://x/y.png)\n", &local);
        let h = &r.blocks[0].html;
        assert!(
            h.contains("<img src=\"data:image/png;base64,AAAA\" alt=\"one\""),
            "{h}"
        );
        assert!(h.contains("Image: two"));
        assert!(
            !h.contains("src=\"https:"),
            "a remote image is never loaded: {h}"
        );
    }

    #[test]
    fn math_tables_tasks_and_footnotes() {
        let h = html("$a<b$ and\n\n$$x^2$$\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\nNote[^1]\n\n[^1]: here\n").join("");
        assert!(h.contains("class=\"math\" data-tex=\"a&lt;b\""), "{h}");
        assert!(h.contains("class=\"math display\" data-tex=\"x^2\""));
        assert!(h.contains("<table>"));
        assert!(h.contains("type=\"checkbox\""));
        assert!(h.contains("footnote"));
    }

    #[test]
    fn headings_get_github_ids() {
        let h = html("# Hello, World!\n\n## Hello, World!\n\n### Own {#mine}\n").join("");
        assert!(h.contains("<h1 id=\"hello-world\">"), "{h}");
        assert!(h.contains("<h2 id=\"hello-world-1\">"));
        assert!(h.contains("<h3 id=\"mine\">"));
    }

    #[test]
    fn empty_and_odd_input() {
        assert!(html("").is_empty());
        for s in [
            "![",
            "![a](",
            "<",
            "$",
            "```\nunclosed",
            "> > > deep",
            "[^x]:",
        ] {
            let _ = html(s);
        }
    }
}
