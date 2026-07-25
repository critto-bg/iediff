//! ImprovedAnvil BAF preprocessor (`--templates`): `@@name` directive lines
//! expanded from a directory of template fragments before the source reaches
//! the compiler.
//!
//! The normative grammar is `baf-preprocessor-grammar.md` (syntax v2);
//! this mirrors the mod's own `lib/tpa/baf_preprocessor.tpa`, and the two MUST
//! produce byte-identical expansions.
//!
//! A directive is a whole line whose first non-blank characters are `@@`. It
//! names a template and carries optional `key=value` args — on the one line, or
//! spread over several `@@name(` … `)` lines. Args fill the template's `%key%`
//! *slot lines* (a whole line that is only `%key%`, repeated once per value and
//! deleted when the key is absent) and its inline `%key%` placeholders. The
//! spliced text is inserted verbatim and never rescanned; after every directive
//! is expanded, a leftover `@@` line or `%token%` anywhere is a fatal error.

use std::fmt;
use std::path::{Path, PathBuf};

/// Template names the grammar forbids in a call.
const RESERVED: [&str; 2] = ["foreach", "end"];

/// The `key=value` pairs of one directive, in call order. Duplicate keys
/// accumulate their values (a key may fill a multi-value slot); `used` drives
/// the "provided but never referenced" check.
struct Args {
    entries: Vec<Entry>,
}

impl Args {
    fn new() -> Args {
        Args {
            entries: Vec::new(),
        }
    }

    fn push(&mut self, key: String, value: Vec<u8>) {
        match self.entries.iter_mut().find(|e| e.key == key) {
            Some(entry) => entry.values.push(value),
            None => self.entries.push(Entry {
                key,
                values: vec![value],
                used: false,
            }),
        }
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }

    fn first_unused(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| !e.used)
            .map(|e| e.key.as_str())
    }
}

struct Entry {
    key: String,
    values: Vec<Vec<u8>>,
    used: bool,
}

/// A preprocessing failure, naming the source `.baf` and the offending
/// construct. All are fatal — a mod that ships a bad directive must not compile.
#[derive(Debug)]
pub struct PreprocessError {
    source: PathBuf,
    kind: ErrorKind,
}

impl fmt::Display for PreprocessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "preprocessing {}: {}", self.source.display(), self.kind)
    }
}

impl std::error::Error for PreprocessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::UnknownTemplate { io, .. } => Some(io),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum ErrorKind {
    UnknownTemplate {
        name: String,
        path: PathBuf,
        io: std::io::Error,
    },
    Reserved {
        name: String,
    },
    Malformed {
        line: String,
    },
    Unterminated {
        name: String,
    },
    EmptyValue {
        name: String,
        key: String,
    },
    InlineMultiValue {
        name: String,
        key: String,
    },
    UnusedKey {
        name: String,
        key: String,
    },
    ResidualDirective {
        line: String,
    },
    ResidualPlaceholder {
        token: String,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnknownTemplate { name, path, io } => {
                write!(
                    f,
                    "unknown template @@{name}: cannot read {}: {io}",
                    path.display()
                )
            }
            ErrorKind::Reserved { name } => write!(f, "reserved directive name @@{name}"),
            ErrorKind::Malformed { line } => write!(f, "malformed directive: {line}"),
            ErrorKind::Unterminated { name } => {
                write!(f, "unterminated multi-line call @@{name}")
            }
            ErrorKind::EmptyValue { name, key } => {
                write!(f, "@@{name}: empty value for key '{key}'")
            }
            ErrorKind::InlineMultiValue { name, key } => {
                write!(
                    f,
                    "@@{name}: multi-value key '{key}' used as an inline placeholder"
                )
            }
            ErrorKind::UnusedKey { name, key } => {
                write!(f, "@@{name}: key '{key}' is not used by the template")
            }
            ErrorKind::ResidualDirective { line } => {
                write!(f, "unexpanded directive remains after expansion: {line}")
            }
            ErrorKind::ResidualPlaceholder { token } => {
                write!(f, "unresolved placeholder remains after expansion: {token}")
            }
        }
    }
}

