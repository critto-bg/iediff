//! iediff — browse and compare Infinity Engine resources.
//!
//! Format parsers live in [`formats`]. The crate root keeps the shared,
//! format-agnostic building blocks (the byte [`Reader`] and the [`Truncated`]
//! error) plus the CLI driver.

#[cfg(test)]
mod fixture;
mod formats;
mod preprocess;
mod resource;
mod script;
mod search;

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use formats::key::{extension_type, type_extension};
use resource::{ResourceError, ResourceManager, Source};
use search::SearchResults;

/// Looked for in the working directory first, so the binary can be dropped into
/// (or `PATH`-invoked from) a game root. Falls back to the development install.
const LOCAL_KEY_PATH: &str = "chitin.key";
const DEFAULT_KEY_PATH: &str = "game/chitin.key";

/// Maximum matches listed per group before truncating, so a short prefix can't
/// flood the terminal.
const SEARCH_LIMIT: usize = 200;

/// Maximum field paths listed per hit. One resource can reference another from
/// hundreds of places — nearly every transition of a dialogue points at its own
/// file — and a count answers "how often" better than an unreadable list.
const FIELD_LIMIT: usize = 8;

/// The 64-byte key used to XOR-encrypt some text resources (mainly IDS, and
/// occasionally 2DA). See `md/file_formats/ie_formats/encryption.md`.
const TEXT_XOR_KEY: [u8; 64] = [
    0x88, 0xa8, 0x8f, 0xba, 0x8a, 0xd3, 0xb9, 0xf5, 0xed, 0xb1, 0xcf, 0xea, 0xaa, 0xe4, 0xb5, 0xfb,
    0xeb, 0x82, 0xf9, 0x90, 0xca, 0xc9, 0xb5, 0xe7, 0xdc, 0x8e, 0xb7, 0xac, 0xee, 0xf7, 0xe0, 0xca,
    0x8e, 0xea, 0xca, 0x80, 0xce, 0xc5, 0xad, 0xb7, 0xc4, 0xd0, 0x84, 0x93, 0xd5, 0xf0, 0xeb, 0xc8,
    0xb4, 0x9d, 0xcc, 0xaf, 0xa5, 0x95, 0xba, 0x99, 0x87, 0xd2, 0x9d, 0xe3, 0x91, 0xba, 0x90, 0xca,
];

/// Short usage guide. `--help` and a bare invocation print this; it needs no
/// game data, so it works without a readable KEY.
const USAGE: &str = "\
iediff — inspect Infinity Engine resources (BG2:EE)

Usage:
  iediff <name>[.ext]      print a resource; a bare name or an inexact match runs a prefix search
  iediff <name>.ext --uses list the resources that reference this resource
  iediff strref:<n>        resolve a dialog.tlk string by index
  iediff strref:<n> --uses list the resources that reference the string
  iediff --file <path>     print a discrete file by path, bypassing the index
                           (a .baf is compiled to a .bcs next to it)
  iediff --stats           summarise the KEY index and override directory
  iediff [--help|-h]       show this help

Options:
  --raw                 machine-readable key=value output (binary formats)
  --biff                bypass the override directory; read the archived original
  --file                read the argument as a filesystem path (type from its extension)
  --uses                find where a resource or string is referenced; the query
                        must name a type (sw1h01.itm, strref:8000)
  --type <ext>          with --uses, scan only one resource type (e.g. itm)
  --out <dir>           with --file on a .baf, write the .bcs into <dir>
  --using <tra>         with --file on a .baf, resolve @N strings from this tra
                        (repeatable; later files win a shared @N)
  --templates <dir>     with --file on a .baf, expand @@name marker lines from
                        <dir>/name.baf (ImprovedAnvil mod convention)

Decoded formats: 2da ids eff itm spl cre pro sto are dlg bcs
Other resource types are located and summarised (type, size) but not parsed.

