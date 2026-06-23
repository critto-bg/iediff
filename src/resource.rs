//! Resource coordinator.
//!
//! [`ResourceManager`] ties the KEY index to the BIF archives: given a resource
//! name and type, it resolves the locator through the KEY, opens (and caches)
//! the owning BIF, and returns the resource's raw bytes. BIFs are opened lazily
//! and kept open, so repeated reads from the same archive reuse one handle.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::Reader;
use crate::formats::bif::{BifArchive, BifError};
use crate::formats::cre::{CRE_TYPE, Cre, CreError, CreNames};
use crate::formats::eff::{EFF_TYPE, Eff, EffError};
use crate::formats::ids::{IDS_TYPE, IdsFile};
use crate::formats::itm::{ITM_TYPE, Itm, ItmError};
use crate::formats::key::{KeyError, KeyFile, extension_type, type_extension};
use crate::formats::spl::{SPL_TYPE, Spl, SplError};
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
    Itm(ItmError),
    Spl(SplError),
    Cre(CreError),
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
            ResourceError::Itm(e) => write!(f, "{e}"),
            ResourceError::Spl(e) => write!(f, "{e}"),
            ResourceError::Cre(e) => write!(f, "{e}"),
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
            ResourceError::Itm(e) => Some(e),
            ResourceError::Spl(e) => Some(e),
            ResourceError::Cre(e) => Some(e),
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

impl From<ItmError> for ResourceError {
    fn from(e: ItmError) -> Self {
        ResourceError::Itm(e)
    }
}

impl From<SplError> for ResourceError {
    fn from(e: SplError) -> Self {
        ResourceError::Spl(e)
    }
}

impl From<CreError> for ResourceError {
    fn from(e: CreError) -> Self {
        ResourceError::Cre(e)
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
    Itm(Itm),
    Spl(Spl),
    Cre(Cre),
    Other(ResourceInfo),
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resource::Ids(ids) => write!(f, "{ids}"),
            Resource::TwoDA(table) => write!(f, "{table}"),
            Resource::Eff(eff) => write!(f, "{eff}"),
            Resource::Itm(itm) => write!(f, "{itm}"),
            Resource::Spl(spl) => write!(f, "{spl}"),
            Resource::Cre(cre) => write!(f, "{cre}"),
            Resource::Other(info) => write!(f, "{info}"),
        }
    }
}

impl Resource {
    /// Write the machine-readable view: flat `key=value` lines for the binary
    /// formats. The text formats (IDS, 2DA) are already field-oriented, so they
    /// reuse their normal rendering.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        match self {
            Resource::Eff(eff) => eff.write_raw(out, ""),
            Resource::Itm(itm) => itm.write_raw(out),
            Resource::Spl(spl) => spl.write_raw(out),
            Resource::Cre(cre) => cre.write_raw(out),
            Resource::Ids(ids) => write!(out, "{ids}"),
            Resource::TwoDA(table) => write!(out, "{table}"),
            Resource::Other(info) => write!(out, "{info}"),
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
    /// Cache of resolved names, keyed by (UPPERCASE resref, type). A creature
    /// may reference the same spell/item many times.
    name_cache: HashMap<(String, u16), ResourceNames>,
    /// Cache of parsed dictionary 2DAs, keyed by UPPERCASE name. `None` records
    /// a table that could not be loaded, so it is not retried.
    twoda_cache: HashMap<String, Option<TwoDA>>,
    /// Cache of parsed IDS tables, keyed by UPPERCASE name.
    ids_cache: HashMap<String, Option<IdsFile>>,
}

/// The display names a resource (ITM/SPL) carries, resolved from its header
/// strrefs. Both are kept so a consumer can pick whichever it needs; most want
/// [`display`](ResourceNames::display).
#[derive(Clone, Default)]
pub struct ResourceNames {
    pub identified: Option<String>,
    pub unidentified: Option<String>,
}

