//! Effect opcode names for the BG(2):EE engine (opcodes 0–367).
//!
//! Effects (a.k.a. opcodes) are the verbs of the engine: every feature on a
//! spell, item, or creature is an opcode plus parameters. This table maps an
//! opcode number to its human name for readable output; it is shared by every
//! format that carries effects (EFF, and later ITM/SPL/CRE).
//!
//! Generated from `md/opcodes/bgee.md` (IESDP). BG(2):EE only, matching the
//! engine this tool targets.

use crate::formats::are::ARE_TYPE;
use crate::formats::bcs::BCS_TYPE;
use crate::formats::cre::CRE_TYPE;
use crate::formats::eff::EFF_TYPE;
use crate::formats::itm::ITM_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::formats::twoda::TWODA_TYPE;
use crate::formats::{BAM_TYPE, BMP_TYPE, MVE_TYPE, VVC_TYPE, WAV_TYPE};

/// An effect field that can hold a TLK strref, for the opcodes that display a
/// string. [`Param1`](StrrefSlot::Param1) is the first parameter (V1 0x04 /
/// V2 body 0x14); [`Special`](StrrefSlot::Special) is the Special field (V1 0x2c
/// / V2 body 0x40).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrrefSlot {
    Param1,
    Special,
}

/// One of an effect's resref fields. [`Resource`](ResourceSlot::Resource) is the
/// only one a V1 [`Feature`](super::eff::Feature) has (0x14); the V2 body adds
/// [`Resource2`](ResourceSlot::Resource2) (0x68) and
/// [`Resource3`](ResourceSlot::Resource3) (0x70).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceSlot {
    Resource,
    Resource2,
    Resource3,
}

/// One resref an effect names: which field holds it, what type it names, and
/// whether the engine will accept more than that type there.
#[derive(Clone, Copy, Debug)]
pub struct ResourceRef {
    pub slot: ResourceSlot,
    pub kind: u16,
    /// The engine resolves this field as more than one type, so the `kind` is
    /// the likely reading rather than the guaranteed one.
    pub soft: bool,
}

const fn hard(slot: ResourceSlot, kind: u16) -> ResourceRef {
    ResourceRef {
        slot,
        kind,
        soft: false,
    }
}

const fn soft(slot: ResourceSlot, kind: u16) -> ResourceRef {
    ResourceRef {
        slot,
        kind,
        soft: true,
    }
}

/// The item named by the effect's one resref field.
static ITEM: [ResourceRef; 1] = [hard(ResourceSlot::Resource, ITM_TYPE)];

/// Opcode #112: the item to remove, plus (when `Sound` is 2) a WAV to play.
static ITEM_AND_SOUND: [ResourceRef; 2] = [
    hard(ResourceSlot::Resource, ITM_TYPE),
    soft(ResourceSlot::Resource2, WAV_TYPE),
];

/// Opcode #122 (EE): the item created is picked at random from whichever of the
/// three resource fields are non-empty, so all three name items. (#255, its
/// sibling, documents no such fallback.)
static ITEM_RANDOM_OF_THREE: [ResourceRef; 3] = [
    hard(ResourceSlot::Resource, ITM_TYPE),
    hard(ResourceSlot::Resource2, ITM_TYPE),
    hard(ResourceSlot::Resource3, ITM_TYPE),
];

/// Opcode #182: apply the EFF in `Resource2` while the ITM in `Resource` is
/// equipped — the two slots name different types.
static ITEM_THEN_EFFECT: [ResourceRef; 2] = [
    hard(ResourceSlot::Resource, ITM_TYPE),
    hard(ResourceSlot::Resource2, EFF_TYPE),
];

/// "The spell or item specified in the Resource field" (#318/#321/#324): both
/// readings are offered, neither is certain.
static ITEM_OR_SPELL: [ResourceRef; 2] = [
    soft(ResourceSlot::Resource, ITM_TYPE),
    soft(ResourceSlot::Resource, SPL_TYPE),
];

/// The spell named by the effect's one resref field.
static SPELL: [ResourceRef; 1] = [hard(ResourceSlot::Resource, SPL_TYPE)];

