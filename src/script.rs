//! What the engine's script calls name: the vocabulary that types a compiled
//! script's calls and the quoted resrefs in a DLG's script blocks.
//!
//! The signatures are read from the install's own `ACTION.IDS` / `TRIGGER.IDS`
//! ([`from_signatures`]) rather than transcribed. The calls are implemented in
//! the closed-source executable, so a mod cannot add one — but decompiling
//! needs their numeric ids and full parameter lists, and the *value* tables
//! they point at (`SPELL.IDS`, `OBJECT.IDS`) are mod-extended, so the game must
//! be read in any case.
//!
//! What the IDS cannot supply is what a parameter *name* means. It says
//! `HasItem(S:ResRef*,…)` and `Global(S:Name*,S:Area*,…)`; only convention says
//! the first is an item and the second pair a variable and its scope. Some
//! names lie outright — `GiveItem(S:Object*,…)` takes an item. That curated
//! layer ([`ArgKind`], [`param_kind`], [`call_override`]) is the one thing kept
//! in code; uncertainty resolves to [`ArgKind::Unknown`], which admits rather
//! than rejects.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::formats::bcs;
use crate::formats::ids::IdsFile;

use crate::formats::are::ARE_TYPE;
use crate::formats::bcs::BCS_TYPE;
use crate::formats::cre::CRE_TYPE;
use crate::formats::dlg::DLG_TYPE;
use crate::formats::eff::EFF_TYPE;
use crate::formats::itm::ITM_TYPE;
use crate::formats::spl::SPL_TYPE;
use crate::formats::{BMP_TYPE, WAV_TYPE};

/// The `IDS` tables behind an object's seven enumerated attributes, in the
/// order the fields are stored.
pub const OBJECT_TABLES: [&str; 7] = [
    "EA", "GENERAL", "RACE", "CLASS", "SPECIFIC", "GENDER", "ALIGNMEN",
];

/// The table behind an object's five identifier fields, which chain into
/// `LastAttackerOf(LastHeardBy(Myself))`.
pub const IDENTIFIER_TABLE: &str = "OBJECT";

/// For each call, what each argument names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Vocabulary {
    /// Action id -> the signatures sharing it, in file order.
    actions: HashMap<i32, Vec<Signature>>,
    /// Trigger id -> the signatures sharing it, in file order.
    triggers: HashMap<i32, Vec<Signature>>,
    /// Lowercased name -> signature, for parsing a call by name back into a
    /// record. Names are unique within each table, so one entry each.
    action_by_name: HashMap<String, Signature>,
    trigger_by_name: HashMap<String, Signature>,
    /// UPPERCASE table name -> value -> symbol, for the tables the signatures
    /// name. These are the ones a mod really can extend, so they are read from
    /// the install rather than assumed.
    symbols: HashMap<String, HashMap<i64, String>>,
    /// The reverse: UPPERCASE table -> UPPERCASE symbol -> value, for parsing a
    /// symbol back to its number. Names are unique, so this is unambiguous even
    /// where a value carries several.
    values: HashMap<String, HashMap<String, i64>>,
}

