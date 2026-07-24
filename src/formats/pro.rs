//! PRO parser.
//!
//! A projectile: graphics, speed, area-of-effect, and sound for the things a
//! spell or projectile weapon hurls at a target. Unlike [ITM](super::itm) /
//! [SPL](super::spl), there are no offset/count arrays and no effect pool —
//! just three fixed 256-byte structs laid end to end, switched on by the
//! projectile-type word at `0x08`:
//!
//! - type 0/1 (No Projectile / No BAM): the base header only (256 bytes).
//! - type 2 (Single Target): base + travel struct (512 bytes).
//! - type 3 (Area of Effect): base + travel + area struct (768 bytes).
//!
//! All offsets are absolute from the file start. Spec:
//! `md/file_formats/ie_formats/pro_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::formats::fields::{set_flags, write_bitfield};
use crate::formats::spl::SPL_TYPE;
use crate::formats::{BAM_TYPE, BMP_TYPE, WAV_TYPE};
use crate::{ProjectileUse, Reader, ResrefUse, StrrefUse, Truncated, describe_tag, join_bytes};

/// Resource type code for PRO files.
pub const PRO_TYPE: u16 = 0x03fd;

const PRO_SIGNATURE: &[u8; 4] = b"PRO ";
const PRO_VERSION: &[u8; 4] = b"V1.0";

/// Base of the travel struct (present for single-target and area projectiles).
const TRAVEL_BASE: usize = 0x100;
/// Base of the area struct (present for area projectiles only).
const AREA_BASE: usize = 0x200;

/// A parsed PRO file: the base header plus the travel/area structs that the
/// projectile type calls for, with names the manager has resolved attached.
pub struct Pro {
    header: ProHeader,
    travel: Option<ProTravel>,
    area: Option<ProArea>,
    particle_color_name: Option<String>,
    message_name: Option<String>,
    default_spell_name: Option<String>,
    success_spell_name: Option<String>,
}

impl Pro {
    /// Parse a PRO V1 resource. The projectile type word governs how many of the
    /// fixed structs follow the base header.
    pub fn parse(data: &[u8]) -> Result<Pro, ProError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != PRO_SIGNATURE {
            return Err(ProError::BadSignature(signature));
        }

        let version = reader.tag_at(0x04, "version")?;
        if &version != PRO_VERSION {
            return Err(ProError::BadVersion(version));
        }

        let header = ProHeader::parse(&reader)?;

        // Single-target (2) and area (3) projectiles carry the travel struct;
        // only area projectiles carry the area struct on top of that.
        let travel = if header.projectile_type >= 2 {
            Some(ProTravel::parse(&reader)?)
        } else {
            None
        };

        let area = if header.projectile_type == 3 {
            Some(ProArea::parse(&reader)?)
        } else {
            None
        };