--uses searches the decoded binary formats (eff itm spl cre pro sto are dlg bcs)
for typed references, so it can find these as targets:
  itm spl cre dlg are pro eff bcs sto 2da bam vvc wav wed bmp mve mos wmp
Effect opcodes are covered in full: every resource an effect names is resolved to
its type. BCS scripts and a DLG's trigger/action scripts are compiled and their
calls' arguments typed, so a script naming an item, spell (by number too),
creature, store or strref is a hard hit. A .pro is named by projectl.ids number
rather than by resref, and resolved through that table. 2DA/IDS tables are
scanned as text and reported separately as candidates.

Reads ./chitin.key if present (run from a game root), else game/chitin.key.
";

/// A read that ran past the end of the available bytes. Shared by all parsers;
/// each format's error type wraps this via `From`.
#[derive(Debug)]
pub struct Truncated {
    context: &'static str,
    offset: usize,
    needed: usize,
    len: usize,
}

impl fmt::Display for Truncated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "truncated while reading {}: need {} byte(s) at offset {}, but data is only {} byte(s)",
            self.context, self.needed, self.offset, self.len
        )
    }
}

impl std::error::Error for Truncated {}

/// A bounds-checked cursor over raw file bytes. Every read is by absolute
/// offset, since IE formats store offsets from the start of the file.
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn slice(
        &self,
        offset: usize,
        len: usize,
        context: &'static str,
    ) -> Result<&'a [u8], Truncated> {
        match offset.checked_add(len) {
            Some(end) if end <= self.data.len() => Ok(&self.data[offset..end]),
            _ => Err(Truncated {
                context,
                offset,
                needed: len,
                len: self.data.len(),
            }),
        }
    }

    pub fn u8_at(&self, offset: usize, context: &'static str) -> Result<u8, Truncated> {
        let b = self.slice(offset, 1, context)?;
        Ok(b[0])
    }

    pub fn u16_at(&self, offset: usize, context: &'static str) -> Result<u16, Truncated> {
        let b = self.slice(offset, 2, context)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32_at(&self, offset: usize, context: &'static str) -> Result<u32, Truncated> {
        let b = self.slice(offset, 4, context)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a fixed `N`-byte array at `offset` (e.g. a 4-byte tag or a packed
    /// colour run). Bounds-checked via [`slice`](Reader::slice), so the
    /// `copy_from_slice` can never panic.
    pub fn array_at<const N: usize>(
        &self,
        offset: usize,
        context: &'static str,
    ) -> Result<[u8; N], Truncated> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.slice(offset, N, context)?);
        Ok(out)
    }

    pub fn tag_at(&self, offset: usize, context: &'static str) -> Result<[u8; 4], Truncated> {
        self.array_at(offset, context)
    }

    /// Read an 8-byte resref field (a NUL-padded resource name), trimming its
    /// padding.
    pub fn resref_at(&self, offset: usize, context: &'static str) -> Result<String, Truncated> {
        Ok(parse_asciiz(self.slice(offset, 8, context)?))
    }
}

/// One place a resource references a TLK string: the `strref` value and a short
/// path naming the field it came from (e.g. `"identified_name"`,
/// `"soundset[42]"`, `"region[3].info_text"`). Each parsed format yields these
/// via its `strref_uses`; a reverse search filters them by value.
pub struct StrrefUse {
    pub value: u32,
    pub path: String,
}

impl StrrefUse {
    pub fn new(value: u32, path: impl Into<String>) -> StrrefUse {
        StrrefUse {
            value,
            path: path.into(),
        }
    }

    /// Prepend a location to the path (`op139.param1` → `equipping[0].op139.param1`),
    /// for a strref found in an effect nested inside a resource.
    pub fn prefixed(self, prefix: &str) -> StrrefUse {
        StrrefUse {
            value: self.value,
            path: format!("{prefix}.{}", self.path),
        }
    }
}

