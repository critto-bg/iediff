//! ARE (area) parser.
//!
//! An area's content: its actors, triggers/travel regions, containers and the
//! items in them, spawn points, ambient sounds, doors, background animations,
//! automap notes and area variables. Like [STO](super::sto) it is a fixed
//! header followed by header-located sections — but many of them, each with its
//! own absolute offset and count in the header, and stored in arbitrary order.
//!
//! Phase 1 decodes the content-bearing sections; the vertex pool, explored
//! bitmask, tiled objects, projectile traps, song entries and rest
//! interruptions are only surfaced as counts (see the header summary).
//!
//! Only `V1.0` is handled. All offsets are absolute from the file start.
//! Spec: `md/file_formats/ie_formats/are_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::bcs::BCS_TYPE;
use crate::formats::cre::CRE_TYPE;
use crate::formats::dlg::DLG_TYPE;
use crate::formats::fields::{set_bits, set_flags, write_bitfield};
use crate::formats::itm::ITM_TYPE;
use crate::formats::pro::PRO_TYPE;
use crate::formats::{BAM_TYPE, WAV_TYPE, WED_TYPE};
use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag, parse_asciiz};

/// Resource type code for ARE files.
pub const ARE_TYPE: u16 = 0x03f2;

const ARE_SIGNATURE: &[u8; 4] = b"AREA";
const ARE_VERSION: &[u8; 4] = b"V1.0";

/// On-disk size of one record in each parsed section.
const ACTOR_SIZE: usize = 0x110;
const REGION_SIZE: usize = 0xc4;
const ENTRANCE_SIZE: usize = 0x68;
const CONTAINER_SIZE: usize = 0xc0;
const ITEM_SIZE: usize = 0x14;
const SPAWN_SIZE: usize = 0xc8;
const AMBIENT_SIZE: usize = 0xd4;
const VARIABLE_SIZE: usize = 0x54;
const DOOR_SIZE: usize = 0xc8;
const ANIM_SIZE: usize = 0x4c;
const AUTOMAP_SIZE: usize = 0x34;
const VERTEX_SIZE: usize = 0x04;
const TILED_SIZE: usize = 0x68;
const PTRAP_SIZE: usize = 0x1c;
const SONG_SIZE: usize = 0x90;
const REST_SIZE: usize = 0xe4;

/// Names of the six actor script slots, in parse order.
const SCRIPT_SLOTS: [&str; 6] = [
    "override", "general", "class", "race", "default", "specific",
];

/// The hours (schedule bits 0-23) that count as daytime; the rest are night.
/// Matches the day/night split the engine and editors use.
const DAY_HOURS: u32 = 0x1f_ffc0;
const NIGHT_HOURS: u32 = 0x00ff_ffff & !DAY_HOURS;

/// Area type bits (header 0x48), BG2/BGEE meanings.
const AREA_TYPE_BITS: [&str; 8] = [
    "outdoor",
    "day/night",
    "weather",
    "city",
    "forest",
    "dungeon",
    "extended night",
    "rest indoors",
];

/// Door flags (door 0x28).
const DOOR_FLAGS: [(u32, &str); 9] = [
    (0, "open"),
    (1, "locked"),
    (4, "broken"),
    (5, "can't close"),
    (6, "linked"),
    (7, "hidden"),
    (8, "found"),
    (9, "no LOS block"),
    (10, "remove key"),
];

/// A parsed ARE file: the header plus the decoded content sections, with names
/// the manager has resolved attached.
pub struct Are {
    header: AreHeader,
    actors: Vec<Actor>,
    regions: Vec<Region>,
    entrances: Vec<Entrance>,
    containers: Vec<Container>,
    items: Vec<Item>,
    spawns: Vec<Spawn>,
    ambients: Vec<Ambient>,
    variables: Vec<Variable>,
    doors: Vec<Door>,
    animations: Vec<Animation>,
    automap: Vec<Automap>,
    vertices: Vec<(u16, u16)>,
    tiled_objects: Vec<TiledObject>,
    proj_traps: Vec<ProjectileTrap>,
    song: Option<Song>,
    rest: Option<RestInterruption>,
}

impl Are {
    /// Parse an ARE V1.0 resource. Each section is read from its
    /// header-declared offset and count; the sections may appear in any order.
    pub fn parse(data: &[u8]) -> Result<Are, AreError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != ARE_SIGNATURE {
            return Err(AreError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != ARE_VERSION {
            return Err(AreError::BadVersion(version));
        }

        let header = AreHeader::parse(&reader)?;

        // (offset word, count word, count is dword) per section. Counts are a
        // mix of word and dword fields, so each is read at its own width.
        let actors = Self::read_records(
            &reader,
            reader.u32_at(0x54, "actor offset")? as usize,
            reader.u16_at(0x58, "actor count")? as usize,
            ACTOR_SIZE,
            Actor::parse_at,
        )?;
        let regions = Self::read_records(
            &reader,
            reader.u32_at(0x5c, "region offset")? as usize,
            reader.u16_at(0x5a, "region count")? as usize,
            REGION_SIZE,
            Region::parse_at,
        )?;
        let spawns = Self::read_records(
            &reader,
            reader.u32_at(0x60, "spawn offset")? as usize,
            reader.u32_at(0x64, "spawn count")? as usize,
            SPAWN_SIZE,
            Spawn::parse_at,
        )?;
        let entrances = Self::read_records(
            &reader,
            reader.u32_at(0x68, "entrance offset")? as usize,
            reader.u32_at(0x6c, "entrance count")? as usize,
            ENTRANCE_SIZE,
            Entrance::parse_at,
        )?;
        let containers = Self::read_records(
            &reader,
            reader.u32_at(0x70, "container offset")? as usize,
            reader.u16_at(0x74, "container count")? as usize,
            CONTAINER_SIZE,
            Container::parse_at,
        )?;
        let items = Self::read_records(
            &reader,
            reader.u32_at(0x78, "item offset")? as usize,
            reader.u16_at(0x76, "item count")? as usize,
            ITEM_SIZE,
            Item::parse_at,
        )?;
        let ambients = Self::read_records(
            &reader,
            reader.u32_at(0x84, "ambient offset")? as usize,
            reader.u16_at(0x82, "ambient count")? as usize,
            AMBIENT_SIZE,
            Ambient::parse_at,
        )?;
        let variables = Self::read_records(
            &reader,
            reader.u32_at(0x88, "variable offset")? as usize,
            reader.u32_at(0x8c, "variable count")? as usize,
            VARIABLE_SIZE,
            Variable::parse_at,
        )?;
        let doors = Self::read_records(
            &reader,
            reader.u32_at(0xa8, "door offset")? as usize,
            reader.u32_at(0xa4, "door count")? as usize,
            DOOR_SIZE,
            Door::parse_at,
        )?;
        let animations = Self::read_records(
            &reader,
            reader.u32_at(0xb0, "animation offset")? as usize,
            reader.u32_at(0xac, "animation count")? as usize,
            ANIM_SIZE,
            Animation::parse_at,
        )?;
        let automap = Self::read_records(
            &reader,
            reader.u32_at(0xc4, "automap offset")? as usize,
            reader.u32_at(0xc8, "automap count")? as usize,
            AUTOMAP_SIZE,
            Automap::parse_at,
        )?;

        // The vertex pool: a flat array of (x, y) words that regions, containers
        // and doors index into.
        let vertices = Self::read_records(
            &reader,
            reader.u32_at(0x7c, "vertex offset")? as usize,
            reader.u16_at(0x80, "vertex count")? as usize,
            VERTEX_SIZE,
            |r, base| Ok((r.u16_at(base, "vertex x")?, r.u16_at(base + 2, "vertex y")?)),
        )?;
        let tiled_objects = Self::read_records(
            &reader,
            reader.u32_at(0xb8, "tiled object offset")? as usize,
            reader.u32_at(0xb4, "tiled object count")? as usize,
            TILED_SIZE,
            TiledObject::parse_at,
        )?;
        let proj_traps = Self::read_records(
            &reader,
            reader.u32_at(0xcc, "projectile trap offset")? as usize,
            reader.u32_at(0xd0, "projectile trap count")? as usize,
            PTRAP_SIZE,
            ProjectileTrap::parse_at,
        )?;

        // Song entries and rest interruptions are each a single fixed struct
        // located by offset, with no count. Parse when the offset points at a
        // struct that fits within the file; otherwise treat as absent.
        let song = Self::read_struct(&reader, data.len(), 0xbc, SONG_SIZE, Song::parse_at)?;
        let rest = Self::read_struct(
            &reader,
            data.len(),
            0xc0,
            REST_SIZE,
            RestInterruption::parse_at,
        )?
        .filter(RestInterruption::is_populated);

        Ok(Are {
            header,
            actors,
            regions,
            entrances,
            containers,
            items,
            spawns,
            ambients,
            variables,
            doors,
            animations,
            automap,
            vertices,
            tiled_objects,
            proj_traps,
            song,
            rest,
        })
    }

    /// The actors, for the manager to resolve their CRE names.
    pub fn actors_mut(&mut self) -> &mut [Actor] {
        &mut self.actors
    }

    /// The flat item pool, for the manager to resolve their ITM names.
    pub fn items_mut(&mut self) -> &mut [Item] {
        &mut self.items
    }

    /// The spawn points, for the manager to resolve their creature names.
    pub fn spawns_mut(&mut self) -> &mut [Spawn] {
        &mut self.spawns
    }

    /// The regions, for the manager to resolve their info-text strrefs.
    pub fn regions_mut(&mut self) -> &mut [Region] {
        &mut self.regions
    }

    /// The doors, for the manager to resolve their speaker strrefs.
    pub fn doors_mut(&mut self) -> &mut [Door] {
        &mut self.doors
    }

    /// The automap notes, for the manager to resolve their text strrefs.
    pub fn automap_mut(&mut self) -> &mut [Automap] {
        &mut self.automap
    }

    /// The projectile traps, for the manager to resolve their missile symbols.
    pub fn proj_traps_mut(&mut self) -> &mut [ProjectileTrap] {
        &mut self.proj_traps
    }

    /// The rest-interruption table, if present, for the manager to resolve its
    /// creature names.
    pub fn rest_mut(&mut self) -> Option<&mut RestInterruption> {
        self.rest.as_mut()
    }

    /// Every TLK strref this area references, tagged with its field: region info
    /// text and speaker name, container and door lockpick strings, door dialog
    /// speakers, and automap note text.
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        let mut uses = Vec::new();
        for (i, r) in self.regions.iter().enumerate() {
            uses.push(StrrefUse::new(
                r.info_text,
                format!("region[{i}].info_text"),
            ));
            uses.push(StrrefUse::new(
                r.speaker_name,
                format!("region[{i}].speaker_name"),
            ));
        }
        for (i, c) in self.containers.iter().enumerate() {
            uses.push(StrrefUse::new(
                c.lockpick_string,
                format!("container[{i}].lockpick_string"),
            ));
        }
        for (i, d) in self.doors.iter().enumerate() {
            uses.push(StrrefUse::new(
                d.lockpick_string,
                format!("door[{i}].lockpick_string"),
            ));
            uses.push(StrrefUse::new(
                d.dialog_speaker,
                format!("door[{i}].dialog_speaker"),
            ));
        }
        for (i, n) in self.automap.iter().enumerate() {
            uses.push(StrrefUse::new(n.strref(), format!("automap[{i}].text")));
        }
        uses
    }