        Ok(Pro {
            header,
            travel,
            area,
            particle_color_name: None,
            message_name: None,
            default_spell_name: None,
            success_spell_name: None,
        })
    }

    /// Particle-color index (SPRKLCLR.2DA row).
    pub fn particle_color(&self) -> u16 {
        self.header.particle_color
    }

    /// Strref of the displayed targeting message.
    pub fn message_strref(&self) -> u32 {
        self.header.message
    }

    /// Every TLK strref this projectile references: just the targeting message.
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        vec![StrrefUse::new(self.header.message, "message")]
    }

    /// Every resource this projectile names, tagged with the type it names: its
    /// sounds, animations, palette, and the two spells it can cast. The
    /// animations the engine resolves through a VEF → VVC → BAM fallback chain
    /// are tagged BAM but marked soft, since any of the three can satisfy them.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let h = &self.header;
        let mut uses = vec![
            ResrefUse::new(WAV_TYPE, &h.fire_sound, "fire_sound"),
            ResrefUse::new(WAV_TYPE, &h.impact_sound, "impact_sound"),
            ResrefUse::new(BAM_TYPE, &h.source_animation, "source_animation").soft(),
            ResrefUse::new(SPL_TYPE, &h.default_spell, "default_spell"),
            ResrefUse::new(SPL_TYPE, &h.success_spell, "success_spell"),
        ];
        if let Some(t) = &self.travel {
            uses.push(ResrefUse::new(
                BAM_TYPE,
                &t.projectile_animation,
                "travel.projectile_animation",
            ));
            uses.push(ResrefUse::new(
                BAM_TYPE,
                &t.shadow_animation,
                "travel.shadow_animation",
            ));
            uses.push(ResrefUse::new(BMP_TYPE, &t.palette, "travel.palette"));
            for (i, anim) in t.trailing_animation.iter().enumerate() {
                uses.push(ResrefUse::new(
                    BAM_TYPE,
                    anim,
                    format!("travel.trailing_animation[{i}]"),
                ));
            }
        }
        if let Some(a) = &self.area {
            uses.push(ResrefUse::new(
                WAV_TYPE,
                &a.explosion_sound,
                "area.explosion_sound",
            ));
            uses.push(ResrefUse::new(WAV_TYPE, &a.area_sound, "area.area_sound"));
            uses.push(
                ResrefUse::new(BAM_TYPE, &a.explosion_animation, "area.explosion_animation").soft(),
            );
            uses.push(
                ResrefUse::new(BAM_TYPE, &a.spread_animation, "area.spread_animation").soft(),
            );
            uses.push(ResrefUse::new(BAM_TYPE, &a.ring_animation, "area.ring_animation").soft());
        }
        uses.retain(|u| !u.value.is_empty());
        uses
    }

    /// The projectiles this one fires, named by number ([`ProjectileUse`]).
    ///
    /// The explosion projectile only. Its *secondary* neighbour is documented
    /// as `PROJECTL.IDS - 1`, but of the three values that resolve at all in
    /// the install, `ICEGLYP` fits at no offset and `LIGHTB` at plus one — too
    /// little to choose a reading, and a wrong one invents references.
    pub fn projectile_uses(&self) -> Vec<ProjectileUse> {
        self.area_codes()
            .and_then(|codes| codes.explosion_projectile_ids())
            .map(|number| ProjectileUse::new(number, "area.explosion_projectile"))
            .into_iter()
            .collect()
    }

    /// Resref of the spell cast when the projectile misses (`Default spell`).
    pub fn default_spell(&self) -> &str {
        &self.header.default_spell
    }

    /// Resref of the spell cast on success (`Success spell`).
    pub fn success_spell(&self) -> &str {
        &self.header.success_spell
    }

    /// Attach the resolved header-level names.
    pub fn set_header_names(
        &mut self,
        particle_color: Option<String>,
        message: Option<String>,
        default_spell: Option<String>,
        success_spell: Option<String>,
    ) {
        self.particle_color_name = particle_color;
        self.message_name = message;
        self.default_spell_name = default_spell;
        self.success_spell_name = success_spell;
    }

    /// The travel struct's smoke-animation code (animate.ids), if present.
    pub fn travel_smoke(&self) -> Option<u16> {
        self.travel.as_ref().map(|t| t.smoke_animation)
    }

    /// Attach the resolved smoke-animation name to the travel struct.
    pub fn set_travel_smoke_name(&mut self, name: Option<String>) {
        if let Some(travel) = self.travel.as_mut() {
            travel.smoke_animation_name = name;
        }
    }

    /// The area struct's IDS-resolvable codes, if this is an area projectile.
    pub fn area_codes(&self) -> Option<AreaCodes> {
        self.area.as_ref().map(ProArea::codes)
    }

    /// Attach the resolved area-struct names.
    pub fn set_area_names(
        &mut self,
        fragment: Option<String>,
        secondary_projectile: Option<String>,
        explosion_projectile: Option<String>,
        explosion_effect: Option<String>,
    ) {
        if let Some(area) = self.area.as_mut() {
            area.fragment_animation_name = fragment;
            area.secondary_projectile_name = secondary_projectile;
            area.explosion_projectile_name = explosion_projectile;
            area.explosion_effect_name = explosion_effect;
        }
    }

    /// Write the whole projectile as flat `key=value` lines: base header fields
    /// bare, the travel struct under `travel.`, the area struct under `area.`.
    /// Structs absent for this projectile type are simply omitted, keeping the
    /// output a faithful image of the 256/512/768-byte file.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        let h = &self.header;
        writeln!(out, "projectile_type={}", h.projectile_type)?;
        writeln!(out, "speed={}", h.speed)?;
        writeln!(out, "behaviour_flags={}", h.behaviour_flags)?;
        writeln!(out, "fire_sound={}", h.fire_sound)?;
        writeln!(out, "impact_sound={}", h.impact_sound)?;
        writeln!(out, "source_animation={}", h.source_animation)?;
        writeln!(out, "particle_color={}", h.particle_color)?;
        writeln!(out, "width={}", h.width)?;
        writeln!(out, "ext_flags={}", h.ext_flags)?;
        writeln!(out, "message={}", h.message)?;
        writeln!(out, "pulse_color={}", h.pulse_color)?;
        writeln!(out, "color_speed={}", h.color_speed)?;
        writeln!(out, "screen_shake={}", h.screen_shake)?;
        writeln!(out, "creature_value_1={}", h.creature_value_1)?;
        writeln!(out, "creature_type_1={}", h.creature_type_1)?;
        writeln!(out, "creature_value_2={}", h.creature_value_2)?;
        writeln!(out, "creature_type_2={}", h.creature_type_2)?;
        writeln!(out, "default_spell={}", h.default_spell)?;
        writeln!(out, "success_spell={}", h.success_spell)?;
        writeln!(out, "angle_increase_min={}", h.angle_increase_min)?;
        writeln!(out, "angle_increase_max={}", h.angle_increase_max)?;
        writeln!(out, "curve_min={}", h.curve_min)?;
        writeln!(out, "curve_max={}", h.curve_max)?;
        writeln!(out, "thac0_bonus={}", h.thac0_bonus)?;
        writeln!(out, "thac0_bonus_nonactor={}", h.thac0_bonus_nonactor)?;
        writeln!(out, "radius_min={}", h.radius_min)?;
        writeln!(out, "radius_max={}", h.radius_max)?;

        if let Some(t) = &self.travel {
            writeln!(out, "travel.bam_flags={}", t.bam_flags)?;
            writeln!(
                out,
                "travel.projectile_animation={}",
                t.projectile_animation
            )?;
            writeln!(out, "travel.shadow_animation={}", t.shadow_animation)?;
            writeln!(out, "travel.projectile_number={}", t.projectile_number)?;
            writeln!(out, "travel.shadow_number={}", t.shadow_number)?;
            writeln!(
                out,
                "travel.light_spot_intensity={}",
                t.light_spot_intensity
            )?;
            writeln!(out, "travel.light_spot_width={}", t.light_spot_width)?;
            writeln!(out, "travel.light_spot_height={}", t.light_spot_height)?;
            writeln!(out, "travel.palette={}", t.palette)?;
            writeln!(
                out,
                "travel.projectile_colors={}",
                join_bytes(&t.projectile_colors)
            )?;
            writeln!(out, "travel.smoke_puff_delay={}", t.smoke_puff_delay)?;
            writeln!(out, "travel.smoke_colors={}", join_bytes(&t.smoke_colors))?;
            writeln!(out, "travel.face_target={}", t.face_target)?;
            writeln!(out, "travel.smoke_animation={}", t.smoke_animation)?;
            for (i, anim) in t.trailing_animation.iter().enumerate() {
                writeln!(out, "travel.trailing_animation_{}={}", i + 1, anim)?;
            }
            for (i, num) in t.trailing_number.iter().enumerate() {
                writeln!(out, "travel.trailing_number_{}={}", i + 1, num)?;
            }
            writeln!(out, "travel.puff_flags={}", t.puff_flags)?;
        }

        if let Some(a) = &self.area {
            writeln!(out, "area.area_flags={}", a.area_flags)?;
            writeln!(out, "area.ray_count={}", a.ray_count)?;
            writeln!(out, "area.trap_size={}", a.trap_size)?;
            writeln!(out, "area.explosion_size={}", a.explosion_size)?;
            writeln!(out, "area.explosion_sound={}", a.explosion_sound)?;
            writeln!(out, "area.explosion_frequency={}", a.explosion_frequency)?;
            writeln!(out, "area.fragment_animation={}", a.fragment_animation)?;
            writeln!(out, "area.secondary_projectile={}", a.secondary_projectile)?;
            writeln!(out, "area.repetitions={}", a.repetitions)?;
            writeln!(out, "area.explosion_effect={}", a.explosion_effect)?;
            writeln!(out, "area.explosion_color={}", a.explosion_color)?;
            writeln!(out, "area.explosion_projectile={}", a.explosion_projectile)?;
            writeln!(out, "area.explosion_animation={}", a.explosion_animation)?;
            writeln!(out, "area.cone_width={}", a.cone_width)?;
            writeln!(out, "area.rotate_rays={}", a.rotate_rays)?;
            writeln!(out, "area.spread_animation={}", a.spread_animation)?;
            writeln!(out, "area.ring_animation={}", a.ring_animation)?;
            writeln!(out, "area.area_sound={}", a.area_sound)?;
            writeln!(out, "area.area_ext_flags={}", a.area_ext_flags)?;
            writeln!(out, "area.dice_count={}", a.dice_count)?;
            writeln!(out, "area.dice_size={}", a.dice_size)?;
            writeln!(out, "area.anim_granularity={}", a.anim_granularity)?;
            writeln!(
                out,
                "area.anim_granularity_divider={}",
                a.anim_granularity_divider
            )?;
        }

        Ok(())
    }
}

