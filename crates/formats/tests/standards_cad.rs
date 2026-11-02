//! CAD, 3D and engineering standards (`inspect::standards`): STL, OBJ, glTF/GLB, STEP, IFC,
//! IGES, DXF, G-code, OPC UA NodeSet and AUTOSAR ARXML.
//!
//! Fixtures in tests/fixtures/standards:
//! - written by hand: bracket.step, house.ifc, plan.dxf, pumps.nodeset2.xml, cluster.arxml;
//! - generated with the standard library only, from the repository root:
//!   `python3 -I tools/make-cad-fixtures.py` writes cube.stl, cube-binary.stl, open.stl,
//!   box.obj, box.gltf, box.glb, drawing.igs, part.gcode and mill.nc.
use isolinear_formats::{
    detect,
    inspect::{Inspection, Stage, State, Table, Visual, inspect_file},
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

fn open(name: &str) -> Inspection {
    inspect(&fixture(name))
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?} in {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

fn cell<'a>(t: &'a Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

fn mesh(i: &Inspection) -> (&[[f32; 3]], &[[u32; 3]]) {
    i.visuals
        .iter()
        .find_map(|v| match v {
            Visual::Mesh {
                vertices,
                triangles,
            } => Some((vertices.as_slice(), triangles.as_slice())),
            _ => None,
        })
        .expect("a mesh")
}

fn bounds(vertices: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in vertices {
        for k in 0..3 {
            min[k] = min[k].min(v[k]);
            max[k] = max[k].max(v[k]);
        }
    }
    (min, max)
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-4)
}

fn drawing(i: &Inspection) -> (&[Vec<[f32; 2]>], &str) {
    i.visuals
        .iter()
        .find_map(|v| match v {
            Visual::Drawing { paths, unit } => Some((paths.as_slice(), unit.as_str())),
            _ => None,
        })
        .expect("a drawing")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-cad-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of a fixture with one text replacement, inspected.
fn variant(name: &str, from: &str, to: &str) -> Inspection {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    assert!(text.contains(from), "{from:?} not in {name}");
    let dir = temp_dir("variant");
    let path = dir.join(name);
    std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    let i = inspect(&path).unwrap().unwrap();
    let _ = std::fs::remove_file(&path);
    i
}

#[test]
fn stl_ascii_and_binary_cubes_are_closed_with_volume_and_area() {
    for (name, kind) in [("cube.stl", "ASCII"), ("cube-binary.stl", "binary")] {
        let i = open(name);
        assert_eq!(i.family, "STL");
        assert!(i.kind.contains(kind), "{name}: {}", i.kind);
        assert_eq!(fact(&i, "Triangles"), "12");
        assert_eq!(fact(&i, "Vertices (merged)"), "8");
        assert_eq!(fact(&i, "Size (x × y × z)"), "10 × 10 × 10");
        assert_eq!(fact(&i, "Surface area"), "600");
        assert_eq!(fact(&i, "Volume (signed tetrahedra)"), "1000");
        assert_eq!(check(&i, Stage::Semantic, "Closed mesh"), State::Passed);
        assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
        let (v, t) = mesh(&i);
        assert_eq!((v.len(), t.len()), (8, 12));
        assert_eq!(bounds(v), ([0.0; 3], [10.0; 3]));
    }
    // The binary file's header starts with "solid": it was recognized by its size.
    let i = open("cube-binary.stl");
    assert_eq!(fact(&i, "Header"), "solid cube written as binary STL");
    assert_eq!(check(&i, Stage::Parsed, "size matches"), State::Passed);
    assert_eq!(fact(&open("cube.stl"), "Solid names"), "cube");
}

#[test]
fn an_open_stl_mesh_fails_the_closed_check() {
    let i = open("open.stl");
    assert_eq!(fact(&i, "Triangles"), "10");
    assert_eq!(check(&i, Stage::Semantic, "Closed mesh"), State::Failed);
    assert!(
        i.checks
            .iter()
            .any(|c| c.detail.contains("4 edges belong to one triangle"))
    );
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("volume is not meaningful"))
    );
}