impl Vocabulary {
    /// The call a compiled action names, with whether the reading is complete.
    /// `None` only when the id is unlisted.
    pub fn name_action(&self, action: &bcs::Action) -> Option<Naming<'_>> {
        let used = action_slots(action);
        let signature = select(self.actions.get(&action.id)?, &used)?;
        Some(Naming {
            signature,
            covered: signature.slots().covers(&used),
        })
    }

    /// The call a compiled trigger names, with whether the reading is complete.
    /// `None` only when the id is unlisted.
    pub fn name_trigger(&self, trigger: &bcs::Trigger) -> Option<Naming<'_>> {
        let used = trigger_slots(trigger);
        let signature = select(self.triggers.get(&trigger.id)?, &used)?;
        Some(Naming {
            signature,
            covered: signature.slots().covers(&used),
        })
    }

    /// The call an action names only when a signature fully accounts for it.
    pub fn action(&self, action: &bcs::Action) -> Option<&Signature> {
        self.name_action(action)
            .filter(|naming| naming.covered)
            .map(|naming| naming.signature)
    }

    /// The call a trigger names only when a signature fully accounts for it.
    pub fn trigger(&self, trigger: &bcs::Trigger) -> Option<&Signature> {
        self.name_trigger(trigger)
            .filter(|naming| naming.covered)
            .map(|naming| naming.signature)
    }

    /// What a compiled action's arguments name — resources and dialog strings —
    /// for a reverse search.
    pub fn action_args<'a>(&self, action: &'a bcs::Action) -> Vec<Arg<'a>> {
        self.name_action(action)
            .map(|naming| call_args(naming.signature, &action.ints, &action.strings))
            .unwrap_or_default()
    }

    /// What a compiled trigger's arguments name, for a reverse search.
    pub fn trigger_args<'a>(&self, trigger: &'a bcs::Trigger) -> Vec<Arg<'a>> {
        self.name_trigger(trigger)
            .map(|naming| call_args(naming.signature, &trigger.ints, &trigger.strings))
            .unwrap_or_default()
    }

    /// Every `IDS` table needed to print arguments symbolically: those the
    /// signatures name for their integers, plus the object tables. Returned
    /// owned so the caller can load each without holding a borrow.
    pub fn symbol_tables(&self) -> Vec<String> {
        let declared = self
            .actions
            .values()
            .chain(self.triggers.values())
            .flatten()
            .flat_map(|signature| signature.params.iter())
            .filter_map(|param| param.ids.as_deref());

        let mut tables: Vec<String> = declared
            .chain(OBJECT_TABLES)
            .chain([IDENTIFIER_TABLE])
            .map(|table| table.to_ascii_uppercase())
            .collect();
        tables.sort();
        tables.dedup();
        tables
    }

    /// Record a table's value -> symbol mapping, and its reverse for parsing.
    pub fn set_symbols(&mut self, table: &str, entries: HashMap<i64, String>) {
        let table = table.to_ascii_uppercase();
        let reverse = entries
            .iter()
            .map(|(&value, name)| (name.to_ascii_uppercase(), value))
            .collect();
        self.values.insert(table.clone(), reverse);
        self.symbols.insert(table, entries);
    }

    /// The symbol a table gives a value, if the table was loaded and lists it.
    pub fn symbol(&self, table: &str, value: i64) -> Option<&str> {
        let symbols = self.symbols.get(&table.to_ascii_uppercase())?;
        symbols
            .get(&value)
            .or_else(|| {
                // A table may write a 32-bit flag unsigned where the record
                // stores it signed: `STATE.IDS` has `0x80000000 STATE_CONFUSED`
                // for what a script holds as i32::MIN.
                let unsigned =
                    i64::from(u32::from_ne_bytes(i32::try_from(value).ok()?.to_ne_bytes()));
                symbols.get(&unsigned)
            })
            .map(String::as_str)
    }

    /// The value a table gives a symbol, for parsing BAF back to a record. The
    /// unsigned-flag case is folded to `i32` so it matches what the record
    /// stores (`STATE_CONFUSED` -> i32::MIN, not 0x80000000).
    pub fn value(&self, table: &str, symbol: &str) -> Option<i32> {
        let value = *self
            .values
            .get(&table.to_ascii_uppercase())?
            .get(&symbol.to_ascii_uppercase())?;
        i32::try_from(value)
            .ok()
            .or_else(|| u32::try_from(value).ok().map(|u| u as i32))
    }

    /// The action a name resolves to, for parsing a response line.
    pub fn action_named(&self, name: &str) -> Option<&Signature> {
        self.action_by_name.get(&name.to_ascii_lowercase())
    }

    /// The trigger a name resolves to, for parsing a condition line.
    pub fn trigger_named(&self, name: &str) -> Option<&Signature> {
        self.trigger_by_name.get(&name.to_ascii_lowercase())
    }

    /// How many distinct call ids are known; confirms the tables were read.
    #[cfg(test)]
    pub fn call_ids(&self) -> (usize, usize) {
        (self.actions.len(), self.triggers.len())
    }

    /// What argument `arg` of `call` names, for the tests that fix the curated
    /// typing. [`ArgKind::Unknown`] when the call is unlisted or the position is
    /// past its declared arguments.
    #[cfg(test)]
    fn arg_kind(&self, call: &str, arg: usize) -> ArgKind {
        let name = call.to_ascii_lowercase();
        self.action_by_name
            .get(&name)
            .or_else(|| self.trigger_by_name.get(&name))
            .map(arg_kinds)
            .and_then(|kinds| kinds.get(arg).copied())
            .unwrap_or(ArgKind::Unknown)
    }
}