/// Expand every `@@` directive line in `source` against `templates_dir`,
/// returning the flattened BAF to compile. `source_path` only names the file in
/// error messages.
pub fn expand(
    source: &[u8],
    templates_dir: &Path,
    source_path: &Path,
) -> Result<Vec<u8>, PreprocessError> {
    expand_inner(source, templates_dir).map_err(|kind| PreprocessError {
        source: source_path.to_owned(),
        kind,
    })
}

fn expand_inner(source: &[u8], templates_dir: &Path) -> Result<Vec<u8>, ErrorKind> {
    let lines = physical_lines(source);
    let mut out = Vec::with_capacity(source.len());
    let mut i = 0;

    while let Some(&line) = lines.get(i) {
        let (content, newline) = split_newline(line);
        let stripped = strip_leading_ws(content);
        if !stripped.starts_with(b"@@") {
            out.extend_from_slice(line);
            i += 1;
            continue;
        }

        // A directive replaces its whole line; its own indentation is discarded
        // but its trailing newline is kept, so neighbouring blocks stay apart.
        match classify(stripped)? {
            Directive::Bare(name) => {
                out.extend_from_slice(&expand_call(name, Args::new(), templates_dir)?);
                out.extend_from_slice(newline);
                i += 1;
            }

            Directive::OneLine(name, inner) => {
                let args = parse_args(split_pairs(inner), name)?;
                out.extend_from_slice(&expand_call(name, args, templates_dir)?);
                out.extend_from_slice(newline);
                i += 1;
            }

            Directive::MultiOpen(name) => {
                let (segments, close) = gather_multiline(&lines, i + 1, name)?;
                let args = parse_args(segments, name)?;
                out.extend_from_slice(&expand_call(name, args, templates_dir)?);
                out.extend_from_slice(split_newline(lines[close]).1);
                i = close + 1;
            }
        }
    }

    check_postconditions(&out)?;
    Ok(out)
}

/// Classify a directive line (already stripped of leading blanks and its
/// newline). `name` and the arg text borrow the line.
fn classify(stripped: &[u8]) -> Result<Directive<'_>, ErrorKind> {
    let after = stripped.get(2..).unwrap_or_default();
    let name_len = after.iter().take_while(|b| is_name(b)).count();
    let (name, rest) = after.split_at(name_len);
    let name = match (name_len, std::str::from_utf8(name)) {
        (0, _) | (_, Err(_)) => return Err(malformed(stripped)),
        (_, Ok(name)) => name,
    };

    match rest {
        b"" => Ok(Directive::Bare(name)),
        b"(" => Ok(Directive::MultiOpen(name)),
        // A one-line call closes `)` as the final character; empty `()` is not a
        // no-args call but malformed (the bare form omits the parens entirely).
        _ if rest.first() == Some(&b'(') && rest.last() == Some(&b')') && rest.len() > 2 => Ok(
            Directive::OneLine(name, rest.get(1..rest.len() - 1).unwrap_or_default()),
        ),
        _ => Err(malformed(stripped)),
    }
}