/// One place a resource names another: the resref, the type it names, and a
/// path locating the field (`"container[3].key_item"`). Each format yields
/// these via its `resref_uses`.
///
/// `value` usually borrows rather than owning: a scan builds these for every
/// field of every resource and discards all but a handful, so cloning would
/// double the allocations for nothing. It owns only when the resref is
/// *computed* rather than stored — a compiled script names a spell by number,
/// so the resref (`SPWI001`) exists nowhere in the bytes to borrow from.
pub struct ResrefUse<'a> {
    /// The resource type the resref names (`ITM_TYPE`, `BCS_TYPE`, …).
    pub kind: u16,
    pub value: Cow<'a, str>,
    pub path: String,
    /// Typed only by convention: the engine accepts more than one kind here,
    /// so a consumer must judge it.
    pub soft: bool,
}

impl<'a> ResrefUse<'a> {
    pub fn new(
        kind: u16,
        value: impl Into<Cow<'a, str>>,
        path: impl Into<String>,
    ) -> ResrefUse<'a> {
        ResrefUse {
            kind,
            value: value.into(),
            path: path.into(),
            soft: false,
        }
    }

    /// Mark the reference as untyped-by-convention (see [`soft`](ResrefUse::soft)).
    pub fn soft(mut self) -> ResrefUse<'a> {
        self.soft = true;
        self
    }

    /// Prepend a location to the path (`op122.resource` →
    /// `equipping[0].op122.resource`), for a resref found in an effect nested
    /// inside a resource.
    pub fn prefixed(self, prefix: &str) -> ResrefUse<'a> {
        ResrefUse {
            path: format!("{prefix}.{}", self.path),
            ..self
        }
    }
}

/// One place a resource names a projectile by *number* rather than by resref:
/// a `projectl.ids` entry and the field it came from. Resolving it needs the
/// install's own table, so that is left to the caller that has one.
pub struct ProjectileUse {
    pub number: i64,
    pub path: String,
}

impl ProjectileUse {
    pub fn new(number: i64, path: impl Into<String>) -> ProjectileUse {
        ProjectileUse {
            number,
            path: path.into(),
        }
    }

    /// Prepend a location to the path, as [`ResrefUse::prefixed`] does.
    pub fn prefixed(self, prefix: &str) -> ProjectileUse {
        ProjectileUse {
            path: format!("{prefix}.{}", self.path),
            ..self
        }
    }
}

/// The `chitin.key` to open: the one in the working directory if it exists (a
/// real game root), otherwise the development copy under `game/`.
fn key_path() -> &'static Path {
    let local = Path::new(LOCAL_KEY_PATH);
    if local.is_file() {
        local
    } else {
        Path::new(DEFAULT_KEY_PATH)
    }
}

/// ASCII case-insensitive prefix test, without allocating. Resrefs are ASCII,
/// so this is the basis of case-insensitive name and prefix matching.
pub fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    let h = haystack.as_bytes();
    let p = prefix.as_bytes();
    h.len() >= p.len() && h.iter().zip(p).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Decode a NUL-terminated (and possibly NUL-padded) ASCII field. Trailing
/// garbage after the first NUL is ignored, per the format conventions.
pub fn parse_asciiz(bytes: &[u8]) -> String {
    let end = match bytes.iter().position(|&b| b == 0) {
        Some(i) => i,
        None => bytes.len(),
    };
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Render a 4-byte format tag for diagnostics, e.g. `"KEY " (4b 45 59 20)`.
pub fn describe_tag(tag: &[u8; 4]) -> String {
    let printable: String = tag
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    let hex: Vec<String> = tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{printable}\" ({})", hex.join(" "))
}

/// Render a byte slice as comma-separated decimals, for raw field output of
/// packed byte runs (e.g. a projectile's colour indices).
pub fn join_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Decrypt a text resource if it carries the `0xFFFF` encryption marker;
/// otherwise borrow it unchanged. The cipher is a repeating-key XOR.
pub fn decrypt_text(bytes: &[u8]) -> Cow<'_, [u8]> {
    match bytes {
        [0xff, 0xff, body @ ..] => Cow::Owned(
            body.iter()
                .enumerate()
                .map(|(i, b)| b ^ TEXT_XOR_KEY[i % TEXT_XOR_KEY.len()])
                .collect(),
        ),
        _ => Cow::Borrowed(bytes),
    }
}

