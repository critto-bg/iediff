//! Small field-rendering helpers shared by the binary resource formats (ITM,
//! SPL, …). These are format-agnostic mechanics — bit decoding and a couple of
//! common line layouts; the per-format bit tables live with their format.

use std::fmt;

/// Write a `<label>0x...` line, appending the set flag names in parentheses
/// when any are set.
pub fn write_bitfield(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    value: u32,
    names: &[&str],
) -> fmt::Result {
    if names.is_empty() {
        writeln!(f, "{label}0x{value:08x}")
    } else {
        writeln!(f, "{label}0x{value:08x} ({})", names.join(", "))
    }
}

/// Collect the names of the set bits in `value`, given a sparse `(bit, name)`
/// table — for flag words where only some bits are meaningful.
pub fn set_flags(value: u32, table: &[(u32, &'static str)]) -> Vec<&'static str> {
    table
        .iter()
        .filter(|(bit, _)| value & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Collect the names of the set bits in `value`, where bit `i` names entry `i`
/// of `names` (a contiguous bit-to-name table).
pub fn set_bits(value: u32, names: &[&'static str]) -> Vec<&'static str> {
    names
        .iter()
        .enumerate()
        .filter(|(bit, _)| value & (1u32 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Write a `<label>: <value> (<name>)` school line, omitting the parenthetical
/// when the dictionary lookup left the name unresolved.
pub fn write_school(
    f: &mut fmt::Formatter<'_>,
    indent: &str,
    label: &str,
    value: u8,
    name: &Option<String>,
) -> fmt::Result {
    let field = format!("{label}:");
    match name {
        Some(name) => writeln!(f, "{indent}{field:<9} {value} ({name})"),
        None => writeln!(f, "{indent}{field:<9} {value}"),
    }
}