impl fmt::Display for Pro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = &self.header;
        writeln!(f, "PRO V1")?;
        writeln!(
            f,
            "  type:        {} ({})",
            h.projectile_type,
            projectile_type_name(h.projectile_type)
        )?;
        if h.speed != 0 {
            writeln!(f, "  speed:       {}", h.speed)?;
        }
        if h.behaviour_flags != 0 {
            write_bitfield(
                f,
                "  behaviour:   ",
                h.behaviour_flags,
                &behaviour_flags(h.behaviour_flags),
            )?;
        }
        if h.ext_flags != 0 {
            write_bitfield(f, "  ext flags:   ", h.ext_flags, &ext_flags(h.ext_flags))?;
        }
        if !h.fire_sound.is_empty() {
            writeln!(f, "  fire sound:  {:?}", h.fire_sound)?;
        }
        if !h.impact_sound.is_empty() {
            writeln!(f, "  hit sound:   {:?}", h.impact_sound)?;
        }
        if !h.source_animation.is_empty() {
            writeln!(f, "  source anim: {:?}", h.source_animation)?;
        }
        if h.particle_color != 0 {
            write_named(
                f,
                "  particle:    ",
                h.particle_color,
                &self.particle_color_name,
            )?;
        }
        if h.width != 0 {
            writeln!(f, "  width:       {}", h.width)?;
        }
        if h.screen_shake != 0 {
            writeln!(f, "  shake:       {}", h.screen_shake)?;
        }
        if h.message != u32::MAX && h.message != 0 {
            match &self.message_name {
                Some(name) => writeln!(f, "  message:     {} ({name:?})", h.message)?,
                None => writeln!(f, "  message:     {}", h.message)?,
            }
        }
        write_spell(
            f,
            "  default spl:",
            &h.default_spell,
            &self.default_spell_name,
        )?;
        write_spell(
            f,
            "  success spl:",
            &h.success_spell,
            &self.success_spell_name,
        )?;

        if let Some(t) = &self.travel {
            writeln!(f)?;
            writeln!(f, "  travel:")?;
            t.write_human(f)?;
        }

        if let Some(a) = &self.area {
            writeln!(f)?;
            writeln!(f, "  area:")?;
            a.write_human(f)?;
        }

        Ok(())
    }
}

