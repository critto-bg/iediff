//! BCS compiled-script parser: the raw layer.
//!
//! Despite the name, a compiled script is ASCII text — a stream of two-letter
//! markers, decimal integers and quoted strings. "Compiled" means only that
//! symbolic names became numeric `ACTION.IDS` / `TRIGGER.IDS` values:
//!
//! ```text
//! SC                          <- script
//!  CR                         <- condition-response block
//!   CO  TR ... TR  CO         <- condition: triggers, ANDed
//!   RS  RE <weight> AC ... AC RE  RS
//!  CR
//! SC
//! ```
//!
//! Markers open and close every construct and glue to adjacent tokens without
//! whitespace (`100AC`, `151OB`, `""OB`), so the reader is token- rather than
//! line-oriented.
//!
//! Records are fixed-arity: a call's arguments occupy declared slots and unused
//! ones hold a default. Which slots a call actually reads takes the IDS
//! vocabulary, which this layer deliberately does not have. It keeps every
//! stored value verbatim — including junk the engine left in slots no call
//! reads — so [`Script::encode`] reproduces its input byte for byte.
//!
//! Spec: `md/iesdp/file_formats/ie_formats/bcs.md`.

use std::collections::HashMap;
use std::fmt;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::rc::Rc;

use crate::script::{Arg, Class, IDENTIFIER_TABLE, OBJECT_TABLES, Signature, Vocabulary};
use crate::{ResrefUse, StrrefUse};

/// Resource type code for compiled scripts.
pub const BCS_TYPE: u16 = 0x03ef;

/// Integer fields in an object: seven enumerated attributes (EA, general, race,
/// class, specific, gender, alignment) then five `OBJECT.IDS` identifiers.
pub const OBJECT_FIELDS: usize = 12;

/// Width of the scope prefix glued onto a variable name. Area names are six
/// characters, and so are `GLOBAL` and `LOCALS`.
const SCOPE_WIDTH: usize = 6;

/// A compiled script: blocks evaluated in order, the first whose condition
/// holds taking effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    pub blocks: Vec<Block>,
    /// Whether re-encoding reproduced the bytes read, or `None` when the check
    /// was skipped. [`parse_verified`](Self::parse_verified) sets it so a view
    /// can flag a script this layer does not fully model; the search path leaves
    /// it unchecked.
    exact: Option<bool>,
    /// The install's call vocabulary, attached after parsing. Without it the
    /// views can only show numeric ids.
    vocabulary: Option<Rc<Vocabulary>>,
    /// End-of-line comments the manager resolves at decode time, since the
    /// writer cannot reach the TLK: a strref's text (and sound), and a named
    /// resource's display name. Keyed by the value the comment explains.
    comments: Comments,
}

impl Script {
    /// Read a compiled script. Whitespace between tokens is taken leniently,
    /// since only the writer needs to match the engine's exact layout.
    ///
    /// Does *not* verify the round trip: a whole-install reverse search parses
    /// thousands of scripts and never asks whether they re-encode, so the check
    /// is left to [`parse_verified`](Self::parse_verified).
    pub fn parse(data: &[u8]) -> Result<Script, BcsError> {
        let mut lex = Lexer::new(data);
        lex.marker("SC")?;
        let mut blocks = Vec::new();
        while lex.peek_marker() == Some(*b"CR") {
            blocks.push(parse_block(&mut lex)?);
        }
        lex.marker("SC")?;
        lex.skip_space();
        if lex.pos < data.len() {
            return Err(BcsError::Trailing { at: lex.pos });
        }

        Ok(Script {
            blocks,
            exact: None,
            vocabulary: None,
            comments: Comments::default(),
        })
    }

    /// Parse and check the round trip, so a view can warn when re-encoding would
    /// not reproduce the original bytes. Use for the single script a user asks
    /// to see, not the many a search sweeps.
    pub fn parse_verified(data: &[u8]) -> Result<Script, BcsError> {
        let mut script = Script::parse(data)?;
        script.exact = Some(script.encode() == data);
        Ok(script)
    }

    /// Build a script from blocks, as the BAF compiler does. Unverified: only a
    /// parse from bytes can check the round trip.
    pub fn from_blocks(blocks: Vec<Block>) -> Script {
        Script {
            blocks,
            exact: None,
            vocabulary: None,
            comments: Comments::default(),
        }
    }

    /// Attach the vocabulary that names this script's calls.
    pub fn set_vocabulary(&mut self, vocabulary: Rc<Vocabulary>) {
        self.vocabulary = Some(vocabulary);
    }

    /// Attach the resolved end-of-line comments.
    pub fn set_comments(&mut self, comments: Comments) {
        self.comments = comments;
    }

    /// Whether a verified script re-encodes to the bytes it was read from.
    /// `None` when the round trip was never checked (the search path). A false
    /// means the writer does not reproduce this file's layout — the parse is
    /// still usable, but it could not be written back safely.
    pub fn is_exact(&self) -> Option<bool> {
        self.exact
    }

    /// Visit everything in the script the manager can resolve into a comment: a
    /// strref, a string resref (a spell named by number shows its symbol
    /// instead, so is skipped), and a death-variable object name.
    pub fn for_each_reference(
        &self,
        vocabulary: &Vocabulary,
        mut strref: impl FnMut(u32),
        mut resource: impl FnMut(u16, &str),
        mut object: impl FnMut(&str),
    ) {
        for block in &self.blocks {
            for trigger in &block.triggers {
                for arg in vocabulary.trigger_args(trigger) {
                    reference_arg(arg, &mut strref, &mut resource);
                }
                death_variable(&trigger.object, &mut object);
            }
            for response in &block.responses {
                for action in &response.actions {
                    for arg in vocabulary.action_args(action) {
                        reference_arg(arg, &mut strref, &mut resource);
                    }
                    for obj in &action.objects {
                        death_variable(obj, &mut object);
                    }
                }
            }
        }
    }

