//! TeX and LaTeX outline: sectioning commands nest by level inside the environments that hold
//! them; environments nest by `\begin` and `\end`; labels, citations, references and included
//! files are listed where they appear. Comments are skipped and verbatim-like environments are
//! not looked into. Nothing is expanded or compiled.

use std::sync::atomic::AtomicBool;

use super::{Builder, Kind, Limits, Scalar, Span, Tree, TreeFormat};

pub const FORMAT: Option<&'static dyn TreeFormat> = Some(&Tex);

pub struct Tex;

const SECTIONS: &[(&str, u8)] = &[
    ("part", 0),
    ("chapter", 1),
    ("section", 2),
    ("subsection", 3),
    ("subsubsection", 4),
    ("paragraph", 5),
    ("subparagraph", 6),
];

/// Commands listed as entries, with the argument that names them.
const ENTRIES: &[&str] = &[
    "label",
    "cite",
    "citep",
    "citet",
    "autocite",
    "parencite",
    "textcite",
    "nocite",
    "ref",
    "eqref",
    "autoref",
    "cref",
    "Cref",
    "pageref",
    "input",
    "include",
    "includegraphics",
    "bibliography",
    "addbibresource",
];

/// Environments whose content is not TeX.
const VERBATIM: &[&str] = &[
    "verbatim",
    "verbatim*",
    "Verbatim",
    "lstlisting",
    "minted",
    "comment",
    "filecontents",
];

enum Frame {
    Environment(String),
    Section(u8),
}

