//! Medical and scientific formats recognized as standards (`inspect`): DICOM, EDF/BDF, VCF,
//! FASTA, FASTQ, BAM, CRAM, FITS and MATLAB MAT-files.
//!
//! The fixtures in tests/fixtures/standards are synthetic and written by a script that uses only
//! the Python standard library; regenerate them from the repository root with
//!
//!     python3 -I tests/fixtures/standards/make_science.py
//!
//! Damaged variants (cut, mutated, inconsistent) are made by the tests themselves.
use isolinear_formats::{
    detect,
    inspect::{
        Handler, Input, Inspection, Stage, State, Table, Visual, inspect_file, probe, recognize,
    },
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
        let names: Vec<&str> = i.tables.iter().map(|t| t.name.as_str()).collect();
        panic!("no table {name:?} in {names:?}")
    })
}

/// The cell in `column` of the first row whose first cell is `key`.
fn cell<'a>(t: &'a Table, key: &str, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    let row = t
        .rows
        .iter()
        .find(|r| r.cells[0] == key)
        .unwrap_or_else(|| panic!("no row {key:?} in {:?}", t.name));
    &row.cells[c]
}

fn grey(i: &Inspection, n: usize) -> (u32, u32, &[f32], &str) {
    match i
        .visuals
        .iter()
        .filter(|v| matches!(v, Visual::Image { .. }))
        .nth(n)
    {
        Some(Visual::Image {
            width,
            height,
            grey: Some(g),
            caption,
            ..
        }) => (*width, *height, g, caption),
        other => panic!("no grey image {n}: {other:?}"),
    }
}

