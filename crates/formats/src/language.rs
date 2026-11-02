//! Programming languages by extension, file name or shebang. Names are the ones the editor's
//! language list understands (case-insensitive, aliases allowed).

/// Language for a file extension (without the dot).
pub fn by_extension(ext: &str) -> Option<&'static str> {
    #[rustfmt::skip]
    const TABLE: &[(&str, &str)] = &[
        ("c", "c"), ("h", "c"),
        ("cpp", "cpp"), ("cc", "cpp"), ("cxx", "cpp"), ("hpp", "cpp"), ("hh", "cpp"), ("hxx", "cpp"), ("ino", "cpp"),
        ("py", "python"), ("pyi", "python"), ("pyw", "python"),
        ("r", "r"), ("rmd", "r"),
        ("js", "javascript"), ("mjs", "javascript"), ("cjs", "javascript"), ("jsx", "jsx"),
        ("ts", "typescript"), ("mts", "typescript"), ("cts", "typescript"), ("tsx", "tsx"),
        ("rs", "rust"),
        ("go", "go"),
        ("java", "java"),
        ("kt", "kotlin"), ("kts", "kotlin"),
        ("swift", "swift"),
        ("cs", "csharp"),
        ("rb", "ruby"), ("rake", "ruby"), ("gemspec", "ruby"),
        ("php", "php"),
        ("sh", "shell"), ("bash", "shell"), ("zsh", "shell"), ("ksh", "shell"),
        ("ps1", "powershell"), ("psm1", "powershell"),
        ("sql", "sql"),
        ("lua", "lua"),
        ("pl", "perl"), ("pm", "perl"),
        ("scala", "scala"), ("sc", "scala"),
        ("hs", "haskell"),
        ("jl", "julia"),
        ("m", "objective-c"),
        ("f", "fortran"), ("f90", "fortran"), ("f95", "fortran"),
        ("dart", "dart"),
        ("groovy", "groovy"), ("gradle", "groovy"),
        ("clj", "clojure"), ("cljs", "clojure"), ("edn", "clojure"),
        ("erl", "erlang"), ("ex", "elixir"), ("exs", "elixir"),
        ("nix", "nix"),
        ("proto", "protobuf"),
        ("html", "html"), ("htm", "html"), ("vue", "vue"), ("svelte", "svelte"),
        ("css", "css"), ("scss", "scss"), ("sass", "sass"), ("less", "less"),
        ("dockerfile", "dockerfile"),
        ("mk", "makefile"),
        ("cmake", "cmake"),
        ("diff", "diff"), ("patch", "diff"),
        ("wat", "wast"),
    ];
    TABLE
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, l)| *l)
}

/// Language for a file without an extension, by its name.
pub fn by_file_name(name: &str) -> Option<&'static str> {
    const NAMES: &[(&str, &str)] = &[
        ("Dockerfile", "dockerfile"),
        ("Containerfile", "dockerfile"),
        ("Makefile", "makefile"),
        ("GNUmakefile", "makefile"),
        ("CMakeLists.txt", "cmake"),
        ("Rakefile", "ruby"),
        ("Gemfile", "ruby"),
        ("Jenkinsfile", "groovy"),
    ];
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// Language from a vim or Emacs modeline in the first or last five lines (`vim: set ft=python`,
/// `vi: filetype=sh`, `-*- mode: ruby -*-`, `-*- c++ -*-`).
pub fn by_modeline(probe: &[u8]) -> Option<&'static str> {
    let text = String::from_utf8_lossy(&probe[..probe.len().min(64 * 1024)]);
    let lines: Vec<&str> = text.lines().collect();
    let edges = lines.iter().take(5).chain(lines.iter().rev().take(5));
    for line in edges {
        if let Some(name) = vim_filetype(line).or_else(|| emacs_mode(line)) {
            return modeline_language(&name.to_ascii_lowercase());
        }
    }
    None
}

fn vim_filetype(line: &str) -> Option<&str> {
    let at = ["vim:", "vi:", "ex:"]
        .iter()
        .find_map(|m| line.find(m).map(|i| i + m.len()))?;
    line[at..]
        .split(|c: char| c.is_whitespace() || c == ':')
        .find_map(|opt| {
            opt.strip_prefix("ft=")
                .or_else(|| opt.strip_prefix("filetype="))
        })
        .filter(|n| !n.is_empty())
}

