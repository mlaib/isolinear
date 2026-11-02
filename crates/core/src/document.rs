//! Reading a file: always its first bytes, the whole of it only when it is small enough to edit.
//! Larger files are read by the views that need them ([`crate::paged`] for text).

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::limits::{self, Tier};

/// A file as read from disk.
#[derive(Debug)]
pub struct Opened {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub tier: Tier,
    /// The first [`limits::PROBE`] bytes, used for detection.
    pub probe: Vec<u8>,
    /// The whole file when its tier is [`Tier::Editable`].
    pub bytes: Option<Vec<u8>>,
}

/// Reads a regular file. Directories and special files are refused.
pub fn open(path: &Path) -> io::Result<Opened> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let size = meta.len();
    let tier = Tier::for_size(size);
    let mut file = File::open(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if tier != Tier::Editable {
        let mut probe = vec![0; limits::PROBE];
        let n = read_fully(&mut file, &mut probe)?;
        probe.truncate(n);
        return Ok(Opened {
            path: path.to_owned(),
            name,
            size,
            tier,
            probe,
            bytes: None,
        });
    }
    // The size is a hint: the file may have grown since `metadata`, so read to the end.
    let mut bytes = Vec::with_capacity(size as usize + 1);
    file.read_to_end(&mut bytes)?;
    let probe = bytes[..bytes.len().min(limits::PROBE)].to_vec();
    let tier = Tier::for_size(bytes.len() as u64);
    Ok(Opened {
        path: path.to_owned(),
        name,
        size: bytes.len() as u64,
        tier,
        probe,
        bytes: Some(bytes),
    })
}

fn read_fully(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_small_file_whole() {
        let dir = std::env::temp_dir().join(format!("isolinear-core-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        std::fs::write(&p, b"hello").unwrap();
        let o = open(&p).unwrap();
        assert_eq!(o.name, "a.txt");
        assert_eq!(o.size, 5);
        assert_eq!(o.tier, Tier::Editable);
        assert_eq!(o.probe, b"hello");
        assert_eq!(o.bytes.as_deref(), Some(&b"hello"[..]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_directory() {
        let err = open(&std::env::temp_dir()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn missing_file_is_not_found() {
        let err = open(Path::new("/nonexistent/isolinear")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