fn plot(i: &Inspection) -> &[isolinear_formats::inspect::Channel] {
    i.visuals
        .iter()
        .find_map(|v| match v {
            Visual::Plot { channels } => Some(channels.as_slice()),
            _ => None,
        })
        .expect("a plot")
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("isolinear-science-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

// ---------------------------------------------------------------------------------------------
// DICOM

#[test]
fn dicom_explicit_little_endian_ct_with_sequences_and_rescaled_pixels() {
    let i = open("ct_explicit.dcm");
    assert_eq!(i.family, "DICOM");
    assert_eq!(i.kind, "CT Image Storage");
    assert!(
        i.version
            .as_deref()
            .unwrap()
            .starts_with("Explicit VR Little Endian")
    );
    assert_eq!(fact(&i, "Patient name"), "Doe Jane");
    assert_eq!(fact(&i, "Patient ID"), "PID-0042");
    assert_eq!(fact(&i, "Patient birth date"), "1970-01-01");
    assert_eq!(fact(&i, "Patient sex"), "F");
    assert_eq!(fact(&i, "Study date"), "2024-01-15");
    assert_eq!(fact(&i, "Study time"), "10:15:30");
    assert_eq!(fact(&i, "Modality"), "CT");
    assert_eq!(fact(&i, "Manufacturer"), "Example Medical");
    assert_eq!(fact(&i, "Rows"), "6");
    assert_eq!(fact(&i, "Columns"), "8");
    assert_eq!(fact(&i, "Bits stored"), "12");
    assert_eq!(fact(&i, "Pixel representation"), "signed");
    assert_eq!(fact(&i, "Photometric interpretation"), "MONOCHROME2");
    assert_eq!(fact(&i, "Pixel spacing (mm)"), "0.5\\0.5");
    assert_eq!(fact(&i, "Window centre"), "40");
    assert_eq!(fact(&i, "Window width"), "400");
    assert_eq!(fact(&i, "VOI LUT sequence"), "1 item(s)");
    // Patient-identifying facts carry "Patient" in their label, so masking hides them.
    for f in i.facts.iter().filter(|f| f.group == "Patient") {
        assert!(f.label.contains("atient"), "{}", f.label);
    }
    let t = table(&i, "Data elements");
    // Identifying values have a column of their own, masked with the patient facts.
    assert_eq!(cell(t, "(0010,0010)", "Value"), "");
    assert_eq!(
        cell(t, "(0010,0010)", "Patient-identifying value"),
        "Doe^Jane"
    );
    assert_eq!(
        cell(t, "(0008,0090)", "Patient-identifying value"),
        "Smith^Anna"
    );
    assert_eq!(cell(t, "(0008,0060)", "Patient-identifying value"), "");
    assert_eq!(cell(t, "(0010,0010)", "VR"), "PN");
    assert_eq!(cell(t, "(0028,3010)", "Value"), "sequence of 1 item(s)");
    // Nested: an undefined-length item holding a defined-length sequence.
    let deep = t
        .rows
        .iter()
        .find(|r| r.cells[1] == "› › CodeMeaning")
        .expect("nested code meaning");
    assert_eq!(deep.cells[3], "Brain");
    assert!(t.rows.iter().any(|r| r.cells[3] == "0\\100\\200\\300"));
    let pixel = t.rows.iter().find(|r| r.cells[0] == "(7FE0,0010)").unwrap();
    assert_eq!(pixel.cells[3], "binary, 96 bytes");
    // Rows point at their bytes.
    let bytes = std::fs::read(fixture("ct_explicit.dcm")).unwrap();
    let src = pixel.source.clone().unwrap();
    assert_eq!(
        &bytes[src.start as usize..src.start as usize + 4],
        b"\xe0\x7f\x10\x00"
    );
    // 12-bit signed values, rescaled to Hounsfield units: (index * 10 - 100) - 1024.
    let (w, h, g, _) = grey(&i, 0);
    assert_eq!((w, h), (8, 6));
    assert_eq!(g[0], -1124.0);
    assert_eq!(g[47], -654.0);
    assert_eq!(
        check(&i, Stage::Parsed, "data elements read"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Semantic, "SOP class"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "SOP instance"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "Pixel data holds"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
    assert!(i.notes.iter().any(|n| n.contains("not for diagnosis")));
}

#[test]
fn dicom_implicit_monochrome1_frames_private_tags_big_endian_rgb() {
    let i = open("mr_implicit.dcm");
    assert_eq!(i.kind, "MR Image Storage");
    assert!(
        i.version
            .as_deref()
            .unwrap()
            .starts_with("Implicit VR Little Endian")
    );
    assert_eq!(fact(&i, "Number of frames"), "2");
    assert_eq!(fact(&i, "Magnetic field strength (T)"), "1.5");
    let (w, h, g, caption) = grey(&i, 0);
    assert_eq!((w, h), (4, 4));
    // MONOCHROME1 is inverted: the darkest stored value becomes the brightest.
    assert_eq!((g[0], g[15]), (15.0, 0.0));
    assert!(caption.contains("Frame 1 of 2"), "{caption}");
    let t = table(&i, "Data elements");
    assert_eq!(cell(t, "(0009,1001)", "Keyword"), "Private");
    assert_eq!(cell(t, "(0009,1001)", "Value"), "binary, 4 bytes");
    assert_eq!(cell(t, "(0009,0010)", "Keyword"), "PrivateCreator");
    assert_eq!(
        check(&i, Stage::Semantic, "Pixel data holds 2 frame"),
        State::Passed
    );

    let i = open("rgb_big_endian.dcm");
    assert!(i.version.as_deref().unwrap().contains("Big Endian"));
    assert_eq!(fact(&i, "Rows"), "2");
    assert_eq!(fact(&i, "Columns"), "3");
    match &i.visuals[0] {
        Visual::Image {
            width: 3,
            height: 2,
            rgba: Some(px),
            ..
        } => {
            assert_eq!(px.len(), 24);
            assert_eq!(&px[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
        }
        v => panic!("{v:?}"),
    }
}

#[test]
fn dicom_compressed_bare_and_inconsistent() {
    let i = open("jpeg_encapsulated.dcm");
    assert!(i.version.as_deref().unwrap().starts_with("JPEG Baseline"));
    assert!(i.visuals.is_empty());
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("compressed (JPEG Baseline") && n.contains("not decoded")),
        "{:?}",
        i.notes
    );
    let t = table(&i, "Data elements");
    assert_eq!(
        cell(t, "(7FE0,0010)", "Value"),
        "encapsulated pixel data, 1 fragment(s), 38 bytes"
    );
    // The meta header names another SOP instance than the data set.
    assert_eq!(check(&i, Stage::Semantic, "SOP instance"), State::Failed);

    let i = open("bare_dataset.dcm");
    assert_eq!(fact(&i, "Patient name"), "Bare Data");
    assert!(i.notes.iter().any(|n| n.contains("no 128-byte preamble")));
    assert!(i.version.as_deref().unwrap().contains("guessed"));

    // Pixel data shorter than declared: shown as a failed rule, no image, no panic.
    let mut b = std::fs::read(fixture("ct_explicit.dcm")).unwrap();
    b.truncate(b.len() - 20);
    let i = inspect(&temp("cut.dcm", &b)).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Parsed, "ends inside"), State::Partial);
    assert_eq!(fact(&i, "Patient ID"), "PID-0042");
}

// ---------------------------------------------------------------------------------------------
// EDF+ and BDF

#[test]
fn edf_plus_header_signals_annotations_and_plot() {
    let i = open("sample.edf");
    assert_eq!(i.family, "EDF");
    assert!(i.kind.starts_with("EDF+ (continuous)"), "{}", i.kind);
    assert_eq!(fact(&i, "Patient code"), "MCH-0234567");
    assert_eq!(fact(&i, "Patient sex"), "F");
    assert_eq!(fact(&i, "Patient birth date"), "02-MAY-1951");
    assert_eq!(fact(&i, "Patient name"), "Haagse Harry");
    assert_eq!(fact(&i, "Start date"), "2002-03-02");
    assert_eq!(fact(&i, "Start time"), "14:30:00");
    assert_eq!(fact(&i, "Data records"), "3");
    assert_eq!(fact(&i, "Signals"), "2");
    assert_eq!(fact(&i, "Duration"), "00:00:03 (3 s)");
    assert_eq!(fact(&i, "Annotations"), "3");
    let s = table(&i, "Signals");
    assert_eq!(cell(s, "EEG Fpz-Cz", "Sampling rate (Hz)"), "100");
    assert_eq!(cell(s, "EEG Fpz-Cz", "Physical dimension"), "uV");
    assert_eq!(cell(s, "Temp", "Prefiltering"), "");
    assert_eq!(cell(s, "Temp", "Digital min"), "-2048");
    let a = table(&i, "Annotations");
    let rows: Vec<Vec<&str>> = a
        .rows
        .iter()
        .map(|r| r.cells.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        rows,
        [
            vec!["1.5", "0.5", "Arousal"],
            vec!["2.25", "", "Lights off"],
            vec!["2.25", "", "Snoring"]
        ]
    );
    let ch = plot(&i);
    assert_eq!(ch.len(), 2);
    assert_eq!((ch[0].rate, ch[0].samples.len()), (100.0, 300));
    assert_eq!(ch[1].unit, "degC");
    // Digital -2048 and 2047 are the physical 30 and 40 °C.
    assert!((ch[1].samples[0] - 30.0).abs() < 1e-4);
    assert!((ch[1].samples[1] - 40.0).abs() < 1e-4);
    assert_eq!(
        check(&i, Stage::Semantic, "File size matches"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Parsed, "3 data records"), State::Passed);

    // One record short: the size rule fails and the read is partial.
    let mut b = std::fs::read(fixture("sample.edf")).unwrap();
    b.truncate(b.len() - 280);
    let i = inspect(&temp("short.edf", &b)).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "does not match"), State::Failed);
    assert_eq!(check(&i, Stage::Parsed, "Only 2 of 3"), State::Partial);
    assert_eq!(plot(&i)[0].samples.len(), 200);
}

