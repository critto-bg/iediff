//! GAM (game state) parser.
//!
//! Everything the engine keeps outside any single area: who is in the party,
//! which recruitable NPCs are still waiting and where, the GLOBAL variables,
//! the journal, and which creature the Find Familiar spell summons. A save game
//! is the obvious instance, but `baldur.gam` is the template a *new* game starts
//! from, so the format matters outside `save/` too — it is the only place the
//! starting area and the whole recruitable roster are written down.
//!
//! Like [ARE](super::are) it is a fixed header followed by header-located
//! sections, each with its own absolute offset and count. Party and non-party
//! NPCs share one 352-byte record; a save carries each one's whole CRE inside
//! the file, which is decoded here so its references are not lost.
//!
//! Two versions are read, `V1.1` and `V2.0`. They agree on the 180-byte header
//! and on every record size. `V2.0` fills the 76 bytes `V1.1` leaves unused, and
//! adds the sections they point at. BG:EE still ships BG1's `V1.1` `baldur.gam`
//! as its new-game template, though it writes `V2.0` saves. `V2.2` (IWD2) is a
//! different layout and is rejected.
//! Spec: `md/file_formats/ie_formats/gam_v2.0.md` and `gam_v1.1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::WMP_TYPE;
use crate::formats::are::ARE_TYPE;
use crate::formats::cre::{CRE_TYPE, Cre};
use crate::formats::fields::{set_bits, write_bitfield};
use crate::formats::itm::ITM_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag, parse_asciiz};

/// Resource type code for GAM files.
pub const GAM_TYPE: u16 = 0x03f5;

const GAM_SIGNATURE: &[u8; 4] = b"GAME";

/// On-disk size of one record in each section. An IWD `V1.1` NPC record is 32
/// bytes longer, for a voice-set path; the games this reads never write one.
const NPC_SIZE: usize = 0x160;
const INVENTORY_SIZE: usize = 0x14;
const VARIABLE_SIZE: usize = 0x54;
const JOURNAL_SIZE: usize = 0x0c;
const LOCATION_SIZE: usize = 0x0c;

/// The nine alignments the familiar table lists, in its own order — the same run
/// `ALIGNMEN.IDS` numbers 17-19, 33-35 and 49-51, abbreviated here.
const ALIGNMENTS: [&str; 9] = ["lg", "ln", "le", "ng", "tn", "ne", "cg", "cn", "ce"];

/// Familiar counts: nine alignments by nine caster levels.
const FAMILIAR_COUNTS: usize = ALIGNMENTS.len() * 9;

/// Weather bits (header 0x1e), bit `i` naming entry `i`.
const WEATHER_BITS: [&str; 9] = [
    "rain",
    "snow",
    "light rain",
    "medium rain",
    "light wind",
    "medium wind",
    "rare lightning",
    "lightning",
    "storm increasing",
];

/// A parsed GAM V2.0 file: the header plus its sections, with the names the
/// manager has resolved attached.
pub struct Gam {
    version: Version,
    header: GamHeader,
    party: Vec<Npc>,
    npcs: Vec<Npc>,
    inventory: Vec<PartyItem>,
    variables: Vec<Variable>,
    journal: Vec<JournalEntry>,
    /// Absent under `V1.1`, which has no familiar table, and under `V2.0` when
    /// the header's offset is the 0 / -1 "no familiars" sentinel.
    familiars: Option<Familiars>,
    stored: Vec<Location>,
    pocket: Vec<Location>,
}

impl Gam {
    /// Parse a GAM V2.0 resource. Each section is read from its header-declared
    /// offset and count; the familiar block has an offset but no count, since
    /// there is exactly one.
    pub fn parse(data: &[u8]) -> Result<Gam, GamError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != GAM_SIGNATURE {
            return Err(GamError::BadSignature(signature));
        }

        let tag = reader.tag_at(0x04, "version")?;
        let version = Version::parse(&tag).ok_or(GamError::BadVersion(tag))?;

        let header = GamHeader::parse(&reader, version)?;

        let party = Self::read_records(&reader, 0x20, 0x24, NPC_SIZE, Npc::parse_at)?;
        let npcs = Self::read_records(&reader, 0x30, 0x34, NPC_SIZE, Npc::parse_at)?;
        let inventory =
            Self::read_records(&reader, 0x28, 0x2c, INVENTORY_SIZE, PartyItem::parse_at)?;
        let variables = Self::read_records(&reader, 0x38, 0x3c, VARIABLE_SIZE, Variable::parse_at)?;
        // The journal pair is stored count-first, the reverse of every other
        // section's.
        let journal =
            Self::read_records(&reader, 0x50, 0x4c, JOURNAL_SIZE, JournalEntry::parse_at)?;
        // A `V1.1` header has none of these. Reading them anyway would take the
        // offsets out of its 76 unused bytes, which is where a junk reference
        // would come from.
        let (familiars, stored, pocket) = match version {
            Version::V1_1 => (None, Vec::new(), Vec::new()),
            Version::V2_0 => (
                section_offset(&reader, 0x68, "familiar offset")?
                    .map(|offset| Familiars::parse_at(&reader, offset))
                    .transpose()?,
                Self::read_records(&reader, 0x6c, 0x70, LOCATION_SIZE, Location::parse_at)?,
                Self::read_records(&reader, 0x78, 0x7c, LOCATION_SIZE, Location::parse_at)?,
            ),
        };