/// One `ACTION.IDS` / `TRIGGER.IDS` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub id: i32,
    pub name: String,
    pub params: Vec<Param>,
}

impl Signature {
    /// The slots this call occupies. A scope parameter shares its predecessor's
    /// string slot: `Global(S:Name*,S:Area*,…)` stores one string, the scope
    /// prefixed to the name.
    pub fn slots(&self) -> Slots {
        let mut slots = Slots::default();
        for param in &self.params {
            match param.class {
                Class::Int => slots.ints += 1,
                Class::Object => slots.objects += 1,
                Class::Point => slots.point = true,
                Class::Action => {}
                Class::String => {
                    if !(slots.strings > 0 && is_scope_param(&self.name, &param.name)) {
                        slots.strings += 1;
                    }
                }
            }
        }
        slots
    }
}

/// One declared parameter of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub class: Class,
    pub name: String,
    /// The IDS table an integer argument draws its symbols from, when the
    /// signature names one (`I:Spell*Spell` -> `SPELL.IDS`).
    pub ids: Option<String>,
}

/// Which record slot class a parameter occupies. A call's arguments are not
/// stored in declaration order: each class fills its own slots in order, so
/// `CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)` puts its integer in
/// the first integer slot even though it is declared last.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    String,
    Int,
    Object,
    Point,
    /// Only `ActionOverride(O:Actor*,A:Action*)`, which the engine never
    /// stores: the actor goes in the call's own object slot 0.
    Action,
}

/// How many slots of each class a signature occupies. Slots fill from zero, so
/// a count is enough to say which are in use.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Slots {
    pub ints: usize,
    pub strings: usize,
    /// Object slots the call itself uses, ignoring the `ActionOverride` slot.
    pub objects: usize,
    pub point: bool,
}

impl Slots {
    /// Whether a signature covering `self` can account for a record occupying
    /// `used` — every slot the record fills must be one the call reads.
    fn covers(&self, used: &Slots) -> bool {
        self.ints >= used.ints
            && self.strings >= used.strings
            && self.objects >= used.objects
            && (self.point || !used.point)
    }
}

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

/// The signature chosen to name a record, and whether it accounts for every
/// slot the record fills. A false [`covered`](Naming::covered) means the record
/// carries data the call has no parameter for — junk the compiler left behind —
/// which the caller preserves rather than dropping.
#[derive(Clone, Copy, Debug)]
pub struct Naming<'a> {
    pub signature: &'a Signature,
    pub covered: bool,
}

/// Something a compiled call's argument names, tagged with the parameter it
/// came from (for the search path).
pub enum Arg<'a> {
    /// A resource of a given type. The resref usually borrows from the record,
    /// but a spell named by number is computed and so owns.
    Resource {
        kind: u16,
        resref: Cow<'a, str>,
        param: String,
    },
    /// A dialog string, by its `dialog.tlk` index.
    Strref { value: u32, param: String },
}

/// Read one table's calls from the install's own `ACTION.IDS` / `TRIGGER.IDS`.
///
/// Separate from [`from_signatures`] because a caller holding the manager can
/// only borrow one IDS table at a time.
pub fn signatures(table: &IdsFile) -> Vec<Signature> {
    table
        .entries()
        .iter()
        .filter_map(|entry| {
            let id = i32::try_from(entry.value).ok()?;
            parse_call(id, &entry.name)
        })
        .collect()
}