/// Opcode #78 (Mold Touch) applies a SPL only in one of its modes.
static SPELL_MAYBE: [ResourceRef; 1] = [soft(ResourceSlot::Resource, SPL_TYPE)];

/// Opcode #256: the sequencer stores a spell, a VVC, and a second spell.
static SEQUENCER: [ResourceRef; 3] = [
    hard(ResourceSlot::Resource, SPL_TYPE),
    hard(ResourceSlot::Resource2, VVC_TYPE),
    hard(ResourceSlot::Resource3, SPL_TYPE),
];

/// The creature named by the effect's one resref field.
static CREATURE: [ResourceRef; 1] = [hard(ResourceSlot::Resource, CRE_TYPE)];

/// Opcode #67: the creature to summon, plus (EE) an animation at the summoned
/// creature's location and another at the ability target's.
static CREATURE_WITH_ANIMATIONS: [ResourceRef; 5] = [
    hard(ResourceSlot::Resource, CRE_TYPE),
    soft(ResourceSlot::Resource2, VVC_TYPE),
    soft(ResourceSlot::Resource2, BAM_TYPE),
    soft(ResourceSlot::Resource3, VVC_TYPE),
    soft(ResourceSlot::Resource3, BAM_TYPE),
];

/// An EFF file applied by the effect (#248/#249 note explicitly that the field
/// "references a EFF not a SPL").
static EFFECT: [ResourceRef; 1] = [hard(ResourceSlot::Resource, EFF_TYPE)];

/// A custom 2DA driving the effect's choice of spells or monsters.
static TABLE: [ResourceRef; 1] = [hard(ResourceSlot::Resource, TWODA_TYPE)];

static SCRIPT: [ResourceRef; 1] = [hard(ResourceSlot::Resource, BCS_TYPE)];
static PORTRAIT: [ResourceRef; 1] = [hard(ResourceSlot::Resource, BMP_TYPE)];
static MOVIE: [ResourceRef; 1] = [hard(ResourceSlot::Resource, MVE_TYPE)];
static SOUND: [ResourceRef; 1] = [hard(ResourceSlot::Resource, WAV_TYPE)];
static AREA: [ResourceRef; 1] = [hard(ResourceSlot::Resource, ARE_TYPE)];

/// "Animation file (VVC or BAM)" — the engine takes either, so both are
/// offered as candidates.
static ANIMATION: [ResourceRef; 2] = [
    soft(ResourceSlot::Resource, VVC_TYPE),
    soft(ResourceSlot::Resource, BAM_TYPE),
];

/// Human name for an effect `opcode`, or `None` if it is outside the range
/// this engine defines (0–367). Used only to annotate readable output.
pub fn opcode_name(opcode: u32) -> Option<&'static str> {
    OPCODE_NAMES.get(opcode as usize).copied()
}

/// Which effect field(s) *unconditionally* carry a TLK strref for `opcode`, in
/// the EE engine — the key to finding strrefs buried in item/spell/creature
/// effects, since the parser stores every parameter as an untyped dword.
///
/// Most string opcodes put the strref in Parameter #1; a couple (EE) use the
/// Special field. Value-dependent cases can't be expressed here (this keys only
/// on the opcode) and are handled by the caller: op319's Parameter #1 is a
/// strref when Parameter #2 is 10 (see [`effect_strref_uses`](super::eff)). One
/// case is deferred entirely — op330's Parameter #3 is the start of a `Count`
/// (Special)-long *range* of strrefs, only under the EFF-invocation path.
///
/// Derived from IESDP (`md/opcodes/bgee.md`); opcodes 13 and 68 are excluded —
/// their "Text Notification" Parameter #1 is a 0/1 flag, not a strref.
pub fn opcode_strref_slots(opcode: u32) -> &'static [StrrefSlot] {
    match opcode {
        103 | 139 | 180 | 206 | 253 | 254 | 267 | 290 | 330 | 338 => &[StrrefSlot::Param1],
        181 | 319 => &[StrrefSlot::Special],
        _ => &[],
    }
}