#[test]
fn bdf_24_bit_samples_in_physical_units() {
    let i = open("sample.bdf");
    assert_eq!(i.family, "BDF");
    assert_eq!(i.kind, "BDF — physiological signal recording");
    assert_eq!(fact(&i, "Data records"), "2");
    let ch = plot(&i);
    assert_eq!(ch[0].name, "A1");
    assert_eq!(ch[0].rate, 8.0);
    assert_eq!(ch[0].samples.len(), 16);
    assert_eq!(ch[0].samples[0], -262144.0);
    assert_eq!(ch[0].samples[1], 262143.0);
    assert_eq!(ch[1].samples[8], 1.0);
    assert_eq!(
        check(&i, Stage::Semantic, "File size matches"),
        State::Passed
    );
}

// ---------------------------------------------------------------------------------------------
// VCF, FASTA, FASTQ

#[test]
fn vcf_meta_variants_genotypes_and_counts() {
    for name in ["sample.vcf", "sample.vcf.gz"] {
        let i = open(name);
        assert_eq!(i.family, "VCF");
        assert_eq!(i.version.as_deref(), Some("VCFv4.2"));
        assert_eq!(fact(&i, "Reference"), "GRCh38");
        assert_eq!(fact(&i, "Samples"), "3");
        assert_eq!(fact(&i, "Sample names"), "S1, S2, S3");
        assert_eq!(fact(&i, "Variants"), "5");
        assert_eq!(fact(&i, "Chromosomes"), "2");
        assert_eq!(fact(&i, "SNVs"), "2");
        assert_eq!(fact(&i, "Indels"), "2");
        assert_eq!(fact(&i, "Other (MNV, structural, reference)"), "1");
        assert_eq!(fact(&i, "Multi-allelic sites"), "1");
        assert_eq!(fact(&i, "Passing all filters (PASS)"), "4");
        let v = table(&i, "Variants");
        assert_eq!(v.columns[8..], ["S1", "S2", "S3"]);
        assert_eq!(v.rows.len(), 5);
        assert_eq!(
            v.rows[0].cells[..7],
            ["chr1", "1000", "rs1", "A", "G", "50", "PASS"]
        );
        assert_eq!(v.rows[0].cells[9], "1/1");
        assert_eq!(v.rows[3].cells[8], "0|1");
        assert_eq!(cell(table(&i, "INFO fields"), "AF", "Number"), "A");
        assert_eq!(cell(table(&i, "Contigs"), "chr2", "Length"), "242193529");
        assert_eq!(
            cell(table(&i, "Filters"), "q10", "Description"),
            "Quality below 10"
        );
        assert_eq!(check(&i, Stage::Parsed, "5 variants"), State::Passed);
        assert_eq!(
            check(&i, Stage::Semantic, "declared columns"),
            State::Passed
        );
        assert_eq!(check(&i, Stage::Semantic, "sorted"), State::Passed);
        assert_eq!(check(&i, Stage::Semantic, "##contig"), State::Passed);
        assert_eq!(v.rows[0].source.is_some(), name == "sample.vcf");
    }
    let text = std::fs::read_to_string(fixture("sample.vcf")).unwrap();
    let bad = text.replace("chr2\t500", "chr1\t500")
        + "chr3\t1\t.\tA\tC\t.\t.\t.\tGT\t0/0\t0/0\t0/0\nchr3\tx\t.\tA\n";
    let i = inspect(&temp("bad.vcf", bad.as_bytes())).unwrap().unwrap();
    assert_eq!(check(&i, Stage::Semantic, "malformed"), State::Failed);
    assert_eq!(check(&i, Stage::Semantic, "not sorted"), State::Failed);
    assert_eq!(
        check(&i, Stage::Semantic, "without a ##contig"),
        State::Failed
    );
}