    /// Every resource this area names, tagged with the type it names: the
    /// tilemap and neighbouring areas, the area and per-record scripts, the CRE
    /// of every actor / spawn / rest creature, the ITM of every pooled item and
    /// key, the DLG of every actor and door, and the sounds and animations.
    ///
    /// Note that a container's contents are *not* named on the container: every
    /// item in the area lives in one flat pool, which the containers index into.
    /// A hit therefore reads `item[12]`, not `container[3].item[0]`.
    ///
    /// Skipped as area-internal labels rather than resource references: each
    /// record's `name`, a door's `door_id` and a tiled object's `tile_id` (both
    /// name entries inside the WED), and a region's `entrance` (an entrance name
    /// within the destination area).
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let h = &self.header;
        let mut uses = vec![
            ResrefUse::new(WED_TYPE, &h.wed, "wed"),
            ResrefUse::new(BCS_TYPE, &h.script, "script"),
            ResrefUse::new(ARE_TYPE, &h.north.area, "north"),
            ResrefUse::new(ARE_TYPE, &h.east.area, "east"),
            ResrefUse::new(ARE_TYPE, &h.south.area, "south"),
            ResrefUse::new(ARE_TYPE, &h.west.area, "west"),
        ];

        for (i, a) in self.actors.iter().enumerate() {
            uses.push(ResrefUse::new(
                CRE_TYPE,
                &a.cre_file,
                format!("actor[{i}].cre"),
            ));
            uses.push(ResrefUse::new(
                DLG_TYPE,
                &a.dialog,
                format!("actor[{i}].dialog"),
            ));
            for (j, script) in a.scripts.iter().enumerate() {
                uses.push(ResrefUse::new(
                    BCS_TYPE,
                    script,
                    format!("actor[{i}].script[{j}]"),
                ));
            }
        }

