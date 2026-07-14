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

use crate::formats::fields::{set_bits, set_flags, write_bitfield};
use crate::{Reader, Truncated, describe_tag, parse_asciiz};

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
    // Sections not decoded in phase 1, surfaced as counts/sizes.
    vertices: u16,
    tiled_objects: u32,
    proj_traps: u32,
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
            vertices: reader.u16_at(0x80, "vertex count")?,
            tiled_objects: reader.u32_at(0xb4, "tiled object count")?,
            proj_traps: reader.u32_at(0xd0, "projectile trap count")?,
            explored_size: reader.u32_at(0x9c, "explored bitmask size")?,
        })
    }
}

/// A creature/party actor placed in the area (0x110 bytes).
pub struct Actor {
    name: String,
    x: u16,
    y: u16,
    flags: u32,
    orientation: u16,
    dialog: String,
    scripts: [String; 6],
    cre_file: String,
    /// CRE display name, resolved by the manager.
    name_text: Option<String>,
}

impl Actor {
    fn parse_at(reader: &Reader, base: usize) -> Result<Actor, Truncated> {
        Ok(Actor {
            name: name_at(reader, base, "actor name")?,
            x: reader.u16_at(base + 0x20, "actor x")?,
            y: reader.u16_at(base + 0x22, "actor y")?,
            flags: reader.u32_at(base + 0x28, "actor flags")?,
            orientation: reader.u16_at(base + 0x34, "actor orientation")?,
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
}

/// Names of the six actor script slots, in parse order.
const SCRIPT_SLOTS: [&str; 6] = [
    "override", "general", "class", "race", "default", "specific",
];

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
    frequency: u16,
    enabled: u16,
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
            frequency: reader.u16_at(base + 0x78, "spawn frequency")?,
            enabled: reader.u16_at(base + 0x86, "spawn enabled")?,
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
        })
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
        writeln!(out, "vertices={}", h.vertices)?;
        writeln!(out, "tiled_objects={}", h.tiled_objects)?;
        writeln!(out, "proj_traps={}", h.proj_traps)?;
        writeln!(out, "explored_size={}", h.explored_size)?;

        for (i, a) in self.actors.iter().enumerate() {
            writeln!(out, "actor.{i}.name={}", a.name)?;
            writeln!(out, "actor.{i}.x={}", a.x)?;
            writeln!(out, "actor.{i}.y={}", a.y)?;
            writeln!(out, "actor.{i}.flags={}", a.flags)?;
            writeln!(out, "actor.{i}.orientation={}", a.orientation)?;
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
            writeln!(out, "region.{i}.info_text={}", r.info_text)?;
            writeln!(out, "region.{i}.trapped={}", r.trapped)?;
            writeln!(out, "region.{i}.key_item={}", r.key_item)?;
            writeln!(out, "region.{i}.script={}", r.script)?;
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
            writeln!(out, "door.{i}.dialog_speaker={}", d.dialog_speaker)?;
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
            writeln!(out, "automap.{i}.text={}", n.text)?;
            writeln!(out, "automap.{i}.color={}", n.color)?;
        }

        Ok(())
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

        // Sections not yet decoded, reported so their presence isn't hidden.
        let other = [
            ("vertices", h.vertices as u32),
            ("tiled objects", h.tiled_objects),
            ("projectile traps", h.proj_traps),
        ];
        if other.iter().any(|(_, n)| *n != 0) {
            let parts: Vec<String> = other
                .iter()
                .filter(|(_, n)| *n != 0)
                .map(|(label, n)| format!("{n} {label}"))
                .collect();
            writeln!(f, "\n  other: {}", parts.join(", "))?;
        }

        Ok(())
    }
}

impl Are {
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

    fn put_u32(data: &mut Vec<u8>, at: usize, value: u32) {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put_u16(data: &mut Vec<u8>, at: usize, value: u16) {
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
}