        Ok(Gam {
            version,
            header,
            party,
            npcs,
            inventory,
            variables,
            journal,
            familiars,
            stored,
            pocket,
        })
    }

    /// Every TLK strref this game state references: each character's "most
    /// powerful vanquished" and each journal entry's text, plus whatever an
    /// embedded creature carries.
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        let mut uses = Vec::new();
        for (label, roster) in [("party", &self.party), ("npc", &self.npcs)] {
            for (i, npc) in roster.iter().enumerate() {
                uses.extend(npc.strref_uses(&format!("{label}[{i}]")));
            }
        }

        for (i, entry) in self.journal.iter().enumerate() {
            uses.push(StrrefUse::new(entry.text, format!("journal[{i}].text")));
        }

        uses
    }

    /// Every resource this game state names, tagged with the type it names: the
    /// areas the party and the roster sit in, the quick and favourite spells,
    /// the party's shared inventory, and the familiar of each alignment.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let h = &self.header;
        let mut uses = vec![
            ResrefUse::new(ARE_TYPE, &h.main_area, "main_area"),
            ResrefUse::new(ARE_TYPE, &h.current_area, "current_area"),
        ];
        if let Some(v2) = &h.v2 {
            uses.push(ResrefUse::new(
                ARE_TYPE,
                &v2.encounter_area,
                "encounter_area",
            ));
            uses.push(ResrefUse::new(WMP_TYPE, &v2.worldmap, "worldmap"));
        }

        for (label, roster) in [("party", &self.party), ("npc", &self.npcs)] {
            for (i, npc) in roster.iter().enumerate() {
                uses.extend(npc.resref_uses(&format!("{label}[{i}]")));
            }
        }

        for (i, item) in self.inventory.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &item.resource,
                format!("inventory[{i}]"),
            ));
        }

        if let Some(familiars) = &self.familiars {
            for (alignment, resref) in ALIGNMENTS.iter().zip(&familiars.by_alignment) {
                uses.push(ResrefUse::new(
                    CRE_TYPE,
                    resref,
                    format!("familiar.{alignment}"),
                ));
            }
            for (i, resref) in familiars.extra.iter().enumerate() {
                uses.push(ResrefUse::new(
                    CRE_TYPE,
                    resref,
                    format!("familiar.extra[{i}]"),
                ));
            }
        }

        for (label, list) in [("stored", &self.stored), ("pocket", &self.pocket)] {
            for (i, location) in list.iter().enumerate() {
                uses.push(ResrefUse::new(
                    ARE_TYPE,
                    &location.area,
                    format!("{label}[{i}].area"),
                ));
            }
        }

        uses.retain(|u| !u.value.is_empty());
        uses
    }

    /// The party's shared inventory, for the manager to resolve their ITM names.
    pub fn inventory_mut(&mut self) -> &mut [PartyItem] {
        &mut self.inventory
    }

    /// The journal entries, for the manager to resolve their TLK text.
    pub fn journal_mut(&mut self) -> &mut [JournalEntry] {
        &mut self.journal
    }

    /// Write the whole game state as flat `key=value` lines: header fields bare,
    /// the sections under `party.<n>.` / `npc.<n>.` / `variable.<n>.` and so on.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        // The version leads, since it says which of the keys below to expect.
        writeln!(out, "version={}", self.version.tag())?;
        writeln!(out, "game_time={}", h.game_time)?;
        writeln!(out, "formation={}", h.formation)?;
        writeln!(out, "party_gold={}", h.party_gold)?;
        writeln!(out, "weather={}", h.weather)?;
        writeln!(out, "main_area={}", h.main_area)?;
        writeln!(out, "current_area={}", h.current_area)?;
        writeln!(out, "reputation={}", h.reputation)?;
        writeln!(out, "gui_flags={}", h.gui_flags)?;
        // 0x64 is two different fields, so it gets two different keys.
        match self.version {
            Version::V1_1 => writeln!(out, "save_version={}", h.progress)?,
            Version::V2_0 => writeln!(out, "progress={}", h.progress)?,
        }
        if let Some(v2) = &h.v2 {
            writeln!(out, "active_area_from={}", v2.active_area_from as i16)?;
            writeln!(out, "real_seconds={}", v2.real_seconds)?;
            writeln!(out, "zoom={}", v2.zoom)?;
            writeln!(out, "encounter_area={}", v2.encounter_area)?;
            writeln!(out, "worldmap={}", v2.worldmap)?;
            writeln!(out, "campaign={}", v2.campaign)?;
            writeln!(out, "familiar_owner={}", v2.familiar_owner)?;
            writeln!(out, "encounter_entry={}", v2.encounter_entry)?;
        }

        for (label, roster) in [("party", &self.party), ("npc", &self.npcs)] {
            for (i, npc) in roster.iter().enumerate() {
                npc.write_raw(out, &format!("{label}.{i}"))?;
            }
        }

        for (i, item) in self.inventory.iter().enumerate() {
            writeln!(out, "inventory.{i}.resource={}", item.resource)?;
            for (j, q) in item.quantity.iter().enumerate() {
                writeln!(out, "inventory.{i}.quantity_{}={q}", j + 1)?;
            }
            writeln!(out, "inventory.{i}.flags={}", item.flags)?;
        }

        for (i, v) in self.variables.iter().enumerate() {
            writeln!(out, "variable.{i}.name={}", v.name)?;
            writeln!(out, "variable.{i}.type={}", v.var_type)?;
            writeln!(out, "variable.{i}.value={}", v.value)?;
        }

        for (i, e) in self.journal.iter().enumerate() {
            writeln!(out, "journal.{i}.text={}", e.text as i32)?;
            writeln!(out, "journal.{i}.time={}", e.time)?;
            writeln!(out, "journal.{i}.chapter={}", e.chapter)?;
            writeln!(out, "journal.{i}.section={}", e.section)?;
        }

        if let Some(familiars) = &self.familiars {
            for (alignment, resref) in ALIGNMENTS.iter().zip(&familiars.by_alignment) {
                writeln!(out, "familiar.{alignment}={resref}")?;
            }
            // One line rather than 81: the counts are a flat level-by-alignment
            // grid whose only use here is sizing the resource table below.
            let counts: Vec<String> = familiars.counts.iter().map(u32::to_string).collect();
            writeln!(out, "familiar.counts={}", counts.join(","))?;
            for (i, resref) in familiars.extra.iter().enumerate() {
                writeln!(out, "familiar.extra.{i}={resref}")?;
            }
        }

        for (label, list) in [("stored", &self.stored), ("pocket", &self.pocket)] {
            for (i, location) in list.iter().enumerate() {
                writeln!(out, "{label}.{i}.area={}", location.area)?;
                writeln!(out, "{label}.{i}.x={}", location.x)?;
                writeln!(out, "{label}.{i}.y={}", location.y)?;
            }
        }

        Ok(())
    }

    /// Read a fixed-size record array from a header-declared section. An empty
    /// section is not read at all, since its offset is then meaningless — a real
    /// file leaves it 0 or pointing at the next section.
    fn read_records<T>(
        reader: &Reader,
        offset_at: usize,
        count_at: usize,
        record_size: usize,
        parse_at: impl Fn(&Reader, usize) -> Result<T, Truncated>,
    ) -> Result<Vec<T>, Truncated> {
        let count = reader.u32_at(count_at, "section count")? as usize;
        if count == 0 {
            return Ok(Vec::new());
        }
        let offset = reader.u32_at(offset_at, "section offset")? as usize;
        // Don't pre-size from an untrusted count; a bad offset errors on the
        // first read below rather than allocating gigabytes up front.
        let mut records = Vec::new();
        for i in 0..count {
            records.push(parse_at(reader, offset + i * record_size)?);
        }
        Ok(records)
    }
}