        for (i, r) in self.regions.iter().enumerate() {
            uses.push(ResrefUse::new(
                ARE_TYPE,
                &r.destination,
                format!("region[{i}].destination"),
            ));
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &r.key_item,
                format!("region[{i}].key_item"),
            ));
            uses.push(ResrefUse::new(
                BCS_TYPE,
                &r.script,
                format!("region[{i}].script"),
            ));
        }

        for (i, c) in self.containers.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &c.key_item,
                format!("container[{i}].key_item"),
            ));
        }

        for (i, item) in self.items.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &item.resource,
                format!("item[{i}]"),
            ));
        }

        for (i, s) in self.spawns.iter().enumerate() {
            for (j, c) in s.creatures.iter().enumerate() {
                uses.push(ResrefUse::new(
                    CRE_TYPE,
                    &c.resource,
                    format!("spawn[{i}].creature[{j}]"),
                ));
            }
        }

        for (i, a) in self.ambients.iter().enumerate() {
            for (j, sound) in a.sounds.iter().enumerate() {
                uses.push(ResrefUse::new(
                    WAV_TYPE,
                    sound,
                    format!("ambient[{i}].sound[{j}]"),
                ));
            }
        }

        for (i, d) in self.doors.iter().enumerate() {
            uses.push(ResrefUse::new(
                ITM_TYPE,
                &d.key_item,
                format!("door[{i}].key_item"),
            ));
            uses.push(ResrefUse::new(
                BCS_TYPE,
                &d.script,
                format!("door[{i}].script"),
            ));
            uses.push(ResrefUse::new(
                DLG_TYPE,
                &d.dialog,
                format!("door[{i}].dialog"),
            ));
            uses.push(ResrefUse::new(
                WAV_TYPE,
                &d.open_sound,
                format!("door[{i}].open_sound"),
            ));
            uses.push(ResrefUse::new(
                WAV_TYPE,
                &d.close_sound,
                format!("door[{i}].close_sound"),
            ));
        }

        for (i, a) in self.animations.iter().enumerate() {
            // "BGEE: BAM/WBM/PVRZ, Others: BAM" — BAM is the likely reading on
            // this engine, not the guaranteed one.
            uses.push(
                ResrefUse::new(BAM_TYPE, &a.resource, format!("animation[{i}].resource")).soft(),
            );
        }

        for (i, t) in self.proj_traps.iter().enumerate() {
            uses.push(ResrefUse::new(
                PRO_TYPE,
                &t.projectile,
                format!("proj_trap[{i}].projectile"),
            ));
        }

        if let Some(song) = &self.song {
            for (i, sound) in song.day_ambient.iter().enumerate() {
                uses.push(ResrefUse::new(
                    WAV_TYPE,
                    sound,
                    format!("song.day_ambient[{i}]"),
                ));
            }
            for (i, sound) in song.night_ambient.iter().enumerate() {
                uses.push(ResrefUse::new(
                    WAV_TYPE,
                    sound,
                    format!("song.night_ambient[{i}]"),
                ));
            }
        }

        if let Some(rest) = &self.rest {
            for (i, c) in rest.creatures.iter().enumerate() {
                uses.push(ResrefUse::new(
                    CRE_TYPE,
                    &c.resource,
                    format!("rest.creature[{i}]"),
                ));
            }
        }

        uses.retain(|u| !u.value.is_empty());
        uses
    }

    /// Write the whole area as flat `key=value` lines: header fields bare, each
    /// section under `<section>.<n>.`.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "wed={}", h.wed)?;
        writeln!(out, "last_saved={}", h.last_saved)?;
        writeln!(out, "flags={}", h.flags)?;
        writeln!(out, "area_type={}", h.area_type)?;
        writeln!(out, "rain={}", h.rain)?;
        writeln!(out, "snow={}", h.snow)?;
        writeln!(out, "fog={}", h.fog)?;
        writeln!(out, "lightning={}", h.lightning)?;
        writeln!(out, "script={}", h.script)?;
        for (label, n) in [
            ("north", &h.north),
            ("east", &h.east),
            ("south", &h.south),
            ("west", &h.west),
        ] {
            writeln!(out, "{label}={}", n.area)?;
            writeln!(out, "{label}_flags={}", n.flags)?;
        }

        for (i, a) in self.actors.iter().enumerate() {
            writeln!(out, "actor.{i}.name={}", a.name)?;
            writeln!(out, "actor.{i}.x={}", a.x)?;
            writeln!(out, "actor.{i}.y={}", a.y)?;
            writeln!(out, "actor.{i}.flags={}", a.flags)?;
            writeln!(out, "actor.{i}.orientation={}", a.orientation)?;
            writeln!(out, "actor.{i}.animation={}", a.animation)?;
            writeln!(
                out,
                "actor.{i}.appearance_schedule={}",
                a.appearance_schedule
            )?;
            writeln!(out, "actor.{i}.removal_timer={}", a.removal_timer)?;
            writeln!(out, "actor.{i}.dest_x={}", a.dest_x)?;
            writeln!(out, "actor.{i}.dest_y={}", a.dest_y)?;
            writeln!(out, "actor.{i}.cre={}", a.cre_file)?;
            writeln!(out, "actor.{i}.dialog={}", a.dialog)?;
            for (slot, script) in SCRIPT_SLOTS.iter().zip(&a.scripts) {
                writeln!(out, "actor.{i}.script_{slot}={script}")?;
            }
        }

        for (i, r) in self.regions.iter().enumerate() {
            writeln!(out, "region.{i}.name={}", r.name)?;
            writeln!(out, "region.{i}.type={}", r.region_type)?;
            writeln!(out, "region.{i}.vertices={}", r.vertex_count)?;
            writeln!(out, "region.{i}.trigger_value={}", r.trigger_value)?;
            writeln!(out, "region.{i}.destination={}", r.destination)?;
            writeln!(out, "region.{i}.entrance={}", r.entrance)?;
            writeln!(out, "region.{i}.flags={}", r.flags)?;
            writeln!(out, "region.{i}.info_text={}", strref_raw(r.info_text))?;
            writeln!(out, "region.{i}.trapped={}", r.trapped)?;
            writeln!(out, "region.{i}.key_item={}", r.key_item)?;
            writeln!(out, "region.{i}.script={}", r.script)?;
            writeln!(
                out,
                "region.{i}.speaker_name={}",
                strref_raw(r.speaker_name)
            )?;
        }

        for (i, e) in self.entrances.iter().enumerate() {
            writeln!(out, "entrance.{i}.name={}", e.name)?;
            writeln!(out, "entrance.{i}.x={}", e.x)?;
            writeln!(out, "entrance.{i}.y={}", e.y)?;
            writeln!(out, "entrance.{i}.orientation={}", e.orientation)?;
        }

        for (i, c) in self.containers.iter().enumerate() {
            writeln!(out, "container.{i}.name={}", c.name)?;
            writeln!(out, "container.{i}.x={}", c.x)?;
            writeln!(out, "container.{i}.y={}", c.y)?;
            writeln!(out, "container.{i}.type={}", c.container_type)?;
            writeln!(out, "container.{i}.lock={}", c.lock_difficulty)?;
            writeln!(out, "container.{i}.flags={}", c.flags)?;
            writeln!(out, "container.{i}.trapped={}", c.trapped)?;
            writeln!(out, "container.{i}.item_index={}", c.item_index)?;
            writeln!(out, "container.{i}.item_count={}", c.item_count)?;
            writeln!(out, "container.{i}.key_item={}", c.key_item)?;
            writeln!(
                out,
                "container.{i}.lockpick_string={}",
                strref_raw(c.lockpick_string)
            )?;
        }

        for (i, it) in self.items.iter().enumerate() {
            writeln!(out, "item.{i}.resource={}", it.resource)?;
            for (j, q) in it.quantity.iter().enumerate() {
                writeln!(out, "item.{i}.quantity_{}={q}", j + 1)?;
            }
            writeln!(out, "item.{i}.flags={}", it.flags)?;
        }

        for (i, s) in self.spawns.iter().enumerate() {
            writeln!(out, "spawn.{i}.name={}", s.name)?;
            writeln!(out, "spawn.{i}.x={}", s.x)?;
            writeln!(out, "spawn.{i}.y={}", s.y)?;
            writeln!(out, "spawn.{i}.frequency={}", s.frequency)?;
            writeln!(out, "spawn.{i}.enabled={}", s.enabled)?;
            writeln!(out, "spawn.{i}.max_spawn={}", s.max_spawn)?;
            writeln!(out, "spawn.{i}.probability_day={}", s.prob_day)?;
            writeln!(out, "spawn.{i}.probability_night={}", s.prob_night)?;
            writeln!(out, "spawn.{i}.method={}", s.method)?;
            writeln!(out, "spawn.{i}.base_number={}", s.base_number)?;
            writeln!(out, "spawn.{i}.removal_timer={}", s.removal_timer)?;
            writeln!(
                out,
                "spawn.{i}.appearance_schedule={}",
                s.appearance_schedule
            )?;
            for (j, c) in s.creatures.iter().enumerate() {
                writeln!(out, "spawn.{i}.creature.{j}={}", c.resource)?;
            }
        }

        for (i, a) in self.ambients.iter().enumerate() {
            writeln!(out, "ambient.{i}.name={}", a.name)?;
            writeln!(out, "ambient.{i}.x={}", a.x)?;
            writeln!(out, "ambient.{i}.y={}", a.y)?;
            writeln!(out, "ambient.{i}.radius={}", a.radius)?;
            writeln!(out, "ambient.{i}.volume={}", a.volume)?;
            writeln!(out, "ambient.{i}.flags={}", a.flags)?;
            for (j, sound) in a.sounds.iter().enumerate() {
                writeln!(out, "ambient.{i}.sound.{j}={sound}")?;
            }
        }

        for (i, v) in self.variables.iter().enumerate() {
            writeln!(out, "variable.{i}.name={}", v.name)?;
            writeln!(out, "variable.{i}.type={}", v.var_type)?;
            writeln!(out, "variable.{i}.value={}", v.value)?;
        }

        for (i, d) in self.doors.iter().enumerate() {
            writeln!(out, "door.{i}.name={}", d.name)?;
            writeln!(out, "door.{i}.id={}", d.door_id)?;
            writeln!(out, "door.{i}.flags={}", d.flags)?;
            writeln!(out, "door.{i}.hp={}", d.hp)?;
            writeln!(out, "door.{i}.ac={}", d.ac)?;
            writeln!(out, "door.{i}.open_sound={}", d.open_sound)?;
            writeln!(out, "door.{i}.close_sound={}", d.close_sound)?;
            writeln!(out, "door.{i}.trapped={}", d.trapped)?;
            writeln!(out, "door.{i}.key_item={}", d.key_item)?;
            writeln!(out, "door.{i}.script={}", d.script)?;
            writeln!(out, "door.{i}.lock={}", d.lock_difficulty)?;
            writeln!(
                out,
                "door.{i}.lockpick_string={}",
                strref_raw(d.lockpick_string)
            )?;
            writeln!(
                out,
                "door.{i}.dialog_speaker={}",
                strref_raw(d.dialog_speaker)
            )?;
            writeln!(out, "door.{i}.dialog={}", d.dialog)?;
        }

        for (i, a) in self.animations.iter().enumerate() {
            writeln!(out, "animation.{i}.name={}", a.name)?;
            writeln!(out, "animation.{i}.x={}", a.x)?;
            writeln!(out, "animation.{i}.y={}", a.y)?;
            writeln!(out, "animation.{i}.resource={}", a.resource)?;
            writeln!(out, "animation.{i}.flags={}", a.flags)?;
            writeln!(out, "animation.{i}.transparency={}", a.transparency)?;
        }

        for (i, n) in self.automap.iter().enumerate() {
            writeln!(out, "automap.{i}.x={}", n.x)?;
            writeln!(out, "automap.{i}.y={}", n.y)?;
            writeln!(out, "automap.{i}.text={}", strref_raw(n.text))?;
            writeln!(out, "automap.{i}.color={}", n.color)?;
        }

        for (i, t) in self.tiled_objects.iter().enumerate() {
            writeln!(out, "tiled.{i}.name={}", t.name)?;
            writeln!(out, "tiled.{i}.id={}", t.tile_id)?;
            writeln!(out, "tiled.{i}.flags={}", t.flags)?;
        }

        for (i, t) in self.proj_traps.iter().enumerate() {
            writeln!(out, "trap.{i}.projectile={}", t.projectile)?;
            writeln!(out, "trap.{i}.missile={}", t.missile)?;
            writeln!(out, "trap.{i}.x={}", t.x)?;
            writeln!(out, "trap.{i}.y={}", t.y)?;
            writeln!(out, "trap.{i}.owner={}", t.owner)?;
        }

        if let Some(s) = &self.song {
            writeln!(out, "song.day={}", s.day)?;
            writeln!(out, "song.night={}", s.night)?;
            writeln!(out, "song.win={}", s.win)?;
            writeln!(out, "song.battle={}", s.battle)?;
            writeln!(out, "song.lose={}", s.lose)?;
            writeln!(out, "song.day_ambient_1={}", s.day_ambient[0])?;
            writeln!(out, "song.day_ambient_2={}", s.day_ambient[1])?;
            writeln!(out, "song.day_volume={}", s.day_volume)?;
            writeln!(out, "song.night_ambient_1={}", s.night_ambient[0])?;
            writeln!(out, "song.night_ambient_2={}", s.night_ambient[1])?;
            writeln!(out, "song.night_volume={}", s.night_volume)?;
        }

        if let Some(r) = &self.rest {
            writeln!(out, "rest.name={}", r.name)?;
            writeln!(out, "rest.difficulty={}", r.difficulty)?;
            writeln!(out, "rest.max_spawn={}", r.max_spawn)?;
            writeln!(out, "rest.enabled={}", r.enabled)?;
            writeln!(out, "rest.probability_day={}", r.prob_day)?;
            writeln!(out, "rest.probability_night={}", r.prob_night)?;
            for (j, c) in r.creatures.iter().enumerate() {
                writeln!(out, "rest.creature.{j}={}", c.resource)?;
            }
        }

        for (i, (x, y)) in self.vertices.iter().enumerate() {
            writeln!(out, "vertex.{i}={x},{y}")?;
        }
        writeln!(out, "explored_size={}", self.header.explored_size)?;

        Ok(())
    }

    /// Read a single fixed struct located by an offset word at `offset_at`, with
    /// no count. Returns `None` when the offset is zero or the struct would run
    /// past the end of the file (the section is absent).
    fn read_struct<T>(
        reader: &Reader,
        len: usize,
        offset_at: usize,
        size: usize,
        parse_at: impl Fn(&Reader, usize) -> Result<T, Truncated>,
    ) -> Result<Option<T>, Truncated> {
        let offset = reader.u32_at(offset_at, "struct offset")? as usize;
        if offset == 0 || offset.saturating_add(size) > len {
            return Ok(None);
        }
        Ok(Some(parse_at(reader, offset)?))
    }

    /// Read a fixed-size record array from a header-declared section. A zero
    /// count yields an empty vector without touching `offset`.
    fn read_records<T>(
        reader: &Reader,
        offset: usize,
        count: usize,
        record_size: usize,
        parse_at: impl Fn(&Reader, usize) -> Result<T, Truncated>,
    ) -> Result<Vec<T>, Truncated> {
        // Don't pre-size from an untrusted count; a bad offset errors on the
        // first read below rather than allocating gigabytes up front.
        let mut records = Vec::new();
        for i in 0..count {
            records.push(parse_at(reader, offset + i * record_size)?);
        }
        Ok(records)
    }

    /// The items belonging to a container, sliced out of the flat item pool by
    /// its `item_index`/`item_count`. Bounds are clamped so a bad index yields
    /// an empty slice rather than panicking.
    fn container_items(&self, c: &Container) -> &[Item] {
        let start = c.item_index as usize;
        let end = start.saturating_add(c.item_count as usize);
        self.items
            .get(start..end.min(self.items.len()))
            .unwrap_or(&[])
    }
}

