//! Resource coordinator.
//!
//! [`ResourceManager`] ties the KEY index to the BIF archives: given a resource
//! name and type, it resolves the locator through the KEY, opens (and caches)
//! the owning BIF, and returns the resource's raw bytes. BIFs are opened lazily
//! and kept open, so repeated reads from the same archive reuse one handle.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::formats::bif::{BifArchive, BifError};
use crate::formats::eff::{EFF_TYPE, Eff, EffError};
use crate::formats::ids::{IDS_TYPE, IdsFile};
use crate::formats::key::{KeyError, KeyFile, extension_type, type_extension};
use crate::formats::tlk::{TlkError, TlkFile};
use crate::formats::twoda::{TWODA_TYPE, TwoDA, TwoDAError};

/// Locale whose `dialog.tlk` strings are resolved against. BG2:EE ships several
/// under `lang/`; English is the default. (Selectable later.)
const DEFAULT_LOCALE: &str = "en_US";

/// Anything that can go wrong while resolving or reading a resource.
#[derive(Debug)]
pub enum ResourceError {
    Io(std::io::Error),
    Key(KeyError),
    Bif(BifError),
    Tlk(TlkError),
    TwoDA(TwoDAError),
    Eff(EffError),
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
            ResourceError::Tlk(e) => write!(f, "{e}"),
            ResourceError::TwoDA(e) => write!(f, "{e}"),
            ResourceError::Eff(e) => write!(f, "{e}"),
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
            ResourceError::Tlk(e) => Some(e),
            ResourceError::TwoDA(e) => Some(e),
            ResourceError::Eff(e) => Some(e),
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

impl From<TlkError> for ResourceError {
    fn from(e: TlkError) -> Self {
        ResourceError::Tlk(e)
    }
}

impl From<TwoDAError> for ResourceError {
    fn from(e: TwoDAError) -> Self {
        ResourceError::TwoDA(e)
    }
}

impl From<EffError> for ResourceError {
    fn from(e: EffError) -> Self {
        ResourceError::Eff(e)
    }
}

/// Where to resolve a resource from. `Auto` prefers an override file over the
/// archived original; `Biff` ignores the override entirely (the `--biff` flag).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Auto,
    Biff,
}

/// Where a resolved resource actually came from.
pub enum Origin {
    /// A file in the override directory (its path).
    Override(PathBuf),
    /// A BIF archive, by the name the KEY lists.
    Bif(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Override(path) => write!(f, "override: {}", path.display()),
            Origin::Bif(bif) => write!(f, "biff: {bif}"),
        }
    }
}

/// Where a resource lives and how big it is, without its data.
pub struct ResourceInfo {
    pub res_type: u16,
    pub size: u32,
}

impl fmt::Display for ResourceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "  type: 0x{:04x} ({})",
            self.res_type,
            type_extension(self.res_type)
        )?;
        writeln!(f, "  size: {} bytes", self.size)
    }
}

/// A loaded resource: a parsed view for formats we understand, otherwise the
/// basic metadata. Renders itself via [`Display`](fmt::Display), so callers
/// print it without knowing the concrete format.
pub enum Resource {
    Ids(IdsFile),
    TwoDA(TwoDA),
    Eff(Eff),
    Other(ResourceInfo),
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resource::Ids(ids) => write!(f, "{ids}"),
            Resource::TwoDA(table) => write!(f, "{table}"),
            Resource::Eff(eff) => write!(f, "{eff}"),
            Resource::Other(info) => write!(f, "{info}"),
        }
    }
}

/// A resource resolved to a concrete place, ready to read.
enum Located {
    Override {
        path: PathBuf,
        res_type: u16,
    },
    Bif {
        source: usize,
        file_index: u32,
        res_type: u16,
        bif: String,
    },
}

impl Located {
    fn origin(&self) -> Origin {
        match self {
            Located::Override { path, .. } => Origin::Override(path.clone()),
            Located::Bif { bif, .. } => Origin::Bif(bif.clone()),
        }
    }
}

/// A loaded resource together with where it was resolved from.
pub struct Loaded {
    pub origin: Origin,
    pub resource: Resource,
}