#[test]
fn fasta_sequences_composition_and_n50() {
    for name in ["sample.fa", "sample.fa.gz"] {
        let i = open(name);
        assert_eq!(i.family, "FASTA");
        assert_eq!(fact(&i, "Sequences"), "5");
        assert_eq!(fact(&i, "Total length"), "290");
        assert_eq!(fact(&i, "N50"), "80");
        assert_eq!(fact(&i, "Longest"), "100");
        assert_eq!(fact(&i, "Shortest"), "20");
        assert_eq!(fact(&i, "Composition"), "nucleotide");
        let t = table(&i, "Sequences");
        assert_eq!(cell(t, "seq1", "Description"), "Example contig one");
        assert_eq!(cell(t, "seq1", "Length"), "100");
        assert_eq!(cell(t, "seq1", "GC %"), "50.0");
        assert_eq!(cell(t, "seq1", "First 80 characters").len(), 80);
        assert_eq!(
            cell(t, "seq5", "First 80 characters"),
            "GGGGGCCCCCAAAAATTTTT"
        );
        assert_eq!(check(&i, Stage::Semantic, "unique ID"), State::Passed);
    }
    let i = open("protein.faa");
    assert_eq!(fact(&i, "Composition"), "protein");
    assert_eq!(
        cell(table(&i, "Sequences"), "sp|P69905|HBA_TEST", "Length"),
        "92"
    );
    let i = inspect(&temp("dup.fasta", b";c\nACGT\n>a\nACGT\n>a\n\n"))
        .unwrap()
        .unwrap();
    let state = check(&i, Stage::Semantic, "duplicate ID a");
    assert_eq!(state, State::Failed);
    assert!(
        i.checks
            .iter()
            .any(|c| c.detail.contains("before the first header"))
    );
}

