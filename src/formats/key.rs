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

/// One resource entry: a name + type pointing at a slot inside a BIF.
pub struct ResourceEntry {
    name: String,
    res_type: u16,
    locator: u32,
}

impl ResourceEntry {
    /// The resref as stored in the KEY (original case).
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

    /// Non-tileset file index within the BIF (locator bits 13-0).
    pub fn file_index(&self) -> u32 {
        self.locator & 0x3fff
    }
}

/// A fully parsed KEY file, with a lookup index for resolving resources by name.
pub struct KeyFile {
    /// BIF archive paths, relative to the game root, indexed by source index.
    bifs: Vec<String>,
    resources: Vec<ResourceEntry>,
    /// (UPPERCASE name, type) -> index into `resources`. Resrefs are case-insensitive.
    index: HashMap<(String, u16), usize>,
    /// (UPPERCASE name, type) keys the KEY lists more than once. Empty for a
    /// clean KEY; a modded one may append an entry for an existing resref. The
    /// index keeps the last occurrence (newer wins); these are surfaced so the
    /// caller can warn.
    duplicates: Vec<(String, u16)>,
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
        let mut duplicates = Vec::new();
        for (i, res) in resources.iter().enumerate() {
            let key = (res.name.to_ascii_uppercase(), res.res_type);
            // `insert` overwrites, so a duplicate (name, type) resolves to its
            // last occurrence. Record the collision for a caller warning.
            if index.insert(key.clone(), i).is_some() {
                duplicates.push(key);
            }
        }

        Ok(KeyFile {
            bifs,
            resources,
            index,
            duplicates,
        })
    }

    fn parse_bifs(reader: &Reader, count: usize, offset: usize) -> Result<Vec<String>, KeyError> {
        let mut bifs = Vec::with_capacity(count);
        for i in 0..count {
            let base = offset + i * BIF_ENTRY_LEN;
            // Bytes 0-3 (file length) and 10-11 (location flags) are unused for now.
            let name_offset = reader.u32_at(base + 4, "bif name offset")? as usize;
            let name_length = reader.u16_at(base + 8, "bif name length")? as usize;
            let raw = reader.slice(name_offset, name_length, "bif name")?;
            bifs.push(parse_asciiz(raw));
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

    /// Every resource entry the KEY lists, in file order.
    pub fn resources(&self) -> &[ResourceEntry] {
        &self.resources
    }

    /// Resolve a resource by name and type (case-insensitive name).
    pub fn lookup(&self, name: &str, res_type: u16) -> Option<&ResourceEntry> {
        let key = (name.to_ascii_uppercase(), res_type);
        self.index.get(&key).and_then(|&i| self.resources.get(i))
    }

    pub fn bif_name(&self, index: usize) -> Option<&str> {
        self.bifs.get(index).map(String::as_str)
    }

    /// (UPPERCASE name, type) pairs the KEY lists more than once. Empty for a
    /// clean KEY.
    pub fn duplicates(&self) -> &[(String, u16)] {
        &self.duplicates
    }

    /// A high-level summary of the index: archive and resource counts, and a
    /// per-type breakdown (most common first).
    pub fn stats(&self) -> KeyStats {
        let mut counts: HashMap<u16, usize> = HashMap::new();
        for res in &self.resources {
            *counts.entry(res.res_type).or_insert(0) += 1;
        }
        let mut by_type: Vec<(u16, usize)> = counts.into_iter().collect();
        by_type.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        KeyStats {
            bifs: self.bifs.len(),
            resources: self.resources.len(),
            by_type,
        }
    }
}

/// Summary statistics for a KEY index. See [`KeyFile::stats`].
pub struct KeyStats {
    pub bifs: usize,
    pub resources: usize,
    /// (resource type, count), most common first.
    pub by_type: Vec<(u16, usize)>,
}

impl fmt::Display for KeyStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} BIFs, {} resources", self.bifs, self.resources)?;
        writeln!(f, "resources by type:")?;
        for (res_type, count) in &self.by_type {
            writeln!(
                f,
                "  {:<5} (0x{:04x})  {:>6}",
                type_extension(*res_type),
                res_type,
                count
            )?;
        }
        Ok(())
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

