//! A synthetic game tree, for tests that need a whole install.
//!
//! The real `game/` symlink is a licensed install plus whatever mods are
//! layered on it, so its resource counts shift underfoot. Tests that assert
//! *exact* answers build this instead: a few resources in one BIF, shadowed by
//! an `override` directory, laid out as the engine expects.
//!
//! Generated into a temp directory rather than committed as blobs, so a wrong
//! offset fails a test instead of hiding in a file nobody can diff. It
//! exercises the search plumbing, not the parsers — those have their own
//! fixtures.

use std::fs;
use std::path::PathBuf;

use crate::formats::are::ARE_TYPE;
use crate::formats::bcs::BCS_TYPE;
use crate::formats::cre::CRE_TYPE;
use crate::formats::dlg::DLG_TYPE;
use crate::formats::eff::{EFF_TYPE, FEATURE_SIZE};
use crate::formats::itm::ITM_TYPE;
use crate::formats::pro::PRO_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::formats::twoda::TWODA_TYPE;

/// A spell named by the number the fixture script passes to `ForceSpell`, via
/// the `SPELL.IDS` filename convention (`2001` -> `SPWI001`).
pub const SPELL_BY_NUMBER: &str = "SPWI001";

/// The strref the fixture script passes to `DisplayString`.
pub const SCRIPT_STRREF: u32 = 12345;

/// The `projectl.ids` number the fixture gives [`PRO_TARGET`], which is the
/// only way a spell or item can name it.
pub const PROJECTILE_NUMBER: u16 = 7;

// What every reference in the fixture points at. Distinct per type, so an
// assertion can never be satisfied by the wrong kind.
pub const ITM_TARGET: &str = "SW1H01";
pub const SPL_TARGET: &str = "MAGICMIS";
pub const CRE_TARGET: &str = "SUMMON1";
pub const DLG_TARGET: &str = "CHATTER";
pub const ARE_TARGET: &str = "AR0001";
pub const PRO_TARGET: &str = "PROJ01";
pub const EFF_TARGET: &str = "EFFECT1";
pub const BCS_TARGET: &str = "SCRIPT1";
pub const TWODA_TARGET: &str = "TABLE1";
pub const BAM_TARGET: &str = "ANIM1";
pub const OVERLAY_TARGET: &str = "OVERLAY1";
pub const WAV_TARGET: &str = "SOUND1";
pub const WED_TARGET: &str = "TILEMAP1";
pub const BMP_TARGET: &str = "PORTRAIT";
pub const MVE_TARGET: &str = "MOVIE1";
pub const STO_TARGET: &str = "SHOP1";
pub const MOS_TARGET: &str = "LOADSCR1";
pub const WMP_TARGET: &str = "WORLD1";

/// One casting effect per resource-bearing opcode family, so a single spell
/// witnesses most of the opcode table.
const SPELL_EFFECTS: [(u16, &str); 11] = [
    (122, ITM_TARGET),     // creates an item
    (318, ITM_TARGET),     // "spell or item" — soft, under both readings
    (146, SPL_TARGET),     // casts a spell
    (67, CRE_TARGET),      // summons a creature
    (177, EFF_TARGET),     // applies an EFF file
    (214, TWODA_TARGET),   // custom spell table
    (152, MVE_TARGET),     // plays a movie
    (107, BMP_TARGET),     // changes the portrait
    (215, OVERLAY_TARGET), // VVC-or-BAM animation — soft, under both
    (82, BCS_TARGET),      // sets an AI script
    (186, ARE_TARGET),     // moves to an area
];

/// A fixture game tree that deletes itself when dropped.
pub struct FixtureGame {
    root: PathBuf,
}

impl FixtureGame {
    /// The `chitin.key` to hand to `ResourceManager::open`.
    pub fn key_path(&self) -> PathBuf {
        self.root.join("chitin.key")
    }
}

