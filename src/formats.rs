//! Parsers for individual Infinity Engine resource formats.
//!
//! Each submodule handles one format and owns its own error type. Shared
//! building blocks (the byte [`Reader`](crate::Reader), the [`Truncated`](crate::Truncated)
//! error, and small parsing helpers) live in the crate root.

pub mod are;
pub mod bif;
pub mod cre;
pub mod eff;
pub mod fields;
pub mod ids;
pub mod itm;
pub mod key;
pub mod opcode;
pub mod pro;
pub mod spl;
pub mod sto;
pub mod tlk;
pub mod twoda;

/// Name an item category code, as used in the `item type` field of an
/// [ITM](itm) header and the "items purchased" list of a [STO](sto). Covers the
/// BG2:EE inventory categories.
pub(crate) fn item_category_name(value: u16) -> &'static str {
    match value {
        0x00 => "books/misc",
        0x01 => "amulets",
        0x02 => "armor",
        0x03 => "belts",
        0x04 => "boots",
        0x05 => "arrows",
        0x06 => "bracers/gauntlets",
        0x07 => "headgear",
        0x08 => "keys",
        0x09 => "potions",
        0x0a => "rings",
        0x0b => "scrolls",
        0x0c => "shields",
        0x0d => "food",
        0x0e => "bullets",
        0x0f => "bows",
        0x10 => "daggers",
        0x11 => "maces",
        0x12 => "slings",
        0x13 => "small swords",
        0x14 => "large swords",
        0x15 => "hammers",
        0x16 => "morning stars",
        0x17 => "flails",
        0x18 => "darts",
        0x19 => "axes",
        0x1a => "quarterstaff",
        0x1b => "crossbow",
        0x1c => "hand-to-hand",
        0x1d => "spears",
        0x1e => "halberds",
        0x1f => "crossbow bolts",
        0x20 => "cloaks/robes",
        0x21 => "gold",
        0x22 => "gems",
        0x23 => "wands",
        0x24 => "containers",
        0x25 => "books/bracelets",
        0x26 => "familiars/earrings",
        0x3a => "container",
        0x3b => "fur/pelt",
        0x3c => "leather armor",
        0x3d => "studded leather",
        0x3e => "chain mail",
        0x3f => "splint mail",
        0x40 => "half plate",
        0x41 => "full plate",
        0x42 => "hide armor",
        0x43 => "robe",
        0x45 => "bastard sword",
        0x47 => "food",
        0x48 => "hat",
        0x49 => "gauntlet",
        _ => "?",
    }
}
