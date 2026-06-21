//! EFF parser.
//!
//! An EFF describes a single effect (an opcode plus its parameters): the atom
//! of the engine's magic system. Standalone `.eff` files use version 2.0; the
//! same effect structure is also embedded — as the 48-byte V1 "feature block" —
//! in [ITM](itm) and SPL files, and (V1 or V2) in [CRE](cre) files.
//!
//! This module parses the standalone V2.0 file. The field-interpretation
//! helpers ([`target_type`], [`timing_mode`], …) are shared with the embedded
//! forms, which read the same meanings out of a tighter byte layout.
//! Spec: `md/file_formats/ie_formats/eff_v2.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::opcode::opcode_name;
use crate::{Reader, Truncated, describe_tag, parse_asciiz};

/// Resource type code for EFF files.
pub const EFF_TYPE: u16 = 0x03f8;

const EFF_SIGNATURE: &[u8; 4] = b"EFF ";
const EFF_VERSION: &[u8; 4] = b"V2.0";

/// Anything that can go wrong while parsing an EFF file.
#[derive(Debug)]
pub enum EffError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for EffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffError::Truncated(e) => write!(f, "{e}"),
            EffError::BadSignature(found) => {
                write!(f, "not an EFF file: signature was {}", describe_tag(found))
            }
            EffError::BadVersion(found) => {
                write!(f, "unsupported EFF version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for EffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EffError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for EffError {
    fn from(e: Truncated) -> Self {
        EffError::Truncated(e)
    }
}

/// A parsed EFF V2.0 effect, holding the complete on-disk record in field
/// order. [`Display`](fmt::Display) surfaces a human-readable subset; the rest —
/// runtime bookkeeping the engine fills in (coordinates, time-applied, …) and
/// the format's unknown/padding words — is retained verbatim so the forthcoming
/// byte-faithful diff output can compare every field.
#[allow(dead_code)] // full record stored; not every field is surfaced in Display yet
pub struct Eff {
    /// Body signature/version, repeated from the header in standalone EFFs and
    /// zeroed in embedded ones (0x08/0x0c).
    signature2: [u8; 4],
    version2: [u8; 4],

    opcode: u32,
    target: u32,
    power: u32,
    param1: u32,
    param2: u32,
    timing_mode: u16,
    /// 0x26 "Timing ?" — undocumented secondary timing word.
    timing_unknown: u16,
    duration: u32,
    probability1: u16,
    probability2: u16,
    resource: String,
    dice_thrown: u32,
    dice_sides: u32,
    save_type: u32,
    save_bonus: u32,
    special: u32,
    primary_type: u32,
    unknown_0x50: u32,
    affected_level_low: u32,
    affected_level_high: u32,
    dispel: u32,
    param3: u32,
    param4: u32,
    param5: u32,
    time_applied: u32,
    resource2: String,
    resource3: String,
    caster_x: u32,
    caster_y: u32,
    target_x: u32,
    target_y: u32,
    parent_resource_type: u32,
    parent_resource: String,
    parent_resource_flags: u32,
    projectile: u32,
    parent_resource_slot: u32,
    variable: String,
    caster_level: u32,
    first_apply: u32,
    secondary_type: u32,
    /// 0xd4 trailing 15×dword block, undocumented.
    unknown_tail: [u32; 15],

    /// Symbolic names for `primary_type`/`secondary_type`, resolved by the
    /// manager from the MSCHOOL/MSECTYPE dictionary tables. The parser leaves
    /// these unset, as it cannot reach other resources.
    primary_name: Option<String>,
    secondary_name: Option<String>,
}

impl Eff {
    /// Parse a standalone EFF V2.0 resource.
    pub fn parse(data: &[u8]) -> Result<Eff, EffError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != EFF_SIGNATURE {
            return Err(EffError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != EFF_VERSION {
            return Err(EffError::BadVersion(version));
        }

        // The body repeats the signature/version at 0x08; the effect proper
        // begins at 0x10 (see eff_v2.md).
        let mut unknown_tail = [0u32; 15];
        for (i, slot) in unknown_tail.iter_mut().enumerate() {
            *slot = reader.u32_at(0xd4 + i * 4, "trailing unknown")?;
        }

        Ok(Eff {
            signature2: reader.tag_at(0x08, "body signature")?,
            version2: reader.tag_at(0x0c, "body version")?,
            opcode: reader.u32_at(0x10, "opcode")?,
            target: reader.u32_at(0x14, "target type")?,
            power: reader.u32_at(0x18, "power")?,
            param1: reader.u32_at(0x1c, "parameter 1")?,
            param2: reader.u32_at(0x20, "parameter 2")?,
            timing_mode: reader.u16_at(0x24, "timing mode")?,
            timing_unknown: reader.u16_at(0x26, "timing (secondary)")?,
            duration: reader.u32_at(0x28, "duration")?,
            probability1: reader.u16_at(0x2c, "probability 1")?,
            probability2: reader.u16_at(0x2e, "probability 2")?,
            resource: reader.resref_at(0x30, "resource")?,
            dice_thrown: reader.u32_at(0x38, "dice thrown")?,
            dice_sides: reader.u32_at(0x3c, "dice sides")?,
            save_type: reader.u32_at(0x40, "saving throw type")?,
            save_bonus: reader.u32_at(0x44, "save bonus")?,
            special: reader.u32_at(0x48, "special")?,
            primary_type: reader.u32_at(0x4c, "primary type")?,
            unknown_0x50: reader.u32_at(0x50, "unknown")?,
            affected_level_low: reader.u32_at(0x54, "lowest affected level")?,
            affected_level_high: reader.u32_at(0x58, "highest affected level")?,
            dispel: reader.u32_at(0x5c, "dispel/resistance")?,
            param3: reader.u32_at(0x60, "parameter 3")?,
            param4: reader.u32_at(0x64, "parameter 4")?,
            param5: reader.u32_at(0x68, "parameter 5")?,
            time_applied: reader.u32_at(0x6c, "time applied")?,
            resource2: reader.resref_at(0x70, "resource 2")?,
            resource3: reader.resref_at(0x78, "resource 3")?,
            caster_x: reader.u32_at(0x80, "caster x")?,
            caster_y: reader.u32_at(0x84, "caster y")?,
            target_x: reader.u32_at(0x88, "target x")?,
            target_y: reader.u32_at(0x8c, "target y")?,
            parent_resource_type: reader.u32_at(0x90, "parent resource type")?,
            parent_resource: reader.resref_at(0x94, "parent resource")?,
            parent_resource_flags: reader.u32_at(0x9c, "parent resource flags")?,
            projectile: reader.u32_at(0xa0, "projectile")?,
            parent_resource_slot: reader.u32_at(0xa4, "parent resource slot")?,
            variable: parse_asciiz(reader.slice(0xa8, 32, "variable name")?),
            caster_level: reader.u32_at(0xc8, "caster level")?,
            first_apply: reader.u32_at(0xcc, "first apply")?,
            secondary_type: reader.u32_at(0xd0, "secondary type")?,
            unknown_tail,
            primary_name: None,
            secondary_name: None,
        })
    }

    /// The primary type (school), an index into MSCHOOL.2DA.
    pub fn primary_type(&self) -> u32 {
        self.primary_type
    }

    /// The secondary type, an index into MSECTYPE.2DA.
    pub fn secondary_type(&self) -> u32 {
        self.secondary_type
    }

    /// Attach the symbolic names resolved for the primary/secondary types. Used
    /// by the manager, which alone can reach the dictionary tables.
    pub fn set_type_names(&mut self, primary: Option<String>, secondary: Option<String>) {
        self.primary_name = primary;
        self.secondary_name = secondary;
    }

    /// Write the effect's fields as `key=value` lines, in disk order, exactly as
    /// stored (unsigned, undecoded). The machine-readable counterpart to
    /// [`Display`](fmt::Display); the format's unknown/structural words and the
    /// resolved dictionary names are intentionally omitted.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "opcode={}", self.opcode)?;
        writeln!(out, "target={}", self.target)?;
        writeln!(out, "power={}", self.power)?;
        writeln!(out, "param1={}", self.param1)?;
        writeln!(out, "param2={}", self.param2)?;
        writeln!(out, "timing_mode={}", self.timing_mode)?;
        writeln!(out, "duration={}", self.duration)?;
        writeln!(out, "probability1={}", self.probability1)?;
        writeln!(out, "probability2={}", self.probability2)?;
        writeln!(out, "resource={}", self.resource)?;
        writeln!(out, "dice_thrown={}", self.dice_thrown)?;
        writeln!(out, "dice_sides={}", self.dice_sides)?;
        writeln!(out, "save_type={}", self.save_type)?;
        writeln!(out, "save_bonus={}", self.save_bonus)?;
        writeln!(out, "special={}", self.special)?;
        writeln!(out, "primary_type={}", self.primary_type)?;
        writeln!(out, "affected_level_low={}", self.affected_level_low)?;
        writeln!(out, "affected_level_high={}", self.affected_level_high)?;
        writeln!(out, "dispel={}", self.dispel)?;
        writeln!(out, "param3={}", self.param3)?;
        writeln!(out, "param4={}", self.param4)?;
        writeln!(out, "param5={}", self.param5)?;
        writeln!(out, "time_applied={}", self.time_applied)?;
        writeln!(out, "resource2={}", self.resource2)?;
        writeln!(out, "resource3={}", self.resource3)?;
        writeln!(out, "caster_x={}", self.caster_x)?;
        writeln!(out, "caster_y={}", self.caster_y)?;
        writeln!(out, "target_x={}", self.target_x)?;
        writeln!(out, "target_y={}", self.target_y)?;
        writeln!(out, "parent_resource_type={}", self.parent_resource_type)?;
        writeln!(out, "parent_resource={}", self.parent_resource)?;
        writeln!(out, "parent_resource_flags={}", self.parent_resource_flags)?;
        writeln!(out, "projectile={}", self.projectile)?;
        writeln!(out, "parent_resource_slot={}", self.parent_resource_slot)?;
        writeln!(out, "variable={}", self.variable)?;
        writeln!(out, "caster_level={}", self.caster_level)?;
        writeln!(out, "first_apply={}", self.first_apply)?;
        writeln!(out, "secondary_type={}", self.secondary_type)?;
        Ok(())
    }
}

impl fmt::Display for Eff {
    /// Human-readable view: the authored effect fields, with the optional
    /// resrefs, dice, and extra parameters shown only when they carry a value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "EFF V2.0")?;
        match opcode_name(self.opcode) {
            Some(name) => writeln!(f, "  opcode:      {} ({name})", self.opcode)?,
            None => writeln!(f, "  opcode:      {}", self.opcode)?,
        }
        // Many authored effects store -1 in target/parameter fields as a "none"
        // sentinel, so these read as signed; annotate the target only when it
        // names a known type.
        match target_type(self.target) {
            Some(name) => writeln!(f, "  target:      {} ({name})", self.target as i32)?,
            None => writeln!(f, "  target:      {}", self.target as i32)?,
        }
        writeln!(f, "  power:       {}", self.power)?;
        writeln!(
            f,
            "  parameters:  p1={} p2={}",
            self.param1 as i32, self.param2 as i32
        )?;
        writeln!(
            f,
            "  timing:      {} ({}), duration {}",
            self.timing_mode,
            timing_mode(self.timing_mode),
            self.duration
        )?;
        writeln!(
            f,
            "  probability: {}..{}",
            self.probability1, self.probability2
        )?;
        writeln!(
            f,
            "  save:        {} (bonus {})",
            save_type(self.save_type),
            self.save_bonus as i32
        )?;
        writeln!(
            f,
            "  resistance:  {} ({})",
            self.dispel,
            dispel_desc(self.dispel)
        )?;
        write_type(f, "primary", self.primary_type, &self.primary_name)?;
        write_type(f, "secondary", self.secondary_type, &self.secondary_name)?;

        // Dual-purpose with max/min level for non-damage opcodes, which store
        // the -1 "no limit" sentinel here; render signed so it reads as `-1`.
        if self.dice_thrown != 0 || self.dice_sides != 0 {
            writeln!(
                f,
                "  dice:        {}d{}",
                self.dice_thrown as i32, self.dice_sides as i32
            )?;
        }
        if self.param3 != 0 || self.param4 != 0 || self.param5 != 0 {
            writeln!(
                f,
                "  params 3-5:  {} {} {}",
                self.param3 as i32, self.param4 as i32, self.param5 as i32
            )?;
        }
        if self.special != 0 {
            writeln!(f, "  special:     {}", self.special as i32)?;
        }
        if self.affected_level_low != 0 || self.affected_level_high != 0 {
            writeln!(
                f,
                "  levels:      {}-{}",
                self.affected_level_low, self.affected_level_high
            )?;
        }
        if self.projectile != 0 {
            writeln!(f, "  projectile:  {}", self.projectile)?;
        }
        if !self.resource.is_empty() {
            writeln!(f, "  resource:    {:?}", self.resource)?;
        }
        if !self.resource2.is_empty() {
            writeln!(f, "  resource 2:  {:?}", self.resource2)?;
        }
        if !self.resource3.is_empty() {
            writeln!(f, "  resource 3:  {:?}", self.resource3)?;
        }
        if !self.parent_resource.is_empty() {
            writeln!(f, "  parent:      {:?}", self.parent_resource)?;
        }
        if !self.variable.is_empty() {
            writeln!(f, "  variable:    {:?}", self.variable)?;
        }

        Ok(())
    }
}