impl Drop for FixtureGame {
    fn drop(&mut self) {
        // Best-effort: a leaked temp dir is not worth failing a test over.
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One resource to place in the fixture.
struct Entry {
    name: &'static str,
    res_type: u16,
    data: Vec<u8>,
}

/// Build the fixture game in a fresh temporary directory.
///
/// Between them these name every type in `REFERENCEABLE_KINDS`, and cover every
/// format in `SCANNED_TYPES` as a referrer. `search::tests` asserts that, so a
/// new kind fails the suite until it gains a witness here.
///
/// | resource       | where          | role                                   |
/// |----------------|----------------|----------------------------------------|
/// | `SW1H01.itm`   | bif            | names its replacement item and an icon |
/// | `DECOY.itm`    | bif            | names nothing — the negative control   |
/// | `SHADOWED.cre` | bif + override | 1 item archived, 2 in the override     |
/// | `ARCHIVED.cre` | bif only       | portraits, scripts, dialog, spell, item |
/// | `MAGIC.spl`    | bif            | one effect per opcode, plus a projectile |
/// | `MAP.are`      | bif            | tilemap, neighbour, actors, doors, …   |
/// | `BOLT.pro`     | bif            | sounds, animations, palette, spell     |
/// | `TALK.dlg`     | bif            | a DLG handoff, plus a trigger and action |
/// | `SINGLE.eff`   | bif            | a standalone effect naming an item     |
/// | `LOOT.2da`     | bif            | the item as a row label and as a cell  |
/// | `LOOSE.sto`    | override only  | rumours, a sale item, a cure           |
pub fn build() -> FixtureGame {
    let root = temp_dir();
    let data_dir = root.join("data");
    let override_dir = root.join("override");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&override_dir).expect("create override dir");

    let archived = vec![
        Entry {
            name: "SW1H01",
            res_type: ITM_TYPE,
            data: itm(),
        },
        Entry {
            name: "DECOY",
            res_type: ITM_TYPE,
            data: bare_itm(),
        },
        Entry {
            name: "SHADOWED",
            res_type: CRE_TYPE,
            data: cre(&[ITM_TARGET]),
        },
        Entry {
            name: "ARCHIVED",
            res_type: CRE_TYPE,
            data: cre(&[ITM_TARGET]),
        },
        Entry {
            name: "MAGIC",
            res_type: SPL_TYPE,
            data: spl(),
        },
        Entry {
            name: "MAP",
            res_type: ARE_TYPE,
            data: are(),
        },
        Entry {
            name: "BOLT",
            res_type: PRO_TYPE,
            data: pro(),
        },
        Entry {
            name: "TALK",
            res_type: DLG_TYPE,
            data: dlg(),
        },
        Entry {
            name: "SINGLE",
            res_type: EFF_TYPE,
            data: eff(),
        },
        Entry {
            name: "LOOT",
            res_type: TWODA_TYPE,
            data: twoda(),
        },
        Entry {
            name: "AISCRIPT",
            res_type: BCS_TYPE,
            data: bcs(),
        },
    ];

    fs::write(data_dir.join("test.bif"), bif(&archived)).expect("write bif");
    fs::write(root.join("chitin.key"), key("data/test.bif", &archived)).expect("write key");

    // The override layer: one file shadowing an archived creature with a
    // *different* body, and one resource that exists nowhere else.
    fs::write(
        override_dir.join("SHADOWED.CRE"),
        cre(&[ITM_TARGET, ITM_TARGET]),
    )
    .expect("write override cre");
    fs::write(override_dir.join("LOOSE.STO"), sto()).expect("write override sto");

    // The script vocabulary: the calls the fixture's DLG and BCS scripts use,
    // and the one object identifier the DLG names (`Player1`), which the
    // compiler needs to read that script's `ActionOverride`.
    fs::write(override_dir.join("ACTION.IDS"), action_ids()).expect("write action.ids");
    fs::write(override_dir.join("TRIGGER.IDS"), trigger_ids()).expect("write trigger.ids");
    fs::write(override_dir.join("OBJECT.IDS"), b"21 Player1\n".as_slice())
        .expect("write object.ids");

    // The only thing naming the .pro a spell fires. Two numbers for the one
    // file, as the real table has.
    fs::write(
        override_dir.join("PROJECTL.IDS"),
        format!(
            "{PROJECTILE_NUMBER} {PRO_TARGET}\n{} {PRO_TARGET}\n",
            PROJECTILE_NUMBER + 40
        )
        .into_bytes(),
    )
    .expect("write projectl.ids");

    FixtureGame { root }
}

/// Unique per process and call, so concurrent tests never share one.
fn temp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("iediff-fixture-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

// ---- resource bodies -------------------------------------------------------
//
// The smallest byte sequence each parser accepts, carrying only the fields a
// search has to find. Offsets mirror the parsers'.

/// An ITM naming nothing: the negative control.
fn bare_itm() -> Vec<u8> {
    let mut d = vec![0u8; 0x9a];
    d[0x00..0x04].copy_from_slice(b"ITM ");
    d[0x04..0x08].copy_from_slice(b"V1  ");
    d
}

/// An ITM naming its replacement item and its inventory icon.
fn itm() -> Vec<u8> {
    let mut d = bare_itm();
    put_resref(&mut d, 0x10, ITM_TARGET); // replacement_item
    put_resref(&mut d, 0x3a, BAM_TARGET); // inventory_icon
    d
}

/// A CRE carrying `items`, plus a portrait, a script, a dialogue and a spell.
fn cre(items: &[&str]) -> Vec<u8> {
    const ITEM_SIZE: usize = 20;
    const KNOWN_SPELL_SIZE: usize = 12;
    const SLOT_COUNT: usize = 38;
    let mut d = vec![0u8; 0x2d4];
    d[0x00..0x04].copy_from_slice(b"CRE ");
    d[0x04..0x08].copy_from_slice(b"V1.0");
    put_resref(&mut d, 0x34, BMP_TARGET); // small_portrait
    put_resref(&mut d, 0x248, BCS_TARGET); // script_override
    put_resref(&mut d, 0x2cc, DLG_TARGET); // dialog

    let slots_off = d.len();
    d.resize(slots_off + (SLOT_COUNT + 2) * 2, 0);
    put_u32(&mut d, 0x2b8, slots_off as u32);

    let known_off = d.len();
    d.resize(known_off + KNOWN_SPELL_SIZE, 0);
    put_resref(&mut d, known_off, SPL_TARGET);
    put_u32(&mut d, 0x2a0, known_off as u32);
    put_u32(&mut d, 0x2a4, 1);

    let items_off = d.len();
    d.resize(items_off + items.len() * ITEM_SIZE, 0);
    for (i, name) in items.iter().enumerate() {
        put_resref(&mut d, items_off + i * ITEM_SIZE, name);
    }
    put_u32(&mut d, 0x2bc, items_off as u32);
    put_u32(&mut d, 0x2c0, items.len() as u32);
    d
}

/// An SPL with a completion sound, the casting-effect pool ([`SPELL_EFFECTS`]),
/// and one ability firing the projectile by number.
fn spl() -> Vec<u8> {
    const EXT_HEADER_SIZE: usize = 40;
    let mut d = vec![0u8; 0x9a];
    d[0x00..0x04].copy_from_slice(b"SPL ");
    d[0x04..0x08].copy_from_slice(b"V1  ");
    put_resref(&mut d, 0x10, WAV_TARGET); // completion_sound

    let pool_off = d.len();
    d.resize(pool_off + SPELL_EFFECTS.len() * FEATURE_SIZE, 0);
    for (i, (opcode, resource)) in SPELL_EFFECTS.iter().enumerate() {
        let base = pool_off + i * FEATURE_SIZE;
        put_u16(&mut d, base, *opcode);
        put_resref(&mut d, base + 0x14, resource);
    }

    // One more effect, protecting from a projectile: opcode #83 names it by
    // table number in Parameter #2, with no resref anywhere.
    let guard_off = d.len();
    d.resize(guard_off + FEATURE_SIZE, 0);
    put_u16(&mut d, guard_off, 83);
    put_u32(&mut d, guard_off + 0x08, u32::from(PROJECTILE_NUMBER));

    // One ability, its projectile stored one past the projectl.ids number.
    let ext_off = d.len();
    d.resize(ext_off + EXT_HEADER_SIZE, 0);
    put_u16(&mut d, ext_off + 0x26, PROJECTILE_NUMBER + 1);

    put_u32(&mut d, 0x64, ext_off as u32); // ext header offset
    put_u16(&mut d, 0x68, 1); // ext header count
    put_u32(&mut d, 0x6a, pool_off as u32); // feature pool
    put_u16(&mut d, 0x6e, 0); // casting index
    put_u16(&mut d, 0x70, SPELL_EFFECTS.len() as u16 + 1);
    d
}

/// A store: two rumour dialogues, one item for sale, one cure.
fn sto() -> Vec<u8> {
    const SALE_SIZE: usize = 28;
    const CURE_SIZE: usize = 12;
    let mut d = vec![0u8; 0x9c];
    d[0x00..0x04].copy_from_slice(b"STOR");
    d[0x04..0x08].copy_from_slice(b"V1.0");
    put_resref(&mut d, 0x44, DLG_TARGET); // rumours_tavern

    let sale_off = d.len();
    d.resize(sale_off + SALE_SIZE, 0);
    put_resref(&mut d, sale_off, ITM_TARGET);
    put_u32(&mut d, 0x34, sale_off as u32);
    put_u32(&mut d, 0x38, 1);

    let cure_off = d.len();
    d.resize(cure_off + CURE_SIZE, 0);
    put_resref(&mut d, cure_off, SPL_TARGET);
    put_u32(&mut d, 0x70, cure_off as u32);
    put_u32(&mut d, 0x74, 1);
    d
}

/// An area naming a tilemap, neighbour, actor, pooled item, door key, ambient
/// sound, animation and projectile trap.
fn are() -> Vec<u8> {
    const ACTOR_SIZE: usize = 0x110;
    const ITEM_SIZE: usize = 0x14;
    const DOOR_SIZE: usize = 0xc8;
    const AMBIENT_SIZE: usize = 0xd4;
    const ANIM_SIZE: usize = 0x4c;
    const PTRAP_SIZE: usize = 0x1c;

    let mut d = vec![0u8; 0x11c];
    d[0x00..0x04].copy_from_slice(b"AREA");
    d[0x04..0x08].copy_from_slice(b"V1.0");
    put_resref(&mut d, 0x08, WED_TARGET); // wed
    put_resref(&mut d, 0x18, ARE_TARGET); // north neighbour
    put_resref(&mut d, 0x94, BCS_TARGET); // area script

    let actor_off = d.len();
    d.resize(actor_off + ACTOR_SIZE, 0);
    put_resref(&mut d, actor_off + 0x48, DLG_TARGET); // actor dialog
    put_resref(&mut d, actor_off + 0x80, CRE_TARGET); // actor CRE
    put_u32(&mut d, 0x54, actor_off as u32);
    put_u16(&mut d, 0x58, 1);

    let item_off = d.len();
    d.resize(item_off + ITEM_SIZE, 0);
    put_resref(&mut d, item_off, ITM_TARGET);
    put_u32(&mut d, 0x78, item_off as u32);
    put_u16(&mut d, 0x76, 1);

    let door_off = d.len();
    d.resize(door_off + DOOR_SIZE, 0);
    put_resref(&mut d, door_off + 0x58, WAV_TARGET); // open sound
    put_resref(&mut d, door_off + 0x78, ITM_TARGET); // key item
    put_u32(&mut d, 0xa8, door_off as u32);
    put_u32(&mut d, 0xa4, 1);

    let ambient_off = d.len();
    d.resize(ambient_off + AMBIENT_SIZE, 0);
    put_resref(&mut d, ambient_off + 0x30, WAV_TARGET); // sound[0]
    put_u32(&mut d, 0x84, ambient_off as u32);
    put_u16(&mut d, 0x82, 1);

    let anim_off = d.len();
    d.resize(anim_off + ANIM_SIZE, 0);
    put_resref(&mut d, anim_off + 0x28, BAM_TARGET); // animation resref
    put_u32(&mut d, 0xb0, anim_off as u32);
    put_u32(&mut d, 0xac, 1);

    let trap_off = d.len();
    d.resize(trap_off + PTRAP_SIZE, 0);
    put_resref(&mut d, trap_off, PRO_TARGET);
    put_u32(&mut d, 0xcc, trap_off as u32);
    put_u32(&mut d, 0xd0, 1);
    d
}

/// An area-effect projectile: sounds, animations, a palette and a spell.
fn pro() -> Vec<u8> {
    let mut d = vec![0u8; 768];
    d[0x00..0x04].copy_from_slice(b"PRO ");
    d[0x04..0x08].copy_from_slice(b"V1.0");
    put_u16(&mut d, 0x08, 3); // area of effect: both substructs present
    put_resref(&mut d, 0x10, WAV_TARGET); // fire sound
    put_resref(&mut d, 0x20, OVERLAY_TARGET); // source animation (VEF/VVC/BAM)
    put_resref(&mut d, 0x44, SPL_TARGET); // default spell
    put_resref(&mut d, 0x104, BAM_TARGET); // travel.projectile_animation
    put_resref(&mut d, 0x11c, BMP_TARGET); // travel.palette
    d
}

/// A dialogue naming another dialogue in its typed `next_dlg`, and the item in
/// a trigger and an action — the two ways a DLG references a resource.
fn dlg() -> Vec<u8> {
    const STATE_SIZE: usize = 0x10;
    const TRANSITION_SIZE: usize = 0x20;
    const STRING_ENTRY_SIZE: usize = 0x08;

    let trigger = format!("PartyHasItem(\"{ITM_TARGET}\")");
    let action = format!("ActionOverride(Player1,GiveItemCreate(\"{ITM_TARGET}\",Player1,0,0,0))");

    let state_off = 0x34usize;
    let trans_off = state_off + STATE_SIZE;
    let strig_off = trans_off + TRANSITION_SIZE;
    let action_off = strig_off + STRING_ENTRY_SIZE;
    let text_off = action_off + STRING_ENTRY_SIZE;

    let mut d = vec![0u8; text_off];
    d[0x00..0x04].copy_from_slice(b"DLG ");
    d[0x04..0x08].copy_from_slice(b"V1.0");
    put_u32(&mut d, 0x08, 1); // state count
    put_u32(&mut d, 0x0c, state_off as u32);
    put_u32(&mut d, 0x10, 1); // transition count
    put_u32(&mut d, 0x14, trans_off as u32);
    put_u32(&mut d, 0x18, strig_off as u32); // state triggers
    put_u32(&mut d, 0x1c, 1);
    put_u32(&mut d, 0x20, text_off as u32); // transition triggers: none
    put_u32(&mut d, 0x24, 0);
    put_u32(&mut d, 0x28, action_off as u32); // actions
    put_u32(&mut d, 0x2c, 1);

    put_u32(&mut d, state_off + 0x08, 1); // one transition on the state
    put_resref(&mut d, trans_off + 0x14, DLG_TARGET);

    // Each script table entry is an (offset, length) pair into the text at the
    // end of the file.
    put_u32(&mut d, strig_off, text_off as u32);
    put_u32(&mut d, strig_off + 4, trigger.len() as u32);
    put_u32(&mut d, action_off, (text_off + trigger.len()) as u32);
    put_u32(&mut d, action_off + 4, action.len() as u32);
    d.extend_from_slice(trigger.as_bytes());
    d.extend_from_slice(action.as_bytes());
    d
}

/// A 2DA naming the item as a row label and as a cell, plus a near-miss row to
/// guard against prefix matching.
fn twoda() -> Vec<u8> {
    format!("2DA V1.0\n*\nCOL1 COL2\n{ITM_TARGET} 1 2\nROW2 {ITM_TARGET} 0\nROW3 {ITM_TARGET}A 0\n")
        .into_bytes()
}

/// The `ACTION.IDS` the fixture scripts draw on: the item verb the DLG's
/// `ActionOverride` wraps, plus the calls the BCS witness uses to name a
/// creature, a script, a spell by number, a display string, and the three types
/// only a script ever names — a store, a loading screen, a worldmap.
fn action_ids() -> Vec<u8> {
    "200 GiveItemCreate(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)\n\
     201 CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)\n\
     202 ChangeAIScript(S:ScriptFile*,I:Level*Scrlev)\n\
     203 ForceSpell(O:Target*,I:Spell*Spell)\n\
     204 DisplayString(O:Object*,I:StrRef*)\n\
     205 StartStore(S:Store*,O:Target*)\n\
     206 LeaveAreaLUA(S:Area*,S:Parchment*,P:Point*,I:Face*DIR)\n\
     207 SetWorldmap(S:Worldmap*)\n"
        .as_bytes()
        .to_vec()
}

/// The `TRIGGER.IDS`: the DLG's state trigger and the BCS witness's item check.
fn trigger_ids() -> Vec<u8> {
    "100 PartyHasItem(S:Item*)\n\
     101 HasItem(S:ResRef*,O:Object*)\n"
        .as_bytes()
        .to_vec()
}

/// A compiled AI script whose calls name resources through every channel the
/// reverse search reads: an item by a trigger's string, a creature and a script
/// by an action's string, a spell by number, and a dialog string by strref.
/// `LeaveAreaLUA` fills both its string slots, so a parchment misread as an area
/// (or the reverse) fails rather than passing on a lucky alignment.
fn bcs() -> Vec<u8> {
    // A default (all-zero, unnamed) object, both markers included.
    let obj = "OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB";
    format!(
        "SC\nCR\nCO\n\
         TR\n101 0 0 0 0 \"{ITM_TARGET}\" \"\" {obj}\nTR\n\
         CO\nRS\nRE\n\
         100AC\n201{obj}\n{obj}\n{obj}\n0 0 0 0 0\"{CRE_TARGET}\" \"\" AC\n\
         AC\n202{obj}\n{obj}\n{obj}\n0 0 0 0 0\"{BCS_TARGET}\" \"\" AC\n\
         AC\n203{obj}\n{obj}\n{obj}\n2001 0 0 0 0\"\" \"\" AC\n\
         AC\n204{obj}\n{obj}\n{obj}\n{SCRIPT_STRREF} 0 0 0 0\"\" \"\" AC\n\
         AC\n205{obj}\n{obj}\n{obj}\n0 0 0 0 0\"{STO_TARGET}\" \"\" AC\n\
         AC\n206{obj}\n{obj}\n{obj}\n0 0 0 0 0\"{ARE_TARGET}\" \"{MOS_TARGET}\" AC\n\
         AC\n207{obj}\n{obj}\n{obj}\n0 0 0 0 0\"{WMP_TARGET}\" \"\" AC\n\
         RE\nRS\nCR\nSC\n"
    )
    .into_bytes()
}

/// A standalone V2 effect that creates an item.
fn eff() -> Vec<u8> {
    let mut d = vec![0u8; 0x110];
    d[0x00..0x04].copy_from_slice(b"EFF ");
    d[0x04..0x08].copy_from_slice(b"V2.0");
    put_u32(&mut d, 0x10, 122); // body 0x08: opcode
    put_resref(&mut d, 0x30, ITM_TARGET); // body 0x28: resource
    d
}

// ---- index containers ------------------------------------------------------

/// A BIFF V1 archive; each entry's file index is its position, which is what
/// the KEY locators point at.
fn bif(entries: &[Entry]) -> Vec<u8> {
    const HEADER_LEN: usize = 0x14;
    const FILE_ENTRY_LEN: usize = 16;

    let table_off = HEADER_LEN;
    let mut data_off = table_off + entries.len() * FILE_ENTRY_LEN;

    let mut d = Vec::new();
    d.extend_from_slice(b"BIFF");
    d.extend_from_slice(b"V1  ");
    d.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes()); // tileset count
    d.extend_from_slice(&(table_off as u32).to_le_bytes());

    for (i, e) in entries.iter().enumerate() {
        d.extend_from_slice(&(i as u32).to_le_bytes()); // locator: file index
        d.extend_from_slice(&(data_off as u32).to_le_bytes());
        d.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        d.extend_from_slice(&e.res_type.to_le_bytes());
        d.extend_from_slice(&0u16.to_le_bytes()); // unused
        data_off += e.data.len();
    }
    for e in entries {
        d.extend_from_slice(&e.data);
    }
    d
}

/// A KEY listing one BIF and one entry per archived resource.
fn key(bif_name: &str, entries: &[Entry]) -> Vec<u8> {
    const BIF_ENTRY_LEN: usize = 12;
    let name = format!("{bif_name}\0");
    let bif_name_off = (0x18 + BIF_ENTRY_LEN) as u32;
    let resource_off = bif_name_off + name.len() as u32;

    let mut d = Vec::new();
    d.extend_from_slice(b"KEY ");
    d.extend_from_slice(b"V1  ");
    d.extend_from_slice(&1u32.to_le_bytes()); // bif count
    d.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    d.extend_from_slice(&0x18u32.to_le_bytes()); // bif entry offset
    d.extend_from_slice(&resource_off.to_le_bytes());

    // BIF entry: length, name offset, name length, location flags.
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&bif_name_off.to_le_bytes());
    d.extend_from_slice(&(name.len() as u16).to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes());
    d.extend_from_slice(name.as_bytes());

