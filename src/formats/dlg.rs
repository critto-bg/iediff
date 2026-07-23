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

use crate::{Reader, ResrefUse, StrrefUse, Truncated, describe_tag};

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

/// Transition flag bits that gate its optional fields.
const HAS_TEXT: u32 = 1 << 0;
const HAS_TRIGGER: u32 = 1 << 1;
const HAS_ACTION: u32 = 1 << 2;
const TERMINATES: u32 = 1 << 3;
const HAS_JOURNAL: u32 = 1 << 4;

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

    /// Every resource this dialogue names in a typed field: the DLG each
    /// transition hands off to. Resrefs named inside the trigger and action
    /// scripts are text, not typed fields, and are found by the soft scan.
    pub fn resref_uses(&self) -> Vec<ResrefUse<'_>> {
        let mut uses = Vec::new();
        for (i, t) in self.transitions.iter().enumerate() {
            uses.push(ResrefUse::new(
                DLG_TYPE,
                &t.next_dlg,
                format!("transition[{i}].next_dlg"),
            ));
        }
        uses.retain(|u| !u.value.is_empty());
        uses
    }

    /// The resources the trigger and action scripts name. Each slot is compiled
    /// and its calls' arguments typed exactly, so these are *hard* references —
    /// the same reading a compiled script gives, not a text heuristic. A slot
    /// the compiler cannot read (rare) contributes nothing rather than a guess.
    ///
    /// Owns its resrefs: they come from records this call builds and drops, so
    /// nothing in the DLG remains to borrow.
    pub fn script_references(&self, vocabulary: &crate::script::Vocabulary) -> Vec<ResrefUse<'_>> {
        use crate::script::Arg;
        let mut out = Vec::new();
        for (label, scripts) in self.script_tables() {
            for (i, script) in scripts.iter().enumerate() {
                self.for_script_args(script, label == "action", vocabulary, |arg| {
                    if let Arg::Resource {
                        kind,
                        resref,
                        param,
                    } = arg
                    {
                        out.push(ResrefUse::new(
                            *kind,
                            resref.clone().into_owned(),
                            format!("{label}[{i}].{param}"),
                        ));
                    }
                });
            }
        }
        out
    }

    /// The dialog strings the action scripts name, hard: each action whose call
    /// takes a typed strref argument (`DisplayString`, journal entries, …).
    pub fn script_strref_uses(&self, vocabulary: &crate::script::Vocabulary) -> Vec<StrrefUse> {
        use crate::script::Arg;
        let mut out = Vec::new();
        for (i, action) in self.actions.iter().enumerate() {
            self.for_script_args(action, true, vocabulary, |arg| {
                if let Arg::Strref { value, param } = arg {
                    out.push(StrrefUse::new(*value, format!("action[{i}].{param}")));
                }
            });
        }
        out
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

    /// The three script tables, tagged for the path.
    fn script_tables(&self) -> [(&'static str, &Vec<String>); 3] {
        [
            ("state_trigger", &self.state_triggers),
            ("trans_trigger", &self.trans_triggers),
            ("action", &self.actions),
        ]
    }

    /// Compile one script slot and visit every argument its calls name. Actions
    /// and triggers share the grammar but build different records.
    fn for_script_args(
        &self,
        script: &str,
        is_action: bool,
        vocabulary: &crate::script::Vocabulary,
        mut visit: impl FnMut(&crate::script::Arg<'_>),
    ) {
        use crate::formats::baf;
        if is_action {
            for action in baf::actions(script.as_bytes(), vocabulary).unwrap_or_default() {
                for arg in vocabulary.action_args(&action) {
                    visit(&arg);
                }
            }
        } else {
            for trigger in baf::triggers(script.as_bytes(), vocabulary).unwrap_or_default() {
                for arg in vocabulary.trigger_args(&trigger) {
                    visit(&arg);
                }
            }
        }
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
    use crate::formats::are::ARE_TYPE;
    use crate::formats::bcs::BCS_TYPE;
    use crate::formats::itm::ITM_TYPE;

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
    fn script_strref_uses_report_the_slot_and_call() {
        // The fixture action `EraseJournalEntry(103019)` names its strref by an
        // integer argument the call types.
        let dlg = Dlg::parse(&build_dlg()).expect("parse");
        let uses: Vec<(u32, String)> = dlg
            .script_strref_uses(&scripts_vocab())
            .into_iter()
            .map(|u| (u.value, u.path))
            .collect();
        assert_eq!(
            uses,
            vec![(103019, "action[0].EraseJournalEntry.STRREF".to_string())]
        );
    }

    #[test]
    fn resref_uses_report_each_transition_handoff() {
        // The fixture's two transitions have an empty next_dlg; name one.
        let mut data = build_dlg();
        let t1 = 0x34 + STATE_SIZE + TRANSITION_SIZE;
        data[t1 + 0x14..t1 + 0x18].copy_from_slice(b"NEXT");

        let dlg = Dlg::parse(&data).expect("parse");
        let uses = dlg.resref_uses();
        let got: Vec<(u16, &str, &str)> = uses
            .iter()
            .map(|u| (u.kind, u.value.as_ref(), u.path.as_str()))
            .collect();
        assert_eq!(got, vec![(DLG_TYPE, "NEXT", "transition[1].next_dlg")]);
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

    /// A DLG carrying one state trigger and one action, for the script scan.
    fn build_scripted_dlg(trigger: &str, action: &str) -> Dlg {
        let state_off = 0x34usize;
        let trans_off = state_off + STATE_SIZE;
        let strig_off = trans_off + TRANSITION_SIZE;
        let action_off = strig_off + STRING_ENTRY_SIZE;
        let text_off = action_off + STRING_ENTRY_SIZE;

        let mut d = vec![0u8; text_off];
        d[0x00..0x04].copy_from_slice(DLG_SIGNATURE);
        d[0x04..0x08].copy_from_slice(DLG_VERSION);
        let put = |d: &mut [u8], at: usize, v: u32| d[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut d, 0x08, 1); // states
        put(&mut d, 0x0c, state_off as u32);
        put(&mut d, 0x10, 1); // transitions
        put(&mut d, 0x14, trans_off as u32);
        put(&mut d, 0x18, strig_off as u32);
        put(&mut d, 0x1c, 1); // one state trigger
        put(&mut d, 0x20, text_off as u32);
        put(&mut d, 0x24, 0); // no transition triggers
        put(&mut d, 0x28, action_off as u32);
        put(&mut d, 0x2c, 1); // one action

        put(&mut d, strig_off, text_off as u32);
        put(&mut d, strig_off + 4, trigger.len() as u32);
        put(&mut d, action_off, (text_off + trigger.len()) as u32);
        put(&mut d, action_off + 4, action.len() as u32);
        d.extend_from_slice(trigger.as_bytes());
        d.extend_from_slice(action.as_bytes());
        Dlg::parse(&d).expect("parse scripted dlg")
    }

    #[test]
    fn script_references_attribute_a_resref_to_the_call_that_takes_it() {
        // The item belongs to the inner call, not to the ActionOverride that
        // wraps it. The path names the call and its parameter, from the parsed
        // record rather than a text position.
        let dlg = build_scripted_dlg(
            "PartyHasItem(\"SW1H01\")",
            "ActionOverride(Player1,GiveItemCreate(\"SW1H01\",Player1,0,0,0))",
        );
        let refs = paths(dlg.script_references(&scripts_vocab()), ITM_TYPE);
        assert_eq!(
            refs,
            vec![
                "state_trigger[0].PartyHasItem.Item".to_string(),
                "action[0].GiveItemCreate.ResRef".to_string(),
            ]
        );
    }

    #[test]
    fn script_references_survive_malformed_scripts() {
        // Scripts come from untrusted bytes and the compiler may reject a slot;
        // a rejected slot yields nothing rather than a panic.
        let vocab = scripts_vocab();
        for junk in [
            "",
            "((((",
            "))))",
            ",,,,",
            "HasItem(\"SW1H0",
            "NoSuchCall(\"x\")",
        ] {
            let dlg = build_scripted_dlg(junk, junk);
            let _ = dlg.script_references(&vocab);
            let _ = dlg.script_strref_uses(&vocab);
        }
    }

    /// The calls the DLG script tests reference, typed from IDS lines, with the
    /// one object symbol their scripts name.
    fn scripts_vocab() -> crate::script::Vocabulary {
        let mut vocab = crate::script::from_lines(
            &[
                "15 GiveItemCreate(S:ResRef*,O:Object*,I:Usage1*,I:Usage2*,I:Usage3*)",
                "30 SetGlobal(S:Name*,S:Area*,I:Value*)",
                "263 EraseJournalEntry(I:STRREF*)",
            ],
            &["70 AreaCheck(S:ResRef*)", "80 PartyHasItem(S:Item*)"],
        );
        vocab.set_symbols("OBJECT", [(21, "Player1".to_string())].into());
        vocab
    }

    /// The paths of a DLG's script references naming `kind`.
    fn paths(uses: Vec<ResrefUse<'_>>, kind: u16) -> Vec<String> {
        uses.into_iter()
            .filter(|u| u.kind == kind)
            .map(|u| u.path)
            .collect()
    }

    #[test]
    fn script_references_drop_calls_that_name_another_type() {
        // `AR0602` exists in the real game as both an area and a script, which
        // is exactly the collision the call typing resolves.
        let dlg = build_scripted_dlg("AreaCheck(\"AR0602\")", "SetGlobal(\"done\",\"AR0602\",1)");
        let vocab = scripts_vocab();

        // As an area: AreaCheck names it; SetGlobal does not, its second
        // argument being a variable scope rather than an area file.
        assert_eq!(
            paths(dlg.script_references(&vocab), ARE_TYPE),
            vec!["state_trigger[0].AreaCheck.ResRef".to_string()]
        );
        // As a script: nothing, because AreaCheck names an area.
        assert!(
            paths(dlg.script_references(&vocab), BCS_TYPE).is_empty(),
            "AreaCheck names an ARE and must not answer a BCS query"
        );
    }

    #[test]
    fn script_references_omit_calls_the_vocabulary_cannot_type() {
        // Unlike the old text heuristic, an unlisted call yields no reference at
        // all — the hard scan reports only what it can type, never a guess.
        let dlg = build_scripted_dlg("MysteryTrigger(\"AR0602\")", "NoSuchAction(\"AR0602\")");
        let vocab = scripts_vocab();
        for kind in [ARE_TYPE, BCS_TYPE, ITM_TYPE] {
            assert!(paths(dlg.script_references(&vocab), kind).is_empty());
        }
    }
}