#[test]
fn obj_objects_groups_materials_and_mesh() {
    let i = open("box.obj");
    assert_eq!(i.family, "Wavefront OBJ");
    assert_eq!(fact(&i, "Faces"), "10");
    assert_eq!(fact(&i, "Faces with more than three corners"), "6");
    assert_eq!(fact(&i, "Triangles"), "16");
    assert_eq!(fact(&i, "Vertices (merged)"), "12");
    assert_eq!(fact(&i, "Objects"), "2");
    assert_eq!(fact(&i, "Groups"), "2");
    assert_eq!(fact(&i, "Materials used"), "2");
    assert_eq!(fact(&i, "Material libraries (not opened)"), "materials.mtl");
    assert_eq!(fact(&i, "Volume (signed tetrahedra)"), "1000.17");
    let objects = table(&i, "Objects");
    assert_eq!(cell(objects, 0, "Object"), "Box");
    assert_eq!(cell(objects, 0, "Faces"), "6");
    assert_eq!(cell(objects, 0, "Materials"), "Red, Blue");
    assert_eq!(cell(objects, 1, "Object"), "Tetra");
    let materials = table(&i, "Materials");
    assert_eq!(cell(materials, 0, "Material"), "Red");
    assert_eq!(cell(materials, 0, "Faces"), "6");
    assert_eq!(cell(table(&i, "Groups"), 1, "Group"), "sides");
    // Negative indices and every v/vt/vn form were resolved: the box is closed.
    assert_eq!(check(&i, Stage::Semantic, "Closed mesh"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "Face indices"), State::Passed);
    let (v, t) = mesh(&i);
    assert_eq!((v.len(), t.len()), (12, 16));
    assert_eq!(bounds(v), ([0.0; 3], [21.0, 10.0, 10.0]));
    // A face pointing past the vertices fails the index rule.
    let bad = variant("box.obj", "f 10 11 12", "f 10 11 99");
    assert_eq!(check(&bad, Stage::Semantic, "Face indices"), State::Failed);
    assert_eq!(check(&bad, Stage::Semantic, "Closed mesh"), State::Failed);
}

#[test]
fn gltf_json_with_a_data_uri_buffer_and_node_transforms() {
    let i = open("box.gltf");
    assert_eq!(i.family, "glTF");
    assert!(i.kind.contains("JSON"));
    assert_eq!(i.version.as_deref(), Some("glTF 2.0"));
    assert_eq!(fact(&i, "Generator"), "Isolinear fixture generator");
    assert_eq!(fact(&i, "Nodes"), "3");
    let prims = table(&i, "Primitives");
    assert_eq!(cell(prims, 0, "Mode"), "triangles");
    assert_eq!(cell(prims, 0, "Vertices"), "8");
    assert_eq!(cell(prims, 0, "Indices"), "36");
    assert_eq!(cell(prims, 0, "Material"), "Grey");
    assert!(cell(table(&i, "Images"), 0, "Source").contains("“textures/label.png” (not opened)"));
    assert!(cell(table(&i, "Buffers"), 0, "Source").starts_with("embedded data URI"));
    assert_eq!(cell(table(&i, "Animations"), 0, "Duration (s)"), "1.5");
    assert_eq!(
        cell(table(&i, "Animations"), 0, "Targets"),
        "Box.translation"
    );
    assert_eq!(cell(table(&i, "Cameras"), 0, "Type"), "perspective");
    // Outline: scene, root node, its children.
    let labels: Vec<(u16, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str()))
        .collect();
    assert_eq!(
        labels,
        [(0, "Main"), (1, "Root"), (2, "Box"), (2, "Camera")]
    );
    assert!(i.outline[2].value.contains("mesh Cube"));
    let (v, t) = mesh(&i);
    assert_eq!((v.len(), t.len()), (8, 12));
    assert_eq!(bounds(v), ([5.0, 0.0, 0.0], [15.0, 10.0, 10.0]));
    assert_eq!(fact(&i, "Size (x × y × z)"), "10 × 10 × 10 m");
    assert_eq!(check(&i, Stage::Semantic, "Closed mesh"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "References"), State::Passed);
    assert!(
        i.visuals
            .iter()
            .all(|v| !matches!(v, Visual::Picture { .. }))
    );
}