/// The 256-byte base header, present in every PRO. The PSTEE-only angle/curve/
/// THAC0/radius fields are retained verbatim for the raw view but skipped in the
/// human view, since they are inert in BG2:EE.
struct ProHeader {
    projectile_type: u16,
    speed: u16,
    behaviour_flags: u32,
    fire_sound: String,
    impact_sound: String,
    source_animation: String,
    particle_color: u16,
    width: u16,
    ext_flags: u32,
    message: u32,
    pulse_color: u32,
    color_speed: u16,
    screen_shake: u16,
    creature_value_1: u16,
    creature_type_1: u16,
    creature_value_2: u16,
    creature_type_2: u16,
    default_spell: String,
    success_spell: String,
    angle_increase_min: u16,
    angle_increase_max: u16,
    curve_min: u16,
    curve_max: u16,
    thac0_bonus: u16,
    thac0_bonus_nonactor: u16,
    radius_min: u16,
    radius_max: u16,
}

impl ProHeader {
    fn parse(reader: &Reader) -> Result<ProHeader, Truncated> {
        Ok(ProHeader {
            projectile_type: reader.u16_at(0x08, "projectile type")?,
            speed: reader.u16_at(0x0a, "speed")?,
            behaviour_flags: reader.u32_at(0x0c, "behaviour flags")?,
            fire_sound: reader.resref_at(0x10, "fire sound")?,
            impact_sound: reader.resref_at(0x18, "impact sound")?,
            source_animation: reader.resref_at(0x20, "source animation")?,
            particle_color: reader.u16_at(0x28, "particle color")?,
            width: reader.u16_at(0x2a, "width")?,
            ext_flags: reader.u32_at(0x2c, "extended flags")?,
            message: reader.u32_at(0x30, "displayed message")?,
            pulse_color: reader.u32_at(0x34, "pulse color")?,
            color_speed: reader.u16_at(0x38, "color speed")?,
            screen_shake: reader.u16_at(0x3a, "screen shake")?,
            creature_value_1: reader.u16_at(0x3c, "creature value 1")?,
            creature_type_1: reader.u16_at(0x3e, "creature type 1")?,
            creature_value_2: reader.u16_at(0x40, "creature value 2")?,
            creature_type_2: reader.u16_at(0x42, "creature type 2")?,
            default_spell: reader.resref_at(0x44, "default spell")?,
            success_spell: reader.resref_at(0x4c, "success spell")?,
            angle_increase_min: reader.u16_at(0x54, "angle increase min")?,
            angle_increase_max: reader.u16_at(0x56, "angle increase max")?,
            curve_min: reader.u16_at(0x58, "curve min")?,
            curve_max: reader.u16_at(0x5a, "curve max")?,
            thac0_bonus: reader.u16_at(0x5c, "thac0 bonus")?,
            thac0_bonus_nonactor: reader.u16_at(0x5e, "thac0 bonus (non-actor)")?,
            radius_min: reader.u16_at(0x60, "radius min")?,
            radius_max: reader.u16_at(0x62, "radius max")?,
        })
    }
}

/// The 256-byte travel struct: the moving projectile's BAM graphics, lighting,
/// trailing animations, and smoke.
struct ProTravel {
    bam_flags: u32,
    projectile_animation: String,
    shadow_animation: String,
    projectile_number: u8,
    shadow_number: u8,
    light_spot_intensity: u16,
    light_spot_width: u16,
    light_spot_height: u16,
    palette: String,
    projectile_colors: [u8; 7],
    smoke_puff_delay: u8,
    smoke_colors: [u8; 7],
    face_target: u8,
    smoke_animation: u16,
    trailing_animation: [String; 3],
    trailing_number: [u16; 3],
    puff_flags: u32,
    /// Smoke animation name, resolved by the manager from `animate.ids`.
    smoke_animation_name: Option<String>,
}