    /// The resources this script names, for a reverse search. Needs the
    /// vocabulary to know which argument of each call names what; a compiled
    /// call is otherwise just numbers.
    pub fn references<'a>(&'a self, vocabulary: &Vocabulary) -> Vec<ResrefUse<'a>> {
        let mut uses = Vec::new();
        self.for_each_arg(vocabulary, |b, arg| {
            if let Arg::Resource {
                kind,
                resref,
                param,
            } = arg
            {
                uses.push(ResrefUse::new(kind, resref, format!("block[{b}].{param}")));
            }
        });
        uses
    }

    /// The dialog strings this script names, for a reverse search.
    pub fn strref_uses(&self, vocabulary: &Vocabulary) -> Vec<StrrefUse> {
        let mut uses = Vec::new();
        self.for_each_arg(vocabulary, |b, arg| {
            if let Arg::Strref { value, param } = arg {
                uses.push(StrrefUse::new(value, format!("block[{b}].{param}")));
            }
        });
        uses
    }

    /// Write the script back in the engine's layout. Exact for anything
    /// [`parse`](Self::parse) accepted, which is what lets a decompiler check
    /// its own work.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::from("SC\n");
        for block in &self.blocks {
            out.push_str("CR\nCO\n");
            for trigger in &block.triggers {
                encode_trigger(&mut out, trigger);
            }
            out.push_str("CO\nRS\n");
            for response in &block.responses {
                let _ = write!(out, "RE\n{}", response.weight);
                for action in &response.actions {
                    encode_action(&mut out, action);
                }
                out.push_str("RE\n");
            }
            out.push_str("RS\nCR\n");
        }
        out.push_str("SC\n");
        out.into_bytes()
    }

    /// Write the machine-readable view: one `key=value` line per stored field,
    /// including the slots no call reads.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "blocks={}", self.blocks.len())?;
        for (b, block) in self.blocks.iter().enumerate() {
            for (t, trigger) in block.triggers.iter().enumerate() {
                let key = format!("block.{b}.trigger.{t}");
                writeln!(out, "{key}.id={}", trigger.id)?;
                writeln!(out, "{key}.flags={}", trigger.flags)?;
                for (i, value) in trigger.ints.iter().enumerate() {
                    writeln!(out, "{key}.int.{i}={value}")?;
                }
                writeln!(out, "{key}.string.0={}", trigger.strings[0])?;
                writeln!(out, "{key}.string.1={}", trigger.strings[1])?;
                writeln!(out, "{key}.object={}", raw_object(&trigger.object))?;
            }
            for (r, response) in block.responses.iter().enumerate() {
                writeln!(out, "block.{b}.response.{r}.weight={}", response.weight)?;
                for (a, action) in response.actions.iter().enumerate() {
                    let key = format!("block.{b}.response.{r}.action.{a}");
                    writeln!(out, "{key}.id={}", action.id)?;
                    for (i, object) in action.objects.iter().enumerate() {
                        writeln!(out, "{key}.object.{i}={}", raw_object(object))?;
                    }
                    for (i, value) in action.ints.iter().enumerate() {
                        writeln!(out, "{key}.int.{i}={value}")?;
                    }
                    writeln!(out, "{key}.point={},{}", action.point.0, action.point.1)?;
                    writeln!(out, "{key}.string.0={}", action.strings[0])?;
                    writeln!(out, "{key}.string.1={}", action.strings[1])?;
                }
            }
        }
        Ok(())
    }

    /// Visit every argument of every call, tagged with its block index.
    fn for_each_arg<'a>(&'a self, vocabulary: &Vocabulary, mut visit: impl FnMut(usize, Arg<'a>)) {
        for (b, block) in self.blocks.iter().enumerate() {
            for trigger in &block.triggers {
                for arg in vocabulary.trigger_args(trigger) {
                    visit(b, arg);
                }
            }
            for response in &block.responses {
                for action in &response.actions {
                    for arg in vocabulary.action_args(action) {
                        visit(b, arg);
                    }
                }
            }
        }
    }

    /// Calls the vocabulary could not account for, and so left numeric.
    ///
    /// Distinct from [`is_exact`](Self::is_exact), which asks whether the
    /// *bytes* survive a round trip. A script can re-encode perfectly and still
    /// count here: the record is stored faithfully, but no signature explains
    /// the slots it fills, so naming it would mean dropping something.
    fn unnamed(&self) -> usize {
        let Some(vocabulary) = self.vocabulary.as_deref() else {
            return 0;
        };
        let triggers = self
            .blocks
            .iter()
            .flat_map(|block| block.triggers.iter())
            .filter(|trigger| vocabulary.trigger(trigger).is_none())
            .count();
        let actions = self
            .blocks
            .iter()
            .flat_map(|block| block.responses.iter())
            .flat_map(|response| response.actions.iter())
            .filter(|action| vocabulary.action(action).is_none())
            .count();
        triggers + actions
    }
}

/// The decompiled BAF view: `IF` triggers `THEN` responses `END`, the text
/// WeiDU compiles back to BCS. Calls stay numeric only when the vocabulary is
/// absent (no game to read `ACTION.IDS`), which does not round-trip but keeps
/// the structure legible.
impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_exact() == Some(false) {
            writeln!(f, "// warning: does not re-encode to the bytes read")?;
        }
        if let n @ 1.. = self.unnamed() {
            writeln!(
                f,
                "// note: {n} call(s) carry data no signature explains, kept in comments"
            )?;
        }

        let view = View {
            vocab: self.vocabulary.as_deref(),
            comments: &self.comments,
        };
        for block in &self.blocks {
            writeln!(f, "IF")?;
            write_condition(f, &view, &block.triggers)?;
            writeln!(f, "THEN")?;
            for response in &block.responses {
                writeln!(f, "  RESPONSE #{}", response.weight)?;
                for action in &response.actions {
                    write_call(f, "    ", render_action(&view, action))?;
                }
            }
            writeln!(f, "END")?;
            writeln!(f)?;
        }

        Ok(())
    }
}

/// A creature or thing a call acts on, matched by attributes, by a chain of
/// `OBJECT.IDS` identifiers, or by death-variable name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Object {
    pub fields: [i32; OBJECT_FIELDS],
    pub name: String,
}

impl Object {
    /// Whether every slot holds its default, i.e. the object was never set.
    pub fn is_empty(&self) -> bool {
        self.fields.iter().all(|&f| f == 0) && self.name.is_empty()
    }
}

/// One condition of a block: a `TRIGGER.IDS` call with its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trigger {
    pub id: i32,
    /// The three integer parameters, in declaration order. `flags` is stored
    /// between the first and second. IESDP calls the third slot unknown, but
    /// `NearLocation(O:Object*,I:PointX*,I:PointY*,I:Range*)` fills all three.
    pub ints: [i32; 3],
    /// Bit 0 negates the trigger.
    pub flags: i32,
    pub strings: [String; 2],
    pub object: Object,
}