/// Size of one EFF V1 feature block on disk.
pub const FEATURE_SIZE: usize = 48;

/// A parsed EFF V1 "feature block": the 48-byte embedded effect carried by ITM
/// and SPL files (as equipping/casting effects and per-ability effects). It is
/// the same effect the standalone V2.0 [`Eff`] describes, in a tighter layout:
/// every field is byte-sized where V2 widened it to a dword, and the V2-only
/// runtime/coordinate fields are absent. The interpretation helpers
/// ([`target_type`], [`timing_mode`], …) are shared with [`Eff`].
///
/// Unlike [`Eff`], this is never a standalone resource, so it has no `Display`
/// of its own: the embedding format (ITM/SPL) drives layout and passes the
/// indent/prefix.
pub struct Feature {
    opcode: u16,
    target: u8,
    power: u8,
    param1: u32,
    param2: u32,
    timing_mode: u8,
    dispel: u8,
    duration: u32,
    probability1: u8,
    probability2: u8,
    resource: String,
    dice_thrown: u32,
    dice_sides: u32,
    save_type: u32,
    save_bonus: u32,
    /// 0x2c — "Unknown" in eff_v1, repurposed as a TobEx stacking ID in ITM.
    special: u32,
}

impl Feature {
    /// Read `count` feature blocks from the shared pool at `pool_offset`,
    /// starting at pool `index`. ITM and SPL both reference their effects this
    /// way — a continuous pool sliced by `(index, count)`. Bounds-checked.
    pub fn read_pool(
        reader: &Reader,
        pool_offset: u32,
        index: u16,
        count: u16,
    ) -> Result<Vec<Feature>, Truncated> {
        let mut features = Vec::with_capacity(count as usize);
        for k in 0..count as usize {
            let base = pool_offset as usize + (index as usize + k) * FEATURE_SIZE;
            features.push(Feature::parse_at(reader, base)?);
        }
        Ok(features)
    }