#[test]
fn glb_with_bin_chunk_embedded_picture_and_rotation() {
    let i = open("box.glb");
    assert!(i.kind.contains("GLB"));
    assert_eq!(fact(&i, "GLB version"), "2");
    assert_eq!(check(&i, Stage::Parsed, "GLB"), State::Passed);
    assert_eq!(cell(table(&i, "Buffers"), 0, "Source"), "GLB BIN chunk");
    assert_eq!(
        cell(table(&i, "Images"), 0, "Source"),
        "embedded, buffer view 4"
    );
    let picture = i
        .visuals
        .iter()
        .find_map(|v| match v {
            Visual::Picture { bytes, caption } => Some((bytes, caption)),
            _ => None,
        })
        .expect("the embedded PNG");
    assert!(picture.0.starts_with(b"\x89PNG"));
    assert_eq!(picture.1, "Checker");
    // Scaled by 2, turned 90° about z, moved 5 along x.
    let (v, _) = mesh(&i);
    let (min, max) = bounds(v);
    assert!(
        close(min, [-15.0, 0.0, 0.0]) && close(max, [5.0, 20.0, 20.0]),
        "{min:?} {max:?}"
    );
    assert_eq!(fact(&i, "Volume (signed tetrahedra)"), "8000 m³");
}

#[test]
fn step_header_products_assembly_units_and_geometry_counts() {
    let i = open("bracket.step");
    assert_eq!(i.family, "STEP (ISO 10303-21)");
    assert!(i.kind.starts_with("AP214"), "{}", i.kind);
    assert_eq!(fact(&i, "Author"), "Test Author");
    assert_eq!(fact(&i, "Organization"), "Example Org");
    assert_eq!(fact(&i, "Originating system"), "Hand-written");
    assert_eq!(
        fact(&i, "Schema"),
        "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }"
    );
    assert_eq!(fact(&i, "Products"), "3");
    assert_eq!(fact(&i, "Assembly usages"), "3");
    assert_eq!(fact(&i, "Length unit"), "millimetre");
    assert_eq!(fact(&i, "Plane angle unit"), "radian");
    assert_eq!(fact(&i, "Advanced faces"), "1");
    assert_eq!(fact(&i, "B-rep solids"), "1");
    let products = table(&i, "Products");
    assert_eq!(
        cell(products, 2, "Description"),
        "Hexagon bolt Ø6 'ISO 4017'"
    );
    let outline: Vec<(u16, &str, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str(), o.value.as_str()))
        .collect();
    assert_eq!(
        outline,
        [
            (0, "Bracket assembly", "ASM-1"),
            (1, "Base plate", "P-100 · Plate"),
            (1, "Bolt M6", "P-200 · Bolt 1"),
            (1, "Bolt M6", "P-200 · Bolt 2"),
        ]
    );
    let entities = table(&i, "Entities");
    assert_eq!(entities.rows.len(), 37);
    assert_eq!(cell(entities, 0, "#"), "#1");
    let complex = entities.rows.iter().find(|r| r.cells[0] == "#50").unwrap();
    assert_eq!(complex.cells[1], "LENGTH_UNIT + NAMED_UNIT + SI_UNIT");
    let types = table(&i, "Entity types");
    // Largest counts first, ties by name.
    assert_eq!(types.rows[0].cells, ["NEXT_ASSEMBLY_USAGE_OCCURRENCE", "3"]);
    assert_eq!(types.rows[1].cells, ["PRODUCT", "3"]);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "References"), State::Passed);
    assert!(i.notes.iter().any(|n| n.contains("not tessellated")));
    assert!(i.visuals.is_empty());
    // Every instance row points at its statement.
    let text = std::fs::read_to_string(fixture("bracket.step")).unwrap();
    let src = complex.source.clone().unwrap();
    assert!(text[src.start as usize..src.end as usize].starts_with("#50=("));
    assert!(text[src.start as usize..src.end as usize].ends_with(";"));
    let bad = variant("bracket.step", "(#73,#63)", "(#999,#63)");
    assert_eq!(check(&bad, Stage::Semantic, "References"), State::Failed);
}