impl Trigger {
    /// Whether the trigger succeeds when its condition is *false*.
    pub fn negated(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// One step of a response: an `ACTION.IDS` call with its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Action {
    pub id: i32,
    /// Slot 0 holds the `ActionOverride` actor — the engine stores no nested
    /// action, only the actor the call is redirected to. Slots 1 and 2 are the
    /// call's own object parameters.
    pub objects: [Object; 3],
    /// The three integer parameters. `ints[0]` is stored before the point, the
    /// other two after it.
    pub ints: [i32; 3],
    pub point: (i32, i32),
    pub strings: [String; 2],
}

/// One alternative of a block's response set, chosen with probability
/// `weight / sum(weights)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub weight: i32,
    pub actions: Vec<Action>,
}

/// "If every trigger holds, run one of these responses."
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub triggers: Vec<Trigger>,
    pub responses: Vec<Response>,
}

/// The resolved comments a decompiled view shows after its calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comments {
    /// strref -> the text after `//` (`~Trap Sprung~ [AMB_D05C]`).
    pub strrefs: HashMap<u32, String>,
    /// (kind, UPPERCASE resref) -> the display name (`~Orog~`).
    pub resources: HashMap<(u16, String), String>,
    /// UPPERCASE death-variable name -> the creature's display name. The death
    /// variable doubles as the creature's resref, so `Dead("OHDHAMMS")` resolves
    /// to `~Garfunkle~`.
    pub objects: HashMap<String, String>,
}

/// What the renderer needs beyond the record: the vocabulary that names calls,
/// and the comments the manager resolved from the TLK.
struct View<'a> {
    vocab: Option<&'a Vocabulary>,
    comments: &'a Comments,
}

/// The BAF text for one call, plus the end-of-line comment (everything after
/// `//`): a resolved strref or resource name, or junk the signature cannot
/// explain — WeiDU drops the latter, this keeps it.
struct Call {
    text: String,
    comment: Option<String>,
}

/// A token reader over the raw bytes. Every accessor is bounds-checked and
/// reports the byte offset it failed at.
struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(data: &'a [u8]) -> Lexer<'a> {
        Lexer { data, pos: 0 }
    }

    fn skip_space(&mut self) {
        while self
            .data
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    /// The next two-letter marker, without consuming it. Drives the choice
    /// between "another item" and "the closing marker" at every level.
    fn peek_marker(&self) -> Option<[u8; 2]> {
        let mut at = self.pos;
        while self.data.get(at).is_some_and(|b| b.is_ascii_whitespace()) {
            at += 1;
        }
        let rest = self.data.get(at..)?;
        match rest {
            [a, b, ..] if a.is_ascii_uppercase() && b.is_ascii_uppercase() => Some([*a, *b]),
            _ => None,
        }
    }

    fn marker(&mut self, want: &'static str) -> Result<(), BcsError> {
        self.skip_space();
        let at = self.pos;
        let found = self.data.get(at..).and_then(|r| r.get(..want.len()));
        if found == Some(want.as_bytes()) {
            self.pos = at + want.len();
            Ok(())
        } else {
            Err(BcsError::Expected { want, at })
        }
    }

    fn int(&mut self) -> Result<i32, BcsError> {
        self.skip_space();
        let start = self.pos;
        if self.data.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        while self.data.get(self.pos).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let text = self.data.get(start..self.pos).unwrap_or_default();
        std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.parse::<i32>().ok())
            .ok_or_else(|| BcsError::BadNumber {
                text: String::from_utf8_lossy(text).into_owned(),
                at: start,
            })
    }

    /// A double-quoted string. The format has no escape, so the first closing
    /// quote ends it and the body is taken verbatim — a name may be a single
    /// space, and dropping it would change the script.
    fn string(&mut self) -> Result<String, BcsError> {
        self.skip_space();
        let at = self.pos;
        if self.data.get(at) != Some(&b'"') {
            return Err(BcsError::Expected { want: "\"", at });
        }
        let body = at + 1;
        let len = self
            .data
            .get(body..)
            .and_then(|rest| rest.iter().position(|&b| b == b'"'))
            .ok_or(BcsError::UnterminatedString { at })?;
        let text = self.data.get(body..body + len).unwrap_or_default();
        self.pos = body + len + 1;
        Ok(String::from_utf8_lossy(text).into_owned())
    }
}

/// Anything that can go wrong while reading a compiled script.
#[derive(Debug)]
pub enum BcsError {
    /// A marker, quote or number was required here and something else was found.
    Expected { want: &'static str, at: usize },
    /// Digits were expected but the run did not parse as a 32-bit integer.
    BadNumber { text: String, at: usize },
    /// A quoted string ran to the end of the file.
    UnterminatedString { at: usize },
    /// Bytes follow the closing `SC`.
    Trailing { at: usize },
}

impl fmt::Display for BcsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BcsError::Expected { want, at } => {
                write!(f, "malformed BCS: expected {want:?} at byte {at}")
            }
            BcsError::BadNumber { text, at } => {
                write!(f, "malformed BCS: {text:?} at byte {at} is not an integer")
            }
            BcsError::UnterminatedString { at } => {
                write!(f, "malformed BCS: unterminated string at byte {at}")
            }
            BcsError::Trailing { at } => {
                write!(f, "malformed BCS: trailing bytes at {at}")
            }
        }
    }
}

impl std::error::Error for BcsError {}

/// Route one argument to the strref or resource callback. A borrowed resref is a
/// string argument; an owned one is a spell named by number, which shows its
/// symbol rather than a name, so it is skipped.
fn reference_arg(arg: Arg<'_>, strref: &mut impl FnMut(u32), resource: &mut impl FnMut(u16, &str)) {
    match arg {
        Arg::Strref { value, .. } => strref(value),
        Arg::Resource { kind, resref, .. } => {
            if let std::borrow::Cow::Borrowed(name) = resref {
                resource(kind, name);
            }
        }
    }
}

/// The death-variable name an object carries, if it is one — a plain name with
/// no attribute or identifier fields, as `Dead("OHDHAMMS")` stores.
fn death_variable(object: &Object, visit: &mut impl FnMut(&str)) {
    if !object.name.is_empty() {
        visit(&object.name);
    }
}

/// An object's twelve fields and name on one line, for the raw view.
fn raw_object(object: &Object) -> String {
    let fields: Vec<String> = object.fields.iter().map(|f| f.to_string()).collect();
    format!("{} \"{}\"", fields.join(","), object.name)
}

