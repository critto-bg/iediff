//! BAF (textual script) compiler: the reverse of the [`bcs`](crate::formats::bcs)
//! decompiler.
//!
//! It reads the same text WeiDU compiles — `IF` triggers `THEN` responses `END`
//! — back into a [`Script`], resolving each call against the vocabulary and
//! every symbol back to its number. Everything the decompiler does is undone:
//! `OR(n)` is a trigger like any other, `TriggerOverride` unfolds to a
//! `NextTriggerObject` plus its trigger, `ActionOverride` rides in object slot
//! 0, a scope rejoins its variable, and an identifier chain becomes the object
//! fields it came from.
//!
//! The compiler is the inverse of the writer, so `compile(render(bcs)) == bcs`
//! byte for byte wherever the record round-trips at all.

use crate::formats::bcs::{Action, Block, Object, Response, Script, Trigger};
use crate::script::{
    Class, IDENTIFIER_TABLE, OBJECT_TABLES, Signature, Vocabulary, is_scope_param,
};

/// A cursor over the BAF text.
struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    vocabulary: &'a Vocabulary,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8], vocabulary: &'a Vocabulary) -> Parser<'a> {
        Parser {
            data,
            pos: 0,
            vocabulary,
        }
    }

    // ---- structure ---------------------------------------------------------

    fn block(&mut self) -> Result<Block, BafError> {
        self.keyword("IF")?;
        let mut triggers = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_keyword("THEN") {
                break;
            }
            self.condition_line(&mut triggers)?;
        }
        self.keyword("THEN")?;

        let mut responses = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_keyword("END") {
                break;
            }
            responses.push(self.response()?);
        }
        self.keyword("END")?;
        Ok(Block {
            triggers,
            responses,
        })
    }

    /// One line of a condition: a trigger, or `TriggerOverride`, which unfolds
    /// into a `NextTriggerObject` plus the trigger it wraps. A leading `!`
    /// negates the (wrapped) trigger.
    fn condition_line(&mut self, out: &mut Vec<Trigger>) -> Result<(), BafError> {
        let negate = self.eat(b'!');
        let name = self.peek_ident();
        if name.eq_ignore_ascii_case("TriggerOverride") {
            let at = self.pos;
            self.ident();
            self.expect(b'(')?;
            let object = self.object()?;
            self.expect(b',')?;
            out.push(self.next_trigger_object(object, at)?);
            let mut inner = self.trigger_call()?;
            self.expect(b')')?;
            set_negate(&mut inner, negate);
            out.push(inner);
        } else {
            let mut trigger = self.trigger_call()?;
            set_negate(&mut trigger, negate);
            out.push(trigger);
        }
        Ok(())
    }

    fn response(&mut self) -> Result<Response, BafError> {
        self.keyword("RESPONSE")?;
        self.skip_inline_ws();
        self.expect(b'#')?;
        let weight = self.number()?;
        let mut actions = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek_keyword("END") || self.peek_keyword("RESPONSE") {
                break;
            }
            actions.push(self.action()?);
        }
        Ok(Response { weight, actions })
    }

    // ---- calls -------------------------------------------------------------

    fn trigger_call(&mut self) -> Result<Trigger, BafError> {
        let at = self.pos;
        let name = self.ident();
        let signature = self
            .vocabulary
            .trigger_named(&name)
            .ok_or_else(|| BafError::UnknownCall {
                name: name.clone(),
                at,
            })?
            .clone();
        let filled = self.args(&signature)?;
        Ok(build_trigger(&signature, filled))
    }

    /// An action, optionally wrapped in `ActionOverride`, whose object rides in
    /// the action's own object slot 0.
    fn action(&mut self) -> Result<Action, BafError> {
        let name = self.peek_ident();
        if name.eq_ignore_ascii_case("ActionOverride") {
            self.ident();
            self.expect(b'(')?;
            let actor = self.object()?;
            self.expect(b',')?;
            let mut action = self.action_call()?;
            self.expect(b')')?;
            action.objects[0] = actor;
            Ok(action)
        } else {
            self.action_call()
        }
    }

    fn action_call(&mut self) -> Result<Action, BafError> {
        let at = self.pos;
        let name = self.ident();
        let signature = self
            .vocabulary
            .action_named(&name)
            .ok_or_else(|| BafError::UnknownCall {
                name: name.clone(),
                at,
            })?
            .clone();
        let filled = self.args(&signature)?;
        Ok(build_action(&signature, filled))
    }

    /// Build the `NextTriggerObject` a `TriggerOverride` folds from.
    fn next_trigger_object(&self, object: Object, at: usize) -> Result<Trigger, BafError> {
        let signature =
            self.vocabulary
                .trigger_named("NextTriggerObject")
                .ok_or(BafError::UnknownCall {
                    name: "NextTriggerObject".to_string(),
                    at,
                })?;
        Ok(Trigger {
            id: signature.id,
            object,
            ..Trigger::default()
        })
    }

    /// Read a call's parenthesised arguments in declaration order, one per
    /// parameter (a scope shares a parameter but still a comma-separated slot).
    fn args(&mut self, signature: &Signature) -> Result<Filled, BafError> {
        self.expect(b'(')?;
        let mut out = Vec::new();
        for (i, param) in signature.params.iter().enumerate() {
            if param.class == Class::Action {
                continue; // The `ActionOverride` sugar; never an argument here.
            }
            if i > 0 {
                self.expect(b',')?;
            }
            self.skip_inline_ws();
            let value = match param.class {
                Class::String => Value::Str(self.string()?),
                Class::Int => Value::Int(self.int_arg(param.ids.as_deref())?),
                Class::Object => Value::Object(self.object()?),
                Class::Point => {
                    let (x, y) = self.point()?;
                    Value::Point(x, y)
                }
                Class::Action => unreachable!(),
            };
            out.push((param.class, value));
        }
        self.skip_inline_ws();
        self.expect(b')')?;
        Ok(out.into_iter().zip(scope_flags(signature)).collect())
    }

    // ---- arguments ---------------------------------------------------------

    /// An integer argument: a bare number, or a symbol resolved through the
    /// table the parameter names.
    fn int_arg(&mut self, table: Option<&str>) -> Result<i32, BafError> {
        if self.peek_number() {
            return self.number();
        }
        let at = self.pos;
        let symbol = self.ident();
        let table = table.ok_or(BafError::Expected { want: "number", at })?;
        self.vocabulary
            .value(table, &symbol)
            .ok_or_else(|| BafError::UnknownSymbol {
                table: table.to_string(),
                symbol,
                at,
            })
    }

    /// An object argument: identifiers chaining outward around a base of an
    /// attribute bracket, a death-variable name, or nothing (`[ANYONE]`).
    fn object(&mut self) -> Result<Object, BafError> {
        match self.peek() {
            Some(b'[') => self.bracket_object(),
            Some(b'"') => Ok(Object {
                name: self.string()?,
                ..Object::default()
            }),
            _ => {
                let at = self.pos;
                let name = self.ident();
                if self.eat(b'(') {
                    // An identifier wrapping an inner object.
                    let mut inner = self.object()?;
                    self.skip_inline_ws();
                    self.expect(b')')?;
                    self.push_identifier(&mut inner, &name, at)?;
                    Ok(inner)
                } else {
                    // A bare identifier is the innermost object.
                    let mut object = Object::default();
                    self.push_identifier(&mut object, &name, at)?;
                    Ok(object)
                }
            }
        }
    }

    /// The `[EA.GENERAL.…]` form, or the all-zero `[ANYONE]` marker.
    fn bracket_object(&mut self) -> Result<Object, BafError> {
        self.expect(b'[')?;
        let mut object = Object::default();
        // `[ANYONE]` is the decompiler's name for an object with every field at
        // its default, so it fills nothing.
        if self.peek_ident().eq_ignore_ascii_case("ANYONE") {
            self.ident();
            self.expect(b']')?;
            return Ok(object);
        }
        let mut field = 0;
        loop {
            let at = self.pos;
            let value = if self.peek_number() {
                self.number()?
            } else {
                let symbol = self.ident();
                let table = OBJECT_TABLES.get(field).copied().unwrap_or_default();
                self.vocabulary
                    .value(table, &symbol)
                    .ok_or_else(|| BafError::UnknownSymbol {
                        table: table.to_string(),
                        symbol,
                        at,
                    })?
            };
            if let Some(slot) = object.fields.get_mut(field) {
                *slot = value;
            }
            field += 1;
            if !self.eat(b'.') {
                break;
            }
        }
        self.expect(b']')?;
        Ok(object)
    }

    /// A `[x.y]` point.
    fn point(&mut self) -> Result<(i32, i32), BafError> {
        self.expect(b'[')?;
        let x = self.number()?;
        self.expect(b'.')?;
        let y = self.number()?;
        self.expect(b']')?;
        Ok((x, y))
    }

    /// Set the first free identifier field to `name`'s `OBJECT.IDS` value, the
    /// inverse of the decompiler chaining them outward.
    fn push_identifier(&self, object: &mut Object, name: &str, at: usize) -> Result<(), BafError> {
        let value = self
            .vocabulary
            .value(IDENTIFIER_TABLE, name)
            .ok_or_else(|| BafError::UnknownSymbol {
                table: IDENTIFIER_TABLE.to_string(),
                symbol: name.to_string(),
                at,
            })?;
        // Identifier fields follow the seven attributes.
        if let Some(slot) = object.fields[OBJECT_TABLES.len()..]
            .iter_mut()
            .find(|f| **f == 0)
        {
            *slot = value;
        }
        Ok(())
    }

    // ---- lexing ------------------------------------------------------------

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Skip spaces, newlines and `//` comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => self.pos += 1,
                Some(b'/') if self.data.get(self.pos + 1) == Some(&b'/') => {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// Skip spaces and tabs only, staying on the line.
    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.skip_inline_ws();
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), BafError> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(BafError::Expected {
                want: byte_name(byte),
                at: self.pos,
            })
        }
    }

    /// The identifier at the cursor, without consuming it.
    fn peek_ident(&self) -> String {
        let mut at = self.pos;
        while self.data.get(at).is_some_and(|b| b.is_ascii_whitespace()) {
            at += 1;
        }
        let start = at;
        while self.data.get(at).is_some_and(|&b| is_ident_byte(b)) {
            at += 1;
        }
        String::from_utf8_lossy(self.data.get(start..at).unwrap_or_default()).into_owned()
    }

    fn ident(&mut self) -> String {
        self.skip_inline_ws();
        let start = self.pos;
        while self.peek().is_some_and(is_ident_byte) {
            self.pos += 1;
        }
        String::from_utf8_lossy(self.data.get(start..self.pos).unwrap_or_default()).into_owned()
    }

    fn peek_keyword(&self, word: &str) -> bool {
        self.peek_ident().eq_ignore_ascii_case(word)
    }

    fn keyword(&mut self, word: &'static str) -> Result<(), BafError> {
        self.skip_trivia();
        if self.peek_keyword(word) {
            self.ident();
            Ok(())
        } else {
            Err(BafError::Expected {
                want: word,
                at: self.pos,
            })
        }
    }

    fn peek_number(&self) -> bool {
        let mut at = self.pos;
        while self.data.get(at).is_some_and(|b| *b == b' ' || *b == b'\t') {
            at += 1;
        }
        matches!(self.data.get(at), Some(b) if b.is_ascii_digit() || *b == b'-')
    }

    fn number(&mut self) -> Result<i32, BafError> {
        self.skip_inline_ws();
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(self.data.get(start..self.pos).unwrap_or_default())
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(BafError::Expected {
                want: "number",
                at: start,
            })
    }

    /// A double-quoted string; the format has no escapes.
    fn string(&mut self) -> Result<String, BafError> {
        self.skip_inline_ws();
        if self.peek() != Some(b'"') {
            return Err(BafError::Expected {
                want: "string",
                at: self.pos,
            });
        }
        let start = self.pos + 1;
        let len = self
            .data
            .get(start..)
            .and_then(|rest| rest.iter().position(|&b| b == b'"'))
            .ok_or(BafError::UnexpectedEnd)?;
        self.pos = start + len + 1;
        Ok(
            String::from_utf8_lossy(self.data.get(start..start + len).unwrap_or_default())
                .into_owned(),
        )
    }
}