#[test]
fn ifc_spatial_structure_elements_properties_quantities_and_units() {
    let i = open("house.ifc");
    assert_eq!(i.family, "IFC (ISO 16739)");
    assert_eq!(i.version.as_deref(), Some("IFC4"));
    assert_eq!(fact(&i, "Name"), "Sample House");
    assert_eq!(
        fact(&i, "Authoring application"),
        "Isolinear Fixture Writer 1.0"
    );
    assert_eq!(fact(&i, "Storeys"), "2");
    assert_eq!(fact(&i, "Elements"), "5");
    assert_eq!(fact(&i, "Length unit"), "millimetre");
    let outline: Vec<(u16, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str()))
        .collect();
    assert_eq!(
        outline,
        [
            (0, "IfcProject “Sample House”"),
            (1, "IfcSite “Default Site”"),
            (2, "IfcBuilding “House A”"),
            (3, "IfcBuildingStorey “Ground Floor”"),
            (4, "IfcSpace “Living Room”"),
            (4, "IfcWall “Wall North”"),
            (4, "IfcWall “Wall South”"),
            (4, "IfcSlab “Floor Slab”"),
            (4, "IfcDoor “Front Door”"),
            (3, "IfcBuildingStorey “First Floor”"),
            (4, "IfcWindow “Window 1”"),
        ]
    );
    assert_eq!(i.outline[0].value, "2O2Fr$t4X7Zf8NOew3FLOH");
    let classes = table(&i, "Elements by class");
    let walls = classes
        .rows
        .iter()
        .find(|r| r.cells[0] == "IfcWall")
        .unwrap();
    assert_eq!(walls.cells[1], "2");
    let elements = table(&i, "Elements");
    assert_eq!(elements.rows.len(), 5);
    assert_eq!(cell(elements, 0, "Tag"), "W-01");
    assert_eq!(
        cell(elements, 0, "Container"),
        "IfcBuildingStorey “Ground Floor”"
    );
    assert_eq!(
        cell(elements, 4, "Container"),
        "IfcBuildingStorey “First Floor”"
    );
    let props = table(&i, "Properties");
    assert_eq!(props.rows.len(), 6);
    assert_eq!(cell(props, 0, "Property set"), "Pset_WallCommon");
    assert_eq!(cell(props, 0, "Value"), "true");
    assert_eq!(cell(props, 1, "Value"), "REI 60");
    assert_eq!(cell(props, 2, "Value"), "0.24");
    assert_eq!(cell(props, 3, "Element"), "IfcWall “Wall South”");
    let quants = table(&i, "Quantities");
    assert_eq!(quants.rows.len(), 3);
    assert_eq!(cell(quants, 0, "Unit"), "millimetre");
    assert_eq!(cell(quants, 1, "Value"), "21.6");
    assert_eq!(cell(quants, 1, "Unit"), "square metre");
    assert_eq!(check(&i, Stage::Semantic, "GlobalIds"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "References"), State::Passed);
    // A GlobalId used twice fails.
    let bad = variant(
        "house.ifc",
        "'1F3pz2KD10ewZqeJ_6O9lc'",
        "'0Qqy4oVOD4uO7cx5HdJw0h'",
    );
    assert_eq!(check(&bad, Stage::Semantic, "GlobalIds"), State::Failed);
}

#[test]
fn iges_global_section_directory_drawing_and_terminate_counts() {
    let i = open("drawing.igs");
    assert_eq!(i.family, "IGES");
    assert_eq!(i.version.as_deref(), Some("5.3"));
    assert_eq!(fact(&i, "Units"), "millimetre");
    assert_eq!(fact(&i, "Author"), "Test Author");
    assert_eq!(fact(&i, "Organization"), "Example Org");
    assert_eq!(fact(&i, "File name"), "drawing.igs");
    assert_eq!(fact(&i, "Generated"), "2026-11-01 12:00:00");
    assert_eq!(fact(&i, "Entities"), "7");
    assert_eq!(fact(&i, "Entities drawn"), "5");
    let dir = table(&i, "Directory entries");
    assert_eq!(dir.rows.len(), 7);
    assert_eq!(cell(dir, 3, "Entity"), "Circular arc");
    assert_eq!(cell(dir, 5, "Transform"), "9");
    assert_eq!(cell(dir, 6, "Entity"), "Rational B-spline curve");
    assert_eq!(cell(dir, 0, "Label"), "E1");
    let types = table(&i, "Entity types");
    assert_eq!(types.rows[0].cells, ["110 Line", "3"]);
    // Three lines, the arc and the point's cross.
    let (paths, unit) = drawing(&i);
    assert_eq!(paths.len(), 5);
    assert_eq!(unit, "millimetre");
    // The line at DE 11 is moved 200 along x by the matrix at DE 9.
    assert!(paths.iter().any(|p| p.iter().all(|q| q[0] == 200.0)));
    // The arc is the upper half circle around (50, 50).
    let arc = paths.iter().find(|p| p.len() > 5).unwrap();
    assert!(arc.iter().all(|q| q[1] >= 50.0 - 1e-3));
    assert_eq!(check(&i, Stage::Semantic, "Terminate"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "Directory entries"),
        State::Passed
    );
    let bad = variant("drawing.igs", "D     14P", "D     16P");
    assert_eq!(check(&bad, Stage::Semantic, "Terminate"), State::Failed);
}