impl ProTravel {
    fn parse(reader: &Reader) -> Result<ProTravel, Truncated> {
        let b = TRAVEL_BASE;
        Ok(ProTravel {
            bam_flags: reader.u32_at(b, "bam flags")?,
            projectile_animation: reader.resref_at(b + 0x04, "projectile animation")?,
            shadow_animation: reader.resref_at(b + 0x0c, "shadow animation")?,
            projectile_number: reader.u8_at(b + 0x14, "projectile number")?,
            shadow_number: reader.u8_at(b + 0x15, "shadow number")?,
            light_spot_intensity: reader.u16_at(b + 0x16, "light spot intensity")?,
            light_spot_width: reader.u16_at(b + 0x18, "light spot width")?,
            light_spot_height: reader.u16_at(b + 0x1a, "light spot height")?,
            palette: reader.resref_at(b + 0x1c, "palette")?,
            projectile_colors: reader.array_at(b + 0x24, "projectile colors")?,
            smoke_puff_delay: reader.u8_at(b + 0x2b, "smoke puff delay")?,
            smoke_colors: reader.array_at(b + 0x2c, "smoke colors")?,
            face_target: reader.u8_at(b + 0x33, "face target granularity")?,
            smoke_animation: reader.u16_at(b + 0x34, "smoke animation")?,
            trailing_animation: [
                reader.resref_at(b + 0x36, "trailing animation 1")?,
                reader.resref_at(b + 0x3e, "trailing animation 2")?,
                reader.resref_at(b + 0x46, "trailing animation 3")?,
            ],
            trailing_number: [
                reader.u16_at(b + 0x4e, "trailing number 1")?,
                reader.u16_at(b + 0x50, "trailing number 2")?,
                reader.u16_at(b + 0x52, "trailing number 3")?,
            ],
            puff_flags: reader.u32_at(b + 0x54, "puff flags")?,
            smoke_animation_name: None,
        })
    }

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bam_flags != 0 {
            write_bitfield(
                f,
                &label("bam flags:"),
                self.bam_flags,
                &bam_flags(self.bam_flags),
            )?;
        }
        if !self.projectile_animation.is_empty() {
            writeln!(f, "{}{:?}", label("animation:"), self.projectile_animation)?;
        }
        if self.projectile_number != 0 {
            writeln!(f, "{}{}", label("sequence:"), self.projectile_number)?;
        }
        if !self.shadow_animation.is_empty() {
            writeln!(f, "{}{:?}", label("shadow:"), self.shadow_animation)?;
        }
        if !self.palette.is_empty() {
            writeln!(f, "{}{:?}", label("palette:"), self.palette)?;
        }
        if self.face_target != 0 {
            writeln!(
                f,
                "{}{} ({})",
                label("face:"),
                self.face_target,
                face_target_name(self.face_target)
            )?;
        }
        if self.smoke_animation != 0 {
            write_named(
                f,
                "smoke:",
                self.smoke_animation,
                &self.smoke_animation_name,
            )?;
        }
        if self.smoke_puff_delay != 0 {
            writeln!(f, "{}{}", label("smoke delay:"), self.smoke_puff_delay)?;
        }
        for (i, anim) in self.trailing_animation.iter().enumerate() {
            if !anim.is_empty() {
                writeln!(f, "{}{:?}", label(&format!("trail {}:", i + 1)), anim)?;
            }
        }
        if self.puff_flags != 0 {
            write_bitfield(
                f,
                &label("puff:"),
                self.puff_flags,
                &puff_flags(self.puff_flags),
            )?;
        }
        Ok(())
    }
}

/// The numeric codes an area projectile carries that the manager resolves
/// against IDS tables.
#[derive(Clone, Copy)]
pub struct AreaCodes {
    pub fragment_animation: u16,
    pub secondary_projectile: u16,
    pub explosion_projectile: u16,
    pub explosion_effect: u8,
}

impl AreaCodes {
    /// The `projectl.ids` number the explosion fires at each target: a plain
    /// table number, unlike an ability's `missile.ids` one. `0` is unset.
    pub fn explosion_projectile_ids(&self) -> Option<i64> {
        (self.explosion_projectile > 0).then(|| i64::from(self.explosion_projectile))
    }
}