enum Directive<'a> {
    Bare(&'a str),
    OneLine(&'a str, &'a [u8]),
    MultiOpen(&'a str),
}

/// Collect the `key=value;` argument lines of a multi-line call, starting at
/// `start`, up to the lone `)` line whose index is returned. Every arg line must
/// end with `;` and there must be at least one.
fn gather_multiline<'a>(
    lines: &[&'a [u8]],
    start: usize,
    name: &str,
) -> Result<(Vec<&'a [u8]>, usize), ErrorKind> {
    let mut segments = Vec::new();
    let mut j = start;
    loop {
        let line = lines.get(j).ok_or_else(|| ErrorKind::Unterminated {
            name: name.to_owned(),
        })?;
        let stripped = strip_leading_ws(split_newline(line).0);
        if stripped == b")" {
            if segments.is_empty() {
                return Err(malformed(stripped));
            }
            return Ok((segments, j));
        }

        // An arg line is `pair ;`; the value cannot contain `;`, so the final
        // byte is the separator. Anything else in the span is a fatal error.
        match stripped.split_last() {
            Some((&b';', pair)) => segments.push(pair),
            _ => return Err(malformed(stripped)),
        }

        j += 1;
    }
}

/// Parse pair segments (`key = value`, blanks around key and value trimmed) into
/// ordered args. `name` only labels errors.
fn parse_args(segments: Vec<&[u8]>, name: &str) -> Result<Args, ErrorKind> {
    let mut args = Args::new();
    for segment in segments {
        let segment = strip_leading_ws(segment);
        let key_len = segment.iter().take_while(|b| is_name(b)).count();
        let after = strip_leading_ws(segment.get(key_len..).unwrap_or_default());

        let key = match (
            key_len,
            after.split_first(),
            std::str::from_utf8(&segment[..key_len]),
        ) {
            (0, _, _) | (_, None, _) | (_, _, Err(_)) => return Err(malformed(segment)),
            (_, Some((&b'=', _)), Ok(key)) => key,
            (_, Some(_), _) => return Err(malformed(segment)),
        };

        let value = trim_ws(after.get(1..).unwrap_or_default());
        if value.is_empty() {
            return Err(ErrorKind::EmptyValue {
                name: name.to_owned(),
                key: key.to_owned(),
            });
        }

        args.push(key.to_owned(), value.to_vec());
    }

    Ok(args)
}

/// Load the template for one directive and apply its args: slot lines, then
/// inline placeholders, then the unused-key check. Returns the replacement text
/// (no trailing newline — the directive line contributes its own).
fn expand_call(name: &str, mut args: Args, templates_dir: &Path) -> Result<Vec<u8>, ErrorKind> {
    if RESERVED.contains(&name) {
        return Err(ErrorKind::Reserved {
            name: name.to_owned(),
        });
    }

    let path = templates_dir.join(name).with_extension("baf");
    let template = std::fs::read(&path).map_err(|io| ErrorKind::UnknownTemplate {
        name: name.to_owned(),
        path,
        io,
    })?;

    // A template ends with exactly one LF; the splice text drops it so the
    // directive line's own newline is the single separator that survives.
    let splice = template.strip_suffix(b"\n").unwrap_or(&template);
    let after_slots = expand_slots(splice, &mut args);
    let expanded = expand_inline(&after_slots, &mut args, name)?;

    match args.first_unused() {
        Some(key) => Err(ErrorKind::UnusedKey {
            name: name.to_owned(),
            key: key.to_owned(),
        }),
        None => Ok(expanded),
    }
}

/// Replace each slot line (`<ws>%key%` alone) with one line per value at the
/// slot's indentation, or delete it when the key is absent. Non-slot lines pass
/// through unchanged.
fn expand_slots(splice: &[u8], args: &mut Args) -> Vec<u8> {
    let mut out = Vec::with_capacity(splice.len());
    let mut first = true;

    for line in splice.split(|&b| b == b'\n') {
        match slot_key(line) {
            Some((indent_len, key)) => {
                if let Some(idx) = args.index_of(key) {
                    args.entries[idx].used = true;
                    let indent = line.get(..indent_len).unwrap_or_default().to_vec();
                    for value in &args.entries[idx].values {
                        if !first {
                            out.push(b'\n');
                        }
                        first = false;
                        out.extend_from_slice(&indent);
                        out.extend_from_slice(value);
                    }
                }
                // Absent key: emit nothing, so the line and its separator vanish.
            }

            None => {
                if !first {
                    out.push(b'\n');
                }
                first = false;
                out.extend_from_slice(line);
            }
        }
    }

    out
}

/// Replace each remaining `%key%` with its single value. A provided key is
/// marked used; an unprovided one is left for the postcondition to reject; a
/// multi-value key used inline is a fatal error.
fn expand_inline(buf: &[u8], args: &mut Args, name: &str) -> Result<Vec<u8>, ErrorKind> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while let Some(&b) = buf.get(i) {
        if let Some((key, end)) = placeholder_at(buf, i) {
            match args.index_of(key) {
                Some(idx) if args.entries[idx].values.len() > 1 => {
                    return Err(ErrorKind::InlineMultiValue {
                        name: name.to_owned(),
                        key: key.to_owned(),
                    });
                }

                Some(idx) => {
                    args.entries[idx].used = true;
                    out.extend_from_slice(&args.entries[idx].values[0]);
                }

                None => out.extend_from_slice(buf.get(i..end).unwrap_or_default()),
            }
            i = end;
        } else {
            out.push(b);
            i += 1;
        }
    }

    Ok(out)
}