impl fmt::Display for Are {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "ARE V1.0")?;
        if !h.wed.is_empty() {
            writeln!(f, "  wed:         {}", h.wed)?;
        }
        if !h.script.is_empty() {
            writeln!(f, "  script:      {}", h.script)?;
        }
        if h.flags != 0 {
            write_bitfield(f, "  flags:       ", h.flags, &area_flags(h.flags))?;
        }
        if h.area_type != 0 {
            write_bitfield(
                f,
                "  type:        ",
                h.area_type as u32,
                &set_bits(h.area_type as u32, &AREA_TYPE_BITS),
            )?;
        }
        let weather = [
            ("rain", h.rain),
            ("snow", h.snow),
            ("fog", h.fog),
            ("lightning", h.lightning),
        ];
        if weather.iter().any(|(_, p)| *p != 0) {
            let parts: Vec<String> = weather
                .iter()
                .filter(|(_, p)| *p != 0)
                .map(|(label, p)| format!("{label} {p}%"))
                .collect();
            writeln!(f, "  weather:     {}", parts.join("  "))?;
        }
        let neighbours = [
            ("N", &h.north),
            ("E", &h.east),
            ("S", &h.south),
            ("W", &h.west),
        ];
        if neighbours.iter().any(|(_, n)| !n.area.is_empty()) {
            let parts: Vec<String> = neighbours
                .iter()
                .filter(|(_, n)| !n.area.is_empty())
                .map(|(dir, n)| format!("{dir}={}", n.area))
                .collect();
            writeln!(f, "  neighbours:  {}", parts.join("  "))?;
        }

        if !self.actors.is_empty() {
            writeln!(f, "\n  actors: ({})", self.actors.len())?;
            for a in &self.actors {
                write!(f, "    {}", a.name)?;
                if let Some(name) = &a.name_text {
                    write!(f, " ({name})")?;
                }
                write!(f, " @{},{}  cre={}", a.x, a.y, a.cre_file)?;
                if let Some(anim) = &a.animation_name {
                    write!(f, " anim={anim}")?;
                }
                if let Some(sched) = schedule_label(a.appearance_schedule) {
                    write!(f, " [{sched}]")?;
                }
                if !a.dialog.is_empty() {
                    write!(f, " dialog={}", a.dialog)?;
                }
                writeln!(f)?;
            }
        }

        if !self.regions.is_empty() {
            writeln!(f, "\n  regions: ({})", self.regions.len())?;
            for r in &self.regions {
                write!(f, "    {} [{}]", r.name, region_type_name(r.region_type))?;
                if !r.destination.is_empty() {
                    write!(f, " -> {}", r.destination)?;
                    if !r.entrance.is_empty() {
                        write!(f, "({})", r.entrance)?;
                    }
                }
                if let Some(info) = &r.info_label {
                    write!(f, " {info:?}")?;
                }
                if r.trapped != 0 {
                    write!(f, " trapped")?;
                }
                if !r.script.is_empty() {
                    write!(f, " script={}", r.script)?;
                }
                writeln!(f)?;
            }
        }

        if !self.entrances.is_empty() {
            writeln!(f, "\n  entrances: ({})", self.entrances.len())?;
            for e in &self.entrances {
                writeln!(f, "    {} @{},{}", e.name, e.x, e.y)?;
            }
        }

        if !self.containers.is_empty() {
            writeln!(f, "\n  containers: ({})", self.containers.len())?;
            for c in &self.containers {
                write!(
                    f,
                    "    {} [{}]",
                    c.name,
                    container_type_name(c.container_type)
                )?;
                if c.lock_difficulty != 0 {
                    write!(f, " lock {}", c.lock_difficulty)?;
                }
                if c.trapped != 0 {
                    write!(f, " trapped")?;
                }
                writeln!(f)?;
                for item in self.container_items(c) {
                    write!(f, "      {}", item.resource)?;
                    if let Some(name) = &item.name {
                        write!(f, " ({name})")?;
                    }
                    let charges: Vec<String> = item
                        .quantity
                        .iter()
                        .filter(|q| **q != 0)
                        .map(|q| q.to_string())
                        .collect();
                    if !charges.is_empty() {
                        write!(f, " x{}", charges.join("/"))?;
                    }
                    writeln!(f)?;
                }
            }
        }

        if !self.spawns.is_empty() {
            writeln!(f, "\n  spawns: ({})", self.spawns.len())?;
            for s in &self.spawns {
                write!(f, "    {} @{},{}", s.name, s.x, s.y)?;
                if s.enabled == 0 {
                    write!(f, " (inactive)")?;
                }
                writeln!(f)?;
                for c in &s.creatures {
                    match &c.name {
                        Some(name) => writeln!(f, "      {} ({name})", c.resource)?,
                        None => writeln!(f, "      {}", c.resource)?,
                    }
                }
            }
        }

        if !self.ambients.is_empty() {
            writeln!(f, "\n  ambients: ({})", self.ambients.len())?;
            for a in &self.ambients {
                writeln!(f, "    {} {}", a.name, a.sounds.join(" "))?;
            }
        }

        if !self.doors.is_empty() {
            writeln!(f, "\n  doors: ({})", self.doors.len())?;
            for d in &self.doors {
                write!(f, "    {}", d.name)?;
                let flags = set_flags(d.flags, &DOOR_FLAGS);
                if !flags.is_empty() {
                    write!(f, " [{}]", flags.join(", "))?;
                }
                if !d.script.is_empty() {
                    write!(f, " script={}", d.script)?;
                }
                // A speaker only means anything alongside a dialog.
                if !d.dialog.is_empty() {
                    write!(f, " dialog={}", d.dialog)?;
                    if let Some(speaker) = &d.speaker_label {
                        write!(f, " speaker={speaker:?}")?;
                    }
                }
                writeln!(f)?;
            }
        }

        if !self.animations.is_empty() {
            writeln!(f, "\n  animations: ({})", self.animations.len())?;
            for a in &self.animations {
                writeln!(f, "    {} {} @{},{}", a.name, a.resource, a.x, a.y)?;
            }
        }

        if !self.automap.is_empty() {
            writeln!(f, "\n  automap notes: ({})", self.automap.len())?;
            for n in &self.automap {
                match &n.text_label {
                    Some(text) => writeln!(f, "    @{},{} {text:?}", n.x, n.y)?,
                    None => writeln!(f, "    @{},{} {}", n.x, n.y, n.text)?,
                }
            }
        }

        if !self.variables.is_empty() {
            writeln!(f, "\n  variables: ({})", self.variables.len())?;
            for v in &self.variables {
                writeln!(f, "    {} = {}", v.name, v.value)?;
            }
        }

        if !self.tiled_objects.is_empty() {
            writeln!(f, "\n  tiled objects: ({})", self.tiled_objects.len())?;
            for t in &self.tiled_objects {
                writeln!(f, "    {} id={}", t.name, t.tile_id)?;
            }
        }

        if !self.proj_traps.is_empty() {
            writeln!(f, "\n  projectile traps: ({})", self.proj_traps.len())?;
            for t in &self.proj_traps {
                write!(f, "    {} @{},{}", t.projectile, t.x, t.y)?;
                if let Some(name) = &t.missile_name {
                    write!(f, " ({name})")?;
                }
                writeln!(f)?;
            }
        }

        if let Some(s) = &self.song {
            let songs = [
                ("day", s.day),
                ("night", s.night),
                ("battle", s.battle),
                ("win", s.win),
                ("lose", s.lose),
            ];
            let parts: Vec<String> = songs
                .iter()
                .map(|(label, n)| format!("{label}={}", song_ref(*n)))
                .collect();
            writeln!(f, "\n  songs: {}", parts.join("  "))?;
            for (when, ambient, volume) in [
                ("day", &s.day_ambient, s.day_volume),
                ("night", &s.night_ambient, s.night_volume),
            ] {
                let sounds: Vec<&str> = ambient
                    .iter()
                    .filter(|a| !a.is_empty())
                    .map(String::as_str)
                    .collect();
                if !sounds.is_empty() {
                    writeln!(f, "    ambient({when}): {} {volume}%", sounds.join(" "))?;
                }
            }
        }

        if let Some(r) = &self.rest {
            writeln!(f, "\n  rest interruptions: {}", r.name)?;
            writeln!(
                f,
                "    difficulty {}  max {}  prob day {}% night {}%{}",
                r.difficulty,
                r.max_spawn,
                r.prob_day,
                r.prob_night,
                if r.enabled == 0 { "  (inactive)" } else { "" }
            )?;
            for c in &r.creatures {
                match &c.name {
                    Some(name) => writeln!(f, "    {} ({name})", c.resource)?,
                    None => writeln!(f, "    {}", c.resource)?,
                }
            }
        }

        // The vertex pool and explored bitmask are geometry/bitmap blobs; report
        // their extent rather than dumping coordinates.
        if !self.vertices.is_empty() || h.explored_size != 0 {
            writeln!(
                f,
                "\n  geometry: {} vertices, explored {} bytes",
                self.vertices.len(),
                h.explored_size
            )?;
        }

        Ok(())
    }
}