/// Write a call line at `indent`, appending its comment if any.
fn write_call(f: &mut fmt::Formatter<'_>, indent: &str, call: Call) -> fmt::Result {
    match call.comment {
        Some(comment) => writeln!(f, "{indent}{}  // {comment}", call.text),
        None => writeln!(f, "{indent}{}", call.text),
    }
}

/// The comment a call earns: the first of its arguments the manager could
/// resolve — a strref, a named resource, or a death-variable creature. One per
/// line, matching WeiDU.
fn annotate(view: &View, args: Vec<Arg<'_>>, objects: &[&Object]) -> Option<String> {
    let comments = view.comments;
    for arg in args {
        match arg {
            Arg::Strref { value, .. } => {
                if let Some(text) = comments.strrefs.get(&value) {
                    return Some(text.clone());
                }
            }
            // Borrowed only: a string resref, not a spell named by number.
            Arg::Resource { kind, resref, .. } => {
                if let std::borrow::Cow::Borrowed(name) = resref
                    && let Some(text) = comments.resources.get(&(kind, name.to_ascii_uppercase()))
                {
                    return Some(text.clone());
                }
            }
        }
    }
    for object in objects {
        if !object.name.is_empty()
            && let Some(text) = comments.objects.get(&object.name.to_ascii_uppercase())
        {
            return Some(text.clone());
        }
    }
    None
}

/// Trigger ids WeiDU treats specially in BAF. `OR` groups the triggers that
/// follow it; `NextTriggerObject` redirects the next trigger to an object and
/// prints as `TriggerOverride`. Both are recognised by name, so a game whose
/// `TRIGGER.IDS` renumbered them would still resolve.
fn is_named(vocabulary: Option<&Vocabulary>, trigger: &Trigger, name: &str) -> bool {
    vocabulary
        .and_then(|v| v.name_trigger(trigger))
        .is_some_and(|naming| naming.signature.name.eq_ignore_ascii_case(name))
}

/// Write a block's condition: each trigger on its own line, `OR(n)` indenting
/// the `n` that follow, and `NextTriggerObject` folded into `TriggerOverride`.
fn write_condition(f: &mut fmt::Formatter<'_>, view: &View, triggers: &[Trigger]) -> fmt::Result {
    let mut i = 0;
    let mut grouped = 0usize;
    while let Some(trigger) = triggers.get(i) {
        let indent = if grouped > 0 { "    " } else { "  " };

        // `NextTriggerObject(obj)` carries no condition of its own; it redirects
        // the following trigger, which supplies the negation.
        if is_named(view.vocab, trigger, "NextTriggerObject")
            && let Some(inner) = triggers.get(i + 1)
        {
            let object = show_object(view.vocab, &trigger.object);
            let call = render_trigger(view, inner);
            let negate = if inner.negated() { "!" } else { "" };
            let folded = Call {
                text: format!("{negate}TriggerOverride({object},{})", call.text),
                comment: call.comment,
            };
            write_call(f, indent, folded)?;
            i += 2;
            grouped = grouped.saturating_sub(1);
            continue;
        }

        let call = render_trigger(view, trigger);
        let negate = if trigger.negated() { "!" } else { "" };
        write_call(
            f,
            indent,
            Call {
                text: format!("{negate}{}", call.text),
                comment: call.comment,
            },
        )?;

        if is_named(view.vocab, trigger, "OR") {
            // `OR(n)` sits at the condition's own indent; the n that follow are
            // its alternatives, indented under it.
            grouped = usize::try_from(trigger.ints[0]).unwrap_or(0);
        } else {
            grouped = grouped.saturating_sub(1);
        }
        i += 1;
    }
    Ok(())
}

/// Render a trigger as `Name(args)`, without its leading negation (the caller
/// places it, so `TriggerOverride` can hoist it). Numeric when unnamed.
fn render_trigger(view: &View, trigger: &Trigger) -> Call {
    if let Some(vocabulary) = view.vocab
        && let Some(naming) = vocabulary.name_trigger(trigger)
    {
        let args = arguments(
            vocabulary,
            naming.signature,
            &trigger.ints,
            &trigger.strings,
            std::slice::from_ref(&trigger.object),
            None,
        );
        let comment = if naming.covered {
            annotate(view, vocabulary.trigger_args(trigger), &[&trigger.object])
        } else {
            Some(format!(
                "unmodelled: {}",
                trigger_junk(naming.signature, trigger)
            ))
        };
        return Call {
            text: format!("{}({})", naming.signature.name, args.join(",")),
            comment,
        };
    }

    let mut args = Vec::new();
    push_ints(&mut args, &trigger.ints);
    push_strings(&mut args, &trigger.strings);
    push_object(&mut args, view.vocab, &trigger.object);
    Call {
        text: format!("trigger {}{}", trigger.id, joined(&args)),
        comment: None,
    }
}

/// Render an action as `Name(args)`. An occupied object slot 0 is the
/// `ActionOverride` actor, which wraps the call rather than being an argument.
fn render_action(view: &View, action: &Action) -> Call {
    let named = match (view.vocab, view.vocab.and_then(|v| v.name_action(action))) {
        (Some(vocabulary), Some(naming)) => {
            let args = arguments(
                vocabulary,
                naming.signature,
                &action.ints,
                &action.strings,
                action.objects.get(1..).unwrap_or_default(),
                Some(action.point),
            );
            let comment = if naming.covered {
                let objects: Vec<&Object> =
                    action.objects.get(1..).unwrap_or_default().iter().collect();
                annotate(view, vocabulary.action_args(action), &objects)
            } else {
                Some(format!(
                    "unmodelled: {}",
                    action_junk(naming.signature, action)
                ))
            };
            Some(Call {
                text: format!("{}({})", naming.signature.name, args.join(",")),
                comment,
            })
        }
        _ => None,
    };

    let call = named.unwrap_or_else(|| {
        let mut args = Vec::new();
        push_ints(&mut args, &action.ints);
        if action.point != (0, 0) {
            args.push(format!("[{}.{}]", action.point.0, action.point.1));
        }
        push_strings(&mut args, &action.strings);
        for object in action.objects.get(1..).unwrap_or_default() {
            push_object(&mut args, view.vocab, object);
        }
        Call {
            text: format!("action {}{}", action.id, joined(&args)),
            comment: None,
        }
    });

    match action.objects.first().filter(|o| !o.is_empty()) {
        Some(actor) => Call {
            text: format!(
                "ActionOverride({},{})",
                show_object(view.vocab, actor),
                call.text
            ),
            comment: call.comment,
        },
        None => call,
    }
}