/// The 256-byte area struct: the explosion's extent, sound, secondary/explosion
/// projectiles, and ring/spread animations.
struct ProArea {
    area_flags: u16,
    ray_count: u16,
    trap_size: u16,
    explosion_size: u16,
    explosion_sound: String,
    explosion_frequency: u16,
    fragment_animation: u16,
    secondary_projectile: u16,
    repetitions: u8,
    explosion_effect: u8,
    explosion_color: u8,
    explosion_projectile: u16,
    explosion_animation: String,
    cone_width: u16,
    rotate_rays: u16,
    spread_animation: String,
    ring_animation: String,
    area_sound: String,
    area_ext_flags: u32,
    dice_count: u16,
    dice_size: u16,
    anim_granularity: u16,
    anim_granularity_divider: u16,
    /// Names resolved by the manager (animate.ids, projectl.ids, fireball.ids).
    fragment_animation_name: Option<String>,
    secondary_projectile_name: Option<String>,
    explosion_projectile_name: Option<String>,
    explosion_effect_name: Option<String>,
}

impl ProArea {
    fn parse(reader: &Reader) -> Result<ProArea, Truncated> {
        let b = AREA_BASE;
        Ok(ProArea {
            area_flags: reader.u16_at(b, "area flags")?,
            ray_count: reader.u16_at(b + 0x02, "ray count")?,
            trap_size: reader.u16_at(b + 0x04, "trap size")?,
            explosion_size: reader.u16_at(b + 0x06, "explosion size")?,
            explosion_sound: reader.resref_at(b + 0x08, "explosion sound")?,
            explosion_frequency: reader.u16_at(b + 0x10, "explosion frequency")?,
            fragment_animation: reader.u16_at(b + 0x12, "fragment animation")?,
            secondary_projectile: reader.u16_at(b + 0x14, "secondary projectile")?,
            repetitions: reader.u8_at(b + 0x16, "repetitions")?,
            explosion_effect: reader.u8_at(b + 0x17, "explosion effect")?,
            explosion_color: reader.u8_at(b + 0x18, "explosion color")?,
            explosion_projectile: reader.u16_at(b + 0x1a, "explosion projectile")?,
            explosion_animation: reader.resref_at(b + 0x1c, "explosion animation")?,
            cone_width: reader.u16_at(b + 0x24, "cone width")?,
            rotate_rays: reader.u16_at(b + 0x26, "rotate rays")?,
            spread_animation: reader.resref_at(b + 0x28, "spread animation")?,
            ring_animation: reader.resref_at(b + 0x30, "ring animation")?,
            area_sound: reader.resref_at(b + 0x38, "area sound")?,
            area_ext_flags: reader.u32_at(b + 0x40, "area extended flags")?,
            dice_count: reader.u16_at(b + 0x44, "dice count")?,
            dice_size: reader.u16_at(b + 0x46, "dice size")?,
            anim_granularity: reader.u16_at(b + 0x48, "animation granularity")?,
            anim_granularity_divider: reader.u16_at(b + 0x4a, "animation granularity divider")?,
            fragment_animation_name: None,
            secondary_projectile_name: None,
            explosion_projectile_name: None,
            explosion_effect_name: None,
        })
    }

    fn codes(&self) -> AreaCodes {
        AreaCodes {
            fragment_animation: self.fragment_animation,
            secondary_projectile: self.secondary_projectile,
            explosion_projectile: self.explosion_projectile,
            explosion_effect: self.explosion_effect,
        }
    }

    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.area_flags != 0 {
            write_bitfield(
                f,
                &label("flags:"),
                self.area_flags as u32,
                &area_flags(self.area_flags),
            )?;
        }
        writeln!(f, "{}{}", label("trap size:"), self.trap_size)?;
        writeln!(f, "{}{}", label("expl. size:"), self.explosion_size)?;
        if self.repetitions != 0 {
            writeln!(f, "{}{}", label("reps:"), self.repetitions)?;
        }
        if self.explosion_frequency != 0 {
            writeln!(f, "{}{}", label("frequency:"), self.explosion_frequency)?;
        }
        if self.ray_count != 0 {
            writeln!(f, "{}{}", label("rays:"), self.ray_count)?;
        }
        if self.cone_width != 0 {
            writeln!(f, "{}{}", label("cone width:"), self.cone_width)?;
        }
        if !self.explosion_sound.is_empty() {
            writeln!(f, "{}{:?}", label("expl sound:"), self.explosion_sound)?;
        }
        // 255 is the "none" sentinel; 0 is a real effect (FIRE in fireball.ids).
        if self.explosion_effect != 255 {
            write_named(
                f,
                "expl fx:",
                self.explosion_effect as u16,
                &self.explosion_effect_name,
            )?;
        }
        if self.explosion_projectile != 0 {
            write_named(
                f,
                "expl proj:",
                self.explosion_projectile,
                &self.explosion_projectile_name,
            )?;
        }
        if self.secondary_projectile != 0 {
            write_named(
                f,
                "2nd proj:",
                self.secondary_projectile,
                &self.secondary_projectile_name,
            )?;
        }
        if self.fragment_animation != 0 {
            write_named(
                f,
                "fragments:",
                self.fragment_animation,
                &self.fragment_animation_name,
            )?;
        }
        if !self.explosion_animation.is_empty() {
            writeln!(f, "{}{:?}", label("expl anim:"), self.explosion_animation)?;
        }
        if !self.spread_animation.is_empty() {
            writeln!(f, "{}{:?}", label("spread:"), self.spread_animation)?;
        }
        if !self.ring_animation.is_empty() {
            writeln!(f, "{}{:?}", label("ring:"), self.ring_animation)?;
        }
        if !self.area_sound.is_empty() {
            writeln!(f, "{}{:?}", label("area sound:"), self.area_sound)?;
        }
        if self.area_ext_flags != 0 {
            write_bitfield(
                f,
                &label("ext flags:"),
                self.area_ext_flags,
                &area_ext_flags(self.area_ext_flags),
            )?;
        }
        Ok(())
    }
}

