//! CAD, 3D and engineering: STL, OBJ, glTF, STEP, IFC, IGES, DXF, G-code, OPC UA NodeSet, AUTOSAR ARXML.
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
    &super::super::stl::Stl,
    &super::super::obj::Obj,
    &super::super::gltf::Gltf,
    &super::super::ifc::Ifc,
    &super::super::step::Step,
    &super::super::iges::Iges,
    &super::super::dxf::Dxf,
    &super::super::gcode::Gcode,
];
/// Handlers asked after them.
pub static AFTER: &[&dyn Handler] = &[];
/// XML standards, matched on the root element and namespace.
pub static XML: &[&XmlSpec] = &[&super::super::opcua::NODESET, &super::super::autosar::ARXML];
/// XML standards that also need the start of the text.
pub static XML_BY_CONTENT: &[(&XmlSpec, HeadTest)] = &[];
/// JSON standards, matched on the top-level keys and the start of the text.
pub static JSON: &[&JsonSpec] = &[];