#[test]
fn dxf_header_layers_blocks_entities_and_drawing() {
    let i = open("plan.dxf");
    assert_eq!(i.family, "DXF");
    assert_eq!(fact(&i, "Version"), "AC1015 (AutoCAD 2000)");
    assert_eq!(fact(&i, "Units"), "millimetres");
    assert_eq!(fact(&i, "Extents max"), "100.0, 60.0, 0.0");
    let layers = table(&i, "Layers");
    assert_eq!(layers.rows.len(), 3);
    assert_eq!(cell(layers, 2, "Layer"), "Hidden");
    assert_eq!(cell(layers, 2, "Frozen"), "yes");
    assert_eq!(cell(layers, 2, "Off"), "yes");
    assert_eq!(cell(layers, 2, "Colour"), "3");
    assert_eq!(cell(layers, 2, "Line type"), "DASHED");
    assert_eq!(cell(table(&i, "Blocks"), 0, "Block"), "Chair");
    assert_eq!(cell(table(&i, "Blocks"), 0, "Entities"), "1");
    let entities = table(&i, "Entities");
    assert_eq!(entities.rows.len(), 10);
    assert_eq!(cell(entities, 0, "Coordinates"), "(0, 0) → (100, 0)");
    assert_eq!(cell(entities, 0, "Handle"), "1A");
    assert_eq!(cell(entities, 7, "Text or block"), "Plan view");
    assert_eq!(cell(entities, 9, "Type"), "INSERT");
    assert_eq!(cell(entities, 9, "Text or block"), "Chair");
    assert_eq!(fact(&i, "Block references (not expanded)"), "1");
    // Seven drawn entities and three marked points.
    let (paths, unit) = drawing(&i);
    assert_eq!(paths.len(), 10);
    assert_eq!(unit, "millimetres");
    // The polyline's bulge 1 makes a half circle from (40, 10) to (40, 30), bulging to x = 50.
    let poly = &paths[1];
    let max_x = poly.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
    assert!((max_x - 50.0).abs() < 1e-3, "{max_x}");
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "References"), State::Passed);
    assert!(i.notes.iter().any(|n| n.contains("control polygon")));
    let bad = variant("plan.dxf", "  2\nChair\n 10\n20.0", "  2\nTable\n 10\n20.0");
    assert_eq!(check(&bad, Stage::Semantic, "References"), State::Failed);
    // Binary DXF is recognized and declined.
    let dir = temp_dir("dxf");
    let path = dir.join("binary.dxf");
    std::fs::write(&path, b"AutoCAD Binary DXF\r\n\x1a\x00\x00\x00SECTION\x00").unwrap();
    let i = inspect(&path).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Parsed, "Binary DXF"), State::NotDone);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gcode_printer_program_with_checksums_and_toolpath() {
    let i = open("part.gcode");
    assert_eq!(i.family, "G-code");
    assert_eq!(
        fact(&i, "Dialect (guess)"),
        "Marlin (declared by the slicer)"
    );
    assert_eq!(fact(&i, "Units"), "millimetres");
    assert_eq!(fact(&i, "Feed rates"), "1200, 3000, 6000");
    // 0.2 up, a 20 mm square and a half circle of radius 5.
    assert_eq!(fact(&i, "Feed path length"), "95.908 millimetres");
    assert_eq!(fact(&i, "Rapid path length"), "28.2843 millimetres");
    assert_eq!(
        fact(&i, "Extent of feed moves (x × y × z)"),
        "30 × 30 × 0.2 millimetres"
    );
    assert_eq!(check(&i, Stage::Semantic, "Checksums"), State::Passed);
    let commands = table(&i, "Commands");
    let arc = commands
        .rows
        .iter()
        .position(|r| r.cells[2] == "G2")
        .unwrap();
    assert_eq!(cell(commands, arc, "Meaning"), "Clockwise arc");
    assert_eq!(cell(commands, arc, "I"), "5");
    assert_eq!(cell(commands, arc, "Comment"), "clockwise half circle");
    let (paths, _) = drawing(&i);
    assert_eq!(paths.len(), 2);
    // The clockwise half circle from (20, 20) to (30, 20) passes over the top, at y = 25.
    let top = paths[1].iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    assert!((top - 25.0).abs() < 1e-3, "{top}");
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("rapid moves (G0) are not drawn"))
    );
    let bad = variant("part.gcode", "N2 M84*29", "N2 M84*30");
    assert_eq!(check(&bad, Stage::Semantic, "Checksums"), State::Failed);
}