/// Resource types found for a name, grouped by where they live.
pub struct NameMatches {
    /// Types present in the override directory.
    pub over: Vec<u16>,
    /// Types present in the BIF archives (via the KEY).
    pub biff: Vec<u16>,
}

/// Loads a KEY and serves resource bytes out of the BIFs it indexes.
pub struct ResourceManager {
    key: KeyFile,
    /// Root that the KEY's (relative) BIF paths are resolved against.
    game_dir: PathBuf,
    /// BIF source index -> open archive. Populated lazily on first access.
    open_bifs: HashMap<usize, BifArchive>,
    /// The `dialog.tlk` string table, opened lazily on first string lookup.
    dialog: Option<TlkFile>,
    /// Index of the override directory; its resources shadow the BIFs.
    over: Override,
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

        let over = Override::scan(&game_dir);

        Ok(ResourceManager {
            key,
            game_dir,
            open_bifs: HashMap::new(),
            dialog: None,
            over,
        })
    }

    /// The underlying KEY index.
    pub fn key(&self) -> &KeyFile {
        &self.key
    }

    /// Resolve a resource to a concrete location: an override file when one
    /// exists (and `source` permits it), otherwise the BIF the KEY points at.
    fn locate(&self, name: &str, res_type: u16, source: Source) -> Option<Located> {
        if source == Source::Auto {
            if let Some(path) = self.over.path(name, res_type) {
                return Some(Located::Override {
                    path: path.to_path_buf(),
                    res_type,
                });
            }
        }

        let res = self.key.lookup(name, res_type)?;
        let bif = self.key.bif_name(res.source_index())?.to_string();

        Some(Located::Bif {
            source: res.source_index(),
            file_index: res.file_index(),
            res_type: res.res_type(),
            bif,
        })
    }

    /// Load a resource, parsing it when the format is supported and otherwise
    /// returning its metadata. The result records where it was resolved from.
    pub fn load(
        &mut self,
        name: &str,
        res_type: u16,
        source: Source,
    ) -> Result<Loaded, ResourceError> {
        let located =
            self.locate(name, res_type, source)
                .ok_or_else(|| ResourceError::NotFound {
                    name: name.to_string(),
                    res_type,
                })?;

        let origin = located.origin();
        let resource = match res_type {
            IDS_TYPE => Resource::Ids(IdsFile::parse(&self.read_located(&located)?)),
            TWODA_TYPE => Resource::TwoDA(TwoDA::parse(&self.read_located(&located)?)?),
            EFF_TYPE => {
                let mut eff = Eff::parse(&self.read_located(&located)?)?;
                // The primary/secondary types index dictionary tables; resolve
                // them to symbolic names for the human-readable view.
                let primary = self.dictionary_label("MSCHOOL", eff.primary_type());
                let secondary = self.dictionary_label("MSECTYPE", eff.secondary_type());
                eff.set_type_names(primary, secondary);
                Resource::Eff(eff)
            }
            _ => Resource::Other(self.info_located(&located)?),
        };

        Ok(Loaded { origin, resource })
    }

    /// Read a located resource's raw bytes — from disk (override) or the archive.
    fn read_located(&mut self, located: &Located) -> Result<Vec<u8>, ResourceError> {
        match located {
            Located::Override { path, .. } => Ok(std::fs::read(path)?),
            Located::Bif {
                source,
                file_index,
                res_type,
                ..
            } => {
                let archive = self.archive_for(*source)?;
                Ok(archive.read_resource(*file_index, *res_type)?)
            }
        }
    }

    /// Report a located resource's type and size without reading its bulk data.
    fn info_located(&mut self, located: &Located) -> Result<ResourceInfo, ResourceError> {
        match located {
            Located::Override { path, res_type } => {
                let size = std::fs::metadata(path)?.len() as u32;
                Ok(ResourceInfo {
                    res_type: *res_type,
                    size,
                })
            }
            Located::Bif {
                source,
                file_index,
                res_type,
                ..
            } => {
                let archive = self.archive_for(*source)?;
                let size = archive.resource_size(*file_index, *res_type)?;
                Ok(ResourceInfo {
                    res_type: *res_type,
                    size,
                })
            }
        }
    }

    /// Resource types available for a name, split by where they live. Drives
    /// disambiguation of an extension-less query.
    pub fn matches(&self, name: &str) -> NameMatches {
        let mut over: Vec<u16> = self
            .over
            .find_by_name(name)
            .iter()
            .map(|entry| entry.res_type)
            .collect();
        over.sort_unstable();
        over.dedup();

        let mut biff: Vec<u16> = self
            .key
            .find_by_name(name)
            .iter()
            .map(|res| res.res_type())
            .collect();
        biff.sort_unstable();
        biff.dedup();

        NameMatches { over, biff }
    }

    /// Resolve a numeric value to its row label in a dictionary 2DA (e.g.
    /// MSCHOOL, MSECTYPE), where the value is the 0-based row index.
    fn dictionary_label(&mut self, table: &str, value: u32) -> Option<String> {
        let twoda = self.load_2da(table)?;
        twoda.row_label(value as usize).map(str::to_string)
    }

    /// Load and parse a 2DA by name, honouring the override layer. `None` on any
    /// failure — suited to the best-effort dictionary lookups above.
    fn load_2da(&mut self, name: &str) -> Option<TwoDA> {
        let located = self.locate(name, TWODA_TYPE, Source::Auto)?;
        let bytes = self.read_located(&located).ok()?;
        TwoDA::parse(&bytes).ok()
    }

    /// Resolve a strref to its display string from `dialog.tlk`. The TLK is
    /// opened lazily and kept open for subsequent lookups.
    pub fn string(&mut self, strref: u32) -> Result<String, ResourceError> {
        Ok(self.dialog()?.string(strref)?)
    }

    /// Borrow the dialog TLK, opening it on first use from the locale's
    /// `lang/<locale>/dialog.tlk`.
    fn dialog(&mut self) -> Result<&mut TlkFile, ResourceError> {
        match self.dialog {
            Some(ref mut tlk) => Ok(tlk),
            None => {
                let path = self
                    .game_dir
                    .join("lang")
                    .join(DEFAULT_LOCALE)
                    .join("dialog.tlk");
                Ok(self.dialog.insert(TlkFile::open(&path)?))
            }
        }
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

/// One resource sitting in the override directory.
struct OverrideEntry {
    /// Resref as named on disk, without extension (original case).
    name: String,
    res_type: u16,
    path: PathBuf,
}

/// An index of the override directory. Files are named `<resref>.<ext>`; one
/// whose extension maps to a known type shadows the same resource in the BIFs.
struct Override {
    entries: Vec<OverrideEntry>,
    /// (UPPERCASE resref, type) -> index into `entries`. First file wins.
    index: HashMap<(String, u16), usize>,
}

impl Override {
    /// Scan `<game_dir>/override`, indexing every file whose extension names a
    /// known resource type. A missing directory yields an empty index.
    fn scan(game_dir: &Path) -> Override {
        let mut entries = Vec::new();
        let mut index = HashMap::new();

        let read_dir = match std::fs::read_dir(game_dir.join("override")) {
            Ok(read_dir) => read_dir,
            Err(_) => return Override { entries, index },
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // A file is addressable only if its name splits into a resref and an
            // extension that maps to a type; skip anything else.
            let (Some(stem), Some(ext)) = (
                path.file_stem().and_then(OsStr::to_str),
                path.extension().and_then(OsStr::to_str),
            ) else {
                continue;
            };
            let Some(res_type) = extension_type(&ext.to_ascii_lowercase()) else {
                continue;
            };

            index
                .entry((stem.to_ascii_uppercase(), res_type))
                .or_insert(entries.len());
            entries.push(OverrideEntry {
                name: stem.to_string(),
                res_type,
                path,
            });
        }

        Override { entries, index }
    }

    /// Path of the override file for this name and type, if present.
    fn path(&self, name: &str, res_type: u16) -> Option<&Path> {
        let key = (name.to_ascii_uppercase(), res_type);
        self.index
            .get(&key)
            .and_then(|&i| self.entries.get(i))
            .map(|entry| entry.path.as_path())
    }

    /// Every override entry sharing a name, regardless of type.
    fn find_by_name(&self, name: &str) -> Vec<&OverrideEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case(name))
            .collect()
    }
}
