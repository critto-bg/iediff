//! TLK V1 parser.
//!
//! `dialog.tlk` is the engine's string table: a flat array of strings indexed
//! by a 32-bit `strref`. Most user-visible text — item names, journal entries,
//! and above all dialogue — lives here, so other formats store strrefs and
//! resolve them through this table.
//!
//! The file is large (the string section runs to many megabytes) but access is
//! random by strref, so — like the BIF reader — we hold the header and the
//! fixed-size entry table in memory and `seek`/`read` each string on demand.
//! Spec: `md/file_formats/ie_formats/tlk_v1.md`.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{Reader, Truncated, describe_tag, parse_asciiz};

const TLK_SIGNATURE: &[u8; 4] = b"TLK ";
const TLK_VERSION: &[u8; 4] = b"V1  ";

/// Entries are hardcoded to begin at byte 18; the header is exactly that long.
const HEADER_LEN: usize = 0x12;
const ENTRY_LEN: usize = 26;

/// Anything that can go wrong while opening or reading a TLK file.
#[derive(Debug)]
pub enum TlkError {
    /// A read ran past the end of the (small) header/table buffer.
    Truncated(Truncated),
    /// Opening, seeking, or reading the underlying file failed.
    Io(std::io::Error),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
    /// A field describes a structure too large to be plausible.
    Malformed(&'static str),
    /// No entry exists for this strref (index past the end of the table).
    StrrefOutOfRange(u32),
}

impl fmt::Display for TlkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlkError::Truncated(e) => write!(f, "{e}"),
            TlkError::Io(e) => write!(f, "i/o error: {e}"),
            TlkError::BadSignature(found) => {
                write!(f, "not a TLK file: signature was {}", describe_tag(found))
            }
            TlkError::BadVersion(found) => {
                write!(f, "unsupported TLK version: {}", describe_tag(found))
            }
            TlkError::Malformed(what) => write!(f, "malformed TLK: {what}"),
            TlkError::StrrefOutOfRange(strref) => {
                write!(f, "strref {strref} is out of range")
            }
        }
    }
}

impl std::error::Error for TlkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TlkError::Truncated(e) => Some(e),
            TlkError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for TlkError {
    fn from(e: Truncated) -> Self {
        TlkError::Truncated(e)
    }
}

impl From<std::io::Error> for TlkError {
    fn from(e: std::io::Error) -> Self {
        TlkError::Io(e)
    }
}

/// One string slot: where its bytes sit within the string section, and the
/// sound that plays with it (empty when there is none).
#[derive(Clone)]
struct StringEntry {
    sound: String,
    /// Offset of the string relative to the start of the string section.
    offset: u32,
    length: u32,
}

/// An opened TLK file. Holds the entry table plus an open handle for reading
/// string bytes on demand.
pub struct TlkFile {
    file: File,
    /// Absolute offset of the string-data section.
    string_section: u32,
    entries: Vec<StringEntry>,
}

impl TlkFile {
    /// Open a TLK and parse its header and entry table. Does not read any
    /// string data.
    pub fn open(path: &Path) -> Result<TlkFile, TlkError> {
        let mut file = File::open(path)?;

        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        let reader = Reader::new(&header);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != TLK_SIGNATURE {
            return Err(TlkError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != TLK_VERSION {
            return Err(TlkError::BadVersion(version));
        }

        // Bytes 8-9 (language id) are unused for now.
        let entry_count = reader.u32_at(0x0a, "string count")? as usize;
        let string_section = reader.u32_at(0x0e, "string data offset")?;

        let entries = Self::read_table(&mut file, entry_count)?;

        Ok(TlkFile {
            file,
            string_section,
            entries,
        })
    }

    /// Read and parse the entry table (`count * 26` bytes, starting right after
    /// the header).
    fn read_table(file: &mut File, count: usize) -> Result<Vec<StringEntry>, TlkError> {
        let table_len = count
            .checked_mul(ENTRY_LEN)
            .ok_or(TlkError::Malformed("entry table size overflow"))?;

        let mut table = vec![0u8; table_len];
        file.seek(SeekFrom::Start(HEADER_LEN as u64))?;
        file.read_exact(&mut table)?;
        let reader = Reader::new(&table);

        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let base = i * ENTRY_LEN;
            // Bytes 0-1 (flags) and 10-17 (volume/pitch variance) are unused;
            // the sound resref (2-9) and the string's location matter.
            let sound = reader.resref_at(base + 0x02, "entry sound resref")?;
            let offset = reader.u32_at(base + 0x12, "entry string offset")?;
            let length = reader.u32_at(base + 0x16, "entry string length")?;
            entries.push(StringEntry {
                sound,
                offset,
                length,
            });
        }

        Ok(entries)
    }

    /// Resolve a strref to its string. Empty entries (no text) yield an empty
    /// string; an index past the end of the table is an error.
    pub fn string(&mut self, strref: u32) -> Result<String, TlkError> {
        let entry = self
            .entries
            .get(strref as usize)
            .cloned()
            .ok_or(TlkError::StrrefOutOfRange(strref))?;

        if entry.length == 0 {
            return Ok(String::new());
        }

        let start = self.string_section as u64 + entry.offset as u64;
        self.file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; entry.length as usize];
        self.file.read_exact(&mut buf)?;

        // Some strings are NUL-terminated within their recorded length, others
        // not; trim at the first NUL. EE stores UTF-8, so decode lossily.
        Ok(parse_asciiz(&buf))
    }

    /// The sound resref that plays with a strref, if any. Empty for a string
    /// with no attached sound, or a strref past the table.
    pub fn sound(&self, strref: u32) -> Option<&str> {
        self.entries
            .get(strref as usize)
            .map(|entry| entry.sound.as_str())
            .filter(|sound| !sound.is_empty())
    }
}
