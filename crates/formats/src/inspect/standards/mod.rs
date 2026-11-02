//! The registered standards. Each family keeps its own registry in `families/`: handlers asked
//! before and after the generic XML and JSON standards, and the XML and JSON standards described
//! by paths. This file asks the families in a fixed order, specific before general.
//!
//! To add a standard: put its module in this directory, declare it below, and list it in its
//! family's registry.

use super::{
    Handler, Input, Inspection, Probe, Stage, State,
    spec::{self, JsonSpec, XmlSpec},
    xmldoc,
};

pub mod families;

pub mod acord;
pub mod akn;
pub mod asic;
pub mod autosar;
pub mod bai2;
pub mod bam;
pub mod bordereau;
pub mod cad;
pub mod cargo_imp;
pub mod cda;
pub mod cfb;
pub mod cii;
pub mod class;
pub mod cms;
pub mod common;
pub mod cram;
pub mod cyclonedx;
pub mod ddi;
pub mod der;
pub mod dicom;
pub mod doc_common;
pub mod doc_der;
pub mod dxf;
pub mod edf;
mod edi;
pub mod edifact;
pub mod elf;
pub mod fasta;
pub mod fastq;
pub mod fec;
pub mod fhir;
pub mod fits;
pub mod fix;
pub mod fixed_width;
pub mod gcode;
pub mod geo;
pub mod geojson;
pub mod geotiff;
pub mod gltf;
pub mod gpkg;
pub mod gpx;
pub mod gs1;
pub mod har;
pub mod hash;
pub mod hl7;
pub mod iata;
pub mod ifc;
pub mod iges;
pub mod iso20022;
pub mod jupyter;
pub mod jwt;
pub mod kml;
pub mod las;
pub mod mail;
pub mod mat;
pub mod mbtiles;
pub mod msg;
pub mod mt940;
pub mod obj;
pub mod ofx;
pub mod opcua;
pub mod p21;
pub mod pcap;
pub mod pdfdoc;
pub mod pe;
pub mod pem;
pub mod pkcs12;
pub mod presentation;
pub mod qif;
pub mod records;
pub mod saft;
pub mod sarif;
mod sci_common;
pub mod sdmx;
pub mod sdmx_csv;
pub mod sdmx_json;
pub mod shapefile;
pub mod spdx;
pub mod step;
pub mod stl;
mod support;
pub mod swift_mt;
pub mod ubl;
pub mod util;
pub mod vcf;
pub mod wasm;
pub mod x12;
pub mod x509;
pub mod xbrl;
pub mod xls;
pub mod xmldsig;

pub type HeadTest = fn(&str) -> bool;

/// The XML signature namespace: a signature in any XML standard is reported, never verified.
pub const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";

use families::{
    accounting, banking, clinical, documents, edi as edi_family, engineering, geo as geo_family,
    research, science, security, software,
};

/// Families whose handlers come before the generic XML and JSON standards, in order.
fn first() -> impl Iterator<Item = &'static dyn Handler> {
    [
        accounting::FIRST,
        edi_family::FIRST,
        banking::FIRST,
        science::FIRST,
        geo_family::FIRST,
        engineering::FIRST,
        clinical::FIRST,
        research::FIRST,
        documents::FIRST,
        security::FIRST,
        software::FIRST,
    ]
    .into_iter()
    .flatten()
    .copied()
}

fn after() -> impl Iterator<Item = &'static dyn Handler> {
    [
        // SPDX tag-value looks like mail headers: software first. Generic DER (security) last.
        software::AFTER,
        documents::AFTER,
        research::AFTER,
        accounting::AFTER,
        security::AFTER,
        banking::AFTER,
        edi_family::AFTER,
        clinical::AFTER,
        geo_family::AFTER,
        science::AFTER,
        engineering::AFTER,
    ]
    .into_iter()
    .flatten()
    .copied()
}

fn xml_specs() -> impl Iterator<Item = &'static XmlSpec> {
    [
        banking::XML,
        accounting::XML,
        edi_family::XML,
        research::XML,
        geo_family::XML,
        engineering::XML,
        clinical::XML,
        documents::XML,
        science::XML,
        security::XML,
        software::XML,
    ]
    .into_iter()
    .flatten()
    .copied()
}

