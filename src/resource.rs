//! Resource coordinator.
//!
//! [`ResourceManager`] ties the KEY index to the BIF archives: given a resource
//! name and type, it resolves the locator through the KEY, opens (and caches)
//! the owning BIF, and returns the resource's raw bytes. BIFs are opened lazily
//! and kept open, so repeated reads from the same archive reuse one handle.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::formats::bif::{BifArchive, BifError};
use crate::formats::ids::{IDS_TYPE, IdsFile};
use crate::formats::key::{KeyError, KeyFile, type_extension};

/// Anything that can go wrong while resolving or reading a resource.
#[derive(Debug)]
pub enum ResourceError {
    Io(std::io::Error),
    Key(KeyError),
    Bif(BifError),
    /// No resource with this name and type exists in the KEY.
    NotFound {
        name: String,
        res_type: u16,
    },
    /// The KEY locator points at a BIF index that the KEY doesn't list.
    BifIndexOutOfRange(usize),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::Io(e) => write!(f, "i/o error: {e}"),
            ResourceError::Key(e) => write!(f, "{e}"),
            ResourceError::Bif(e) => write!(f, "{e}"),
            ResourceError::NotFound { name, res_type } => {
                write!(f, "resource not found: {name} (type 0x{res_type:04x})")
            }
            ResourceError::BifIndexOutOfRange(index) => {
                write!(f, "KEY refers to BIF #{index}, which it does not list")
            }
        }
    }
}

impl std::error::Error for ResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResourceError::Io(e) => Some(e),
            ResourceError::Key(e) => Some(e),
            ResourceError::Bif(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ResourceError {
    fn from(e: std::io::Error) -> Self {
        ResourceError::Io(e)
    }
}

impl From<KeyError> for ResourceError {
    fn from(e: KeyError) -> Self {
        ResourceError::Key(e)
    }
}

impl From<BifError> for ResourceError {
    fn from(e: BifError) -> Self {
        ResourceError::Bif(e)
    }
}

/// Where a resource lives and how big it is, without its data.
pub struct ResourceInfo {
    pub res_type: u16,
    pub size: u32,
    /// Name of the BIF archive that holds it, as listed in the KEY.
    pub bif: String,
}

impl fmt::Display for ResourceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "  type: 0x{:04x} ({})",
            self.res_type,
            type_extension(self.res_type)
        )?;
        writeln!(f, "  size: {} bytes", self.size)?;
        writeln!(f, "  bif:  {}", self.bif)
    }
}

/// A loaded resource: a parsed view for formats we understand, otherwise the
/// basic metadata. Renders itself via [`Display`](fmt::Display), so callers
/// print it without knowing the concrete format.
pub enum Resource {
    Ids(IdsFile),
    Other(ResourceInfo),
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resource::Ids(ids) => write!(f, "{ids}"),
            Resource::Other(info) => write!(f, "{info}"),
        }
    }
}

/// Loads a KEY and serves resource bytes out of the BIFs it indexes.
pub struct ResourceManager {
    key: KeyFile,
    /// Root that the KEY's (relative) BIF paths are resolved against.
    game_dir: PathBuf,
    /// BIF source index -> open archive. Populated lazily on first access.
    open_bifs: HashMap<usize, BifArchive>,
}

impl ResourceManager {
    /// Open a KEY file and prepare to serve resources from its game directory
    /// (taken as the KEY file's parent). No BIFs are opened yet.
    pub fn open(key_path: &Path) -> Result<ResourceManager, ResourceError> {
        let data = std::fs::read(key_path)?;
        let key = KeyFile::parse(&data)?;
        let game_dir = key_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        Ok(ResourceManager {
            key,
            game_dir,
            open_bifs: HashMap::new(),
        })
    }

    /// The underlying KEY index.
    pub fn key(&self) -> &KeyFile {
        &self.key
    }

    /// Resolve a resource and report where it lives and how big it is, without
    /// reading the bulk data.
    pub fn info(&mut self, name: &str, res_type: u16) -> Result<ResourceInfo, ResourceError> {
        let (source, file_index, expected_type) = match self.key.lookup(name, res_type) {
            Some(res) => (res.source_index(), res.file_index(), res.res_type()),
            None => {
                return Err(ResourceError::NotFound {
                    name: name.to_string(),
                    res_type,
                });
            }
        };

        let bif = self
            .key
            .bif_name(source)
            .map(str::to_string)
            .ok_or(ResourceError::BifIndexOutOfRange(source))?;

        let archive = self.archive_for(source)?;
        let size = archive.resource_size(file_index, expected_type)?;
        Ok(ResourceInfo {
            res_type: expected_type,
            size,
            bif,
        })
    }

    /// Load a resource, parsing it when the format is supported and otherwise
    /// returning its metadata.
    pub fn load(&mut self, name: &str, res_type: u16) -> Result<Resource, ResourceError> {
        if res_type == IDS_TYPE {
            Ok(Resource::Ids(self.ids(name)?))
        } else {
            Ok(Resource::Other(self.info(name, res_type)?))
        }
    }

    /// Load and parse an IDS table by name.
    pub fn ids(&mut self, name: &str) -> Result<IdsFile, ResourceError> {
        let bytes = self.read(name, IDS_TYPE)?;
        Ok(IdsFile::parse(&bytes))
    }

    /// Resolve a resource by name and type and return its raw bytes. The core
    /// extraction path behind the format parsers.
    pub fn read(&mut self, name: &str, res_type: u16) -> Result<Vec<u8>, ResourceError> {
        let (source, file_index, expected_type) = match self.key.lookup(name, res_type) {
            Some(res) => (res.source_index(), res.file_index(), res.res_type()),
            None => {
                return Err(ResourceError::NotFound {
                    name: name.to_string(),
                    res_type,
                });
            }
        };

        let archive = self.archive_for(source)?;
        Ok(archive.read_resource(file_index, expected_type)?)
    }

    /// Borrow the archive for a BIF source index, opening and caching it on
    /// first use.
    fn archive_for(&mut self, source: usize) -> Result<&mut BifArchive, ResourceError> {
        if !self.open_bifs.contains_key(&source) {
            let bif_name = self
                .key
                .bif_name(source)
                .ok_or(ResourceError::BifIndexOutOfRange(source))?;

            // KEY paths are relative to the game root, possibly with `\` separators.
            let path = self.game_dir.join(bif_name.replace('\\', "/"));
            let archive = BifArchive::open(&path)?;
            self.open_bifs.insert(source, archive);
        }

        self.open_bifs
            .get_mut(&source)
            .ok_or(ResourceError::BifIndexOutOfRange(source))
    }
}
