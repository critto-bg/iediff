//! SPL parser.
//!
//! A spell: mage/priest spells, innate abilities, bard songs, and engine helper
//! "spells". Structurally a sibling of [ITM](super::itm) — a 114-byte header
//! plus two header-located arrays: extended headers (40 bytes each, here the
//! caster-level variants of the spell) and a continuous pool of [`Feature`]
//! blocks (embedded EFF V1 effects, 48 bytes each). The header references a
//! slice of that pool for the spell's *casting* effects; each extended header
//! references its own slice. All offsets are absolute from the file start.
//! Spec: `md/file_formats/ie_formats/spl_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::eff::Feature;
use crate::formats::fields::{set_flags, write_bitfield};
use crate::{Reader, StrrefUse, Truncated, describe_tag, parse_asciiz};

/// Resource type code for SPL files.
pub const SPL_TYPE: u16 = 0x03ee;

/// Size of one extended header on disk.
const EXT_HEADER_SIZE: usize = 40;

const SPL_SIGNATURE: &[u8; 4] = b"SPL ";
const SPL_VERSION: &[u8; 4] = b"V1  ";

/// Anything that can go wrong while parsing an SPL file.
#[derive(Debug)]
pub enum SplError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for SplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplError::Truncated(e) => write!(f, "{e}"),
            SplError::BadSignature(found) => {
                write!(f, "not an SPL file: signature was {}", describe_tag(found))
            }
            SplError::BadVersion(found) => {
                write!(f, "unsupported SPL version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for SplError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SplError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for SplError {
    fn from(e: Truncated) -> Self {
        SplError::Truncated(e)
    }
}

/// The 114-byte SPL header. All documented fields are retained verbatim,
/// including the ones the engine leaves unused (leftovers of the shared ITM
/// layout); the structural offset/count words are read but not stored.
struct SplHeader {
    version: String,
    unidentified_name: u32,
    identified_name: u32,
    completion_sound: String,
    flags: u32,
    spell_type: u16,
    exclusion_flags: u32,
    casting_graphics: u16,
    min_level: u8,
    primary_type: u8,
    min_strength: u8,
    secondary_type: u8,
    min_strength_bonus: u8,
    usability_1: u8,
    min_intelligence: u8,
    usability_2: u8,
    min_dexterity: u8,
    usability_3: u8,
    min_wisdom: u8,
    usability_4: u8,
    min_constitution: u16,
    min_charisma: u16,
    spell_level: u32,
    stack_amount: u16,
    spellbook_icon: String,
    lore_to_id: u16,
    ground_icon: String,
    weight: u32,
    unidentified_description: u32,
    identified_description: u32,
    description_icon: String,
    enchantment: u32,
}

impl SplHeader {
    fn parse(reader: &Reader) -> Result<SplHeader, Truncated> {
        Ok(SplHeader {
            version: parse_asciiz(&reader.tag_at(0x04, "version")?),
            unidentified_name: reader.u32_at(0x08, "unidentified name")?,
            identified_name: reader.u32_at(0x0c, "identified name")?,
            completion_sound: reader.resref_at(0x10, "completion sound")?,
            flags: reader.u32_at(0x18, "flags")?,
            spell_type: reader.u16_at(0x1c, "spell type")?,
            exclusion_flags: reader.u32_at(0x1e, "exclusion flags")?,
            casting_graphics: reader.u16_at(0x22, "casting graphics")?,
            min_level: reader.u8_at(0x24, "min level")?,
            primary_type: reader.u8_at(0x25, "primary type")?,
            min_strength: reader.u8_at(0x26, "min strength")?,
            secondary_type: reader.u8_at(0x27, "secondary type")?,
            min_strength_bonus: reader.u8_at(0x28, "min strength bonus")?,
            usability_1: reader.u8_at(0x29, "usability 1")?,
            min_intelligence: reader.u8_at(0x2a, "min intelligence")?,
            usability_2: reader.u8_at(0x2b, "usability 2")?,
            min_dexterity: reader.u8_at(0x2c, "min dexterity")?,
            usability_3: reader.u8_at(0x2d, "usability 3")?,
            min_wisdom: reader.u8_at(0x2e, "min wisdom")?,
            usability_4: reader.u8_at(0x2f, "usability 4")?,
            min_constitution: reader.u16_at(0x30, "min constitution")?,
            min_charisma: reader.u16_at(0x32, "min charisma")?,
            spell_level: reader.u32_at(0x34, "spell level")?,
            stack_amount: reader.u16_at(0x38, "stack amount")?,
            spellbook_icon: reader.resref_at(0x3a, "spellbook icon")?,
            lore_to_id: reader.u16_at(0x42, "lore to id")?,
            ground_icon: reader.resref_at(0x44, "ground icon")?,
            weight: reader.u32_at(0x4c, "weight")?,
            unidentified_description: reader.u32_at(0x50, "unidentified description")?,
            identified_description: reader.u32_at(0x54, "identified description")?,
            description_icon: reader.resref_at(0x58, "description icon")?,
            enchantment: reader.u32_at(0x60, "enchantment")?,
        })
    }
}

/// One extended header — a caster-level variant of the spell — plus its effects.
pub struct SplExtHeader {
    spell_form: u8,
    friendly: u8,
    location: u16,
    memorised_icon: String,
    target_type: u8,
    target_count: u8,
    range: u16,
    level_required: u16,
    casting_time: u16,
    times_per_day: u16,
    dice_sides: u16,
    dice_thrown: u16,
    enchanted: u16,
    damage_type: u16,
    charges: u16,
    charge_depletion: u16,
    projectile: u16,
    /// Effects this header applies (its slice of the shared feature pool).
    features: Vec<Feature>,
    /// Projectile name, resolved by the manager from `projectl.ids`.
    projectile_name: Option<String>,
}

impl SplExtHeader {
    /// Parse the 40-byte extended header at `base`. Its effects are read
    /// separately by [`Spl::parse`], which knows the feature-pool offset.
    fn parse_at(reader: &Reader, base: usize) -> Result<SplExtHeader, Truncated> {
        Ok(SplExtHeader {
            spell_form: reader.u8_at(base, "spell form")?,
            friendly: reader.u8_at(base + 0x01, "friendly flag")?,
            location: reader.u16_at(base + 0x02, "location")?,
            memorised_icon: reader.resref_at(base + 0x04, "memorised icon")?,
            target_type: reader.u8_at(base + 0x0c, "target type")?,
            target_count: reader.u8_at(base + 0x0d, "target count")?,
            range: reader.u16_at(base + 0x0e, "range")?,
            level_required: reader.u16_at(base + 0x10, "level required")?,
            casting_time: reader.u16_at(base + 0x12, "casting time")?,
            times_per_day: reader.u16_at(base + 0x14, "times per day")?,
            dice_sides: reader.u16_at(base + 0x16, "dice sides")?,
            dice_thrown: reader.u16_at(base + 0x18, "dice thrown")?,
            enchanted: reader.u16_at(base + 0x1a, "enchanted")?,
            damage_type: reader.u16_at(base + 0x1c, "damage type")?,
            charges: reader.u16_at(base + 0x22, "charges")?,
            charge_depletion: reader.u16_at(base + 0x24, "charge depletion")?,
            projectile: reader.u16_at(base + 0x26, "projectile")?,
            features: Vec::new(),
            projectile_name: None,
        })
    }

    /// The projectile number, for the manager to resolve against `projectl.ids`.
    pub fn projectile(&self) -> u16 {
        self.projectile
    }

    /// Attach the projectile name resolved from `projectl.ids`.
    pub fn set_projectile_name(&mut self, name: Option<String>) {
        self.projectile_name = name;
    }

    /// Render the header and its effects in the human-readable view. `index` is
    /// the header's position, used only for the heading.
    fn write_human(&self, f: &mut fmt::Formatter<'_>, index: usize) -> fmt::Result {
        writeln!(f, "  header {index}: level {}+", self.level_required)?;
        let d = "    ";
        writeln!(
            f,
            "{d}{:<11} {} ({})",
            "target:",
            self.target_type,
            spell_target_name(self.target_type)
        )?;
        writeln!(
            f,
            "{d}{:<11} {} ({})",
            "location:",
            self.location,
            location_name(self.location)
        )?;
        if self.range != 0 {
            writeln!(f, "{d}{:<11} {}", "range:", self.range)?;
        }
        if self.casting_time != 0 {
            writeln!(f, "{d}{:<11} {} (speed)", "cast:", self.casting_time)?;
        }
        if self.times_per_day != 0 {
            writeln!(f, "{d}{:<11} {}", "per day:", self.times_per_day)?;
        }
        if self.projectile != 0 {
            match &self.projectile_name {
                Some(name) => writeln!(f, "{d}{:<11} {} ({name})", "projectile:", self.projectile)?,
                None => writeln!(f, "{d}{:<11} {}", "projectile:", self.projectile)?,
            }
        }
        writeln!(f, "{d}{:<11} {}", "effects:", self.features.len())?;
        for feat in &self.features {
            feat.write_human(f, "      ")?;
        }
        Ok(())
    }

    /// Write the header's fields and effects as `key=value` lines under `prefix`
    /// (e.g. `"ext.0."`). Structural offset/count words are omitted — the effect
    /// count is implicit in the `effect.<n>` keys.
    fn write_raw(&self, out: &mut dyn Write, prefix: &str) -> io::Result<()> {
        writeln!(out, "{prefix}spell_form={}", self.spell_form)?;
        writeln!(out, "{prefix}friendly={}", self.friendly)?;
        writeln!(out, "{prefix}location={}", self.location)?;
        writeln!(out, "{prefix}memorised_icon={}", self.memorised_icon)?;
        writeln!(out, "{prefix}target_type={}", self.target_type)?;
        writeln!(out, "{prefix}target_count={}", self.target_count)?;
        writeln!(out, "{prefix}range={}", self.range)?;
        writeln!(out, "{prefix}level_required={}", self.level_required)?;
        writeln!(out, "{prefix}casting_time={}", self.casting_time)?;
        writeln!(out, "{prefix}times_per_day={}", self.times_per_day)?;
        writeln!(out, "{prefix}dice_sides={}", self.dice_sides)?;
        writeln!(out, "{prefix}dice_thrown={}", self.dice_thrown)?;
        writeln!(out, "{prefix}enchanted={}", self.enchanted)?;
        writeln!(out, "{prefix}damage_type={}", self.damage_type)?;
        writeln!(out, "{prefix}charges={}", self.charges)?;
        writeln!(out, "{prefix}charge_depletion={}", self.charge_depletion)?;
        writeln!(out, "{prefix}projectile={}", self.projectile)?;
        for (j, feat) in self.features.iter().enumerate() {
            feat.write_raw(out, &format!("{prefix}effect.{j}."))?;
        }
        Ok(())
    }
}

/// A parsed SPL file: the header, the on-cast effects, and the spell's
/// caster-level headers (each with its own effects).
pub struct Spl {
    header: SplHeader,
    casting: Vec<Feature>,
    ext_headers: Vec<SplExtHeader>,
    unidentified_name: Option<String>,
    identified_name: Option<String>,
    unidentified_description: Option<String>,
    identified_description: Option<String>,
    primary_name: Option<String>,
    secondary_name: Option<String>,
}

impl Spl {
    /// Parse an SPL V1 resource.
    pub fn parse(data: &[u8]) -> Result<Spl, SplError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != SPL_SIGNATURE {
            return Err(SplError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != SPL_VERSION {
            return Err(SplError::BadVersion(version));
        }

        let header = SplHeader::parse(&reader)?;

        // The header's structural words locate the two arrays below.
        let ext_offset = reader.u32_at(0x64, "ext header offset")? as usize;
        let ext_count = reader.u16_at(0x68, "ext header count")? as usize;
        let feature_offset = reader.u32_at(0x6a, "feature block offset")?;
        let casting_index = reader.u16_at(0x6e, "casting index")?;
        let casting_count = reader.u16_at(0x70, "casting count")?;

        // On-cast effects: a slice of the shared feature pool.
        let casting = Feature::read_pool(&reader, feature_offset, casting_index, casting_count)?;

        // Each extended header reads its own slice of the same pool.
        let mut ext_headers = Vec::with_capacity(ext_count);
        for i in 0..ext_count {
            let base = ext_offset + i * EXT_HEADER_SIZE;
            let mut ext = SplExtHeader::parse_at(&reader, base)?;
            let feature_index = reader.u16_at(base + 0x20, "feature index")?;
            let feature_count = reader.u16_at(base + 0x1e, "feature count")?;
            ext.features =
                Feature::read_pool(&reader, feature_offset, feature_index, feature_count)?;
            ext_headers.push(ext);
        }

        Ok(Spl {
            header,
            casting,
            ext_headers,
            unidentified_name: None,
            identified_name: None,
            unidentified_description: None,
            identified_description: None,
            primary_name: None,
            secondary_name: None,
        })
    }

    /// The header strrefs for the spell's unidentified and identified names.
    pub fn name_strrefs(&self) -> (u32, u32) {
        (self.header.unidentified_name, self.header.identified_name)
    }

    /// The header strrefs for the spell's unidentified and identified descriptions.
    pub fn description_strrefs(&self) -> (u32, u32) {
        (
            self.header.unidentified_description,
            self.header.identified_description,
        )
    }

    /// Every TLK strref this spell references, tagged with its field. The four
    /// header strings (names and descriptions); embedded-effect string opcodes
    /// are not yet decoded.
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        let h = &self.header;
        let mut uses = vec![
            StrrefUse::new(h.unidentified_name, "unidentified_name"),
            StrrefUse::new(h.identified_name, "identified_name"),
            StrrefUse::new(h.unidentified_description, "unidentified_description"),
            StrrefUse::new(h.identified_description, "identified_description"),
        ];
        // Effects can carry a strref (a "display string" opcode): the on-cast
        // effects, then each ability's own effects.
        for (i, feat) in self.casting.iter().enumerate() {
            let at = format!("casting[{i}]");
            uses.extend(feat.strref_uses().into_iter().map(|u| u.prefixed(&at)));
        }
        for (j, ext) in self.ext_headers.iter().enumerate() {
            for (k, feat) in ext.features.iter().enumerate() {
                let at = format!("ability[{j}].effect[{k}]");
                uses.extend(feat.strref_uses().into_iter().map(|u| u.prefixed(&at)));
            }
        }
        uses
    }

    /// The primary type (school), an index into MSCHOOL.2DA.
    pub fn primary_type(&self) -> u32 {
        self.header.primary_type as u32
    }

    /// The secondary type, an index into MSECTYPE.2DA.
    pub fn secondary_type(&self) -> u32 {
        self.header.secondary_type as u32
    }

    /// Attach the display names resolved from the header strrefs.
    pub fn set_names(&mut self, unidentified: Option<String>, identified: Option<String>) {
        self.unidentified_name = unidentified;
        self.identified_name = identified;
    }

    /// Attach the descriptions resolved from the header strrefs.
    pub fn set_descriptions(&mut self, unidentified: Option<String>, identified: Option<String>) {
        self.unidentified_description = unidentified;
        self.identified_description = identified;
    }

    /// Attach the symbolic names resolved for the primary/secondary types.
    pub fn set_type_names(&mut self, primary: Option<String>, secondary: Option<String>) {
        self.primary_name = primary;
        self.secondary_name = secondary;
    }

    /// The spell's headers, for the manager to resolve their projectile names.
    pub fn ext_headers_mut(&mut self) -> &mut [SplExtHeader] {
        &mut self.ext_headers
    }

    /// The spell's headers (read-only).
    pub fn ext_headers(&self) -> &[SplExtHeader] {
        &self.ext_headers
    }

    /// Write the whole spell as flat, self-locating `key=value` lines: header
    /// fields bare, on-cast effects under `cast.<n>.`, headers under `ext.<n>.`
    /// (and their effects under `ext.<n>.effect.<m>.`).
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "version={}", h.version)?;
        writeln!(out, "unidentified_name={}", h.unidentified_name)?;
        writeln!(out, "identified_name={}", h.identified_name)?;
        writeln!(out, "completion_sound={}", h.completion_sound)?;
        writeln!(out, "flags={}", h.flags)?;
        writeln!(out, "spell_type={}", h.spell_type)?;
        writeln!(out, "exclusion_flags={}", h.exclusion_flags)?;
        writeln!(out, "casting_graphics={}", h.casting_graphics)?;
        writeln!(out, "min_level={}", h.min_level)?;
        writeln!(out, "primary_type={}", h.primary_type)?;
        writeln!(out, "min_strength={}", h.min_strength)?;
        writeln!(out, "secondary_type={}", h.secondary_type)?;
        writeln!(out, "min_strength_bonus={}", h.min_strength_bonus)?;
        writeln!(out, "usability_1={}", h.usability_1)?;
        writeln!(out, "min_intelligence={}", h.min_intelligence)?;
        writeln!(out, "usability_2={}", h.usability_2)?;
        writeln!(out, "min_dexterity={}", h.min_dexterity)?;
        writeln!(out, "usability_3={}", h.usability_3)?;
        writeln!(out, "min_wisdom={}", h.min_wisdom)?;
        writeln!(out, "usability_4={}", h.usability_4)?;
        writeln!(out, "min_constitution={}", h.min_constitution)?;
        writeln!(out, "min_charisma={}", h.min_charisma)?;
        writeln!(out, "spell_level={}", h.spell_level)?;
        writeln!(out, "stack_amount={}", h.stack_amount)?;
        writeln!(out, "spellbook_icon={}", h.spellbook_icon)?;
        writeln!(out, "lore_to_id={}", h.lore_to_id)?;
        writeln!(out, "ground_icon={}", h.ground_icon)?;
        writeln!(out, "weight={}", h.weight)?;
        writeln!(
            out,
            "unidentified_description={}",
            h.unidentified_description
        )?;
        writeln!(out, "identified_description={}", h.identified_description)?;
        writeln!(out, "description_icon={}", h.description_icon)?;
        writeln!(out, "enchantment={}", h.enchantment)?;
        for (i, feat) in self.casting.iter().enumerate() {
            feat.write_raw(out, &format!("cast.{i}."))?;
        }
        for (i, ext) in self.ext_headers.iter().enumerate() {
            ext.write_raw(out, &format!("ext.{i}."))?;
        }
        Ok(())
    }
}

