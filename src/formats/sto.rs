//! STO parser.
//!
//! A store: the items, drinks, cures, and inn rooms a shop/tavern/inn/temple
//! offers, plus which item categories it will buy back. Like [ITM](super::itm) /
//! [SPL](super::spl) it is a fixed header followed by header-located arrays —
//! here four of them, each with its own absolute offset and count in the header:
//!
//! - items for sale (28 bytes each): an ITM resref plus stock bookkeeping.
//! - drinks (20 bytes each): a tavern rumour, a name strref, price, strength.
//! - cures (12 bytes each): an SPL resref plus its price.
//! - items purchased: a flat array of item-category dwords.
//!
//! Only `V1.0` (BG1/BG2/EE) is handled; `V1.1` (PST) and `V9.0` (IWD) are
//! rejected. All offsets are absolute from the file start.
//! Spec: `md/file_formats/ie_formats/sto_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::dlg::DLG_TYPE;
use crate::formats::fields::{set_flags, write_bitfield};
use crate::formats::item_category_name;
use crate::formats::itm::ITM_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag};

/// Resource type code for STO files.
pub const STO_TYPE: u16 = 0x03f6;

const STO_SIGNATURE: &[u8; 4] = b"STOR";
const STO_VERSION: &[u8; 4] = b"V1.0";

/// On-disk size of one record in each variable-length section.
const SALE_SIZE: usize = 0x1c;
const DRINK_SIZE: usize = 0x14;
const CURE_SIZE: usize = 0x0c;

