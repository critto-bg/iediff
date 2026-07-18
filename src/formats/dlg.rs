//! DLG (dialogue) parser.
//!
//! A conversation stored as a state machine. Each [`State`] is a line the actor
//! may say, guarded by a trigger; it fans out to a run of [`Transition`]s — the
//! player's possible replies, each of which may carry its own text, a journal
//! entry, a script action, and a link to the next state (in this file or
//! another DLG). The trigger and action *scripts* live in three side tables of
//! `(offset, length)` pairs pointing at non-NUL-terminated source text.
//!
//! Only `V1.0` is handled. All offsets are absolute from the file start.
//! Spec: `md/file_formats/ie_formats/dlg_v1.md`.

use std::fmt;
use std::io::{self, Write};

use crate::{Reader, StrrefUse, Truncated, describe_tag};

/// Resource type code for DLG files.
pub const DLG_TYPE: u16 = 0x03f3;

const DLG_SIGNATURE: &[u8; 4] = b"DLG ";
const DLG_VERSION: &[u8; 4] = b"V1.0";

/// On-disk size of one record in each fixed-size section.
const STATE_SIZE: usize = 0x10;
const TRANSITION_SIZE: usize = 0x20;
const STRING_ENTRY_SIZE: usize = 0x08;

/// The "no trigger/no string" sentinel used in strref and index fields.
const NONE: u32 = u32::MAX;

/// Script actions whose integer argument is a TLK strref, so a number appearing
/// there is a genuine (if untyped) string reference. Derived from the `StrRef`-typed
/// parameters in the game's `ACTION.IDS`, plus `AddJournalEntry`, whose `Entry`
/// argument is a journal strref the IDS simply labels differently. Triggers
/// declare no strref parameters, so only actions are scanned. Matched
/// case-insensitively.
const STRREF_ACTIONS: [&str; 19] = [
    "DisplayString",
    "DisplayStringHead",
    "DisplayStringHeadDead",
    "DisplayStringHeadNoLog",
    "DisplayStringHeadOwner",
    "DisplayStringNoName",
    "DisplayStringNoNameDlg",
    "DisplayStringNoNameHead",
    "DisplayStringPoint",
    "DisplayStringPointLog",
    "DisplayStringWait",
    "AddJournalEntry",
    "EraseJournalEntry",
    "GameOver",
    "RemoveMapNote",
    "SetName",
    "SetPlayerSound",
    "SetQuestDone",
    "SetToken",
];

/// Transition flag bits that gate its optional fields.
const HAS_TEXT: u32 = 1 << 0;
const HAS_TRIGGER: u32 = 1 << 1;
const HAS_ACTION: u32 = 1 << 2;
const TERMINATES: u32 = 1 << 3;
const HAS_JOURNAL: u32 = 1 << 4;

/// Anything that can go wrong while parsing a DLG file.
#[derive(Debug)]
pub enum DlgError {
    Truncated(Truncated),
    BadSignature([u8; 4]),
    BadVersion([u8; 4]),
}

impl fmt::Display for DlgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DlgError::Truncated(e) => write!(f, "{e}"),
            DlgError::BadSignature(found) => {
                write!(f, "not a DLG file: signature was {}", describe_tag(found))
            }
            DlgError::BadVersion(found) => {
                write!(f, "unsupported DLG version: {}", describe_tag(found))
            }
        }
    }
}