impl fmt::Display for Gam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "GAM {}", self.version.tag())?;
        writeln!(f, "  main area:   {}", h.main_area)?;
        if !h.current_area.is_empty() && h.current_area != h.main_area {
            writeln!(f, "  current:     {}", h.current_area)?;
        }
        if let Some(v2) = &h.v2 {
            if !v2.worldmap.is_empty() {
                writeln!(f, "  worldmap:    {}", v2.worldmap)?;
            }
            if !v2.campaign.is_empty() {
                writeln!(f, "  campaign:    {}", v2.campaign)?;
            }
        }

        // 300 time units make an hour; the hour count is what a reader wants.
        writeln!(f, "  game time:   {} ({}h)", h.game_time, h.game_time / 300)?;
        writeln!(f, "  reputation:  {:.1}", f64::from(h.reputation) / 10.0)?;
        writeln!(f, "  party gold:  {}", h.party_gold)?;
        match self.version {
            Version::V1_1 => writeln!(
                f,
                "  expansion:   {} ({})",
                h.progress,
                save_version_name(h.progress)
            )?,
            Version::V2_0 => writeln!(
                f,
                "  progress:    {} ({})",
                h.progress,
                progress_name(h.progress)
            )?,
        }

        if h.weather != 0 {
            write_bitfield(
                f,
                "  weather:     ",
                u32::from(h.weather),
                &set_bits(u32::from(h.weather), &WEATHER_BITS),
            )?;
        }

        if let Some(v2) = &h.v2
            && v2.zoom != 0
        {
            writeln!(f, "  zoom:        {}%", v2.zoom)?;
        }

        for (label, roster) in [("party", &self.party), ("npcs", &self.npcs)] {
            if roster.is_empty() {
                continue;
            }
            writeln!(f, "\n  {label}: ({})", roster.len())?;
            for npc in roster {
                npc.write_human(f)?;
            }
        }

        // `baldur.gam` reserves a hundred empty slots, so count what is
        // actually carried rather than how much room there is.
        let carried: Vec<&PartyItem> = self
            .inventory
            .iter()
            .filter(|item| !item.resource.is_empty())
            .collect();
        if !carried.is_empty() {
            writeln!(f, "\n  party inventory: ({})", carried.len())?;
            for item in carried {
                item.write_human(f)?;
            }
        }

        if !self.variables.is_empty() {
            writeln!(f, "\n  variables: ({})", self.variables.len())?;
            for v in &self.variables {
                writeln!(f, "    {} = {}", v.name, v.value)?;
            }
        }

        if !self.journal.is_empty() {
            writeln!(f, "\n  journal: ({})", self.journal.len())?;
            for e in &self.journal {
                e.write_human(f)?;
            }
        }

        if let Some(familiars) = &self.familiars {
            familiars.write_human(f)?;
        }

        for (label, list) in [
            ("stored locations", &self.stored),
            ("pocket plane", &self.pocket),
        ] {
            if list.is_empty() {
                continue;
            }
            writeln!(f, "\n  {label}: ({})", list.len())?;
            for location in list {
                writeln!(f, "    {} @{},{}", location.area, location.x, location.y)?;
            }
        }

        Ok(())
    }
}

/// Which GAME version a file is, and so which fields and sections it has.
#[derive(Clone, Copy)]
enum Version {
    /// BG1's, and the new-game template BG:EE still ships.
    V1_1,
    /// BG2's, and what every EE save game is written as.
    V2_0,
}

impl Version {
    fn parse(tag: &[u8; 4]) -> Option<Version> {
        match tag {
            b"V1.1" => Some(Version::V1_1),
            b"V2.0" => Some(Version::V2_0),
            _ => None,
        }
    }

    /// The tag as it reads in the file, for output that names the version.
    fn tag(self) -> &'static str {
        match self {
            Version::V1_1 => "V1.1",
            Version::V2_0 => "V2.0",
        }
    }
}

/// The 180-byte GAM header, in the part both versions share. Documented fields
/// are kept verbatim; the structural offset/count words are read locally in
/// [`Gam::parse`] and not stored, since the section counts are implicit in the
/// parsed vectors.
struct GamHeader {
    game_time: u32,
    formation: u16,
    party_gold: u32,
    weather: u16,
    main_area: String,
    reputation: u32,
    current_area: String,
    gui_flags: u32,
    /// Header 0x64, which the two versions read differently: under `V2.0` how
    /// far the game has got (see [`progress_name`]), under `V1.1` whether TotSC
    /// is installed (see [`save_version_name`]).
    progress: u32,
    /// The rest of the header, which only `V2.0` has.
    v2: Option<V2Header>,
}

