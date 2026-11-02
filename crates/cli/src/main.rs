//! `isolinear-cli inspect FILE… [--json]`: the same detection the window uses, in a terminal.
//! `isolinear-cli preview FILE`: the HTML page the file managers' quick previews show.

use std::path::Path;
use std::process::ExitCode;

use isolinear_core::{document, encoding};
use isolinear_formats::detect;

const USAGE: &str = "usage: isolinear-cli inspect [--json] FILE...\n       isolinear-cli tree FILE\n       isolinear-cli preview FILE";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = match args.split_first().map(|(c, r)| (c.as_str(), r)) {
        Some(("inspect", rest)) => rest,
        Some(("tree", [file])) => return tree(Path::new(file)),
        Some(("preview", [file])) => {
            print!("{}", isolinear_preview::html(Path::new(file)));
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let json = rest.iter().any(|a| a == "--json");
    let files: Vec<&String> = rest.iter().filter(|a| *a != "--json").collect();
    if files.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for file in files {
        match inspect(Path::new(file)) {
            Ok(report) if json => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
            Ok(report) => print_plain(&report),
            Err(e) => {
                eprintln!("{file}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[derive(serde::Serialize)]
struct Report {
    path: String,
    size: u64,
    tier: isolinear_core::limits::Tier,
    format: &'static str,
    format_name: &'static str,
    language: Option<&'static str>,
    reason: String,
    views: Vec<isolinear_formats::View>,
    suggested: isolinear_formats::View,
    editable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<encoding::Encoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_ending: Option<encoding::LineEnding>,
}

fn inspect(path: &Path) -> std::io::Result<Report> {
    let opened = document::open(path)?;
    let d = detect(&opened.name, &opened.probe);
    let text = (d.format.kind == isolinear_formats::registry::Kind::Text)
        .then(|| encoding::decode(opened.bytes.as_deref().unwrap_or(&opened.probe)));
    Ok(Report {
        path: opened.path.display().to_string(),
        size: opened.size,
        tier: opened.tier,
        format: d.format.id,
        format_name: d.format.name,
        language: d.language,
        reason: d.reason,
        views: d.views,
        suggested: d.suggested,
        editable: d.editable,
        encoding: text.as_ref().map(|t| t.encoding),
        line_ending: text.as_ref().map(|t| t.line_ending),
    })
}

fn print_plain(r: &Report) {
    let lang = r.language.map(|l| format!(" ({l})")).unwrap_or_default();
    println!(
        "{}: {}{lang}, {} bytes, {}",
        r.path, r.format_name, r.size, r.reason
    );
    println!(
        "  views: {:?}, opens on {:?}, {}",
        r.views,
        r.suggested,
        if r.editable { "editable" } else { "read-only" }
    );
    if let (Some(e), Some(le)) = (r.encoding, r.line_ending) {
        println!("  encoding: {e:?}, line endings: {le:?}");
    }
}

/// Prints the structure tree of a text file, as the Tree view would show it.
fn tree(path: &Path) -> ExitCode {
    let opened = match document::open(path) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let d = detect(&opened.name, &opened.probe);
    let Some(format) = isolinear_formats::tree::for_format(d.format.id, d.language) else {
        eprintln!("{}: {} has no tree view", path.display(), d.format.name);
        return ExitCode::FAILURE;
    };
    let Some(bytes) = opened.bytes else {
        eprintln!("{}: too large for a tree", path.display());
        return ExitCode::FAILURE;
    };
    let text = encoding::decode(&bytes).text;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let tree = format.parse(&text, &Default::default(), &cancel);
    print!("{}", isolinear_formats::tree::dump(&tree, &text));
    if tree.errors() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