impl fmt::Display for Spl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "SPL V1")?;
        match (&self.identified_name, &self.unidentified_name) {
            (Some(id), Some(unid)) if unid != id => {
                writeln!(f, "  name:        {id}")?;
                writeln!(f, "  unid. name:  {unid}")?;
            }
            (Some(name), _) | (None, Some(name)) => writeln!(f, "  name:        {name}")?,
            (None, None) => writeln!(
                f,
                "  name:        unid strref {} / id strref {}",
                h.unidentified_name, h.identified_name
            )?,
        }
        writeln!(
            f,
            "  spell type:  {} ({})",
            h.spell_type,
            spell_type_name(h.spell_type)
        )?;
        writeln!(f, "  level:       {}", h.spell_level)?;
        write_header_school(f, "primary", h.primary_type, &self.primary_name)?;
        write_header_school(f, "secondary", h.secondary_type, &self.secondary_name)?;
        write_bitfield(f, "  flags:       ", h.flags, &header_flags(h.flags))?;
        write_bitfield(
            f,
            "  exclusion:   ",
            h.exclusion_flags,
            &exclusion_flags(h.exclusion_flags),
        )?;
        writeln!(
            f,
            "  casting gfx: {} ({})",
            h.casting_graphics,
            casting_graphics_name(h.casting_graphics)
        )?;
        if !h.completion_sound.is_empty() {
            writeln!(f, "  sound:       {:?}", h.completion_sound)?;
        }

        writeln!(f)?;
        writeln!(f, "  casting effects: {}", self.casting.len())?;
        for feat in &self.casting {
            feat.write_human(f, "    ")?;
        }

        writeln!(f)?;
        writeln!(f, "  headers: {}", self.ext_headers.len())?;
        for (i, ext) in self.ext_headers.iter().enumerate() {
            writeln!(f)?;
            ext.write_human(f, i)?;
        }

        // The description, if resolved, trails the structural data so the header
        // and effects stay immediately visible.
        if let Some(desc) = self
            .identified_description
            .as_ref()
            .or(self.unidentified_description.as_ref())
        {
            writeln!(f)?;
            writeln!(f, "  description:")?;
            for line in desc.lines() {
                writeln!(f, "    {line}")?;
            }
        }

        Ok(())
    }
}