/// Assemble both tables into the vocabulary, indexing calls by id for
/// decompiling and by name for the DLG scan.
pub fn from_signatures(actions: Vec<Signature>, triggers: Vec<Signature>) -> Vocabulary {
    let mut vocabulary = Vocabulary::default();
    for (by_id, by_name, table) in [
        (
            &mut vocabulary.actions,
            &mut vocabulary.action_by_name,
            actions,
        ),
        (
            &mut vocabulary.triggers,
            &mut vocabulary.trigger_by_name,
            triggers,
        ),
    ] {
        for signature in table {
            let name = signature.name.to_ascii_lowercase();
            // First definition wins, as when a name appears in both tables.
            by_name.entry(name).or_insert_with(|| signature.clone());
            by_id.entry(signature.id).or_default().push(signature);
        }
    }
    vocabulary
}

/// Build a vocabulary from `IDS` lines, for tests in this module and in
/// [`bcs`](crate::formats::bcs).
#[cfg(test)]
pub fn from_lines(actions: &[&str], triggers: &[&str]) -> Vocabulary {
    fn read(lines: &[&str]) -> Vec<Signature> {
        lines
            .iter()
            .filter_map(|line| {
                let (id, text) = line.trim().split_once(char::is_whitespace)?;
                parse_call(id.parse().ok()?, text.trim())
            })
            .collect()
    }
    from_signatures(read(actions), read(triggers))
}

/// Whether a call's scope parameter shares its variable's string slot, stored
/// as a six-character prefix on the name.
///
/// IESDP says the only reliable way to know is to hardcode the calls that do
/// it, and it is right: the signatures give no clue. `Global(S:Name*,S:Area*)`
/// packs both into one slot while `GlobalTimerNotExpired(S:Name*,S:Area*)`
/// keeps them apart, and the two readings of id 446 disagree with each other.
/// This list was derived by compiling one probe per candidate call with WeiDU
/// and reading back which slots it filled.
pub fn is_scope_param(call: &str, param: &str) -> bool {
    const CONCATENATING: [&str; 16] = [
        "createcreatureatlocation",
        "createitemglobal",
        "global",
        "globalgt",
        "globallt",
        "incrementglobal",
        "incrementglobalonce",
        "movetosavedlocation",
        "movetosavedlocationn",
        "realsetglobaltimer",
        "setglobal",
        "setglobalrandom",
        "setglobalrandomplus",
        "setglobaltimer",
        "setglobaltimerrandom",
        "settokenglobal",
    ];
    is_scope_name(param) && CONCATENATING.iter().any(|c| call.eq_ignore_ascii_case(c))
}

/// The `.spl` resref a `SPELL.IDS` number names, by the filename convention: a
/// four-digit `T·NNN` where `T` selects the prefix — priest, wizard, innate,
/// class — and `NNN` is the file's numeric suffix (`2001` -> `SPWI001`). `None`
/// for a number outside the scheme.
pub fn spell_resref(number: i32) -> Option<String> {
    let prefix = match number / 1000 {
        1 => "SPPR",
        2 => "SPWI",
        3 => "SPIN",
        4 => "SPCL",
        _ => return None,
    };
    Some(format!("{prefix}{:03}", number % 1000))
}

/// Split `12 CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)`'s call text
/// into its name and typed parameters. `None` when the text is not a call.
fn parse_call(id: i32, text: &str) -> Option<Signature> {
    let (name, rest) = text.split_once('(')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let args = rest.strip_suffix(')').unwrap_or(rest);

    let params = args
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .filter_map(|p| {
            let p = p.trim();
            let (class, tail) = p.split_once(':')?;
            let class = match class.trim() {
                "S" => Class::String,
                "I" => Class::Int,
                "O" => Class::Object,
                "P" => Class::Point,
                "A" => Class::Action,
                _ => return None,
            };
            // `Name*Table`: the text after the asterisk names an IDS table, and
            // only integer parameters have one.
            let (name, ids) = tail.split_once('*').unwrap_or((tail, ""));
            let ids = (!ids.is_empty() && class == Class::Int).then(|| ids.to_string());
            Some(Param {
                class,
                name: name.to_string(),
                ids,
            })
        })
        .collect();

    Some(Signature {
        id,
        name: name.to_string(),
        params,
    })
}