/// Which of an effect's resref fields carry a resource for `opcode`, and what
/// each names — the key to finding resources buried in item/spell/creature
/// effects, since the parser stores every resref as an untyped string.
///
/// Complete for BG(2):EE as far as IESDP documents it (`md/opcodes/bgee.md`):
/// every opcode whose resref field names a *file* is listed. Opcodes that put
/// something else in a resref field are deliberately excluded — #265/#309 store
/// a variable name, #144/#279 an arbitrary matching key, #319 a scripting name,
/// #339 an ARE animation label, and #336 only a filename *prefix* that the
/// engine completes with a sequence number.
///
/// A slot may appear more than once with different types. That is how the
/// genuinely ambiguous fields are expressed: "the spell or item specified in
/// the Resource field" (#318/#321/#324) yields both an ITM and a SPL reading,
/// each marked soft, so a search for either finds it as a candidate. IESDP
/// flags this collision directly — `spwi112.itm` and `spwi112.spl` are
/// indistinguishable to these opcodes. Animation fields work the same way,
/// since the engine accepts a VVC or a BAM.
pub fn opcode_resource_slots(opcode: u32) -> &'static [ResourceRef] {
    match opcode {
        // Items: create, remove, restrict, drain.
        111 | 123 | 143 | 180 | 243 | 255 => &ITEM,
        112 => &ITEM_AND_SOUND,
        122 => &ITEM_RANDOM_OF_THREE,
        182 => &ITEM_THEN_EFFECT,
        318 | 321 | 324 => &ITEM_OR_SPELL,
        // Spells: cast, learn, grant, remove, react with.
        146
        | 147
        | 148
        | 171
        | 172
        | 200
        | 201
        | 206
        | 207
        | 223
        | 226..=228
        | 232
        | 251
        | 252
        | 258..=260
        | 266
        | 313
        | 326
        | 333
        | 335
        | 340
        | 341
        | 361
        | 366 => &SPELL,
        78 => &SPELL_MAYBE,
        256 => &SEQUENCER,
        // Creatures: summon, replace, polymorph into.
        67 => &CREATURE_WITH_ANIMATIONS,
        135 | 151 => &CREATURE,
        // Effects applied from another file.
        177 | 183 | 248 | 249 | 272 | 283 => &EFFECT,
        // Everything else that names a file.
        82 => &SCRIPT,
        107 => &PORTRAIT,
        127 | 214 | 273 | 331 => &TABLE,
        152 => &MOVIE,
        174 => &SOUND,
        186 => &AREA,
        68 | 153..=158 | 215 | 296 => &ANIMATION,
        _ => &[],
    }
}