/// Map a (lowercase) file extension to its resource type code. The inverse of
/// [`type_extension`]; returns `None` for unknown extensions.
pub fn extension_type(ext: &str) -> Option<u16> {
    let res_type = match ext {
        "bmp" => 0x0001,
        "mve" => 0x0002,
        "wav" => 0x0004,
        "wfx" => 0x0005,
        "plt" => 0x0006,
        "bam" => 0x03e8,
        "wed" => 0x03e9,
        "chu" => 0x03ea,
        "tis" => 0x03eb,
        "mos" => 0x03ec,
        "itm" => 0x03ed,
        "spl" => 0x03ee,
        "bcs" => 0x03ef,
        "ids" => 0x03f0,
        "cre" => 0x03f1,
        "are" => 0x03f2,
        "dlg" => 0x03f3,
        "2da" => 0x03f4,
        "gam" => 0x03f5,
        "sto" => 0x03f6,
        "wmp" => 0x03f7,
        "eff" => 0x03f8,
        "bs" => 0x03f9,
        "chr" => 0x03fa,
        "vvc" => 0x03fb,
        "vef" => 0x03fc,
        "pro" => 0x03fd,
        "bio" => 0x03fe,
        "wbm" => 0x03ff,
        "fnt" => 0x0400,
        "gui" => 0x0402,
        "sql" => 0x0403,
        "pvrz" => 0x0404,
        "glsl" => 0x0405,
        "tot" => 0x0406,
        "toh" => 0x0407,
        "menu" => 0x0408,
        "lua" => 0x0409,
        "ttf" => 0x040a,
        "png" => 0x040b,
        "ini" => 0x0802,
        _ => return None,
    };
    Some(res_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal one-BIF KEY from `(name, type, locator)` resource rows.
    fn build_key(resources: &[(&str, u16, u32)]) -> Vec<u8> {
        let name = b"x.bif\0";
        let bif_name_offset = (0x18 + BIF_ENTRY_LEN) as u32;
        let resource_offset = bif_name_offset + name.len() as u32;

        let mut buf = Vec::new();
        buf.extend_from_slice(KEY_SIGNATURE);
        buf.extend_from_slice(KEY_VERSION);
        buf.extend_from_slice(&1u32.to_le_bytes()); // bif count
        buf.extend_from_slice(&(resources.len() as u32).to_le_bytes());
        buf.extend_from_slice(&0x18u32.to_le_bytes()); // bif offset
        buf.extend_from_slice(&resource_offset.to_le_bytes());
        // BIF entry @ 0x18: length, name offset, name length, location.
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&bif_name_offset.to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name);
        for (n, t, loc) in resources {
            let mut field = [0u8; 8];
            field[..n.len()].copy_from_slice(n.as_bytes());
            buf.extend_from_slice(&field);
            buf.extend_from_slice(&t.to_le_bytes());
            buf.extend_from_slice(&loc.to_le_bytes());
        }
        buf
    }

    #[test]
    fn detects_duplicate_and_keeps_last() {
        let key = KeyFile::parse(&build_key(&[("FOO", 1, 1), ("BAR", 1, 9), ("FOO", 1, 2)]))
            .expect("parse");
        // The repeated (FOO, type 1) is reported once.
        assert_eq!(key.duplicates(), &[("FOO".to_string(), 1)]);
        // Resolution keeps the last occurrence (locator 2).
        assert_eq!(key.lookup("foo", 1).expect("found").file_index(), 2);
    }

    #[test]
    fn clean_key_has_no_duplicates() {
        let key = KeyFile::parse(&build_key(&[("BAZ", 2, 5), ("QUX", 3, 6)])).expect("parse");
        assert!(key.duplicates().is_empty());
    }
}