impl GamHeader {
    fn parse(reader: &Reader, version: Version) -> Result<GamHeader, Truncated> {
        Ok(GamHeader {
            game_time: reader.u32_at(0x08, "game time")?,
            formation: reader.u16_at(0x0c, "formation")?,
            party_gold: reader.u32_at(0x18, "party gold")?,
            weather: reader.u16_at(0x1e, "weather")?,
            main_area: reader.resref_at(0x40, "main area")?,
            reputation: reader.u32_at(0x54, "reputation")?,
            current_area: reader.resref_at(0x58, "current area")?,
            gui_flags: reader.u32_at(0x60, "gui flags")?,
            progress: reader.u32_at(0x64, "loading progress")?,
            v2: match version {
                Version::V1_1 => None,
                Version::V2_0 => Some(V2Header::parse(reader)?),
            },
        })
    }
}

/// The header fields only a `V2.0` file has. A `V1.1` header holds a party count
/// at 0x1c instead, and leaves 0x68 to the end of the header unused, so reading
/// any of these from one would return whatever happens to sit there.
struct V2Header {
    /// Party slot whose area overrides `current_area`; -1 when none.
    active_area_from: u16,
    real_seconds: u32,
    zoom: u32,
    encounter_area: String,
    worldmap: String,
    campaign: String,
    familiar_owner: u32,
    encounter_entry: String,
}

impl V2Header {
    fn parse(reader: &Reader) -> Result<V2Header, Truncated> {
        Ok(V2Header {
            active_area_from: reader.u16_at(0x1c, "active area slot")?,
            real_seconds: reader.u32_at(0x74, "real seconds")?,
            zoom: reader.u32_at(0x80, "zoom level")?,
            encounter_area: reader.resref_at(0x84, "random encounter area")?,
            worldmap: reader.resref_at(0x8c, "worldmap")?,
            campaign: text_at(reader, 0x94, 8, "campaign")?,
            familiar_owner: reader.u32_at(0x9c, "familiar owner")?,
            encounter_entry: text_at(reader, 0xa0, 20, "random encounter entry")?,
        })
    }
}

/// One character the game state tracks (352 bytes), in the party or waiting to
/// join it. The same record serves both rosters.
struct Npc {
    party_order: u16,
    /// The creature's script name. In `baldur.gam` this is its resref; once the
    /// engine has spawned the NPC it overwrites the first letter with `*`.
    character: String,
    area: String,
    x: u16,
    y: u16,
    quick_spells: [String; 3],
    name: String,
    stats: Stats,
    /// The character's whole CRE, carried inside a save game. `None` when the
    /// record points at no data (as every entry of `baldur.gam` does) or when
    /// the embedded bytes do not parse.
    cre: Option<Cre>,
    /// Size of those embedded bytes, kept even when they did not parse so the
    /// human view can still say the CRE is there.
    cre_size: u32,
}

impl Npc {
    fn parse_at(reader: &Reader, base: usize) -> Result<Npc, Truncated> {
        let cre_offset = reader.u32_at(base + 0x04, "embedded creature offset")? as usize;
        let cre_size = reader.u32_at(base + 0x08, "embedded creature size")?;
        let cre = match cre_size {
            0 => None,
            size => {
                let bytes = reader.slice(cre_offset, size as usize, "embedded creature")?;
                Cre::parse(bytes).ok()
            }
        };

        Ok(Npc {
            party_order: reader.u16_at(base + 0x02, "party order")?,
            character: reader.resref_at(base + 0x0c, "character name")?,
            area: reader.resref_at(base + 0x18, "character area")?,
            x: reader.u16_at(base + 0x20, "character x")?,
            y: reader.u16_at(base + 0x22, "character y")?,
            quick_spells: [
                reader.resref_at(base + 0x9c, "quick spell 1")?,
                reader.resref_at(base + 0xa4, "quick spell 2")?,
                reader.resref_at(base + 0xac, "quick spell 3")?,
            ],
            name: text_at(reader, base + 0xc0, 32, "character display name")?,
            stats: Stats::parse_at(reader, base + 0xe4)?,
            cre,
            cre_size,
        })
    }

    /// Every strref this character carries, under `prefix`.
    fn strref_uses(&self, prefix: &str) -> Vec<StrrefUse> {
        let mut uses = vec![StrrefUse::new(
            self.stats.vanquished,
            format!("{prefix}.vanquished"),
        )];
        if let Some(cre) = &self.cre {
            let nested = format!("{prefix}.cre");
            uses.extend(cre.strref_uses().into_iter().map(|u| u.prefixed(&nested)));
        }
        uses
    }

    /// Every resource this character names, under `prefix`.
    fn resref_uses(&self, prefix: &str) -> Vec<ResrefUse<'_>> {
        let mut uses = vec![ResrefUse::new(
            ARE_TYPE,
            &self.area,
            format!("{prefix}.area"),
        )];
        // A roster entry with no CRE of its own is named only by its script
        // name, which BioWare's convention makes the creature's resref. A `*`
        // first letter is the engine's "already spawned" mark and names no file.
        if !self.character.starts_with('*') {
            uses.push(ResrefUse::new(
                CRE_TYPE,
                &self.character,
                format!("{prefix}.character"),
            ));
        }

        for (i, spell) in self.quick_spells.iter().enumerate() {
            uses.push(ResrefUse::new(
                SPL_TYPE,
                spell,
                format!("{prefix}.quick_spell[{i}]"),
            ));
        }

        for (i, spell) in self.stats.favourite_spells.iter().enumerate() {
            uses.push(ResrefUse::new(
                SPL_TYPE,
                spell,
                format!("{prefix}.favourite_spell[{i}]"),
            ));
        }

