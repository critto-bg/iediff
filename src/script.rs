//! What the engine's script calls name, so a quoted resref in a DLG script can
//! be typed by the call that takes it.
//!
//! [`SIGNATURES`] copies `ACTION.IDS` / `TRIGGER.IDS` verbatim rather than
//! reading them: the calls are implemented in the closed-source executable, so
//! a mod cannot add one without patching it, and a static table keeps the
//! search working without the game present.
//!
//! What the IDS cannot supply is what a parameter *name* means. It says
//! `HasItem(S:ResRef*,…)` and `Global(S:Name*,S:Area*,…)`; only convention says
//! the first is an item and the second pair a variable and its scope. Some
//! names lie outright — `GiveItem(S:Object*,…)` takes an item.
//!
//! So the tables stay conservative: uncertainty resolves to
//! [`ArgKind::Unknown`], which admits rather than rejects.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::formats::are::ARE_TYPE;
use crate::formats::cre::CRE_TYPE;
use crate::formats::dlg::DLG_TYPE;
use crate::formats::eff::EFF_TYPE;
use crate::formats::itm::ITM_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::formats::{BCS_TYPE, BMP_TYPE, WAV_TYPE};

/// What a script call's string argument names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgKind {
    /// A resource of this type.
    Resource(u16),
    /// A variable, scope, token or death variable — never a reference.
    Variable,
    /// No opinion: the call is unlisted, or its parameter is a generic
    /// `S:ResRef*` whose meaning depends on the call.
    Unknown,
}

/// For each call, what each argument names.
#[derive(Default)]
pub struct Vocabulary {
    /// Lowercased call name -> argument kinds, by position.
    calls: HashMap<String, Vec<ArgKind>>,
}

/// The engine's vocabulary, parsed once and shared.
pub fn engine() -> &'static Vocabulary {
    static VOCABULARY: OnceLock<Vocabulary> = OnceLock::new();
    VOCABULARY.get_or_init(|| {
        let mut calls = HashMap::new();
        for signature in SIGNATURES {
            if let Some((name, kinds)) = parse_signature(signature) {
                // First definition wins, as when a name appears in both tables.
                calls.entry(name).or_insert(kinds);
            }
        }
        Vocabulary { calls }
    })
}

impl Vocabulary {
    /// What argument `arg` of `call` names. [`ArgKind::Unknown`] when the call
    /// is unlisted or the position is past its declared arguments.
    pub fn arg_kind(&self, call: &str, arg: usize) -> ArgKind {
        self.calls
            .get(&call.to_ascii_lowercase())
            .and_then(|kinds| kinds.get(arg))
            .copied()
            .unwrap_or(ArgKind::Unknown)
    }

    /// Whether argument `arg` of `call` could name a resource of type `kind`.
    /// Only a confident contradiction rejects, so an unlisted call still
    /// reports.
    pub fn could_name(&self, call: &str, arg: usize, kind: u16) -> bool {
        match self.arg_kind(call, arg) {
            ArgKind::Resource(named) => named == kind,
            ArgKind::Variable => false,
            ArgKind::Unknown => true,
        }
    }

    /// How many calls are typed; confirms the table parsed in full.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.calls.len()
    }
}

/// Split `SetGlobal(S:Name*,S:Area*,I:Value*)` into a lowercased name and the
/// kind of each argument. Positions count *every* parameter, since the scanner
/// that consumes this counts commas.
fn parse_signature(signature: &str) -> Option<(String, Vec<ArgKind>)> {
    let (name, rest) = signature.split_once('(')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let args = rest.strip_suffix(')').unwrap_or(rest);

    // `T:Name*Suffix`; only the `S` (string) class can hold a resref.
    let params: Vec<(char, &str)> = args
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let p = p.trim();
            let (class, tail) = p.split_once(':').unwrap_or(("?", p));
            let param = tail.split('*').next().unwrap_or(tail);
            (class.chars().next().unwrap_or('?'), param)
        })
        .collect();

    // An `Area` alongside a variable name is that variable's *scope*, not an
    // area reference — the `Global`/`SetGlobal` shape. `MoveGlobal` has no name
    // parameter and really does name an area.
    let scoped = params
        .iter()
        .any(|(class, param)| *class == 'S' && is_variable_name(param));

    let kinds = params
        .iter()
        .map(|(class, param)| {
            if *class != 'S' {
                return ArgKind::Unknown;
            }
            if let Some(kind) = call_override(name, param) {
                return kind;
            }
            if is_variable_name(param) {
                return ArgKind::Variable;
            }
            if param.eq_ignore_ascii_case("area") && scoped {
                return ArgKind::Variable;
            }
            param_kind(param)
        })
        .collect();

    Some((name.to_ascii_lowercase(), kinds))
}

