//! One `Spec` per format: its names, extensions, the views it offers and whether it is edited.
//!
//! The first view in `views` is the one a file opens on. `Source` is always offered for
//! text and `Hex` for bytes, even when they are not listed, so no file is a dead end.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    /// The text as written.
    Source,
    /// The text indented and highlighted by its structure; display only.
    Formatted,
    Tree,
    Table,
    Preview,
    Outline,
    Cells,
    Hex,
    /// What a file is: its type, size and the facts its header gives.
    Summary,
    /// Readable text runs found in the bytes, with their offsets.
    Strings,
    Overview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Text,
    Binary,
}

#[derive(Debug, Serialize)]
pub struct Spec {
    pub id: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub kind: Kind,
    /// Highlighting language when it is fixed by the format (`code` resolves it per file).
    pub language: Option<&'static str>,
    pub views: &'static [View],
    pub editable: bool,
}

use View::*;

#[rustfmt::skip]
pub static SPECS: &[Spec] = &[
    // Structured text: edited, with a structure view beside the source.
    Spec { id: "markdown", name: "Markdown", extensions: &["md", "markdown", "mdown", "mkd"], kind: Kind::Text, language: Some("markdown"), views: &[Preview, Source, Outline], editable: true },
    Spec { id: "tex", name: "TeX", extensions: &["tex", "sty", "cls", "ltx", "dtx"], kind: Kind::Text, language: Some("latex"), views: &[Source, Outline], editable: true },
    Spec { id: "bibtex", name: "BibTeX", extensions: &["bib"], kind: Kind::Text, language: Some("bibtex"), views: &[Table, Source], editable: true },
    Spec { id: "json", name: "JSON", extensions: &["json", "geojson", "har", "webmanifest", "jsonc"], kind: Kind::Text, language: Some("json"), views: &[Tree, Source, Formatted], editable: true },
    Spec { id: "jsonl", name: "JSON Lines", extensions: &["jsonl", "ndjson"], kind: Kind::Text, language: Some("json"), views: &[Tree, Source], editable: true },
    Spec { id: "xml", name: "XML", extensions: &["xml", "xsd", "xsl", "xslt", "svg", "rss", "atom", "plist", "xhtml", "kml", "gpx"], kind: Kind::Text, language: Some("xml"), views: &[Tree, Source, Formatted], editable: true },
    Spec { id: "yaml", name: "YAML", extensions: &["yaml", "yml"], kind: Kind::Text, language: Some("yaml"), views: &[Tree, Source], editable: true },
    Spec { id: "toml", name: "TOML", extensions: &["toml"], kind: Kind::Text, language: Some("toml"), views: &[Tree, Source], editable: true },
    Spec { id: "ini", name: "INI", extensions: &["ini", "cfg", "conf", "properties"], kind: Kind::Text, language: Some("properties"), views: &[Tree, Source], editable: true },
    Spec { id: "csv", name: "CSV", extensions: &["csv"], kind: Kind::Text, language: None, views: &[Table, Source], editable: true },
    Spec { id: "tsv", name: "TSV", extensions: &["tsv", "tab"], kind: Kind::Text, language: None, views: &[Table, Source], editable: true },
    Spec { id: "code", name: "Source code", extensions: &[], kind: Kind::Text, language: None, views: &[Source, Outline], editable: true },
    Spec { id: "text", name: "Plain text", extensions: &["txt", "text", "log", "out", "err"], kind: Kind::Text, language: None, views: &[Source], editable: true },
    // Read-only text with a domain view.
    Spec { id: "notebook", name: "Jupyter notebook", extensions: &["ipynb"], kind: Kind::Text, language: Some("json"), views: &[Cells, Tree, Source], editable: false },
    // Binary data: never edited.
    Spec { id: "parquet", name: "Parquet", extensions: &["parquet", "parq", "pq"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "arrow", name: "Arrow IPC / Feather", extensions: &["arrow", "feather", "ipc"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "avro", name: "Avro", extensions: &["avro"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "orc", name: "Apache ORC", extensions: &["orc"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "hdf5", name: "HDF5", extensions: &["h5", "hdf5", "he5", "nc"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "npz", name: "NumPy archive", extensions: &["npz"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "xlsx", name: "Excel workbook", extensions: &["xlsx", "xlsm", "xltx", "xltm"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "docx", name: "Word document", extensions: &["docx", "docm", "dotx"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "odt", name: "OpenDocument text", extensions: &["odt", "ott"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "npy", name: "NumPy array", extensions: &["npy"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "sqlite", name: "SQLite database", extensions: &["sqlite", "sqlite3", "db", "db3"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "pdf", name: "PDF", extensions: &["pdf"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "png", name: "PNG image", extensions: &["png"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "jpeg", name: "JPEG image", extensions: &["jpg", "jpeg"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "gif", name: "GIF image", extensions: &["gif"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "webp", name: "WebP image", extensions: &["webp"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "bmp", name: "BMP image", extensions: &["bmp"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "ico", name: "Icon", extensions: &["ico", "cur"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "msgpack", name: "MessagePack", extensions: &["msgpack", "mpk"], kind: Kind::Binary, language: Some("json"), views: &[Tree, Source, Hex], editable: false },
    Spec { id: "protobuf", name: "Protobuf message", extensions: &["pb", "binpb", "protobuf"], kind: Kind::Binary, language: Some("json"), views: &[Tree, Source, Hex], editable: false },
    Spec { id: "tiff", name: "TIFF image", extensions: &["tif", "tiff"], kind: Kind::Binary, language: None, views: &[Preview, Hex], editable: false },
    Spec { id: "psd", name: "Photoshop document", extensions: &["psd", "psb"], kind: Kind::Binary, language: None, views: &[Hex], editable: false },
    Spec { id: "heif", name: "HEIF image", extensions: &["heic", "heif", "hif"], kind: Kind::Binary, language: None, views: &[Hex], editable: false },
    Spec { id: "avif", name: "AVIF image", extensions: &["avif"], kind: Kind::Binary, language: None, views: &[Hex], editable: false },
    Spec { id: "zip", name: "ZIP archive", extensions: &["zip", "jar", "war", "apk", "pptx", "ods", "odp", "epub", "whl", "nupkg", "vsix"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "tar", name: "tar archive", extensions: &["tar"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "gzip", name: "gzip", extensions: &["gz", "tgz"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "zstd", name: "Zstandard", extensions: &["zst", "tzst"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "xz", name: "xz", extensions: &["xz", "txz"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "bzip2", name: "bzip2", extensions: &["bz2", "tbz2"], kind: Kind::Binary, language: None, views: &[Table, Hex], editable: false },
    Spec { id: "elf", name: "ELF executable", extensions: &["so", "o"], kind: Kind::Binary, language: None, views: &[Hex], editable: false },
    Spec { id: "binary", name: "Binary data", extensions: &[], kind: Kind::Binary, language: None, views: &[Hex], editable: false },
];

pub fn by_id(id: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.id == id)
}

/// The spec whose extension list contains `ext` (ASCII case-insensitive).
pub fn by_extension(ext: &str) -> Option<&'static Spec> {
    SPECS
        .iter()
        .find(|s| s.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// The spec of a file known by its whole name: dotenv and tool configuration as INI, lock files
/// as the TOML or JSON they are written in. Matched exactly; dotenv also as `.env.<anything>`.
pub fn by_file_name(name: &str) -> Option<&'static Spec> {
    let id = match name {
        ".env" | ".editorconfig" | ".gitconfig" | ".npmrc" => "ini",
        n if n.starts_with(".env.") => "ini",
        "Cargo.lock" | "poetry.lock" | "uv.lock" | "pdm.lock" | "Pipfile" => "toml",
        "Pipfile.lock" | "composer.lock" | "flake.lock" | "deno.lock" => "json",
        _ => return None,
    };
    by_id(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_extensions_are_unique() {
        let mut ids = HashSet::new();
        let mut exts = HashSet::new();
        for s in SPECS {
            assert!(ids.insert(s.id), "duplicate id {}", s.id);
            for e in s.extensions {
                assert!(exts.insert(*e), "extension {e} claimed twice");
                assert_eq!(*e, e.to_ascii_lowercase(), "extensions are lower-case");
            }
        }
    }

    #[test]
    fn every_spec_opens_on_a_view_and_binary_is_never_edited() {
        for s in SPECS {
            assert!(!s.views.is_empty(), "{} has no view", s.id);
            if s.kind == Kind::Binary {
                assert!(!s.editable, "{} is binary and editable", s.id);
                assert!(s.views.contains(&View::Hex), "{} lacks Hex", s.id);
            }
        }
    }

    #[test]
    fn lookups() {
        assert_eq!(by_extension("JSON").unwrap().id, "json");
        assert_eq!(by_id("markdown").unwrap().name, "Markdown");
        assert!(by_extension("nope").is_none());
    }
}