#[test]
fn gcode_cnc_program_with_tools_arcs_and_cycles() {
    let i = open("mill.nc");
    assert!(fact(&i, "Dialect (guess)").starts_with("Fanuc/LinuxCNC"));
    assert_eq!(fact(&i, "Tools"), "T1, T2");
    assert_eq!(fact(&i, "Feed path length"), "157.416 millimetres");
    assert_eq!(fact(&i, "Rapid path length"), "11 millimetres");
    assert_eq!(
        fact(&i, "Extent of feed moves (x × y × z)"),
        "40 × 40 × 6 millimetres"
    );
    let commands = table(&i, "Commands");
    let drill = commands
        .rows
        .iter()
        .position(|r| r.cells[1] == "140")
        .unwrap();
    assert_eq!(cell(commands, drill, "Meaning"), "Drilling cycle");
    let tool = commands
        .rows
        .iter()
        .position(|r| r.cells[1] == "20")
        .unwrap();
    assert_eq!(cell(commands, tool, "Tool"), "T1");
    assert_eq!(cell(commands, tool, "Comment"), "10 MM END MILL");
    let codes = table(&i, "Codes used");
    assert!(codes.rows.iter().any(|r| r.cells[0] == "G3"));
    assert!(i.notes.iter().any(|n| n.contains("not simulated")));
    // The G3 arc with R20 from (40, 20) to (20, 40) turns around (20, 20): the points beyond
    // both ends lie on that circle, through (34.14, 34.14).
    let (paths, _) = drawing(&i);
    let on_arc: Vec<&[f32; 2]> = paths
        .iter()
        .flatten()
        .filter(|p| p[0] > 20.5 && p[1] > 20.5)
        .collect();
    assert!(on_arc.len() > 5);
    for p in on_arc {
        let r = ((p[0] - 20.0).powi(2) + (p[1] - 20.0).powi(2)).sqrt();
        assert!((r - 20.0).abs() < 1e-3, "{p:?}");
    }
}

#[test]
fn opc_ua_nodeset_models_aliases_nodes_and_hierarchy() {
    let i = open("pumps.nodeset2.xml");
    assert_eq!(i.family, "OPC UA");
    assert_eq!(fact(&i, "Model URI"), "http://example.com/UA/Pumps/");
    assert_eq!(fact(&i, "Version"), "1.0.0");
    assert_eq!(fact(&i, "Objects"), "2");
    assert_eq!(fact(&i, "Variables"), "3");
    assert_eq!(fact(&i, "Methods"), "1");
    assert_eq!(fact(&i, "References"), "14");
    assert_eq!(table(&i, "Aliases").rows.len(), 7);
    assert_eq!(
        cell(table(&i, "Required models"), 0, "Required by"),
        "http://example.com/UA/Pumps/"
    );
    let nodes = table(&i, "Nodes");
    assert_eq!(nodes.rows.len(), 8);
    let pump = nodes
        .rows
        .iter()
        .position(|r| r.cells[0] == "ns=1;i=5002")
        .unwrap();
    assert_eq!(cell(nodes, pump, "DisplayName"), "Pump 1");
    assert_eq!(cell(nodes, pump, "Type definition"), "ns=1;i=1001");
    assert_eq!(cell(nodes, pump + 1, "DataType"), "Double");
    let outline: Vec<(u16, &str)> = i
        .outline
        .iter()
        .map(|o| (o.depth, o.label.as_str()))
        .collect();
    assert_eq!(
        outline,
        [
            (0, "1:PumpType"),
            (1, "1:Speed"),
            (0, "1:Pumps"),
            (1, "1:Pump1"),
            (2, "1:Speed"),
            (2, "1:SerialNumber"),
            (2, "1:Start"),
        ]
    );
    assert!(i.outline[2].value.contains("outside this file"));
    let refs = table(&i, "Reference types used");
    assert_eq!(refs.rows[0].cells, ["HasTypeDefinition", "5"]);
    assert_eq!(check(&i, Stage::Semantic, "Aliases"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "NodeIds"), State::Passed);
    let bad = variant(
        "pumps.nodeset2.xml",
        "DataType=\"String\"",
        "DataType=\"Text\"",
    );
    assert_eq!(check(&bad, Stage::Semantic, "Aliases"), State::Failed);
}