impl std::error::Error for DlgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DlgError::Truncated(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Truncated> for DlgError {
    fn from(e: Truncated) -> Self {
        DlgError::Truncated(e)
    }
}

/// One actor line: the text spoken, the run of transitions it offers, and the
/// state trigger that guards it (0x10 bytes).
pub struct State {
    response_text: u32,
    first_transition: u32,
    transition_count: u32,
    /// Index into the state-trigger table, or [`NONE`].
    trigger_index: u32,
    /// Response text, resolved by the manager from the TLK.
    response_label: Option<String>,
}

impl State {
    fn parse_at(reader: &Reader, base: usize) -> Result<State, Truncated> {
        Ok(State {
            response_text: reader.u32_at(base, "state response text")?,
            first_transition: reader.u32_at(base + 0x04, "state first transition")?,
            transition_count: reader.u32_at(base + 0x08, "state transition count")?,
            trigger_index: reader.u32_at(base + 0x0c, "state trigger index")?,
            response_label: None,
        })
    }

    /// The actor response strref, for the manager to resolve against the TLK.
    pub fn strref(&self) -> u32 {
        self.response_text
    }

    /// Attach the resolved response text.
    pub fn set_label(&mut self, label: Option<String>) {
        self.response_label = label;
    }
}

/// One player reply: its text and journal strrefs (each present only when the
/// matching flag bit is set), an optional trigger and action, and the link to
/// the next state (0x20 bytes).
pub struct Transition {
    flags: u32,
    player_text: u32,
    journal_text: u32,
    /// Index into the transition-trigger table (valid only with [`HAS_TRIGGER`]).
    trigger_index: u32,
    /// Index into the action table (valid only with [`HAS_ACTION`]).
    action_index: u32,
    /// DLG holding the next state (empty = this file); unused when [`TERMINATES`].
    next_dlg: String,
    next_state: u32,
    /// Player text, resolved by the manager from the TLK.
    player_label: Option<String>,
    /// Journal text, resolved by the manager from the TLK.
    journal_label: Option<String>,
}

impl Transition {
    fn parse_at(reader: &Reader, base: usize) -> Result<Transition, Truncated> {
        Ok(Transition {
            flags: reader.u32_at(base, "transition flags")?,
            player_text: reader.u32_at(base + 0x04, "transition player text")?,
            journal_text: reader.u32_at(base + 0x08, "transition journal text")?,
            trigger_index: reader.u32_at(base + 0x0c, "transition trigger index")?,
            action_index: reader.u32_at(base + 0x10, "transition action index")?,
            next_dlg: reader.resref_at(base + 0x14, "transition next dlg")?,
            next_state: reader.u32_at(base + 0x1c, "transition next state")?,
            player_label: None,
            journal_label: None,
        })
    }

    fn has(&self, bit: u32) -> bool {
        self.flags & bit != 0
    }

    /// The player-text strref, present only when the text flag is set.
    pub fn player_strref(&self) -> Option<u32> {
        self.has(HAS_TEXT).then_some(self.player_text)
    }

    /// The journal-text strref, present only when the journal flag is set.
    pub fn journal_strref(&self) -> Option<u32> {
        self.has(HAS_JOURNAL).then_some(self.journal_text)
    }

    /// Attach the resolved player text.
    pub fn set_player_label(&mut self, label: Option<String>) {
        self.player_label = label;
    }

    /// Attach the resolved journal text.
    pub fn set_journal_label(&mut self, label: Option<String>) {
        self.journal_label = label;
    }
}

/// A parsed DLG file: the states and transitions, plus the trigger and action
/// script tables they index into.
pub struct Dlg {
    /// Interrupt behaviour flags (absent in BG1 DLGs, treated as 0).
    flags: u32,
    states: Vec<State>,
    transitions: Vec<Transition>,
    state_triggers: Vec<String>,
    trans_triggers: Vec<String>,
    actions: Vec<String>,
}

impl Dlg {
    /// Parse a DLG V1.0 resource. Each section is read from its header-declared
    /// offset and count.
    pub fn parse(data: &[u8]) -> Result<Dlg, DlgError> {
        let reader = Reader::new(data);

        let signature = reader.tag_at(0x00, "signature")?;
        if &signature != DLG_SIGNATURE {
            return Err(DlgError::BadSignature(signature));
        }
        let version = reader.tag_at(0x04, "version")?;
        if &version != DLG_VERSION {
            return Err(DlgError::BadVersion(version));
        }

        let states = Self::read_records(
            &reader,
            reader.u32_at(0x0c, "state offset")? as usize,
            reader.u32_at(0x08, "state count")? as usize,
            STATE_SIZE,
            State::parse_at,
        )?;
        let transitions = Self::read_records(
            &reader,
            reader.u32_at(0x14, "transition offset")? as usize,
            reader.u32_at(0x10, "transition count")? as usize,
            TRANSITION_SIZE,
            Transition::parse_at,
        )?;
        let state_triggers = Self::read_strings(
            &reader,
            reader.u32_at(0x18, "state trigger offset")? as usize,
            reader.u32_at(0x1c, "state trigger count")? as usize,
        )?;
        let trans_triggers = Self::read_strings(
            &reader,
            reader.u32_at(0x20, "transition trigger offset")? as usize,
            reader.u32_at(0x24, "transition trigger count")? as usize,
        )?;
        let actions = Self::read_strings(
            &reader,
            reader.u32_at(0x28, "action offset")? as usize,
            reader.u32_at(0x2c, "action count")? as usize,
        )?;

        // The interrupt-behaviour flags word does not exist in BG1 DLGs; treat a
        // truncated read as absent rather than failing the whole parse.
        let flags = reader.u32_at(0x30, "dialog flags").unwrap_or(0);

        Ok(Dlg {
            flags,
            states,
            transitions,
            state_triggers,
            trans_triggers,
            actions,
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
        let mut records = Vec::new();
        for i in 0..count {
            records.push(parse_at(reader, offset + i * record_size)?);
        }
        Ok(records)
    }

    /// Read a script-string table: `count` `(offset, length)` pairs, each
    /// pointing at a run of non-NUL-terminated source text elsewhere in the file.
    fn read_strings(
        reader: &Reader,
        offset: usize,
        count: usize,
    ) -> Result<Vec<String>, Truncated> {
        let mut out = Vec::new();
        for i in 0..count {
            let base = offset + i * STRING_ENTRY_SIZE;
            let str_off = reader.u32_at(base, "script string offset")? as usize;
            let str_len = reader.u32_at(base + 0x04, "script string length")? as usize;
            let bytes = reader.slice(str_off, str_len, "script string")?;
            out.push(String::from_utf8_lossy(bytes).trim().to_string());
        }
        Ok(out)
    }

    /// The states, for the manager to resolve their response strrefs.
    pub fn states_mut(&mut self) -> &mut [State] {
        &mut self.states
    }

    /// The transitions, for the manager to resolve their player/journal strrefs.
    pub fn transitions_mut(&mut self) -> &mut [Transition] {
        &mut self.transitions
    }

    /// Every TLK strref this dialogue references, tagged with its field: each
    /// state's response text, and each transition's player text and journal
    /// entry (the latter two only when the transition flags say they are present,
    /// so an inactive field can't produce a false match).
    pub fn strref_uses(&self) -> Vec<StrrefUse> {
        let mut uses = Vec::new();

        for (i, s) in self.states.iter().enumerate() {
            uses.push(StrrefUse::new(
                s.response_text,
                format!("state[{i}].response"),
            ));
        }

        for (i, t) in self.transitions.iter().enumerate() {
            if let Some(strref) = t.player_strref() {
                uses.push(StrrefUse::new(
                    strref,
                    format!("transition[{i}].player_text"),
                ));
            }

            if let Some(strref) = t.journal_strref() {
                uses.push(StrrefUse::new(strref, format!("transition[{i}].journal")));
            }
        }
        uses
    }

    /// Soft ("candidate") strref matches inside the action scripts: each action
    /// slot that calls a [`STRREF_ACTIONS`] function with `target` as its integer
    /// argument, tagged with the slot and the calling action.
    pub fn script_strref_candidates(&self, target: u32) -> Vec<String> {
        let mut out = Vec::new();
        for (i, action) in self.actions.iter().enumerate() {
            for name in matched_actions(action, target) {
                out.push(format!("action[{i}] {name}"));
            }
        }
        out
    }

    /// The transitions belonging to a state, sliced from the flat transition
    /// pool by its `first_transition`/`transition_count`. Bounds are clamped so a
    /// bad index yields an empty slice rather than panicking.
    fn state_transitions(&self, s: &State) -> &[Transition] {
        let start = s.first_transition as usize;
        let end = start.saturating_add(s.transition_count as usize);
        self.transitions
            .get(start..end.min(self.transitions.len()))
            .unwrap_or(&[])
    }

    /// Look up a script string by index into one of the side tables, skipping
    /// the [`NONE`] sentinel and out-of-range or empty entries.
    fn script(table: &[String], index: u32) -> Option<&str> {
        if index == NONE {
            return None;
        }
        table
            .get(index as usize)
            .filter(|s| !s.is_empty())
            .map(String::as_str)
    }

    /// Write the whole dialogue as flat `key=value` lines: header flags bare,
    /// each record under `<section>.<n>.`.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "flags={}", self.flags)?;

        for (i, s) in self.states.iter().enumerate() {
            writeln!(out, "state.{i}.response={}", strref_raw(s.response_text))?;
            writeln!(out, "state.{i}.first_transition={}", s.first_transition)?;
            writeln!(out, "state.{i}.transition_count={}", s.transition_count)?;
            writeln!(out, "state.{i}.trigger={}", strref_raw(s.trigger_index))?;
        }

        for (i, t) in self.transitions.iter().enumerate() {
            writeln!(out, "transition.{i}.flags={}", t.flags)?;
            writeln!(
                out,
                "transition.{i}.player_text={}",
                strref_raw(t.player_text)
            )?;
            writeln!(out, "transition.{i}.journal={}", strref_raw(t.journal_text))?;
            writeln!(
                out,
                "transition.{i}.trigger={}",
                strref_raw(t.trigger_index)
            )?;
            writeln!(out, "transition.{i}.action={}", strref_raw(t.action_index))?;
            writeln!(out, "transition.{i}.next_dlg={}", t.next_dlg)?;
            writeln!(out, "transition.{i}.next_state={}", t.next_state)?;
        }

        for (i, s) in self.state_triggers.iter().enumerate() {
            writeln!(out, "state_trigger.{i}={s}")?;
        }
        for (i, s) in self.trans_triggers.iter().enumerate() {
            writeln!(out, "transition_trigger.{i}={s}")?;
        }
        for (i, s) in self.actions.iter().enumerate() {
            writeln!(out, "action.{i}={s}")?;
        }

        Ok(())
    }
}

impl fmt::Display for Dlg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "DLG V1.0  ({} states, {} transitions)",
            self.states.len(),
            self.transitions.len()
        )?;

        for (i, s) in self.states.iter().enumerate() {
            write!(f, "\n  state {i}:")?;
            match &s.response_label {
                Some(text) => writeln!(f, " {text:?}")?,
                None => writeln!(f, " [{}]", strref_raw(s.response_text))?,
            }
            if let Some(trigger) = Self::script(&self.state_triggers, s.trigger_index) {
                writeln!(f, "    trigger: {trigger}")?;
            }
            for t in self.state_transitions(s) {
                write!(f, "    ->")?;
                if t.has(HAS_TEXT) {
                    match &t.player_label {
                        Some(text) => write!(f, " {text:?}")?,
                        None => write!(f, " [{}]", strref_raw(t.player_text))?,
                    }
                }
                if t.has(TERMINATES) {
                    write!(f, " (end)")?;
                } else if t.next_dlg.is_empty() {
                    write!(f, " :{}", t.next_state)?;
                } else {
                    write!(f, " {}:{}", t.next_dlg, t.next_state)?;
                }
                writeln!(f)?;
                if t.has(HAS_JOURNAL) {
                    match &t.journal_label {
                        Some(text) => writeln!(f, "       journal: {text:?}")?,
                        None => writeln!(f, "       journal: [{}]", strref_raw(t.journal_text))?,
                    }
                }
                if t.has(HAS_TRIGGER)
                    && let Some(trigger) = Self::script(&self.trans_triggers, t.trigger_index)
                {
                    writeln!(f, "       trigger: {trigger}")?;
                }
                if t.has(HAS_ACTION)
                    && let Some(action) = Self::script(&self.actions, t.action_index)
                {
                    writeln!(f, "       action: {action}")?;
                }
            }
        }

        Ok(())
    }
}