/// Reject the two whole-file leftovers the grammar forbids: a line that still
/// opens with `@@` (unexpanded/injected directive) or any `%token%` (unresolved
/// placeholder, including a missing required inline key).
fn check_postconditions(out: &[u8]) -> Result<(), ErrorKind> {
    for line in out.split(|&b| b == b'\n') {
        if strip_leading_ws(line).starts_with(b"@@") {
            return Err(ErrorKind::ResidualDirective { line: lossy(line) });
        }
    }

    let mut i = 0;
    while i < out.len() {
        match placeholder_at(out, i) {
            Some((_, end)) => {
                return Err(ErrorKind::ResidualPlaceholder {
                    token: lossy(out.get(i..end).unwrap_or_default()),
                });
            }
            None => i += 1,
        }
    }
    Ok(())
}

/// The `key` of a slot line — optional blanks then `%key%` filling the whole
/// line — with the length of its leading indentation. `None` if not a slot.
fn slot_key(line: &[u8]) -> Option<(usize, &str)> {
    let indent_len = line.iter().take_while(|b| is_ws(b)).count();
    let (key, end) = placeholder_at(line, indent_len)?;
    (end == line.len()).then_some((indent_len, key))
}

/// If a `%key%` placeholder starts at `at`, its key and the index just past its
/// closing `%`. `key` is `[a-z0-9_]+`.
fn placeholder_at(buf: &[u8], at: usize) -> Option<(&str, usize)> {
    if buf.get(at) != Some(&b'%') {
        return None;
    }

    let name_start = at + 1;
    let name_len = buf
        .get(name_start..)?
        .iter()
        .take_while(|b| is_name(b))
        .count();
    if name_len == 0 || buf.get(name_start + name_len) != Some(&b'%') {
        return None;
    }

    let key = std::str::from_utf8(buf.get(name_start..name_start + name_len)?).ok()?;
    Some((key, name_start + name_len + 1))
}

/// Split a buffer into physical lines, each keeping its trailing `\n`. The final
/// line has none when the buffer does not end in `\n`.
fn physical_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;

    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            lines.push(buf.get(start..=i).unwrap_or_default());
            start = i + 1;
        }
    }

    if let Some(tail) = buf.get(start..).filter(|t| !t.is_empty()) {
        lines.push(tail);
    }

    lines
}

/// Split a physical line into its content and its terminator (`b"\n"` or empty).
fn split_newline(line: &[u8]) -> (&[u8], &[u8]) {
    match line.strip_suffix(b"\n") {
        Some(content) => (content, line.get(content.len()..).unwrap_or_default()),
        None => (line, b""),
    }
}

/// Split one-line args (`inner` between the parens) into pair segments on `;`;
/// values never contain `;`, so this is exact.
fn split_pairs(inner: &[u8]) -> Vec<&[u8]> {
    inner.split(|&b| b == b';').collect()
}

fn strip_leading_ws(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|b| is_ws(b)).count();
    s.get(n..).unwrap_or_default()
}

fn trim_ws(s: &[u8]) -> &[u8] {
    let start = s.iter().take_while(|b| is_ws(b)).count();
    let end = s.len() - s.iter().rev().take_while(|b| is_ws(b)).count();
    s.get(start..end.max(start)).unwrap_or_default()
}

fn is_ws(b: &u8) -> bool {
    *b == b' ' || *b == b'\t'
}

fn is_name(b: &u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_'
}