#[test]
fn fastq_reads_qualities_offset_and_structure() {
    for name in ["sample.fastq", "sample.fq.gz"] {
        let i = open(name);
        assert_eq!(i.family, "FASTQ");
        assert_eq!(fact(&i, "Reads read"), "5");
        assert_eq!(fact(&i, "Reads in file"), "5");
        assert!(fact(&i, "Phred offset").starts_with("33"));
        assert_eq!(fact(&i, "Bases read"), "43");
        assert_eq!(fact(&i, "Longest"), "12");
        let t = table(&i, "Reads");
        assert_eq!(cell(t, "r1", "Mean quality"), "40.0");
        // "IIIII#####": five 40s and five 2s.
        assert_eq!(cell(t, "r2", "Mean quality"), "21.0");
        assert_eq!(cell(t, "r3", "First bases"), "ACGTN");
        let ch = plot(&i);
        assert_eq!(ch[0].samples.len(), 12);
        assert_eq!(ch[0].samples[11], 40.0);
        assert_eq!(check(&i, Stage::Parsed, "All 5 reads"), State::Passed);
        assert_eq!(
            check(&i, Stage::Semantic, "Every sampled record"),
            State::Passed
        );
    }
    let i = open("illumina64.fq");
    assert!(fact(&i, "Phred offset").starts_with("64"));
    assert_eq!(cell(table(&i, "Reads"), "s1", "Mean quality"), "40.0");
    let bad = b"@a\nACGT\n+\nIIII\n@b\nACGTA\n+\nIIII\nc\nAC\n+\nII\n@d\nAC\n";
    let i = inspect(&temp("bad.fastq", bad)).unwrap().unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "record 2: sequence length 5"),
        State::Failed
    );
    assert_eq!(check(&i, Stage::Semantic, "3 malformed"), State::Failed);
}

// ---------------------------------------------------------------------------------------------
// BAM and CRAM