/// The record slots a trigger fills that its signature has no parameter for.
fn trigger_junk(signature: &Signature, trigger: &Trigger) -> String {
    let declared = signature.slots();
    let mut parts = Vec::new();
    leftover_ints(&mut parts, declared.ints, &trigger.ints);
    leftover_strings(&mut parts, declared.strings, &trigger.strings);
    parts.join(" ")
}

/// The record slots an action fills that its signature has no parameter for.
fn action_junk(signature: &Signature, action: &Action) -> String {
    let declared = signature.slots();
    let mut parts = Vec::new();
    leftover_ints(&mut parts, declared.ints, &action.ints);
    leftover_strings(&mut parts, declared.strings, &action.strings);
    if !declared.point && action.point != (0, 0) {
        parts.push(format!("point=[{}.{}]", action.point.0, action.point.1));
    }
    parts.join(" ")
}

fn leftover_ints(parts: &mut Vec<String>, declared: usize, ints: &[i32]) {
    for (i, &value) in ints.iter().enumerate() {
        if i >= declared && value != 0 {
            parts.push(format!("int{i}={value}"));
        }
    }
}

fn leftover_strings(parts: &mut Vec<String>, declared: usize, strings: &[String]) {
    for (i, text) in strings.iter().enumerate() {
        if i >= declared && !text.is_empty() {
            parts.push(format!("string{i}={text:?}"));
        }
    }
}

/// Lay a record's slots out in the call's declared argument order. Each class
/// fills its own slots in order, so the two sequences differ.
fn arguments(
    vocabulary: &Vocabulary,
    signature: &Signature,
    ints: &[i32],
    strings: &[String],
    objects: &[Object],
    point: Option<(i32, i32)>,
) -> Vec<String> {
    let (mut next_int, mut next_string, mut next_object) = (0, 0, 0);
    let mut args = Vec::new();
    let mut params = signature.params.iter().peekable();

    while let Some(param) = params.next() {
        match param.class {
            Class::Int => {
                let value = ints.get(next_int).copied().unwrap_or(0);
                next_int += 1;
                // `I:Spell*Spell` draws its symbols from SPELL.IDS, which mods
                // extend; an unlisted value stays numeric.
                let symbol = param
                    .ids
                    .as_deref()
                    .and_then(|table| vocabulary.symbol(table, i64::from(value)));
                args.push(symbol.map_or_else(|| value.to_string(), str::to_string));
            }
            Class::Object => {
                let empty = Object::default();
                let object = objects.get(next_object).unwrap_or(&empty);
                args.push(show_object(Some(vocabulary), object));
                next_object += 1;
            }
            Class::Point => {
                let (x, y) = point.unwrap_or((0, 0));
                args.push(format!("[{x}.{y}]"));
            }
            // The engine stores no nested action; the caller prints the actor.
            Class::Action => {}
            Class::String => {
                let stored = strings.get(next_string).cloned().unwrap_or_default();
                next_string += 1;
                // A scope shares this slot as a fixed-width prefix (see
                // `is_scope_param`); split it back into name and scope.
                let scoped = params.peek().is_some_and(|next| {
                    next.class == Class::String
                        && crate::script::is_scope_param(&signature.name, &next.name)
                });
                if scoped {
                    params.next();
                    let (scope, name) = stored.split_at(stored.len().min(SCOPE_WIDTH));
                    args.push(format!("{name:?}"));
                    args.push(format!("{scope:?}"));
                } else {
                    args.push(format!("{stored:?}"));
                }
            }
        }
    }
    args
}

/// An object as WeiDU writes it: the identifiers chain outwards around a base
/// of the death-variable name or the attribute bracket.
///
/// The engine stores the chain innermost first, so `1 20 10` — Myself,
/// LastHeardBy, LastAttackerOf — is `LastAttackerOf(LastHeardBy(Myself))`.
fn show_object(vocabulary: Option<&Vocabulary>, object: &Object) -> String {
    let symbol = |table: &str, value: i32| {
        vocabulary
            .and_then(|v| v.symbol(table, i64::from(value)))
            .map(str::to_string)
    };

    let (attributes, identifiers) = object.fields.split_at(OBJECT_TABLES.len());

    // The base an identifier chain wraps: a name, else the attributes.
    let mut base = if !object.name.is_empty() {
        Some(format!("{:?}", object.name))
    } else {
        attributes.iter().rposition(|&f| f != 0).map(|last| {
            let shown: Vec<String> = attributes
                .iter()
                .take(last + 1)
                .enumerate()
                .map(|(i, &field)| {
                    // A zero attribute is a wildcard, printed as such even where
                    // its table happens to name zero.
                    match OBJECT_TABLES.get(i).filter(|_| field != 0) {
                        Some(table) => symbol(table, field).unwrap_or_else(|| field.to_string()),
                        None => field.to_string(),
                    }
                })
                .collect();
            format!("[{}]", shown.join("."))
        })
    };

    for &identifier in identifiers.iter().filter(|&&f| f != 0) {
        let name =
            symbol(IDENTIFIER_TABLE, identifier).unwrap_or_else(|| format!("object {identifier}"));
        base = Some(match base {
            Some(inner) => format!("{name}({inner})"),
            None => name,
        });
    }

    base.unwrap_or_else(|| "[ANYONE]".to_string())
}

/// Join collected arguments onto an unnamed call line.
fn joined(args: &[String]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!("  {}", args.join(" "))
    }
}

fn push_ints(args: &mut Vec<String>, ints: &[i32]) {
    for value in ints.iter().filter(|&&v| v != 0) {
        args.push(value.to_string());
    }
}

fn push_strings(args: &mut Vec<String>, strings: &[String]) {
    for text in strings.iter().filter(|s| !s.is_empty()) {
        args.push(format!("{text:?}"));
    }
}

fn push_object(args: &mut Vec<String>, vocabulary: Option<&Vocabulary>, object: &Object) {
    if !object.is_empty() {
        args.push(show_object(vocabulary, object));
    }
}