/// Anything that can go wrong while parsing a STO file.
#[derive(Debug)]
pub enum StoError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for StoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoError::Truncated(e) => write!(f, "{e}"),
            StoError::BadSignature(found) => {
                write!(f, "not a STO file: signature was {}", describe_tag(found))
            }
            StoError::BadVersion(found) => {
                write!(f, "unsupported STO version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for StoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for StoError {
    fn from(e: Truncated) -> Self {
        StoError::Truncated(e)
    }
}

/// The 156-byte STO V1.0 header. Documented fields are kept verbatim; the
/// structural offset/count words are read locally in [`Sto::parse`] and not
/// stored, since the section counts are implicit in the parsed vectors.
struct StoHeader {
    store_type: u32,
    name: u32,
    flags: u32,
    sell_markup: u32,
    buy_markup: u32,
    depreciation: u32,
    steal_failure: u16,
    capacity: u16,
    lore: u32,
    id_price: u32,
    rumours_tavern: String,
    rumours_temple: String,
    room_flags: u32,
    room_price_peasant: u32,
    room_price_merchant: u32,
    room_price_noble: u32,
    room_price_royal: u32,
}

impl StoHeader {
    fn parse(reader: &Reader) -> Result<StoHeader, Truncated> {
        Ok(StoHeader {
            store_type: reader.u32_at(0x08, "store type")?,
            name: reader.u32_at(0x0c, "name")?,
            flags: reader.u32_at(0x10, "flags")?,
            sell_markup: reader.u32_at(0x14, "sell markup")?,
            buy_markup: reader.u32_at(0x18, "buy markup")?,
            depreciation: reader.u32_at(0x1c, "depreciation")?,
            steal_failure: reader.u16_at(0x20, "steal failure")?,
            capacity: reader.u16_at(0x22, "capacity")?,
            lore: reader.u32_at(0x3c, "lore")?,
            id_price: reader.u32_at(0x40, "id price")?,
            rumours_tavern: reader.resref_at(0x44, "tavern rumours")?,
            rumours_temple: reader.resref_at(0x54, "temple rumours")?,
            room_flags: reader.u32_at(0x5c, "room flags")?,
            room_price_peasant: reader.u32_at(0x60, "peasant room price")?,
            room_price_merchant: reader.u32_at(0x64, "merchant room price")?,
            room_price_noble: reader.u32_at(0x68, "noble room price")?,
            room_price_royal: reader.u32_at(0x6c, "royal room price")?,
        })
    }
}

/// One item the store offers for sale (28 bytes).
pub struct SaleItem {
    resource: String,
    expiration: u16,
    quantity: [u16; 3],
    flags: u32,
    amount: u32,
    infinite: u32,
    /// Item display name, resolved by the manager from the ITM header.
    name: Option<String>,
}

impl SaleItem {
    fn parse_at(reader: &Reader, base: usize) -> Result<SaleItem, Truncated> {
        Ok(SaleItem {
            resource: reader.resref_at(base, "sale item")?,
            expiration: reader.u16_at(base + 0x08, "sale expiration")?,
            quantity: [
                reader.u16_at(base + 0x0a, "sale quantity 1")?,
                reader.u16_at(base + 0x0c, "sale quantity 2")?,
                reader.u16_at(base + 0x0e, "sale quantity 3")?,
            ],
            flags: reader.u32_at(base + 0x10, "sale flags")?,
            amount: reader.u32_at(base + 0x14, "sale amount")?,
            infinite: reader.u32_at(base + 0x18, "sale infinite")?,
            name: None,
        })
    }

    /// The ITM resref, for the manager to resolve a display name.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Attach the resolved ITM display name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

/// One drink a tavern offers (20 bytes).
pub struct Drink {
    rumour: String,
    name: u32,
    price: u32,
    strength: u32,
    /// Drink name, resolved by the manager from the TLK.
    name_text: Option<String>,
}

impl Drink {
    fn parse_at(reader: &Reader, base: usize) -> Result<Drink, Truncated> {
        Ok(Drink {
            rumour: reader.resref_at(base, "drink rumour")?,
            name: reader.u32_at(base + 0x08, "drink name")?,
            price: reader.u32_at(base + 0x0c, "drink price")?,
            strength: reader.u32_at(base + 0x10, "drink strength")?,
            name_text: None,
        })
    }

    /// The drink's name strref, for the manager to resolve against the TLK.
    pub fn name_strref(&self) -> u32 {
        self.name
    }

    /// Attach the resolved drink name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name_text = name;
    }
}

/// One cure a temple offers (12 bytes).
pub struct Cure {
    resource: String,
    price: u32,
    /// Spell display name, resolved by the manager from the SPL header.
    name: Option<String>,
}

impl Cure {
    fn parse_at(reader: &Reader, base: usize) -> Result<Cure, Truncated> {
        Ok(Cure {
            resource: reader.resref_at(base, "cure spell")?,
            price: reader.u32_at(base + 0x08, "cure price")?,
            name: None,
        })
    }

    /// The SPL resref, for the manager to resolve a display name.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Attach the resolved SPL display name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

/// A parsed STO file: the header plus its four sections, with names the manager
/// has resolved attached.
pub struct Sto {
    header: StoHeader,
    sale: Vec<SaleItem>,
    drinks: Vec<Drink>,
    cures: Vec<Cure>,
    purchased: Vec<u32>,
    name_text: Option<String>,
}

impl Sto {
    /// Parse a STO V1.0 resource. The four sections are read from their
    /// header-declared offsets and counts.
    pub fn parse(data: &[u8]) -> Result<Sto, StoError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != STO_SIGNATURE {
            return Err(StoError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != STO_VERSION {
            return Err(StoError::BadVersion(version));
        }

        let header = StoHeader::parse(&reader)?;

        let purchased = Self::read_section(&reader, 0x2c, 0x30, "purchased")?
            .map(|(offset, count)| {
                (0..count)
                    .map(|i| reader.u32_at(offset + i * 4, "purchased category"))
                    .collect::<Result<Vec<u32>, Truncated>>()
            })
            .transpose()?
            .unwrap_or_default();

        let sale = Self::read_records(&reader, 0x34, 0x38, SALE_SIZE, SaleItem::parse_at)?;
        let drinks = Self::read_records(&reader, 0x4c, 0x50, DRINK_SIZE, Drink::parse_at)?;
        let cures = Self::read_records(&reader, 0x70, 0x74, CURE_SIZE, Cure::parse_at)?;

        Ok(Sto {
            header,
            sale,
            drinks,
            cures,
            purchased,
            name_text: None,
        })
    }

    /// Read a section's (offset, count) header pair, or `None` when it is empty.
    fn read_section(
        reader: &Reader,
        offset_at: usize,
        count_at: usize,
        context: &'static str,
    ) -> Result<Option<(usize, usize)>, Truncated> {
        let count = reader.u32_at(count_at, context)? as usize;
        if count == 0 {
            return Ok(None);
        }
        let offset = reader.u32_at(offset_at, context)? as usize;
        Ok(Some((offset, count)))
    }

    /// Read a fixed-size record array from a header-declared section.
    fn read_records<T>(
        reader: &Reader,
        offset_at: usize,
        count_at: usize,
        record_size: usize,
        parse_at: impl Fn(&Reader, usize) -> Result<T, Truncated>,
    ) -> Result<Vec<T>, Truncated> {
        let Some((offset, count)) = Self::read_section(reader, offset_at, count_at, "section")?
        else {
            return Ok(Vec::new());
        };
        let mut records = Vec::with_capacity(count);
        for i in 0..count {
            records.push(parse_at(reader, offset + i * record_size)?);
        }
        Ok(records)
    }

    /// The store's name strref, for the manager to resolve against the TLK.
    pub fn name_strref(&self) -> u32 {
        self.header.name
    }

    /// Every TLK strref this store references, tagged with its field: the store
    /// name, then each drink's name.
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        let mut uses = vec![StrrefUse::new(self.header.name, "name")];
        for (i, drink) in self.drinks.iter().enumerate() {
            uses.push(StrrefUse::new(
                drink.name_strref(),
                format!("drink[{i}].name"),
            ));
        }
        uses
    }

    /// Every resource this store names, tagged with the type it names: the two
    /// rumour dialogues, the ITM of each item for sale, the DLG of each drink's
    /// rumour, and the SPL of each cure. The "items purchased" section holds
    /// item *categories*, not resrefs, so it contributes nothing.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let h = &self.header;
        let mut uses = vec![
            ResrefUse::new(DLG_TYPE, &h.rumours_tavern, "rumours_tavern"),
            ResrefUse::new(DLG_TYPE, &h.rumours_temple, "rumours_temple"),
        ];
        for (i, item) in self.sale.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &item.resource,
                format!("sale[{i}]"),
            ));
        }
        for (i, drink) in self.drinks.iter().enumerate() {
            uses.push(ResrefUse::new(
                DLG_TYPE,
                &drink.rumour,
                format!("drink[{i}].rumour"),
            ));
        }
        for (i, cure) in self.cures.iter().enumerate() {
            uses.push(ResrefUse::new(
                SPL_TYPE,
                &cure.resource,
                format!("cure[{i}]"),
            ));
        }
        uses.retain(|u| !u.value.is_empty());
        uses
    }

    /// Attach the resolved store name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name_text = name;
    }

    /// The items offered for sale, for the manager to resolve their ITM names.
    pub fn sale_mut(&mut self) -> &mut [SaleItem] {
        &mut self.sale
    }

    /// The cures offered, for the manager to resolve their SPL names.
    pub fn cures_mut(&mut self) -> &mut [Cure] {
        &mut self.cures
    }

    /// The drinks offered, for the manager to resolve their TLK names.
    pub fn drinks_mut(&mut self) -> &mut [Drink] {
        &mut self.drinks
    }

    /// Write the whole store as flat `key=value` lines: header fields bare, the
    /// sections under `sale.<n>.` / `drink.<n>.` / `cure.<n>.` / `buy.<n>`.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "store_type={}", h.store_type)?;
        writeln!(out, "name={}", h.name)?;
        writeln!(out, "flags={}", h.flags)?;
        writeln!(out, "sell_markup={}", h.sell_markup)?;
        writeln!(out, "buy_markup={}", h.buy_markup)?;
        writeln!(out, "depreciation={}", h.depreciation)?;
        writeln!(out, "steal_failure={}", h.steal_failure)?;
        writeln!(out, "capacity={}", h.capacity)?;
        writeln!(out, "lore={}", h.lore)?;
        writeln!(out, "id_price={}", h.id_price)?;
        writeln!(out, "rumours_tavern={}", h.rumours_tavern)?;
        writeln!(out, "rumours_temple={}", h.rumours_temple)?;
        writeln!(out, "room_flags={}", h.room_flags)?;
        writeln!(out, "room_price_peasant={}", h.room_price_peasant)?;
        writeln!(out, "room_price_merchant={}", h.room_price_merchant)?;
        writeln!(out, "room_price_noble={}", h.room_price_noble)?;
        writeln!(out, "room_price_royal={}", h.room_price_royal)?;

        for (i, s) in self.sale.iter().enumerate() {
            writeln!(out, "sale.{i}.resource={}", s.resource)?;
            writeln!(out, "sale.{i}.expiration={}", s.expiration)?;
            for (j, q) in s.quantity.iter().enumerate() {
                writeln!(out, "sale.{i}.quantity_{}={q}", j + 1)?;
            }
            writeln!(out, "sale.{i}.flags={}", s.flags)?;
            writeln!(out, "sale.{i}.amount={}", s.amount)?;
            writeln!(out, "sale.{i}.infinite={}", s.infinite)?;
        }

        for (i, d) in self.drinks.iter().enumerate() {
            writeln!(out, "drink.{i}.rumour={}", d.rumour)?;
            writeln!(out, "drink.{i}.name={}", d.name)?;
            writeln!(out, "drink.{i}.price={}", d.price)?;
            writeln!(out, "drink.{i}.strength={}", d.strength)?;
        }

        for (i, c) in self.cures.iter().enumerate() {
            writeln!(out, "cure.{i}.resource={}", c.resource)?;
            writeln!(out, "cure.{i}.price={}", c.price)?;
        }

        for (i, category) in self.purchased.iter().enumerate() {
            writeln!(out, "buy.{i}={category}")?;
        }

        Ok(())
    }
}

