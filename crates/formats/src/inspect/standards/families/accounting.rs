//! Accounting, e-invoicing and insurance: UBL, UN/CEFACT CII, Factur-X, XBRL, SAF-T and JPK, FEC, fixed-width records, ACORD, insurance EDI and bordereaux.
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
    &super::super::fixed_width::FixedWidth,
    &super::super::fec::Fec,
];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[&super::super::bordereau::Bordereau];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[
    &super::super::ubl::INVOICE,
    &super::super::ubl::CREDIT_NOTE,
    &super::super::cii::INVOICE,
    &super::super::xbrl::INSTANCE,
    &super::super::saft::PORTUGAL,
    &super::super::saft::OECD_SAFT,
    &super::super::saft::JPK,
    &super::super::acord::PC,
    &super::super::acord::LIFE,
];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] =
    &[(&super::super::xbrl::INLINE, super::super::xbrl::is_inline)];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
