//! Reading named entries out of an EPUB zip under a decompression budget.
//!
//! Two details matter for safety and determinism:
//!
//! * Entry lookup compares [`normalize_path`] forms, so an OPF href of
//!   `../img/a.png` finds the same entry as `img/a.png`, and the first matching
//!   entry in central-directory order wins.
//! * Both the *declared* uncompressed size and the bytes actually produced are
//!   checked against [`MAX_DECOMPRESSED_ENTRY`], because a crafted archive can
//!   lie in its central directory.

use std::io::{Read, Seek};

use zip::ZipArchive;

use crate::budget::Budget;
use crate::path::normalize_path;
use crate::{MAX_DECOMPRESSED_ENTRY, ReaderError};

/// Index of the first archive entry whose normalized name matches `name`.
pub(crate) fn find_entry<R: Read + Seek>(archive: &ZipArchive<R>, name: &str) -> Option<usize> {
    let wanted = normalize_path(name);
    (0..archive.len()).find(|&index| {
        archive
            .name_for_index(index)
            .is_some_and(|entry| normalize_path(entry) == wanted)
    })
}

/// Read one entry, charging its bytes to `budget`.
pub(crate) fn zip_bytes<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    budget: &mut Budget,
) -> Result<Vec<u8>, ReaderError> {
    let index =
        find_entry(archive, name).ok_or_else(|| ReaderError::MissingEntry(name.to_string()))?;
    let mut file = archive
        .by_index(index)
        .map_err(|error| ReaderError::Io(std::io::Error::other(error)))?;
    if file.size() > MAX_DECOMPRESSED_ENTRY as u64 {
        return Err(ReaderError::EntryTooLarge(MAX_DECOMPRESSED_ENTRY >> 20));
    }
    read_entry(&mut file, budget)
}

/// Read one entry as lossy UTF-8.
///
/// XHTML and OPF are supposed to be UTF-8; a file that is not loses only the
/// offending bytes instead of failing the whole book, matching how Go's
/// `string(entry)` handed raw bytes to the XML/HTML parsers.
pub(crate) fn zip_text<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    budget: &mut Budget,
) -> Result<String, ReaderError> {
    let data = zip_bytes(archive, name, budget)?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Read at most one entry's worth of bytes and charge them to the archive
/// budget.
fn read_entry<R: Read>(reader: &mut R, budget: &mut Budget) -> Result<Vec<u8>, ReaderError> {
    let mut data = Vec::new();
    // Read one byte past the cap so an over-limit entry is detected rather than
    // silently truncated at exactly the limit.
    reader
        .by_ref()
        .take(MAX_DECOMPRESSED_ENTRY as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() as i64 > MAX_DECOMPRESSED_ENTRY {
        return Err(ReaderError::EntryTooLarge(MAX_DECOMPRESSED_ENTRY >> 20));
    }
    budget.take(data.len() as i64)?;
    Ok(data)
}