impl fmt::Display for Sto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "STO V1.0")?;
        writeln!(
            f,
            "  type:        {} ({})",
            h.store_type,
            store_type_name(h.store_type)
        )?;
        match &self.name_text {
            Some(name) => writeln!(f, "  name:        {} ({name:?})", h.name)?,
            None => writeln!(f, "  name:        {}", h.name)?,
        }
        if h.flags != 0 {
            write_bitfield(f, "  flags:       ", h.flags, &store_flags(h.flags))?;
        }
        writeln!(f, "  sell markup: {}%", h.sell_markup)?;
        writeln!(f, "  buy markup:  {}%", h.buy_markup)?;
        if h.depreciation != 0 {
            writeln!(f, "  depreciate:  {}", h.depreciation)?;
        }
        if h.steal_failure != 0 {
            writeln!(f, "  steal fail:  {}%", h.steal_failure)?;
        }
        writeln!(
            f,
            "  capacity:    {}",
            if h.capacity == 0 {
                "unlimited".to_string()
            } else {
                h.capacity.to_string()
            }
        )?;
        if h.lore != 0 {
            writeln!(f, "  lore:        {}", h.lore)?;
        }
        if h.id_price != 0 {
            writeln!(f, "  id price:    {}", h.id_price)?;
        }
        if !h.rumours_tavern.is_empty() {
            writeln!(f, "  rumours(T):  {:?}", h.rumours_tavern)?;
        }
        if !h.rumours_temple.is_empty() {
            writeln!(f, "  rumours(t):  {:?}", h.rumours_temple)?;
        }
        if h.room_flags != 0 {
            write_bitfield(
                f,
                "  rooms:       ",
                h.room_flags,
                &room_flags(h.room_flags),
            )?;
            writeln!(f, "    peasant:   {}", h.room_price_peasant)?;
            writeln!(f, "    merchant:  {}", h.room_price_merchant)?;
            writeln!(f, "    noble:     {}", h.room_price_noble)?;
            writeln!(f, "    royal:     {}", h.room_price_royal)?;
        }

        if !self.sale.is_empty() {
            writeln!(f, "\n  sale: ({})", self.sale.len())?;
            for s in &self.sale {
                s.write_human(f)?;
            }
        }

        if !self.drinks.is_empty() {
            writeln!(f, "\n  drinks: ({})", self.drinks.len())?;
            for d in &self.drinks {
                d.write_human(f)?;
            }
        }

        if !self.cures.is_empty() {
            writeln!(f, "\n  cures: ({})", self.cures.len())?;
            for c in &self.cures {
                c.write_human(f)?;
            }
        }

        if !self.purchased.is_empty() {
            writeln!(f, "\n  buys: ({})", self.purchased.len())?;
            for category in &self.purchased {
                writeln!(
                    f,
                    "    {} ({})",
                    category,
                    item_category_name(*category as u16)
                )?;
            }
        }

        Ok(())
    }
}