#[test]
fn autosar_packages_components_ports_interfaces_and_types() {
    let i = open("cluster.arxml");
    assert_eq!(i.family, "AUTOSAR");
    assert_eq!(
        i.version.as_deref(),
        Some("AUTOSAR 4.3.0 (AUTOSAR_4-3-0.xsd)")
    );
    assert_eq!(fact(&i, "Packages"), "5");
    assert_eq!(fact(&i, "Software components"), "3");
    assert_eq!(fact(&i, "Ports"), "3");
    assert_eq!(fact(&i, "Port interfaces"), "2");
    assert_eq!(fact(&i, "Data types"), "3");
    assert_eq!(fact(&i, "ECU instances"), "2");
    assert_eq!(fact(&i, "Runnables"), "1");
    let comps = table(&i, "Software components");
    assert_eq!(cell(comps, 1, "Component"), "Dashboard");
    assert_eq!(cell(comps, 1, "Package"), "/Demo/Components");
    assert_eq!(cell(comps, 1, "Ports"), "2");
    assert_eq!(cell(comps, 2, "Kind"), "COMPOSITION-SW-COMPONENT-TYPE");
    let ports = table(&i, "Ports");
    assert_eq!(cell(ports, 0, "Direction"), "provided");
    assert_eq!(cell(ports, 2, "Interface"), "/Demo/Interfaces/DiagIf");
    let ifs = table(&i, "Port interfaces");
    assert_eq!(cell(ifs, 0, "Members"), "Speed: Speed_T");
    assert_eq!(cell(ifs, 1, "Members"), "ReadDtc(Code)");
    assert_eq!(cell(table(&i, "Data types"), 0, "Category"), "FIXED_LENGTH");
    assert_eq!(
        (i.outline[0].depth, i.outline[0].label.as_str()),
        (0, "Demo")
    );
    let sensor = i.outline.iter().find(|o| o.label == "SpeedSensor").unwrap();
    assert_eq!(
        (sensor.depth, sensor.value.as_str()),
        (2, "APPLICATION-SW-COMPONENT-TYPE")
    );
    assert_eq!(check(&i, Stage::Semantic, "Port interfaces"), State::Passed);
    let other = variant(
        "cluster.arxml",
        ">/Demo/Interfaces/DiagIf<",
        ">/Other/DiagIf<",
    );
    assert_eq!(
        check(&other, Stage::Semantic, "Port interfaces"),
        State::Partial
    );
}

/// A handler that panics is caught by the guard and reported as an internal error: fail on it.
fn sound(path: &Path, r: Option<Result<Inspection, String>>) -> Option<Result<Inspection, String>> {
    if let Some(Err(e)) = &r {
        assert!(!e.contains("internal error"), "{}: {e}", path.display());
    }
    r
}

const FIXTURES: &[&str] = &[
    "cube.stl",
    "cube-binary.stl",
    "open.stl",
    "box.obj",
    "box.gltf",
    "box.glb",
    "bracket.step",
    "house.ifc",
    "drawing.igs",
    "plan.dxf",
    "part.gcode",
    "mill.nc",
    "pumps.nodeset2.xml",
    "cluster.arxml",
];

