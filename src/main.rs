//! iediff — browse and compare Infinity Engine resources.
//!
//! Format parsers live in [`formats`]. The crate root keeps the shared,
//! format-agnostic building blocks (the byte [`Reader`] and the [`Truncated`]
//! error) plus the CLI driver.

mod formats;

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::process::ExitCode;

use formats::key::{KeyFile, type_extension};

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

/// Print a summary of the parsed KEY, enough to verify the parser by eye.
fn report(key: &KeyFile) {
    println!(
        "KEY V1 parsed: {} BIFs, {} resources",
        key.bifs().len(),
        key.resources().len()
    );

    println!("\nFirst BIFs:");
    for bif in key.bifs().iter().take(5) {
        println!(
            "  {:<28} {:>10} bytes  flags=0x{:04x}",
            bif.name(),
            bif.file_length(),
            bif.flags()
        );
    }

    // Resource counts per type — a good sanity check on the whole table.
    let mut counts: HashMap<u16, usize> = HashMap::new();
    for res in key.resources() {
        *counts.entry(res.res_type()).or_insert(0) += 1;
    }

    let mut by_count: Vec<(u16, usize)> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1));

    println!("\nTop resource types:");
    for (res_type, count) in by_count.iter().take(8) {
        println!(
            "  {:<5} (0x{:04x})  {:>6}",
            type_extension(*res_type),
            res_type,
            count
        );
    }

    // Demonstrate a round-trip lookup using the first resource entry.
    if let Some(first) = key.resources().first() {
        let ext = type_extension(first.res_type());
        println!("\nExample lookup: {}.{}", first.name(), ext);
        match key.lookup(first.name(), first.res_type()) {
            Some(res) => {
                let bif = key.bif_name(res.source_index()).unwrap_or("<out of range>");
                println!(
                    "  found in BIF #{} ({}), file index {}, tileset {}",
                    res.source_index(),
                    bif,
                    res.file_index(),
                    res.tileset_index()
                );
            }
            None => println!("  not found (lookup index inconsistent)"),
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_KEY_PATH.to_string());
    let data = std::fs::read(&path)?;
    let key = KeyFile::parse(&data)?;
    report(&key);
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
