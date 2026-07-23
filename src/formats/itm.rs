//! ITM parser.
//!
//! An item: weapons, armour, rings, scrolls, potions. An ITM is a 114-byte
//! header followed by two variable arrays the header points at — extended
//! headers (the item's selectable "abilities", 56 bytes each) and a continuous
//! pool of [`Feature`] blocks (embedded EFF V1 effects, 48 bytes each). The
//! header references a slice of that pool for the item's always-on *equipping*
//! effects; each extended header references its own slice for the effects that
//! ability applies. All offsets are absolute from the start of the file.
//! Spec: `md/file_formats/ie_formats/itm_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::eff::Feature;
use crate::formats::fields::{set_bits, set_flags, write_bitfield, write_school};
use crate::formats::{BAM_TYPE, item_category_name};
use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag, parse_asciiz};

/// Resource type code for ITM files.
pub const ITM_TYPE: u16 = 0x03ed;

/// Size of one extended header on disk.
const EXT_HEADER_SIZE: usize = 56;

const ITM_SIGNATURE: &[u8; 4] = b"ITM ";
const ITM_VERSION: &[u8; 4] = b"V1  ";

/// Anything that can go wrong while parsing an ITM file.
#[derive(Debug)]
pub enum ItmError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for ItmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItmError::Truncated(e) => write!(f, "{e}"),
            ItmError::BadSignature(found) => {
                write!(f, "not an ITM file: signature was {}", describe_tag(found))
            }
            ItmError::BadVersion(found) => {
                write!(f, "unsupported ITM version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for ItmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ItmError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for ItmError {
    fn from(e: Truncated) -> Self {
        ItmError::Truncated(e)
    }
}

/// The 114-byte ITM header. All documented fields are retained verbatim; the
/// structural offset/count words (which only locate the arrays below) are read
/// during parsing but not stored.
struct ItmHeader {
    version: String,
    unidentified_name: u32,
    identified_name: u32,
    replacement_item: String,
    flags: u32,
    item_type: u16,
    usability: u32,
    animation: String,
    min_level: u16,
    min_strength: u16,
    min_strength_bonus: u8,
    kit_usability_1: u8,
    min_intelligence: u8,
    kit_usability_2: u8,
    min_dexterity: u8,
    kit_usability_3: u8,
    min_wisdom: u8,
    kit_usability_4: u8,
    min_constitution: u8,
    weapon_proficiency: u8,
    min_charisma: u16,
    price: u32,
    stack_amount: u16,
    inventory_icon: String,
    lore_to_id: u16,
    ground_icon: String,
    weight: u32,
    unidentified_description: u32,
    identified_description: u32,
    description_icon: String,
    enchantment: u32,
}

impl ItmHeader {
    fn parse(reader: &Reader) -> Result<ItmHeader, Truncated> {
        Ok(ItmHeader {
            version: parse_asciiz(&reader.tag_at(0x04, "version")?),
            unidentified_name: reader.u32_at(0x08, "unidentified name")?,
            identified_name: reader.u32_at(0x0c, "identified name")?,
            replacement_item: reader.resref_at(0x10, "replacement item")?,
            flags: reader.u32_at(0x18, "flags")?,
            item_type: reader.u16_at(0x1c, "item type")?,
            usability: reader.u32_at(0x1e, "usability")?,
            animation: parse_asciiz(reader.slice(0x22, 2, "animation")?),
            min_level: reader.u16_at(0x24, "min level")?,
            min_strength: reader.u16_at(0x26, "min strength")?,
            min_strength_bonus: reader.u8_at(0x28, "min strength bonus")?,
            kit_usability_1: reader.u8_at(0x29, "kit usability 1")?,
            min_intelligence: reader.u8_at(0x2a, "min intelligence")?,
            kit_usability_2: reader.u8_at(0x2b, "kit usability 2")?,
            min_dexterity: reader.u8_at(0x2c, "min dexterity")?,
            kit_usability_3: reader.u8_at(0x2d, "kit usability 3")?,
            min_wisdom: reader.u8_at(0x2e, "min wisdom")?,
            kit_usability_4: reader.u8_at(0x2f, "kit usability 4")?,
            min_constitution: reader.u8_at(0x30, "min constitution")?,
            weapon_proficiency: reader.u8_at(0x31, "weapon proficiency")?,
            min_charisma: reader.u16_at(0x32, "min charisma")?,
            price: reader.u32_at(0x34, "price")?,
            stack_amount: reader.u16_at(0x38, "stack amount")?,
            inventory_icon: reader.resref_at(0x3a, "inventory icon")?,
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

/// One extended header — an item "ability" — plus the effects it applies.
pub struct ExtHeader {
    attack_type: u8,
    id_req: u8,
    location: u8,
    alternative_dice_sides: u8,
    use_icon: String,
    target_type: u8,
    target_count: u8,
    range: u16,
    launcher_required: u8,
    alternative_dice_thrown: u8,
    speed_factor: u8,
    alternative_damage_bonus: u8,
    thac0_bonus: u16,
    dice_sides: u8,
    primary_type: u8,
    dice_thrown: u8,
    secondary_type: u8,
    damage_bonus: u16,
    damage_type: u16,
    max_charges: u16,
    charge_depletion: u16,
    flags: u32,
    projectile_animation: u16,
    melee_overhand: u16,
    melee_backhand: u16,
    melee_thrust: u16,
    is_arrow: u16,
    is_bolt: u16,
    is_bullet: u16,
    /// Effects this ability applies (its slice of the shared feature pool).
    features: Vec<Feature>,
    /// Symbolic names for `primary_type`/`secondary_type`, resolved by the
    /// manager from the MSCHOOL/MSECTYPE dictionary tables.
    primary_name: Option<String>,
    secondary_name: Option<String>,
}

impl ExtHeader {
    /// Parse the 56-byte extended header at `base`. Its effects are read
    /// separately by [`Itm::parse`], which knows the feature-pool offset.
    fn parse_at(reader: &Reader, base: usize) -> Result<ExtHeader, Truncated> {
        Ok(ExtHeader {
            attack_type: reader.u8_at(base, "attack type")?,
            id_req: reader.u8_at(base + 0x01, "id requirement")?,
            location: reader.u8_at(base + 0x02, "location")?,
            alternative_dice_sides: reader.u8_at(base + 0x03, "alt dice sides")?,
            use_icon: reader.resref_at(base + 0x04, "use icon")?,
            target_type: reader.u8_at(base + 0x0c, "target type")?,
            target_count: reader.u8_at(base + 0x0d, "target count")?,
            range: reader.u16_at(base + 0x0e, "range")?,
            launcher_required: reader.u8_at(base + 0x10, "launcher required")?,
            alternative_dice_thrown: reader.u8_at(base + 0x11, "alt dice thrown")?,
            speed_factor: reader.u8_at(base + 0x12, "speed factor")?,
            alternative_damage_bonus: reader.u8_at(base + 0x13, "alt damage bonus")?,
            thac0_bonus: reader.u16_at(base + 0x14, "thac0 bonus")?,
            dice_sides: reader.u8_at(base + 0x16, "dice sides")?,
            primary_type: reader.u8_at(base + 0x17, "primary type")?,
            dice_thrown: reader.u8_at(base + 0x18, "dice thrown")?,
            secondary_type: reader.u8_at(base + 0x19, "secondary type")?,
            damage_bonus: reader.u16_at(base + 0x1a, "damage bonus")?,
            damage_type: reader.u16_at(base + 0x1c, "damage type")?,
            max_charges: reader.u16_at(base + 0x22, "max charges")?,
            charge_depletion: reader.u16_at(base + 0x24, "charge depletion")?,
            flags: reader.u32_at(base + 0x26, "flags")?,
            projectile_animation: reader.u16_at(base + 0x2a, "projectile animation")?,
            melee_overhand: reader.u16_at(base + 0x2c, "melee overhand")?,
            melee_backhand: reader.u16_at(base + 0x2e, "melee backhand")?,
            melee_thrust: reader.u16_at(base + 0x30, "melee thrust")?,
            is_arrow: reader.u16_at(base + 0x32, "is arrow")?,
            is_bolt: reader.u16_at(base + 0x34, "is bolt")?,
            is_bullet: reader.u16_at(base + 0x36, "is bullet")?,
            features: Vec::new(),
            primary_name: None,
            secondary_name: None,
        })
    }

    /// The primary type (school), an index into MSCHOOL.2DA.
    pub fn primary_type(&self) -> u32 {
        self.primary_type as u32
    }

    /// The secondary type, an index into MSECTYPE.2DA.
    pub fn secondary_type(&self) -> u32 {
        self.secondary_type as u32
    }

    /// Attach the symbolic names resolved for the primary/secondary types.
    pub fn set_type_names(&mut self, primary: Option<String>, secondary: Option<String>) {
        self.primary_name = primary;
        self.secondary_name = secondary;
    }

    /// Render the ability and its effects in the human-readable view. `index` is
    /// the ability's position, used only for the heading.
    fn write_human(&self, f: &mut fmt::Formatter<'_>, index: usize) -> fmt::Result {
        writeln!(
            f,
            "  ability {index}: {} ({})",
            self.attack_type,
            attack_type_name(self.attack_type)
        )?;
        let d = "    ";
        writeln!(
            f,
            "{d}target:  {} ({})",
            self.target_type,
            item_target_name(self.target_type)
        )?;
        if self.range != 0 {
            writeln!(f, "{d}range:   {}", self.range)?;
        }
        writeln!(
            f,
            "{d}damage:  {}d{} {:+} ({})",
            self.dice_thrown,
            self.dice_sides,
            self.damage_bonus as i16,
            damage_type_name(self.damage_type)
        )?;
        if self.thac0_bonus != 0 {
            writeln!(f, "{d}thac0:   {:+}", self.thac0_bonus as i16)?;
        }
        if self.speed_factor != 0 {
            writeln!(f, "{d}speed:   {}", self.speed_factor)?;
        }
        if self.launcher_required != 0 {
            writeln!(
                f,
                "{d}launcher: {} ({})",
                self.launcher_required,
                launcher_name(self.launcher_required)
            )?;
        }
        write_school(f, d, "primary", self.primary_type, &self.primary_name)?;
        write_school(f, d, "secondary", self.secondary_type, &self.secondary_name)?;
        if self.max_charges != 0 {
            writeln!(
                f,
                "{d}charges: {} ({})",
                self.max_charges,
                depletion_name(self.charge_depletion)
            )?;
        }
        let flags = ext_flags(self.flags);
        if !flags.is_empty() {
            writeln!(f, "{d}flags:   {}", flags.join(", "))?;
        }
        writeln!(f, "{d}effects: {}", self.features.len())?;
        for feat in &self.features {
            feat.write_human(f, "      ")?;
        }
        Ok(())
    }

    /// Write the ability's fields and effects as `key=value` lines under
    /// `prefix` (e.g. `"ext.0."`). Structural offset/count words are omitted —
    /// the effect count is implicit in the `effect.<n>` keys.
    fn write_raw(&self, out: &mut dyn Write, prefix: &str) -> io::Result<()> {
        writeln!(out, "{prefix}attack_type={}", self.attack_type)?;
        writeln!(out, "{prefix}id_req={}", self.id_req)?;
        writeln!(out, "{prefix}location={}", self.location)?;
        writeln!(
            out,
            "{prefix}alternative_dice_sides={}",
            self.alternative_dice_sides
        )?;
        writeln!(out, "{prefix}use_icon={}", self.use_icon)?;
        writeln!(out, "{prefix}target_type={}", self.target_type)?;
        writeln!(out, "{prefix}target_count={}", self.target_count)?;
        writeln!(out, "{prefix}range={}", self.range)?;
        writeln!(out, "{prefix}launcher_required={}", self.launcher_required)?;
        writeln!(
            out,
            "{prefix}alternative_dice_thrown={}",
            self.alternative_dice_thrown
        )?;
        writeln!(out, "{prefix}speed_factor={}", self.speed_factor)?;
        writeln!(
            out,
            "{prefix}alternative_damage_bonus={}",
            self.alternative_damage_bonus
        )?;
        writeln!(out, "{prefix}thac0_bonus={}", self.thac0_bonus)?;
        writeln!(out, "{prefix}dice_sides={}", self.dice_sides)?;
        writeln!(out, "{prefix}primary_type={}", self.primary_type)?;
        writeln!(out, "{prefix}dice_thrown={}", self.dice_thrown)?;
        writeln!(out, "{prefix}secondary_type={}", self.secondary_type)?;
        writeln!(out, "{prefix}damage_bonus={}", self.damage_bonus)?;
        writeln!(out, "{prefix}damage_type={}", self.damage_type)?;
        writeln!(out, "{prefix}max_charges={}", self.max_charges)?;
        writeln!(out, "{prefix}charge_depletion={}", self.charge_depletion)?;
        writeln!(out, "{prefix}flags={}", self.flags)?;
        writeln!(
            out,
            "{prefix}projectile_animation={}",
            self.projectile_animation
        )?;
        writeln!(out, "{prefix}melee_overhand={}", self.melee_overhand)?;
        writeln!(out, "{prefix}melee_backhand={}", self.melee_backhand)?;
        writeln!(out, "{prefix}melee_thrust={}", self.melee_thrust)?;
        writeln!(out, "{prefix}is_arrow={}", self.is_arrow)?;
        writeln!(out, "{prefix}is_bolt={}", self.is_bolt)?;
        writeln!(out, "{prefix}is_bullet={}", self.is_bullet)?;
        for (j, feat) in self.features.iter().enumerate() {
            feat.write_raw(out, &format!("{prefix}effect.{j}."))?;
        }
        Ok(())
    }
}

/// A parsed ITM file: the header, the always-on equipping effects, and the
/// item's abilities (each with its own effects).
pub struct Itm {
    header: ItmHeader,
    equipping: Vec<Feature>,
    ext_headers: Vec<ExtHeader>,
    /// The item's display names, resolved from the header strrefs by the
    /// manager (the parser cannot reach the TLK). `None` when unset or absent.
    unidentified_name: Option<String>,
    identified_name: Option<String>,
}

impl Itm {
    /// Parse an ITM V1 resource.
    pub fn parse(data: &[u8]) -> Result<Itm, ItmError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != ITM_SIGNATURE {
            return Err(ItmError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != ITM_VERSION {
            return Err(ItmError::BadVersion(version));
        }

        let header = ItmHeader::parse(&reader)?;

        // The header's structural words locate the two arrays below.
        let ext_offset = reader.u32_at(0x64, "ext header offset")? as usize;
        let ext_count = reader.u16_at(0x68, "ext header count")? as usize;
        let feature_offset = reader.u32_at(0x6a, "feature block offset")?;
        let equipping_index = reader.u16_at(0x6e, "equipping index")?;
        let equipping_count = reader.u16_at(0x70, "equipping count")?;

        // Equipping (always-on) effects: a slice of the shared feature pool.
        let equipping =
            Feature::read_pool(&reader, feature_offset, equipping_index, equipping_count)?;

        // Each extended header reads its own slice of the same pool.
        let mut ext_headers = Vec::with_capacity(ext_count);
        for i in 0..ext_count {
            let base = ext_offset + i * EXT_HEADER_SIZE;
            let mut ext = ExtHeader::parse_at(&reader, base)?;
            let feature_index = reader.u16_at(base + 0x20, "feature index")?;
            let feature_count = reader.u16_at(base + 0x1e, "feature count")?;
            ext.features =
                Feature::read_pool(&reader, feature_offset, feature_index, feature_count)?;
            ext_headers.push(ext);
        }

        Ok(Itm {
            header,
            equipping,
            ext_headers,
            unidentified_name: None,
            identified_name: None,
        })
    }

    /// The header strrefs for the item's unidentified and identified names, for
    /// the manager to resolve against the TLK.
    pub fn name_strrefs(&self) -> (u32, u32) {
        (self.header.unidentified_name, self.header.identified_name)
    }

    /// Every TLK strref this item references, tagged with its field. The four
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
        // Effects can carry a strref (a "display string" opcode): the always-on
        // equipping effects, then each ability's own effects.
        for (i, feat) in self.equipping.iter().enumerate() {
            let at = format!("equipping[{i}]");
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

    /// Every resource this item names, tagged with the type it names: the item
    /// it degrades into, and the BAM icons for the header and each ability.
    /// Effect resources are not yet decoded.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let h = &self.header;
        let mut uses = vec![
            ResrefUse::new(ITM_TYPE, &h.replacement_item, "replacement_item"),
            ResrefUse::new(BAM_TYPE, &h.inventory_icon, "inventory_icon"),
            ResrefUse::new(BAM_TYPE, &h.ground_icon, "ground_icon"),
            ResrefUse::new(BAM_TYPE, &h.description_icon, "description_icon"),
        ];
        for (i, ext) in self.ext_headers.iter().enumerate() {
            uses.push(ResrefUse::new(
                BAM_TYPE,
                &ext.use_icon,
                format!("ability[{i}].use_icon"),
            ));
        }
        uses.retain(|u| !u.value.is_empty());
        // Effects can name a resource (creating or removing an item, say): the
        // always-on equipping effects, then each ability's own effects.
        for (i, feat) in self.equipping.iter().enumerate() {
            let at = format!("equipping[{i}]");
            uses.extend(feat.resref_uses().into_iter().map(|u| u.prefixed(&at)));
        }
        for (j, ext) in self.ext_headers.iter().enumerate() {
            for (k, feat) in ext.features.iter().enumerate() {
                let at = format!("ability[{j}].effect[{k}]");
                uses.extend(feat.resref_uses().into_iter().map(|u| u.prefixed(&at)));
            }
        }
        uses
    }

    /// Attach the display names resolved from the header strrefs.
    pub fn set_names(&mut self, unidentified: Option<String>, identified: Option<String>) {
        self.unidentified_name = unidentified;
        self.identified_name = identified;
    }

    /// The item's abilities, for the manager to resolve their school names.
    pub fn ext_headers_mut(&mut self) -> &mut [ExtHeader] {
        &mut self.ext_headers
    }

    /// The item's abilities (read-only).
    pub fn ext_headers(&self) -> &[ExtHeader] {
        &self.ext_headers
    }

    /// Write the whole item as flat, self-locating `key=value` lines: header
    /// fields bare, equipping effects under `equip.<n>.`, abilities under
    /// `ext.<n>.` (and their effects under `ext.<n>.effect.<m>.`).
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "version={}", h.version)?;
        writeln!(out, "unidentified_name={}", h.unidentified_name)?;
        writeln!(out, "identified_name={}", h.identified_name)?;
        writeln!(out, "replacement_item={}", h.replacement_item)?;
        writeln!(out, "flags={}", h.flags)?;
        writeln!(out, "item_type={}", h.item_type)?;
        writeln!(out, "usability={}", h.usability)?;
        writeln!(out, "animation={}", h.animation)?;
        writeln!(out, "min_level={}", h.min_level)?;
        writeln!(out, "min_strength={}", h.min_strength)?;
        writeln!(out, "min_strength_bonus={}", h.min_strength_bonus)?;
        writeln!(out, "kit_usability_1={}", h.kit_usability_1)?;
        writeln!(out, "min_intelligence={}", h.min_intelligence)?;
        writeln!(out, "kit_usability_2={}", h.kit_usability_2)?;
        writeln!(out, "min_dexterity={}", h.min_dexterity)?;
        writeln!(out, "kit_usability_3={}", h.kit_usability_3)?;
        writeln!(out, "min_wisdom={}", h.min_wisdom)?;
        writeln!(out, "kit_usability_4={}", h.kit_usability_4)?;
        writeln!(out, "min_constitution={}", h.min_constitution)?;
        writeln!(out, "weapon_proficiency={}", h.weapon_proficiency)?;
        writeln!(out, "min_charisma={}", h.min_charisma)?;
        writeln!(out, "price={}", h.price)?;
        writeln!(out, "stack_amount={}", h.stack_amount)?;
        writeln!(out, "inventory_icon={}", h.inventory_icon)?;
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
        for (i, feat) in self.equipping.iter().enumerate() {
            feat.write_raw(out, &format!("equip.{i}."))?;
        }
        for (i, ext) in self.ext_headers.iter().enumerate() {
            ext.write_raw(out, &format!("ext.{i}."))?;
        }
        Ok(())
    }
}

impl fmt::Display for Itm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "ITM V1")?;
        match (&self.identified_name, &self.unidentified_name) {
            // Show the identified name; surface the unidentified one only when
            // it is a distinct string (most items reuse the identified name).
            (Some(id), Some(unid)) if unid != id => {
                writeln!(f, "  name:        {id}")?;
                writeln!(f, "  unid. name:  {unid}")?;
            }
            (Some(name), _) | (None, Some(name)) => writeln!(f, "  name:        {name}")?,
            // No string resolved (e.g. TLK unavailable): keep the raw strrefs.
            (None, None) => writeln!(
                f,
                "  name:        unid strref {} / id strref {}",
                h.unidentified_name, h.identified_name
            )?,
        }
        writeln!(
            f,
            "  item type:   {} ({})",
            h.item_type,
            item_category_name(h.item_type)
        )?;
        write_bitfield(f, "  flags:       ", h.flags, &header_flags(h.flags))?;
        writeln!(f, "  usability:   0x{:08x}", h.usability)?;
        let unusable = usability_names(h.usability);
        if !unusable.is_empty() {
            writeln!(f, "    {:<9} {}", "unusable:", unusable.join(", "))?;
        }
        let kits = kit_names(
            h.kit_usability_1,
            h.kit_usability_2,
            h.kit_usability_3,
            h.kit_usability_4,
        );
        if !kits.is_empty() {
            writeln!(f, "    {:<9} {}", "kits:", kits.join(", "))?;
        }
        if h.weapon_proficiency != 0 {
            writeln!(
                f,
                "  proficiency: 0x{:02x} ({})",
                h.weapon_proficiency,
                proficiency_name(h.weapon_proficiency)
            )?;
        }
        if !h.animation.trim().is_empty() {
            writeln!(f, "  animation:   {:?}", h.animation)?;
        }
        writeln!(f, "  enchantment: {}", h.enchantment)?;
        writeln!(f, "  price:       {}", h.price)?;
        writeln!(f, "  weight:      {}", h.weight)?;
        writeln!(f, "  lore:        {}", h.lore_to_id)?;
        writeln!(f, "  stack:       {}", h.stack_amount)?;
        let requires = requirements(h);
        if !requires.is_empty() {
            writeln!(f, "  requires:    {requires}")?;
        }
        if !h.replacement_item.is_empty() {
            writeln!(f, "  replacement: {:?}", h.replacement_item)?;
        }

        writeln!(f)?;
        writeln!(f, "  equipping effects: {}", self.equipping.len())?;
        for feat in &self.equipping {
            feat.write_human(f, "    ")?;
        }

        writeln!(f)?;
        writeln!(f, "  abilities: {}", self.ext_headers.len())?;
        for (i, ext) in self.ext_headers.iter().enumerate() {
            writeln!(f)?;
            ext.write_human(f, i)?;
        }
        Ok(())
    }
}

/// Compact "min stat" line listing only the requirements that are set.
fn requirements(h: &ItmHeader) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |label: &str, value: u32| {
        if value != 0 {
            parts.push(format!("{label} {value}"));
        }
    };
    push("level", h.min_level as u32);
    push("str", h.min_strength as u32);
    push("str+", h.min_strength_bonus as u32);
    push("int", h.min_intelligence as u32);
    push("dex", h.min_dexterity as u32);
    push("wis", h.min_wisdom as u32);
    push("con", h.min_constitution as u32);
    push("cha", h.min_charisma as u32);
    parts.join(", ")
}