/// The ARE V1.0 header. Section offset/count words are read locally in
/// [`Are::parse`] and not stored; counts of the sections we don't yet decode are
/// kept so the summary can report them.
struct AreHeader {
    wed: String,
    last_saved: u32,
    flags: u32,
    area_type: u16,
    rain: u16,
    snow: u16,
    fog: u16,
    lightning: u16,
    script: String,
    north: Neighbor,
    east: Neighbor,
    south: Neighbor,
    west: Neighbor,
    /// Size in bytes of the explored bitmask — an opaque per-cell bitmap that is
    /// reported but not decoded.
    explored_size: u32,
}

impl AreHeader {
    fn parse(reader: &Reader) -> Result<AreHeader, Truncated> {
        Ok(AreHeader {
            wed: reader.resref_at(0x08, "area WED")?,
            last_saved: reader.u32_at(0x10, "last saved")?,
            flags: reader.u32_at(0x14, "area flags")?,
            north: Neighbor::parse_at(reader, 0x18, 0x20)?,
            east: Neighbor::parse_at(reader, 0x24, 0x2c)?,
            south: Neighbor::parse_at(reader, 0x30, 0x38)?,
            west: Neighbor::parse_at(reader, 0x3c, 0x44)?,
            area_type: reader.u16_at(0x48, "area type")?,
            rain: reader.u16_at(0x4a, "rain probability")?,
            snow: reader.u16_at(0x4c, "snow probability")?,
            fog: reader.u16_at(0x4e, "fog probability")?,
            lightning: reader.u16_at(0x50, "lightning probability")?,
            script: reader.resref_at(0x94, "area script")?,
            explored_size: reader.u32_at(0x9c, "explored bitmask size")?,
        })
    }
}

/// A neighbouring area link (the four edges of the map).
struct Neighbor {
    area: String,
    flags: u32,
}

impl Neighbor {
    fn parse_at(reader: &Reader, area_at: usize, flags_at: usize) -> Result<Neighbor, Truncated> {
        Ok(Neighbor {
            area: reader.resref_at(area_at, "neighbour area")?,
            flags: reader.u32_at(flags_at, "neighbour flags")?,
        })
    }
}

/// A creature/party actor placed in the area (0x110 bytes).
pub struct Actor {
    name: String,
    x: u16,
    y: u16,
    dest_x: u16,
    dest_y: u16,
    flags: u32,
    animation: u32,
    orientation: u16,
    removal_timer: i32,
    appearance_schedule: u32,
    dialog: String,
    scripts: [String; 6],
    cre_file: String,
    /// CRE display name, resolved by the manager.
    name_text: Option<String>,
    /// animate.ids symbol for `animation`, resolved by the manager.
    animation_name: Option<String>,
}

impl Actor {
    fn parse_at(reader: &Reader, base: usize) -> Result<Actor, Truncated> {
        Ok(Actor {
            name: name_at(reader, base, "actor name")?,
            x: reader.u16_at(base + 0x20, "actor x")?,
            y: reader.u16_at(base + 0x22, "actor y")?,
            dest_x: reader.u16_at(base + 0x24, "actor dest x")?,
            dest_y: reader.u16_at(base + 0x26, "actor dest y")?,
            flags: reader.u32_at(base + 0x28, "actor flags")?,
            animation: reader.u32_at(base + 0x30, "actor animation")?,
            orientation: reader.u16_at(base + 0x34, "actor orientation")?,
            removal_timer: reader.u32_at(base + 0x38, "actor removal timer")? as i32,
            appearance_schedule: reader.u32_at(base + 0x40, "actor appearance schedule")?,
            dialog: reader.resref_at(base + 0x48, "actor dialog")?,
            scripts: [
                reader.resref_at(base + 0x50, "actor script override")?,
                reader.resref_at(base + 0x58, "actor script general")?,
                reader.resref_at(base + 0x60, "actor script class")?,
                reader.resref_at(base + 0x68, "actor script race")?,
                reader.resref_at(base + 0x70, "actor script default")?,
                reader.resref_at(base + 0x78, "actor script specific")?,
            ],
            cre_file: reader.resref_at(base + 0x80, "actor CRE file")?,
            name_text: None,
            animation_name: None,
        })
    }

    /// The CRE resref, for the manager to resolve a display name.
    pub fn resource(&self) -> &str {
        &self.cre_file
    }

    /// Attach the resolved CRE display name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name_text = name;
    }

    /// The animate.ids value, for the manager to resolve a symbol.
    pub fn animation(&self) -> u32 {
        self.animation
    }

    /// Attach the resolved animate.ids symbol.
    pub fn set_animation_name(&mut self, name: Option<String>) {
        self.animation_name = name;
    }
}

/// A region: a proximity trigger, info point, or travel region (0xC4 bytes).
pub struct Region {
    name: String,
    region_type: u16,
    vertex_count: u16,
    trigger_value: u32,
    destination: String,
    entrance: String,
    flags: u32,
    info_text: u32,
    trapped: u16,
    key_item: String,
    script: String,
    /// Speaker name for a region that starts dialogue (PST/PSTEE only; `-1`
    /// elsewhere). A strref, but not resolved for the human view.
    speaker_name: u32,
    /// Info-point text, resolved by the manager from the TLK.
    info_label: Option<String>,
}

impl Region {
    fn parse_at(reader: &Reader, base: usize) -> Result<Region, Truncated> {
        Ok(Region {
            name: name_at(reader, base, "region name")?,
            region_type: reader.u16_at(base + 0x20, "region type")?,
            vertex_count: reader.u16_at(base + 0x2a, "region vertex count")?,
            trigger_value: reader.u32_at(base + 0x30, "region trigger value")?,
            destination: reader.resref_at(base + 0x38, "region destination")?,
            entrance: name_at(reader, base + 0x40, "region entrance")?,
            flags: reader.u32_at(base + 0x60, "region flags")?,
            info_text: reader.u32_at(base + 0x64, "region info text")?,
            trapped: reader.u16_at(base + 0x6c, "region trapped")?,
            key_item: reader.resref_at(base + 0x74, "region key item")?,
            script: reader.resref_at(base + 0x7c, "region script")?,
            speaker_name: reader.u32_at(base + 0xb8, "region speaker name")?,
            info_label: None,
        })
    }

    /// The info-point text strref, for the manager to resolve against the TLK.
    pub fn strref(&self) -> u32 {
        self.info_text
    }

    /// Attach the resolved info-point text.
    pub fn set_label(&mut self, label: Option<String>) {
        self.info_label = label;
    }
}

/// An entrance/exit marker where travel drops the party (0x68 bytes).
pub struct Entrance {
    name: String,
    x: u16,
    y: u16,
    orientation: u16,
}

impl Entrance {
    fn parse_at(reader: &Reader, base: usize) -> Result<Entrance, Truncated> {
        Ok(Entrance {
            name: name_at(reader, base, "entrance name")?,
            x: reader.u16_at(base + 0x20, "entrance x")?,
            y: reader.u16_at(base + 0x22, "entrance y")?,
            orientation: reader.u16_at(base + 0x24, "entrance orientation")?,
        })
    }
}

/// A container (chest, bag, pile, …). Its items live in the flat item pool,
/// indexed by `item_index`/`item_count` (0xC0 bytes).
pub struct Container {
    name: String,
    x: u16,
    y: u16,
    container_type: u16,
    lock_difficulty: u16,
    flags: u32,
    trapped: u16,
    item_index: u32,
    item_count: u32,
    key_item: String,
    /// Message shown when the lock resists (a strref); not resolved for the
    /// human view.
    lockpick_string: u32,
}

impl Container {
    fn parse_at(reader: &Reader, base: usize) -> Result<Container, Truncated> {
        Ok(Container {
            name: name_at(reader, base, "container name")?,
            x: reader.u16_at(base + 0x20, "container x")?,
            y: reader.u16_at(base + 0x22, "container y")?,
            container_type: reader.u16_at(base + 0x24, "container type")?,
            lock_difficulty: reader.u16_at(base + 0x26, "container lock")?,
            flags: reader.u32_at(base + 0x28, "container flags")?,
            trapped: reader.u16_at(base + 0x30, "container trapped")?,
            item_index: reader.u32_at(base + 0x40, "container item index")?,
            item_count: reader.u32_at(base + 0x44, "container item count")?,
            key_item: reader.resref_at(base + 0x78, "container key item")?,
            lockpick_string: reader.u32_at(base + 0x84, "container lockpick string")?,
        })
    }
}