    /// Parse one feature block at `base` (an absolute offset into the embedding
    /// file). Bounds-checked through `reader`.
    pub fn parse_at(reader: &Reader, base: usize) -> Result<Feature, Truncated> {
        Ok(Feature {
            opcode: reader.u16_at(base, "opcode")?,
            target: reader.u8_at(base + 0x02, "target type")?,
            power: reader.u8_at(base + 0x03, "power")?,
            param1: reader.u32_at(base + 0x04, "parameter 1")?,
            param2: reader.u32_at(base + 0x08, "parameter 2")?,
            timing_mode: reader.u8_at(base + 0x0c, "timing mode")?,
            dispel: reader.u8_at(base + 0x0d, "dispel/resistance")?,
            duration: reader.u32_at(base + 0x0e, "duration")?,
            probability1: reader.u8_at(base + 0x12, "probability 1")?,
            probability2: reader.u8_at(base + 0x13, "probability 2")?,
            resource: reader.resref_at(base + 0x14, "resource")?,
            dice_thrown: reader.u32_at(base + 0x1c, "dice thrown")?,
            dice_sides: reader.u32_at(base + 0x20, "dice sides")?,
            save_type: reader.u32_at(base + 0x24, "saving throw type")?,
            save_bonus: reader.u32_at(base + 0x28, "save bonus")?,
            special: reader.u32_at(base + 0x2c, "special")?,
        })
    }