fn malformed(line: &[u8]) -> ErrorKind {
    ErrorKind::Malformed { line: lossy(line) }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A templates directory holding `<name>.baf` fragments, unique per call so
    /// concurrent tests never collide. Removed when the test's binding drops.
    struct Templates {
        dir: PathBuf,
    }

    impl Templates {
        fn with(fragments: &[(&str, &[u8])]) -> Templates {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("iediff-pp-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create templates dir");
            for (name, body) in fragments {
                fs::write(dir.join(name).with_extension("baf"), body).expect("write fragment");
            }
            Templates { dir }
        }

        fn expand(&self, source: &[u8]) -> Result<Vec<u8>, PreprocessError> {
            super::expand(source, &self.dir, Path::new("test.baf"))
        }
    }

    impl Drop for Templates {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn bare_marker_splices_the_fragment_less_one_newline() {
        let t = Templates::with(&[("vocalize", b"IF\n  True()\nEND\n")]);
        let out = t.expand(b"A\n\n@@vocalize\n\nB\n").expect("expands");
        // The fragment's own trailing newline is dropped, the marker line's kept:
        // exactly one blank line survives on either side.
        assert_eq!(out, b"A\n\nIF\n  True()\nEND\n\nB\n");
    }

    #[test]
    fn an_indented_directive_expands_but_a_mid_line_at_at_does_not() {
        // v2: leading blanks are discarded, so an indented marker is a directive.
        // A mid-line `@@` (first non-blank is `D`) is left for the compiler.
        let t = Templates::with(&[("x", b"Y\n")]);
        assert_eq!(t.expand(b"\t@@x\n").expect("indented expands"), b"Y\n");
        assert_eq!(t.expand(b"Do(@@x)\n").expect("inline left"), b"Do(@@x)\n");
    }

    #[test]
    fn a_slot_repeats_per_value_and_vanishes_when_absent() {
        let t = Templates::with(&[("t", b"IF\n  %guard%\n  Global()\nEND\n")]);
        // Two values -> two indented lines.
        assert_eq!(
            t.expand(b"@@t(guard=A; guard=B)\n").expect("multi"),
            b"IF\n  A\n  B\n  Global()\nEND\n"
        );
        // Absent key -> the slot line and its newline are gone.
        assert_eq!(
            t.expand(b"@@t\n").expect("absent"),
            b"IF\n  Global()\nEND\n"
        );
    }

    #[test]
    fn multi_line_call_matches_the_one_line_form() {
        let t = Templates::with(&[("t", b"%a%\n%b%\n")]);
        let multi = t.expand(b"@@t(\n  a=1;\n  b=2;\n)\n").expect("multi-line");
        let one = t.expand(b"@@t(a=1; b=2)\n").expect("one-line");
        assert_eq!(multi, one);
        assert_eq!(multi, b"1\n2\n");
    }

    #[test]
    fn an_inline_placeholder_takes_a_single_value() {
        let t = Templates::with(&[("t", b"Cast(%who%)\n")]);
        assert_eq!(
            t.expand(b"@@t(who=Myself)\n").expect("inline"),
            b"Cast(Myself)\n"
        );
    }

    #[test]
    fn errors_name_the_offending_construct() {
        let t = Templates::with(&[
            ("slot", b"%x%\n"),
            ("inline", b"Cast(%x%)\n"),
            ("none", b"IF\nEND\n"),
        ]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        assert!(kind(t.expand(b"@@missing\n")).contains("unknown template @@missing"));
        assert!(kind(t.expand(b"@@foreach\n")).contains("reserved directive name @@foreach"));
        assert!(kind(t.expand(b"@@slot()\n")).contains("malformed directive"));
        assert!(kind(t.expand(b"@@slot(x=)\n")).contains("empty value for key 'x'"));
        assert!(kind(t.expand(b"@@none(x=1)\n")).contains("key 'x' is not used"));
        assert!(kind(t.expand(b"@@inline(x=1; x=2)\n")).contains("multi-value key 'x'"));
        // A required inline key left unprovided surfaces as a residual placeholder.
        assert!(kind(t.expand(b"@@inline\n")).contains("unresolved placeholder"));
        assert!(kind(t.expand(b"@@t(\n  a=1;\n")).contains("unterminated multi-line call @@t"));
    }

    #[test]
    fn a_template_injected_directive_is_rejected() {
        let t = Templates::with(&[("bad", b"@@other\n")]);
        let err = format!("{}", t.expand(b"@@bad\n").expect_err("fatal").kind);
        assert!(err.contains("unexpanded directive remains"), "{err}");
    }
}
