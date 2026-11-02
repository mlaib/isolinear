//! Supply chain, transport and health messages: UN/EDIFACT, ANSI X12, HL7 v2, GS1 XML, IATA Cargo-IMP and Cargo-XML.
//!
//! This family's registry: its standards' modules live in `inspect/standards/`; list them here
//! in the order they are asked (more specific first).

#[allow(unused_imports)]
use crate::inspect::Handler;
#[allow(unused_imports)]
use crate::inspect::spec::{JsonSpec, XmlSpec};

use super::super::HeadTest;

/// Handlers asked before the generic XML and JSON standards.
pub static FIRST: &[&dyn Handler] = &[
    &super::super::edifact::Edifact,
    &super::super::x12::X12,
    &super::super::hl7::Hl7,
    &super::super::cargo_imp::CargoImp,
];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[
    &super::super::gs1::MESSAGE,
    &super::super::gs1::SBD,
    &super::super::iata::WAYBILL,
    &super::super::iata::FLIGHT_MANIFEST,
    &super::super::iata::OTHER,
];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
