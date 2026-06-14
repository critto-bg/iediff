//! iediff — browse and compare Infinity Engine resources.
//!
//! Format parsers live in [`formats`]. The crate root keeps the shared,
//! format-agnostic building blocks (the byte [`Reader`] and the [`Truncated`]
//! error) plus the CLI driver.

mod formats;
mod resource;

use std::env;
use std::fmt;
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

/// Look up a single resource and print where it lives and how big it is.
///
/// The query may include an extension (`ABCLASRQ.2da`) to pin the type, or omit
/// it (`ABCLASRQ`). Without an extension, an unambiguous match is printed; an
/// ambiguous one lists the variants so the caller can pick a type.
fn lookup_and_print(
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
                    println!("'{stem}' is ambiguous; specify an extension:");
                    let mut variants: Vec<String> = many
                        .iter()
                        .map(|res| format!("{}.{}", res.name(), type_extension(res.res_type())))
                        .collect();
                    variants.sort();
                    variants.dedup();
                    for variant in variants {
                        println!("  {variant}");
                    }
                    return Ok(());
                }
            }
        }
    };

    let info = manager.info(stem, res_type)?;
    println!("{}.{}", stem, type_extension(info.res_type));
    println!(
        "  type: 0x{:04x} ({})",
        info.res_type,
        type_extension(info.res_type)
    );
    println!("  size: {} bytes", info.size);
    println!("  bif:  {}", info.bif);

    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = ResourceManager::open(Path::new(DEFAULT_KEY_PATH))?;
    match env::args().nth(1) {
        Some(query) => lookup_and_print(&mut manager, &query)?,
        None => print!("{}", manager.key().stats()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
