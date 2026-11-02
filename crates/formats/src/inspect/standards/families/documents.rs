//! Documents, mail and containers: e-mail, mailboxes, Outlook messages, Excel 97-2003, presentations, ASiC-E, PDF attachments and signatures.
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
pub static AFTER: &[&dyn Handler] = &[
    &super::super::mail::Eml,
    &super::super::mail::Mbox,
    &super::super::msg::OutlookMsg,
    &super::super::xls::Xls,
    &super::super::presentation::Presentation,
    &super::super::asic::Asice,
    &super::super::pdfdoc::PdfDocument,
];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