/// One argument, read before it is placed in the record's slots.
enum Value {
    Int(i32),
    Str(String),
    Object(Object),
    Point(i32, i32),
}

/// The record slots being filled as arguments are placed.
#[derive(Default)]
struct Slots {
    next_int: usize,
    next_string: usize,
    next_object: usize,
}

/// A tuple of (parameter class, argument value, is-scope), the shape [`args`]
/// yields for the record builders.
type Filled = Vec<((Class, Value), bool)>;

/// Anything that can go wrong while compiling BAF.
#[derive(Debug, PartialEq, Eq)]
pub enum BafError {
    /// A keyword, punctuation or token was required and something else was found.
    Expected { want: &'static str, at: usize },
    /// A call name absent from `ACTION.IDS` / `TRIGGER.IDS`.
    UnknownCall { name: String, at: usize },
    /// A symbol absent from the IDS table its parameter names.
    UnknownSymbol {
        table: String,
        symbol: String,
        at: usize,
    },
    /// Text ran out mid-construct.
    UnexpectedEnd,
}

impl std::fmt::Display for BafError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BafError::Expected { want, at } => {
                write!(f, "malformed BAF: expected {want} at byte {at}")
            }
            BafError::UnknownCall { name, at } => {
                write!(f, "malformed BAF: unknown call {name:?} at byte {at}")
            }
            BafError::UnknownSymbol { table, symbol, at } => {
                write!(
                    f,
                    "malformed BAF: {symbol:?} is not in {table}.IDS at byte {at}"
                )
            }
            BafError::UnexpectedEnd => write!(f, "malformed BAF: unexpected end of input"),
        }
    }
}

