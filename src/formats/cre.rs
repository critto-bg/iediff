//! CRE parser.
//!
//! A creature: NPCs, monsters, party members. Unlike ITM/SPL, a CRE is a fixed
//! 0x2d4-byte header followed by six independently-offset sections (known
//! spells, spell-memorization info, memorized spells, effects, items, item
//! slots), each an array of fixed-size records reached through a header offset.
//!
//! The effects section holds embedded EFF effects in one of two layouts chosen
//! by the header's `eff_version` byte (0x33): the 48-byte V1 [`Feature`] or the
//! 264-byte V2 body ([`Eff`], minus its 8-byte outer header). BG2:EE uses V2 for
//! all but a handful of creatures. Spec: `md/file_formats/ie_formats/cre_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::eff::{EFF_V2_BODY_SIZE, Eff, FEATURE_SIZE, Feature};
use crate::formats::fields::set_bits;
use crate::{Reader, Truncated, describe_tag, parse_asciiz};

/// Resource type code for CRE files.
pub const CRE_TYPE: u16 = 0x03f1;

const CRE_SIGNATURE: &[u8; 4] = b"CRE ";
const CRE_VERSION: &[u8; 4] = b"V1.0";

/// Fixed sizes of the section records.
const KNOWN_SPELL_SIZE: usize = 12;
const MEM_INFO_SIZE: usize = 16;
const MEMORIZED_SIZE: usize = 12;
const ITEM_SIZE: usize = 20;
/// Inventory slots: 38 slot words, then the selected-weapon and ability words.
const SLOT_COUNT: usize = 38;

