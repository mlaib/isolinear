use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn each_kind_of_file_gets_its_preview() {
    let camt = html(&fixture("standards/camt053.xml"));
    assert!(camt.contains("camt.053"), "Overview");
    assert!(camt.contains("What was checked"));
    let avro = html(&fixture("avro-orc/fastavro-deflate.avro"));
    assert!(
        avro.contains("40 rows × 13 columns") && avro.contains("<td>s1</td>"),
        "table"
    );
    let pdf = html(&fixture("pdf/libreoffice.pdf"));
    assert!(pdf.contains("data:image/png;base64,"), "PDF page");
    let docx = html(&fixture("office/pandoc.docx"));
    assert!(docx.contains("Quick look sample"), "Word");
    assert!(!docx.contains("href="), "Word links are not followed");
    let tif = html(&fixture("tiff/palette8.tif"));
    assert!(tif.contains("data:image/png;base64,"), "TIFF");
    let mp = html(&fixture("doc.msgpack"));
    assert!(mp.contains("&quot;name&quot;"), "MessagePack as JSON");
    let nb = html(&fixture("standards/notebook.ipynb"));
    assert!(nb.contains("<pre>"), "notebook cells");
    let bin = html(&fixture("standards/elf-demo"));
    assert!(
        bin.contains("ELF") || bin.contains("First bytes"),
        "summary"
    );
    for page in [&camt, &avro, &pdf, &docx, &tif, &mp, &nb, &bin] {
        assert!(page.starts_with("<!doctype html>"));
        assert!(!page.to_lowercase().contains("<script"), "no script");
    }
}

#[test]
fn content_from_the_file_is_never_markup() {
    let dir = std::env::temp_dir().join(format!("isolinear-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("evil.md");
    std::fs::write(
        &md,
        "# Hi [site](http://example.org)\n\n<script>alert(1)</script>\n\n<img src=\"http://example.org/x.png\">\n\n![x](http://example.org/y.png)\n",
    )
    .unwrap();
    let page = html(&md);
    assert!(!page.contains("<script>"), "{page}");
    assert!(!page.contains("<img src=\"http"), "{page}");
    assert!(!page.contains("href="), "links are not followed: {page}");
    assert!(page.contains("&lt;script&gt;"));
    assert!(page.contains("Content-Security-Policy"));
    let csv = dir.join("t.csv");
    std::fs::write(&csv, "a,b\n<b>x</b>,\"<script>\"\n").unwrap();
    let page = html(&csv);
    assert!(!page.contains("<b>x</b>") && !page.contains("<script>"));
    // A name that is markup, and a missing file.
    #[cfg(unix)]
    {
        let odd = dir.join("<b>odd<b>.txt");
        std::fs::write(&odd, "plain").unwrap();
        assert!(!html(&odd).contains("<b>odd<b>"));
    }
    assert!(html(&dir.join("missing.bin")).contains("<!doctype html>"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_c_interface_round_trips() {
    let path =
        std::ffi::CString::new(fixture("standards/places.geojson").to_str().unwrap()).unwrap();
    // SAFETY: a valid C string in, the result released once.
    unsafe {
        let page = isolinear_preview_html(path.as_ptr());
        assert!(!page.is_null());
        let text = std::ffi::CStr::from_ptr(page).to_str().unwrap().to_owned();
        isolinear_preview_free(page);
        assert!(text.contains("GeoJSON"));
        assert!(isolinear_preview_html(std::ptr::null()).is_null());
    }
}
