//! Signature, then extension, then content, then text-or-bytes. The reason is kept so the
//! window can say why a file opened the way it did.

use serde::Serialize;

use crate::language;
use crate::registry::{self, Kind, Spec, View};
use crate::signature;

#[derive(Debug, Serialize)]
pub struct Detection {
    pub format: &'static Spec,
    /// Highlighting language for text; `None` for plain text and bytes.
    pub language: Option<&'static str>,
    pub reason: String,
    pub views: Vec<View>,
    pub suggested: View,
    pub editable: bool,
}

/// Detects from the file name and the first bytes. `probe` may be the whole file.
pub fn detect(name: &str, probe: &[u8]) -> Detection {
    if let Some(id) = signature::identify(probe) {
        // ZIP holds many formats; the extension says which (an .xlsx is a ZIP of XML parts).
        let id = match (id, extension(name).map(str::to_ascii_lowercase).as_deref()) {
            ("zip", Some("xlsx" | "xlsm" | "xltx" | "xltm")) => "xlsx",
            ("zip", Some("docx" | "docm" | "dotx")) => "docx",
            ("zip", Some("odt" | "ott")) => "odt",
            ("zip", Some("npz")) => "npz",
            // "ORC" is too short to trust without the extension.
            _ => id,
        };
        let spec = registry::by_id(id).expect("every signature names a spec");
        return finish(spec, None, format!("{} by signature", spec.name));
    }
    let ext = extension(name);
    let binary = signature::looks_binary(probe);
    if let Some(spec) = registry::by_file_name(name)
        && !binary
    {
        return finish(spec, spec.language, format!("{} by file name", spec.name));
    }
    if let Some(e) = ext {
        if let Some(spec) = registry::by_extension(e) {
            if spec.id == "text"
                && !binary
                && let Some(lang) = language::by_modeline(probe)
            {
                let code = registry::by_id("code").unwrap();
                return finish(code, Some(lang), format!("{lang} by modeline"));
            }
            if spec.kind == Kind::Text && binary {
                return finish(
                    registry::by_id("binary").unwrap(),
                    None,
                    format!(".{e} but the content is binary"),
                );
            }
            return finish(
                spec,
                spec.language,
                format!("{} by extension .{e}", spec.name),
            );
        }
        if let Some(lang) = language::by_extension(e) {
            if binary {
                return finish(
                    registry::by_id("binary").unwrap(),
                    None,
                    format!(".{e} but the content is binary"),
                );
            }
            return finish(
                registry::by_id("code").unwrap(),
                Some(lang),
                format!("{lang} by extension .{e}"),
            );
        }
    }
    if let Some(lang) = language::by_file_name(name) {
        return finish(
            registry::by_id("code").unwrap(),
            Some(lang),
            format!("{lang} by file name"),
        );
    }
    if binary {
        return finish(
            registry::by_id("binary").unwrap(),
            None,
            "binary content".into(),
        );
    }
    if let Some(lang) = language::by_modeline(probe) {
        return finish(
            registry::by_id("code").unwrap(),
            Some(lang),
            format!("{lang} by modeline"),
        );
    }
    if let Some(lang) = language::by_shebang(probe) {
        return finish(
            registry::by_id("code").unwrap(),
            Some(lang),
            format!("{lang} by shebang"),
        );
    }
    if let Some((id, why)) = by_content(probe) {
        let spec = registry::by_id(id).unwrap();
        return finish(
            spec,
            spec.language,
            format!("{} by content ({why})", spec.name),
        );
    }
    let spec = registry::by_id("text").unwrap();
    finish(spec, None, "text".into())
}

/// Structured text without a telling extension, from its first non-blank bytes.
fn by_content(probe: &[u8]) -> Option<(&'static str, &'static str)> {
    let trimmed = probe
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|i| &probe[i..])?;
    if trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<!DOCTYPE") {
        return Some(("xml", "XML declaration"));
    }
    if trimmed.starts_with(b"{") || trimmed.starts_with(b"[") {
        return Some(("json", "starts with a brace"));
    }
    if trimmed.starts_with(b"---\n")
        || trimmed.starts_with(b"---\r\n")
        || trimmed.starts_with(b"%YAML")
    {
        return Some(("yaml", "document marker"));
    }
    if trimmed.starts_with(b"<") && trimmed.get(1).is_some_and(|b| b.is_ascii_alphabetic()) {
        return Some(("xml", "starts with a tag"));
    }
    None
}