impl std::error::Error for BafError {}

/// Compile a whole BAF script into the record form.
pub fn compile(text: &[u8], vocabulary: &Vocabulary) -> Result<Script, BafError> {
    let mut p = Parser::new(text, vocabulary);
    let mut blocks = Vec::new();
    p.skip_trivia();
    while !p.at_end() {
        blocks.push(p.block()?);
        p.skip_trivia();
    }
    Ok(Script::from_blocks(blocks))
}

/// Compile a run of triggers (a DLG condition), for typing a dialogue's script
/// blocks exactly rather than by text.
pub fn triggers(text: &[u8], vocabulary: &Vocabulary) -> Result<Vec<Trigger>, BafError> {
    let mut p = Parser::new(text, vocabulary);
    let mut out = Vec::new();
    p.skip_trivia();
    while !p.at_end() {
        p.condition_line(&mut out)?;
        p.skip_trivia();
    }
    Ok(out)
}

/// Compile a run of actions (a DLG response).
pub fn actions(text: &[u8], vocabulary: &Vocabulary) -> Result<Vec<Action>, BafError> {
    let mut p = Parser::new(text, vocabulary);
    let mut out = Vec::new();
    p.skip_trivia();
    while !p.at_end() {
        out.push(p.action()?);
        p.skip_trivia();
    }
    Ok(out)
}