/// Resolve a query and print it. A query loads a single resource only when it
/// fully specifies both name and type (`AERIE6.cre`) and that resource exists;
/// every other query — a name without an extension, or a `name.ext` with no
/// exact match — is treated as a case-insensitive prefix search and lists the
/// matches. An empty name (e.g. `.cre`) is an error. `source` carries the
/// `--biff` choice (skip the override layer).
fn lookup_and_print(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    query: &str,
    source: Source,
    raw: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (stem, ext) = match query.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (query, None),
    };

    if stem.is_empty() {
        return Err(format!("no resource name in query: {query:?}").into());
    }

    let type_filter = match ext {
        None => None,
        Some("") => return Err(format!("malformed query (empty extension): {query:?}").into()),
        Some(ext) => match extension_type(&ext.to_ascii_lowercase()) {
            Some(res_type) => Some(res_type),
            None => return Err(format!("unknown resource extension: .{ext}").into()),
        },
    };

    // An exact hit needs both name and type. Try it; a not-found falls through
    // to prefix search (within that type). A name with no type is always a
    // search.
    if let Some(res_type) = type_filter {
        match manager.load(stem, res_type, source) {
            Ok(loaded) => {
                if raw {
                    loaded.resource.write_raw(out)?;
                } else {
                    writeln!(
                        out,
                        "{}.{}  ({})",
                        stem,
                        type_extension(res_type),
                        loaded.origin
                    )?;
                    write!(out, "{}", loaded.resource)?;
                }
                return Ok(());
            }
            Err(ResourceError::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }

    let results = search::search(manager, stem, type_filter, source);
    print_search(out, stem, type_filter, &results)
}

/// Print a discrete file given by path (`--file`), bypassing the KEY/override
/// index. The resource type comes from the file's extension; referenced names
/// still resolve against the game directory, so a modded file staged in a mod
/// tree renders against the install it will be copied into.
fn print_file(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    path: &str,
    raw: bool,
    out_dir: Option<&str>,
    using: &[String],
    templates: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = Path::new(path);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| format!("no extension to identify the resource type: {path:?}"))?;

    // A `.baf` is textual script; compile it to a `.bcs` on disk rather than
    // echoing it back.
    if ext.eq_ignore_ascii_case("baf") {
        return compile_baf(out, manager, file, out_dir, using, templates);
    }
    if out_dir.is_some() {
        return Err("--out applies only when compiling a .baf".into());
    }
    if !using.is_empty() {
        return Err("--using applies only when compiling a .baf".into());
    }
    if templates.is_some() {
        return Err("--templates applies only when compiling a .baf".into());
    }

    let res_type = extension_type(&ext.to_ascii_lowercase())
        .ok_or_else(|| format!("unknown resource extension: .{ext}"))?;

    let loaded = manager.load_file(file, res_type)?;
    if raw {
        loaded.resource.write_raw(out)?;
    } else {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or(path);
        writeln!(out, "{name}  ({})", loaded.origin)?;
        write!(out, "{}", loaded.resource)?;
    }
    Ok(())
}