impl ResourceNames {
    /// The name to show by default: identified if resolved, else unidentified.
    pub fn display(&self) -> Option<&str> {
        self.identified.as_deref().or(self.unidentified.as_deref())
    }
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
            name_cache: HashMap::new(),
            twoda_cache: HashMap::new(),
            ids_cache: HashMap::new(),
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
            ITM_TYPE => {
                let mut itm = Itm::parse(&self.read_located(&located)?)?;
                // Each ability's primary/secondary type indexes the same
                // dictionary tables as an EFF; resolve them per ability.
                let codes: Vec<(u32, u32)> = itm
                    .ext_headers()
                    .iter()
                    .map(|ext| (ext.primary_type(), ext.secondary_type()))
                    .collect();
                let names: Vec<(Option<String>, Option<String>)> = codes
                    .into_iter()
                    .map(|(primary, secondary)| {
                        (
                            self.dictionary_label("MSCHOOL", primary),
                            self.dictionary_label("MSECTYPE", secondary),
                        )
                    })
                    .collect();
                for (ext, (primary, secondary)) in itm.ext_headers_mut().iter_mut().zip(names) {
                    ext.set_type_names(primary, secondary);
                }

                // Resolve the item's display names from the TLK.
                let (unid, id) = itm.name_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                itm.set_names(unid, id);

                Resource::Itm(itm)
            }
            SPL_TYPE => {
                let mut spl = Spl::parse(&self.read_located(&located)?)?;

                let primary = self.dictionary_label("MSCHOOL", spl.primary_type());
                let secondary = self.dictionary_label("MSECTYPE", spl.secondary_type());
                spl.set_type_names(primary, secondary);

                let (unid, id) = spl.name_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                spl.set_names(unid, id);

                let (unid, id) = spl.description_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                spl.set_descriptions(unid, id);

                // Each header's projectile indexes projectl.ids, off by one
                // (binary N → ids N-1) in BG2.
                let projectiles: Vec<u16> = spl
                    .ext_headers()
                    .iter()
                    .map(|ext| ext.projectile())
                    .collect();
                let names: Vec<Option<String>> = projectiles
                    .into_iter()
                    .map(|projectile| self.ids_label("PROJECTL", projectile as i64 - 1))
                    .collect();
                for (ext, name) in spl.ext_headers_mut().iter_mut().zip(names) {
                    ext.set_projectile_name(name);
                }

                Resource::Spl(spl)
            }
            CRE_TYPE => {
                let mut cre = Cre::parse(&self.read_located(&located)?)?;

                // Identity: names from the TLK, the IDS octet and animation from
                // their IDS tables.
                let (long, short) = cre.name_strrefs();
                let codes = cre.ids_codes();
                let names = CreNames {
                    long_name: self.string_label(long),
                    short_name: self.string_label(short),
                    ea: self.ids_label("EA", codes.ea as i64),
                    general: self.ids_label("GENERAL", codes.general as i64),
                    race: self.ids_label("RACE", codes.race as i64),
                    class: self.ids_label("CLASS", codes.class as i64),
                    specific: self.ids_label("SPECIFIC", codes.specific as i64),
                    gender: self.ids_label("GENDER", codes.gender as i64),
                    alignment: self.ids_label("ALIGNMEN", codes.alignment as i64),
                    animation: self.ids_label("ANIMATE", codes.animation as i64),
                    // KIT.IDS is indexed by the kit dword with its high
                    // and low 16-bit halves swapped (so e.g. the
                    // stored 0x40000000 → 0x00004000 = TRUECLASS, and the
                    // low-word 0x00004000 → 0x40000000 = BARBARIAN).
                    kit: self.ids_label("KIT", codes.kit.rotate_left(16) as i64),
                };
                cre.set_names(names);

                // Resolve display names for referenced spells (SPL) and items (ITM).
                self.resolve_refs(
                    cre.known_spells_mut(),
                    SPL_TYPE,
                    |s| s.resource(),
                    |s, n| s.set_name(n),
                );
                self.resolve_refs(
                    cre.memorized_spells_mut(),
                    SPL_TYPE,
                    |s| s.resource(),
                    |s, n| s.set_name(n),
                );
                self.resolve_refs(
                    cre.items_mut(),
                    ITM_TYPE,
                    |it| it.resource(),
                    |it, n| it.set_name(n),
                );

                Resource::Cre(cre)
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

    /// Best-effort resolve a strref to its TLK string for display. `None` for
    /// the -1 "no string" sentinel, on any lookup error, or when the string is
    /// empty.
    fn string_label(&mut self, strref: u32) -> Option<String> {
        if strref == u32::MAX {
            return None;
        }
        let string = self.string(strref).ok()?;
        let trimmed = string.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Resolve a numeric value to its row label in a dictionary 2DA (e.g.
    /// MSCHOOL, MSECTYPE), where the value is the 0-based row index.
    fn dictionary_label(&mut self, table: &str, value: u32) -> Option<String> {
        self.load_2da(table)?
            .row_label(value as usize)
            .map(str::to_string)
    }

    /// Borrow a parsed 2DA by name, honouring the override layer. Parsed once
    /// and cached; a load failure is cached as `None` so it is not retried.
    fn load_2da(&mut self, name: &str) -> Option<&TwoDA> {
        let key = name.to_ascii_uppercase();
        if !self.twoda_cache.contains_key(&key) {
            let parsed = self.parse_2da(name);
            self.twoda_cache.insert(key.clone(), parsed);
        }
        self.twoda_cache.get(&key)?.as_ref()
    }

    fn parse_2da(&mut self, name: &str) -> Option<TwoDA> {
        let located = self.locate(name, TWODA_TYPE, Source::Auto)?;
        let bytes = self.read_located(&located).ok()?;
        TwoDA::parse(&bytes).ok()
    }

    /// Resolve a numeric value to its symbol in an IDS table (e.g. PROJECTL),
    /// where the value indexes by its IDS number. `None` when unresolved.
    fn ids_label(&mut self, table: &str, value: i64) -> Option<String> {
        self.load_ids(table)?
            .lookup_by_value(value)
            .map(str::to_string)
    }

    /// Borrow a parsed IDS table by name. Parsed once and cached.
    fn load_ids(&mut self, name: &str) -> Option<&IdsFile> {
        let key = name.to_ascii_uppercase();
        if !self.ids_cache.contains_key(&key) {
            let parsed = self.parse_ids(name);
            self.ids_cache.insert(key.clone(), parsed);
        }
        self.ids_cache.get(&key)?.as_ref()
    }

    fn parse_ids(&mut self, name: &str) -> Option<IdsFile> {
        let located = self.locate(name, IDS_TYPE, Source::Auto)?;
        let bytes = self.read_located(&located).ok()?;
        Some(IdsFile::parse(&bytes))
    }

    /// Resolve display names for a slice of records that reference another
    /// resource by resref (e.g. a creature's spells or items). `get` reads the
    /// resref, `set` stores the resolved name back.
    fn resolve_refs<T>(
        &mut self,
        items: &mut [T],
        res_type: u16,
        get: impl Fn(&T) -> &str,
        set: impl Fn(&mut T, Option<String>),
    ) {
        let refs: Vec<String> = items.iter().map(|t| get(t).to_string()).collect();
        let names: Vec<Option<String>> = refs
            .iter()
            .map(|r| {
                self.resource_names(r, res_type)
                    .display()
                    .map(str::to_string)
            })
            .collect();
        for (t, name) in items.iter_mut().zip(names) {
            set(t, name);
        }
    }

    /// Resolved identified/unidentified names for a referenced resource (ITM/SPL),
    /// read from its header strrefs without a full parse. Cached, since references
    /// repeat (a creature may memorize the same spell hundreds of times).
    fn resource_names(&mut self, resref: &str, res_type: u16) -> ResourceNames {
        if resref.is_empty() {
            return ResourceNames::default();
        }

        let key = (resref.to_ascii_uppercase(), res_type);
        if let Some(cached) = self.name_cache.get(&key) {
            return cached.clone();
        }

        let names = self.read_resource_names(resref, res_type);
        self.name_cache.insert(key, names.clone());
        names
    }

    /// Resolve an ITM/SPL's identified (0x0c) and unidentified (0x08) name
    /// strrefs against the TLK, without a full parse.
    fn read_resource_names(&mut self, resref: &str, res_type: u16) -> ResourceNames {
        let Some(located) = self.locate(resref, res_type, Source::Auto) else {
            return ResourceNames::default();
        };
        let Ok(bytes) = self.read_located(&located) else {
            return ResourceNames::default();
        };
        let reader = Reader::new(&bytes);
        let identified = reader.u32_at(0x0c, "identified name").ok();
        let unidentified = reader.u32_at(0x08, "unidentified name").ok();
        ResourceNames {
            identified: identified.and_then(|s| self.string_label(s)),
            unidentified: unidentified.and_then(|s| self.string_label(s)),
        }
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