/// Name an ability's attack type (extended-header 0x00).
fn attack_type_name(value: u8) -> &'static str {
    match value {
        0 => "none",
        1 => "melee",
        2 => "ranged",
        3 => "magical",
        4 => "launcher",
        _ => "?",
    }
}

/// Name an ability's target type (extended-header 0x0c). Distinct from the
/// effect-level target type used by feature blocks.
fn item_target_name(value: u8) -> &'static str {
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

/// Name a launcher requirement (extended-header 0x10).
fn launcher_name(value: u8) -> &'static str {
    match value {
        0 => "none",
        1 => "bow",
        2 => "crossbow",
        3 => "sling",
        40 => "spear",
        100 => "throwing axe",
        _ => "?",
    }
}

/// Name an ability's damage type (extended-header 0x1c).
fn damage_type_name(value: u16) -> &'static str {
    match value {
        0 => "none",
        1 => "piercing",
        2 => "crushing",
        3 => "slashing",
        4 => "missile",
        5 => "fist",
        6 => "piercing/crushing",
        7 => "piercing/slashing",
        8 => "crushing/slashing",
        9 => "blunt missile",
        _ => "none",
    }
}

/// Name a charge-depletion behaviour (extended-header 0x24).
fn depletion_name(value: u16) -> &'static str {
    match value {
        0 => "item remains",
        1 => "item vanishes",
        2 => "replace with used-up",
        3 => "item recharges",
        _ => "item remains",
    }
}