/// Parameter names that hold a variable's scope rather than a resource.
fn is_scope_name(param: &str) -> bool {
    matches!(
        param.to_ascii_lowercase().as_str(),
        "area" | "area1" | "area2" | "scope"
    )
}

/// The slots a compiled action fills. Slot 0 of the objects is the
/// `ActionOverride` actor, never an argument.
fn action_slots(action: &bcs::Action) -> Slots {
    Slots {
        ints: highest(&action.ints, |&v| v != 0),
        strings: highest(&action.strings, |s| !s.is_empty()),
        objects: highest(&action.objects[1..], |o| !o.is_empty()),
        point: action.point != (0, 0),
    }
}

/// The slots a compiled trigger fills.
fn trigger_slots(trigger: &bcs::Trigger) -> Slots {
    Slots {
        ints: highest(&trigger.ints, |&v| v != 0),
        strings: highest(&trigger.strings, |s| !s.is_empty()),
        objects: usize::from(!trigger.object.is_empty()),
        point: false,
    }
}

/// Pick the signature to print for a record, by its string-slot count.
///
/// Only strings discriminate: an unread integer slot holds a zero either way
/// (`StartCutSceneEx("cut212a",FALSE)` takes the spare when offered), but a call
/// declaring a string the record never filled would name nothing. When none
/// matches, the record is padding (`ForceRandomEncounterEntry("","")`) and the
/// last reading wins — as on any tie, since `8 Dialogue`/`8 Dialog` are one call
/// under two names and WeiDU prints the later.
///
/// Chooses even when no reading fully fits; [`Naming::covered`] reports the
/// shortfall. Fitted to the colliding ids, checked against the whole install.
fn select<'a>(candidates: &'a [Signature], used: &Slots) -> Option<&'a Signature> {
    candidates
        .iter()
        .rev()
        .find(|signature| signature.slots().strings == used.strings)
        .or_else(|| candidates.iter().next_back())
}

/// What each argument of a call names. Positions count *every* parameter, since
/// the curated typing is indexed by declaration order.
fn arg_kinds(signature: &Signature) -> Vec<ArgKind> {
    // An `Area` alongside a variable name is that variable's *scope*, not an
    // area reference — the `Global`/`SetGlobal` shape. `MoveGlobal` has no name
    // parameter and really does name an area.
    let scoped = signature
        .params
        .iter()
        .any(|p| p.class == Class::String && is_variable_name(&p.name));

    signature
        .params
        .iter()
        .map(|param| {
            // Only a string argument can hold a resref.
            if param.class != Class::String {
                return ArgKind::Unknown;
            }
            if let Some(kind) = call_override(&signature.name, &param.name) {
                return kind;
            }
            if is_variable_name(&param.name) {
                return ArgKind::Variable;
            }
            if param.name.eq_ignore_ascii_case("area") && scoped {
                return ArgKind::Variable;
            }
            param_kind(&param.name)
        })
        .collect()
}

