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

use crate::formats::opcode::{
    ResourceSlot, StrrefSlot, opcode_name, opcode_resource_slots, opcode_strref_slots,
};
use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag, parse_asciiz};

/// Resource type code for EFF files.
pub const EFF_TYPE: u16 = 0x03f8;

const EFF_SIGNATURE: &[u8; 4] = b"EFF ";
const EFF_VERSION: &[u8; 4] = b"V2.0";

/// Size of a CRE-embedded V2 effect: the 0x110-byte standalone layout minus its
/// 8-byte outer header. See [`Eff::parse_embedded_at`].
pub const EFF_V2_BODY_SIZE: usize = 0x108;

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
    /// Parse a standalone EFF V2.0 resource. The 0x110-byte file is an 8-byte
    /// outer header (sig/ver) followed by the body, which begins with a repeat
    /// of the sig/ver at 0x08.
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

        Ok(Eff::parse_body(&reader, 0x08)?)
    }

    /// Parse a CRE-embedded V2 effect at `base`. These omit the 8-byte outer
    /// header, so the body (starting with its sig/ver echo) begins directly at
    /// `base` — every field then sits at its standalone offset minus 8. The
    /// total length is [`EFF_V2_BODY_SIZE`]. See `cre_v1.md`.
    pub fn parse_embedded_at(reader: &Reader, base: usize) -> Result<Eff, Truncated> {
        Eff::parse_body(reader, base)
    }

    /// Parse the effect body, whose sig/ver echo sits at `body_base`. Every
    /// other field is read relative to it (opcode at `body_base + 0x08`, …),
    /// which is what makes the standalone (`body_base = 0x08`) and CRE-embedded
    /// (`body_base = base`) forms one code path.
    fn parse_body(reader: &Reader, body_base: usize) -> Result<Eff, Truncated> {
        let at = |off: usize| body_base + off;

        let mut unknown_tail = [0u32; 15];
        for (i, slot) in unknown_tail.iter_mut().enumerate() {
            *slot = reader.u32_at(at(0xcc) + i * 4, "trailing unknown")?;
        }

        Ok(Eff {
            signature2: reader.tag_at(at(0x00), "body signature")?,
            version2: reader.tag_at(at(0x04), "body version")?,
            opcode: reader.u32_at(at(0x08), "opcode")?,
            target: reader.u32_at(at(0x0c), "target type")?,
            power: reader.u32_at(at(0x10), "power")?,
            param1: reader.u32_at(at(0x14), "parameter 1")?,
            param2: reader.u32_at(at(0x18), "parameter 2")?,
            timing_mode: reader.u16_at(at(0x1c), "timing mode")?,
            timing_unknown: reader.u16_at(at(0x1e), "timing (secondary)")?,
            duration: reader.u32_at(at(0x20), "duration")?,
            probability1: reader.u16_at(at(0x24), "probability 1")?,
            probability2: reader.u16_at(at(0x26), "probability 2")?,
            resource: reader.resref_at(at(0x28), "resource")?,
            dice_thrown: reader.u32_at(at(0x30), "dice thrown")?,
            dice_sides: reader.u32_at(at(0x34), "dice sides")?,
            save_type: reader.u32_at(at(0x38), "saving throw type")?,
            save_bonus: reader.u32_at(at(0x3c), "save bonus")?,
            special: reader.u32_at(at(0x40), "special")?,
            primary_type: reader.u32_at(at(0x44), "primary type")?,
            unknown_0x50: reader.u32_at(at(0x48), "unknown")?,
            affected_level_low: reader.u32_at(at(0x4c), "lowest affected level")?,
            affected_level_high: reader.u32_at(at(0x50), "highest affected level")?,
            dispel: reader.u32_at(at(0x54), "dispel/resistance")?,
            param3: reader.u32_at(at(0x58), "parameter 3")?,
            param4: reader.u32_at(at(0x5c), "parameter 4")?,
            param5: reader.u32_at(at(0x60), "parameter 5")?,
            time_applied: reader.u32_at(at(0x64), "time applied")?,
            resource2: reader.resref_at(at(0x68), "resource 2")?,
            resource3: reader.resref_at(at(0x70), "resource 3")?,
            caster_x: reader.u32_at(at(0x78), "caster x")?,
            caster_y: reader.u32_at(at(0x7c), "caster y")?,
            target_x: reader.u32_at(at(0x80), "target x")?,
            target_y: reader.u32_at(at(0x84), "target y")?,
            parent_resource_type: reader.u32_at(at(0x88), "parent resource type")?,
            parent_resource: reader.resref_at(at(0x8c), "parent resource")?,
            parent_resource_flags: reader.u32_at(at(0x94), "parent resource flags")?,
            projectile: reader.u32_at(at(0x98), "projectile")?,
            parent_resource_slot: reader.u32_at(at(0x9c), "parent resource slot")?,
            variable: parse_asciiz(reader.slice(at(0xa0), 32, "variable name")?),
            caster_level: reader.u32_at(at(0xc0), "caster level")?,
            first_apply: reader.u32_at(at(0xc4), "first apply")?,
            secondary_type: reader.u32_at(at(0xc8), "secondary type")?,
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

    /// TLK strrefs this effect references, tagged `op<n>.<field>`. Empty for the
    /// vast majority of opcodes; non-empty for the string-display opcodes whose
    /// `param1`/`special` field holds a strref. See [`effect_strref_uses`].
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        effect_strref_uses(self.opcode, self.param1, self.param2, self.special)
    }

    /// Resources this effect names, tagged `op<n>.<field>` with the type each
    /// names. Empty for most opcodes; non-empty for the ones whose resref
    /// fields carry a resource (see [`opcode_resource_slots`]). All three slots
    /// are reachable here, unlike the V1 [`Feature`], which has only the first.
    ///
    /// `parent_resource` is deliberately excluded: it records the ITM/SPL that
    /// *applied* this effect at runtime, which is provenance rather than a
    /// reference the file makes.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let mut uses = Vec::new();
        for slot in opcode_resource_slots(self.opcode) {
            let value = match slot.slot {
                ResourceSlot::Resource => &self.resource,
                ResourceSlot::Resource2 => &self.resource2,
                ResourceSlot::Resource3 => &self.resource3,
            };
            if value.is_empty() {
                continue;
            }
            let field = resource_slot_name(slot.slot);
            let use_ = ResrefUse::new(slot.kind, value, format!("op{}.{field}", self.opcode));
            uses.push(if slot.soft { use_.soft() } else { use_ });
        }
        uses
    }

    /// Write the effect's fields as `key=value` lines, in disk order, exactly as
    /// stored (unsigned, undecoded). Each key carries `prefix` so the line is
    /// self-locating when embedded (e.g. `"effect.3."` in a CRE); the standalone
    /// resource passes `""`. The format's unknown/structural words and the
    /// resolved dictionary names are intentionally omitted.
    pub fn write_raw(&self, out: &mut dyn Write, prefix: &str) -> io::Result<()> {
        writeln!(out, "{prefix}opcode={}", self.opcode)?;
        writeln!(out, "{prefix}target={}", self.target)?;
        writeln!(out, "{prefix}power={}", self.power)?;
        writeln!(out, "{prefix}param1={}", self.param1)?;
        writeln!(out, "{prefix}param2={}", self.param2)?;
        writeln!(out, "{prefix}timing_mode={}", self.timing_mode)?;
        writeln!(out, "{prefix}duration={}", self.duration)?;
        writeln!(out, "{prefix}probability1={}", self.probability1)?;
        writeln!(out, "{prefix}probability2={}", self.probability2)?;
        writeln!(out, "{prefix}resource={}", self.resource)?;
        writeln!(out, "{prefix}dice_thrown={}", self.dice_thrown)?;
        writeln!(out, "{prefix}dice_sides={}", self.dice_sides)?;
        writeln!(out, "{prefix}save_type={}", self.save_type)?;
        writeln!(out, "{prefix}save_bonus={}", self.save_bonus)?;
        writeln!(out, "{prefix}special={}", self.special)?;
        writeln!(out, "{prefix}primary_type={}", self.primary_type)?;
        writeln!(
            out,
            "{prefix}affected_level_low={}",
            self.affected_level_low
        )?;
        writeln!(
            out,
            "{prefix}affected_level_high={}",
            self.affected_level_high
        )?;
        writeln!(out, "{prefix}dispel={}", self.dispel)?;
        writeln!(out, "{prefix}param3={}", self.param3)?;
        writeln!(out, "{prefix}param4={}", self.param4)?;
        writeln!(out, "{prefix}param5={}", self.param5)?;
        writeln!(out, "{prefix}time_applied={}", self.time_applied)?;
        writeln!(out, "{prefix}resource2={}", self.resource2)?;
        writeln!(out, "{prefix}resource3={}", self.resource3)?;
        writeln!(out, "{prefix}caster_x={}", self.caster_x)?;
        writeln!(out, "{prefix}caster_y={}", self.caster_y)?;
        writeln!(out, "{prefix}target_x={}", self.target_x)?;
        writeln!(out, "{prefix}target_y={}", self.target_y)?;
        writeln!(
            out,
            "{prefix}parent_resource_type={}",
            self.parent_resource_type
        )?;
        writeln!(out, "{prefix}parent_resource={}", self.parent_resource)?;
        writeln!(
            out,
            "{prefix}parent_resource_flags={}",
            self.parent_resource_flags
        )?;
        writeln!(out, "{prefix}projectile={}", self.projectile)?;
        writeln!(
            out,
            "{prefix}parent_resource_slot={}",
            self.parent_resource_slot
        )?;
        writeln!(out, "{prefix}variable={}", self.variable)?;
        writeln!(out, "{prefix}caster_level={}", self.caster_level)?;
        writeln!(out, "{prefix}first_apply={}", self.first_apply)?;
        writeln!(out, "{prefix}secondary_type={}", self.secondary_type)?;
        Ok(())
    }
}