#[test]
fn bam_header_references_and_records_across_blocks() {
    let i = open("sample.bam");
    assert_eq!(i.family, "SAM/BAM");
    assert_eq!(i.version.as_deref(), Some("SAM 1.6"));
    assert_eq!(fact(&i, "Sort order"), "coordinate");
    assert_eq!(fact(&i, "Reference sequences"), "2");
    assert_eq!(fact(&i, "Total reference length"), "1500");
    assert_eq!(fact(&i, "Records read"), "4");
    assert_eq!(fact(&i, "Mapped (of records read)"), "3");
    assert_eq!(fact(&i, "Unmapped (of records read)"), "1");
    let t = table(&i, "Alignments");
    let col = |n: &str| t.columns.iter().position(|c| c == n).unwrap();
    let r0 = &t.rows[0].cells;
    assert_eq!(r0[col("Read")], "read1");
    assert_eq!(r0[col("Flag")], "99");
    assert_eq!(
        r0[col("Flag meaning")],
        "paired, proper pair, mate reverse, first"
    );
    assert_eq!(r0[col("Reference")], "chr1");
    assert_eq!(r0[col("Position")], "100");
    assert_eq!(r0[col("MAPQ")], "60");
    assert_eq!(r0[col("CIGAR")], "10M");
    assert_eq!(r0[col("Mate reference")], "=");
    assert_eq!(r0[col("Mate position")], "200");
    assert_eq!(r0[col("Template length")], "110");
    assert_eq!(t.rows[1].cells[col("CIGAR")], "5M1I4M");
    assert_eq!(t.rows[1].cells[col("Template length")], "-110");
    assert_eq!(t.rows[2].cells[col("Reference")], "chr2");
    assert_eq!(t.rows[2].cells[col("Mate reference")], "*");
    assert_eq!(t.rows[3].cells[col("Reference")], "*");
    assert_eq!(t.rows[3].cells[col("CIGAR")], "*");
    assert_eq!(t.rows[3].cells[col("Sequence length")], "8");
    assert_eq!(
        cell(table(&i, "Reference sequences"), "chr2", "Length"),
        "500"
    );
    assert_eq!(table(&i, "@SQ reference sequences").rows.len(), 2);
    assert_eq!(cell(table(&i, "@RG read groups"), "rg1", "SM"), "sampleA");
    assert_eq!(cell(table(&i, "@PG programs"), "bwa", "VN"), "0.7.17");
    assert_eq!(check(&i, Stage::Parsed, "all 4 records"), State::Passed);
    assert_eq!(check(&i, Stage::Semantic, "@SQ"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "end-of-file marker present"),
        State::Passed
    );
    // Without the end-of-file block.
    let b = std::fs::read(fixture("sample.bam")).unwrap();
    let i = inspect(&temp("noeof.bam", &b[..b.len() - 28]))
        .unwrap()
        .unwrap();
    assert_eq!(check(&i, Stage::Semantic, "marker missing"), State::Failed);
    assert_eq!(fact(&i, "Records read"), "4");
}

#[test]
fn cram_definition_header_and_containers_without_decoding() {
    let i = open("sample.cram");
    assert_eq!(i.version.as_deref(), Some("3.0"));
    assert_eq!(fact(&i, "File ID"), "isolinear-test");
    assert_eq!(fact(&i, "Header compression"), "gzip");
    assert_eq!(fact(&i, "Format version"), "1.6");
    assert_eq!(fact(&i, "Reference sequences"), "2");
    assert_eq!(fact(&i, "References with an MD5 checksum"), "1");
    assert_eq!(fact(&i, "Data containers"), "1");
    assert_eq!(fact(&i, "Records (from container headers)"), "3");
    assert_eq!(
        cell(table(&i, "@SQ reference sequences"), "chr1", "M5"),
        "0123456789abcdef0123456789abcdef"
    );
    let c = table(&i, "Containers");
    assert_eq!(c.rows.len(), 2);
    assert_eq!(c.rows[0].cells[1..5], ["0", "100", "150", "3"]);
    assert_eq!(c.rows[1].cells[1], "end of file");
    assert!(i.notes.iter().any(|n| n.contains("never downloads")));
    assert_eq!(
        check(&i, Stage::Parsed, "records not decoded"),
        State::Partial
    );
    assert_eq!(
        check(&i, Stage::Semantic, "End-of-file container present"),
        State::Passed
    );
}

// ---------------------------------------------------------------------------------------------
// FITS

