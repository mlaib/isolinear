use super::super::{ROOT, dump};
use super::*;

fn parse(language: &str, source: &str) -> Tree {
    for_language(language)
        .unwrap()
        .parse(source, &Limits::default(), &AtomicBool::new(false))
}
fn spans(s: &str, t: &Tree) {
    let mut last = 0;
    for id in 0..t.len() as u32 {
        let n = t.node(id);
        assert!(n.span.start >= last, "{s:?}: {n:?}");
        last = n.span.start;
        for r in std::iter::once(&n.span)
            .chain(n.name.iter())
            .chain(n.value.iter())
        {
            assert!(
                r.start <= r.end && r.end as usize <= s.len(),
                "{s:?}: {n:?}"
            );
            assert!(
                s.is_char_boundary(r.start as usize) && s.is_char_boundary(r.end as usize),
                "{s:?}: {n:?}"
            );
            assert!(
                n.span.start <= r.start && r.end <= n.span.end,
                "{s:?}: {n:?}"
            );
        }
        if let Some(p) = n.parent() {
            assert!(
                t.node(p).span.start <= n.span.start && n.span.end <= t.node(p).span.end,
                "{s:?}: {n:?}"
            );
        }
    }
}
fn names<'a>(s: &'a str, t: &Tree) -> Vec<&'a str> {
    (1..t.len() as u32)
        .map(|id| {
            t.node(id)
                .name
                .as_ref()
                .map(|r| &s[r.start as usize..r.end as usize])
                .unwrap_or("")
        })
        .collect()
}
macro_rules! fixture {
    ($test:ident, $lang:literal, $file:literal, [$($name:literal),*]) => {
        #[test]
        fn $test() {
            let source = include_str!(concat!("../../../../../tests/fixtures/tree/code/", $file));
            let t = parse($lang, source);
            spans(source, &t);
            assert_eq!(names(source, &t), [$($name),*], "{}", dump(&t, source));
            assert_eq!(dump(&t, source), include_str!(concat!("../../../../../tests/fixtures/tree/code/", $file, ".outline")));
        }
    };
}
fixture!(
    c,
    "c",
    "sample.c",
    ["Packet", "Header", "sum", "unfinished"]
);
fixture!(
    cpp,
    "cpp",
    "sample.cpp",
    ["tools", "Counter", "next", "unfinished"]
);
fixture!(
    csharp,
    "csharp",
    "sample.cs",
    ["Counter", "Next", "Unfinished"]
);
fixture!(
    java,
    "java",
    "Sample.java",
    ["Counter", "next", "Unfinished"]
);
fixture!(
    javascript,
    "javascript",
    "sample.js",
    ["Counter", "next", "worker", "nested", "other", "unfinished"]
);
fixture!(jsx, "jsx", "sample.jsx", ["View", "label", "unfinished"]);
fixture!(
    typescript,
    "typescript",
    "sample.ts",
    ["Item", "Counter", "next", "worker", "nested", "unfinished"]
);
fixture!(
    tsx,
    "tsx",
    "sample.tsx",
    ["View", "render", "Panel", "unfinished"]
);
fixture!(
    rust,
    "rust",
    "sample.rs",
    [
        "tools",
        "Counter",
        "impl Display for Counter",
        "fmt",
        "unfinished"
    ]
);
fixture!(
    go,
    "go",
    "sample.go",
    ["Counter", "Add", "Delta", "unfinished"]
);
fixture!(
    kotlin,
    "kotlin",
    "sample.kt",
    ["Counter", "next", "unfinished"]
);
fixture!(
    swift,
    "swift",
    "sample.swift",
    ["Counter", "next", "unfinished"]
);
fixture!(
    scala,
    "scala",
    "sample.scala",
    ["Tools", "Counter", "next", "unfinished"]
);
fixture!(
    dart,
    "dart",
    "sample.dart",
    ["Counter", "next", "unfinished"]
);
fixture!(php, "php", "sample.php", ["Counter", "next", "unfinished"]);
fixture!(
    groovy,
    "groovy",
    "sample.groovy",
    ["Counter", "next", "unfinished"]
);
fixture!(
    objective_c,
    "objective-c",
    "sample.m",
    ["Counter", "next", "unfinished"]
);
fixture!(r, "r", "sample.r", ["outer", "inner", "unfinished"]);
fixture!(
    shell,
    "shell",
    "sample.sh",
    ["outer", "inner", "unfinished"]
);
fixture!(
    powershell,
    "powershell",
    "sample.ps1",
    ["Get-Items", "Convert-Item", "Unfinished"]
);
fixture!(
    perl,
    "perl",
    "sample.pl",
    ["Tools", "next_value", "unfinished"]
);
fixture!(
    protobuf,
    "protobuf",
    "sample.proto",
    ["Packet", "Header", "Reader", "Read", "Unfinished"]
);
fixture!(
    python,
    "python",
    "sample.py",
    ["Counter", "next", "nested", "unfinished"]
);
fixture!(
    ruby,
    "ruby",
    "sample.rb",
    ["Tools", "Counter", "next_value", "unfinished"]
);
fixture!(lua, "lua", "sample.lua", ["outer", "inner", "unfinished"]);
fixture!(
    elixir,
    "elixir",
    "sample.ex",
    ["Tools", "next", "helper", "unfinished"]
);
fixture!(
    julia,
    "julia",
    "sample.jl",
    ["Tools", "Counter", "next", "inner", "unfinished"]
);
fixture!(
    sql,
    "sql",
    "sample.sql",
    ["app.items", "app.labels", "next_value", "unfinished"]
);
fixture!(
    makefile,
    "makefile",
    "Makefile",
    ["all", "build", "test", "unfinished"]
);
fixture!(
    dockerfile,
    "dockerfile",
    "Dockerfile",
    ["build", "runtime", "unfinished"]
);
fixture!(
    cmake,
    "cmake",
    "sample.cmake",
    ["build_item", "test_item", "unfinished"]
);