impl Eff {
    /// Render the authored effect fields, each line prefixed with `indent`. The
    /// optional resrefs, dice, and extra parameters appear only when they carry
    /// a value. Shared by the standalone [`Display`](fmt::Display) (indent
    /// `"  "`) and the CRE effect list, which nests it deeper.
    pub fn write_human(&self, f: &mut dyn fmt::Write, indent: &str) -> fmt::Result {
        match opcode_name(self.opcode) {
            Some(name) => writeln!(f, "{indent}{:<13}{} ({name})", "opcode:", self.opcode)?,
            None => writeln!(f, "{indent}{:<13}{}", "opcode:", self.opcode)?,
        }
        // Many authored effects store -1 in target/parameter fields as a "none"
        // sentinel, so these read as signed; annotate the target only when it
        // names a known type.
        match target_type(self.target) {
            Some(name) => writeln!(
                f,
                "{indent}{:<13}{} ({name})",
                "target:", self.target as i32
            )?,
            None => writeln!(f, "{indent}{:<13}{}", "target:", self.target as i32)?,
        }
        writeln!(f, "{indent}{:<13}{}", "power:", self.power)?;
        writeln!(
            f,
            "{indent}{:<13}p1={} p2={}",
            "parameters:", self.param1 as i32, self.param2 as i32
        )?;
        writeln!(
            f,
            "{indent}{:<13}{} ({}), duration {}",
            "timing:",
            self.timing_mode,
            timing_mode(self.timing_mode),
            self.duration
        )?;
        writeln!(
            f,
            "{indent}{:<13}{}..{}",
            "probability:", self.probability1, self.probability2
        )?;
        writeln!(
            f,
            "{indent}{:<13}{} (bonus {})",
            "save:",
            save_type(self.save_type),
            self.save_bonus as i32
        )?;
        writeln!(
            f,
            "{indent}{:<13}{} ({})",
            "resistance:",
            self.dispel,
            dispel_desc(self.dispel)
        )?;
        write_type(f, indent, "primary", self.primary_type, &self.primary_name)?;
        write_type(
            f,
            indent,
            "secondary",
            self.secondary_type,
            &self.secondary_name,
        )?;

        // Dual-purpose with max/min level for non-damage opcodes, which store
        // the -1 "no limit" sentinel here; render signed so it reads as `-1`.
        if self.dice_thrown != 0 || self.dice_sides != 0 {
            writeln!(
                f,
                "{indent}{:<13}{}d{}",
                "dice:", self.dice_thrown as i32, self.dice_sides as i32
            )?;
        }
        if self.param3 != 0 || self.param4 != 0 || self.param5 != 0 {
            writeln!(
                f,
                "{indent}{:<13}{} {} {}",
                "params 3-5:", self.param3 as i32, self.param4 as i32, self.param5 as i32
            )?;
        }
        if self.special != 0 {
            writeln!(f, "{indent}{:<13}{}", "special:", self.special as i32)?;
        }
        if self.affected_level_low != 0 || self.affected_level_high != 0 {
            writeln!(
                f,
                "{indent}{:<13}{}-{}",
                "levels:", self.affected_level_low, self.affected_level_high
            )?;
        }
        if self.projectile != 0 {
            writeln!(f, "{indent}{:<13}{}", "projectile:", self.projectile)?;
        }
        if !self.resource.is_empty() {
            writeln!(f, "{indent}{:<13}{:?}", "resource:", self.resource)?;
        }
        if !self.resource2.is_empty() {
            writeln!(f, "{indent}{:<13}{:?}", "resource 2:", self.resource2)?;
        }
        if !self.resource3.is_empty() {
            writeln!(f, "{indent}{:<13}{:?}", "resource 3:", self.resource3)?;
        }
        if !self.parent_resource.is_empty() {
            writeln!(f, "{indent}{:<13}{:?}", "parent:", self.parent_resource)?;
        }
        if !self.variable.is_empty() {
            writeln!(f, "{indent}{:<13}{:?}", "variable:", self.variable)?;
        }

        Ok(())
    }
}

