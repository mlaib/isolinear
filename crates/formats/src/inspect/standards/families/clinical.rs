//! Clinical records: HL7 FHIR (JSON and XML), CDA R2 and C-CDA.
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
pub static AFTER: &[&dyn Handler] = &[];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[&super::super::fhir::FHIR_XML, &super::super::cda::CDA];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[&super::super::fhir::FHIR_JSON];