impl SaleItem {
    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "    {}", self.resource)?;
        if let Some(name) = &self.name {
            write!(f, " ({name})")?;
        }
        // Quantity/charges; for stackables the first word is the stack size.
        let charges: Vec<String> = self
            .quantity
            .iter()
            .filter(|q| **q != 0)
            .map(|q| q.to_string())
            .collect();
        if !charges.is_empty() {
            write!(f, " charges {}", charges.join("/"))?;
        }
        if self.infinite != 0 {
            write!(f, " (infinite)")?;
        } else if self.amount != 0 {
            write!(f, " x{}", self.amount)?;
        }
        let flags = sale_flags(self.flags);
        if !flags.is_empty() {
            write!(f, " [{}]", flags.join(", "))?;
        }
        writeln!(f)
    }
}

impl Drink {
    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name_text {
            Some(name) => write!(f, "    {} ({name:?})", self.name)?,
            // -1 is the "no name" sentinel some placeholder drinks carry.
            None if self.name == u32::MAX => write!(f, "    (unnamed)")?,
            None => write!(f, "    {}", self.name)?,
        }
        write!(f, " {}gp", self.price)?;
        if self.strength != 0 {
            write!(f, " str {}", self.strength)?;
        }
        writeln!(f)
    }
}

impl Cure {
    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "    {}", self.resource)?;
        if let Some(name) = &self.name {
            write!(f, " ({name})")?;
        }
        writeln!(f, " {}gp", self.price)
    }
}