    /// Render the feature in the human-readable view, each line prefixed with
    /// `indent`. The optional resrefs, dice, and special word print only when
    /// they carry a value — matching [`Eff`]'s `Display`.
    pub fn write_human(&self, f: &mut dyn fmt::Write, indent: &str) -> fmt::Result {
        match opcode_name(self.opcode as u32) {
            Some(name) => writeln!(f, "{indent}opcode {} ({name})", self.opcode)?,
            None => writeln!(f, "{indent}opcode {}", self.opcode)?,
        }
        let d = format!("{indent}  ");
        match target_type(self.target as u32) {
            Some(name) => writeln!(f, "{d}{:<9} {} ({name})", "target:", self.target)?,
            None => writeln!(f, "{d}{:<9} {}", "target:", self.target)?,
        }
        writeln!(f, "{d}{:<9} {}", "power:", self.power)?;
        writeln!(
            f,
            "{d}{:<9} p1={} p2={}",
            "params:", self.param1 as i32, self.param2 as i32
        )?;
        writeln!(
            f,
            "{d}{:<9} {} ({}), duration {}",
            "timing:",
            self.timing_mode,
            timing_mode(self.timing_mode as u16),
            self.duration
        )?;
        writeln!(
            f,
            "{d}{:<9} {}..{}",
            "prob:", self.probability1, self.probability2
        )?;
        writeln!(
            f,
            "{d}{:<9} {} (bonus {})",
            "save:",
            save_type(self.save_type),
            self.save_bonus as i32
        )?;
        writeln!(
            f,
            "{d}{:<9} {} ({})",
            "resist:",
            self.dispel,
            dispel_desc(self.dispel as u32)
        )?;
        // Dual-purpose with max/min level for non-damage opcodes, which store
        // the -1 "no limit" sentinel here; render signed so it reads as `-1`.
        if self.dice_thrown != 0 || self.dice_sides != 0 {
            writeln!(
                f,
                "{d}{:<9} {}d{}",
                "dice:", self.dice_thrown as i32, self.dice_sides as i32
            )?;
        }
        if !self.resource.is_empty() {
            writeln!(f, "{d}{:<9} {:?}", "resource:", self.resource)?;
        }
        if self.special != 0 {
            writeln!(f, "{d}{:<9} {}", "special:", self.special as i32)?;
        }
        Ok(())
    }