#[test]
fn fits_headers_images_cube_and_tables() {
    let i = open("sample.fits");
    assert_eq!(i.family, "FITS");
    assert_eq!(fact(&i, "HDUs"), "4");
    assert_eq!(fact(&i, "Object"), "M31");
    assert_eq!(fact(&i, "Observer name"), "Ann Example");
    assert_eq!(fact(&i, "Exposure time (s)"), "30.0");
    let types: Vec<&str> = i
        .facts
        .iter()
        .filter(|f| f.label == "Type")
        .map(|f| f.value.as_str())
        .collect();
    assert_eq!(types, ["Primary", "IMAGE", "BINTABLE", "TABLE"]);
    let dims: Vec<&str> = i
        .facts
        .iter()
        .filter(|f| f.label == "Dimensions")
        .map(|f| f.value.as_str())
        .collect();
    assert_eq!(dims[1], "4 × 3 × 2");
    assert_eq!(
        cell(table(&i, "HDU 0 header"), "OBJECT", "Comment"),
        "target"
    );
    // Unsigned 16-bit through BZERO; the first stored row is the bottom one.
    let (w, h, g, _) = grey(&i, 0);
    assert_eq!((w, h), (8, 6));
    assert_eq!(g[0], 40000.0);
    assert_eq!(g[40], 0.0);
    assert_eq!(g[47], 7000.0);
    let (w, h, g, caption) = grey(&i, 1);
    assert_eq!((w, h), (4, 3));
    assert!(
        caption.contains("CUBE") && caption.contains("first plane"),
        "{caption}"
    );
    assert_eq!((g[8], g[0]), (0.0, 4.0));
    let t = table(&i, "HDU 2 table (CATALOG)");
    assert_eq!(t.columns, ["ID", "FLUX [Jy]", "NAME", "VEC", "FLAG"]);
    assert_eq!(t.rows[0].cells, ["1", "1.5", "alpha", "[1, 2, 3]", "T"]);
    assert_eq!(t.rows[1].cells[1], "-0.25");
    assert_eq!(t.rows[2].cells[1], "1000");
    let a = table(&i, "HDU 3 table (ASCII)");
    assert_eq!(a.rows[1].cells, ["Sirius", "-1.460"]);
    assert_eq!(check(&i, Stage::Parsed, "4 header-data"), State::Passed);
    assert_eq!(
        check(&i, Stage::Semantic, "Mandatory keywords"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "2880-byte blocks"),
        State::Passed
    );
    // Cut in the middle of the binary table's data.
    let b = std::fs::read(fixture("sample.fits")).unwrap();
    let i = inspect(&temp("cut.fits", &b[..2880 * 5 + 50]))
        .unwrap()
        .unwrap();
    assert_eq!(
        check(&i, Stage::Semantic, "not a whole number"),
        State::Failed
    );
    assert_eq!(
        check(&i, Stage::Parsed, "runs past the end"),
        State::Partial
    );
}

// ---------------------------------------------------------------------------------------------
// MATLAB

#[test]
fn mat_level5_variables_compressed_cells_structs_and_unsupported() {
    let i = open("level5.mat");
    assert_eq!(i.family, "MATLAB");
    assert_eq!(i.version.as_deref(), Some("Level 5, version 0x0100"));
    assert_eq!(fact(&i, "Platform"), "GLNXA64");
    assert_eq!(fact(&i, "Byte order"), "little-endian");
    assert_eq!(fact(&i, "Variables"), "8");
    assert_eq!(fact(&i, "Compressed variables"), "1");
    assert!(fact(&i, "Unsupported values").contains("fh (function handle)"));
    let t = table(&i, "Variables");
    let get = |name: &str, col: &str| cell(t, name, col).to_owned();
    assert_eq!(get("a", "Class"), "double");
    assert_eq!(get("a", "Dimensions"), "2×3");
    assert_eq!(get("a", "First values"), "1, 4, 2, 5, 3, 6");
    assert_eq!(get("z", "Complex"), "yes");
    assert_eq!(get("z", "First values"), "1+0.5i, 2-1i");
    assert_eq!(get("s", "Class"), "struct");
    assert_eq!(get("s", "Fields"), "x, label");
    assert_eq!(get("› s.x", "First values"), "42");
    assert_eq!(get("› s.label", "First values"), "hello");
    assert_eq!(get("c", "Class"), "cell");
    assert_eq!(get("› c{1}", "Class"), "int8");
    assert_eq!(get("› c{1}", "First values"), "1, 2, -3");
    assert_eq!(get("› c{2}", "First values"), "ok");
    assert_eq!(get("flags", "Class"), "logical");
    assert_eq!(get("sp", "Sparse"), "yes");
    assert_eq!(get("sp", "First values"), "2 non-zero: 7, 9");
    assert_eq!(get("big", "Class"), "uint16");
    assert_eq!(get("big", "First values"), "10, 20, 30, 40, 50");
    assert_eq!(get("fh", "First values"), "not supported");
    assert_eq!(check(&i, Stage::Parsed, "8 variable(s)"), State::Passed);
}

