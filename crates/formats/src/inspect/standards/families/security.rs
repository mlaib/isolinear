//! Security: X.509, PEM, DER, CMS, PKCS#12, JWT, XML signatures and XAdES.
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
    &super::super::pem::Pem,
    &super::super::pkcs12::Pkcs12,
    &super::super::cms::Cms,
    &super::super::x509::X509,
    &super::super::jwt::Jwt,
    &super::super::der::GenericDer,
];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[&super::super::xmldsig::SIGNATURE];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[(
    &super::super::xmldsig::EMBEDDED,
    super::super::xmldsig::signature_near_root,
)];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
