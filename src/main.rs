//! iediff — browse and compare Infinity Engine resources.
//!
//! Format parsers live in [`formats`]. The crate root keeps the shared,
//! format-agnostic building blocks (the byte [`Reader`] and the [`Truncated`]
//! error) plus the CLI driver.

mod formats;
mod resource;

use std::borrow::Cow;
use std::env;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use formats::key::{extension_type, type_extension};
use resource::{ResourceError, ResourceManager, SearchResults, Source};

const DEFAULT_KEY_PATH: &str = "game/chitin.key";

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

/// The 64-byte key used to XOR-encrypt some text resources (mainly IDS, and
/// occasionally 2DA). See `md/file_formats/ie_formats/encryption.md`.
const TEXT_XOR_KEY: [u8; 64] = [
    0x88, 0xa8, 0x8f, 0xba, 0x8a, 0xd3, 0xb9, 0xf5, 0xed, 0xb1, 0xcf, 0xea, 0xaa, 0xe4, 0xb5, 0xfb,
    0xeb, 0x82, 0xf9, 0x90, 0xca, 0xc9, 0xb5, 0xe7, 0xdc, 0x8e, 0xb7, 0xac, 0xee, 0xf7, 0xe0, 0xca,
    0x8e, 0xea, 0xca, 0x80, 0xce, 0xc5, 0xad, 0xb7, 0xc4, 0xd0, 0x84, 0x93, 0xd5, 0xf0, 0xeb, 0xc8,
    0xb4, 0x9d, 0xcc, 0xaf, 0xa5, 0x95, 0xba, 0x99, 0x87, 0xd2, 0x9d, 0xe3, 0x91, 0xba, 0x90, 0xca,
];

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

/// Maximum matches listed per group before truncating, so a short prefix can't
/// flood the terminal.
const SEARCH_LIMIT: usize = 200;

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

    let results = manager.search(stem, type_filter, source);
    print_search(out, stem, type_filter, &results)
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
    hits: &[resource::SearchHit],
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

fn run(out: &mut impl Write) -> Result<(), Box<dyn std::error::Error>> {
    let mut query: Option<String> = None;
    let mut source = Source::Auto;
    let mut raw = false;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            // Skip the override layer and read straight from the archives.
            "--biff" => source = Source::Biff,
            // Emit the machine-readable `key=value` view instead of the human one.
            "--raw" => raw = true,
            _ if query.is_none() => query = Some(arg),
            _ => return Err(format!("unexpected argument: {arg}").into()),
        }
    }

    let mut manager = ResourceManager::open(Path::new(DEFAULT_KEY_PATH))?;
    warn_duplicates(manager.key());
    match query {
        None => {
            write!(out, "{}", manager.key().stats())?;
            writeln!(out)?;
            write!(out, "{}", manager.override_stats())?;
        }
        // A `strref:N` query resolves a TLK string; anything else is a resource.
        Some(query) => match query.strip_prefix("strref:") {
            Some(number) => print_string(out, &mut manager, number)?,
            None => lookup_and_print(out, &mut manager, &query, source, raw)?,
        },
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