        for (i, weapon) in self.stats.favourite_weapons.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                weapon,
                format!("{prefix}.favourite_weapon[{i}]"),
            ));
        }

        if let Some(cre) = &self.cre {
            let nested = format!("{prefix}.cre");
            uses.extend(cre.resref_uses().into_iter().map(|u| u.prefixed(&nested)));
        }
        uses
    }

    fn write_raw(&self, out: &mut dyn Write, prefix: &str) -> io::Result<()> {
        writeln!(out, "{prefix}.character={}", self.character)?;
        writeln!(out, "{prefix}.party_order={}", self.party_order as i16)?;
        writeln!(out, "{prefix}.area={}", self.area)?;
        writeln!(out, "{prefix}.x={}", self.x)?;
        writeln!(out, "{prefix}.y={}", self.y)?;
        writeln!(out, "{prefix}.name={}", self.name)?;
        for (i, spell) in self.quick_spells.iter().enumerate() {
            writeln!(out, "{prefix}.quick_spell_{}={spell}", i + 1)?;
        }
        writeln!(out, "{prefix}.vanquished={}", self.stats.vanquished as i32)?;
        for (i, spell) in self.stats.favourite_spells.iter().enumerate() {
            writeln!(out, "{prefix}.favourite_spell_{}={spell}", i + 1)?;
        }
        for (i, weapon) in self.stats.favourite_weapons.iter().enumerate() {
            writeln!(out, "{prefix}.favourite_weapon_{}={weapon}", i + 1)?;
        }
        writeln!(out, "{prefix}.cre_size={}", self.cre_size)
    }

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "    {}", self.character)?;
        if !self.name.is_empty() {
            write!(f, " ({:?})", self.name)?;
        }
        if !self.area.is_empty() {
            write!(f, " @{} {},{}", self.area, self.x, self.y)?;
        }
        let spells: Vec<&str> = self
            .quick_spells
            .iter()
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .collect();
        if !spells.is_empty() {
            write!(f, " quick={}", spells.join("/"))?;
        }
        if self.cre_size != 0 {
            write!(f, " cre {} bytes", self.cre_size)?;
            if self.cre.is_none() {
                write!(f, " (unparsed)")?;
            }
        }
        writeln!(f)
    }
}

/// The 116-byte statistics block inside an [`Npc`]. Only the fields that name
/// something are kept: the rest are play-time counters.
struct Stats {
    vanquished: u32,
    favourite_spells: [String; 4],
    favourite_weapons: [String; 4],
}

impl Stats {
    fn parse_at(reader: &Reader, base: usize) -> Result<Stats, Truncated> {
        let mut favourite_spells: [String; 4] = Default::default();
        let mut favourite_weapons: [String; 4] = Default::default();
        for (i, spell) in favourite_spells.iter_mut().enumerate() {
            *spell = reader.resref_at(base + 0x24 + i * 8, "favourite spell")?;
        }
        for (i, weapon) in favourite_weapons.iter_mut().enumerate() {
            *weapon = reader.resref_at(base + 0x4c + i * 8, "favourite weapon")?;
        }
        Ok(Stats {
            vanquished: reader.u32_at(base, "most powerful vanquished")?,
            favourite_spells,
            favourite_weapons,
        })
    }
}

/// One item in the party's shared inventory (20-byte record, the same layout a
/// creature's inventory uses).
pub struct PartyItem {
    resource: String,
    quantity: [u16; 3],
    flags: u32,
    /// Item display name, resolved by the manager from the ITM header.
    name: Option<String>,
}

impl PartyItem {
    fn parse_at(reader: &Reader, base: usize) -> Result<PartyItem, Truncated> {
        Ok(PartyItem {
            resource: reader.resref_at(base, "inventory item")?,
            quantity: [
                reader.u16_at(base + 0x0a, "item quantity 1")?,
                reader.u16_at(base + 0x0c, "item quantity 2")?,
                reader.u16_at(base + 0x0e, "item quantity 3")?,
            ],
            flags: reader.u32_at(base + 0x10, "item flags")?,
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

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "    {}", self.resource)?;
        if let Some(name) = &self.name {
            write!(f, " ({name})")?;
        }
        let charges: Vec<String> = self
            .quantity
            .iter()
            .filter(|q| **q != 0)
            .map(u16::to_string)
            .collect();
        if !charges.is_empty() {
            write!(f, " x{}", charges.join("/"))?;
        }
        writeln!(f)
    }
}

/// A GLOBAL-scope variable. The engine only reads and writes the INT value
/// (0x54 bytes), as in an [ARE](super::are).
struct Variable {
    name: String,
    var_type: u16,
    value: i32,
}

impl Variable {
    fn parse_at(reader: &Reader, base: usize) -> Result<Variable, Truncated> {
        Ok(Variable {
            name: text_at(reader, base, 32, "variable name")?,
            var_type: reader.u16_at(base + 0x20, "variable type")?,
            value: reader.u32_at(base + 0x28, "variable value")? as i32,
        })
    }
}

/// One journal entry (12 bytes): the strref of its text, when it was added, and
/// which of the journal's tabs it belongs to.
pub struct JournalEntry {
    text: u32,
    time: u32,
    chapter: u8,
    section: u8,
    /// Entry text, resolved by the manager from the TLK.
    text_label: Option<String>,
}

impl JournalEntry {
    fn parse_at(reader: &Reader, base: usize) -> Result<JournalEntry, Truncated> {
        Ok(JournalEntry {
            text: reader.u32_at(base, "journal text")?,
            time: reader.u32_at(base + 0x04, "journal time")?,
            chapter: reader.u8_at(base + 0x08, "journal chapter")?,
            section: reader.u8_at(base + 0x0a, "journal section")?,
            text_label: None,
        })
    }

    /// The entry's text strref, for the manager to resolve against the TLK.
    pub fn strref(&self) -> u32 {
        self.text
    }

    /// Attach the resolved entry text.
    pub fn set_label(&mut self, label: Option<String>) {
        self.text_label = label;
    }

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "    ch{} [{}]", self.chapter, section_name(self.section))?;
        match &self.text_label {
            Some(text) => writeln!(f, " {text:?}"),
            None => writeln!(f, " {}", self.text as i32),
        }
    }
}