/// Opcode → name, indexed directly by opcode number. Bulk data appendix; the
/// module's logic is above.
static OPCODE_NAMES: [&str; 368] = [
    "Stat: AC vs. Damage Type Modifier",               // 0
    "Stat: Attacks Per Round Modifier",                // 1
    "Cure: Sleep",                                     // 2
    "State: Berserking",                               // 3
    "Cure: Berserking",                                // 4
    "Charm: Charm Specific Creature",                  // 5
    "Stat: Charisma Modifier",                         // 6
    "Colour: Set Character colours by Palette",        // 7
    "Colour: Change by RGB",                           // 8
    "Colour: Glow Pulse",                              // 9
    "Stat: Constitution Modifier",                     // 10
    "Cure: Poison",                                    // 11
    "HP: Damage",                                      // 12
    "Death: Instant Death",                            // 13
    "Graphics: Defrost",                               // 14
    "Stat: Dexterity Modifier",                        // 15
    "State: Haste",                                    // 16
    "HP: Current HP Modifier",                         // 17
    "HP: Maximum HP Modifier",                         // 18
    "Stat: Intelligence Modifier",                     // 19
    "State: Invisibility",                             // 20
    "Stat: Lore Modifier",                             // 21
    "Stat: Cumulative Luck Bonus",                     // 22
    "Stat: Morale Modifier",                           // 23
    "State: Horror",                                   // 24
    "State: Poison",                                   // 25
    "Item: Remove Curse",                              // 26
    "Stat: Acid Resistance Modifier",                  // 27
    "Stat: Cold Resistance Modifier",                  // 28
    "Stat: Electricity Resistance Modifier",           // 29
    "Stat: Fire Resistance Modifier",                  // 30
    "Stat: Magic Damage Resistance Modifier",          // 31
    "Cure: Death (Raise Dead)",                        // 32
    "Stat: Save vs. Death Modifier",                   // 33
    "Stat: Save vs. Wands Modifier",                   // 34
    "Stat: Save vs. Petrification/Polymorph Modifier", // 35
    "Stat: Save vs. Breath Weapons Modifier",          // 36
    "Stat: Save vs. Spells Modifier",                  // 37
    "State: Silence",                                  // 38
    "State: Unconsciousness",                          // 39
    "State: Slow",                                     // 40
    "Graphics: Sparkle",                               // 41
    "Spell: Wizard Spell Slots Modifier",              // 42
    "Cure: Stone to Flesh",                            // 43
    "Stat: Strength Modifier",                         // 44
    "State: Stun",                                     // 45
    "Cure: Stun (Unstun)",                             // 46
    "Cure: Invisibility",                              // 47
    "Cure: Silence (Vocalize)",                        // 48
    "Stat: Wisdom Modifier",                           // 49
    "Colour: Glow by RGB (Brief)",                     // 50
    "Colour: Strong/Dark by RGB",                      // 51
    "Colour: Very Bright by RGB",                      // 52
    "Graphics: Animation Change",                      // 53
    "Stat: THAC0 Modifier",                            // 54
    "Death: Kill Creature Type",                       // 55
    "Alignment: Invert",                               // 56
    "Alignment: Change",                               // 57
    "Cure: Dispellable Effects (Dispel Magic)",        // 58
    "Stat: Stealth Modifier",                          // 59
    "Stat: Miscast Magic",                             // 60
    "Creature RGB color fade",                         // 61
    "Spell: Priest Spell Slots Modifier",              // 62
    "State: Infravision",                              // 63
    "State: Remove Infravision",                       // 64
    "Overlay: Blur",                                   // 65
    "Graphics: Transparency Fade",                     // 66
    "Summon: Creature Summoning",                      // 67
    "Summon: Unsummon Creature",                       // 68
    "Protection: From Detection (Non-Detection)",      // 69
    "Cure: Non-Detection",                             // 70
    "IDS: Sex Change",                                 // 71
    "IDS: Set IDS State",                              // 72
    "Stat: Extra Damage Modifier",                     // 73
    "State: Blindness",                                // 74
    "Cure: Blindness",                                 // 75
    "State: Feeblemindedness",                         // 76
    "Cure: Feeblemindedness",                          // 77
    "State: Disease",                                  // 78
    "Cure: Disease",                                   // 79
    "State: Deafness",                                 // 80
    "Cure: Deafness",                                  // 81
    "Set AI Script",                                   // 82
    "Protection: From Projectile",                     // 83
    "Stat: Magical Fire Resistance Modifier",          // 84
    "Stat: Magical Cold Resistance Modifier",          // 85
    "Stat: Slashing Resistance Modifier",              // 86
    "Stat: Crushing Resistance Modifier",              // 87
    "Stat: Piercing Resistance Modifier",              // 88
    "Stat: Missiles Resistance Modifier",              // 89
    "Stat: Open Locks Modifier",                       // 90
    "Stat: Find Traps Modifier",                       // 91
    "Stat: Pick Pockets Modifier",                     // 92
    "Stat: Fatigue Modifier",                          // 93
    "Stat: Drunkenness Modifier",                      // 94
    "Stat: Tracking Skill Modifier",                   // 95
    "Stat: Level Change",                              // 96
    "Stat: Exceptional Strength Modifier",             // 97
    "HP: Regeneration",                                // 98
    "Spell Effect: Duration Modifier",                 // 99
    "Protection: from Creature Type",                  // 100
    "Protection: from Opcode",                         // 101
    "Spell: Immunity (by Power Level)",                // 102
    "Text: Change Name",                               // 103
    "Stat: Experience Points",                         // 104
    "Stat: Gold",                                      // 105
    "Stat: Morale Break Modifier",                     // 106
    "Portrait Change",                                 // 107
    "Stat: Reputation",                                // 108
    "State: Paralyze",                                 // 109
    "Empty:",                                          // 110
    "Item: Create Magical Weapon",                     // 111
    "Item: Remove Item",                               // 112
    "Empty:",                                          // 113
    "Graphics: Dither",                                // 114
    "Detect: Alignment",                               // 115
    "State: Cure Invisibility",                        // 116
    "Spell Effect: Reveal Area",                       // 117
    "Empty:",                                          // 118
    "Spell Effect: Mirror Image",                      // 119
    "Protection: from Weapons",                        // 120
    "Empty:",                                          // 121
    "Item: Create Inventory Item",                     // 122
    "Item: Remove Inventory Item",                     // 123
    "Spell Effect: Teleport (Dimension Door)",         // 124
    "Spell Effect: Unlock (Knock)",                    // 125
    "Stat: Movement Modifier",                         // 126
    "Summon: Monster Summoning",                       // 127
    "State: Confusion",                                // 128
    "State: Aid",                                      // 129
    "State: Bless",                                    // 130
    "State: Positive Chant",                           // 131
    "State: Raise Strength, Constitution, & Dexterity Non-Cumulative", // 132
    "Spell Effect: Luck Non-Cumulative",               // 133
    "State: Petrification",                            // 134
    "Graphics: Polymorph into Specific",               // 135
    "State: Force Visible",                            // 136
    "State: Negative Chant",                           // 137
    "Graphics: Character Animation Change",            // 138
    "Text: Display String",                            // 139
    "Graphics: Casting Glow",                          // 140
    "Graphics: Lighting Effects",                      // 141
    "Graphics: Display Special Effect Icon",           // 142
    "Item: Create Item in Slot",                       // 143
    "Button: Disable Button",                          // 144
    "Spell: Disable Spell Casting Abilities",          // 145
    "Spell: Cast Spell (at Creature)",                 // 146
    "Spell: Learn Spell",                              // 147
    "Spell: Cast Spell (at Point)",                    // 148
    "(Identify)",                                      // 149
    "Spell Effect: Find Traps",                        // 150
    "Summon: Replace Creature",                        // 151
    "Spell Effect: Play Movie",                        // 152
    "Overlay: Sanctuary",                              // 153
    "Overlay: Entangle",                               // 154
    "Overlay: Minor Globe",                            // 155
    "Overlay: Protection from Normal Missiles Cylinder", // 156
    "State: Web Effect",                               // 157
    "Overlay: Grease",                                 // 158
    "Spell Effect: Mirror Image (Exact Number)",       // 159
    "Remove Sanctuary",                                // 160
    "Cure: Horror",                                    // 161
    "Cure: Hold",                                      // 162
    "Protection: Free Action",                         // 163
    "Cure: Drunkeness",                                // 164
    "Spell Effect: Pause Target",                      // 165
    "Stat: Magic Resistance Modifier",                 // 166
    "Stat: THAC0 Modifier with Missile Weapons",       // 167
    "Summon: Remove Creature",                         // 168
    "Graphics: Prevent Special Effect Icon",           // 169
    "Graphics: Play Damage Animation",                 // 170
    "Spell: Give Ability",                             // 171
    "Spell: Remove Spell",                             // 172
    "Stat: Poison Resistance Modifier",                // 173
    "Spell Effect: Play Sound Effect",                 // 174
    "State: Hold",                                     // 175
    "Stat: Movement Modifier (II)",                    // 176
    "Use EFF File",                                    // 177
    "Spell Effect: THAC0 vs. Creature Type Modifier",  // 178
    "Spell Effect: Damage vs. Creature Type Modifier", // 179
    "Item: Can't Use Item",                            // 180
    "Item: Can't Use Itemtype",                        // 181
    "Use EFF file if/while item resource equipped",    // 182
    "Item: Apply Effect Itemtype",                     // 183
    "Graphics: Passwall (Don't Jump)",                 // 184
    "State: Hold (II)",                                // 185
    "Script: MoveToArea",                              // 186
    "Script: Store Local Variable",                    // 187
    "Spell Effect: Aura Cleansing",                    // 188
    "Stat: Casting Time Modifier",                     // 189
    "Stat: Attack Speed Factor",                       // 190
    "Spell: Casting Level Modifier",                   // 191
    "Spell Effect: Find Familiar",                     // 192
    "Spell Effect: Invisible Detection by Script",     // 193
    "Ignore Dialog Pause",                             // 194
    "Spell Effect: Death Dependent Constitution Loss (Familiar Bond)", // 195
    "Spell Effect: Familiar Block",                    // 196
    "Spell: Bounce (by Impact Projectile)",            // 197
    "Spell: Bounce (by Opcode)",                       // 198
    "Spell: Bounce (by Power Level)",                  // 199
    "Spell: Bounce (by Power level, decrementing)",    // 200
    "Spell: Immunity (by Power Level, decrementing)",  // 201
    "Spell: Bounce (by School)",                       // 202
    "Spell: Bounce (by Secondary Type)",               // 203
    "Spell: Protection (by School)",                   // 204
    "Spell: Protection (by Secondary Type)",           // 205
    "Spell: Protection from Spell",                    // 206
    "Spell: Bounce (by Resource)",                     // 207
    "HP: Minimum Limit",                               // 208
    "Death: Kill 60HP",                                // 209
    "Spell Effect: Stun 90HP",                         // 210
    "Spell Effect: Imprisonment",                      // 211
    "Protection: Freedom",                             // 212
    "Spell Effect: Maze",                              // 213
    "Spell Effect: Select Spell",                      // 214
    "Graphics: Play 3D Effect",                        // 215
    "Spell Effect: Level Drain",                       // 216
    "Spell Effect: Unconsciousness 20HP",              // 217
    "Protection: Stoneskin",                           // 218
    "Stat: AC vs. Creature Type Modifier",             // 219
    "Removal: Remove School",                          // 220
    "Removal: Remove Secondary Type",                  // 221
    "Spell Effect: Teleport Field",                    // 222
    "Spell: Immunity (by School, decrementing)",       // 223
    "Cure: Level Drain (Restoration)",                 // 224
    "Spell: Reveal Magic",                             // 225
    "Spell: Immunity (by Secondary Type, decrementing)", // 226
    "Spell: Bounce (by School, decrementing)",         // 227
    "Spell: Bounce (by Secondary Type, decrementing)", // 228
    "Removal: Remove One School",                      // 229
    "Removal: Remove One Secondary Type",              // 230
    "Spell Effect: Time Stop",                         // 231
    "Spell Effect: Cast Spell on Condition",           // 232
    "Stat: Proficiency Modifier",                      // 233
    "Spell Effect: Contingency Creation",              // 234
    "Spell Effect: Wing Buffet",                       // 235
    "Spell Effect: Image Projection",                  // 236
    "Spell Effect: Puppet ID",                         // 237
    "Death: Disintegrate",                             // 238
    "Spell Effect: Farsight",                          // 239
    "Graphics: Remove Special Effect Icon",            // 240
    "Charm: Control Creature",                         // 241
    "Cure: Confusion",                                 // 242
    "Item: Drain Item Charges",                        // 243
    "Spell: Drain Wizard Spell",                       // 244
    "Check For Berserk",                               // 245
    "Spell Effect: Berserking",                        // 246
    "Spell Effect: Attack Nearest Creature",           // 247
    "Item: Set Melee Effect",                          // 248
    "Item: Set Ranged Effect",                         // 249
    "Spell Effect: Damage Modifier",                   // 250
    "Spell Effect: Change Bard Song Effect",           // 251
    "Spell Effect: Set Trap",                          // 252
    "Spell Effect: Add Map Marker",                    // 253
    "Spell Effect: Remove Map Marker",                 // 254
    "Item: Create Inventory Item (days)",              // 255
    "Spell: Spell Sequencer Active",                   // 256
    "Spell: Spell Sequencer Creation",                 // 257
    "Spell: Spell Sequencer Activation",               // 258
    "Protection: Spell Trap (by Power Level, decrementing)", // 259
    "Spell: Spell Sequencer Activation",               // 260
    "Spell: Restore Lost Spells",                      // 261
    "Stat: Visual Range",                              // 262
    "Stat: Backstab",                                  // 263
    "Spell Effect: Drop Weapons in Panic",             // 264
    "Script: Modify Global Variable",                  // 265
    "Spell: Remove Protection from Spell",             // 266
    "Text: Protection from Display Specific String",   // 267
    "Spell Effect: Clear Fog of War (Wizard Eye)",     // 268
    "Spell Effect: Shake Window",                      // 269
    "Cure: Unpause Target",                            // 270
    "Graphics: Avatar Removal",                        // 271
    "Spell: Apply Repeating EFF",                      // 272
    "Remove: Specific Area Effect(Zone of Sweet Air)", // 273
    "Spell Effect: Teleport to Target",                // 274
    "Stat: Hide in Shadows Modifier",                  // 275
    "Stat: Detect Illusion Modifier",                  // 276
    "Stat: Set Traps Modifier",                        // 277
    "Stat: To Hit Modifier",                           // 278
    "Button: Enable Button",                           // 279
    "Spell Effect: Wild Magic",                        // 280
    "Stat: Wild Magic",                                // 281
    "Script: Scripting State Modifier",                // 282
    "Use EFF File (Cursed)",                           // 283
    "Stat: Melee THAC0 Modifier",                      // 284
    "Stat: Melee Weapon Damage Modifier",              // 285
    "Stat: Missile Weapon Damage Modifier",            // 286
    "Graphics: Selection Circle Removal",              // 287
    "Stat: Fist THAC0 Modifier",                       // 288
    "Stat: Fist Damage Modifier",                      // 289
    "Text: Change Title",                              // 290
    "Graphics: Disable Visual Effect",                 // 291
    "Protection: Backstab",                            // 292
    "Script: Enable Offscreen AI",                     // 293
    "Spell Effect: Existance Delay Override",          // 294
    "Graphics: Disable Permanent Death",               // 295
    "Graphics: Protection from Specific Animation",    // 296
    "Spell Effect: Immunity to Turn Undead",           // 297
    "Spell Effect: Execute Script cut250a",            // 298
    "Spell Effect: Chaos Shield",                      // 299
    "Spell Effect: NPCBump",                           // 300
    "Stat: Critical Hit Modifier",                     // 301
    "Item: Can Use Any Item",                          // 302
    "Spell Effect: Backstab Every Hit",                // 303
    "Mass Raise Dead",                                 // 304
    "Stat: THAC0 Modifier (Off-Hand)",                 // 305
    "Stat: THAC0 Modifier (On-Hand)",                  // 306
    "Ranger Tracking Ability",                         // 307
    "Protection: From Tracking",                       // 308
    "Script: Modify Local Variable",                   // 309
    "Protection: from Timestop",                       // 310
    "Spell: Random Wish Spell",                        // 311
    "Crash",                                           // 312
    "High-Level Ability Denotation",                   // 313
    "Spell: Golem Stoneskin",                          // 314
    "Graphics: Animation Removal",                     // 315
    "Spell: Magical Rest",                             // 316
    "State: Haste 2",                                  // 317
    "Protection from Resource",                        // 318
    "Usability: Item Usability",                       // 319
    "Change Weather",                                  // 320
    "Removal: Effects specified by Resource",          // 321
    "Evade Area of Effect",                            // 322
    "Stat: Turn Undead Level",                         // 323
    "Protection: Immunity to Resource and Message",    // 324
    "Stat: Save vs. all",                              // 325
    "Apply Effects List",                              // 326
    "Graphics: Icewind Visual Spell Hit (plays sound)", // 327
    "State: Set Extended or Spell State",              // 328
    "Spell Effect: Slow Poison",                       // 329
    "Text: Float Text",                                // 330
    "Summon: Random Monster Summoning",                // 331
    "Stat: Specific Damage Modifier",                  // 332
    "Spell Effect: Static Charge",                     // 333
    "Spell Effect: Turn Undead",                       // 334
    "Spell Effect: Seven Eyes",                        // 335
    "Graphics: Display Eyes Overlay",                  // 336
    "Remove: Opcode",                                  // 337
    "Disable Rest",                                    // 338
    "Alter Animation",                                 // 339
    "Spell Effect: Change Backstab Effect",            // 340
    "Spell Effect: Change Critical Hit Effect",        // 341
    "Animation: Override Data",                        // 342
    "HP Swap",                                         // 343
    "Enchantment vs. creature type",                   // 344
    "Enchantment bonus",                               // 345
    "Save vs. school bonus",                           // 346
    "Unused",                                          // 347
    "Unused",                                          // 348
    "Unused",                                          // 349
    "Unused",                                          // 350
    "Unused",                                          // 351
    "Unused",                                          // 352
    "Unused",                                          // 353
    "Unused",                                          // 354
    "Unused",                                          // 355
    "Unused",                                          // 356
    "Unused",                                          // 357
    "Unused",                                          // 358
    "Unused",                                          // 359
    "Stat: Ignore Reputation Breaking Point",          // 360
    "Cast spell on critical miss",                     // 361
    "Critical miss bonus",                             // 362
    "Modal state check",                               // 363
    "Empty",                                           // 364
    "Make unselectable",                               // 365
    "Spell: Apply Spell On Move",                      // 366
    "Minimum base stats",                              // 367
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strref_slots_match_the_iesdp_audit() {
        // Parameter #1 string opcodes.
        for op in [103, 139, 180, 206, 253, 254, 267, 290, 330, 338] {
            assert_eq!(opcode_strref_slots(op), [StrrefSlot::Param1], "opcode {op}");
        }
        // Special-field string opcodes (EE).
        for op in [181, 319] {
            assert_eq!(
                opcode_strref_slots(op),
                [StrrefSlot::Special],
                "opcode {op}"
            );
        }
        // Excluded false positives (13/68 "Text Notification" is a 0/1 flag) and
        // ordinary numeric opcodes carry no strref.
        for op in [0, 1, 13, 68, 367, 999] {
            assert!(opcode_strref_slots(op).is_empty(), "opcode {op}");
        }
    }

    fn kinds(opcode: u32) -> Vec<(ResourceSlot, u16, bool)> {
        opcode_resource_slots(opcode)
            .iter()
            .map(|r| (r.slot, r.kind, r.soft))
            .collect()
    }

    #[test]
    fn each_family_names_its_own_type() {
        use ResourceSlot::Resource;
        assert_eq!(kinds(122)[0], (Resource, ITM_TYPE, false));
        assert_eq!(kinds(146)[0], (Resource, SPL_TYPE, false));
        assert_eq!(kinds(151)[0], (Resource, CRE_TYPE, false));
        assert_eq!(kinds(177)[0], (Resource, EFF_TYPE, false));
        assert_eq!(kinds(82)[0], (Resource, BCS_TYPE, false));
        assert_eq!(kinds(186)[0], (Resource, ARE_TYPE, false));
        assert_eq!(kinds(174)[0], (Resource, WAV_TYPE, false));
        assert_eq!(kinds(107)[0], (Resource, BMP_TYPE, false));
        assert_eq!(kinds(152)[0], (Resource, MVE_TYPE, false));
        assert_eq!(kinds(214)[0], (Resource, TWODA_TYPE, false));
    }

    #[test]
    fn ambiguous_fields_offer_every_reading_and_mark_them_soft() {
        use ResourceSlot::Resource;
        // "The spell or item specified in the Resource field."
        assert_eq!(
            kinds(318),
            vec![(Resource, ITM_TYPE, true), (Resource, SPL_TYPE, true)]
        );
        // "Animation file (VVC or BAM)."
        assert_eq!(
            kinds(215),
            vec![(Resource, VVC_TYPE, true), (Resource, BAM_TYPE, true)]
        );
    }

    #[test]
    fn multi_slot_opcodes_type_each_slot_separately() {
        use ResourceSlot::{Resource, Resource2, Resource3};
        // #182: the item to check for, then the effect to apply.
        assert_eq!(
            kinds(182),
            vec![(Resource, ITM_TYPE, false), (Resource2, EFF_TYPE, false)]
        );
        // #256: a spell, the sequencer's VVC, then a second spell.
        assert_eq!(
            kinds(256),
            vec![
                (Resource, SPL_TYPE, false),
                (Resource2, VVC_TYPE, false),
                (Resource3, SPL_TYPE, false),
            ]
        );
    }

    #[test]
    fn fields_that_are_not_resrefs_are_excluded() {
        // Variable names (#265/#309), matching keys (#144/#279), a scripting
        // name (#319), an ARE animation label (#339), and a filename prefix
        // the engine completes itself (#336) all live in a resref field but
        // name no file.
        for opcode in [265, 309, 144, 279, 319, 339, 336] {
            assert!(
                opcode_resource_slots(opcode).is_empty(),
                "opcode {opcode} stores a non-resref in its resource field"
            );
        }
    }
}