/// Parameter names holding a variable or scope rather than a resource.
fn is_variable_name(param: &str) -> bool {
    const NAMES: [&str; 8] = [
        "name", "name1", "name2", "global", "scope", "var1", "var2", "token",
    ];
    NAMES.iter().any(|n| param.eq_ignore_ascii_case(n))
}

/// What a parameter name conventionally holds. Only searchable types are
/// listed; the rest stay [`Unknown`](ArgKind::Unknown).
fn param_kind(param: &str) -> ArgKind {
    const TABLE: [(&str, u16); 15] = [
        ("item", ITM_TYPE),
        ("itemname", ITM_TYPE),
        ("olditem", ITM_TYPE),
        ("newitem", ITM_TYPE),
        ("give", ITM_TYPE),
        ("take", ITM_TYPE),
        ("spell", SPL_TYPE),
        ("res", SPL_TYPE), // used only by the *SpellRES family
        ("dialogfile", DLG_TYPE),
        ("effect", EFF_TYPE),
        ("sound", WAV_TYPE),
        ("script", BCS_TYPE),
        ("scriptfile", BCS_TYPE),
        ("palette", BMP_TYPE),
        ("area", ARE_TYPE),
    ];
    TABLE
        .iter()
        .find(|(name, _)| param.eq_ignore_ascii_case(name))
        .map_or(ArgKind::Unknown, |&(_, kind)| ArgKind::Resource(kind))
}

/// Calls whose parameter name is too generic to type — `S:ResRef*` covers
/// items, areas, spells and global variables alike. Omissions fall through to
/// [`ArgKind::Unknown`] and still report, so this trades effort for precision,
/// not correctness.
fn call_override(call: &str, param: &str) -> Option<ArgKind> {
    const GENERIC: [&str; 3] = ["resref", "object", "newobject"];
    if !GENERIC.iter().any(|g| param.eq_ignore_ascii_case(g)) {
        return None;
    }
    // `S:Object*` on the item verbs is an item, whatever the name suggests.
    const ITEMS: [&str; 28] = [
        "acquired",
        "contains",
        "createitem",
        "currentammo",
        "destroyitem",
        "dropitem",
        "equipitem",
        "equipitemex",
        "getitem",
        "giveitem",
        "giveitemcreate",
        "hasitem",
        "hasitemequiped",
        "hasitemequipedreal",
        "numitems",
        "numitemsgt",
        "numitemslt",
        "numitemsparty",
        "numitemspartygt",
        "numitemspartylt",
        "partyhasitem",
        "partyhasitemidentified",
        "pickupitem",
        "takeitemlistparty",
        "takeitemlistpartynum",
        "takepartyitemnum",
        "unusable",
        "useitem",
    ];
    const AREAS: [&str; 6] = [
        "areacheck",
        "areacheckobject",
        "copygroundpilesto",
        "escapeareaobjectmove",
        "hideareaonmap",
        "revealareaonmap",
    ];
    // `G`/`GGT`/`GLT` declare `S:ResRef*`, but it is a global variable name.
    const GLOBALS: [&str; 3] = ["g", "ggt", "glt"];
    const CREATURES: [&str; 16] = [
        "createcreature",
        "createcreaturecopypoint",
        "createcreaturedoor",
        "createcreatureeffect",
        "createcreatureimpassable",
        "createcreatureimpassableallowoverlap",
        "createcreatureimpassableallowoverlapeffect",
        "createcreatureimpassableeffect",
        "createcreatureobject",
        "createcreatureobjectcopy",
        "createcreatureobjectcopyeffect",
        "createcreatureobjectdoor",
        "createcreatureobjecteffect",
        "createcreatureobjectoffscreen",
        "createcreatureobjectoffset",
        "createcreatureoffscreen",
    ];

    let has = |set: &[&str]| set.iter().any(|c| call.eq_ignore_ascii_case(c));
    if has(&ITEMS) {
        Some(ArgKind::Resource(ITM_TYPE))
    } else if has(&AREAS) {
        Some(ArgKind::Resource(ARE_TYPE))
    } else if has(&GLOBALS) {
        Some(ArgKind::Variable)
    } else if has(&CREATURES) {
        Some(ArgKind::Resource(CRE_TYPE))
    } else {
        None
    }
}