impl fmt::Display for Eff {
    /// Human-readable view of a standalone EFF V2.0 resource.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "EFF V2.0")?;
        self.write_human(f, "  ")
    }
}

/// The strref field uses for an effect with these field values, tagged
/// `op<n>.<field>`. Shared by the standalone/CRE-embedded V2 [`Eff`] and the
/// ITM/SPL/CRE V1 [`Feature`], which store the same effect at different widths.
/// Table-driven from [`opcode_strref_slots`], plus op319's value-dependent
/// Parameter #1 — a strref (the actor name to match) when Parameter #2, the IDS
/// file, selects "Actor's name" (10). The embedding resource prefixes the paths
/// with the effect's location (e.g. `equipping[0].`).
fn effect_strref_uses(opcode: u32, param1: u32, param2: u32, special: u32) -> Vec<StrrefUse> {
    let mut uses: Vec<StrrefUse> = opcode_strref_slots(opcode)
        .iter()
        .map(|slot| {
            let (value, field) = match slot {
                StrrefSlot::Param1 => (param1, "param1"),
                StrrefSlot::Special => (special, "special"),
            };
            StrrefUse::new(value, format!("op{opcode}.{field}"))
        })
        .collect();
    if opcode == 319 && param2 == 10 {
        uses.push(StrrefUse::new(param1, format!("op{opcode}.param1")));
    }
    uses
}

