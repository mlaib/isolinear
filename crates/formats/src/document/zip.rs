//! ZIP for Word and OpenDocument packages: the archive reader's listing and checked extraction (ZIP64, CRC-32,
//! size limits). Callers cap each part themselves.
pub(super) use crate::table::archive::reader::{Listing, extract_zip, list_zip};