/// Anything that can go wrong while parsing a PRO file.
#[derive(Debug)]
pub enum ProError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for ProError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProError::Truncated(e) => write!(f, "{e}"),
            ProError::BadSignature(found) => {
                write!(f, "not a PRO file: signature was {}", describe_tag(found))
            }
            ProError::BadVersion(found) => {
                write!(f, "unsupported PRO version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for ProError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for ProError {
    fn from(e: Truncated) -> Self {
        ProError::Truncated(e)
    }
}

/// A 4-space indented, 13-wide label for the travel/area sub-sections, so values
/// line up in a single column under each section heading.
fn label(text: &str) -> String {
    format!("    {text:<13}")
}

/// Write a `<label><value> (<name>)` line (aligned via [`label`]), omitting the
/// parenthetical when the lookup left the name unresolved.
fn write_named(
    f: &mut fmt::Formatter<'_>,
    text: &str,
    value: u16,
    name: &Option<String>,
) -> fmt::Result {
    let label = label(text);
    match name {
        Some(name) => writeln!(f, "{label}{value} ({name})"),
        None => writeln!(f, "{label}{value}"),
    }
}

/// Write a referenced-spell line, suppressed entirely when the resref is empty.
fn write_spell(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    resref: &str,
    name: &Option<String>,
) -> fmt::Result {
    if resref.is_empty() {
        return Ok(());
    }
    match name {
        Some(name) => writeln!(f, "{label} {resref} ({name})"),
        None => writeln!(f, "{label} {resref}"),
    }
}

/// Name the projectile type (header 0x08).
fn projectile_type_name(value: u16) -> &'static str {
    match value {
        0 => "no projectile",
        1 => "no BAM",
        2 => "single target",
        3 => "area of effect",
        _ => "?",
    }
}

/// Name the face-target / mirroring mode (travel 0x33).
fn face_target_name(value: u8) -> &'static str {
    match value {
        0 => "do not mirror",
        1 => "do not face target",
        5 => "mirrored east (reduced)",
        9 => "mirrored east (full)",
        16 => "not mirrored",
        _ => "?",
    }
}

/// Sparking / behaviour flags (header 0x0c).
fn behaviour_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "show sparkle"),
            (1, "use z coordinate"),
            (2, "loop fire sound"),
            (3, "loop impact sound"),
            (4, "ignore center"),
            (5, "draw below objects"),
            (6, "allow saving"),
            (7, "loop spread anim"),
        ],
    )
}

/// Extended flags (header 0x2c).
fn ext_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "bounce from walls"),
            (1, "pass target"),
            (2, "draw center VVC once"),
            (3, "hit immediately"),
            (4, "face target"),
            (5, "curved path"),
            (6, "start random frame"),
            (7, "pillar"),
            (8, "semi-transparent trail VEF"),
            (9, "tinted trail VEF"),
            (10, "multiple projectiles"),
            (11, "default spell on missed"),
            (12, "falling path"),
            (13, "comet"),
            (14, "lined up AoE"),
            (15, "rectangular AoE"),
            (16, "draw behind target"),
            (17, "casting glow"),
            (18, "travel door"),
            (19, "stop/fade after hit"),
            (20, "display message"),
            (21, "random path"),
            (22, "start random sequence"),
            (23, "colour pulse on hit"),
            (24, "touch projectile"),
            (25, "negate first target"),
            (26, "negate second target"),
            (27, "use either IDS"),
            (28, "delayed payload"),
            (29, "limited path count"),
            (30, "IWD style check"),
            (31, "caster affected"),
        ],
    )
}

/// BAM projectile flags (travel 0x100).
fn bam_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "BAM colouring"),
            (1, "smoke"),
            (2, "colored smoke"),
            (3, "area lighting"),
            (4, "area height"),
            (5, "shadow"),
            (6, "light spot"),
            (7, "brighten"),
            (8, "low brighten"),
            (9, "high brighten"),
        ],
    )
}

/// Trail puff flags (travel 0x154).
fn puff_flags(value: u32) -> Vec<&'static str> {
    set_flags(value, &[(0, "puff at target"), (1, "puff at source")])
}