/// Compile a BAF file to a `.bcs` beside it, or in `out_dir` when given. The
/// vocabulary comes from the game, so a symbol the install does not know is an
/// error rather than a silent pass. `using` tra files (WeiDU's `USING` clause)
/// supply the `@N` strings; later files win a shared `@N`. A `templates`
/// directory expands `@@name` marker lines first (see [`preprocess`]).
fn compile_baf(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    file: &Path,
    out_dir: Option<&str>,
    using: &[String],
    templates: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut text = std::fs::read(file)?;
    if let Some(dir) = templates {
        text = preprocess::expand(&text, Path::new(dir), file)?;
    }
    let vocabulary = manager
        .vocabulary()
        .ok_or("no ACTION.IDS / TRIGGER.IDS in the game to compile against")?;
    let script = if using.is_empty() {
        formats::baf::compile(&text, &vocabulary)?
    } else {
        let strings = load_tra(manager, using)?;
        formats::baf::compile_using(&text, &vocabulary, &strings)?
    };

    let stem = file
        .file_stem()
        .ok_or_else(|| format!("no filename to derive the output from: {}", file.display()))?;
    // A given `--out` must already be a directory; only the default (beside the
    // source) is assumed to exist. Writability surfaces at the write itself.
    let dir = match out_dir {
        Some(dir) => {
            let dir = Path::new(dir);
            if !dir.exists() {
                return Err(format!("--out directory does not exist: {}", dir.display()).into());
            }
            if !dir.is_dir() {
                return Err(format!("--out is not a directory: {}", dir.display()).into());
            }
            dir
        }
        None => file.parent().unwrap_or(Path::new(".")),
    };
    let target = dir.join(stem).with_extension("bcs");
    std::fs::write(&target, script.encode())
        .map_err(|e| format!("could not write {}: {e}", target.display()))?;
    writeln!(out, "compiled {} -> {}", file.display(), target.display())?;
    Ok(())
}

/// Load and merge `using` tra files, resolving each `@N`'s text to the strref it
/// already holds in `dialog.tlk`. Later files override earlier on a shared `@N`.
fn load_tra(
    manager: &mut ResourceManager,
    using: &[String],
) -> Result<TraStrings, Box<dyn std::error::Error>> {
    let mut tra: HashMap<u32, String> = HashMap::new();
    for path in using {
        let bytes = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
        tra.extend(formats::tra::parse(&bytes).map_err(|e| format!("{path}: {e}"))?);
    }
    let mut resolved = HashMap::with_capacity(tra.len());
    for (index, text) in tra {
        resolved.insert(index, manager.strref_for(&text)?.map(|s| s as i32));
    }
    Ok(TraStrings { resolved })
}

/// The `@N` resolver for [`compile_baf`]: each loaded reference paired with the
/// strref its text holds in `dialog.tlk`, or `None` when the tlk lacks it — so a
/// present-but-untranslatable `@N` reads differently from an undefined one.
struct TraStrings {
    resolved: HashMap<u32, Option<i32>>,
}

impl formats::baf::StrrefSource for TraStrings {
    fn resolve(&self, index: u32) -> Result<i32, formats::baf::StrrefError> {
        match self.resolved.get(&index) {
            Some(Some(strref)) => Ok(*strref),
            Some(None) => Err(formats::baf::StrrefError::NotInTlk),
            None => Err(formats::baf::StrrefError::UnknownRef),
        }
    }
}

/// Print prefix-search results, grouped by override vs biff.
fn print_search(
    out: &mut impl Write,
    prefix: &str,
    type_filter: Option<u16>,
    results: &SearchResults,
) -> Result<(), Box<dyn std::error::Error>> {
    let suffix = match type_filter {
        Some(res_type) => format!(".{}", type_extension(res_type)),
        None => String::new(),
    };

    if results.total() == 0 {
        writeln!(out, "no matches for '{prefix}*{suffix}'")?;
        return Ok(());
    }

    writeln!(
        out,
        "{} match(es) for '{prefix}*{suffix}':",
        results.total()
    )?;
    print_group(out, "override", &results.over)?;
    print_group(out, "biff", &results.biff)?;
    Ok(())
}

/// Print one group of search hits, truncated at [`SEARCH_LIMIT`].
fn print_group(
    out: &mut impl Write,
    label: &str,
    hits: &[search::SearchHit],
) -> Result<(), Box<dyn std::error::Error>> {
    if hits.is_empty() {
        return Ok(());
    }
    writeln!(out, "  {label}:")?;
    for hit in hits.iter().take(SEARCH_LIMIT) {
        writeln!(out, "    {}.{}", hit.name, type_extension(hit.res_type))?;
    }
    if hits.len() > SEARCH_LIMIT {
        writeln!(out, "    … and {} more", hits.len() - SEARCH_LIMIT)?;
    }
    Ok(())
}