/// Anything that can go wrong while parsing a CRE file.
#[derive(Debug)]
pub enum CreError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for CreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreError::Truncated(e) => write!(f, "{e}"),
            CreError::BadSignature(found) => {
                write!(f, "not a CRE file: signature was {}", describe_tag(found))
            }
            CreError::BadVersion(found) => {
                write!(f, "unsupported CRE version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for CreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for CreError {
    fn from(e: Truncated) -> Self {
        CreError::Truncated(e)
    }
}

/// The fixed 0x2d4-byte CRE header. All documented BG2:EE fields are retained;
/// the structural section offset/count words are read but not stored.
struct CreHeader {
    version: String,
    long_name: u32,
    short_name: u32,
    flags: u32,
    xp_value: u32,
    power_level: u32,
    gold: u32,
    status_flags: u32,
    current_hp: u16,
    max_hp: u16,
    animation: u32,
    metal_color: u8,
    minor_color: u8,
    major_color: u8,
    skin_color: u8,
    leather_color: u8,
    armor_color: u8,
    hair_color: u8,
    eff_version: u8,
    small_portrait: String,
    large_portrait: String,
    reputation: u8,
    hide_in_shadows: u8,
    ac_natural: i16,
    ac_effective: i16,
    ac_crushing: i16,
    ac_missile: i16,
    ac_piercing: i16,
    ac_slashing: i16,
    thac0: u8,
    num_attacks: u8,
    save_death: u8,
    save_wands: u8,
    save_polymorph: u8,
    save_breath: u8,
    save_spells: u8,
    resist_fire: u8,
    resist_cold: u8,
    resist_electricity: u8,
    resist_acid: u8,
    resist_magic: u8,
    resist_magic_fire: u8,
    resist_magic_cold: u8,
    resist_slashing: u8,
    resist_crushing: u8,
    resist_piercing: u8,
    resist_missile: u8,
    detect_illusion: u8,
    set_traps: u8,
    lore: u8,
    lockpicking: u8,
    move_silently: u8,
    find_traps: u8,
    pick_pockets: u8,
    fatigue: u8,
    intoxication: u8,
    luck: u8,
    proficiencies: [u8; 20],
    turn_undead_level: u8,
    tracking_skill: u8,
    tracking_target: String,
    soundset: [u32; 100],
    level_first: u8,
    level_second: u8,
    level_third: u8,
    sex: u8,
    strength: u8,
    strength_bonus: u8,
    intelligence: u8,
    wisdom: u8,
    dexterity: u8,
    constitution: u8,
    charisma: u8,
    morale: u8,
    morale_break: u8,
    racial_enemy: u8,
    morale_recovery: u16,
    kit: u32,
    script_override: String,
    script_class: String,
    script_race: String,
    script_general: String,
    script_default: String,
    ea: u8,
    general: u8,
    race: u8,
    class: u8,
    specific: u8,
    gender: u8,
    object_refs: [u8; 5],
    alignment: u8,
    global_enum: u16,
    local_enum: u16,
    death_variable: String,
    dialog: String,
}

impl CreHeader {
    fn parse(reader: &Reader) -> Result<CreHeader, Truncated> {
        let mut proficiencies = [0u8; 20];
        for (i, slot) in proficiencies.iter_mut().enumerate() {
            *slot = reader.u8_at(0x6e + i, "proficiency")?;
        }

        let mut soundset = [0u32; 100];
        for (i, slot) in soundset.iter_mut().enumerate() {
            *slot = reader.u32_at(0xa4 + i * 4, "soundset strref")?;
        }

        let mut object_refs = [0u8; 5];
        for (i, slot) in object_refs.iter_mut().enumerate() {
            *slot = reader.u8_at(0x276 + i, "object ref")?;
        }

        Ok(CreHeader {
            version: parse_asciiz(&reader.tag_at(0x04, "version")?),
            long_name: reader.u32_at(0x08, "long name")?,
            short_name: reader.u32_at(0x0c, "short name")?,
            flags: reader.u32_at(0x10, "creature flags")?,
            xp_value: reader.u32_at(0x14, "xp value")?,
            power_level: reader.u32_at(0x18, "power level")?,
            gold: reader.u32_at(0x1c, "gold")?,
            status_flags: reader.u32_at(0x20, "status flags")?,
            current_hp: reader.u16_at(0x24, "current hp")?,
            max_hp: reader.u16_at(0x26, "max hp")?,
            animation: reader.u32_at(0x28, "animation id")?,
            metal_color: reader.u8_at(0x2c, "metal colour")?,
            minor_color: reader.u8_at(0x2d, "minor colour")?,
            major_color: reader.u8_at(0x2e, "major colour")?,
            skin_color: reader.u8_at(0x2f, "skin colour")?,
            leather_color: reader.u8_at(0x30, "leather colour")?,
            armor_color: reader.u8_at(0x31, "armour colour")?,
            hair_color: reader.u8_at(0x32, "hair colour")?,
            eff_version: reader.u8_at(0x33, "eff version")?,
            small_portrait: reader.resref_at(0x34, "small portrait")?,
            large_portrait: reader.resref_at(0x3c, "large portrait")?,
            reputation: reader.u8_at(0x44, "reputation")?,
            hide_in_shadows: reader.u8_at(0x45, "hide in shadows")?,
            ac_natural: reader.u16_at(0x46, "ac natural")? as i16,
            ac_effective: reader.u16_at(0x48, "ac effective")? as i16,
            ac_crushing: reader.u16_at(0x4a, "ac crushing")? as i16,
            ac_missile: reader.u16_at(0x4c, "ac missile")? as i16,
            ac_piercing: reader.u16_at(0x4e, "ac piercing")? as i16,
            ac_slashing: reader.u16_at(0x50, "ac slashing")? as i16,
            thac0: reader.u8_at(0x52, "thac0")?,
            num_attacks: reader.u8_at(0x53, "number of attacks")?,
            save_death: reader.u8_at(0x54, "save vs death")?,
            save_wands: reader.u8_at(0x55, "save vs wands")?,
            save_polymorph: reader.u8_at(0x56, "save vs polymorph")?,
            save_breath: reader.u8_at(0x57, "save vs breath")?,
            save_spells: reader.u8_at(0x58, "save vs spells")?,
            resist_fire: reader.u8_at(0x59, "resist fire")?,
            resist_cold: reader.u8_at(0x5a, "resist cold")?,
            resist_electricity: reader.u8_at(0x5b, "resist electricity")?,
            resist_acid: reader.u8_at(0x5c, "resist acid")?,
            resist_magic: reader.u8_at(0x5d, "resist magic")?,
            resist_magic_fire: reader.u8_at(0x5e, "resist magic fire")?,
            resist_magic_cold: reader.u8_at(0x5f, "resist magic cold")?,
            resist_slashing: reader.u8_at(0x60, "resist slashing")?,
            resist_crushing: reader.u8_at(0x61, "resist crushing")?,
            resist_piercing: reader.u8_at(0x62, "resist piercing")?,
            resist_missile: reader.u8_at(0x63, "resist missile")?,
            detect_illusion: reader.u8_at(0x64, "detect illusion")?,
            set_traps: reader.u8_at(0x65, "set traps")?,
            lore: reader.u8_at(0x66, "lore")?,
            lockpicking: reader.u8_at(0x67, "lockpicking")?,
            move_silently: reader.u8_at(0x68, "move silently")?,
            find_traps: reader.u8_at(0x69, "find traps")?,
            pick_pockets: reader.u8_at(0x6a, "pick pockets")?,
            fatigue: reader.u8_at(0x6b, "fatigue")?,
            intoxication: reader.u8_at(0x6c, "intoxication")?,
            luck: reader.u8_at(0x6d, "luck")?,
            proficiencies,
            turn_undead_level: reader.u8_at(0x82, "turn undead level")?,
            tracking_skill: reader.u8_at(0x83, "tracking skill")?,
            tracking_target: parse_asciiz(reader.slice(0x84, 32, "tracking target")?),
            soundset,
            level_first: reader.u8_at(0x234, "level first class")?,
            level_second: reader.u8_at(0x235, "level second class")?,
            level_third: reader.u8_at(0x236, "level third class")?,
            sex: reader.u8_at(0x237, "sex")?,
            strength: reader.u8_at(0x238, "strength")?,
            strength_bonus: reader.u8_at(0x239, "strength bonus")?,
            intelligence: reader.u8_at(0x23a, "intelligence")?,
            wisdom: reader.u8_at(0x23b, "wisdom")?,
            dexterity: reader.u8_at(0x23c, "dexterity")?,
            constitution: reader.u8_at(0x23d, "constitution")?,
            charisma: reader.u8_at(0x23e, "charisma")?,
            morale: reader.u8_at(0x23f, "morale")?,
            morale_break: reader.u8_at(0x240, "morale break")?,
            racial_enemy: reader.u8_at(0x241, "racial enemy")?,
            morale_recovery: reader.u16_at(0x242, "morale recovery")?,
            kit: reader.u32_at(0x244, "kit")?,
            script_override: reader.resref_at(0x248, "override script")?,
            script_class: reader.resref_at(0x250, "class script")?,
            script_race: reader.resref_at(0x258, "race script")?,
            script_general: reader.resref_at(0x260, "general script")?,
            script_default: reader.resref_at(0x268, "default script")?,
            ea: reader.u8_at(0x270, "enemy-ally")?,
            general: reader.u8_at(0x271, "general")?,
            race: reader.u8_at(0x272, "race")?,
            class: reader.u8_at(0x273, "class")?,
            specific: reader.u8_at(0x274, "specific")?,
            gender: reader.u8_at(0x275, "gender")?,
            object_refs,
            alignment: reader.u8_at(0x27b, "alignment")?,
            global_enum: reader.u16_at(0x27c, "global enumeration")?,
            local_enum: reader.u16_at(0x27e, "local enumeration")?,
            death_variable: parse_asciiz(reader.slice(0x280, 32, "death variable")?),
            dialog: reader.resref_at(0x2cc, "dialog")?,
        })
    }
}

/// A spell the creature knows (12-byte record).
pub struct KnownSpell {
    resource: String,
    level: u16,
    spell_type: u16,
    name: Option<String>,
}

/// A row of the spell-memorization table (16-byte record).
struct MemInfo {
    level: u16,
    memorizable: u16,
    memorizable_after: u16,
    spell_type: u16,
    index: u32,
    count: u32,
}

/// A memorized spell (12-byte record).
pub struct MemorizedSpell {
    resource: String,
    memorized: u32,
    name: Option<String>,
}

/// An item the creature carries (20-byte record).
pub struct CreItem {
    resource: String,
    expiration: u16,
    quantity1: u16,
    quantity2: u16,
    quantity3: u16,
    flags: u32,
    name: Option<String>,
}

/// An embedded effect: V1 feature block or V2 body, per the header's
/// `eff_version`.
enum CreEffect {
    V1(Feature),
    V2(Eff),
}

impl KnownSpell {
    /// The SPL resref, for the manager to resolve a display name.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Attach the resolved SPL display name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

impl MemorizedSpell {
    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

impl CreItem {
    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

/// Symbolic names for the creature's IDS-mapped identity fields, resolved by the
/// manager.
#[derive(Default)]
pub struct CreNames {
    pub long_name: Option<String>,
    pub short_name: Option<String>,
    pub ea: Option<String>,
    pub general: Option<String>,
    pub race: Option<String>,
    pub class: Option<String>,
    pub specific: Option<String>,
    pub gender: Option<String>,
    pub alignment: Option<String>,
    pub animation: Option<String>,
    pub kit: Option<String>,
}

/// A parsed CRE file: the header, the spell/effect/item sections, and the
/// inventory slots.
pub struct Cre {
    header: CreHeader,
    known_spells: Vec<KnownSpell>,
    mem_info: Vec<MemInfo>,
    memorized_spells: Vec<MemorizedSpell>,
    effects: Vec<CreEffect>,
    items: Vec<CreItem>,
    slots: Vec<u16>,
    selected_weapon: u16,
    selected_ability: u16,
    names: CreNames,
}

impl Cre {
    /// Parse a CRE V1.0 resource.
    pub fn parse(data: &[u8]) -> Result<Cre, CreError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != CRE_SIGNATURE {
            return Err(CreError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != CRE_VERSION {
            return Err(CreError::BadVersion(version));
        }

        let header = CreHeader::parse(&reader)?;

        // Section offsets/counts live in the header.
        let known_off = reader.u32_at(0x2a0, "known spells offset")? as usize;
        let known_count = reader.u32_at(0x2a4, "known spells count")? as usize;
        let meminfo_off = reader.u32_at(0x2a8, "mem info offset")? as usize;
        let meminfo_count = reader.u32_at(0x2ac, "mem info count")? as usize;
        let memorized_off = reader.u32_at(0x2b0, "memorized offset")? as usize;
        let memorized_count = reader.u32_at(0x2b4, "memorized count")? as usize;
        let slots_off = reader.u32_at(0x2b8, "item slots offset")? as usize;
        let items_off = reader.u32_at(0x2bc, "items offset")? as usize;
        let items_count = reader.u32_at(0x2c0, "items count")? as usize;
        let effects_off = reader.u32_at(0x2c4, "effects offset")? as usize;
        let effects_count = reader.u32_at(0x2c8, "effects count")? as usize;

        let mut known_spells = Vec::with_capacity(known_count);
        for i in 0..known_count {
            let base = known_off + i * KNOWN_SPELL_SIZE;
            known_spells.push(KnownSpell {
                resource: reader.resref_at(base, "known spell")?,
                level: reader.u16_at(base + 0x08, "known spell level")?,
                spell_type: reader.u16_at(base + 0x0a, "known spell type")?,
                name: None,
            });
        }

        let mut mem_info = Vec::with_capacity(meminfo_count);
        for i in 0..meminfo_count {
            let base = meminfo_off + i * MEM_INFO_SIZE;
            mem_info.push(MemInfo {
                level: reader.u16_at(base, "mem info level")?,
                memorizable: reader.u16_at(base + 0x02, "memorizable")?,
                memorizable_after: reader.u16_at(base + 0x04, "memorizable after")?,
                spell_type: reader.u16_at(base + 0x06, "mem info type")?,
                index: reader.u32_at(base + 0x08, "mem info index")?,
                count: reader.u32_at(base + 0x0c, "mem info count")?,
            });
        }

        let mut memorized_spells = Vec::with_capacity(memorized_count);
        for i in 0..memorized_count {
            let base = memorized_off + i * MEMORIZED_SIZE;
            memorized_spells.push(MemorizedSpell {
                resource: reader.resref_at(base, "memorized spell")?,
                memorized: reader.u32_at(base + 0x08, "memorized flags")?,
                name: None,
            });
        }

        let mut items = Vec::with_capacity(items_count);
        for i in 0..items_count {
            let base = items_off + i * ITEM_SIZE;
            items.push(CreItem {
                resource: reader.resref_at(base, "item")?,
                expiration: reader.u16_at(base + 0x08, "item expiration")?,
                quantity1: reader.u16_at(base + 0x0a, "item quantity 1")?,
                quantity2: reader.u16_at(base + 0x0c, "item quantity 2")?,
                quantity3: reader.u16_at(base + 0x0e, "item quantity 3")?,
                flags: reader.u32_at(base + 0x10, "item flags")?,
                name: None,
            });
        }

        // Effects: V1 (48-byte feature) or V2 (264-byte body) per the header.
        let eff_size = if header.eff_version == 1 {
            EFF_V2_BODY_SIZE
        } else {
            FEATURE_SIZE
        };
        let mut effects = Vec::with_capacity(effects_count);
        for i in 0..effects_count {
            let base = effects_off + i * eff_size;
            let effect = if header.eff_version == 1 {
                CreEffect::V2(Eff::parse_embedded_at(&reader, base)?)
            } else {
                CreEffect::V1(Feature::parse_at(&reader, base)?)
            };
            effects.push(effect);
        }

        // Inventory: 38 slot words then the selected-weapon and ability words.
        let mut slots = Vec::with_capacity(SLOT_COUNT);
        for i in 0..SLOT_COUNT {
            slots.push(reader.u16_at(slots_off + i * 2, "item slot")?);
        }
        let selected_weapon = reader.u16_at(slots_off + SLOT_COUNT * 2, "selected weapon")?;
        let selected_ability =
            reader.u16_at(slots_off + (SLOT_COUNT + 1) * 2, "selected ability")?;

        Ok(Cre {
            header,
            known_spells,
            mem_info,
            memorized_spells,
            effects,
            items,
            slots,
            selected_weapon,
            selected_ability,
            names: CreNames::default(),
        })
    }

    /// The header strrefs for the creature's long and short names.
    pub fn name_strrefs(&self) -> (u32, u32) {
        (self.header.long_name, self.header.short_name)
    }

    /// The IDS-mapped identity codes (EA, general, race, class, specific,
    /// gender, alignment) and the animation id, for the manager to resolve.
    pub fn ids_codes(&self) -> CreIdsCodes {
        CreIdsCodes {
            ea: self.header.ea,
            general: self.header.general,
            race: self.header.race,
            class: self.header.class,
            specific: self.header.specific,
            gender: self.header.gender,
            alignment: self.header.alignment,
            animation: self.header.animation,
            kit: self.header.kit,
        }
    }

    /// Attach all resolved symbolic names.
    pub fn set_names(&mut self, names: CreNames) {
        self.names = names;
    }

    pub fn known_spells_mut(&mut self) -> &mut [KnownSpell] {
        &mut self.known_spells
    }

    pub fn memorized_spells_mut(&mut self) -> &mut [MemorizedSpell] {
        &mut self.memorized_spells
    }

    pub fn items_mut(&mut self) -> &mut [CreItem] {
        &mut self.items
    }

    /// Write the whole creature as flat, self-locating `key=value` lines.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "version={}", h.version)?;
        writeln!(out, "long_name={}", h.long_name)?;
        writeln!(out, "short_name={}", h.short_name)?;
        writeln!(out, "flags={}", h.flags)?;
        writeln!(out, "xp_value={}", h.xp_value)?;
        writeln!(out, "power_level={}", h.power_level)?;
        writeln!(out, "gold={}", h.gold)?;
        writeln!(out, "status_flags={}", h.status_flags)?;
        writeln!(out, "current_hp={}", h.current_hp)?;
        writeln!(out, "max_hp={}", h.max_hp)?;
        writeln!(out, "animation={}", h.animation)?;
        writeln!(out, "metal_color={}", h.metal_color)?;
        writeln!(out, "minor_color={}", h.minor_color)?;
        writeln!(out, "major_color={}", h.major_color)?;
        writeln!(out, "skin_color={}", h.skin_color)?;
        writeln!(out, "leather_color={}", h.leather_color)?;
        writeln!(out, "armor_color={}", h.armor_color)?;
        writeln!(out, "hair_color={}", h.hair_color)?;
        writeln!(out, "eff_version={}", h.eff_version)?;
        writeln!(out, "small_portrait={}", h.small_portrait)?;
        writeln!(out, "large_portrait={}", h.large_portrait)?;
        writeln!(out, "reputation={}", h.reputation)?;
        writeln!(out, "hide_in_shadows={}", h.hide_in_shadows)?;
        writeln!(out, "ac_natural={}", h.ac_natural)?;
        writeln!(out, "ac_effective={}", h.ac_effective)?;
        writeln!(out, "ac_crushing={}", h.ac_crushing)?;
        writeln!(out, "ac_missile={}", h.ac_missile)?;
        writeln!(out, "ac_piercing={}", h.ac_piercing)?;
        writeln!(out, "ac_slashing={}", h.ac_slashing)?;
        writeln!(out, "thac0={}", h.thac0)?;
        writeln!(out, "num_attacks={}", h.num_attacks)?;
        writeln!(out, "save_death={}", h.save_death)?;
        writeln!(out, "save_wands={}", h.save_wands)?;
        writeln!(out, "save_polymorph={}", h.save_polymorph)?;
        writeln!(out, "save_breath={}", h.save_breath)?;
        writeln!(out, "save_spells={}", h.save_spells)?;
        writeln!(out, "resist_fire={}", h.resist_fire)?;
        writeln!(out, "resist_cold={}", h.resist_cold)?;
        writeln!(out, "resist_electricity={}", h.resist_electricity)?;
        writeln!(out, "resist_acid={}", h.resist_acid)?;
        writeln!(out, "resist_magic={}", h.resist_magic)?;
        writeln!(out, "resist_magic_fire={}", h.resist_magic_fire)?;
        writeln!(out, "resist_magic_cold={}", h.resist_magic_cold)?;
        writeln!(out, "resist_slashing={}", h.resist_slashing)?;
        writeln!(out, "resist_crushing={}", h.resist_crushing)?;
        writeln!(out, "resist_piercing={}", h.resist_piercing)?;
        writeln!(out, "resist_missile={}", h.resist_missile)?;
        writeln!(out, "detect_illusion={}", h.detect_illusion)?;
        writeln!(out, "set_traps={}", h.set_traps)?;
        writeln!(out, "lore={}", h.lore)?;
        writeln!(out, "lockpicking={}", h.lockpicking)?;
        writeln!(out, "move_silently={}", h.move_silently)?;
        writeln!(out, "find_traps={}", h.find_traps)?;
        writeln!(out, "pick_pockets={}", h.pick_pockets)?;
        writeln!(out, "fatigue={}", h.fatigue)?;
        writeln!(out, "intoxication={}", h.intoxication)?;
        writeln!(out, "luck={}", h.luck)?;
        for (i, p) in h.proficiencies.iter().enumerate() {
            writeln!(out, "proficiency_{i}={p}")?;
        }
        writeln!(out, "turn_undead_level={}", h.turn_undead_level)?;
        writeln!(out, "tracking_skill={}", h.tracking_skill)?;
        writeln!(out, "tracking_target={}", h.tracking_target)?;
        for (i, s) in h.soundset.iter().enumerate() {
            writeln!(out, "soundset_{i}={s}")?;
        }
        writeln!(out, "level_first={}", h.level_first)?;
        writeln!(out, "level_second={}", h.level_second)?;
        writeln!(out, "level_third={}", h.level_third)?;
        writeln!(out, "sex={}", h.sex)?;
        writeln!(out, "strength={}", h.strength)?;
        writeln!(out, "strength_bonus={}", h.strength_bonus)?;
        writeln!(out, "intelligence={}", h.intelligence)?;
        writeln!(out, "wisdom={}", h.wisdom)?;
        writeln!(out, "dexterity={}", h.dexterity)?;
        writeln!(out, "constitution={}", h.constitution)?;
        writeln!(out, "charisma={}", h.charisma)?;
        writeln!(out, "morale={}", h.morale)?;
        writeln!(out, "morale_break={}", h.morale_break)?;
        writeln!(out, "racial_enemy={}", h.racial_enemy)?;
        writeln!(out, "morale_recovery={}", h.morale_recovery)?;
        writeln!(out, "kit={}", h.kit)?;
        writeln!(out, "script_override={}", h.script_override)?;
        writeln!(out, "script_class={}", h.script_class)?;
        writeln!(out, "script_race={}", h.script_race)?;
        writeln!(out, "script_general={}", h.script_general)?;
        writeln!(out, "script_default={}", h.script_default)?;
        writeln!(out, "ea={}", h.ea)?;
        writeln!(out, "general={}", h.general)?;
        writeln!(out, "race={}", h.race)?;
        writeln!(out, "class={}", h.class)?;
        writeln!(out, "specific={}", h.specific)?;
        writeln!(out, "gender={}", h.gender)?;
        for (i, o) in h.object_refs.iter().enumerate() {
            writeln!(out, "object_{i}={o}")?;
        }
        writeln!(out, "alignment={}", h.alignment)?;
        writeln!(out, "global_enum={}", h.global_enum)?;
        writeln!(out, "local_enum={}", h.local_enum)?;
        writeln!(out, "death_variable={}", h.death_variable)?;
        writeln!(out, "dialog={}", h.dialog)?;

        for (i, s) in self.known_spells.iter().enumerate() {
            writeln!(out, "known.{i}.resource={}", s.resource)?;
            writeln!(out, "known.{i}.level={}", s.level)?;
            writeln!(out, "known.{i}.type={}", s.spell_type)?;
        }
        for (i, m) in self.mem_info.iter().enumerate() {
            writeln!(out, "meminfo.{i}.level={}", m.level)?;
            writeln!(out, "meminfo.{i}.memorizable={}", m.memorizable)?;
            writeln!(out, "meminfo.{i}.memorizable_after={}", m.memorizable_after)?;
            writeln!(out, "meminfo.{i}.type={}", m.spell_type)?;
            writeln!(out, "meminfo.{i}.index={}", m.index)?;
            writeln!(out, "meminfo.{i}.count={}", m.count)?;
        }
        for (i, s) in self.memorized_spells.iter().enumerate() {
            writeln!(out, "memorized.{i}.resource={}", s.resource)?;
            writeln!(out, "memorized.{i}.memorized={}", s.memorized)?;
        }
        for (i, it) in self.items.iter().enumerate() {
            writeln!(out, "item.{i}.resource={}", it.resource)?;
            writeln!(out, "item.{i}.expiration={}", it.expiration)?;
            writeln!(out, "item.{i}.quantity1={}", it.quantity1)?;
            writeln!(out, "item.{i}.quantity2={}", it.quantity2)?;
            writeln!(out, "item.{i}.quantity3={}", it.quantity3)?;
            writeln!(out, "item.{i}.flags={}", it.flags)?;
        }
        for (i, slot) in self.slots.iter().enumerate() {
            writeln!(out, "slot.{i}={slot}")?;
        }
        writeln!(out, "selected_weapon={}", self.selected_weapon)?;
        writeln!(out, "selected_ability={}", self.selected_ability)?;
        for (i, e) in self.effects.iter().enumerate() {
            let prefix = format!("effect.{i}.");
            match e {
                CreEffect::V1(feat) => feat.write_raw(out, &prefix)?,
                CreEffect::V2(eff) => eff.write_raw(out, &prefix)?,
            }
        }
        Ok(())
    }
}

/// The IDS-mapped identity codes a CRE carries, handed to the manager to
/// resolve against the IDS tables.
pub struct CreIdsCodes {
    pub ea: u8,
    pub general: u8,
    pub race: u8,
    pub class: u8,
    pub specific: u8,
    pub gender: u8,
    pub alignment: u8,
    pub animation: u32,
    pub kit: u32,
}

impl fmt::Display for Cre {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        let n = &self.names;
        writeln!(f, "CRE V1.0")?;
        writeln!(
            f,
            "  name:        {}",
            name_or_strref(&n.long_name, h.long_name)
        )?;
        if h.short_name != h.long_name {
            writeln!(
                f,
                "  short name:  {}",
                name_or_strref(&n.short_name, h.short_name)
            )?;
        }
        write_named(f, "EA", h.ea, &n.ea)?;
        write_named(f, "general", h.general, &n.general)?;
        write_named(f, "race", h.race, &n.race)?;
        write_named(f, "class", h.class, &n.class)?;
        write_named(f, "specific", h.specific, &n.specific)?;
        write_named(f, "gender", h.gender, &n.gender)?;
        write_named(f, "alignment", h.alignment, &n.alignment)?;
        if h.kit != 0 {
            match &n.kit {
                Some(name) => writeln!(f, "  kit:         0x{:08x} ({name})", h.kit)?,
                None => writeln!(f, "  kit:         0x{:08x}", h.kit)?,
            }
        }
        match &n.animation {
            Some(name) => writeln!(f, "  animation:   0x{:04x} ({name})", h.animation)?,
            None => writeln!(f, "  animation:   0x{:04x}", h.animation)?,
        }

        let flags = creature_flags(h.flags);
        if !flags.is_empty() {
            writeln!(f, "  flags:       0x{:08x} ({})", h.flags, flags.join(", "))?;
        }
        let status = set_bits(h.status_flags, &STATUS_FLAGS);
        if !status.is_empty() {
            writeln!(
                f,
                "  status:      0x{:08x} ({})",
                h.status_flags,
                status.join(", ")
            )?;
        }

        writeln!(f, "  hp:          {}/{}", h.current_hp, h.max_hp)?;
        writeln!(
            f,
            "  level:       {}/{}/{}",
            h.level_first, h.level_second, h.level_third
        )?;
        writeln!(
            f,
            "  xp value:    {}  power: {}  gold: {}",
            h.xp_value, h.power_level, h.gold
        )?;
        writeln!(
            f,
            "  AC:          {} natural, {} effective (crush {}, missile {}, pierce {}, slash {})",
            h.ac_natural, h.ac_effective, h.ac_crushing, h.ac_missile, h.ac_piercing, h.ac_slashing
        )?;
        writeln!(f, "  THAC0:       {}  attacks: {}", h.thac0, h.num_attacks)?;
        writeln!(
            f,
            "  saves:       death {} wand {} poly {} breath {} spell {}",
            h.save_death, h.save_wands, h.save_polymorph, h.save_breath, h.save_spells
        )?;
        let resist = resistances(h);
        if !resist.is_empty() {
            writeln!(f, "  resist:      {resist}")?;
        }
        writeln!(
            f,
            "  abilities:   STR {}{}  DEX {}  CON {}  INT {}  WIS {}  CHA {}",
            h.strength,
            if h.strength_bonus != 0 {
                format!("/{:02}", h.strength_bonus)
            } else {
                String::new()
            },
            h.dexterity,
            h.constitution,
            h.intelligence,
            h.wisdom,
            h.charisma
        )?;
        let skills = thief_skills(h);
        if !skills.is_empty() {
            writeln!(f, "  skills:      {skills}")?;
        }
        let scripts = scripts(h);
        if !scripts.is_empty() {
            writeln!(f, "  scripts:     {scripts}")?;
        }
        if !h.dialog.is_empty() {
            writeln!(f, "  dialog:      {:?}", h.dialog)?;
        }

        if !self.known_spells.is_empty() {
            writeln!(f)?;
            writeln!(f, "  known spells: {}", self.known_spells.len())?;
            for s in &self.known_spells {
                writeln!(
                    f,
                    "    {:8}{} ({}, L{})",
                    s.resource,
                    s.name
                        .as_deref()
                        .map(|x| format!("  {x}"))
                        .unwrap_or_default(),
                    spell_type_name(s.spell_type),
                    s.level + 1
                )?;
            }
        }

        if !self.memorized_spells.is_empty() {
            writeln!(f)?;
            writeln!(f, "  memorized spells: {}", self.memorized_spells.len())?;
            for s in &self.memorized_spells {
                let state = if s.memorized & 1 == 0 {
                    "  [spent]"
                } else {
                    ""
                };
                writeln!(
                    f,
                    "    {:8}{}{}",
                    s.resource,
                    s.name
                        .as_deref()
                        .map(|x| format!("  {x}"))
                        .unwrap_or_default(),
                    state
                )?;
            }
        }

        if !self.items.is_empty() {
            writeln!(f)?;
            writeln!(f, "  items: {}", self.items.len())?;
            for it in &self.items {
                let qty = if it.quantity1 > 1 {
                    format!("  x{}", it.quantity1)
                } else {
                    String::new()
                };
                writeln!(
                    f,
                    "    {:8}{}{}",
                    it.resource,
                    it.name
                        .as_deref()
                        .map(|x| format!("  {x}"))
                        .unwrap_or_default(),
                    qty
                )?;
            }
            let equipped = self.equipped();
            if !equipped.is_empty() {
                writeln!(f, "  equipped:")?;
                for (slot, item) in equipped {
                    writeln!(f, "    {slot:14}{item}")?;
                }
            }
        }

        if !self.effects.is_empty() {
            writeln!(f)?;
            writeln!(f, "  effects: {}", self.effects.len())?;
            for (i, e) in self.effects.iter().enumerate() {
                writeln!(f)?;
                writeln!(f, "    effect {i}:")?;
                match e {
                    CreEffect::V1(feat) => feat.write_human(f, "      ")?,
                    CreEffect::V2(eff) => eff.write_human(f, "      ")?,
                }
            }
        }

        Ok(())
    }
}

impl Cre {
    /// Equipped items by slot name: the non-empty inventory slots that map to a
    /// named equipment position, resolved to the item in that slot.
    fn equipped(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for (i, &slot) in self.slots.iter().enumerate() {
            if slot == 0xffff {
                continue;
            }
            let Some(name) = SLOT_NAMES.get(i) else {
                continue;
            };
            let Some(item) = self.items.get(slot as usize) else {
                continue;
            };
            let label = item
                .name
                .as_deref()
                .map(|x| format!("{}  {x}", item.resource))
                .unwrap_or_else(|| item.resource.clone());
            out.push((*name, label));
        }
        out
    }
}

/// Render `name (value)` for an IDS-mapped field, or just the value when
/// unresolved. Only the value column carries the IDS number.
fn write_named(
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

/// A resolved name, or the raw strref when the TLK lookup failed.
fn name_or_strref(name: &Option<String>, strref: u32) -> String {
    match name {
        Some(name) => name.clone(),
        None => format!("strref {strref}"),
    }
}

/// Spell type for a CRE known/memorized spell (0=priest, 1=wizard, 2=innate).
fn spell_type_name(value: u16) -> &'static str {
    match value {
        0 => "priest",
        1 => "wizard",
        2 => "innate",
        _ => "?",
    }
}

/// Compact list of the creature's non-zero resistances.
fn resistances(h: &CreHeader) -> String {
    let mut parts = Vec::new();
    let mut push = |label: &str, v: u8| {
        if v != 0 {
            parts.push(format!("{label} {v}"));
        }
    };
    push("fire", h.resist_fire);
    push("cold", h.resist_cold);
    push("elec", h.resist_electricity);
    push("acid", h.resist_acid);
    push("magic", h.resist_magic);
    push("mfire", h.resist_magic_fire);
    push("mcold", h.resist_magic_cold);
    push("slash", h.resist_slashing);
    push("crush", h.resist_crushing);
    push("pierce", h.resist_piercing);
    push("missile", h.resist_missile);
    parts.join("  ")
}

/// Compact list of the creature's non-zero thief skills.
fn thief_skills(h: &CreHeader) -> String {
    let mut parts = Vec::new();
    let mut push = |label: &str, v: u8| {
        if v != 0 {
            parts.push(format!("{label} {v}"));
        }
    };
    push("lockpick", h.lockpicking);
    push("traps", h.find_traps);
    push("pickpocket", h.pick_pockets);
    push("stealth", h.move_silently);
    push("hide", h.hide_in_shadows);
    push("detect", h.detect_illusion);
    push("set-traps", h.set_traps);
    parts.join("  ")
}

/// Compact list of the creature's assigned scripts.
fn scripts(h: &CreHeader) -> String {
    let mut parts = Vec::new();
    for (label, script) in [
        ("override", &h.script_override),
        ("class", &h.script_class),
        ("race", &h.script_race),
        ("general", &h.script_general),
        ("default", &h.script_default),
    ] {
        if !script.is_empty() {
            parts.push(format!("{label}={script}"));
        }
    }
    parts.join("  ")
}

/// Names of the set creature flags (header 0x10). BG2:EE-relevant bits.
fn creature_flags(value: u32) -> Vec<&'static str> {
    set_bits(
        value,
        &[
            "show longname",
            "no corpse",
            "keep corpse",
            "class:fighter",
            "class:mage",
            "class:cleric",
            "class:thief",
            "class:druid",
            "class:ranger",
            "fallen paladin",
            "fallen ranger",
            "exportable",
            "hide injury",
            "quest critical",
            "moving areas",
            "been in party",
            "restore item",
            "unset restore",
            "",
            "",
            "prevent explode death",
            "",
            "no nightmare mods",
            "no tooltip",
            "walk:ea",
            "walk:general",
            "walk:race",
            "walk:class",
            "walk:specific",
            "walk:gender",
            "walk:alignment",
            "uninterruptable",
        ],
    )
}

/// STATE.IDS status-flag names (header 0x20), bit-indexed.
static STATUS_FLAGS: [&str; 32] = [
    "sleeping",
    "berserk",
    "panic",
    "stunned",
    "invisible",
    "helpless",
    "frozen death",
    "stone death",
    "exploding death",
    "flame death",
    "acid death",
    "dead",
    "silenced",
    "charmed",
    "poisoned",
    "hasted",
    "slowed",
    "infravision",
    "blind",
    "diseased",
    "feebleminded",
    "nondetection",
    "improved invisibility",
    "bless",
    "chant",
    "draw upon holy might",
    "luck",
    "aid",
    "bad chant",
    "blur",
    "mirror image",
    "confused",
];

/// Inventory slot names for the 38 indexable BG2 slots (header item-slots array).
static SLOT_NAMES: [&str; SLOT_COUNT] = [
    "helmet",
    "armor",
    "shield",
    "gloves",
    "left ring",
    "right ring",
    "amulet",
    "belt",
    "boots",
    "weapon 1",
    "weapon 2",
    "weapon 3",
    "weapon 4",
    "quiver 1",
    "quiver 2",
    "quiver 3",
    "quiver 4",
    "cloak",
    "quick item 1",
    "quick item 2",
    "quick item 3",
    "inventory 1",
    "inventory 2",
    "inventory 3",
    "inventory 4",
    "inventory 5",
    "inventory 6",
    "inventory 7",
    "inventory 8",
    "inventory 9",
    "inventory 10",
    "inventory 11",
    "inventory 12",
    "inventory 13",
    "inventory 14",
    "inventory 15",
    "inventory 16",
    "magic weapon",
];