/// One item lying in the area's flat item pool (0x14 bytes).
pub struct Item {
    resource: String,
    quantity: [u16; 3],
    flags: u32,
    /// Item display name, resolved by the manager from the ITM header.
    name: Option<String>,
}

impl Item {
    fn parse_at(reader: &Reader, base: usize) -> Result<Item, Truncated> {
        Ok(Item {
            resource: reader.resref_at(base, "item resref")?,
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
}

/// One creature slot of a spawn point.
pub struct SpawnCreature {
    resource: String,
    /// CRE display name, resolved by the manager (empty for spawngrp entries).
    name: Option<String>,
}

impl SpawnCreature {
    /// The creature resref, for the manager to resolve a display name.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Attach the resolved CRE display name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }
}

/// A spawn point: creatures periodically generated in the area (0xC8 bytes).
pub struct Spawn {
    name: String,
    x: u16,
    y: u16,
    creatures: Vec<SpawnCreature>,
    base_number: u16,
    frequency: u16,
    method: u16,
    removal_timer: i32,
    max_spawn: u16,
    enabled: u16,
    appearance_schedule: u32,
    prob_day: u16,
    prob_night: u16,
}

impl Spawn {
    fn parse_at(reader: &Reader, base: usize) -> Result<Spawn, Truncated> {
        // Ten fixed creature slots; keep only the populated ones.
        let mut creatures = Vec::new();
        for i in 0..10 {
            let resource = reader.resref_at(base + 0x24 + i * 8, "spawn creature")?;
            if !resource.is_empty() {
                creatures.push(SpawnCreature {
                    resource,
                    name: None,
                });
            }
        }
        Ok(Spawn {
            name: name_at(reader, base, "spawn name")?,
            x: reader.u16_at(base + 0x20, "spawn x")?,
            y: reader.u16_at(base + 0x22, "spawn y")?,
            creatures,
            base_number: reader.u16_at(base + 0x76, "spawn base number")?,
            frequency: reader.u16_at(base + 0x78, "spawn frequency")?,
            method: reader.u16_at(base + 0x7a, "spawn method")?,
            removal_timer: reader.u32_at(base + 0x7c, "spawn removal timer")? as i32,
            max_spawn: reader.u16_at(base + 0x84, "spawn max")?,
            enabled: reader.u16_at(base + 0x86, "spawn enabled")?,
            appearance_schedule: reader.u32_at(base + 0x88, "spawn appearance schedule")?,
            prob_day: reader.u16_at(base + 0x8c, "spawn probability day")?,
            prob_night: reader.u16_at(base + 0x8e, "spawn probability night")?,
        })
    }

    /// The creature slots, for the manager to resolve their CRE names.
    pub fn creatures_mut(&mut self) -> &mut [SpawnCreature] {
        &mut self.creatures
    }
}

/// An ambient sound emitter (0xD4 bytes).
pub struct Ambient {
    name: String,
    x: u16,
    y: u16,
    radius: u16,
    volume: u16,
    sounds: Vec<String>,
    flags: u32,
}

impl Ambient {
    fn parse_at(reader: &Reader, base: usize) -> Result<Ambient, Truncated> {
        let mut sounds = Vec::new();
        for i in 0..10 {
            let sound = reader.resref_at(base + 0x30 + i * 8, "ambient sound")?;
            if !sound.is_empty() {
                sounds.push(sound);
            }
        }
        Ok(Ambient {
            name: name_at(reader, base, "ambient name")?,
            x: reader.u16_at(base + 0x20, "ambient x")?,
            y: reader.u16_at(base + 0x22, "ambient y")?,
            radius: reader.u16_at(base + 0x24, "ambient radius")?,
            volume: reader.u16_at(base + 0x2e, "ambient volume")?,
            sounds,
            flags: reader.u32_at(base + 0x90, "ambient flags")?,
        })
    }
}

/// An area-scoped variable. The engine only reads/writes the INT value (0x54
/// bytes).
pub struct Variable {
    name: String,
    var_type: u16,
    value: i32,
}

impl Variable {
    fn parse_at(reader: &Reader, base: usize) -> Result<Variable, Truncated> {
        Ok(Variable {
            name: name_at(reader, base, "variable name")?,
            var_type: reader.u16_at(base + 0x20, "variable type")?,
            value: reader.u32_at(base + 0x28, "variable value")? as i32,
        })
    }
}

/// An interactive door: its clickable polygon, lock/trap state and links (0xC8
/// bytes). Distinct from the WED door it is linked to by `door_id`.
pub struct Door {
    name: String,
    door_id: String,
    flags: u32,
    hp: u16,
    ac: u16,
    open_sound: String,
    close_sound: String,
    trapped: u16,
    key_item: String,
    script: String,
    lock_difficulty: u32,
    /// Message shown when the lock resists (a strref); not resolved for the
    /// human view.
    lockpick_string: u32,
    dialog_speaker: u32,
    dialog: String,
    /// Dialog speaker name, resolved by the manager from the TLK.
    speaker_label: Option<String>,
}

impl Door {
    fn parse_at(reader: &Reader, base: usize) -> Result<Door, Truncated> {
        Ok(Door {
            name: name_at(reader, base, "door name")?,
            door_id: reader.resref_at(base + 0x20, "door id")?,
            flags: reader.u32_at(base + 0x28, "door flags")?,
            hp: reader.u16_at(base + 0x54, "door hp")?,
            ac: reader.u16_at(base + 0x56, "door ac")?,
            open_sound: reader.resref_at(base + 0x58, "door open sound")?,
            close_sound: reader.resref_at(base + 0x60, "door close sound")?,
            trapped: reader.u16_at(base + 0x70, "door trapped")?,
            key_item: reader.resref_at(base + 0x78, "door key item")?,
            script: reader.resref_at(base + 0x80, "door script")?,
            lock_difficulty: reader.u32_at(base + 0x8c, "door lock")?,
            lockpick_string: reader.u32_at(base + 0x98, "door lockpick string")?,
            dialog_speaker: reader.u32_at(base + 0xb4, "door dialog speaker")?,
            dialog: reader.resref_at(base + 0xb8, "door dialog")?,
            speaker_label: None,
        })
    }

    /// The dialog speaker strref, for the manager to resolve against the TLK.
    pub fn strref(&self) -> u32 {
        self.dialog_speaker
    }

    /// Attach the resolved speaker name.
    pub fn set_label(&mut self, label: Option<String>) {
        self.speaker_label = label;
    }
}

/// A background animation played in the area (0x4C bytes).
pub struct Animation {
    name: String,
    x: u16,
    y: u16,
    resource: String,
    flags: u32,
    transparency: u16,
}

impl Animation {
    fn parse_at(reader: &Reader, base: usize) -> Result<Animation, Truncated> {
        Ok(Animation {
            name: name_at(reader, base, "animation name")?,
            x: reader.u16_at(base + 0x20, "animation x")?,
            y: reader.u16_at(base + 0x22, "animation y")?,
            resource: reader.resref_at(base + 0x28, "animation resref")?,
            flags: reader.u32_at(base + 0x34, "animation flags")?,
            transparency: reader.u16_at(base + 0x3a, "animation transparency")?,
        })
    }
}

/// A note pinned to the automap (0x34 bytes).
pub struct Automap {
    x: u16,
    y: u16,
    text: u32,
    color: u16,
    /// Note text, resolved by the manager from the TLK.
    text_label: Option<String>,
}

impl Automap {
    fn parse_at(reader: &Reader, base: usize) -> Result<Automap, Truncated> {
        Ok(Automap {
            x: reader.u16_at(base, "automap x")?,
            y: reader.u16_at(base + 0x02, "automap y")?,
            text: reader.u32_at(base + 0x04, "automap text")?,
            color: reader.u16_at(base + 0x0a, "automap colour")?,
            text_label: None,
        })
    }

    /// The note text strref, for the manager to resolve against the TLK.
    pub fn strref(&self) -> u32 {
        self.text
    }

    /// Attach the resolved note text.
    pub fn set_label(&mut self, label: Option<String>) {
        self.text_label = label;
    }
}

/// A tiled object: a WED tile whose state the area scripts can toggle (0x68
/// bytes). Rarely used and, per the spec, not fully supported by the engine.
pub struct TiledObject {
    name: String,
    tile_id: String,
    flags: u32,
}

impl TiledObject {
    fn parse_at(reader: &Reader, base: usize) -> Result<TiledObject, Truncated> {
        Ok(TiledObject {
            name: name_at(reader, base, "tiled object name")?,
            tile_id: reader.resref_at(base + 0x20, "tiled object id")?,
            flags: reader.u32_at(base + 0x28, "tiled object flags")?,
        })
    }
}

/// A saved in-flight projectile (e.g. a Skull Trap). Present essentially only
/// in save games (0x1C bytes).
pub struct ProjectileTrap {
    projectile: String,
    missile: u16,
    x: u16,
    y: u16,
    owner: u8,
    /// projectl.ids symbol for the missile, resolved by the manager.
    missile_name: Option<String>,
}

impl ProjectileTrap {
    fn parse_at(reader: &Reader, base: usize) -> Result<ProjectileTrap, Truncated> {
        Ok(ProjectileTrap {
            projectile: reader.resref_at(base, "trap projectile")?,
            missile: reader.u16_at(base + 0x0e, "trap missile")?,
            x: reader.u16_at(base + 0x14, "trap x")?,
            y: reader.u16_at(base + 0x16, "trap y")?,
            owner: reader.u8_at(base + 0x1b, "trap owner")?,
            missile_name: None,
        })
    }

    /// The missile.ids reference (stored as `projectl.ids - 1`), for the manager
    /// to resolve a symbol.
    pub fn missile(&self) -> u16 {
        self.missile
    }

    /// Attach the resolved projectl.ids symbol.
    pub fn set_missile_name(&mut self, name: Option<String>) {
        self.missile_name = name;
    }
}

/// The area's music and background-ambient assignments — a single fixed struct,
/// not an array (0x90 bytes). Song numbers index songlist.2da, with a few
/// sentinels (0 = silence, -1 = last master area, -2 = day/night).
pub struct Song {
    day: u32,
    night: u32,
    win: u32,
    battle: u32,
    lose: u32,
    day_ambient: [String; 2],
    day_volume: u32,
    night_ambient: [String; 2],
    night_volume: u32,
}

impl Song {
    fn parse_at(reader: &Reader, base: usize) -> Result<Song, Truncated> {
        Ok(Song {
            day: reader.u32_at(base, "day song")?,
            night: reader.u32_at(base + 0x04, "night song")?,
            win: reader.u32_at(base + 0x08, "win song")?,
            battle: reader.u32_at(base + 0x0c, "battle song")?,
            lose: reader.u32_at(base + 0x10, "lose song")?,
            day_ambient: [
                reader.resref_at(base + 0x28, "day ambient 1")?,
                reader.resref_at(base + 0x30, "day ambient 2")?,
            ],
            day_volume: reader.u32_at(base + 0x38, "day ambient volume")?,
            night_ambient: [
                reader.resref_at(base + 0x3c, "night ambient 1")?,
                reader.resref_at(base + 0x44, "night ambient 2")?,
            ],
            night_volume: reader.u32_at(base + 0x4c, "night ambient volume")?,
        })
    }
}

/// A rest-interruption table: creatures that can break the party's rest in this
/// area — a single fixed struct, not an array (0xE4 bytes).
pub struct RestInterruption {
    name: String,
    creatures: Vec<SpawnCreature>,
    difficulty: u16,
    max_spawn: u16,
    enabled: u16,
    prob_day: u16,
    prob_night: u16,
}

impl RestInterruption {
    fn parse_at(reader: &Reader, base: usize) -> Result<RestInterruption, Truncated> {
        let mut creatures = Vec::new();
        for i in 0..10 {
            let resource = reader.resref_at(base + 0x48 + i * 8, "rest creature")?;
            if !resource.is_empty() {
                creatures.push(SpawnCreature {
                    resource,
                    name: None,
                });
            }
        }
        Ok(RestInterruption {
            name: name_at(reader, base, "rest name")?,
            creatures,
            difficulty: reader.u16_at(base + 0x9a, "rest difficulty")?,
            max_spawn: reader.u16_at(base + 0xa4, "rest max spawn")?,
            enabled: reader.u16_at(base + 0xa6, "rest enabled")?,
            prob_day: reader.u16_at(base + 0xa8, "rest probability day")?,
            prob_night: reader.u16_at(base + 0xaa, "rest probability night")?,
        })
    }

    /// The creature slots, for the manager to resolve their CRE names.
    pub fn creatures_mut(&mut self) -> &mut [SpawnCreature] {
        &mut self.creatures
    }

    /// Whether the table carries any interruption content worth rendering.
    fn is_populated(&self) -> bool {
        !self.name.is_empty() || !self.creatures.is_empty()
    }
}

/// Anything that can go wrong while parsing an ARE file.
#[derive(Debug)]
pub enum AreError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for AreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AreError::Truncated(e) => write!(f, "{e}"),
            AreError::BadSignature(found) => {
                write!(f, "not an ARE file: signature was {}", describe_tag(found))
            }
            AreError::BadVersion(found) => {
                write!(f, "unsupported ARE version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for AreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AreError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for AreError {
    fn from(e: Truncated) -> Self {
        AreError::Truncated(e)
    }
}

/// Read a fixed 32-byte NUL-padded name field.
fn name_at(reader: &Reader, offset: usize, context: &'static str) -> Result<String, Truncated> {
    Ok(parse_asciiz(reader.slice(offset, 32, context)?))
}

/// Render a strref for the machine view, mapping the `0xFFFFFFFF` "no string"
/// sentinel to `-1` for readability.
fn strref_raw(value: u32) -> String {
    if value == u32::MAX {
        "-1".to_string()
    } else {
        value.to_string()
    }
}

/// Classify an appearance schedule (bits 0-23) for the human view. Only the
/// clean day/night cases are named; "always" and mixed schedules stay quiet to
/// avoid cluttering the common case.
fn schedule_label(mask: u32) -> Option<&'static str> {
    match mask & 0x00ff_ffff {
        DAY_HOURS => Some("day"),
        NIGHT_HOURS => Some("night"),
        _ => None,
    }
}

/// Render a song reference, naming the sentinel values (songlist.2da numbers
/// are shown bare).
fn song_ref(value: u32) -> String {
    match value as i32 {
        0 => "silent".to_string(),
        -1 => "last-area".to_string(),
        -2 => "day/night".to_string(),
        n => n.to_string(),
    }
}

/// Area flags (header 0x14), BG2/BGEE meanings.
fn area_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "no save"),
            (1, "tutorial"),
            (2, "dead magic"),
            (3, "dream"),
            (4, "no player1 death end"),
            (5, "no rest"),
            (6, "no travel"),
        ],
    )
}