/// Every `ACTION.IDS` / `TRIGGER.IDS` call taking a string parameter, verbatim
/// from a BG2:EE install so it can be diffed against the game files. Calls
/// without one are omitted — no quoted resref can appear in them.
static SIGNATURES: [&str; 191] = [
    "Acquired(S:ResRef*)",
    "AddexperiencePartyGlobal(S:Name*,S:Area*)",
    "AddGlobals(S:Name*,S:Name2*)",
    "AddSpecialAbility(S:ResRef*)",
    "AddStoreItem(S:Store*,S:Item*,I:Count*,I:Flags*)",
    "AddWorldmapAreaFlag(S:Area*,I:Type*WMPFLAG)",
    "AddXP2DA(S:Column*)",
    "ApplySpellRES(S:RES*,O:Target)",
    "AreaCheck(S:ResRef*)",
    "AreaCheckObject(S:ResRef*,O:Object*)",
    "AttachTransitionToDoor(S:GLOBAL*,O:Object*)",
    "BeenInParty(S:Name*)",
    "BitCheck(S:Name*,S:Area*,I:Bit*Bits)",
    "BitClear(S:Name*,S:Scope*,I:Bit*Bits)",
    "BitSet(S:Name*,S:Scope*,I:Bit*Bits)",
    "ChangeAIScript(S:ScriptFile*,I:Level*Scrlev)",
    "ChangeAnimation(S:ResRef*)",
    "ChangeAnimationNoEffect(S:ResRef*)",
    "ChangeStoreMarkup(S:Store*,I:BuyMarkup*,I:SellMarkup*)",
    "CharName(S:Name*,O:Object*)",
    "CheckItemSlot(O:Object*,S:Item*,I:Slot*SLOTS)",
    "Contains(S:ResRef*,O:Object*)",
    "CopyGroundPilesTo(S:ResRef*,P:Location*)",
    "CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)",
    "CreateCreatureAtLocation(S:GLOBAL*,S:Area*,S:ResRef*)",
    "CreateCreatureCopyPoint(S:ResRef*,O:Object*,P:Dest*)",
    "CreateCreatureDoor(S:NewObject*,P:Location*,I:Face*DIR)",
    "CreateCreatureEffect(S:NewObject*,S:Effect*,P:Location*,I:Face*DIR)",
    "CreateCreatureImpassable(S:NewObject*,P:Location*,I:Face*DIR)",
    "CreateCreatureImpassableAllowOverlap(S:NewObject*,P:Location*,I:Face*DIR)",
    "CreateCreatureImpassableAllowOverlapEffect(S:NewObject*,S:Effect*,P:Location*,I:Face*DIR)",
    "CreateCreatureImpassableEffect(S:NewObject*,S:Effect*,P:Location*,I:Face*DIR)",
    "CreateCreatureObject(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "CreateCreatureObjectCopy(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "CreateCreatureObjectCopyEffect(S:ResRef*,S:Effect*,O:Object*)",
    "CreateCreatureObjectDoor(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "CreateCreatureObjectEffect(S:ResRef*,S:Effect*,O:Object*)",
    "CreateCreatureObjectOffScreen(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "CreateCreatureObjectOffset(S:ResRef*,O:Object*,P:Offset*)",
    "CreateCreatureOffScreen(S:ResRef*,I:Face*DIR)",
    "CreateItem(S:ResRef*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "CreateItemGlobal(S:Global*,S:Area*,S:ResRef*)",
    "CreateVisualEffect(S:Object*,P:Location*)",
    "CreateVisualEffectObject(S:Effect*,O:Target*)",
    "CurrentAmmo(S:ResRef*,O:Object*)",
    "Dead(S:Name*)",
    "DeathMatchPositionArea(S:Areaname*,P:Dest*,I:Player*)",
    "DeathMatchPositionGlobal(S:Areaname*,P:Dest*,I:Player*)",
    "DeathMatchPositionLocal(S:Areaname*,P:Dest*,I:Player*)",
    "DestroyItem(S:ResRef*)",
    "DisplayStringHeadOwner(S:Item*,I:STRREF*)",
    "DropItem(S:Object*,P:Location*)",
    "EquipItem(S:Object*)",
    "EquipItemEx(S:Object*,I:EquipUnEquip*)",
    "EscapeAreaMove(S:Area*,I:X*,I:Y*,I:Face*DIR)",
    "EscapeAreaObjectMove(S:ResRef*,O:Object*,I:X*,I:Y*,I:Face*DIR)",
    "ExportParty(S:Name*)",
    "ForceRandomEncounter(S:Area*)",
    "ForceRandomEncounterEntry(S:Area*,S:Entry*)",
    "ForceSpellPointRangeRES(S:RES*,P:Target)",
    "ForceSpellPointRES(S:RES*,P:Target)",
    "ForceSpellRangeRES(S:RES*,O:Target)",
    "ForceSpellRES(S:RES*,O:Target)",
    "G(S:ResRef*,I:Num*)",
    "GetItem(S:Object*,O:Target*)",
    "GGT(S:ResRef*,I:Num*)",
    "GiveItem(S:Object*,O:Target*)",
    "GiveItemCreate(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
    "GiveObjectGoldGlobal(S:Name*,S:Area*,O:Object*)",
    "GivePartyGoldGlobal(S:Name*,S:Area*)",
    "Global(S:Name*,S:Area*,I:Value*)",
    "GlobalGT(S:Name*,S:Area*,I:Value*)",
    "GlobalLT(S:Name*,S:Area*,I:Value*)",
    "GlobalsEqual(S:Name1*,S:Name2*)",
    "GlobalsGT(S:Name1*,S:Name2*)",
    "GlobalsLT(S:Name1*,S:Name2*)",
    "GlobalTimerExact(S:Name*,S:Area*)",
    "GlobalTimerExpired(S:Name*,S:Area*)",
    "GlobalTimerNotExpired(S:Name*,S:Area*)",
    "GLT(S:ResRef*,I:Num*)",
    "HasDLC(S:DLCName*)",
    "HasItem(S:ResRef*,O:Object*)",
    "HasItemEquiped(S:ResRef*,O:Object*)",
    "HasItemEquipedReal(S:ResRef*,O:Object*)",
    "HaveKnownSpellRES(S:Spell*)",
    "HaveSpellRES(S:Spell*)",
    "HideAreaOnMap(S:ResRef*)",
    "IncrementChapter(S:RESREF*)",
    "IncrementGlobal(S:Name*,S:Area*,I:Value*)",
    "IncrementGlobalOnce(S:Name1*,S:Area1*,S:Name2*,S:Area2*,I:Val*)",
    "IncrementGlobalOnceEx(S:Var1*,S:Var2*,I:Val*)",
    "INI(S:Name*,I:Number*)",
    "InLine(S:Target*,O:Object*)",
    "IsForcedRandomEncounterActive(S:Area*)",
    "LeaveArea(S:Area*,P:Point*,I:Face*DIR)",
    "LeaveAreaLUA(S:Area*,S:Parchment*,P:Point*,I:Face*DIR)",
    "LeaveAreaLUAEntry(S:Area*,S:Entry*,P:Point*,I:Face*DIR)",
    "LeaveAreaLUAPanic(S:Area*,S:Parchment*,P:Point*,I:Face*DIR)",
    "LeaveAreaLUAPanicEntry(S:Area*,S:Entry*,P:Point*,I:Face*DIR)",
    "LocalsEqual(S:Name1*,S:Name2*)",
    "LocalsGT(S:Name1*,S:Name2*)",
    "LocalsLT(S:Name1*,S:Name2*)",
    "MoveBetweenAreas(S:Area*,P:Location*,I:Face*DIR)",
    "MoveBetweenAreasEffect(S:Area*,S:Effect*,P:Location*,I:Face*DIR)",
    "MoveContainerContents(S:Container1*,S:Container2*)",
    "MoveGlobal(S:Area*,O:Object*,P:Point*)",
    "MoveGlobalsTo(S:FromArea*,S:ToArea*,P:Location*)",
    "MoveToCampaign(S:Campaign*)",
    "MoveToSavedLocation(S:GLOBAL*,S:Area*)",
    "MoveToSavedLocationn(S:GLOBAL*,S:Area*)",
    "Name(S:Name*,O:Object*)",
    "NearSavedLocation(O:Object*,S:Global*,I:Range*)",
    "NumDead(S:Name*,I:Num*)",
    "NumDeadGT(S:Name*,I:Num*)",
    "NumDeadLT(S:Name*,I:Num*)",
    "NumItems(S:ResRef*,O:Object*,I:Num*)",
    "NumItemsGT(S:ResRef*,O:Object*,I:Num*)",
    "NumItemsLT(S:ResRef*,O:Object*,I:Num*)",
    "NumItemsParty(S:ResRef*,I:Num*)",
    "NumItemsPartyGT(S:ResRef*,I:Num*)",
    "NumItemsPartyLT(S:ResRef*,I:Num*)",
    "PartyHasItem(S:Item*)",
    "PartyHasItemIdentified(S:ResRef*)",
    "PickUpItem(S:ResRef*)",
    "PlaySound(S:Sound*)",
    "PlaySoundNotRanged(S:Sound*)",
    "PlaySoundThroughVoice(S:Sound*)",
    "RealGlobalTimerExact(S:Name*,S:Area*)",
    "RealGlobalTimerExpired(S:Name*,S:Area*)",
    "RealGlobalTimerNotExpired(S:Name*,S:Area*)",
    "ReallyForceSpellDeadRES(S:RES*,O:Target)",
    "ReallyForceSpellPointRES(S:RES*,P:Target*)",
    "ReallyForceSpellRES(S:RES*,O:Target)",
    "RealSetGlobalTimer(S:Name*,S:Area*,I:Time*GTimes)",
    "RemoveSpellRES(S:RES*)",
    "RemoveStoreItem(S:Store*,S:Item*,I:Count*)",
    "RemoveWorldmapAreaFlag(S:Area*,I:Type*WMPFLAG)",
    "RevealAreaOnMap(S:ResRef*)",
    "SaveLocation(S:Area*,S:Global*,P:Point*)",
    "SaveObjectLocation(S:Area*,S:Global*,O:Object*)",
    "SetAreaScript(S:Script*,I:ScriptSlot*ASCRLEV)",
    "SetDialog(S:DialogFile*)",
    "SetDialogue(S:DialogFile*)",
    "SetEncounterProbability(S:FromArea*,S:ToArea*,I:Probability*)",
    "SetGlobal(S:Name*,S:Area*,I:Value*)",
    "SetGlobalRandom(S:Name*,S:Area*,I:Count*,I:Size*)",
    "SetGlobalRandomPlus(S:Name*,S:Area*,I:Count*,I:Size*,I:Plus*)",
    "SetGlobalTimer(S:Name*,S:Area*,I:Time*GTimes)",
    "SetGlobalTimerRandom(S:Name*,S:Area*,I:Min*GTimes,I:Max*GTimes)",
    "SetItemFlags(S:ItemName*,I:Flags*INVITEM,I:SetReset*BOOLEAN)",
    "SetMasterArea(S:Name*)",
    "SetToken(S:Token*,I:STRREF*)",
    "SetTokenGlobal(S:GLOBAL*,S:Area*,S:Token*)",
    "SetTokenObject(S:Token*,O:Object)",
    "SetWorldmap(S:Worldmap*)",
    "SG(S:Name*,I:Num*)",
    "SpellCastInnateRES(S:Spell*,O:Object*)",
    "SpellCastOnMeRES(S:Spell*,O:Caster*)",
    "SpellCastPriestRES(S:Spell*,O:Object*)",
    "SpellCastRES(S:Spell*,O:Object*)",
    "SpellNoDecRES(S:RES*,O:Target*)",
    "SpellPointNoDecRES(S:RES*,P:Target*)",
    "SpellPointRES(S:RES*,P:Target*)",
    "SpellRES(S:RES*,O:Target*)",
    "StartCutScene(S:CutScene*)",
    "StartCutSceneEx(S:CutScene*,I:evaluateConditions*BOOLEAN)",
    "StartDialog(S:DialogFile*,O:Target*)",
    "StartDialogInterrupt(S:DialogFile*,O:Target*)",
    "StartDialogNoName(S:DialogFile*,O:Target*)",
    "StartDialogOverride(S:DialogFile*,O:Target*)",
    "StartDialogOverrideInterrupt(S:DialogFile*,O:Target*)",
    "StartDialogue(S:DialogFile*,O:Target*)",
    "StartDialogueInterrupt(S:DialogFile*,O:Target*)",
    "StartMovie(S:ResRef*)",
    "StartStore(S:Store*,O:Target*)",
    "StaticPalette(S:Palette*,O:Object*)",
    "Switch(S:Global*,S:Area*)",
    "TakeItemListParty(S:ResRef*)",
    "TakeItemListPartyNum(S:ResRef*,I:Num*)",
    "TakeItemReplace(S:Give*,S:Take*,O:Object*)",
    "TakeObjectGoldGlobal(S:Name*,S:Area*,O:Object*)",
    "TakePartyItem(S:Item*)",
    "TakePartyItemAll(S:Item*)",
    "TakePartyItemNum(S:ResRef*,I:Num*)",
    "TakePartyItemRange(S:Item*)",
    "TextScreen(S:TextList*)",
    "TransformItem(S:OldItem*,S:NewItem*)",
    "Unusable(S:ResRef*)",
    "UseItem(S:Object*,O:Target*)",
    "UseItemPoint(S:Item,P:Point*,I:Ability*)",
    "XEquipItem(S:Item*,O:Object*,I:Slot*Slots,I:EquipUnEquip*XEQUIP)",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(signatures: &[&str]) -> Vocabulary {
        let mut calls = HashMap::new();
        for sig in signatures {
            if let Some((name, kinds)) = parse_signature(sig) {
                calls.insert(name, kinds);
            }
        }
        Vocabulary { calls }
    }

    #[test]
    fn parses_argument_positions_including_non_string_parameters() {
        // Positions count every parameter, since the scanner counts commas.
        let v = vocab(&["CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)"]);
        assert_eq!(v.arg_kind("CreateCreature", 0), ArgKind::Resource(CRE_TYPE));
        assert_eq!(v.arg_kind("CreateCreature", 1), ArgKind::Unknown);
        assert_eq!(v.arg_kind("CreateCreature", 2), ArgKind::Unknown);
        // Past the end, and an unlisted call.
        assert_eq!(v.arg_kind("CreateCreature", 9), ArgKind::Unknown);
        assert_eq!(v.arg_kind("NoSuchAction", 0), ArgKind::Unknown);
    }

    #[test]
    fn an_area_parameter_is_a_scope_only_when_the_call_also_names_a_variable() {
        // The Global family scopes a variable by area name.
        let v = vocab(&[
            "SetGlobal(S:Name*,S:Area*,I:Value*)",
            "MoveGlobal(S:Area*,O:Object*,P:Point*)",
        ]);
        assert_eq!(v.arg_kind("SetGlobal", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("SetGlobal", 1), ArgKind::Variable);
        // No variable parameter, so this one really does name an area.
        assert_eq!(v.arg_kind("MoveGlobal", 0), ArgKind::Resource(ARE_TYPE));
    }

    #[test]
    fn generic_resref_parameters_are_typed_per_call() {
        let v = vocab(&[
            "HasItem(S:ResRef*,O:Object*)",
            "AreaCheck(S:ResRef*)",
            "G(S:ResRef*,I:Num*)",
            "MysteryAction(S:ResRef*)",
        ]);
        assert_eq!(v.arg_kind("HasItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("AreaCheck", 0), ArgKind::Resource(ARE_TYPE));
        assert_eq!(v.arg_kind("G", 0), ArgKind::Variable);
        // Uncurated: no opinion, so it still reports.
        assert_eq!(v.arg_kind("MysteryAction", 0), ArgKind::Unknown);
    }

    #[test]
    fn could_name_rejects_only_confident_contradictions() {
        let v = vocab(&[
            "AreaCheck(S:ResRef*)",
            "SetGlobal(S:Name*,S:Area*,I:Value*)",
            "MysteryAction(S:ResRef*)",
        ]);
        // Typed and matching, typed and contradicting.
        assert!(v.could_name("AreaCheck", 0, ARE_TYPE));
        assert!(!v.could_name("AreaCheck", 0, BCS_TYPE));
        // Known not to be a resource at all.
        assert!(!v.could_name("SetGlobal", 1, ARE_TYPE));
        // Unknown always admits, whatever the query.
        assert!(v.could_name("MysteryAction", 0, ARE_TYPE));
        assert!(v.could_name("MysteryAction", 0, BCS_TYPE));
        assert!(v.could_name("NotInTheTable", 0, ITM_TYPE));
    }

    #[test]
    fn the_engine_vocabulary_types_the_calls_that_matter() {
        let v = engine();
        // Every signature in the table parsed.
        assert_eq!(v.len(), SIGNATURES.len());

        // The families the reverse search leans on, spot-checked end to end.
        assert_eq!(v.arg_kind("PartyHasItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("GiveItemCreate", 0), ArgKind::Resource(ITM_TYPE));
        // `S:Object*` on the item verbs is an item, whatever the name says.
        assert_eq!(v.arg_kind("GiveItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("AreaCheck", 0), ArgKind::Resource(ARE_TYPE));
        assert_eq!(v.arg_kind("CreateCreature", 0), ArgKind::Resource(CRE_TYPE));
        assert_eq!(v.arg_kind("ApplySpellRES", 0), ArgKind::Resource(SPL_TYPE));
        // A parameter name that is honest for once.
        assert_eq!(v.arg_kind("StartDialogue", 0), ArgKind::Resource(DLG_TYPE));

        // The variable families, which must never answer a resource search.
        assert_eq!(v.arg_kind("SetGlobal", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("SetGlobal", 1), ArgKind::Variable);
        assert_eq!(v.arg_kind("G", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("Dead", 0), ArgKind::Variable);
        // ...but a genuine area move is still an area.
        assert_eq!(v.arg_kind("MoveGlobal", 0), ArgKind::Resource(ARE_TYPE));
    }

    #[test]
    fn malformed_signatures_are_skipped_not_fatal() {
        assert!(parse_signature("NoParens").is_none());
        assert!(parse_signature("(S:Foo*)").is_none());
        // A signature with no parameters is valid and simply types nothing.
        let v = vocab(&["Continue()"]);
        assert_eq!(v.arg_kind("Continue", 0), ArgKind::Unknown);
        assert_eq!(v.len(), 1);
    }
}