#[test]
fn mat_level4_and_v73() {
    let i = open("level4.mat");
    assert_eq!(i.version.as_deref(), Some("Level 4"));
    let t = table(&i, "Variables");
    assert_eq!(cell(t, "x", "Dimensions"), "2×2");
    assert_eq!(cell(t, "x", "First values"), "1, 3, 2, 4");
    assert_eq!(cell(t, "t", "Class"), "char");
    assert_eq!(cell(t, "t", "First values"), "hello");

    let i = open("v73.mat");
    assert_eq!(i.kind, "MAT-file version 7.3 (HDF5)");
    assert!(i.notes.iter().any(|n| n.contains("HDF5 view")));
    assert_eq!(
        check(&i, Stage::Parsed, "512-byte MATLAB header"),
        State::Partial
    );
    assert!(i.tables.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Robustness

const FIXTURES: &[&str] = &[
    "ct_explicit.dcm",
    "mr_implicit.dcm",
    "rgb_big_endian.dcm",
    "jpeg_encapsulated.dcm",
    "bare_dataset.dcm",
    "sample.edf",
    "sample.bdf",
    "sample.vcf",
    "sample.vcf.gz",
    "sample.fa",
    "sample.fa.gz",
    "protein.faa",
    "sample.fastq",
    "sample.fq.gz",
    "illumina64.fq",
    "sample.bam",
    "sample.cram",
    "sample.fits",
    "level5.mat",
    "level4.mat",
    "v73.mat",
];

/// The handler that recognizes `bytes` under `name`, as `inspect_file` would pick it.
fn handler_for(name: &str, bytes: &[u8]) -> Option<&'static dyn Handler> {
    let path = Path::new(name);
    let head = &bytes[..bytes.len().min(65536)];
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, head).format.id;
    recognize(&probe(path, head, bytes.len() as u64, format))
}

/// Runs a handler directly, without the panic guard of `inspect_file`, so that a panic fails
/// the test; it must also finish quickly.
fn run(handler: &dyn Handler, name: &str, bytes: &[u8], what: &str) {
    let input = Input {
        path: Path::new(name),
        bytes,
        complete: true,
        size: bytes.len() as u64,
    };
    let started = std::time::Instant::now();
    let _ = handler.inspect(&input, &|| false);
    assert!(
        started.elapsed().as_secs() < 2,
        "{name}: {what} took too long"
    );
}

#[test]
fn truncated_and_mutated_inputs_never_panic() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in FIXTURES {
        let bytes = std::fs::read(fixture(name)).unwrap();
        // The fixture's own handler gets every damaged copy, recognized or not; the handler
        // that recognizes the damaged copy (if another) gets it too.
        let own = handler_for(name, &bytes).unwrap_or_else(|| panic!("{name} not recognized"));
        let both = |b: &[u8], what: &str| {
            run(own, name, b, what);
            if let Some(h) = handler_for(name, b).filter(|h| h.name() != own.name()) {
                run(h, name, b, what);
            }
        };
        // Every cut for small files, a sample for larger ones.
        let step = (bytes.len() / 300).max(1);
        for cut in (0..bytes.len()).step_by(step) {
            both(&bytes[..cut], &format!("cut at {cut}"));
        }
        for round in 0..1000 {
            let mut b = bytes.clone();
            for _ in 0..1 + round % 8 {
                let r = next();
                let at = (r % b.len() as u64) as usize;
                b[at] = (r >> 32) as u8;
            }
            both(&b, &format!("mutation round {round}"));
            // A cut after damage, too.
            let r = next() as usize % b.len();
            both(&b[..r], &format!("mutation round {round} cut at {r}"));
        }
    }
}

#[test]
fn ordinary_files_are_not_claimed() {
    // A text file with a FASTA extension but no header, an EDF-like start without a header,
    // a MAT name over random bytes, a .dcm that is text.
    for (name, bytes) in [
        ("notes.fa", &b"just some notes\n"[..]),
        ("notes.fq", b"no reads here\n"),
        ("zero.txt", b"0       this is a text file that happens to start with a zero"),
        ("random.mat", b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18"),
        ("report.dcm", b"Patient report, not a DICOM file.\n"),
        ("simple.txt", b"SIMPLE text\n"),
        ("data.gz", &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    ] {
        let p = temp(name, bytes);
        assert!(inspect(&p).is_none(), "{name} was claimed");
    }
}
