//! Bounded reads for policy, desktop metadata and session files.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Check the opened file before reading it. Opening without blocking lets
/// a reader reject a FIFO even when it has no writer.
pub fn read_small_file(
    path: &Path,
    limit: u64,
    check: impl FnOnce(&File, &Metadata) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    check(&file, &meta)?;
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file larger than {limit} bytes"),
        )
    };
    if meta.len() > limit {
        return Err(too_large());
    }
    // A file can grow after fstat, and some virtual files report size zero.
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

pub fn read_small_text(
    path: &Path,
    limit: u64,
    check: impl FnOnce(&File, &Metadata) -> io::Result<()>,
) -> io::Result<String> {
    String::from_utf8(read_small_file(path, limit, check)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_limit_also_applies_to_files_reporting_zero_size() {
        let path = Path::new("/proc/version");
        assert_eq!(path.metadata().unwrap().len(), 0);
        let size = std::fs::read(path).unwrap().len() as u64;
        assert_eq!(
            read_small_file(path, size, |_, _| Ok(())).unwrap().len() as u64,
            size
        );
        assert_eq!(
            read_small_file(path, size - 1, |_, _| Ok(()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