/// Whether each parameter, in order, is a scope that shares its predecessor's
/// string slot rather than opening its own.
fn scope_flags(signature: &Signature) -> Vec<bool> {
    let mut prev_string = false;
    signature
        .params
        .iter()
        .filter(|p| p.class != Class::Action)
        .map(|p| {
            let scoped =
                prev_string && p.class == Class::String && is_scope_param(&signature.name, &p.name);
            prev_string = p.class == Class::String;
            scoped
        })
        .collect()
}

fn build_trigger(signature: &Signature, filled: Filled) -> Trigger {
    let mut trigger = Trigger {
        id: signature.id,
        ..Trigger::default()
    };
    let mut slots = Slots::default();
    let mut point = (0, 0);
    for ((class, value), scoped) in filled {
        place(
            &mut slots,
            class,
            value,
            scoped,
            &mut trigger.ints,
            &mut trigger.strings,
            std::slice::from_mut(&mut trigger.object),
            &mut point,
        );
    }
    trigger
}

fn build_action(signature: &Signature, filled: Filled) -> Action {
    let mut action = Action {
        id: signature.id,
        ..Action::default()
    };
    let mut slots = Slots::default();
    let mut point = (0, 0);
    // The call's own object parameters fill slots 1 and 2; slot 0 is reserved
    // for the `ActionOverride` actor.
    let (_, objects) = action.objects.split_at_mut(1);
    for ((class, value), scoped) in filled {
        place(
            &mut slots,
            class,
            value,
            scoped,
            &mut action.ints,
            &mut action.strings,
            objects,
            &mut point,
        );
    }
    action.point = point;
    action
}

