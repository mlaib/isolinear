//! Medical and scientific data: DICOM, EDF/BDF, VCF, FASTA/FASTQ, BAM, CRAM, FITS, MATLAB.
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
    &super::super::dicom::Dicom,
    &super::super::edf::Edf,
    &super::super::bam::Bam,
    &super::super::cram::Cram,
    &super::super::fits::Fits,
    &super::super::mat::Mat,
    &super::super::vcf::Vcf,
    &super::super::fastq::Fastq,
    &super::super::fasta::Fasta,
];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