/// Area projectile flags (area 0x200).
fn area_flags(value: u16) -> Vec<&'static str> {
    set_flags(
        value as u32,
        &[
            (0, "trap visible"),
            (1, "triggered by objects"),
            (2, "triggered by condition"),
            (3, "delayed trigger"),
            (4, "secondary projectile"),
            (5, "fragment graphics"),
            (6, "affect only enemies"),
            (7, "affect only allies"),
            (8, "mage-level duration"),
            (9, "cleric-level duration"),
            (10, "draw animation"),
            (11, "cone-shaped"),
            (12, "ignore LOS"),
            (13, "delayed explosion"),
            (14, "skip first condition"),
            (15, "single target"),
        ],
    )
}

/// Area extended flags (area 0x240).
fn area_ext_flags(value: u32) -> Vec<&'static str> {
    set_flags(
        value,
        &[
            (0, "paletted ring"),
            (1, "random speed"),
            (2, "start scattered"),
            (3, "paletted center"),
            (4, "repeat scattering"),
            (5, "paletted animation"),
            (9, "oriented fireball puffs"),
            (10, "use HD lookup"),
            (13, "blend area/ring anim"),
            (14, "glow area/ring anim"),
            (15, "hit point limit"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PRO of the given projectile type: a valid 256-byte header with the
    /// type word set, followed by enough zero bytes for the structs that type
    /// implies (none/256/512/768).
    fn build_pro(projectile_type: u16, total: usize) -> Vec<u8> {
        let mut data = vec![0u8; total];
        data[0x00..0x04].copy_from_slice(PRO_SIGNATURE);
        data[0x04..0x08].copy_from_slice(PRO_VERSION);
        data[0x08..0x0a].copy_from_slice(&projectile_type.to_le_bytes());
        data
    }

    #[test]
    fn no_projectile_has_no_substructs() {
        let pro = Pro::parse(&build_pro(0, 256)).expect("parse");
        assert!(pro.travel_smoke().is_none());
        assert!(pro.area_codes().is_none());
    }

    #[test]
    fn single_target_has_travel_only() {
        let pro = Pro::parse(&build_pro(2, 512)).expect("parse");
        assert!(pro.travel_smoke().is_some());
        assert!(pro.area_codes().is_none());
    }

    #[test]
    fn area_effect_has_both_substructs() {
        let pro = Pro::parse(&build_pro(3, 768)).expect("parse");
        assert!(pro.travel_smoke().is_some());
        assert!(pro.area_codes().is_some());
    }

    #[test]
    fn resref_uses_grow_with_the_substructs_and_mark_fallbacks_soft() {
        // A type-0 projectile has neither substruct, so only header refs appear.
        let mut data = build_pro(0, 256);
        data[0x10..0x14].copy_from_slice(b"FIRE"); // fire sound
        data[0x20..0x24].copy_from_slice(b"SRCA"); // source animation
        data[0x44..0x48].copy_from_slice(b"SPWI"); // default spell
        let pro = Pro::parse(&data).expect("parse");
        let uses = pro.resref_uses();
        let got: Vec<(u16, &str, &str, bool)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str(), u.soft))
            .collect();
        assert_eq!(
            got,
            vec![
                (WAV_TYPE, "FIRE", "fire_sound", false),
                // VEF → VVC → BAM fallback chain: tagged BAM, but only a guess.
                (BAM_TYPE, "SRCA", "source_animation", true),
                (SPL_TYPE, "SPWI", "default_spell", false),
            ]
        );

        // A type-2 projectile adds the travel struct, whose refs are unambiguous.
        let mut data = build_pro(2, 512);
        data[TRAVEL_BASE + 0x04..TRAVEL_BASE + 0x08].copy_from_slice(b"TRVL");
        data[TRAVEL_BASE + 0x1c..TRAVEL_BASE + 0x20].copy_from_slice(b"PLTT");
        let pro = Pro::parse(&data).expect("parse");
        let uses = pro.resref_uses();
        let got: Vec<(u16, &str, &str, bool)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str(), u.soft))
            .collect();
        assert_eq!(
            got,
            vec![
                (BAM_TYPE, "TRVL", "travel.projectile_animation", false),
                (BMP_TYPE, "PLTT", "travel.palette", false),
            ]
        );
    }

    #[test]
    fn strref_uses_reports_the_message() {
        let mut data = build_pro(0, 256);
        data[0x30..0x34].copy_from_slice(&42u32.to_le_bytes());
        let uses = Pro::parse(&data).expect("parse").strref_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!((uses[0].value, uses[0].path.as_str()), (42, "message"));
    }

    #[test]
    fn rejects_bad_signature() {
        let mut data = build_pro(0, 256);
        data[0] = b'X';
        assert!(matches!(Pro::parse(&data), Err(ProError::BadSignature(_))));
    }

    #[test]
    fn rejects_truncated_substruct() {
        // Claims to be area-effect but only the base header is present.
        let data = build_pro(3, 256);
        assert!(matches!(Pro::parse(&data), Err(ProError::Truncated(_))));
    }
}