    for (i, e) in entries.iter().enumerate() {
        let mut field = [0u8; 8];
        let n = e.name.as_bytes();
        assert!(n.len() <= 8, "resref {:?} exceeds 8 bytes", e.name);
        field[..n.len()].copy_from_slice(n);
        d.extend_from_slice(&field);
        d.extend_from_slice(&e.res_type.to_le_bytes());
        // Locator: source index 0 (the only BIF) in bits 31-20, file index in
        // bits 13-0.
        d.extend_from_slice(&(i as u32).to_le_bytes());
    }
    d
}

// ---- little-endian writers -------------------------------------------------

fn put_u16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Write a resref into its 8-byte field. Panics on a longer name — silent
/// truncation would make the tests meaningless.
fn put_resref(d: &mut [u8], at: usize, name: &str) {
    let b = name.as_bytes();
    assert!(b.len() <= 8, "resref {name:?} exceeds 8 bytes");
    d[at..at + b.len()].copy_from_slice(b);
}

/// The fixture must parse, or every test built on it tests the wrong thing.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{ResourceManager, Source};

    #[test]
    fn fixture_game_opens_and_indexes() {
        let game = build();
        let manager = ResourceManager::open(&game.key_path()).expect("open fixture");
        assert_eq!(manager.key().resources().len(), 11);
        let stats = manager.override_stats();
        assert!(stats.present);
        // SHADOWED.cre, LOOSE.sto, and the four IDS tables.
        assert_eq!(stats.resources, 6);
        assert_eq!(stats.shadowing, 1, "SHADOWED.CRE shadows the archived copy");
    }

    #[test]
    fn fixture_game_is_removed_on_drop() {
        let root = {
            let game = build();
            let root = game.root.clone();
            assert!(root.join("chitin.key").is_file());
            root
        };
        assert!(!root.exists(), "temp tree should be cleaned up");
    }

    #[test]
    fn archived_and_override_copies_differ() {
        let game = build();
        let mut manager = ResourceManager::open(&game.key_path()).expect("open fixture");
        let uses = |m: &mut ResourceManager, source| {
            let hits = crate::search::find_resref(m, "SW1H01", ITM_TYPE, source, None);
            hits.into_iter()
                .find(|h| h.name.eq_ignore_ascii_case("SHADOWED"))
                .map(|h| h.fields)
                .unwrap_or_default()
        };
        assert_eq!(uses(&mut manager, Source::Auto), vec!["item[0]", "item[1]"]);
        assert_eq!(uses(&mut manager, Source::Biff), vec!["item[0]"]);
    }
}