/// What a call's arguments name. Walks the signature the way the renderer does
/// — each class fills its own slots, and a scope parameter shares its
/// variable's string slot — but classifies the arguments rather than formatting
/// them. Only curated, concretely-typed arguments are returned; one whose type
/// is uncertain is left out rather than guessed.
fn call_args<'a>(signature: &Signature, ints: &[i32], strings: &'a [String]) -> Vec<Arg<'a>> {
    let kinds = arg_kinds(signature);
    let mut out = Vec::new();
    let (mut next_int, mut next_string) = (0, 0);
    let mut params = signature.params.iter().enumerate().peekable();
    let param_path = |param: &Param| format!("{}.{}", signature.name, param.name);

    while let Some((position, param)) = params.next() {
        match param.class {
            Class::Int => {
                let value = ints.get(next_int).copied().unwrap_or(0);
                next_int += 1;
                // A `SPELL.IDS` integer names a spell by number, which the
                // filename convention turns into a resref (`2001` -> `SPWI001`).
                if param
                    .ids
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case("spell"))
                {
                    if let Some(resref) = spell_resref(value) {
                        out.push(Arg::Resource {
                            kind: SPL_TYPE,
                            resref: Cow::Owned(resref),
                            param: param_path(param),
                        });
                    }
                } else if is_strref_param(&param.name) {
                    // The "no string" sentinel is stored as -1, which fails the
                    // conversion and so is skipped.
                    if let Ok(value) = u32::try_from(value) {
                        out.push(Arg::Strref {
                            value,
                            param: param_path(param),
                        });
                    }
                }
            }
            Class::String => {
                let stored = strings.get(next_string).map(String::as_str).unwrap_or("");
                next_string += 1;
                if let Some(ArgKind::Resource(kind)) = kinds.get(position)
                    && !stored.is_empty()
                {
                    out.push(Arg::Resource {
                        kind: *kind,
                        resref: Cow::Borrowed(stored),
                        param: param_path(param),
                    });
                }
                // A scope parameter shares this string slot, so skip it: the
                // next string parameter reads the next slot, not this one.
                if params.peek().is_some_and(|(_, next)| {
                    next.class == Class::String && is_scope_param(&signature.name, &next.name)
                }) {
                    params.next();
                }
            }
            // Objects hold death-variable names, not resrefs; points and the
            // `ActionOverride` slot name nothing.
            _ => {}
        }
    }
    out
}

/// Whether an integer parameter holds a `dialog.tlk` strref. Nearly all are
/// named `StrRef`; `AddJournalEntry(I:Entry*,…)` is the one that labels its
/// journal strref differently.
fn is_strref_param(name: &str) -> bool {
    name.eq_ignore_ascii_case("strref") || name.eq_ignore_ascii_case("entry")
}