/// Names of the set header flags (header 0x18). BG2:EE-relevant bits only.
fn header_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "critical item"),
            (1, "two-handed"),
            (2, "droppable"),
            (3, "displayable"),
            (4, "cursed"),
            (5, "cannot scribe"),
            (6, "magical"),
            (7, "left-handed"),
            (8, "silver"),
            (9, "cold iron"),
            (10, "off-hand"),
            (11, "conversable"),
            (12, "fake two-handed"),
            (13, "forbid off-hand"),
            (15, "adamantine"),
            (24, "undispellable"),
            (25, "crit-aversion toggle"),
        ],
    )
}

/// Names of the set extended-header flags (extended-header 0x26). BG2:EE bits.
fn ext_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "add strength bonus"),
            (1, "breakable"),
            (2, "damage strength bonus"),
            (3, "thac0 strength bonus"),
            (9, "breaks sanctuary/invisibility"),
            (10, "hostile"),
            (11, "recharge after resting"),
            (25, "toggle backstab"),
            (26, "cannot target invisible"),
        ],
    )
}

/// Classes, races, and alignments the item is *unusable* by (header 0x1e). The
/// four usability bytes pack into one dword, bit `i` naming the i-th entry.
fn usability_names(value: u32) -> Vec<&'static str> {
    set_bits(
        value,
        &[
            // byte 1 (bits 0-7): alignment, then bard/cleric
            "chaotic",
            "evil",
            "good",
            "neutral(g/e)",
            "lawful",
            "neutral(l/c)",
            "bard",
            "cleric",
            // byte 2 (bits 8-15)
            "cleric/mage",
            "cleric/thief",
            "cleric/ranger",
            "fighter",
            "fighter/druid",
            "fighter/mage",
            "fighter/cleric",
            "fighter/mage/cleric",
            // byte 3 (bits 16-23)
            "fighter/mage/thief",
            "fighter/thief",
            "mage/sorcerer",
            "mage/thief",
            "paladin",
            "ranger",
            "thief",
            "elf",
            // byte 4 (bits 24-31)
            "dwarf",
            "half-elf",
            "halfling",
            "human",
            "gnome",
            "monk",
            "druid/shaman",
            "half-orc",
        ],
    )
}

