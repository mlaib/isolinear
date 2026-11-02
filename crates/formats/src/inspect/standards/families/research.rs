//! Statistics, research and legal documents: SDMX (XML, JSON, CSV), DDI, Akoma Ntoso.
//!
//! This family's registry: its standards' modules live in `inspect/standards/`; list them here
//! in the order they are asked (more specific first).

#[allow(unused_imports)]
use crate::inspect::Handler;
#[allow(unused_imports)]
use crate::inspect::spec::{JsonSpec, XmlSpec};

use super::super::HeadTest;

/// Handlers asked before the generic XML and JSON standards.
pub static FIRST: &[&dyn Handler] = &[];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[&super::super::sdmx_csv::SdmxCsv];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[
    &super::super::akn::DOCUMENT,
    &super::super::sdmx::DATA,
    &super::super::sdmx::STRUCTURE,
    &super::super::ddi::CODEBOOK,
    &super::super::ddi::LIFECYCLE,
];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[
    &super::super::jupyter::JUPYTER,
    &super::super::sdmx_json::SDMX_JSON,
];