/// Name the store type (header 0x08).
fn store_type_name(value: u32) -> &'static str {
    match value {
        0 => "store",
        1 => "tavern",
        2 => "inn",
        3 => "temple",
        5 => "container",
        _ => "?",
    }
}

/// Store flags (header 0x10).
fn store_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "can buy"),
            (1, "can sell"),
            (2, "can identify"),
            (3, "can steal"),
            (4, "can donate"),
            (5, "can buy cures"),
            (6, "can buy drinks"),
            (9, "quality bit 1"),
            (10, "quality bit 2"),
            (12, "buy fenced goods"),
            (13, "ignore reputation"),
            (14, "toggle item recharge"),
            (15, "can sell critical items"),
        ],
    )
}

/// Inn room flags (header 0x5c).
fn room_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[(0, "peasant"), (1, "merchant"), (2, "noble"), (3, "royal")],
    )
}

/// Sale-item flags (sale 0x10).
fn sale_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "identified"),
            (1, "unstealable"),
            (2, "stolen"),
            (3, "undroppable"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a STO V1.0 file: a 156-byte header followed by the four sections
    /// in fixed order (purchased, sale, drinks, cures), with the header offsets
    /// and counts wired to match.
    fn build_sto(
        store_type: u32,
        purchased: &[u32],
        sale: usize,
        drinks: usize,
        cures: usize,
    ) -> Vec<u8> {
        let header_len = 0x9c;
        let buy_off = header_len;
        let sale_off = buy_off + purchased.len() * 4;
        let drink_off = sale_off + sale * SALE_SIZE;
        let cure_off = drink_off + drinks * DRINK_SIZE;
        let total = cure_off + cures * CURE_SIZE;

        let mut data = vec![0u8; total];
        data[0x00..0x04].copy_from_slice(STO_SIGNATURE);
        data[0x04..0x08].copy_from_slice(STO_VERSION);
        data[0x08..0x0c].copy_from_slice(&store_type.to_le_bytes());

        let put = |data: &mut [u8], at: usize, value: u32| {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(&mut data, 0x2c, buy_off as u32);
        put(&mut data, 0x30, purchased.len() as u32);
        put(&mut data, 0x34, sale_off as u32);
        put(&mut data, 0x38, sale as u32);
        put(&mut data, 0x4c, drink_off as u32);
        put(&mut data, 0x50, drinks as u32);
        put(&mut data, 0x70, cure_off as u32);
        put(&mut data, 0x74, cures as u32);

        for (i, category) in purchased.iter().enumerate() {
            put(&mut data, buy_off + i * 4, *category);
        }
        data
    }

    #[test]
    fn empty_store_has_no_sections() {
        let sto = Sto::parse(&build_sto(0, &[], 0, 0, 0)).expect("parse");
        assert!(sto.sale.is_empty());
        assert!(sto.drinks.is_empty());
        assert!(sto.cures.is_empty());
        assert!(sto.purchased.is_empty());
    }

    #[test]
    fn reads_all_sections() {
        let sto = Sto::parse(&build_sto(2, &[2, 12], 3, 1, 2)).expect("parse");
        assert_eq!(sto.purchased, vec![2, 12]);
        assert_eq!(sto.sale.len(), 3);
        assert_eq!(sto.drinks.len(), 1);
        assert_eq!(sto.cures.len(), 2);
        assert_eq!(sto.header.store_type, 2);
    }

    #[test]
    fn resref_uses_separates_sale_items_from_drinks_and_cures() {
        let mut data = build_sto(0, &[], 2, 1, 1);
        data[0x44..0x48].copy_from_slice(b"TAVR"); // tavern rumours (DLG)
        let sale_off = 0x9c;
        data[sale_off..sale_off + 4].copy_from_slice(b"SW1H");
        data[sale_off + SALE_SIZE..sale_off + SALE_SIZE + 4].copy_from_slice(b"BOW1");
        let drink_off = sale_off + 2 * SALE_SIZE;
        data[drink_off..drink_off + 4].copy_from_slice(b"RUMR");
        let cure_off = drink_off + DRINK_SIZE;
        data[cure_off..cure_off + 4].copy_from_slice(b"CURE");

        let sto = Sto::parse(&data).expect("parse");
        let uses = sto.resref_uses();
        let got: Vec<(u16, &str, &str)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (DLG_TYPE, "TAVR", "rumours_tavern"),
                (ITM_TYPE, "SW1H", "sale[0]"),
                (ITM_TYPE, "BOW1", "sale[1]"),
                (DLG_TYPE, "RUMR", "drink[0].rumour"),
                (SPL_TYPE, "CURE", "cure[0]"),
            ]
        );
    }

    #[test]
    fn strref_uses_covers_store_and_drink_names() {
        // Header only precedes the drinks (no purchased/sale), so they start at
        // the 0x9c header length.
        let mut data = build_sto(0, &[], 0, 2, 0);
        let put = |data: &mut [u8], at: usize, value: u32| {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(&mut data, 0x0c, 100); // store name
        put(&mut data, 0x9c + 0x08, 201); // drink[0].name
        put(&mut data, 0x9c + DRINK_SIZE + 0x08, 202); // drink[1].name

        let uses = Sto::parse(&data).expect("parse").strref_uses();
        let got: Vec<(u32, &str)> = uses.iter().map(|u| (u.value, u.path.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (100, "name"),
                (201, "drink[0].name"),
                (202, "drink[1].name"),
            ]
        );
    }

    #[test]
    fn rejects_bad_signature() {
        let mut data = build_sto(0, &[], 0, 0, 0);
        data[0] = b'X';
        assert!(matches!(Sto::parse(&data), Err(StoError::BadSignature(_))));
    }

    #[test]
    fn rejects_bad_version() {
        let mut data = build_sto(0, &[], 0, 0, 0);
        data[0x04..0x08].copy_from_slice(b"V9.0");
        assert!(matches!(Sto::parse(&data), Err(StoError::BadVersion(_))));
    }

    #[test]
    fn rejects_truncated_section() {
        // Claims one sale item but the file ends at the header.
        let mut data = build_sto(0, &[], 0, 0, 0);
        let put = |data: &mut [u8], at: usize, value: u32| {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(&mut data, 0x34, 0x9c); // sale offset past end
        put(&mut data, 0x38, 1); // sale count
        assert!(matches!(Sto::parse(&data), Err(StoError::Truncated(_))));
    }
}