/// Kits the item is *unusable* by (header 0x29/0x2b/0x2d/0x2f). The four kit
/// bytes pack into one dword, bit `i` naming the i-th entry.
fn kit_names(b1: u8, b2: u8, b3: u8, b4: u8) -> Vec<&'static str> {
    let bits = (b1 as u32) | ((b2 as u32) << 8) | ((b3 as u32) << 16) | ((b4 as u32) << 24);
    set_bits(
        bits,
        &[
            // byte 1 (bits 0-7)
            "cleric of talos",
            "cleric of helm",
            "cleric of lathander",
            "totemic druid",
            "shapeshifter druid",
            "avenger druid",
            "barbarian",
            "wild mage",
            // byte 2 (bits 8-15)
            "stalker",
            "beastmaster",
            "assassin",
            "bounty hunter",
            "swashbuckler",
            "blade",
            "jester",
            "skald",
            // byte 3 (bits 16-23)
            "diviner",
            "enchanter",
            "illusionist",
            "invoker",
            "necromancer",
            "transmuter",
            "all (no kit)",
            "ferlain",
            // byte 4 (bits 24-31)
            "berserker",
            "wizard slayer",
            "kensai",
            "cavalier",
            "inquisitor",
            "undead hunter",
            "abjurer",
            "conjurer",
        ],
    )
}