/// Write a header-level school line, aligned with the 13-wide header labels and
/// omitting the parenthetical when the dictionary lookup left the name unset.
fn write_header_school(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    value: u8,
    name: &Option<String>,
) -> fmt::Result {
    let field = format!("{label}:");
    match name {
        Some(name) => writeln!(f, "  {field:<13}{value} ({name})"),
        None => writeln!(f, "  {field:<13}{value}"),
    }
}

/// Name a spell type (header 0x1c).
fn spell_type_name(value: u16) -> &'static str {
    match value {
        0 => "special",
        1 => "wizard",
        2 => "priest",
        3 => "psionic",
        4 => "innate",
        5 => "bard song",
        _ => "?",
    }
}

/// Name a spell's ability target (extended-header 0x0c).
fn spell_target_name(value: u8) -> &'static str {
    match value {
        0 => "invalid",
        1 => "living actor",
        2 => "inventory",
        3 => "dead actor",
        4 => "any point in range",
        5 => "caster",
        6 => "crash",
        7 => "caster (instant)",
        _ => "?",
    }
}

/// Name an ability location (extended-header 0x02).
fn location_name(value: u16) -> &'static str {
    match value {
        0 => "none",
        1 => "weapon",
        2 => "spell",
        3 => "item",
        4 => "innate",
        _ => "none",
    }
}

