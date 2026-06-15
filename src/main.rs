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
use resource::ResourceManager;

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

    pub fn u16_at(&self, offset: usize, context: &'static str) -> Result<u16, Truncated> {
        let b = self.slice(offset, 2, context)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32_at(&self, offset: usize, context: &'static str) -> Result<u32, Truncated> {
        let b = self.slice(offset, 4, context)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn tag_at(&self, offset: usize, context: &'static str) -> Result<[u8; 4], Truncated> {
        let b = self.slice(offset, 4, context)?;
        Ok([b[0], b[1], b[2], b[3]])
    }
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

/// Look up a single resource and print where it lives and how big it is.
///
/// The query may include an extension (`ABCLASRQ.2da`) to pin the type, or omit
/// it (`ABCLASRQ`). Without an extension, an unambiguous match is printed; an
/// ambiguous one lists the variants so the caller can pick a type.
fn lookup_and_print(
    out: &mut impl Write,
    manager: &mut ResourceManager,
    query: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (stem, ext) = match query.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (query, None),
    };

    let res_type = match ext {
        Some(ext) => match extension_type(&ext.to_ascii_lowercase()) {
            Some(res_type) => res_type,
            None => return Err(format!("unknown resource extension: .{ext}").into()),
        },
        // No extension: resolve by name alone. The block ends the KEY borrow
        // before we go on to read through the manager.
        None => {
            let matches = manager.key().find_by_name(stem);
            match matches.as_slice() {
                [] => return Err(format!("resource not found: {query}").into()),
                [only] => only.res_type(),
                many => {
                    writeln!(out, "'{stem}' is ambiguous; specify an extension:")?;
                    let mut variants: Vec<String> = many
                        .iter()
                        .map(|res| format!("{}.{}", res.name(), type_extension(res.res_type())))
                        .collect();
                    variants.sort();
                    variants.dedup();
                    for variant in variants {
                        writeln!(out, "  {variant}")?;
                    }
                    return Ok(());
                }
            }
        }
    };

    // The manager dispatches on type and returns a self-rendering resource; we
    // print an identity heading and its view.
    let resource = manager.load(stem, res_type)?;
    writeln!(out, "{}.{}", stem, type_extension(res_type))?;
    write!(out, "{resource}")?;

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
    let mut manager = ResourceManager::open(Path::new(DEFAULT_KEY_PATH))?;
    match env::args().nth(1) {
        None => write!(out, "{}", manager.key().stats())?,
        // A `strref:N` query resolves a TLK string; anything else is a resource.
        Some(query) => match query.strip_prefix("strref:") {
            Some(number) => print_string(out, &mut manager, number)?,
            None => lookup_and_print(out, &mut manager, &query)?,
        },
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