/// Route one argument to its record slot, joining a scope onto the string slot
/// it shares. Each class fills the next free slot of its kind.
#[allow(clippy::too_many_arguments)]
fn place(
    slots: &mut Slots,
    class: Class,
    value: Value,
    scoped: bool,
    ints: &mut [i32],
    strings: &mut [String; 2],
    objects: &mut [Object],
    point: &mut (i32, i32),
) {
    match (class, value) {
        (Class::Int, Value::Int(v)) => {
            if let Some(slot) = ints.get_mut(slots.next_int) {
                *slot = v;
            }
            slots.next_int += 1;
        }
        (Class::String, Value::Str(s)) => {
            if scoped {
                // The scope is stored as a prefix on the variable in the slot
                // the previous string parameter opened.
                if let Some(slot) = slots
                    .next_string
                    .checked_sub(1)
                    .and_then(|i| strings.get_mut(i))
                {
                    *slot = format!("{s}{slot}");
                }
            } else {
                if let Some(slot) = strings.get_mut(slots.next_string) {
                    *slot = s;
                }
                slots.next_string += 1;
            }
        }
        (Class::Object, Value::Object(o)) => {
            if let Some(slot) = objects.get_mut(slots.next_object) {
                *slot = o;
            }
            slots.next_object += 1;
        }
        (Class::Point, Value::Point(x, y)) => *point = (x, y),
        _ => {}
    }
}

/// Set or clear a trigger's negate bit.
fn set_negate(trigger: &mut Trigger, negate: bool) {
    if negate {
        trigger.flags |= 1;
    }
}

/// Whether a byte can appear in a call name or symbol. IDS symbols include a
/// hyphen (`KUO-TOA`); a leading `-` is claimed as a number first, so an
/// internal one is unambiguous.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'#' | b'-')
}