/// The Find Familiar table (400 bytes): which creature each caster alignment
/// summons, and how many of each the party has already had.
struct Familiars {
    by_alignment: [String; ALIGNMENTS.len()],
    /// Nine caster levels per alignment. Only their sum is used here — it sizes
    /// the resource table below — and the spec's own ordering of the grid
    /// disagrees with the alignment order above, so they are left unlabelled.
    counts: Vec<u32>,
    /// The creatures those counts account for, from the header's own offset.
    extra: Vec<String>,
}

impl Familiars {
    fn parse_at(reader: &Reader, base: usize) -> Result<Familiars, Truncated> {
        let mut by_alignment: [String; ALIGNMENTS.len()] = Default::default();
        for (i, resref) in by_alignment.iter_mut().enumerate() {
            *resref = reader.resref_at(base + i * 8, "familiar")?;
        }

        let mut counts = Vec::with_capacity(FAMILIAR_COUNTS);
        for i in 0..FAMILIAR_COUNTS {
            counts.push(reader.u32_at(base + 0x4c + i * 4, "familiar count")?);
        }

        // The table of summoned creatures is header-located and its length is
        // the counts' total, so an empty grid means there is nothing to read.
        // The counts are untrusted, so the total saturates rather than
        // overflowing; a bogus one runs off the end of the file and errors.
        let total = counts
            .iter()
            .fold(0u32, |total, n| total.saturating_add(*n));
        let mut extra = Vec::new();
        if let Some(offset) = section_offset(reader, 0x48, "familiar resource offset")? {
            for i in 0..total as usize {
                extra.push(reader.resref_at(offset + i * 8, "familiar resource")?);
            }
        }

        Ok(Familiars {
            by_alignment,
            counts,
            extra,
        })
    }

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named: Vec<String> = ALIGNMENTS
            .iter()
            .zip(&self.by_alignment)
            .filter(|(_, resref)| !resref.is_empty())
            .map(|(alignment, resref)| format!("{alignment}={resref}"))
            .collect();
        if named.is_empty() && self.extra.is_empty() {
            return Ok(());
        }
        writeln!(f, "\n  familiars:")?;
        if !named.is_empty() {
            writeln!(f, "    {}", named.join("  "))?;
        }
        if !self.extra.is_empty() {
            writeln!(f, "    summoned: {}", self.extra.join(", "))?;
        }
        Ok(())
    }
}

/// A remembered party position (12 bytes): the stored-locations and pocket-plane
/// sections share one layout.
struct Location {
    area: String,
    x: u16,
    y: u16,
}

impl Location {
    fn parse_at(reader: &Reader, base: usize) -> Result<Location, Truncated> {
        Ok(Location {
            area: reader.resref_at(base, "location area")?,
            x: reader.u16_at(base + 0x08, "location x")?,
            y: reader.u16_at(base + 0x0a, "location y")?,
        })
    }
}

/// Anything that can go wrong while parsing a GAM file.
#[derive(Debug)]
pub enum GamError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for GamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GamError::Truncated(e) => write!(f, "{e}"),
            GamError::BadSignature(found) => {
                write!(f, "not a GAM file: signature was {}", describe_tag(found))
            }
            GamError::BadVersion(found) => write!(
                f,
                "unsupported GAM version: {} (V1.1 and V2.0 are read)",
                describe_tag(found)
            ),
        }
    }
}

impl std::error::Error for GamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GamError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for GamError {
    fn from(e: Truncated) -> Self {
        GamError::Truncated(e)
    }
}

/// A header offset that points at a real section, or `None` for the 0 / -1
/// sentinels a file uses for "this section is absent".
fn section_offset(
    reader: &Reader,
    offset_at: usize,
    context: &'static str,
) -> Result<Option<usize>, Truncated> {
    match reader.u32_at(offset_at, context)? {
        0 | u32::MAX => Ok(None),
        offset => Ok(Some(offset as usize)),
    }
}

/// Read a fixed-width NUL-padded text field (a name, a campaign tag), trimming
/// its padding.
fn text_at(
    reader: &Reader,
    offset: usize,
    len: usize,
    context: &'static str,
) -> Result<String, Truncated> {
    Ok(parse_asciiz(reader.slice(offset, len, context)?))
}

/// Name the loading-progress code (header 0x64). The spec lists `3` twice; the
/// second is `4`, the value a game writes once `xnewarea.2da` has been applied.
fn progress_name(value: u32) -> &'static str {
    match value {
        0 => "BG1 XP limit",
        1 => "TotSC XP limit",
        2 => "SoA XP limit",
        3 => "xnewarea pending",
        4 => "xnewarea done",
        5 => "ToB",
        _ => "?",
    }
}

/// Name the `V1.1` save version (header 0x64), which says which expansion the
/// game was saved under. Only BG1 writes it.
fn save_version_name(value: u32) -> &'static str {
    match value {
        0 => "BG1 without TotSC",
        1 => "BG1 with TotSC",
        _ => "?",
    }
}