/// Names of the set SPL header flags (header 0x18). BG2:EE-relevant bits only;
/// these differ entirely from the ITM header flags.
fn header_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (9, "breaks sanctuary/invisibility"),
            (10, "hostile"),
            (11, "no LOS required"),
            (12, "allow spotting"),
            (13, "outdoors only"),
            (14, "ignore dead-magic/wild-surge"),
            (15, "ignore wild surge"),
            (16, "not in combat"),
            (24, "can target invisible"),
            (25, "castable when silenced"),
        ],
    )
}

/// Names of the set exclusion flags (header 0x1e): which classes/alignments
/// cannot learn or use the spell. Alignment and school bits are mutually
/// exclusive in practice.
fn exclusion_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "chaotic priests"),
            (1, "evil priests"),
            (2, "good priests"),
            (3, "ge-neutral priests"),
            (4, "lawful priests"),
            (5, "lc-neutral priests"),
            (6, "abjurers"),
            (7, "conjurers"),
            (8, "diviners"),
            (9, "enchanters"),
            (10, "illusionists"),
            (11, "invokers"),
            (12, "necromancers"),
            (13, "transmuters"),
            (14, "generalists (wild magic)"),
            (30, "cleric/paladin"),
            (31, "druid/ranger"),
        ],
    )
}

