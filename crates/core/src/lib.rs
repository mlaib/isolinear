//! UI-free core: how a file is read, decoded, sized and written back.
//!
//! The bytes on disk are authoritative. Only [`save::write_atomic`] writes to a file, and
//! only on an explicit save.

pub mod document;
pub mod encoding;
pub mod limits;
pub mod paged;
pub mod save;
