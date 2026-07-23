//! BIFF V1 parser.
//!
//! A BIF is an archive: `header → file-entry table → tileset-entry table →
//! bulk resource data`. The tables are small; the bulk data can be hundreds of
//! megabytes. So we read only the header and entry table up front, keep the
//! file handle open, and `seek`/`read_exact` each requested resource on demand.
//!
//! Only uncompressed `BIFF V1` is handled (what the EE games ship). The
//! zlib-compressed `BIFC`/`BIF ` variants are out of scope.
//! Spec: `md/file_formats/ie_formats/bif_v1.md`.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{Reader, Truncated, describe_tag};

const BIF_SIGNATURE: &[u8; 4] = b"BIFF";
const BIF_VERSION: &[u8; 4] = b"V1  ";

const HEADER_LEN: usize = 0x14;
const FILE_ENTRY_LEN: usize = 16;

/// An opened BIF archive. Holds the file-entry table plus an open handle for
/// reading resource bytes on demand. Tileset (TIS) entries are not yet parsed.
pub struct BifArchive {
    file: File,
    entries: Vec<FileEntry>,
    /// file index -> position in `entries`, for O(1) extraction.
    index: HashMap<u32, usize>,
}

impl BifArchive {
    /// Open a BIF and parse its header and file-entry table. Does not read any
    /// resource data.
    pub fn open(path: &Path) -> Result<BifArchive, BifError> {
        let mut file = File::open(path)?;

        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        let reader = Reader::new(&header);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != BIF_SIGNATURE {
            return Err(BifError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != BIF_VERSION {
            return Err(BifError::BadVersion(version));
        }

        let file_count = reader.u32_at(0x08, "file entry count")? as usize;
        // Tileset count (0x0c) is read for completeness but not yet used.
        let _tileset_count = reader.u32_at(0x0c, "tileset entry count")?;
        let table_offset = reader.u32_at(0x10, "file entry offset")? as u64;

        let entries = Self::read_table(&mut file, file_count, table_offset)?;

        let mut index = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            index.insert(entry.index, i);
        }

        Ok(BifArchive {
            file,
            entries,
            index,
        })
    }

    /// Extract a resource's raw bytes by its file index (locator bits 0-13).
    /// `expected_type` is the resource type the KEY recorded; it's cross-checked
    /// against the BIF's own type field.
    pub fn read_resource(
        &mut self,
        file_index: u32,
        expected_type: u16,
    ) -> Result<Vec<u8>, BifError> {
        let entry = self.entry_for(file_index, expected_type)?;

        self.file.seek(SeekFrom::Start(entry.offset as u64))?;
        let mut buf = vec![0u8; entry.size as usize];
        self.file.read_exact(&mut buf)?;

        Ok(buf)
    }

    /// Read and parse the file-entry table (small: `count * 16` bytes).
    fn read_table(file: &mut File, count: usize, offset: u64) -> Result<Vec<FileEntry>, BifError> {
        let table_len = count
            .checked_mul(FILE_ENTRY_LEN)
            .ok_or(BifError::Malformed("file entry table size overflow"))?;

        let mut table = vec![0u8; table_len];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut table)?;
        let reader = Reader::new(&table);

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let base = i * FILE_ENTRY_LEN;
            let locator = reader.u32_at(base, "file entry locator")?;
            let data_offset = reader.u32_at(base + 4, "file entry offset")?;
            let size = reader.u32_at(base + 8, "file entry size")?;
            // Bytes 14-15 are unused. We keep the type to cross-check against
            // the KEY at read time.
            let res_type = reader.u16_at(base + 12, "file entry type")?;
            entries.push(FileEntry {
                index: locator & 0x3fff,
                offset: data_offset,
                size,
                res_type,
            });
        }

        Ok(entries)
    }

    /// Locate a file entry by index and cross-check its type against the type
    /// the KEY recorded. Shared by the size and read paths.
    fn entry_for(&self, file_index: u32, expected_type: u16) -> Result<FileEntry, BifError> {
        let entry = match self
            .index
            .get(&file_index)
            .and_then(|&i| self.entries.get(i))
        {
            Some(entry) => *entry,
            None => return Err(BifError::ResourceNotFound(file_index)),
        };

        if entry.res_type != expected_type {
            return Err(BifError::TypeMismatch {
                file_index,
                expected: expected_type,
                found: entry.res_type,
            });
        }

        Ok(entry)
    }
}

/// One non-tileset resource slot in the archive. Small and cheap to copy.
#[derive(Clone, Copy)]
struct FileEntry {
    /// File index (locator bits 0-13); matched against the KEY locator.
    index: u32,
    /// Offset of the resource data from the start of the BIF.
    offset: u32,
    size: u32,
    res_type: u16,
}

/// Anything that can go wrong while opening or reading a BIF archive.
#[derive(Debug)]
pub enum BifError {
    /// A read ran past the end of the (small) header/table buffer.
    Truncated(Truncated),
    /// Opening, seeking, or reading the underlying file failed.
    Io(std::io::Error),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
    /// A field describes a structure too large to be plausible.
    Malformed(&'static str),
    /// No file entry in this archive matches the requested file index.
    ResourceNotFound(u32),
    /// The BIF's recorded type disagrees with the type the KEY expected — a sign
    /// of a corrupt index or a mis-resolved locator.
    TypeMismatch {
        file_index: u32,
        expected: u16,
        found: u16,
    },
}

impl fmt::Display for BifError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BifError::Truncated(e) => write!(f, "{e}"),
            BifError::Io(e) => write!(f, "i/o error: {e}"),
            BifError::BadSignature(found) => {
                write!(f, "not a BIF file: signature was {}", describe_tag(found))
            }
            BifError::BadVersion(found) => {
                write!(f, "unsupported BIF version: {}", describe_tag(found))
            }
            BifError::Malformed(what) => write!(f, "malformed BIF: {what}"),
            BifError::ResourceNotFound(index) => {
                write!(f, "no resource with file index {index} in this BIF")
            }
            BifError::TypeMismatch {
                file_index,
                expected,
                found,
            } => write!(
                f,
                "type mismatch for file index {file_index}: \
                 KEY expected 0x{expected:04x}, BIF records 0x{found:04x}"
            ),
        }
    }
}

impl std::error::Error for BifError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BifError::Truncated(e) => Some(e),
            BifError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for BifError {
    fn from(e: Truncated) -> Self {
        BifError::Truncated(e)
    }
}

impl From<std::io::Error> for BifError {
    fn from(e: std::io::Error) -> Self {
        BifError::Io(e)
    }
}