/// Name a region type (region 0x20).
fn region_type_name(value: u16) -> &'static str {
    match value {
        0 => "trigger",
        1 => "info",
        2 => "travel",
        _ => "?",
    }
}

/// Name a container type (container 0x24).
fn container_type_name(value: u16) -> &'static str {
    match value {
        0 => "n/a",
        1 => "bag",
        2 => "chest",
        3 => "drawer",
        4 => "pile",
        5 => "table",
        6 => "shelf",
        7 => "altar",
        8 => "nonvisible",
        9 => "spellbook",
        10 => "body",
        11 => "barrel",
        12 => "crate",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a minimal ARE V1.0 file: a full header with all section counts
    /// zeroed, then patch in the sections a test needs.
    fn build_are() -> Vec<u8> {
        let mut data = vec![0u8; 0x11c];
        data[0x00..0x04].copy_from_slice(ARE_SIGNATURE);
        data[0x04..0x08].copy_from_slice(ARE_VERSION);
        data
    }

    fn put_u32(data: &mut [u8], at: usize, value: u32) {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put_u16(data: &mut [u8], at: usize, value: u16) {
        data[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn empty_area_has_no_sections() {
        let are = Are::parse(&build_are()).expect("parse");
        assert!(are.actors.is_empty());
        assert!(are.containers.is_empty());
        assert!(are.items.is_empty());
        assert!(are.doors.is_empty());
    }

    #[test]
    fn reads_actors_and_items() {
        let mut data = build_are();
        let actor_off = data.len();
        data.resize(actor_off + ACTOR_SIZE, 0);
        data[actor_off..actor_off + 4].copy_from_slice(b"Gua\0");
        let item_off = data.len();
        data.resize(item_off + ITEM_SIZE, 0);
        data[item_off..item_off + 4].copy_from_slice(b"SW1H");

        put_u32(&mut data, 0x54, actor_off as u32);
        put_u16(&mut data, 0x58, 1);
        put_u32(&mut data, 0x78, item_off as u32);
        put_u16(&mut data, 0x76, 1);

        let are = Are::parse(&data).expect("parse");
        assert_eq!(are.actors.len(), 1);
        assert_eq!(are.actors[0].name, "Gua");
        assert_eq!(are.items.len(), 1);
        assert_eq!(are.items[0].resource, "SW1H");
    }

    #[test]
    fn container_items_slice_is_bounds_checked() {
        let mut data = build_are();
        let cont_off = data.len();
        data.resize(cont_off + CONTAINER_SIZE, 0);
        // Claim 5 items starting at index 0, but the pool is empty.
        put_u32(&mut data, cont_off + 0x40, 0);
        put_u32(&mut data, cont_off + 0x44, 5);
        put_u32(&mut data, 0x70, cont_off as u32);
        put_u16(&mut data, 0x74, 1);

        let are = Are::parse(&data).expect("parse");
        assert_eq!(are.containers.len(), 1);
        assert!(are.container_items(&are.containers[0]).is_empty());
    }

    #[test]
    fn resref_uses_tag_each_reference_with_the_type_it_names() {
        let mut data = build_are();
        data[0x08..0x0c].copy_from_slice(b"AR10"); // WED
        data[0x94..0x98].copy_from_slice(b"AREA"); // area script
        data[0x18..0x1c].copy_from_slice(b"AR20"); // north neighbour

        let actor_off = data.len();
        data.resize(actor_off + ACTOR_SIZE, 0);
        data[actor_off + 0x48..actor_off + 0x4c].copy_from_slice(b"CHAT"); // dialog
        data[actor_off + 0x50..actor_off + 0x54].copy_from_slice(b"SCRP"); // script[0]
        data[actor_off + 0x80..actor_off + 0x84].copy_from_slice(b"GUAR"); // CRE

        let item_off = data.len();
        data.resize(item_off + ITEM_SIZE, 0);
        data[item_off..item_off + 4].copy_from_slice(b"SW1H");

        let door_off = data.len();
        data.resize(door_off + DOOR_SIZE, 0);
        data[door_off + 0x78..door_off + 0x7c].copy_from_slice(b"KEY1"); // key item

        put_u32(&mut data, 0x54, actor_off as u32);
        put_u16(&mut data, 0x58, 1);
        put_u32(&mut data, 0x78, item_off as u32);
        put_u16(&mut data, 0x76, 1);
        put_u32(&mut data, 0xa8, door_off as u32);
        put_u32(&mut data, 0xa4, 1);

        let are = Are::parse(&data).expect("parse");
        let uses = are.resref_uses();
        let got: Vec<(u16, &str, &str)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (WED_TYPE, "AR10", "wed"),
                (BCS_TYPE, "AREA", "script"),
                (ARE_TYPE, "AR20", "north"),
                (CRE_TYPE, "GUAR", "actor[0].cre"),
                (DLG_TYPE, "CHAT", "actor[0].dialog"),
                (BCS_TYPE, "SCRP", "actor[0].script[0]"),
                (ITM_TYPE, "SW1H", "item[0]"),
                (ITM_TYPE, "KEY1", "door[0].key_item"),
            ]
        );
        // Empty resrefs (the unset neighbours, the door's script and dialog, the
        // actor's five other script slots) are dropped, not reported as "".
        assert!(uses.iter().all(|u| !u.value.is_empty()));
    }

    #[test]
    fn strref_uses_labels_each_section() {
        let mut data = build_are();
        let region_off = data.len();
        data.resize(region_off + REGION_SIZE, 0);
        put_u32(&mut data, region_off + 0x64, 100); // region info text
        put_u32(&mut data, region_off + 0xb8, 101); // region speaker name
        let cont_off = data.len();
        data.resize(cont_off + CONTAINER_SIZE, 0);
        put_u32(&mut data, cont_off + 0x84, 400); // container lockpick string
        let door_off = data.len();
        data.resize(door_off + DOOR_SIZE, 0);
        put_u32(&mut data, door_off + 0x98, 201); // door lockpick string
        put_u32(&mut data, door_off + 0xb4, 200); // door dialog speaker
        let note_off = data.len();
        data.resize(note_off + AUTOMAP_SIZE, 0);
        put_u32(&mut data, note_off + 0x04, 300); // automap note text

        put_u32(&mut data, 0x5c, region_off as u32);
        put_u16(&mut data, 0x5a, 1);
        put_u32(&mut data, 0x70, cont_off as u32);
        put_u16(&mut data, 0x74, 1);
        put_u32(&mut data, 0xa8, door_off as u32);
        put_u32(&mut data, 0xa4, 1);
        put_u32(&mut data, 0xc4, note_off as u32);
        put_u32(&mut data, 0xc8, 1);

        let uses = Are::parse(&data).expect("parse").strref_uses();
        let got: Vec<(u32, &str)> = uses.iter().map(|u| (u.value, u.path.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (100, "region[0].info_text"),
                (101, "region[0].speaker_name"),
                (400, "container[0].lockpick_string"),
                (201, "door[0].lockpick_string"),
                (200, "door[0].dialog_speaker"),
                (300, "automap[0].text"),
            ]
        );
    }

    #[test]
    fn rejects_bad_signature() {
        let mut data = build_are();
        data[0] = b'X';
        assert!(matches!(Are::parse(&data), Err(AreError::BadSignature(_))));
    }

    #[test]
    fn rejects_bad_version() {
        let mut data = build_are();
        data[0x04..0x08].copy_from_slice(b"V9.1");
        assert!(matches!(Are::parse(&data), Err(AreError::BadVersion(_))));
    }

    #[test]
    fn rejects_truncated_section() {
        let mut data = build_are();
        // Claims one actor at an offset past the end of the file.
        put_u32(&mut data, 0x54, 0x1000);
        put_u16(&mut data, 0x58, 1);
        assert!(matches!(Are::parse(&data), Err(AreError::Truncated(_))));
    }

    #[test]
    fn reads_actor_and_spawn_detail_fields() {
        let mut data = build_are();
        let actor_off = data.len();
        data.resize(actor_off + ACTOR_SIZE, 0);
        put_u16(&mut data, actor_off + 0x24, 111); // dest x
        put_u16(&mut data, actor_off + 0x26, 222); // dest y
        put_u32(&mut data, actor_off + 0x30, 0x6000); // animation
        put_u32(&mut data, actor_off + 0x38, u32::MAX); // removal timer = -1
        put_u32(&mut data, actor_off + 0x40, 0xffffff); // schedule = always
        let spawn_off = data.len();
        data.resize(spawn_off + SPAWN_SIZE, 0);
        put_u16(&mut data, spawn_off + 0x84, 5); // max spawn
        put_u16(&mut data, spawn_off + 0x8c, 30); // prob day
        put_u16(&mut data, spawn_off + 0x8e, 70); // prob night

        put_u32(&mut data, 0x54, actor_off as u32);
        put_u16(&mut data, 0x58, 1);
        put_u32(&mut data, 0x60, spawn_off as u32);
        put_u32(&mut data, 0x64, 1);

        let are = Are::parse(&data).expect("parse");
        let a = &are.actors[0];
        assert_eq!((a.dest_x, a.dest_y), (111, 222));
        assert_eq!(a.animation, 0x6000);
        assert_eq!(a.removal_timer, -1);
        assert_eq!(a.appearance_schedule, 0xffffff);
        let s = &are.spawns[0];
        assert_eq!(s.max_spawn, 5);
        assert_eq!((s.prob_day, s.prob_night), (30, 70));
    }

    #[test]
    fn strref_sentinel_renders_as_negative_one() {
        assert_eq!(strref_raw(u32::MAX), "-1");
        assert_eq!(strref_raw(42), "42");
    }

    #[test]
    fn reads_vertex_pool() {
        let mut data = build_are();
        let off = data.len();
        data.resize(off + 3 * VERTEX_SIZE, 0);
        for (i, (x, y)) in [(10u16, 20u16), (30, 40), (50, 60)].iter().enumerate() {
            put_u16(&mut data, off + i * VERTEX_SIZE, *x);
            put_u16(&mut data, off + i * VERTEX_SIZE + 2, *y);
        }
        put_u32(&mut data, 0x7c, off as u32);
        put_u16(&mut data, 0x80, 3);

        let are = Are::parse(&data).expect("parse");
        assert_eq!(are.vertices, vec![(10, 20), (30, 40), (50, 60)]);
    }

    #[test]
    fn reads_tiled_objects_and_traps() {
        let mut data = build_are();
        let tiled_off = data.len();
        data.resize(tiled_off + TILED_SIZE, 0);
        data[tiled_off..tiled_off + 4].copy_from_slice(b"Door");
        let trap_off = data.len();
        data.resize(trap_off + PTRAP_SIZE, 0);
        data[trap_off..trap_off + 4].copy_from_slice(b"SKUL");

        put_u32(&mut data, 0xb8, tiled_off as u32);
        put_u32(&mut data, 0xb4, 1);
        put_u32(&mut data, 0xcc, trap_off as u32);
        put_u32(&mut data, 0xd0, 1);

        let are = Are::parse(&data).expect("parse");
        assert_eq!(are.tiled_objects.len(), 1);
        assert_eq!(are.tiled_objects[0].name, "Door");
        assert_eq!(are.proj_traps.len(), 1);
        assert_eq!(are.proj_traps[0].projectile, "SKUL");
    }

    #[test]
    fn reads_song_and_rest_structs() {
        let mut data = build_are();
        let song_off = data.len();
        data.resize(song_off + SONG_SIZE, 0);
        put_u32(&mut data, song_off, 7); // day song
        let rest_off = data.len();
        data.resize(rest_off + REST_SIZE, 0);
        data[rest_off..rest_off + 4].copy_from_slice(b"Sold");
        data[rest_off + 0x48..rest_off + 0x4c].copy_from_slice(b"GUAR"); // 1st creature

        put_u32(&mut data, 0xbc, song_off as u32);
        put_u32(&mut data, 0xc0, rest_off as u32);

        let are = Are::parse(&data).expect("parse");
        let song = are.song.as_ref().expect("song");
        assert_eq!(song.day, 7);
        let rest = are.rest.as_ref().expect("rest");
        assert_eq!(rest.name, "Sold");
        assert_eq!(rest.creatures.len(), 1);
        assert_eq!(rest.creatures[0].resource, "GUAR");
    }

    #[test]
    fn single_struct_absent_when_offset_out_of_bounds() {
        let mut data = build_are();
        // A song offset past the end of the file reads as absent, not an error.
        put_u32(&mut data, 0xbc, 0x9000);
        let are = Are::parse(&data).expect("parse");
        assert!(are.song.is_none());
        assert!(are.rest.is_none());
    }
}
