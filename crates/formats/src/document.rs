//! Word and OpenDocument text rendered as bounded, constructed HTML. Only archive parts are
//! read; macros, fields, entities and external resources are never executed or loaded.
mod html;
mod model;
mod reader;
mod zip;

/// The OOXML/ODF XML walker and package helpers, for standards built on office packages.
pub(crate) use reader::{Ev, Ns, properties, relationship_ids, rels_of, resolve_target, walk};

use std::{fs::File, path::Path};

pub const READY: bool = true;

/// Preview content, not page layout. Every input string is escaped. The caller runs this on
/// its I/O worker; the existing API does not expose a cancellation token.
pub fn to_html(format: &str, path: &Path) -> Result<String, String> {
    let expected = match format {
        "docx" | "docm" | "dotx" => reader::Kind::Docx,
        "odt" | "ott" => reader::Kind::Odt,
        _ => {
            return Err(format!(
                "Expected a Word or OpenDocument text format, found {format}"
            ));
        }
    };
    let mut file = File::open(path).map_err(|e| format!("Cannot open the document: {e}"))?;
    let listing = zip::list_zip(&mut file, &|| false).map_err(|e| e.to_string())?;
    if reader::kind(&mut file, &listing) != Some(expected) {
        return Err("The archive does not contain the expected text document".into());
    }
    let mut document = reader::read(&mut file, &listing, &|| false)
        .ok_or("The archive does not contain a text document")??;
    if (format == "docm"
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("docm")))
        && !document.warnings.iter().any(|w| w.contains("macros"))
    {
        document
            .warnings
            .insert(0, "Contains macros; they are never run".into());
    }
    html::render(&document, &mut file, &listing)
}

#[cfg(test)]
mod tests;