fn emacs_mode(line: &str) -> Option<&str> {
    let start = line.find("-*-")? + 3;
    let end = start + line[start..].find("-*-")?;
    let inner = line[start..end].trim();
    if !inner.contains(':') {
        return Some(inner).filter(|n| !n.is_empty());
    }
    inner.split(';').find_map(|part| {
        let (k, v) = part.split_once(':')?;
        k.trim().eq_ignore_ascii_case("mode").then(|| v.trim())
    })
}

fn modeline_language(name: &str) -> Option<&'static str> {
    Some(match name {
        "sh" | "bash" | "zsh" | "shell-script" => "shell",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "c++" | "cpp" => "cpp",
        "make" | "makefile" => "makefile",
        "dockerfile" => "dockerfile",
        "python" => "python",
        "ruby" => "ruby",
        "perl" | "cperl" => "perl",
        "rust" => "rust",
        "go" => "go",
        "java" => "java",
        "lua" => "lua",
        "sql" => "sql",
        "php" => "php",
        "haskell" => "haskell",
        "julia" => "julia",
        "elixir" => "elixir",
        "erlang" => "erlang",
        "cmake" => "cmake",
        "groovy" => "groovy",
        "html" => "html",
        "css" => "css",
        other => return by_extension(other),
    })
}

/// Language from a `#!` first line.
pub fn by_shebang(probe: &[u8]) -> Option<&'static str> {
    let line = probe.strip_prefix(b"#!")?;
    let end = line.iter().position(|&b| b == b'\n').unwrap_or(line.len());
    let line = std::str::from_utf8(&line[..end]).ok()?;
    let mut words = line.split_whitespace();
    let mut interpreter = words.next()?.rsplit('/').next()?;
    if interpreter == "env" {
        // `#!/usr/bin/env -S python3 -u` or `#!/usr/bin/env python3`
        interpreter = words.find(|w| !w.starts_with('-'))?;
    }
    let interpreter = interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match interpreter {
        "python" => "python",
        "sh" | "bash" | "zsh" | "ksh" | "dash" => "shell",
        "node" | "nodejs" | "deno" | "bun" => "javascript",
        "ruby" => "ruby",
        "perl" => "perl",
        "php" => "php",
        "Rscript" => "r",
        "lua" => "lua",
        "pwsh" => "powershell",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(by_extension("py"), Some("python"));
        assert_eq!(by_extension("CPP"), Some("cpp"));
        assert_eq!(by_extension("R"), Some("r"));
        assert_eq!(by_extension("xyz"), None);
    }

    #[test]
    fn shebangs() {
        assert_eq!(
            by_shebang(b"#!/usr/bin/env python3\nprint(1)\n"),
            Some("python")
        );
        assert_eq!(
            by_shebang(b"#!/usr/bin/env -S node --harmony\n"),
            Some("javascript")
        );
        assert_eq!(by_shebang(b"#!/bin/bash\n"), Some("shell"));
        assert_eq!(by_shebang(b"#!/usr/bin/Rscript\n"), Some("r"));
        assert_eq!(by_shebang(b"#!/opt/bin/unknown\n"), None);
        assert_eq!(by_shebang(b"no shebang"), None);
    }

    #[test]
    fn modelines() {
        assert_eq!(
            by_modeline(b"#!/x\n# vim: set ft=python :\n"),
            Some("python")
        );
        assert_eq!(by_modeline(b"// vi: filetype=sh\n"), Some("shell"));
        assert_eq!(
            by_modeline(b"; -*- mode: C++; tab-width: 4 -*-\n"),
            Some("cpp")
        );
        assert_eq!(by_modeline(b"# -*- ruby -*-\n"), Some("ruby"));
        let tail = format!("{}# vim:ft=lua\n", "x\n".repeat(50));
        assert_eq!(
            by_modeline(tail.as_bytes()),
            Some("lua"),
            "last lines count too"
        );
        let middle = format!("{}# vim:ft=lua\n{}", "x\n".repeat(20), "y\n".repeat(20));
        assert_eq!(
            by_modeline(middle.as_bytes()),
            None,
            "a modeline in the middle does not"
        );
        assert_eq!(by_modeline(b"vim: set ts=4\n"), None);
        assert_eq!(by_modeline(b"-*- coding: utf-8 -*-\n"), None);
    }

    #[test]
    fn names() {
        assert_eq!(by_file_name("Makefile"), Some("makefile"));
        assert_eq!(by_file_name("README"), None);
    }
}