/// One past the last slot holding something other than its default. Slots fill
/// from zero, so this is how many the record occupies.
fn highest<T>(slots: &[T], occupied: impl Fn(&T) -> bool) -> usize {
    slots
        .iter()
        .rposition(occupied)
        .map_or(0, |index| index + 1)
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
    const TABLE: [(&str, u16); 16] = [
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
        ("cutscene", BCS_TYPE), // StartCutScene(Ex) — a cutscene is a script
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(signatures: &[&str]) -> Vocabulary {
        let sigs: Vec<Signature> = signatures.iter().filter_map(|s| parse_call(0, s)).collect();
        from_signatures(sigs, Vec::new())
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
    fn the_vocabulary_types_the_calls_the_reverse_search_leans_on() {
        // The same curated calls the DLG scan relies on, now typed from IDS
        // lines rather than a transcribed table.
        let v = vocab(&[
            "PartyHasItem(S:Item*)",
            "GiveItemCreate(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
            "GiveItem(S:Object*,O:Target*)",
            "AreaCheck(S:ResRef*)",
            "CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)",
            "ApplySpellRES(S:RES*,O:Target)",
            "StartDialogue(S:DialogFile*,O:Target*)",
            "SetGlobal(S:Name*,S:Area*,I:Value*)",
            "G(S:ResRef*,I:Num*)",
            "Dead(S:Name*)",
            "MoveGlobal(S:Area*,O:Object*,P:Point*)",
            "ChangeAIScript(S:ScriptFile*,I:Type*SCRPLEV)",
            "StartCutScene(S:CutScene*)",
            "StartCutSceneEx(S:CutScene*,I:evaluateConditions*BOOLEAN)",
        ]);

        assert_eq!(v.arg_kind("PartyHasItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("GiveItemCreate", 0), ArgKind::Resource(ITM_TYPE));
        // `S:Object*` on the item verbs is an item, whatever the name says.
        assert_eq!(v.arg_kind("GiveItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("AreaCheck", 0), ArgKind::Resource(ARE_TYPE));
        assert_eq!(v.arg_kind("CreateCreature", 0), ArgKind::Resource(CRE_TYPE));
        assert_eq!(v.arg_kind("ApplySpellRES", 0), ArgKind::Resource(SPL_TYPE));
        // A parameter name that is honest for once.
        assert_eq!(v.arg_kind("StartDialogue", 0), ArgKind::Resource(DLG_TYPE));
        // A cutscene is a script, whichever name the parameter goes by.
        assert_eq!(v.arg_kind("ChangeAIScript", 0), ArgKind::Resource(BCS_TYPE));
        assert_eq!(v.arg_kind("StartCutScene", 0), ArgKind::Resource(BCS_TYPE));
        assert_eq!(
            v.arg_kind("StartCutSceneEx", 0),
            ArgKind::Resource(BCS_TYPE)
        );

        // The variable families, which must never answer a resource search.
        assert_eq!(v.arg_kind("SetGlobal", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("SetGlobal", 1), ArgKind::Variable);
        assert_eq!(v.arg_kind("G", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("Dead", 0), ArgKind::Variable);
        // ...but a genuine area move is still an area.
        assert_eq!(v.arg_kind("MoveGlobal", 0), ArgKind::Resource(ARE_TYPE));
    }

    /// The ids whose overloads actually collide in the install, with the
    /// reading WeiDU gives them.
    fn overloaded() -> Vocabulary {
        from_lines(
            &[
                "7 CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)",
                "7 CreateCreatureEffect(S:NewObject*,S:Effect*,P:Location*,I:Face*DIR)",
                "8 Dialogue(O:Object*)",
                "8 Dialog(O:Object*)",
                "120 StartCutScene(S:CutScene*)",
                "120 StartCutSceneEx(S:CutScene*,I:evaluateConditions*BOOLEAN)",
                "227 CreateCreatureObject(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
                "227 CreateCreatureObjectEffect(S:ResRef*,S:Effect*,O:Object*)",
                "357 ForceRandomEncounter(S:Area*)",
                "357 ForceRandomEncounterEntry(S:Area*,S:Entry*)",
            ],
            &[],
        )
    }

    fn action(id: i32, strings: [&str; 2], ints: [i32; 3], objects: usize) -> bcs::Action {
        let mut action = bcs::Action {
            id,
            ints,
            strings: [strings[0].to_string(), strings[1].to_string()],
            ..bcs::Action::default()
        };
        // Object slot 0 is the ActionOverride actor, so arguments start at 1.
        for slot in 1..=objects {
            if let Some(object) = action.objects.get_mut(slot) {
                object.fields[7] = 1;
            }
        }
        action
    }

    #[test]
    fn an_overload_must_account_for_every_string_the_record_holds() {
        let v = overloaded();
        // One string stored: the one-string reading, not the two-string one.
        let one = action(227, ["bdoffscr", ""], [0, 0, 0], 1);
        assert_eq!(
            v.action(&one).map(|s| &*s.name),
            Some("CreateCreatureObject")
        );
        // Both stored: only the two-string reading can hold them.
        let two = action(227, ["bdoffscr", "eff"], [0, 0, 0], 1);
        assert_eq!(
            v.action(&two).map(|s| &*s.name),
            Some("CreateCreatureObjectEffect")
        );
    }

    #[test]
    fn a_spare_integer_parameter_is_taken_but_a_spare_string_is_not() {
        let v = overloaded();
        // Same string count, so the extra integer decides nothing and the later
        // reading wins — as WeiDU prints `StartCutSceneEx("cut212a",FALSE)`.
        let cut = action(120, ["cut212a", ""], [0, 0, 0], 0);
        assert_eq!(v.action(&cut).map(|s| &*s.name), Some("StartCutSceneEx"));

        // A stored integer rules out the reading that has nowhere to put it.
        let created = action(7, ["MINOTAUR", ""], [4, 0, 0], 0);
        assert_eq!(v.action(&created).map(|s| &*s.name), Some("CreateCreature"));
    }

    #[test]
    fn an_empty_record_falls_back_to_the_last_reading() {
        let v = overloaded();
        // Nothing is stored, so no reading matches exactly and the record is
        // padding; WeiDU prints `ForceRandomEncounterEntry("","")`.
        let empty = action(357, ["", ""], [0, 0, 0], 0);
        assert_eq!(
            v.action(&empty).map(|s| &*s.name),
            Some("ForceRandomEncounterEntry")
        );
        // Two names for one call: the later one.
        let talk = action(8, ["", ""], [0, 0, 0], 1);
        assert_eq!(v.action(&talk).map(|s| &*s.name), Some("Dialog"));
    }

    #[test]
    fn an_unaccountable_slot_leaves_the_call_unnamed() {
        // BioWare left junk in integer slots no reading declares. Naming the
        // call would mean dropping it, so nothing is named.
        let v = from_lines(&["107 RandomFly()"], &[]);
        assert!(v.action(&action(107, ["", ""], [0, 0, 0], 0)).is_some());
        assert!(
            v.action(&action(107, ["", ""], [0, 1598211621, 0], 0))
                .is_none()
        );
    }

    #[test]
    fn only_the_curated_calls_pack_a_scope_into_the_variable_slot() {
        let v = from_lines(
            &[],
            &[
                "16399 Global(S:Name*,S:Area*,I:Value*)",
                "16449 GlobalTimerNotExpired(S:Name*,S:Area*)",
            ],
        );
        // Identical signatures, opposite storage: `Global` concatenates into
        // one slot, the timer keeps two.
        let packed = bcs::Trigger {
            id: 16399,
            strings: ["LOCALSPrep".to_string(), String::new()],
            ..bcs::Trigger::default()
        };
        assert_eq!(v.trigger(&packed).map(|s| &*s.name), Some("Global"));

        let split = bcs::Trigger {
            id: 16449,
            strings: ["Iaspell".to_string(), "LOCALS".to_string()],
            ..bcs::Trigger::default()
        };
        assert_eq!(
            v.trigger(&split).map(|s| &*s.name),
            Some("GlobalTimerNotExpired")
        );
    }

    #[test]
    fn each_parameter_class_fills_its_own_slots() {
        // The integer is declared last but occupies the first integer slot.
        let signature =
            parse_call(7, "CreateCreature(S:NewObject*,P:Location*,I:Face*DIR)").expect("parses");
        let slots = signature.slots();
        assert_eq!(slots.strings, 1);
        assert_eq!(slots.ints, 1);
        assert!(slots.point);
        assert_eq!(slots.objects, 0);
        assert_eq!(signature.params[2].ids.as_deref(), Some("DIR"));
    }

    #[test]
    fn the_runtime_tables_type_the_same_calls_as_the_static_one() {
        // Reading the IDS must not lose what the transcribed table knew.
        let v = from_lines(
            &[
                "15 GiveItem(S:Object*,O:Target*)",
                "30 SetGlobal(S:Name*,S:Area*,I:Value*)",
            ],
            &[],
        );
        assert_eq!(v.arg_kind("GiveItem", 0), ArgKind::Resource(ITM_TYPE));
        assert_eq!(v.arg_kind("SetGlobal", 0), ArgKind::Variable);
        assert_eq!(v.arg_kind("SetGlobal", 1), ArgKind::Variable);
        assert_eq!(v.call_ids(), (2, 0));
    }

    #[test]
    fn malformed_signatures_are_skipped_not_fatal() {
        assert!(parse_call(0, "NoParens").is_none());
        assert!(parse_call(0, "(S:Foo*)").is_none());
        // A signature with no parameters is valid and simply types nothing.
        let v = vocab(&["Continue()"]);
        assert_eq!(v.arg_kind("Continue", 0), ArgKind::Unknown);
        assert_eq!(v.call_ids(), (1, 0));
    }
}
