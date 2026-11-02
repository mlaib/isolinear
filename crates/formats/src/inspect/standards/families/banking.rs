//! Banking and payments: ISO 20022, SWIFT MT, MT940/942/950, BAI2, FIX, OFX, QIF.
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
    &super::super::mt940::Mt940,
    &super::super::swift_mt::SwiftMt,
    &super::super::bai2::Bai2,
    &super::super::fix::Fix,
    &super::super::ofx::Ofx,
    &super::super::qif::Qif,
];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[
    &super::super::iso20022::PAIN_001,
    &super::super::iso20022::PAIN_002,
    &super::super::iso20022::PACS_008,
    &super::super::iso20022::CAMT_052,
    &super::super::iso20022::CAMT_053,
    &super::super::iso20022::CAMT_054,
    &super::super::iso20022::OTHER,
];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