/// Resolve and print a single `dialog.tlk` string by its strref.
fn print_string(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    number: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let strref: u32 = number
        .parse()
        .map_err(|_| format!("invalid strref: {number}"))?;
    let string = manager.string(strref)?;
    writeln!(out, "strref {strref}:")?;
    writeln!(out, "{string}")?;
    Ok(())
}

/// Report every resource that references a strref (`strref:<n> --uses`), honouring
/// `--biff` and an optional `--type` filter. Each hit shows where it resolved from
/// and which of its fields carry the strref. The typed scan covers the decoded
/// strref-bearing formats (EFF, ITM, SPL, CRE, PRO, STO, ARE, DLG), and BCS
/// scripts, whose strref-bearing calls (`DisplayString`, journal entries, …)
/// name the string exactly. 2DA/IDS text tables and DLG action scripts are
/// scanned heuristically and reported separately as candidates.
fn print_strref_uses(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    number: &str,
    source: Source,
    type_ext: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let strref: u32 = number
        .parse()
        .map_err(|_| format!("invalid strref: {number}"))?;
    if strref == u32::MAX {
        return Err(
            "strref -1 (0xffffffff) is the \"no string\" sentinel, not a searchable index".into(),
        );
    }
    let type_filter = match type_ext {
        None => None,
        Some(ext) => Some(
            extension_type(&ext.to_ascii_lowercase())
                .ok_or_else(|| format!("unknown resource extension: .{ext}"))?,
        ),
    };

    // Lead with the string itself for context, when it resolves.
    match manager.string(strref) {
        Ok(s) => writeln!(out, "strref {strref}: {s:?}")?,
        Err(_) => writeln!(out, "strref {strref}:")?,
    }

    let hits = search::find_strref(manager, strref, source, type_filter);
    let (hard, soft): (Vec<_>, Vec<_>) = hits.iter().partition(|h| !h.soft);

    if hard.is_empty() && soft.is_empty() {
        writeln!(out, "no references found")?;
        return Ok(());
    }

    if !hard.is_empty() {
        writeln!(out, "{} resource(s) reference it:", hard.len())?;
        print_hits(out, &hard)?;
    }
    if !soft.is_empty() {
        writeln!(
            out,
            "{} possible match(es) in 2DA/IDS text or DLG scripts (verify — the number may not be a strref):",
            soft.len()
        )?;
        print_hits(out, &soft)?;
    }
    Ok(())
}

/// Report every resource that names another (`<name>.<ext> --uses`), honouring
/// `--biff` and `--type`. Typed fields are matched exactly; 2DA/IDS text and DLG
/// scripts are scanned heuristically and reported separately as candidates.
///
/// The query must name a type, since one resref can exist as several. The
/// target need not exist — a dangling reference is worth searching for.
fn print_resref_uses(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    query: &str,
    source: Source,
    type_ext: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((name, ext)) = query.rsplit_once('.') else {
        return Err(
            format!("--uses needs a resource type: {query}.<ext> (e.g. sw1h01.itm)").into(),
        );
    };
    if name.is_empty() {
        return Err(format!("no resource name in query: {query:?}").into());
    }
    // A resref is 8 bytes on disk, so a longer name can never be stored in one.
    if name.len() > 8 {
        return Err(format!("not a resref (over 8 characters): {name:?}").into());
    }
    let kind = extension_type(&ext.to_ascii_lowercase())
        .ok_or_else(|| format!("unknown resource extension: .{ext}"))?;
    let type_filter = match type_ext {
        None => None,
        Some(ext) => Some(
            extension_type(&ext.to_ascii_lowercase())
                .ok_or_else(|| format!("unknown resource extension: .{ext}"))?,
        ),
    };

    // Lead with where the target itself resolves, for context — and say so when
    // it resolves nowhere, since that changes how to read the results.
    match manager.load(name, kind, source) {
        Ok(loaded) => writeln!(out, "{name}.{ext}  ({})", loaded.origin)?,
        Err(_) => writeln!(out, "{name}.{ext}  (not in the index)")?,
    }

    // Say so rather than scanning: "no references found" would read as a
    // finding when it is a blind spot.
    if !search::REFERENCEABLE_KINDS.contains(&kind) {
        writeln!(
            out,
            "no decoded format names a .{} in a typed field — nothing to search",
            type_extension(kind)
        )?;
        return Ok(());
    }

    let hits = search::find_resref(manager, name, kind, source, type_filter);
    let (hard, soft): (Vec<_>, Vec<_>) = hits.iter().partition(|h| !h.soft);

    if hard.is_empty() && soft.is_empty() {
        writeln!(out, "no references found")?;
        return Ok(());
    }

    if !hard.is_empty() {
        writeln!(out, "{} resource(s) reference it:", hard.len())?;
        print_hits(out, &hard)?;
    }
    if !soft.is_empty() {
        writeln!(
            out,
            "{} possible match(es) in text tables, scripts, or multi-type fields (verify):",
            soft.len()
        )?;
        print_hits(out, &soft)?;
    }
    Ok(())
}

