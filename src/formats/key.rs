//! KEY V1 parser.
//!
//! `chitin.key` is the central index that maps every resource (`resref` + type)
//! to the BIF archive and slot that hold it.
//! Spec: `md/file_formats/ie_formats/key_v1.md`.

use std::collections::HashMap;
use std::fmt;

use crate::{Reader, Truncated, describe_tag, parse_asciiz};

const KEY_SIGNATURE: &[u8; 4] = b"KEY ";
const KEY_VERSION: &[u8; 4] = b"V1  ";

const BIF_ENTRY_LEN: usize = 12;
const RESOURCE_ENTRY_LEN: usize = 14;

/// Anything that can go wrong while parsing a KEY file.
#[derive(Debug)]
pub enum KeyError {
    /// A read ran past the end of the file's bytes.
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Truncated(e) => write!(f, "{e}"),
            KeyError::BadSignature(found) => {
                write!(f, "not a KEY file: signature was {}", describe_tag(found))
            }
            KeyError::BadVersion(found) => {
                write!(f, "unsupported KEY version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for KeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeyError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for KeyError {
    fn from(e: Truncated) -> Self {
        KeyError::Truncated(e)
    }
}

/// One BIF archive listed in the KEY directory.
pub struct BifEntry {
    file_length: u32,
    name: String,
    /// Location flags: bit 0 = in `data`, bit 1 = in `cache`, bits 2-7 = CD index.
    flags: u16,
}

impl BifEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn file_length(&self) -> u32 {
        self.file_length
    }

    pub fn flags(&self) -> u16 {
        self.flags
    }
}

/// One resource entry: a name + type pointing at a slot inside a BIF.
pub struct ResourceEntry {
    name: String,
    res_type: u16,
    locator: u32,
}

impl ResourceEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn res_type(&self) -> u16 {
        self.res_type
    }

    /// Index of the owning BIF in [`KeyFile::bifs`] (locator bits 31-20).
    pub fn source_index(&self) -> usize {
        (self.locator >> 20) as usize
    }

    /// Tileset index within the BIF (locator bits 19-14).
    pub fn tileset_index(&self) -> u32 {
        (self.locator >> 14) & 0x3f
    }

    /// Non-tileset file index within the BIF (locator bits 13-0).
    pub fn file_index(&self) -> u32 {
        self.locator & 0x3fff
    }
}

/// A fully parsed KEY file, with a lookup index for resolving resources by name.
pub struct KeyFile {
    bifs: Vec<BifEntry>,
    resources: Vec<ResourceEntry>,
    /// (UPPERCASE name, type) -> index into `resources`. Resrefs are case-insensitive.
    index: HashMap<(String, u16), usize>,
}

impl KeyFile {
    pub fn parse(data: &[u8]) -> Result<KeyFile, KeyError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != KEY_SIGNATURE {
            return Err(KeyError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != KEY_VERSION {
            return Err(KeyError::BadVersion(version));
        }

        let bif_count = reader.u32_at(0x08, "bif count")? as usize;
        let resource_count = reader.u32_at(0x0c, "resource count")? as usize;
        let bif_offset = reader.u32_at(0x10, "bif offset")? as usize;
        let resource_offset = reader.u32_at(0x14, "resource offset")? as usize;

        let bifs = Self::parse_bifs(&reader, bif_count, bif_offset)?;
        let resources = Self::parse_resources(&reader, resource_count, resource_offset)?;

        let mut index = HashMap::with_capacity(resources.len());
        for (i, res) in resources.iter().enumerate() {
            index.insert((res.name.to_ascii_uppercase(), res.res_type), i);
        }

        Ok(KeyFile {
            bifs,
            resources,
            index,
        })
    }

    fn parse_bifs(reader: &Reader, count: usize, offset: usize) -> Result<Vec<BifEntry>, KeyError> {
        let mut bifs = Vec::with_capacity(count);
        for i in 0..count {
            let base = offset + i * BIF_ENTRY_LEN;
            let file_length = reader.u32_at(base, "bif entry length")?;
            let name_offset = reader.u32_at(base + 4, "bif name offset")? as usize;
            let name_length = reader.u16_at(base + 8, "bif name length")? as usize;
            let flags = reader.u16_at(base + 10, "bif flags")?;

            let raw = reader.slice(name_offset, name_length, "bif name")?;
            bifs.push(BifEntry {
                file_length,
                name: parse_asciiz(raw),
                flags,
            });
        }

        Ok(bifs)
    }

    fn parse_resources(
        reader: &Reader,
        count: usize,
        offset: usize,
    ) -> Result<Vec<ResourceEntry>, KeyError> {
        let mut resources = Vec::with_capacity(count);
        for i in 0..count {
            let base = offset + i * RESOURCE_ENTRY_LEN;
            let raw_name = reader.slice(base, 8, "resource name")?;
            let res_type = reader.u16_at(base + 8, "resource type")?;
            let locator = reader.u32_at(base + 10, "resource locator")?;
            resources.push(ResourceEntry {
                name: parse_asciiz(raw_name),
                res_type,
                locator,
            });
        }

        Ok(resources)
    }

    pub fn bifs(&self) -> &[BifEntry] {
        &self.bifs
    }

    pub fn resources(&self) -> &[ResourceEntry] {
        &self.resources
    }

    /// Resolve a resource by name and type (case-insensitive name).
    pub fn lookup(&self, name: &str, res_type: u16) -> Option<&ResourceEntry> {
        let key = (name.to_ascii_uppercase(), res_type);
        self.index.get(&key).and_then(|&i| self.resources.get(i))
    }

    pub fn bif_name(&self, index: usize) -> Option<&str> {
        self.bifs.get(index).map(|b| b.name.as_str())
    }
}

/// Map a resource type code to its file extension (`md/file_formats/general.md`).
pub fn type_extension(res_type: u16) -> &'static str {
    match res_type {
        0x0001 => "bmp",
        0x0002 => "mve",
        0x0004 => "wav",
        0x0005 => "wfx",
        0x0006 => "plt",
        0x03e8 => "bam",
        0x03e9 => "wed",
        0x03ea => "chu",
        0x03eb => "tis",
        0x03ec => "mos",
        0x03ed => "itm",
        0x03ee => "spl",
        0x03ef => "bcs",
        0x03f0 => "ids",
        0x03f1 => "cre",
        0x03f2 => "are",
        0x03f3 => "dlg",
        0x03f4 => "2da",
        0x03f5 => "gam",
        0x03f6 => "sto",
        0x03f7 => "wmp",
        0x03f8 => "eff",
        0x03f9 => "bs",
        0x03fa => "chr",
        0x03fb => "vvc",
        0x03fc => "vef",
        0x03fd => "pro",
        0x03fe => "bio",
        0x03ff => "wbm",
        0x0400 => "fnt",
        0x0402 => "gui",
        0x0403 => "sql",
        0x0404 => "pvrz",
        0x0405 => "glsl",
        0x0406 => "tot",
        0x0407 => "toh",
        0x0408 => "menu",
        0x0409 => "lua",
        0x040a => "ttf",
        0x040b => "png",
        0x0802 => "ini",
        _ => "???",
    }
}