fn finish(spec: &'static Spec, language: Option<&'static str>, reason: String) -> Detection {
    let mut views = spec.views.to_vec();
    if crate::tree::for_format(spec.id, language).is_none() {
        views.retain(|v| !matches!(v, View::Tree | View::Outline));
    }
    if spec.kind == Kind::Binary && !crate::table::supports(spec.id) {
        views.retain(|v| *v != View::Table);
    }
    let preview_ready = match spec.id {
        "pdf" => crate::pdf::READY,
        "docx" | "odt" => crate::document::READY,
        _ => true,
    };
    if !preview_ready {
        views.retain(|v| *v != View::Preview);
    }
    // Every text gets Source and every binary file Summary, Strings and Hex, in the format's own
    // order when it lists them, else at the end (Summary, Strings, Hex).
    match spec.kind {
        Kind::Text if !views.contains(&View::Source) => views.push(View::Source),
        Kind::Binary => {
            if !views.contains(&View::Summary) {
                let at = views
                    .iter()
                    .position(|v| *v == View::Hex)
                    .unwrap_or(views.len());
                views.insert(at, View::Summary);
            }
            if !views.contains(&View::Hex) {
                views.push(View::Hex);
            }
            if !views.contains(&View::Strings) {
                let at = views.iter().position(|v| *v == View::Hex).unwrap();
                views.insert(at, View::Strings);
            }
        }
        _ => {}
    }
    Detection {
        format: spec,
        language,
        reason,
        suggested: views[0],
        views,
        editable: spec.editable,
    }
}