#[test]
fn truncated_cad_files_show_what_was_read() {
    let dir = temp_dir("cut");
    for name in FIXTURES {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for keep in [bytes.len() / 2, bytes.len() / 5, 90, 3] {
            let p = dir.join(format!("{keep}-{name}"));
            std::fs::write(&p, &bytes[..keep.min(bytes.len())]).unwrap();
            let started = std::time::Instant::now();
            sound(&p, inspect(&p));
            assert!(started.elapsed().as_secs() < 2, "{name} cut at {keep}");
        }
    }
    // Specific partial reads are reported as partial.
    let cut = |name: &str| {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let p = dir.join(format!("half-{name}"));
        std::fs::write(&p, &bytes[..bytes.len() * 2 / 3]).unwrap();
        inspect(&p).unwrap().unwrap()
    };
    let i = cut("cube-binary.stl");
    assert_eq!(check(&i, Stage::Parsed, "the file is cut"), State::Partial);
    let i = cut("bracket.step");
    assert_eq!(check(&i, Stage::Parsed, "END-ISO-10303-21"), State::Partial);
    assert_eq!(fact(&i, "Author"), "Test Author");
    let i = cut("plan.dxf");
    assert_eq!(check(&i, Stage::Parsed, "0/EOF"), State::Partial);
    let i = cut("drawing.igs");
    assert_eq!(check(&i, Stage::Semantic, "Terminate"), State::Failed);
    let i = cut("cube.stl");
    assert_eq!(check(&i, Stage::Parsed, "endsolid"), State::Partial);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn randomly_damaged_cad_files_do_not_panic_and_finish_quickly() {
    let dir = temp_dir("damage");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for name in FIXTURES {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for round in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..1 + round % 8 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed % b.len() as u64) as usize;
                b[at] = (seed >> 32) as u8;
            }
            let p = dir.join(format!("{round}-{name}"));
            std::fs::write(&p, &b).unwrap();
            let started = std::time::Instant::now();
            sound(&p, inspect(&p));
            assert!(started.elapsed().as_secs() < 2, "{name} round {round}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_sizes_counts_and_nesting_are_bounded() {
    let dir = temp_dir("hostile");
    let run = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        let started = std::time::Instant::now();
        let r = inspect(&p);
        let r = sound(&p, r);
        assert!(started.elapsed().as_secs() < 2, "{name}");
        r
    };
    // Binary STL declaring four billion triangles in a tiny file.
    let mut stl = vec![0u8; 80];
    stl.extend(u32::MAX.to_le_bytes());
    stl.extend([0u8; 50]);
    let i = run("huge.stl", &stl).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Parsed, "the file is cut"), State::Partial);
    // GLB whose chunk length runs past the end.
    let mut glb = b"glTF".to_vec();
    glb.extend(2u32.to_le_bytes());
    glb.extend(40u32.to_le_bytes());
    glb.extend(u32::MAX.to_le_bytes());
    glb.extend(0x4E4F_534Au32.to_le_bytes());
    glb.extend(b"{\"asset\":{\"version\":\"2.0\"}}");
    let _ = run("bad.glb", &glb);
    // glTF accessors pointing anywhere.
    let gltf = br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0,0,7]}],"nodes":[{"mesh":0,"children":[0]}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":5}]}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":4000000000,"type":"VEC3"}],
        "bufferViews":[{"buffer":0,"byteOffset":18446744073709551615,"byteLength":12}],
        "buffers":[{"byteLength":12,"uri":"data:application/octet-stream;base64,AAAA"}]}"#;
    let i = run("bad.gltf", gltf).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "References"), State::Failed);
    // Deeply nested STEP parameters and a self-referencing assembly.
    let mut step = String::from(
        "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n",
    );
    step.push_str(&format!(
        "#1=X({}{});\n",
        "(".repeat(5000),
        ")".repeat(5000)
    ));
    step.push_str("#2=PRODUCT('A','A','',());\n#3=PRODUCT_DEFINITION_FORMATION('','',#2);\n#4=PRODUCT_DEFINITION('','',#3,$);\n");
    step.push_str(
        "#5=NEXT_ASSEMBLY_USAGE_OCCURRENCE('1','loop','',#4,#4,$);\nENDSEC;\nEND-ISO-10303-21;\n",
    );
    let _ = run("deep.step", step.as_bytes()).unwrap().unwrap();
    // IGES directory entry with an absurd parameter pointer and line count.
    let mut iges = std::fs::read_to_string(fixture("drawing.igs")).unwrap();
    iges = iges.replacen("     110       1", "     11099999999", 1);
    let _ = run("bad.igs", iges.as_bytes()).unwrap().unwrap();
    // G-code with an enormous arc radius and a zero-length arc.
    let _ = run(
        "arcs.gcode",
        b"G21\nG90\nG1 X0 Y0 F100\nG2 X0 Y0 I1e300 J0\nG3 X1 Y0 R0\nG2 X5 Y5 R1\n",
    )
    .unwrap();
    // Plain files of these extensions are not claimed.
    assert!(run("note.stl", b"hello").is_none());
    assert!(run("data.nc", b"CDF\x01\x00\x00\x00\x00").is_none());
    assert!(run("thing.obj", b"\x4c\x01\x02\x00binary object file").is_none());
    assert!(run("config.json", br#"{"asset": 1, "name": "x"}"#).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