fn xml_by_content() -> impl Iterator<Item = &'static (&'static XmlSpec, HeadTest)> {
    [
        accounting::XML_BY_CONTENT,
        banking::XML_BY_CONTENT,
        edi_family::XML_BY_CONTENT,
        research::XML_BY_CONTENT,
        geo_family::XML_BY_CONTENT,
        engineering::XML_BY_CONTENT,
        clinical::XML_BY_CONTENT,
        documents::XML_BY_CONTENT,
        science::XML_BY_CONTENT,
        security::XML_BY_CONTENT,
        software::XML_BY_CONTENT,
    ]
    .into_iter()
    .flatten()
}

fn json_specs() -> impl Iterator<Item = &'static JsonSpec> {
    [
        geo_family::JSON,
        clinical::JSON,
        security::JSON,
        software::JSON,
        research::JSON,
        banking::JSON,
        accounting::JSON,
        edi_family::JSON,
        documents::JSON,
        science::JSON,
        engineering::JSON,
    ]
    .into_iter()
    .flatten()
    .copied()
}

/// Every handler, in the order they are asked.
pub fn handlers() -> impl Iterator<Item = &'static dyn Handler> {
    first()
        .chain([&XmlStandards as &dyn Handler, &JsonStandards])
        .chain(after())
}

const XML_EXTENSIONS: &[&str] = &[
    "xml", "xbrl", "arxml", "kml", "gpx", "xsd", "cda", "fhir", "xhtml", "html", "htm",
];

struct XmlStandards;

impl XmlStandards {
    fn spec(root: &str, ns: &str, head: &str) -> Option<&'static XmlSpec> {
        xml_specs().find(|s| (s.matches)(root, ns)).or_else(|| {
            xml_by_content()
                .find(|(s, test)| (s.matches)(root, ns) && test(head))
                .map(|(s, _)| *s)
        })
    }
}

impl Handler for XmlStandards {
    fn name(&self) -> &'static str {
        "XML standards"
    }

    fn recognize(&self, probe: &Probe) -> bool {
        if probe.format != "xml" && !probe.ext_is(XML_EXTENSIONS) {
            return false;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        xmldoc::root_of(text).is_some_and(|(root, ns)| Self::spec(&root, &ns, text).is_some())
    }

    fn inspect(&self, input: &Input, cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        let doc = xmldoc::Doc::parse(&text, cancelled)?;
        let root = doc.root();
        let mut end = text.len().min(super::PROBE);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let spec = Self::spec(&root.name, &root.ns, &text[..end]).ok_or("No known standard")?;
        let mut inspection = spec::run_xml(spec, &doc);
        let signed = doc
            .elements
            .iter()
            .any(|e| e.name == "Signature" && e.ns == DSIG_NS);
        if signed
            && !inspection
                .checks
                .iter()
                .any(|c| c.stage == Stage::Signature)
        {
            inspection.check(
                Stage::Signature,
                State::NotDone,
                "The document carries an XML signature; it was not verified",
            );
        }
        Ok(inspection)
    }
}

struct JsonStandards;

impl JsonStandards {
    fn spec(keys: &[String], head: &str) -> Option<&'static JsonSpec> {
        json_specs().find(|s| (s.matches)(keys, head))
    }
}

impl Handler for JsonStandards {
    fn name(&self) -> &'static str {
        "JSON standards"
    }

    fn recognize(&self, probe: &Probe) -> bool {
        if probe.format != "json"
            && !probe.ext_is(&["json", "geojson", "sarif", "har", "ipynb", "gltf", "spdx"])
        {
            return false;
        }
        let Some(text) = probe.text() else {
            return false;
        };
        Self::spec(&super::jsondoc::top_keys(text), text).is_some()
    }

    fn inspect(&self, input: &Input, _cancelled: &dyn Fn() -> bool) -> Result<Inspection, String> {
        let text = input.text()?;
        let mut end = text.len().min(super::PROBE);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let head = &text[..end];
        let spec = Self::spec(&super::jsondoc::top_keys(head), head).ok_or("No known standard")?;
        let root = super::jsondoc::parse(&text)?;
        Ok(spec::run_json(spec, &root))
    }
}
