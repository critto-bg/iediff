//! IDS parser.
//!
//! An IDS file is a text lookup table mapping a numeric value to a symbolic
//! identifier (e.g. RACE.IDS: `2 ELF`). Other resources reference these tables
//! to name their fields. Some IDS files are XOR-encrypted (see
//! [`decrypt_text`](crate::decrypt_text)).
//!
//! The format is loosely specified and inconsistent in practice: the optional
//! header may be a signature line (`IDS V1.0`), an (unreliable) entry count, a
//! blank line, or absent entirely. So parsing is lenient — any line that isn't
//! a valid `<value> <identifier>` row is skipped.
//! Spec: `md/file_formats/ie_formats/ids.md`.

use std::collections::HashMap;
use std::fmt;

use crate::decrypt_text;

/// Resource type code for IDS files.
pub const IDS_TYPE: u16 = 0x03f0;

/// One row of an IDS table.
pub struct IdsEntry {
    /// Values are `i64` because the corpus spans negatives and 32-bit-wide
    /// unsigned magnitudes that don't both fit in any 32-bit type.
    pub value: i64,
    pub name: String,
}

/// A parsed IDS table. Preserves entries in file order, and indexes them both
/// ways for dictionary lookups (value -> name and name -> value).
pub struct IdsFile {
    entries: Vec<IdsEntry>,
    /// value -> index into `entries`. First occurrence wins, since a value can
    /// appear more than once with different identifiers.
    by_value: HashMap<i64, usize>,
    /// UPPERCASE name -> index into `entries`. First occurrence wins; names are
    /// matched case-insensitively, as elsewhere in the engine.
    by_name: HashMap<String, usize>,
}

impl IdsFile {
    /// Parse an IDS resource. Decrypts first if needed. Infallible: malformed or
    /// header lines are skipped rather than rejected.
    pub fn parse(raw: &[u8]) -> IdsFile {
        let data = decrypt_text(raw);
        let text = String::from_utf8_lossy(&data);

        let mut entries = Vec::new();
        let mut by_value = HashMap::new();
        let mut by_name = HashMap::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Split into the value token and the rest of the line. A line with
            // no second column (a bare count) is skipped.
            let Some((value_token, rest)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let name = rest.trim();
            if name.is_empty() {
                continue;
            }

            // A non-numeric first token marks a header (e.g. `IDS V1.0`); skip it.
            let Some(value) = parse_value(value_token) else {
                continue;
            };

            let index = entries.len();
            by_value.entry(value).or_insert(index);
            by_name.entry(name.to_ascii_uppercase()).or_insert(index);
            entries.push(IdsEntry {
                value,
                name: name.to_string(),
            });
        }

        IdsFile {
            entries,
            by_value,
            by_name,
        }
    }

    /// Every row in file order. A value may repeat: `ACTION.IDS` gives 451
    /// signatures only 405 distinct ids, so a caller resolving a script call
    /// needs all the candidates, not just the first.
    pub fn entries(&self) -> &[IdsEntry] {
        &self.entries
    }

    /// The identifier mapped to a value, if any (e.g. race `2` -> `ELF`).
    #[allow(dead_code)] // consumed by resource-field annotation (next milestone)
    pub fn lookup_by_value(&self, value: i64) -> Option<&str> {
        self.by_value
            .get(&value)
            .and_then(|&i| self.entries.get(i))
            .map(|entry| entry.name.as_str())
    }

    /// The value mapped to an identifier, if any (e.g. `ELF` -> `2`). Used when
    /// a symbolic name needs resolving back to its number, as in decompiled
    /// scripts. Case-insensitive.
    #[allow(dead_code)] // consumed by script handling (later milestone)
    pub fn lookup_by_name(&self, name: &str) -> Option<i64> {
        self.by_name
            .get(&name.to_ascii_uppercase())
            .and_then(|&i| self.entries.get(i))
            .map(|entry| entry.value)
    }
}

impl fmt::Display for IdsFile {
    /// Human-readable view: entry count, then rows sorted by value for
    /// readability. The stored order (file order) is left untouched for
    /// programmatic access.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} entries", self.entries.len())?;
        let mut rows: Vec<&IdsEntry> = self.entries.iter().collect();
        rows.sort_by_key(|entry| entry.value);
        for entry in rows {
            writeln!(f, "  {:>11}  {}", entry.value, entry.name)?;
        }
        Ok(())
    }
}

/// Parse an IDS value token: hexadecimal (`0x..`) or signed decimal.
fn parse_value(token: &str) -> Option<i64> {
    match token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => token.parse::<i64>().ok(),
    }
}