impl TreeFormat for Tex {
    fn parse(&self, source: &str, limits: &Limits, cancel: &AtomicBool) -> Tree {
        let b = &mut Builder::new(source.len(), limits, cancel);
        let s = source.as_bytes();
        let mut stack: Vec<Frame> = Vec::new();
        let mut i = 0;
        while i < s.len() {
            match s[i] {
                b'%' => {
                    i = s[i..]
                        .iter()
                        .position(|&c| c == b'\n')
                        .map_or(s.len(), |p| i + p);
                    continue;
                }
                b'\\' => {}
                _ => {
                    i += 1;
                    continue;
                }
            }
            let start = i;
            let name_start = i + 1;
            let mut j = name_start;
            while j < s.len() && s[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j == name_start {
                // A control symbol (\\, \%, \{): skip it whole.
                i = (j + 1).min(s.len());
                if i < s.len() {
                    i = next_char(source, j);
                }
                continue;
            }
            let name = &source[name_start..j];
            let star = s.get(j) == Some(&b'*');
            let mut k = j + usize::from(star);
            let result = if let Some(&(_, level)) = SECTIONS.iter().find(|(n, _)| *n == name) {
                k = skip_optional(s, skip_space(s, k));
                let title = group(s, skip_space(s, k));
                k = title.as_ref().map_or(k, |t| t.end as usize + 1);
                while let Some(Frame::Section(l)) = stack.last() {
                    if *l < level {
                        break;
                    }
                    stack.pop();
                    b.close(start as u32);
                }
                let title = title.map(|t| t.start..t.end);
                stack.push(Frame::Section(level));
                b.open(Kind::Section, start as u32, title).map(drop)
            } else if name == "begin" {
                match group(s, skip_space(s, k)) {
                    Some(env) => {
                        let env_name = source[env.start as usize..env.end as usize].to_owned();
                        k = env.end as usize + 1;
                        if VERBATIM.contains(&env_name.as_str()) {
                            let end = format!("\\end{{{env_name}}}");
                            let stop = source[k..]
                                .find(&end)
                                .map_or(s.len(), |p| k + p + end.len());
                            if stop == s.len() && !source[k..].contains(&end) {
                                b.warning(
                                    start as u32..k as u32,
                                    format!("\\begin{{{env_name}}} is never ended"),
                                );
                            }
                            let r = b.leaf(
                                Kind::Element,
                                None,
                                start as u32..stop as u32,
                                Some(env),
                                None,
                            );
                            k = stop;
                            r.map(drop)
                        } else {
                            stack.push(Frame::Environment(env_name));
                            b.open(Kind::Element, start as u32, Some(env)).map(drop)
                        }
                    }
                    None => Ok(()),
                }
            } else if name == "end" {
                if let Some(env) = group(s, skip_space(s, k)) {
                    let env_name = &source[env.start as usize..env.end as usize];
                    k = env.end as usize + 1;
                    let open = stack
                        .iter()
                        .rposition(|f| matches!(f, Frame::Environment(n) if n == env_name));
                    match open {
                        Some(at) => {
                            if stack[at + 1..]
                                .iter()
                                .any(|f| matches!(f, Frame::Environment(_)))
                            {
                                b.warning(
                                    start as u32..k as u32,
                                    format!("\\end{{{env_name}}} closes environments that are still open"),
                                );
                            }
                            while stack.len() > at {
                                stack.pop();
                                b.close(k as u32);
                            }
                        }
                        None => b.warning(
                            start as u32..k as u32,
                            format!("\\end{{{env_name}}} has no matching \\begin"),
                        ),
                    }
                }
                Ok(())
            } else if ENTRIES.contains(&name) {
                let mut at = skip_space(s, k);
                while s.get(at) == Some(&b'[') {
                    let past = skip_optional(s, at);
                    if past == at {
                        break; // never closed
                    }
                    at = skip_space(s, past);
                }
                match group(s, at) {
                    Some(arg) => {
                        k = arg.end as usize + 1;
                        b.leaf(
                            Kind::Scalar,
                            Some(Scalar::String),
                            start as u32..k as u32,
                            Some(start as u32..j as u32),
                            Some(arg),
                        )
                        .map(drop)
                    }
                    None => Ok(()),
                }
            } else {
                Ok(())
            };
            if result.is_err() {
                break;
            }
            i = k.max(j);
        }
        for f in stack.iter().rev() {
            if let Frame::Environment(n) = f {
                b.warning(
                    source.len() as u32..source.len() as u32,
                    format!("\\begin{{{n}}} is never ended"),
                );
            }
        }
        let end = source.trim_end().len() as u32;
        std::mem::replace(b, Builder::new(0, limits, cancel)).finish(end)
    }
}

fn next_char(source: &str, at: usize) -> usize {
    source[at..]
        .chars()
        .next()
        .map_or(at + 1, |c| at + c.len_utf8())
}

fn skip_space(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// Past an optional `[...]` argument at `i` (brackets may nest braces).
fn skip_optional(s: &[u8], i: usize) -> usize {
    if s.get(i) != Some(&b'[') {
        return i;
    }
    let mut depth = 0i32;
    for (k, &c) in s.iter().enumerate().skip(i + 1) {
        match c {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b']' if depth <= 0 => return k + 1,
            b'\\' => {}
            _ => {}
        }
    }
    i
}

/// The inside of a `{...}` group at `i`, braces balanced and escaped braces skipped. A group
/// that never closes, or spans a paragraph break, is not taken.
fn group(s: &[u8], i: usize) -> Option<Span> {
    if s.get(i) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut k = i;
    while k < s.len() {
        match s[k] {
            b'\\' => k += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i as u32 + 1..k as u32);
                }
            }
            b'\n' if s.get(k + 1) == Some(&b'\n') => return None,
            _ => {}
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::dump;
    use super::*;

    fn parse(s: &str) -> Tree {
        Tex.parse(s, &Limits::default(), &AtomicBool::new(false))
    }

    #[test]
    fn sections_nest_inside_the_document() {
        let s = r"\documentclass{article}
\begin{document}
\section{Intro}\label{sec:intro}
See \cite[p.~3]{knuth84,lamport94}.
\subsection*{Detail {with} braces}
\begin{figure}[h]
\includegraphics[width=3cm]{plot.pdf}
\end{figure}
\section[Short]{Methods}
% \section{Commented out}
\input{methods}
\end{document}
";
        let t = parse(s);
        let d = dump(&t, s);
        let lines: Vec<&str> = d.lines().map(|l| l.split(" @").next().unwrap()).collect();
        assert_eq!(
            lines,
            [
                "Document [1]",
                "  Element document [2]",
                "    Section Intro [3]",
                "      Scalar(String) \\label = sec:intro",
                "      Scalar(String) \\cite = knuth84,lamport94",
                "      Section Detail {with} braces [1]",
                "        Element figure [1]",
                "          Scalar(String) \\includegraphics = plot.pdf",
                "    Section Methods [1]",
                "      Scalar(String) \\input = methods",
            ],
            "{d}"
        );
        assert!(t.diagnostics.is_empty(), "{:?}", t.diagnostics);
    }

    #[test]
    fn verbatim_is_not_read_and_mismatches_warn() {
        let s = "\\begin{verbatim}\n\\section{no}\n\\end{verbatim}\n\\begin{a}\\begin{b}\\end{a}\\end{c}\n\\begin{open}";
        let t = parse(s);
        assert!(
            !dump(&t, s).contains("Section"),
            "verbatim content is not parsed"
        );
        let messages: Vec<_> = t.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "\\end{a} closes environments that are still open",
                "\\end{c} has no matching \\begin",
                "\\begin{open} is never ended",
            ]
        );
    }

    #[test]
    fn odd_input_never_panics() {
        for s in [
            "\\",
            "\\section",
            "\\section{",
            "\\begin{",
            "\\end{x",
            "{\\}",
            "é\\é",
            "\\cite[",
            "%\\x",
        ] {
            let _ = parse(s);
        }
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let alphabet = "\\{}[]%*\nabsectionbeginendlabel é"
            .chars()
            .collect::<Vec<_>>();
        for _ in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let s: String = (0..(seed % 40) as usize)
                .map(|i| alphabet[((seed >> (i % 50)) as usize + i * 11) % alphabet.len()])
                .collect();
            let t = parse(&s);
            for id in 0..t.len() as u32 {
                assert!(t.node(id).span.end as usize <= s.len());
            }
        }
    }
}
