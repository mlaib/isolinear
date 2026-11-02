//! Bounded HDF5 and NetCDF-4 dataset tables, read without the HDF5 C library.
use super::TableSource;
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
mod reader;
pub const READY: bool = true;
/// Opens on a worker; metadata is read now, dataset pages only when requested.
pub fn open(path: &Path, table: Option<&str>) -> Result<Box<dyn TableSource>, String> {
    open_with_cancel(path, table, Arc::new(AtomicBool::new(false)))
}
/// The same reader with a shared worker cancellation token, also checked while paging.
pub fn open_with_cancel(
    path: &Path,
    table: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<Box<dyn TableSource>, String> {
    reader::Hdf5::open(path, table, cancelled).map(|r| Box::new(r) as Box<dyn TableSource>)
}
