//! The preview pane of Windows Explorer (Alt+P, or View › Preview pane): a COM preview handler
//! that shows the page Isolinear builds for a file (`isolinear-preview`) in WebView2, with
//! scripts, the context menu and developer tools turned off. Explorer runs it in its preview host
//! process. Nothing is registered by installing the app: `isolinear
//! --enable-file-manager-preview` writes the user's registry keys ([`register`]) and
//! `--disable-file-manager-preview` removes them.

#[cfg(windows)]
mod handler;
#[cfg(windows)]
mod registry;

#[cfg(windows)]
pub use registry::{register, unregister};

/// The class of the handler, as registered.
pub const CLSID: &str = "{39085B8B-A5F4-4A5B-9C0E-94459B4E55B2}";

/// File name extensions the preview pane shows with Isolinear: formats Windows has no preview
/// for, and the structured texts and standards Isolinear explains.
pub const EXTENSIONS: &[&str] = &[
    "parquet", "avro", "orc", "arrow", "arrows", "feather", "h5", "hdf5", "nc", "npy", "npz",
    "sqlite", "sqlite3", "db3", "msgpack", "pb", "binpb", "ipynb", "geojson", "edi", "edifact",
    "x12", "hl7", "dcm", "bib", "toml", "yaml", "yml", "md", "markdown", "fhir", "xbrl", "ifc",
    "stp", "step", "stl", "las", "gpx", "kml", "sarif", "har", "pcap", "pcapng", "xml", "json",
    "csv", "tsv",
];