fn parse_block(lex: &mut Lexer<'_>) -> Result<Block, BcsError> {
    lex.marker("CR")?;
    lex.marker("CO")?;

    let mut triggers = Vec::new();
    while lex.peek_marker() == Some(*b"TR") {
        triggers.push(parse_trigger(lex)?);
    }
    lex.marker("CO")?;

    lex.marker("RS")?;
    let mut responses = Vec::new();
    while lex.peek_marker() == Some(*b"RE") {
        responses.push(parse_response(lex)?);
    }
    lex.marker("RS")?;
    lex.marker("CR")?;

    Ok(Block {
        triggers,
        responses,
    })
}

fn parse_trigger(lex: &mut Lexer<'_>) -> Result<Trigger, BcsError> {
    lex.marker("TR")?;
    let id = lex.int()?;
    let int0 = lex.int()?;
    let flags = lex.int()?;
    let int1 = lex.int()?;
    let int2 = lex.int()?;
    let string0 = lex.string()?;
    let string1 = lex.string()?;
    let object = parse_object(lex)?;
    lex.marker("TR")?;

    Ok(Trigger {
        id,
        ints: [int0, int1, int2],
        flags,
        strings: [string0, string1],
        object,
    })
}

fn parse_response(lex: &mut Lexer<'_>) -> Result<Response, BcsError> {
    lex.marker("RE")?;
    let weight = lex.int()?;
    let mut actions = Vec::new();
    while lex.peek_marker() == Some(*b"AC") {
        actions.push(parse_action(lex)?);
    }
    lex.marker("RE")?;

    Ok(Response { weight, actions })
}

fn parse_action(lex: &mut Lexer<'_>) -> Result<Action, BcsError> {
    lex.marker("AC")?;
    let id = lex.int()?;
    let objects = [parse_object(lex)?, parse_object(lex)?, parse_object(lex)?];
    let int0 = lex.int()?;
    let x = lex.int()?;
    let y = lex.int()?;
    let int1 = lex.int()?;
    let int2 = lex.int()?;
    let string0 = lex.string()?;
    let string1 = lex.string()?;
    lex.marker("AC")?;

    Ok(Action {
        id,
        objects,
        ints: [int0, int1, int2],
        point: (x, y),
        strings: [string0, string1],
    })
}

fn parse_object(lex: &mut Lexer<'_>) -> Result<Object, BcsError> {
    lex.marker("OB")?;
    let mut fields = [0i32; OBJECT_FIELDS];
    for field in fields.iter_mut() {
        *field = lex.int()?;
    }
    let name = lex.string()?;
    lex.marker("OB")?;

    Ok(Object { fields, name })
}

/// The engine writes a space before a trigger's strings but not before an
/// action's, and none before a closing `OB`. These are reproduced exactly:
/// scripts are compared byte for byte, so layout is part of the format.
fn encode_trigger(out: &mut String, trigger: &Trigger) {
    let _ = write!(
        out,
        "TR\n{} {} {} {} {} \"{}\" \"{}\" ",
        trigger.id,
        trigger.ints[0],
        trigger.flags,
        trigger.ints[1],
        trigger.ints[2],
        trigger.strings[0],
        trigger.strings[1],
    );
    encode_object(out, &trigger.object);
    out.push_str("TR\n");
}

fn encode_action(out: &mut String, action: &Action) {
    let _ = write!(out, "AC\n{}", action.id);
    for object in &action.objects {
        encode_object(out, object);
    }
    let _ = writeln!(
        out,
        "{} {} {} {} {}\"{}\" \"{}\" AC",
        action.ints[0],
        action.point.0,
        action.point.1,
        action.ints[1],
        action.ints[2],
        action.strings[0],
        action.strings[1],
    );
}