/// Name the casting-graphics glow (header 0x22). A value enum, not a bitfield:
/// the school glows (9-16), the spark variants (17-34), and the "no animation"
/// sentinels.
fn casting_graphics_name(value: u16) -> &'static str {
    match value {
        0 | 56461 | 65535 => "none",
        9 => "necromancy",
        10 => "alteration",
        11 => "enchantment",
        12 => "abjuration",
        13 => "illusion",
        14 => "conjuration",
        15 => "invocation",
        16 => "divination",
        17..=34 => "sparks",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal SPL V1 file: a full header with both array counts zeroed.
    fn build_spl() -> Vec<u8> {
        let mut data = vec![0u8; 0x74];
        data[0x00..0x04].copy_from_slice(SPL_SIGNATURE);
        data[0x04..0x08].copy_from_slice(SPL_VERSION);
        data
    }

    #[test]
    fn strref_uses_lists_the_four_header_strings() {
        let mut data = build_spl();
        for (at, value) in [(0x08, 10u32), (0x0c, 11), (0x50, 20), (0x54, 21)] {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let uses = Spl::parse(&data).expect("parse").strref_uses();
        let got: Vec<(u32, &str)> = uses.iter().map(|u| (u.value, u.path.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (10, "unidentified_name"),
                (11, "identified_name"),
                (20, "unidentified_description"),
                (21, "identified_description"),
            ]
        );
    }
}