    /// Write the feature's fields as `key=value` lines, in disk order, exactly as
    /// stored. Each key is prefixed (e.g. `"ext.0.effect.2."`) so the line is
    /// self-locating in the flat machine-readable view.
    pub fn write_raw(&self, out: &mut dyn Write, prefix: &str) -> io::Result<()> {
        writeln!(out, "{prefix}opcode={}", self.opcode)?;
        writeln!(out, "{prefix}target={}", self.target)?;
        writeln!(out, "{prefix}power={}", self.power)?;
        writeln!(out, "{prefix}param1={}", self.param1)?;
        writeln!(out, "{prefix}param2={}", self.param2)?;
        writeln!(out, "{prefix}timing_mode={}", self.timing_mode)?;
        writeln!(out, "{prefix}dispel={}", self.dispel)?;
        writeln!(out, "{prefix}duration={}", self.duration)?;
        writeln!(out, "{prefix}probability1={}", self.probability1)?;
        writeln!(out, "{prefix}probability2={}", self.probability2)?;
        writeln!(out, "{prefix}resource={}", self.resource)?;
        writeln!(out, "{prefix}dice_thrown={}", self.dice_thrown)?;
        writeln!(out, "{prefix}dice_sides={}", self.dice_sides)?;
        writeln!(out, "{prefix}save_type={}", self.save_type)?;
        writeln!(out, "{prefix}save_bonus={}", self.save_bonus)?;
        writeln!(out, "{prefix}special={}", self.special)?;
        Ok(())
    }
}