/// Print reverse-search hits, one line each (`NAME.ext  (origin)  fields`),
/// truncated at [`SEARCH_LIMIT`] hits and [`FIELD_LIMIT`] fields per hit.
fn print_hits(
    out: &mut impl Write,
    hits: &[&search::RefHit],
) -> Result<(), Box<dyn std::error::Error>> {
    for hit in hits.iter().take(SEARCH_LIMIT) {
        let shown = hit.fields.len().min(FIELD_LIMIT);
        let mut fields = hit.fields[..shown].join(", ");
        if let Some(hidden) = hit.fields.len().checked_sub(FIELD_LIMIT).filter(|&n| n > 0) {
            fields.push_str(&format!(" (+{hidden} more)"));
        }
        writeln!(
            out,
            "  {}.{}  ({})  {}",
            hit.name,
            type_extension(hit.res_type),
            hit.origin,
            fields,
        )?;
    }
    if hits.len() > SEARCH_LIMIT {
        writeln!(out, "  … and {} more", hits.len() - SEARCH_LIMIT)?;
    }
    Ok(())
}

/// Warn (to stderr) when the KEY lists the same resource more than once — a
/// modded `chitin.key` can do this. Resolution keeps the last entry; this just
/// surfaces the anomaly so it isn't silent.
fn warn_duplicates(key: &formats::key::KeyFile) {
    const SHOWN: usize = 10;
    let dupes = key.duplicates();
    if dupes.is_empty() {
        return;
    }
    eprintln!(
        "warning: KEY lists {} duplicate resource(s); the last entry of each wins",
        dupes.len()
    );
    for (name, res_type) in dupes.iter().take(SHOWN) {
        eprintln!("  {}.{}", name, type_extension(*res_type));
    }
    if dupes.len() > SHOWN {
        eprintln!("  … and {} more", dupes.len() - SHOWN);
    }
}