fn extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && !ext.contains(' ')).then_some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_beats_extension() {
        let d = detect("photo.txt", b"\x89PNG\r\n\x1a\n");
        assert_eq!(d.format.id, "png");
        assert_eq!(d.suggested, View::Preview);
        assert!(!d.editable);
    }

    #[test]
    fn dotenv_and_lock_files_by_name() {
        for (name, id) in [
            (".env", "ini"),
            (".env.production", "ini"),
            (".editorconfig", "ini"),
            ("Cargo.lock", "toml"),
            ("poetry.lock", "toml"),
            ("Pipfile", "toml"),
            ("flake.lock", "json"),
            ("composer.lock", "json"),
        ] {
            let d = detect(name, b"KEY=value\n");
            assert_eq!(d.format.id, id, "{name}");
            assert_eq!(d.reason, format!("{} by file name", d.format.name));
        }
        assert_eq!(
            detect("yarn.lock", b"# yarn lockfile v1\n").format.id,
            "text"
        );
        assert_eq!(detect("Cargo.lock", b"\0\x01\x02\x03").format.id, "binary");
    }

    #[test]
    fn photoshop_heif_and_avif_by_signature() {
        assert_eq!(detect("x.bin", b"8BPS\x00\x01").format.id, "psd");
        assert_eq!(
            detect("IMG_1.HEIC", b"\0\0\0\x18ftypheic").format.id,
            "heif"
        );
        assert_eq!(detect("a", b"\0\0\0\x1cftypavif").format.id, "avif");
        assert_eq!(detect("scan.TIF", b"II*\0\x08\0\0\0").format.id, "tiff");
        assert_eq!(detect("big", b"MM\0+\0\x08\0\0").format.id, "tiff");
        assert_eq!(
            detect("clip.mp4", b"\0\0\0\x18ftypisom").format.id,
            "binary"
        );
        let d = detect("a.psd", b"8BPS\x00\x01");
        assert_eq!(d.views, [View::Summary, View::Strings, View::Hex]);
    }

    #[test]
    fn utf16_text_with_a_byte_order_mark_is_text() {
        let le: Vec<u8> = "\u{feff}Hello\r\nwörld 日本\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let be: Vec<u8> = "\u{feff}x = 1\n"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert_eq!(detect("notes.txt", &le).format.id, "text");
        assert_eq!(detect("a.json", &be).format.id, "json");
        // Odd length: the last unit is cut by the probe.
        assert_eq!(detect("notes.txt", &le[..le.len() - 1]).format.id, "text");
        // Binary that happens to start like a byte-order mark stays binary.
        let mut noise = vec![0xFF, 0xFE];
        noise.extend((0..200u8).flat_map(|i| [i % 7, 0]));
        assert_eq!(detect("x.txt", &noise).format.id, "binary");
        // UTF-32 is not UTF-16.
        assert_eq!(detect("x.txt", b"\xFF\xFE\0\0a\0\0\0").format.id, "binary");
    }

    #[test]
    fn zip_is_refined_by_extension() {
        assert_eq!(detect("book.xlsx", b"PK\x03\x04").format.id, "xlsx");
        assert_eq!(detect("Report.DOCX", b"PK\x03\x04").format.id, "docx");
        assert_eq!(detect("a.odt", b"PK\x03\x04").format.id, "odt");
        assert_eq!(detect("arrays.npz", b"PK\x03\x04").format.id, "npz");
        assert_eq!(detect("lib.jar", b"PK\x03\x04").format.id, "zip");
    }

    #[test]
    fn every_binary_file_has_summary_and_hex_and_table_needs_a_reader() {
        let d = detect("a.npy", b"\x93NUMPY\x01\x00");
        assert_eq!(
            d.views,
            [View::Table, View::Summary, View::Strings, View::Hex]
        );
        let d = detect("a.db", b"SQLite format 3\0");
        assert_eq!(
            d.views.contains(&View::Table),
            crate::table::supports("sqlite")
        );
        assert_eq!(*d.views.last().unwrap(), View::Hex);
        let d = detect("blob", b"\0\x01\x02");
        assert_eq!(d.views, [View::Summary, View::Strings, View::Hex]);
    }

    #[test]
    fn extension_names_the_format() {
        let d = detect("notes.md", b"# Title\n");
        assert_eq!(d.format.id, "markdown");
        assert_eq!(d.language, Some("markdown"));
        assert_eq!(d.suggested, View::Preview);
        assert!(d.views.contains(&View::Source));
        assert!(d.editable);
    }

    #[test]
    fn code_by_extension_and_shebang() {
        let d = detect("main.cpp", b"int main() {}\n");
        assert_eq!((d.format.id, d.language), ("code", Some("cpp")));
        assert!(d.editable);
        let d = detect("run", b"#!/usr/bin/env python3\n");
        assert_eq!((d.format.id, d.language), ("code", Some("python")));
        let d = detect("build", b"#!/bin/sh\n# vim: ft=python\n");
        assert_eq!(d.language, Some("python"), "a modeline beats the shebang");
        let d = detect("notes.txt", b"-*- mode: sql -*-\nselect 1;\n");
        assert_eq!((d.format.id, d.language), ("code", Some("sql")));
        let d = detect("Makefile", b"all:\n");
        assert_eq!(d.language, Some("makefile"));
    }

    #[test]
    fn text_extension_with_binary_content_is_bytes() {
        let d = detect("data.csv", b"\0\x01\x02\x03");
        assert_eq!(d.format.id, "binary");
        assert_eq!(d.suggested, View::Summary);
        assert!(!d.editable);
    }

    #[test]
    fn content_probe_without_extension() {
        assert_eq!(detect("config", b"  {\"a\":1}").format.id, "json");
        assert_eq!(
            detect("doc", b"<?xml version=\"1.0\"?><a/>").format.id,
            "xml"
        );
        assert_eq!(detect("doc", b"---\nkey: v\n").format.id, "yaml");
        assert_eq!(detect("README", b"Hello\n").format.id, "text");
        assert_eq!(detect("", b"").format.id, "text");
    }

    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(extension(".bashrc"), None);
        assert_eq!(extension("a.b.c"), Some("c"));
        assert_eq!(extension("noext"), None);
    }

    #[test]
    fn tree_is_offered_only_with_a_parser() {
        let d = detect("a.json", b"{}");
        assert_eq!(d.suggested, View::Tree);
        for (name, id) in [
            ("a.xml", "xml"),
            ("a.yaml", "yaml"),
            ("a.toml", "toml"),
            ("a.ini", "ini"),
        ] {
            let d = detect(name, b"x");
            assert_eq!(d.format.id, id);
            let has_parser = crate::tree::for_format(id, None).is_some();
            assert_eq!(d.views.contains(&View::Tree), has_parser, "{name}");
            assert!(d.views.contains(&View::Source));
        }
    }

    #[test]
    fn code_opens_on_its_source() {
        let d = detect("main.rs", b"fn main() {}\n");
        assert_eq!(d.suggested, View::Source);
        assert_eq!(d.views[0], View::Source);
        let d = detect("notes.md", b"# x\n");
        assert_eq!(d.views, [View::Preview, View::Source, View::Outline]);
    }

    #[test]
    fn notebook_is_read_only_cells() {
        let d = detect("x.ipynb", b"{\"cells\": []}");
        assert_eq!(d.format.id, "notebook");
        assert_eq!(d.suggested, View::Cells);
        assert!(!d.editable);
    }
}