fn expect(language: &str, source: &str, expected: &[&str]) -> Tree {
    let t = parse(language, source);
    spans(source, &t);
    assert_eq!(names(source, &t), expected, "{}", dump(&t, source));
    t
}

#[test]
fn controls_calls_and_type_references_are_not_definitions() {
    let s = "struct Thing *value;\nint run(int x) {\n if (x) { call(); }\n for (;;) { call(); }\n while (x) { --x; }\n switch(x) { default: break; }\n return call();\n}\n";
    expect("c", s, &["run"]);
    expect(
        "java",
        "class A { void run() { try { call(); } catch (Exception e) { call(); } synchronized(this) {} new Other() {}; object.method(); } }",
        &["A", "run"],
    );
}

#[test]
fn assigned_arrows_expressions_generators_and_functions() {
    let s = "const one = x => x + 1;\nlet two = async (x) => { return x; };\nvar three = function named(x) { return x; };\nfunction* four() { yield 4; }";
    expect("javascript", s, &["one", "two", "three", "four"]);
    expect(
        "typescript",
        "const identity = <T>(x: T): T => x;\nclass Box { get value(): number { return 1; } }",
        &["identity", "Box", "value"],
    );
}

#[test]
fn raw_strings_comments_regexes_and_heredocs_are_opaque() {
    let cases = [
        (
            "rust",
            "/* outer /* fn ghost() {} */ still comment */\nconst S: &str = r###\"fn ghost() {}\"###;\nfn real<'a>(x: &'a str) {}",
        ),
        (
            "cpp",
            "const char *s = R\"tag(void ghost() { \" })tag\";\nvoid real() {}",
        ),
        (
            "javascript",
            "const re = /function ghost\\(\\) \\{[}]/g; const s = `function ghost() { ${1} }`; function real() {}",
        ),
        (
            "python",
            "note = '''def ghost():\n    pass\n'''\ndef real():\n    pass\n",
        ),
        (
            "lua",
            "--[=[ function ghost() end ]=]\nlocal s = [==[function ghost() end]==]\nfunction real()\nend\n",
        ),
        (
            "julia",
            "#= function ghost() #= nested =# end =#\nfunction real()\nend\n",
        ),
        (
            "shell",
            "cat <<'DATA'\nghost() { echo fake; }\nDATA\nreal() {}\n",
        ),
        (
            "php",
            "<?php\n$s = <<<'DATA'\nfunction ghost() {}\nDATA;\nfunction real() {}\n",
        ),
        (
            "powershell",
            "$s = @\"\n\" function Ghost {} \"\n\"@\nfunction real {}\n",
        ),
        (
            "perl",
            "my $s = q{sub ghost {}}; my $r = qr/sub ghost {}/; sub real {}",
        ),
        ("ruby", "=begin\ndef ghost\nend\n=end\ndef real\nend\n"),
        (
            "cmake",
            "#[=[ function(ghost) ]=]\nset(S [==[ function(ghost) ]==])\nfunction(real)\nendfunction()\n",
        ),
    ];
    for (language, source) in cases {
        expect(language, source, &["real"]);
    }
}

#[test]
fn unicode_crlf_and_decorators_use_source_byte_spans() {
    let s = "# 😀\r\n@decorate\r\nclass Café:\r\n    @cached\r\n    async def méthode(self):\r\n        return '日'\r\n\r\n";
    let t = expect("python", s, &["Café", "méthode"]);
    assert_eq!(t.node(1).span.start as usize, s.find('@').unwrap());
    assert_eq!(
        t.node(2).span.start as usize,
        s.find("    @cached").unwrap()
    );
    assert_eq!(t.node(2).parent(), Some(1));
    let at = s.find("return").unwrap() as u32;
    assert_eq!(t.path_at(at), [ROOT, 1, 2]);
    let s = "def outer():\n    note = '''one\n    two\n    three'''\n";
    let t = expect("python", s, &["outer"]);
    assert_eq!(t.node(1).span.end as usize, s.trim_end().len());
}