/// A readable name for the byte a parser expected.
fn byte_name(byte: u8) -> &'static str {
    match byte {
        b'(' => "(",
        b')' => ")",
        b'[' => "[",
        b']' => "]",
        b',' => ",",
        b'.' => ".",
        b'#' => "#",
        _ => "delimiter",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::from_lines;

    /// A vocabulary covering the calls and symbols the compiler tests use.
    fn vocab() -> Vocabulary {
        let mut v = from_lines(
            &[
                "151 DisplayString(O:Object*,I:StrRef*)",
                "113 ForceSpell(O:Target*,I:Spell*Spell)",
                "30 SetGlobal(S:Name*,S:Area*,I:Value*)",
                "1 ActionOverride(O:Actor*,A:Action*)",
                "202 ChangeAnimation(S:ResRef*)",
            ],
            &[
                "76 Entered(O:Object*)",
                "16419 True()",
                "16521 OR(I:OrCount*)",
                "16443 Dead(S:Name*)",
                "16608 NextTriggerObject(O:Object*)",
            ],
        );
        v.set_symbols(
            "OBJECT",
            [(17, "LastTrigger".to_string()), (21, "Player1".to_string())].into(),
        );
        v.set_symbols("SPELL", [(2001, "TRAP_FIREBALL".to_string())].into());
        v
    }

    /// Compiling the decompiled form reproduces the record: a full block with a
    /// trigger, two actions, a symbol, an object identifier and a strref.
    #[test]
    fn a_block_round_trips_through_the_compiler() {
        let baf = "IF\n  Entered([ANYONE])\nTHEN\n  RESPONSE #100\n    \
                   DisplayString(LastTrigger,14381)\n    \
                   ForceSpell(LastTrigger,TRAP_FIREBALL)\nEND\n";
        let script = compile(baf.as_bytes(), &vocab()).expect("compiles");
        // Re-rendering with the same vocabulary gives the text back, plus the
        // blank line the writer puts after every block.
        let mut rendered = script.clone();
        rendered.set_vocabulary(std::rc::Rc::new(vocab()));
        assert_eq!(format!("{rendered}"), format!("{baf}\n"));
    }

    #[test]
    fn a_scope_rejoins_its_variable() {
        // `Global`'s scope is stored as a six-character prefix on the name.
        let script = compile(
            b"IF\nTHEN\n  RESPONSE #100\n    SetGlobal(\"Prep\",\"LOCALS\",1)\nEND\n",
            &vocab(),
        )
        .expect("compiles");
        let action = &script.blocks[0].responses[0].actions[0];
        assert_eq!(action.strings[0], "LOCALSPrep");
        assert_eq!(action.ints[0], 1);
    }

    #[test]
    fn an_action_override_rides_in_object_slot_zero() {
        let script = compile(
            b"IF\nTHEN\n  RESPONSE #100\n    \
              ActionOverride(Player1,ChangeAnimation(\"S!ammar2\"))\nEND\n",
            &vocab(),
        )
        .expect("compiles");
        let action = &script.blocks[0].responses[0].actions[0];
        assert_eq!(action.objects[0].fields[7], 21); // Player1 in slot 0
        assert_eq!(action.strings[0], "S!ammar2");
    }

    #[test]
    fn a_trigger_override_unfolds_to_next_trigger_object() {
        let script = compile(
            b"IF\n  !TriggerOverride(Player1,Dead(\"foo\"))\nTHEN\n  RESPONSE #100\nEND\n",
            &vocab(),
        )
        .expect("compiles");
        let triggers = &script.blocks[0].triggers;
        assert_eq!(triggers.len(), 2);
        // The object rides on NextTriggerObject; the negation on the wrapped one.
        assert_eq!(triggers[0].object.fields[7], 21);
        assert!(!triggers[0].negated());
        assert_eq!(triggers[1].strings[0], "foo");
        assert!(triggers[1].negated());
    }

    #[test]
    fn an_or_is_just_a_trigger_with_its_count() {
        let script = compile(
            b"IF\n  OR(2)\n    True()\n    Dead(\"x\")\nTHEN\n  RESPONSE #100\nEND\n",
            &vocab(),
        )
        .expect("compiles");
        let triggers = &script.blocks[0].triggers;
        assert_eq!(triggers.len(), 3); // OR, then its two members, flat
        assert_eq!(triggers[0].ints[0], 2);
    }

    #[test]
    fn an_unknown_call_or_symbol_is_an_error_not_a_panic() {
        let v = vocab();
        assert!(matches!(
            compile(b"IF\n  NoSuchTrigger()\nTHEN\n  RESPONSE #100\nEND\n", &v),
            Err(BafError::UnknownCall { .. })
        ));
        assert!(matches!(
            compile(
                b"IF\nTHEN\n  RESPONSE #100\n    ForceSpell(Player1,NOT_A_SPELL)\nEND\n",
                &v
            ),
            Err(BafError::UnknownSymbol { .. })
        ));
        // Truncated at every length: every accessor must meet the end safely.
        let full = b"IF\n  Entered([ANYONE])\nTHEN\n  RESPONSE #100\n    True()\nEND\n";
        for len in 0..full.len() {
            let _ = compile(full.get(..len).unwrap_or_default(), &v);
        }
    }
}