/// Write a `<label>: <value> (<name>)` line, omitting the parenthetical when
/// the dictionary lookup left the name unresolved.
fn write_type(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    value: u32,
    name: &Option<String>,
) -> fmt::Result {
    let field = format!("{label}:");
    match name {
        Some(name) => writeln!(f, "  {field:<13}{value} ({name})"),
        None => writeln!(f, "  {field:<13}{value}"),
    }
}

/// Name the target of an effect (`Target type`, shared V1/V2), or `None` for a
/// value outside the documented 0-9 range (e.g. the -1 "no target" sentinel).
fn target_type(value: u32) -> Option<&'static str> {
    Some(match value {
        0 => "none",
        1 => "self",
        2 => "projectile target",
        3 => "party",
        4 => "everyone",
        5 => "everyone except party",
        6 => "caster group",
        7 => "target group",
        8 => "everyone except self",
        9 => "original caster",
        _ => return None,
    })
}

/// Name an effect's timing mode (shared V1/V2).
fn timing_mode(value: u16) -> &'static str {
    match value {
        0 => "instant/limited",
        1 => "instant/permanent",
        2 => "instant/while equipped",
        3 => "delay/limited",
        4 => "delay/permanent",
        5 => "delay/while equipped",
        6 => "limited after duration",
        7 => "permanent after duration",
        8 => "equipped after duration",
        9 => "instant/permanent after death",
        10 => "instant/limited (ticks)",
        4096 => "absolute duration",
        _ => "?",
    }
}

/// Describe the dispel/resistance field in its traditional 0-3 form (shared
/// V1/V2). Only the low two bits are interpreted.
fn dispel_desc(value: u32) -> &'static str {
    match value & 0b11 {
        0 => "natural, ignores resistance",
        1 => "dispellable, affected by resistance",
        2 => "undispellable, ignores resistance",
        _ => "dispellable, ignores resistance",
    }
}

/// List the saving-throw categories a save-type bitmask selects (shared V1/V2).
/// Only the five classic categories are named; `none` when none are set.
fn save_type(value: u32) -> String {
    const SAVES: [&str; 5] = ["spells", "breath", "death", "wands", "petrify"];
    let names: Vec<&str> = SAVES
        .iter()
        .enumerate()
        .filter(|(bit, _)| value & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect();

    if names.is_empty() {
        "none".to_string()
    } else {
        names.join("|")
    }
}