#[test]
fn same_line_nested_definitions_and_siblings_keep_source_order() {
    let s = "class A { class B { void f() {} } } class C {}";
    let t = expect("java", s, &["A", "B", "f", "C"]);
    assert_eq!(t.node(2).parent(), Some(1));
    assert_eq!(t.node(3).parent(), Some(2));
    assert_eq!(t.node(4).parent(), Some(ROOT));
}

#[test]
fn flat_formats_cover_all_requested_statement_kinds() {
    let s = "CREATE SCHEMA app; CREATE TABLE app.data(x INT); CREATE OR REPLACE VIEW v AS SELECT 1; CREATE FUNCTION f() RETURNS INT AS $$ BEGIN CREATE TABLE ghost(x int); END $$ LANGUAGE SQL; CREATE PROCEDURE p() AS 'CREATE VIEW ghost'; CREATE UNIQUE INDEX i ON app.data(x); CREATE TRIGGER tr AFTER INSERT ON app.data; CREATE TYPE typ AS ENUM ('a');";
    let t = expect(
        "sql",
        s,
        &["app", "app.data", "v", "f", "p", "i", "tr", "typ"],
    );
    assert_eq!(t.node(ROOT).child_count(), 8);
    expect(
        "dockerfile",
        "FROM alpine AS build-stage\nRUN echo ready\nFROM alpine\nRUN echo final\n",
        &["build-stage"],
    );
    expect(
        "makefile",
        "VALUE := ignored\nall test : input\n\techo 'fake: target'\n",
        &["all test"],
    );
    let t = expect(
        "cmake",
        "FUNCTION(outer)\n MACRO(inner)\n ENDMACRO()\nENDFUNCTION()",
        &["outer", "inner"],
    );
    assert_eq!(t.node(ROOT).child_count(), 2);
}

#[test]
fn registry_is_complete_and_unknown_languages_stay_unsupported() {
    assert_eq!(LANGUAGES.len(), 31);
    for language in LANGUAGES {
        assert!(for_language(&language.to_uppercase()).is_some());
    }
    for language in ["", "unknown", "html", "haskell", "css"] {
        assert!(for_language(language).is_none());
    }
}

#[test]
fn limits_and_cancellation_are_explicit() {
    let cancel = AtomicBool::new(false);
    let parser = for_language("c").unwrap();
    let s = "int f() {}\n".repeat(50);
    let t = parser.parse(
        &s,
        &Limits {
            max_nodes: 7,
            ..Limits::default()
        },
        &cancel,
    );
    assert_eq!(t.len(), 7);
    assert_eq!(t.stopped, Some(Stop::Nodes));
    spans(&s, &t);
    let s = "class A {\n".repeat(30);
    let t = for_language("java").unwrap().parse(
        &s,
        &Limits {
            max_depth: 4,
            max_diagnostics: 2,
            ..Limits::default()
        },
        &cancel,
    );
    assert_eq!(t.stopped, Some(Stop::Depth));
    assert!(t.diagnostics.len() <= 2);
    assert!(t.more_diagnostics > 0);
    spans(&s, &t);
    for language in LANGUAGES {
        for source in ["", "# comment", "function f() {}", "\"unfinished"] {
            let t = for_language(language).unwrap().parse(
                source,
                &Limits::default(),
                &AtomicBool::new(true),
            );
            assert_eq!(t.stopped, Some(Stop::Cancelled), "{language}");
            spans(source, &t);
        }
    }
}

#[test]
fn malformed_delimiters_keep_the_recognized_prefix() {
    for (language, source, expected) in [
        (
            "javascript",
            "function good() {}\nfunction broken() {",
            vec!["good", "broken"],
        ),
        (
            "ruby",
            "def good\nend\ndef broken\n",
            vec!["good", "broken"],
        ),
        (
            "cmake",
            "function(good)\nendfunction()\nmacro(broken)",
            vec!["good", "broken"],
        ),
        ("c", "int good() {}\n/* unfinished", vec!["good"]),
    ] {
        let t = expect(language, source, &expected);
        assert!(!t.diagnostics.is_empty(), "{language}");
    }
}