/// Name a weapon proficiency code (header 0x31). Only the classic weapon
/// proficiencies are named; the "extra proficiency" slots fall through.
fn proficiency_name(value: u8) -> &'static str {
    match value {
        0x00 => "none",
        0x59 => "bastard sword",
        0x5a => "long sword",
        0x5b => "short sword",
        0x5c => "axe",
        0x5d => "two-handed sword",
        0x5e => "katana",
        0x5f => "scimitar",
        0x60 => "dagger",
        0x61 => "war hammer",
        0x62 => "spear",
        0x63 => "halberd",
        0x64 => "flail/morningstar",
        0x65 => "mace",
        0x66 => "quarterstaff",
        0x67 => "crossbow",
        0x68 => "long bow",
        0x69 => "short bow",
        0x6a => "dart",
        0x6b => "sling",
        0x6c => "blackjack",
        0x6d => "gun",
        0x6e => "martial arts",
        0x6f => "two-handed style",
        0x70 => "sword and shield style",
        0x71 => "single weapon style",
        0x72 => "two weapon style",
        0x73 => "club",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ITM V1 file: a full header with both array counts zeroed.
    fn build_itm() -> Vec<u8> {
        let mut data = vec![0u8; 0x9a];
        data[0x00..0x04].copy_from_slice(ITM_SIGNATURE);
        data[0x04..0x08].copy_from_slice(ITM_VERSION);
        data
    }

    #[test]
    fn resref_uses_reports_the_replacement_item_and_icons() {
        let mut data = build_itm();
        data[0x10..0x14].copy_from_slice(b"SW1H"); // replacement item
        data[0x3a..0x3e].copy_from_slice(b"ISW1"); // inventory icon
        data[0x44..0x48].copy_from_slice(b"GSW1"); // ground icon

        let itm = Itm::parse(&data).expect("parse");
        let uses = itm.resref_uses();
        let got: Vec<(u16, &str, &str)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (ITM_TYPE, "SW1H", "replacement_item"),
                (BAM_TYPE, "ISW1", "inventory_icon"),
                (BAM_TYPE, "GSW1", "ground_icon"),
            ]
        );
    }

    #[test]
    fn strref_uses_lists_the_four_header_strings() {
        let mut data = build_itm();
        for (at, value) in [(0x08, 10u32), (0x0c, 11), (0x50, 20), (0x54, 21)] {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let uses = Itm::parse(&data).expect("parse").strref_uses();
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