fn run(out: &mut impl Write) -> Result<(), Box<dyn std::error::Error>> {
    let mut query: Option<String> = None;
    let mut source = Source::Auto;
    let mut raw = false;
    let mut stats = false;
    let mut file = false;
    let mut help = false;
    let mut uses = false;
    let mut type_ext: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut using: Vec<String> = Vec::new();
    let mut templates: Option<String> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // Skip the override layer and read straight from the archives.
            "--biff" => source = Source::Biff,
            // Emit the machine-readable `key=value` view instead of the human one.
            "--raw" => raw = true,
            // Summarise the KEY index and override directory.
            "--stats" => stats = true,
            // Read the query as a filesystem path, bypassing the KEY/override index.
            "--file" => file = true,
            // Report where a `strref:<n>` query's string is referenced.
            "--uses" => uses = true,
            // Narrow `--uses` to a single resource type (e.g. `--type itm`).
            "--type" => {
                type_ext = Some(
                    args.next()
                        .ok_or("--type needs a resource extension (e.g. itm)")?,
                );
            }
            // Directory to write a compiled `.baf`'s `.bcs` into.
            "--out" => {
                out_dir = Some(args.next().ok_or("--out needs a directory")?);
            }
            // A tra file supplying `@N` strings for compilation; repeatable, and
            // later files win on a shared `@N`. See [`compile_baf`].
            "--using" => {
                using.push(args.next().ok_or("--using needs a tra file")?);
            }
            // A directory of `@@name` template fragments for the preprocessor.
            "--templates" => {
                templates = Some(args.next().ok_or("--templates needs a directory")?);
            }
            "--help" | "-h" => help = true,
            _ if query.is_none() => query = Some(arg),
            _ => return Err(format!("unexpected argument: {arg}").into()),
        }
    }

    if help || (query.is_none() && !stats) {
        write!(out, "{USAGE}")?;
        return Ok(());
    }

    let mut manager = ResourceManager::open(key_path())?;
    warn_duplicates(manager.key());

    // `--file` reads a discrete file off disk; the index isn't consulted, but
    // referenced names still resolve against the game directory.
    if file {
        let path = query.ok_or("--file needs a path to a resource file")?;
        return print_file(
            out,
            &mut manager,
            &path,
            raw,
            out_dir.as_deref(),
            &using,
            templates.as_deref(),
        );
    }
    if out_dir.is_some() {
        return Err("--out applies only when compiling a --file .baf".into());
    }
    if !using.is_empty() {
        return Err("--using applies only when compiling a --file .baf".into());
    }
    if templates.is_some() {
        return Err("--templates applies only when compiling a --file .baf".into());
    }

    if stats {
        write!(out, "{}", manager.key().stats())?;
        writeln!(out)?;
        write!(out, "{}", manager.override_stats())?;
        return Ok(());
    }

    // The guard above guarantees a query here; the `None` arm is an inert
    // fallback rather than a panic.
    if let Some(query) = query {
        if type_ext.is_some() && !uses {
            return Err("--type applies only to a --uses query".into());
        }
        // A `strref:N` query resolves a TLK string, or — with `--uses` — reports
        // where that string is referenced; anything else is a resource, printed
        // or (with `--uses`) searched for as the target of other resources.
        match query.strip_prefix("strref:") {
            Some(number) if uses => {
                print_strref_uses(out, &mut manager, number, source, type_ext.as_deref())?
            }
            Some(number) => print_string(out, &mut manager, number)?,
            None if uses => {
                print_resref_uses(out, &mut manager, &query, source, type_ext.as_deref())?
            }
            None => lookup_and_print(out, &mut manager, &query, source, raw)?,
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    let mut out = io::stdout().lock();
    match run(&mut out) {
        Ok(()) => ExitCode::SUCCESS,
        // A closed stdout — e.g. piping into `head` or quitting a pager — is the
        // reader's choice, not an error. Exit quietly, as is conventional.
        Err(e) if is_broken_pipe(e.as_ref()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Whether an error is (or wraps) a broken-pipe I/O error.
fn is_broken_pipe(e: &(dyn std::error::Error + 'static)) -> bool {
    match e.downcast_ref::<io::Error>() {
        Some(io_err) => io_err.kind() == io::ErrorKind::BrokenPipe,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The usage guide lists the target types by hand so it reads well; fail
    /// loudly if it and `REFERENCEABLE_KINDS` drift apart.
    #[test]
    fn usage_lists_every_referenceable_kind() {
        let expected: Vec<&str> = search::REFERENCEABLE_KINDS
            .iter()
            .map(|&kind| type_extension(kind))
            .collect();
        assert!(
            USAGE.contains(&expected.join(" ")),
            "usage must list the referenceable kinds, in order: {}",
            expected.join(" ")
        );
    }
}