#[test]
fn randomized_and_mutated_inputs_in_every_language() {
    use super::super::json::tests::Rng;
    let alphabet = [
        "x", "f", "class", "def", "function", "fn", "struct", "end", "if", "create", " ", "\n",
        "\r\n", "\t", "{", "}", "(", ")", "[", "]", ";", ":", "#", "//", "/*", "*/", "\"", "'",
        "`", "\\", "=", ">", "<", "@", "é", "日", "😀", "\u{feff}",
    ];
    let seeds = [
        "class A { fn f() { function inner() {} } }",
        "def outer():\n    def inner():\n        pass\n",
        "function(real)\nendfunction()\n",
        "CREATE TABLE t (id INT);",
        "sub f { my $s = q{ghost}; }",
    ];
    let mut rng = Rng(0xA15C_794D_7FE8_8031);
    for round in 0..31_000 {
        let language = LANGUAGES[round % LANGUAGES.len()];
        let mut parts: Vec<String> = if round % 2 == 0 {
            seeds[rng.below(seeds.len())]
                .chars()
                .map(|c| c.to_string())
                .collect()
        } else {
            (0..rng.below(50))
                .map(|_| alphabet[rng.below(alphabet.len())].to_owned())
                .collect()
        };
        for _ in 0..rng.below(6) {
            let index = rng.below(parts.len() + 1);
            if index < parts.len() && rng.below(2) == 0 {
                parts.remove(index);
            } else {
                parts.insert(index, alphabet[rng.below(alphabet.len())].to_owned());
            }
        }
        let source = parts.concat();
        let t = parse(language, &source);
        spans(&source, &t);
        for d in &t.diagnostics {
            assert!(
                source
                    .get(d.span.start as usize..d.span.end as usize)
                    .is_some(),
                "{source:?}: {d:?}"
            );
        }
        let _ = dump(&t, &source);
        let _ = t.path_at(rng.below(source.len() + 1) as u32);
    }
}

#[test]
#[ignore = "release performance measurement"]
fn two_megabyte_c_outline() {
    let unit = "/* function-looking text: void ghost() {} */\nint calculate(int value) {\n    if (value > 0) { return value + 1; }\n    return 0;\n}\n";
    let source = unit.repeat((2usize * 1024 * 1024).div_ceil(unit.len()));
    let start = std::time::Instant::now();
    let t = parse("c", &source);
    let elapsed = start.elapsed();
    assert_eq!(t.stopped, None);
    assert_eq!(
        t.node(ROOT).child_count() as usize,
        source.len() / unit.len()
    );
    assert!(t.diagnostics.is_empty());
    println!(
        "{} bytes, {} symbols, {:.2} ms",
        source.len(),
        t.len() - 1,
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(elapsed < std::time::Duration::from_secs(1));
}

#[test]
fn source_token_line_and_header_limits_report_truncation() {
    let parser = for_language("rust").unwrap();
    let cancel = AtomicBool::new(false);
    for (source, message) in [
        (" ".repeat(32 * 1024 * 1024 + 1), "source limit"),
        ("x;".repeat(500_001), "token limit"),
        ("\n".repeat(1_000_001), "line limit"),
    ] {
        let t = parser.parse(&source, &Limits::default(), &cancel);
        assert_eq!(t.stopped, Some(Stop::Nodes));
        assert!(t.diagnostics.iter().any(|d| d.message.contains(message)));
        spans(&source, &t);
    }
    let s = format!("fn long({}) {{}}", "parameter, ".repeat(600));
    let t = parse("rust", &s);
    assert!(
        t.diagnostics
            .iter()
            .any(|d| d.message.contains("512-token"))
    );
}

#[test]
fn cancellation_during_a_long_scan() {
    let source = format!("/*{}", "é".repeat(8_000_000));
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let job = scope.spawn(|| {
            for_language("c")
                .unwrap()
                .parse(&source, &Limits::default(), &cancel)
        });
        std::thread::sleep(std::time::Duration::from_millis(2));
        cancel.store(true, Ordering::Relaxed);
        let t = job.join().unwrap();
        assert_eq!(t.stopped, Some(Stop::Cancelled));
        spans(&source, &t);
    });
}

#[test]
fn aliases_and_qualified_names() {
    expect("go", "type Name string\nfunc real() {}", &["Name", "real"]);
    expect(
        "swift",
        "typealias Name = String\nfunc real() {}",
        &["Name", "real"],
    );
    expect(
        "kotlin",
        "typealias Name = String\nfun real() {}",
        &["Name", "real"],
    );
    expect("r", "read.data <- function(x) { x }", &["read.data"]);
    expect("lua", "function Table:method()\nend\n", &["Table:method"]);
    expect("ruby", "def self.ready?\n  true\nend\n", &["self.ready?"]);
    expect(
        "objective-c",
        "@implementation Unfinished\n- (void)method {}",
        &["Unfinished", "method"],
    );
}

#[test]
fn long_qualified_r_name_is_scanned_once() {
    let source = format!("{}last\n", "part.".repeat(10_000));
    expect("r", &source, &[]);
}