fn encode_object(out: &mut String, object: &Object) {
    out.push_str("OB\n");
    for field in &object.fields {
        let _ = write!(out, "{field} ");
    }
    let _ = writeln!(out, "\"{}\"OB", object.name);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trap trigger from the install: one block, one trigger, two actions,
    /// covering markers glued to numbers, an identifier chain and a strref
    /// argument.
    const TRAP: &[u8] = b"SC\nCR\nCO\nTR\n\
        76 0 0 0 0 \"\" \"\" OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\nTR\nCO\nRS\nRE\n\
        100AC\n151OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\n\
        OB\n0 0 0 0 0 0 0 17 0 0 0 0 \"\"OB\n\
        OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\n14381 0 0 0 0\"\" \"\" AC\n\
        AC\n113OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\n\
        OB\n0 0 0 0 0 0 0 17 0 0 0 0 \"\"OB\n\
        OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\n2001 0 0 0 0\"\" \"\" AC\n\
        RE\nRS\nCR\nSC\n";

    #[test]
    fn reads_the_structure_of_a_script() {
        let script = Script::parse(TRAP).expect("parses");
        assert_eq!(script.blocks.len(), 1);

        let block = &script.blocks[0];
        assert_eq!(block.triggers.len(), 1);
        assert_eq!(block.triggers[0].id, 76);
        assert!(!block.triggers[0].negated());
        assert!(block.triggers[0].object.is_empty());

        assert_eq!(block.responses.len(), 1);
        assert_eq!(block.responses[0].weight, 100);
        let actions = &block.responses[0].actions;
        assert_eq!(actions.len(), 2);
        // The first integer parameter, here a strref.
        assert_eq!(actions[0].id, 151);
        assert_eq!(actions[0].ints[0], 14381);
        // Object slot 1 is the call's own target; slot 0 stays empty, so the
        // call was not redirected.
        assert_eq!(actions[0].objects[1].fields[7], 17);
        assert!(actions[0].objects[0].is_empty());
        assert_eq!(actions[1].id, 113);
        assert_eq!(actions[1].ints[0], 2001);
    }

    #[test]
    fn encodes_back_to_the_original_bytes() {
        let script = Script::parse(TRAP).expect("parses");
        assert_eq!(script.encode(), TRAP);
    }

    #[test]
    fn keeps_junk_in_slots_no_call_reads() {
        // BioWare's compiler left uninitialised values in the trailing integer
        // slots of some actions. A semantic reader drops them and changes the
        // file; this layer stores them, so they survive.
        let junk = TRAP.to_vec();
        let junk = String::from_utf8_lossy(&junk)
            .replace("14381 0 0 0 0\"\"", "14381 0 0 1598211621 2012894640\"\"")
            .into_bytes();

        let script = Script::parse(&junk).expect("parses");
        let action = &script.blocks[0].responses[0].actions[0];
        assert_eq!(action.ints[1], 1598211621);
        assert_eq!(action.ints[2], 2012894640);
        assert_eq!(script.encode(), junk);
    }

    #[test]
    fn keeps_strings_verbatim() {
        // Object names in the install include a lone space and bare
        // punctuation. Trimming either would rename the object.
        let odd = String::from_utf8_lossy(TRAP)
            .replace(
                "0 0 0 0 0 0 0 17 0 0 0 0 \"\"OB",
                "0 0 0 0 0 0 0 17 0 0 0 0 \" \"OB",
            )
            .replace("14381 0 0 0 0\"\" \"\"", "14381 0 0 0 0\"#\" \"())\"")
            .into_bytes();

        let script = Script::parse(&odd).expect("parses");
        let action = &script.blocks[0].responses[0].actions[0];
        assert_eq!(action.objects[1].name, " ");
        assert_eq!(action.strings[0], "#");
        assert_eq!(action.strings[1], "())");
        assert_eq!(script.encode(), odd);
    }

    #[test]
    fn reads_negation_and_the_full_integer_range() {
        let script = Script::parse(
            b"SC\nCR\nCO\nTR\n\
              16449 -2147483648 1 2147483647 0 \"pfmw\" \"LOCALS\" OB\n\
              0 0 0 0 0 0 0 1 0 0 0 0 \"\"OB\nTR\nCO\nRS\nRS\nCR\nSC\n",
        )
        .expect("parses");

        let trigger = &script.blocks[0].triggers[0];
        assert!(trigger.negated());
        assert_eq!(trigger.ints, [i32::MIN, i32::MAX, 0]);
        // The scope is stored as a fixed-width prefix of the variable name;
        // splitting the pair is the vocabulary's job, not this layer's.
        assert_eq!(trigger.strings[0], "pfmw");
        assert_eq!(trigger.strings[1], "LOCALS");
    }

    #[test]
    fn a_vocabulary_names_the_calls_and_orders_their_arguments() {
        let mut script = Script::parse(TRAP).expect("parses");
        // Unnamed until the install's tables are attached.
        assert!(format!("{script}").contains("action 151"));

        let mut vocabulary = crate::script::from_lines(
            &[
                "151 DisplayString(O:Object*,I:StrRef*)",
                "113 ForceSpell(O:Target*,I:Spell*Spell)",
            ],
            &["76 Entered(O:Object*)"],
        );
        vocabulary.set_symbols("OBJECT", [(17, "LastTrigger".to_string())].into());
        vocabulary.set_symbols("SPELL", [(2001, "TRAP_FIREBALL".to_string())].into());
        script.set_vocabulary(Rc::new(vocabulary));

        // A complete BAF block: markers, indentation and the trailing blank
        // line WeiDU emits — the text it compiles back to these bytes.
        assert_eq!(
            format!("{script}"),
            "IF\n  \
             Entered([ANYONE])\n\
             THEN\n  \
             RESPONSE #100\n    \
             DisplayString(LastTrigger,14381)\n    \
             ForceSpell(LastTrigger,TRAP_FIREBALL)\n\
             END\n\n"
        );
    }

    #[test]
    fn resolved_comments_annotate_the_calls_that_earn_them() {
        let mut script = Script::parse(TRAP).expect("parses");
        let mut vocabulary = crate::script::from_lines(
            &[
                "151 DisplayString(O:Object*,I:StrRef*)",
                "113 ForceSpell(O:Target*,I:Spell*Spell)",
            ],
            &["76 Entered(O:Object*)"],
        );
        vocabulary.set_symbols("OBJECT", [(17, "LastTrigger".to_string())].into());
        vocabulary.set_symbols("SPELL", [(2001, "TRAP_FIREBALL".to_string())].into());
        script.set_vocabulary(Rc::new(vocabulary));

        // The manager resolves these from the TLK; here they are supplied.
        let mut comments = Comments::default();
        comments
            .strrefs
            .insert(14381, "~Trap Sprung~ [AMB_D05C]".to_string());
        script.set_comments(comments);

        let shown = format!("{script}");
        // The strref argument earns its text and sound; the spell-by-number,
        // whose symbol is already its name, earns nothing.
        assert!(
            shown.contains("DisplayString(LastTrigger,14381)  // ~Trap Sprung~ [AMB_D05C]"),
            "{shown}"
        );
        assert!(
            shown.contains("ForceSpell(LastTrigger,TRAP_FIREBALL)\n"),
            "{shown}"
        );
        assert!(!shown.contains("TRAP_FIREBALL)  //"), "{shown}");
    }

    #[test]
    fn an_identifier_chain_nests_outwards_from_its_base() {
        let mut vocabulary = crate::script::from_lines(&["291 MoveGlobalObject(O:A*,O:B*)"], &[]);
        vocabulary.set_symbols(
            "OBJECT",
            [
                (1, "Myself".to_string()),
                (10, "LastAttackerOf".to_string()),
                (18, "LastSeenBy".to_string()),
                (20, "LastHeardBy".to_string()),
            ]
            .into(),
        );
        vocabulary.set_symbols("EA", [(2, "PC".to_string())].into());
        vocabulary.set_symbols("CLASS", [(207, "PALADIN_ALL".to_string())].into());

        // Stored innermost first: Myself, LastHeardBy, LastAttackerOf.
        let mut chained = Object::default();
        chained.fields[7] = 1;
        chained.fields[8] = 20;
        chained.fields[9] = 10;
        assert_eq!(
            show_object(Some(&vocabulary), &chained),
            "LastAttackerOf(LastHeardBy(Myself))"
        );

        // The attribute bracket is the base when there is no name. A zero
        // attribute is a wildcard and stays numeric.
        let mut attributes = Object::default();
        attributes.fields[0] = 2;
        attributes.fields[3] = 207;
        attributes.fields[7] = 18;
        assert_eq!(
            show_object(Some(&vocabulary), &attributes),
            "LastSeenBy([PC.0.0.PALADIN_ALL])"
        );

        // A death-variable name takes precedence over the attributes.
        let named = Object {
            name: "Sarevok".to_string(),
            fields: attributes.fields,
        };
        assert_eq!(
            show_object(Some(&vocabulary), &named),
            "LastSeenBy(\"Sarevok\")"
        );
    }

    #[test]
    fn junk_a_signature_cannot_explain_is_named_but_kept_in_a_comment() {
        // BioWare left a value in an integer slot `DisplayString` never reads.
        // The call is still named — WeiDU would just drop the value — but the
        // value is preserved in a comment, so nothing is silently lost.
        let junk = String::from_utf8_lossy(TRAP)
            .replace("14381 0 0 0 0\"\"", "14381 0 0 1598211621 0\"\"")
            .into_bytes();
        let mut script = Script::parse_verified(&junk).expect("parses");
        let mut vocabulary = crate::script::from_lines(
            &[
                "151 DisplayString(O:Object*,I:StrRef*)",
                "113 ForceSpell(O:Target*,I:Spell*Spell)",
            ],
            &["76 Entered(O:Object*)"],
        );
        vocabulary.set_symbols("OBJECT", [(17, "LastTrigger".to_string())].into());
        script.set_vocabulary(Rc::new(vocabulary));

        let shown = format!("{script}");
        // Summarised at the top and preserved inline, so a reader sees both.
        assert!(
            shown.contains("// note: 1 call(s) carry data no signature explains"),
            "{shown}"
        );
        assert!(
            shown.contains("DisplayString(LastTrigger,14381)  // unmodelled: int1=1598211621"),
            "{shown}"
        );
        // The record itself is untouched, so it still re-encodes exactly.
        assert_eq!(script.is_exact(), Some(true));
        assert_eq!(script.encode(), junk);
    }

    /// Build a trigger with a death-variable name in its object, for the
    /// grouping tests.
    fn named_trigger(id: i32, string: &str) -> Trigger {
        Trigger {
            id,
            strings: [string.to_string(), String::new()],
            ..Trigger::default()
        }
    }

    /// A vocabulary covering the calls the grouping tests use.
    fn grouping_vocab() -> Rc<Vocabulary> {
        Rc::new(crate::script::from_lines(
            &[],
            &[
                "16419 True()",
                "16443 Dead(S:Name*)",
                "16521 OR(I:OrCount*)",
                "16608 NextTriggerObject(O:Object*)",
            ],
        ))
    }

    #[test]
    fn an_or_groups_and_indents_the_triggers_that_follow_it() {
        let mut or = Trigger {
            id: 16521,
            ..Trigger::default()
        };
        or.ints[0] = 2;
        let block = Block {
            triggers: vec![
                or,
                named_trigger(16419, ""),    // in the group
                named_trigger(16443, "foo"), // in the group
                named_trigger(16419, ""),    // back out, ungrouped
            ],
            responses: vec![Response {
                weight: 100,
                actions: Vec::new(),
            }],
        };
        let mut script = Script {
            blocks: vec![block],
            exact: None,
            vocabulary: None,
            comments: Comments::default(),
        };
        script.set_vocabulary(grouping_vocab());

        assert_eq!(
            format!("{script}"),
            "IF\n  \
             OR(2)\n    \
             True()\n    \
             Dead(\"foo\")\n  \
             True()\n\
             THEN\n  \
             RESPONSE #100\n\
             END\n\n"
        );
    }

    #[test]
    fn next_trigger_object_folds_into_a_trigger_override() {
        // A redirected trigger: the object rides on NextTriggerObject, the
        // condition and its negation on the trigger that follows.
        let redirect = Trigger {
            id: 16608,
            object: Object {
                name: "firkra02".to_string(),
                ..Object::default()
            },
            ..Trigger::default()
        };
        let mut inner = named_trigger(16443, "bar");
        inner.flags = 1; // negated
        let block = Block {
            triggers: vec![redirect, inner],
            responses: vec![Response {
                weight: 100,
                actions: Vec::new(),
            }],
        };
        let mut script = Script {
            blocks: vec![block],
            exact: None,
            vocabulary: None,
            comments: Comments::default(),
        };
        script.set_vocabulary(grouping_vocab());

        let shown = format!("{script}");
        assert!(
            shown.contains("  !TriggerOverride(\"firkra02\",Dead(\"bar\"))"),
            "{shown}"
        );
    }

    #[test]
    fn an_occupied_object_slot_zero_prints_as_an_action_override() {
        // The engine stores no nested action: the actor sits in slot 0 and the
        // redirected call is emitted in its own right.
        let redirected = String::from_utf8_lossy(TRAP)
            .replace(
                "151OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB",
                "151OB\n0 0 0 0 0 0 0 0 0 0 0 0 \"S!ammar\"OB",
            )
            .into_bytes();

        let mut script = Script::parse(&redirected).expect("parses");
        script.set_vocabulary(Rc::new(crate::script::from_lines(
            &["151 DisplayString(O:Object*,I:StrRef*)"],
            &[],
        )));

        let shown = format!("{script}");
        assert!(
            shown.contains("ActionOverride(\"S!ammar\",DisplayString("),
            "{shown}"
        );
        assert_eq!(script.encode(), redirected);
    }

    #[test]
    fn a_concatenating_call_splits_its_scope_back_out() {
        let script = Script::parse(
            b"SC\nCR\nCO\nTR\n\
              16399 1 0 0 0 \"LOCALSPrep\" \"\" OB\n\
              0 0 0 0 0 0 0 0 0 0 0 0 \"\"OB\nTR\nCO\nRS\nRS\nCR\nSC\n",
        )
        .expect("parses");
        let mut script = script;
        script.set_vocabulary(Rc::new(crate::script::from_lines(
            &[],
            &["16399 Global(S:Name*,S:Area*,I:Value*)"],
        )));

        // Stored as scope ++ name in one slot; printed as the two arguments the
        // signature declares, in declaration order.
        let shown = format!("{script}");
        assert!(shown.contains(r#"Global("Prep","LOCALS",1)"#), "{shown}");
    }

    #[test]
    fn an_empty_script_round_trips() {
        let empty = b"SC\nSC\n";
        let script = Script::parse(empty).expect("parses");
        assert!(script.blocks.is_empty());
        assert_eq!(script.encode(), empty);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        // Truncated at every length, so every accessor meets the end of input.
        for len in 0..TRAP.len() {
            let _ = Script::parse(TRAP.get(..len).unwrap_or_default());
        }
        // Structurally wrong rather than short.
        assert!(Script::parse(b"").is_err());
        assert!(Script::parse(b"XX\nSC\n").is_err());
        assert!(Script::parse(b"SC\nSC\njunk").is_err());
        // A number too wide for the slot, and a string with no closing quote.
        assert!(Script::parse(b"SC\nCR\nCO\nTR\n99999999999 0 0 0 0 \"\" \"\" OB\n").is_err());
        assert!(Script::parse(b"SC\nCR\nCO\nTR\n1 0 0 0 0 \"unclosed").is_err());
    }
}