/// Name a journal entry's tab (journal 0x0a). No bit set marks a user note.
fn section_name(value: u8) -> &'static str {
    match value {
        0 => "note",
        1 => "quest",
        2 => "done",
        4 => "info",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER_LEN: usize = 0xb4;
    const FAMILIAR_SIZE: usize = 0x190;

    /// Where [`filled_gam`] puts each section, in the order it appends them.
    const PARTY_OFF: usize = HEADER_LEN;
    const NPC_OFF: usize = PARTY_OFF + NPC_SIZE;
    const INVENTORY_OFF: usize = NPC_OFF + NPC_SIZE;
    const VARIABLE_OFF: usize = INVENTORY_OFF + INVENTORY_SIZE;
    const JOURNAL_OFF: usize = VARIABLE_OFF + VARIABLE_SIZE;
    const FAMILIAR_OFF: usize = JOURNAL_OFF + JOURNAL_SIZE;
    const CRE_OFF: usize = FAMILIAR_OFF + FAMILIAR_SIZE;

    fn put_u32(d: &mut [u8], at: usize, value: u32) {
        d[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_resref(d: &mut [u8], at: usize, name: &str) {
        let b = name.as_bytes();
        assert!(b.len() <= 8, "resref {name:?} exceeds 8 bytes");
        d[at..at + b.len()].copy_from_slice(b);
    }

    /// A header of the given version with every section empty — the smallest
    /// file the parser takes.
    fn empty_gam(version: &[u8; 4]) -> Vec<u8> {
        let mut d = vec![0u8; HEADER_LEN];
        d[0x00..0x04].copy_from_slice(GAM_SIGNATURE);
        d[0x04..0x08].copy_from_slice(version);
        d
    }

    /// The smallest creature carrying one item, to ride inside the game state
    /// as a save's party member does.
    fn creature_with_item(item: &str) -> Vec<u8> {
        let mut d = vec![0u8; 0x2d4];
        d[0x00..0x04].copy_from_slice(b"CRE ");
        d[0x04..0x08].copy_from_slice(b"V1.0");
        let slots_off = d.len();
        d.resize(slots_off + 40 * 2, 0);
        put_u32(&mut d, 0x2b8, slots_off as u32);

        let items_off = d.len();
        d.resize(items_off + 20, 0);
        put_resref(&mut d, items_off, item);
        put_u32(&mut d, 0x2bc, items_off as u32);
        put_u32(&mut d, 0x2c0, 1);
        d
    }

    /// One record in every section: a party member with a creature of its own,
    /// a roster entry, a carried item, a variable, a journal entry, and the
    /// familiar table.
    fn filled_gam() -> Vec<u8> {
        let mut d = empty_gam(b"V2.0");
        put_resref(&mut d, 0x40, "AR0602"); // main area
        put_resref(&mut d, 0x58, "AR0603"); // current area
        put_resref(&mut d, 0x8c, "WORLDMAP");

        d.resize(NPC_OFF, 0);
        // `*`-marked: the engine has already spawned this one.
        put_resref(&mut d, PARTY_OFF + 0x0c, "*MOEN10");
        put_resref(&mut d, PARTY_OFF + 0x18, "AR0602");
        put_resref(&mut d, PARTY_OFF + 0x9c, "SPWI304");
        put_resref(&mut d, PARTY_OFF + 0xe4 + 0x24, "SPWI112"); // favourite spell
        put_resref(&mut d, PARTY_OFF + 0xe4 + 0x4c, "SW1H01"); // favourite weapon
        put_u32(&mut d, PARTY_OFF + 0xe4, 8000); // most powerful vanquished
        put_u32(&mut d, 0x20, PARTY_OFF as u32);
        put_u32(&mut d, 0x24, 1);

        d.resize(INVENTORY_OFF, 0);
        put_resref(&mut d, NPC_OFF + 0x0c, "YOSHI7");
        put_resref(&mut d, NPC_OFF + 0x18, "AR0603");
        put_u32(&mut d, 0x30, NPC_OFF as u32);
        put_u32(&mut d, 0x34, 1);

        d.resize(VARIABLE_OFF, 0);
        put_resref(&mut d, INVENTORY_OFF, "MISC01");
        put_u32(&mut d, 0x28, INVENTORY_OFF as u32);
        put_u32(&mut d, 0x2c, 1);

        d.resize(JOURNAL_OFF, 0);
        put_resref(&mut d, VARIABLE_OFF, "CHAPTER");
        put_u32(&mut d, VARIABLE_OFF + 0x28, 3);
        put_u32(&mut d, 0x38, VARIABLE_OFF as u32);
        put_u32(&mut d, 0x3c, 1);

        d.resize(FAMILIAR_OFF, 0);
        put_u32(&mut d, JOURNAL_OFF, 9000);
        put_u32(&mut d, 0x50, JOURNAL_OFF as u32);
        put_u32(&mut d, 0x4c, 1);

        d.resize(CRE_OFF, 0);
        put_resref(&mut d, FAMILIAR_OFF, "FAMPSD"); // lawful good
        put_u32(&mut d, 0x68, FAMILIAR_OFF as u32);

        let body = creature_with_item("SW1H01");
        put_u32(&mut d, PARTY_OFF + 0x04, CRE_OFF as u32);
        put_u32(&mut d, PARTY_OFF + 0x08, body.len() as u32);
        d.extend_from_slice(&body);
        d
    }

    /// A `V1.1` file: a header, one roster entry, and junk in every byte the two
    /// versions do not share.
    fn v1_1_gam() -> Vec<u8> {
        let mut d = empty_gam(b"V1.1");
        put_resref(&mut d, 0x40, "AR2626"); // main area
        // 0x48, and 0x68 to the end of the header, are unused under V1.1. Under
        // V2.0 they hold a familiar offset, the stored-location and pocket-plane
        // sections, and the EE tail, so junk here is junk a V2.0 read would take
        // seriously.
        d[0x48..0x4c].fill(b'A');
        d[0x68..HEADER_LEN].fill(b'A');

        d.resize(HEADER_LEN + NPC_SIZE, 0);
        put_resref(&mut d, HEADER_LEN + 0x0c, "KIVAN");
        put_resref(&mut d, HEADER_LEN + 0x18, "AR3200");
        put_u32(&mut d, 0x30, HEADER_LEN as u32);
        put_u32(&mut d, 0x34, 1);
        d
    }

    fn paths(uses: &[ResrefUse<'_>]) -> Vec<(u16, String, String)> {
        uses.iter()
            .map(|u| (u.kind, u.value.to_string(), u.path.clone()))
            .collect()
    }

    #[test]
    fn empty_game_has_no_sections() {
        let gam = Gam::parse(&empty_gam(b"V2.0")).expect("parse");
        assert!(gam.party.is_empty());
        assert!(gam.npcs.is_empty());
        assert!(gam.inventory.is_empty());
        assert!(gam.variables.is_empty());
        assert!(gam.journal.is_empty());
        assert!(gam.stored.is_empty());
        assert!(gam.pocket.is_empty());
        // A 0 familiar offset is the "no table" sentinel, not offset zero.
        assert!(gam.familiars.is_none());
    }

    #[test]
    fn reads_every_section() {
        let gam = Gam::parse(&filled_gam()).expect("parse");
        assert_eq!(gam.header.main_area, "AR0602");
        let v2 = gam.header.v2.as_ref().expect("a V2.0 header tail");
        assert_eq!(v2.worldmap, "WORLDMAP");
        assert_eq!(gam.party.len(), 1);
        assert_eq!(gam.npcs.len(), 1);
        assert_eq!(gam.inventory.len(), 1);
        assert_eq!(gam.journal.len(), 1);
        assert_eq!(gam.variables.len(), 1);
        assert_eq!(gam.variables[0].name, "CHAPTER");
        assert_eq!(gam.variables[0].value, 3);
        let familiars = gam.familiars.expect("familiar table");
        assert_eq!(familiars.by_alignment[0], "FAMPSD");
    }

    #[test]
    fn a_roster_entry_names_its_creature_but_a_spawned_one_does_not() {
        let gam = Gam::parse(&filled_gam()).expect("parse");
        let uses = paths(&gam.resref_uses());
        // The waiting NPC is named by its script name; the party member's has
        // been overwritten with `*` and names no file.
        assert!(uses.contains(&(CRE_TYPE, "YOSHI7".into(), "npc[0].character".into())));
        assert!(
            !uses.iter().any(|(_, value, _)| value.starts_with('*')),
            "a `*`-marked script name is not a resref"
        );
    }

    #[test]
    fn resref_uses_types_every_named_field() {
        let gam = Gam::parse(&filled_gam()).expect("parse");
        assert_eq!(
            paths(&gam.resref_uses()),
            vec![
                (ARE_TYPE, "AR0602".into(), "main_area".into()),
                (ARE_TYPE, "AR0603".into(), "current_area".into()),
                (WMP_TYPE, "WORLDMAP".into(), "worldmap".into()),
                (ARE_TYPE, "AR0602".into(), "party[0].area".into()),
                (SPL_TYPE, "SPWI304".into(), "party[0].quick_spell[0]".into()),
                (
                    SPL_TYPE,
                    "SPWI112".into(),
                    "party[0].favourite_spell[0]".into()
                ),
                (
                    ITM_TYPE,
                    "SW1H01".into(),
                    "party[0].favourite_weapon[0]".into()
                ),
                // The creature carried inside the file is decoded, not skipped.
                (ITM_TYPE, "SW1H01".into(), "party[0].cre.item[0]".into()),
                (ARE_TYPE, "AR0603".into(), "npc[0].area".into()),
                (CRE_TYPE, "YOSHI7".into(), "npc[0].character".into()),
                (ITM_TYPE, "MISC01".into(), "inventory[0]".into()),
                (CRE_TYPE, "FAMPSD".into(), "familiar.lg".into()),
            ]
        );
    }

    #[test]
    fn strref_uses_covers_the_journal_and_each_character() {
        let gam = Gam::parse(&filled_gam()).expect("parse");
        let uses: Vec<(u32, String)> = gam
            .strref_uses()
            .into_iter()
            .map(|u| (u.value, u.path))
            .collect();
        assert!(uses.contains(&(8000, "party[0].vanquished".to_string())));
        assert!(uses.contains(&(9000, "journal[0].text".to_string())));
    }

    #[test]
    fn a_v1_1_file_reads_its_roster_and_none_of_the_v2_fields() {
        let d = v1_1_gam();
        let gam = Gam::parse(&d).expect("parse");
        assert!(matches!(gam.version, Version::V1_1));
        assert!(gam.header.v2.is_none());
        assert!(gam.familiars.is_none());
        assert!(gam.stored.is_empty());
        assert!(gam.pocket.is_empty());
        assert_eq!(
            paths(&gam.resref_uses()),
            vec![
                (ARE_TYPE, "AR2626".into(), "main_area".into()),
                (ARE_TYPE, "AR3200".into(), "npc[0].area".into()),
                (CRE_TYPE, "KIVAN".into(), "npc[0].character".into()),
            ]
        );

        // The same bytes read as V2.0 take the junk for a familiar offset and
        // run off the end. That is what the version gate prevents.
        let mut as_v2 = d.clone();
        as_v2[0x04..0x08].copy_from_slice(b"V2.0");
        assert!(matches!(Gam::parse(&as_v2), Err(GamError::Truncated(_))));
    }

    #[test]
    fn rejects_bad_signature() {
        let mut d = empty_gam(b"V2.0");
        d[0] = b'X';
        assert!(matches!(Gam::parse(&d), Err(GamError::BadSignature(_))));
    }

    #[test]
    fn rejects_a_version_with_another_layout() {
        // V2.2 is IWD2's and V9.0 is IWD's; neither shares this layout.
        for version in [b"V2.2", b"V9.0"] {
            let d = empty_gam(version);
            assert!(matches!(Gam::parse(&d), Err(GamError::BadVersion(_))));
        }
    }

    #[test]
    fn an_absurd_familiar_count_errors_rather_than_overflowing() {
        // The nine-by-nine count grid is untrusted and its total sizes a table
        // read from the file, so the sum must saturate rather than wrap.
        let mut d = filled_gam();
        let past_the_end = d.len() as u32;
        for i in 0..2 {
            put_u32(&mut d, FAMILIAR_OFF + 0x4c + i * 4, u32::MAX);
        }
        put_u32(&mut d, 0x48, past_the_end);
        assert!(matches!(Gam::parse(&d), Err(GamError::Truncated(_))));
    }

    #[test]
    fn rejects_a_section_that_runs_past_the_end() {
        // Claims one roster entry, but the file ends at the header.
        let mut d = empty_gam(b"V2.0");
        put_u32(&mut d, 0x30, HEADER_LEN as u32);
        put_u32(&mut d, 0x34, 1);
        assert!(matches!(Gam::parse(&d), Err(GamError::Truncated(_))));
    }
}