/// Field name for a resource slot, as it appears in a reference path
/// (`op122.resource2`). Matches the `write_raw` keys.
fn resource_slot_name(slot: ResourceSlot) -> &'static str {
    match slot {
        ResourceSlot::Resource => "resource",
        ResourceSlot::Resource2 => "resource2",
        ResourceSlot::Resource3 => "resource3",
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

    /// TLK strrefs this feature references, tagged `op<n>.<field>`. Non-empty only
    /// for string-display opcodes. See [`effect_strref_uses`].
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        effect_strref_uses(self.opcode as u32, self.param1, self.param2, self.special)
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

    /// Resources this effect names, tagged `op<n>.resource`. The V1 layout has
    /// a single resref field, so any [`ResourceSlot::Resource2`]/`Resource3`
    /// the opcode would use in a V2 effect simply isn't there to read — the
    /// `Resource`-slot filter below is what keeps this honest rather than
    /// reporting the same resref three times for opcode #122.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        if self.resource.is_empty() {
            return Vec::new();
        }
        opcode_resource_slots(self.opcode as u32)
            .iter()
            .filter(|slot| slot.slot == ResourceSlot::Resource)
            .map(|slot| {
                let use_ = ResrefUse::new(
                    slot.kind,
                    &self.resource,
                    format!("op{}.resource", self.opcode),
                );
                if slot.soft { use_.soft() } else { use_ }
            })
            .collect()
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
    f: &mut dyn fmt::Write,
    indent: &str,
    label: &str,
    value: u32,
    name: &Option<String>,
) -> fmt::Result {
    let field = format!("{label}:");
    match name {
        Some(name) => writeln!(f, "{indent}{field:<13}{value} ({name})"),
        None => writeln!(f, "{indent}{field:<13}{value}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::itm::ITM_TYPE;
    use crate::formats::spl::SPL_TYPE;

    /// A 48-byte V1 feature block with the fields a strref test cares about.
    fn feature(opcode: u16, param1: u32, param2: u32, special: u32) -> Feature {
        let mut data = vec![0u8; FEATURE_SIZE];
        data[0x00..0x02].copy_from_slice(&opcode.to_le_bytes());
        data[0x04..0x08].copy_from_slice(&param1.to_le_bytes());
        data[0x08..0x0c].copy_from_slice(&param2.to_le_bytes());
        data[0x2c..0x30].copy_from_slice(&special.to_le_bytes());
        Feature::parse_at(&Reader::new(&data), 0).expect("parse feature")
    }

    fn pairs(uses: &[StrrefUse]) -> Vec<(u32, &str)> {
        uses.iter().map(|u| (u.value, u.path.as_str())).collect()
    }

    #[test]
    fn display_string_opcode_uses_param1() {
        assert_eq!(
            pairs(&feature(139, 6831, 0, 0).strref_uses()),
            vec![(6831, "op139.param1")]
        );
    }

    #[test]
    fn item_usability_uses_special_plus_conditional_param1() {
        // Special is always a strref; Parameter #1 only when the IDS file is 10.
        assert_eq!(
            pairs(&feature(319, 100, 9, 200).strref_uses()),
            vec![(200, "op319.special")]
        );
        assert_eq!(
            pairs(&feature(319, 100, 10, 200).strref_uses()),
            vec![(200, "op319.special"), (100, "op319.param1")]
        );
    }

    #[test]
    fn non_string_opcode_has_no_strrefs() {
        assert!(feature(1, 5, 5, 5).strref_uses().is_empty());
    }

    #[test]
    fn standalone_eff_shares_the_slot_table() {
        let mut data = vec![0u8; 0x110];
        data[0x00..0x04].copy_from_slice(EFF_SIGNATURE);
        data[0x04..0x08].copy_from_slice(EFF_VERSION);
        data[0x10..0x14].copy_from_slice(&139u32.to_le_bytes()); // opcode (body 0x08)
        data[0x1c..0x20].copy_from_slice(&4242u32.to_le_bytes()); // param1 (body 0x14)
        let eff = Eff::parse(&data).expect("parse eff");
        assert_eq!(pairs(&eff.strref_uses()), vec![(4242, "op139.param1")]);
    }

    /// A 48-byte V1 feature block carrying a resref, for the resource tests.
    fn feature_with_resource(opcode: u16, resource: &str) -> Feature {
        let mut data = vec![0u8; FEATURE_SIZE];
        data[0x00..0x02].copy_from_slice(&opcode.to_le_bytes());
        let bytes = resource.as_bytes();
        data[0x14..0x14 + bytes.len()].copy_from_slice(bytes);
        Feature::parse_at(&Reader::new(&data), 0).expect("parse feature")
    }

    /// A standalone V2 EFF with an opcode and up to three resrefs.
    fn eff_with_resources(opcode: u32, resources: [&str; 3]) -> Eff {
        let mut data = vec![0u8; 0x110];
        data[0x00..0x04].copy_from_slice(EFF_SIGNATURE);
        data[0x04..0x08].copy_from_slice(EFF_VERSION);
        data[0x10..0x14].copy_from_slice(&opcode.to_le_bytes()); // body 0x08
        // Body starts at 0x08, so resource/2/3 (0x28/0x68/0x70) land here.
        for (at, value) in [
            (0x30, resources[0]),
            (0x70, resources[1]),
            (0x78, resources[2]),
        ] {
            data[at..at + value.len()].copy_from_slice(value.as_bytes());
        }
        Eff::parse(&data).expect("parse eff")
    }

    fn refs<'a>(uses: &'a [ResrefUse<'a>]) -> Vec<(u16, &'a str, &'a str, bool)> {
        uses.iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str(), u.soft))
            .collect()
    }

    #[test]
    fn item_opcode_resource_is_reported_as_an_itm() {
        let feat = feature_with_resource(111, "S!SPSH01");
        assert_eq!(
            refs(&feat.resref_uses()),
            vec![(ITM_TYPE, "S!SPSH01", "op111.resource", false)]
        );
    }

    #[test]
    fn spell_or_item_opcodes_offer_both_readings_as_candidates() {
        // #318/#321/#324 take "the spell or item specified in the Resource
        // field". One slot, two types: a search for either finds it, and
        // neither reading is asserted as fact.
        for opcode in [318, 321, 324] {
            let feat = feature_with_resource(opcode, "SPPR309");
            let path = format!("op{opcode}.resource");
            assert_eq!(
                refs(&feat.resref_uses()),
                vec![
                    (ITM_TYPE, "SPPR309", &path[..], true),
                    (SPL_TYPE, "SPPR309", &path[..], true),
                ],
                "opcode {opcode} should offer both readings, both soft"
            );
        }
    }

    #[test]
    fn opcode_122_reads_all_three_resources_only_in_v2() {
        // In a V2 effect the EE picks at random among the three, so all three
        // name items.
        let eff = eff_with_resources(122, ["POTN42", "POTN52", "POTN55"]);
        assert_eq!(
            refs(&eff.resref_uses()),
            vec![
                (ITM_TYPE, "POTN42", "op122.resource", false),
                (ITM_TYPE, "POTN52", "op122.resource2", false),
                (ITM_TYPE, "POTN55", "op122.resource3", false),
            ]
        );
        // A V1 feature has only the first resref field; the other two slots must
        // not re-report it under resource2/resource3.
        let feat = feature_with_resource(122, "POTN42");
        assert_eq!(
            refs(&feat.resref_uses()),
            vec![(ITM_TYPE, "POTN42", "op122.resource", false)]
        );
    }

    #[test]
    fn opcodes_without_a_resource_report_nothing() {
        // A non-resource opcode, and a resource opcode with the field empty.
        assert!(feature_with_resource(1, "SW1H01").resref_uses().is_empty());
        assert!(feature_with_resource(111, "").resref_uses().is_empty());
        // #182 names two different types: the ITM to check for, and the EFF to
        // apply while it is equipped.
        let eff = eff_with_resources(182, ["SW1H01", "SOMEEFF", ""]);
        assert_eq!(
            refs(&eff.resref_uses()),
            vec![
                (ITM_TYPE, "SW1H01", "op182.resource", false),
                (EFF_TYPE, "SOMEEFF", "op182.resource2", false),
            ]
        );
    }
}