/// The [`STRREF_ACTIONS`] functions whose call in `script` passes `target` as a
/// bare integer argument. Nested calls are found too (the scan doesn't stop at
/// the first call), so a strref action buried in an `ActionOverride(...)` still
/// matches. Each function is reported at most once per script.
fn matched_actions(script: &str, target: u32) -> Vec<&'static str> {
    let bytes = script.as_bytes();
    let mut out: Vec<&'static str> = Vec::new();
    let mut i = 0;

    while let Some(&c) = bytes.get(i) {
        if c.is_ascii_alphabetic() {
            let start = i;
            while matches!(bytes.get(i), Some(b) if b.is_ascii_alphanumeric() || *b == b'_') {
                i += 1;
            }

            if bytes.get(i) == Some(&b'(')
                && let Some(name) = script.get(start..i)
                && let Some(canon) = STRREF_ACTIONS
                    .iter()
                    .copied()
                    .find(|a| a.eq_ignore_ascii_case(name))
                && !out.contains(&canon)
                && call_arg_has(script, i, target)
            {
                out.push(canon);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Whether the call opening at `open` (the index of its `(`) has `target` as a
/// direct integer argument. Digits inside quotes (resrefs/tokens), inside a
/// `[x.y]` point literal, glued to a name (`Player1`), or nested in a deeper
/// call are skipped, so the match tracks the action's typed strref parameter
/// rather than an incidental number.
fn call_arg_has(script: &str, open: usize, target: u32) -> bool {
    let bytes = script.as_bytes();
    let mut depth = 0usize;
    let mut in_quote = false;
    let mut i = open;
    while let Some(&c) = bytes.get(i) {
        if in_quote {
            if c == b'"' {
                in_quote = false;
            }
            i += 1;
            continue;
        }

        match c {
            b'"' => {
                in_quote = true;
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return false;
                }
                i += 1;
            }
            // Skip a point literal `[x.y]`; its coordinates are not strrefs.
            b'[' => {
                while matches!(bytes.get(i), Some(b) if *b != b']') {
                    i += 1;
                }
            }
            _ if c.is_ascii_digit() && depth == 1 => {
                let start = i;
                while matches!(bytes.get(i), Some(b) if b.is_ascii_digit()) {
                    i += 1;
                }
                let glued = start > 0 && bytes.get(start - 1).is_some_and(u8::is_ascii_alphabetic);
                if !glued
                    && script.get(start..i).and_then(|s| s.parse::<u32>().ok()) == Some(target)
                {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// Render a strref/index for the machine view, mapping the `0xFFFFFFFF`
/// sentinel to `-1` for readability.
fn strref_raw(value: u32) -> String {
    if value == NONE {
        "-1".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal one-state DLG: the state offers two transitions (one with
    /// player text + journal + action + terminates, one bare/no-text), guarded by
    /// a state trigger.
    fn build_dlg() -> Vec<u8> {
        let state_off = 0x34u32;
        let trans_off = state_off + STATE_SIZE as u32;
        let strig_off = trans_off + 2 * TRANSITION_SIZE as u32;
        let ttrig_off = strig_off + STRING_ENTRY_SIZE as u32;
        let action_off = ttrig_off; // no transition triggers
        let strings_off = action_off + STRING_ENTRY_SIZE as u32;

        let trigger_str = b"NumTimesTalkedTo(0)";
        let action_str = b"EraseJournalEntry(103019)";

        let mut d = vec![0u8; strings_off as usize];
        d[0x00..0x04].copy_from_slice(b"DLG ");
        d[0x04..0x08].copy_from_slice(b"V1.0");
        let put = |d: &mut [u8], at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut d, 0x08, 1); // states
        put(&mut d, 0x0c, state_off);
        put(&mut d, 0x10, 2); // transitions
        put(&mut d, 0x14, trans_off);
        put(&mut d, 0x18, strig_off);
        put(&mut d, 0x1c, 1); // state triggers
        put(&mut d, 0x20, ttrig_off);
        put(&mut d, 0x24, 0); // transition triggers
        put(&mut d, 0x28, action_off);
        put(&mut d, 0x2c, 1); // actions
        put(&mut d, 0x30, 0); // flags

        let s = state_off as usize;
        put(&mut d, s, 100); // response strref
        put(&mut d, s + 0x04, 0); // first transition
        put(&mut d, s + 0x08, 2); // transition count
        put(&mut d, s + 0x0c, 0); // trigger index

        let t0 = trans_off as usize;
        put(&mut d, t0, HAS_TEXT | HAS_ACTION | HAS_JOURNAL | TERMINATES);
        put(&mut d, t0 + 0x04, 200); // player text
        put(&mut d, t0 + 0x08, 300); // journal
        put(&mut d, t0 + 0x0c, NONE); // trigger index (bit unset)
        put(&mut d, t0 + 0x10, 0); // action index

        let t1 = t0 + TRANSITION_SIZE;
        put(&mut d, t1, TERMINATES); // no text bit
        put(&mut d, t1 + 0x04, 999); // stray value in an inactive field
        put(&mut d, t1 + 0x08, 999);

        // Script string tables point past the fixed sections.
        let mut blob = Vec::new();
        let trig_at = strings_off;
        blob.extend_from_slice(trigger_str);
        let act_at = strings_off + trigger_str.len() as u32;
        blob.extend_from_slice(action_str);

        put(&mut d, strig_off as usize, trig_at);
        put(&mut d, strig_off as usize + 4, trigger_str.len() as u32);
        put(&mut d, action_off as usize, act_at);
        put(&mut d, action_off as usize + 4, action_str.len() as u32);

        d.extend_from_slice(&blob);
        d
    }

    #[test]
    fn parses_states_transitions_and_scripts() {
        let dlg = Dlg::parse(&build_dlg()).expect("parse");
        assert_eq!(dlg.states.len(), 1);
        assert_eq!(dlg.transitions.len(), 2);
        assert_eq!(dlg.states[0].response_text, 100);
        assert_eq!(dlg.state_triggers, vec!["NumTimesTalkedTo(0)"]);
        assert_eq!(dlg.actions, vec!["EraseJournalEntry(103019)"]);
        assert!(dlg.trans_triggers.is_empty());
    }

    #[test]
    fn script_candidates_report_slot_and_action() {
        let dlg = Dlg::parse(&build_dlg()).expect("parse");
        assert_eq!(
            dlg.script_strref_candidates(103019),
            vec!["action[0] EraseJournalEntry".to_string()]
        );
        // A different strref does not match the sole action.
        assert!(dlg.script_strref_candidates(999).is_empty());
    }

    #[test]
    fn matched_actions_tracks_typed_strref_arguments() {
        // Strref-bearing actions match their integer argument.
        assert_eq!(
            matched_actions("EraseJournalEntry(103019)", 103019),
            vec!["EraseJournalEntry"]
        );
        assert_eq!(
            matched_actions("DisplayStringHead(\"OHHFAK\",102468)", 102468),
            vec!["DisplayStringHead"]
        );
        // Gold, XP and flag values live in actions that carry no strref.
        assert!(matched_actions("GiveGoldForce(8000)", 8000).is_empty());
        assert!(matched_actions("SetGlobal(\"x\",\"GLOBAL\",1)", 1).is_empty());
        // Wrong value: the action matches but the number doesn't.
        assert!(matched_actions("EraseJournalEntry(103019)", 999).is_empty());
        // Digits glued to an object name, inside quotes, or inside a point
        // literal are not the strref argument.
        assert!(matched_actions("DisplayStringHead(Player1,5)", 1).is_empty());
        assert_eq!(
            matched_actions("DisplayStringPoint([2844.1560],102468)", 102468),
            vec!["DisplayStringPoint"]
        );
        assert!(matched_actions("DisplayStringPoint([2844.1560],102468)", 2844).is_empty());
        // Case-insensitive, and found even when nested in another action.
        assert_eq!(
            matched_actions("ActionOverride(Player1,eraseJournalEntry(103019))", 103019),
            vec!["EraseJournalEntry"]
        );
    }

    #[test]
    fn strref_uses_are_gated_by_flag_bits() {
        let dlg = Dlg::parse(&build_dlg()).expect("parse");
        let uses: Vec<(u32, String)> = dlg
            .strref_uses()
            .into_iter()
            .map(|u| (u.value, u.path))
            .collect();
        // State response plus transition 0's text and journal; transition 1 has
        // no text bit, so its stray 999 must not appear.
        assert_eq!(
            uses,
            vec![
                (100, "state[0].response".to_string()),
                (200, "transition[0].player_text".to_string()),
                (300, "transition[0].journal".to_string()),
            ]
        );
    }

    #[test]
    fn bad_signature_is_rejected() {
        let mut d = build_dlg();
        d[0] = b'X';
        assert!(matches!(Dlg::parse(&d), Err(DlgError::BadSignature(_))));
    }
}
