//! ImprovedAnvil BAF preprocessor (`--templates`): `@@name` directive lines
//! expanded from a directory of template fragments before the source reaches
//! the compiler.
//!
//! The normative grammar is `baf-preprocessor-grammar.md` (syntax v4). This
//! mirrors the mod's own `lib/tpa/baf_preprocessor.tpa`, and the two MUST
//! produce byte-identical expansions.
//!
//! A directive is a whole line whose first non-blank characters are `@@`. It
//! names a template and carries optional `key=value` args, on the one line or
//! spread over several `@@name(` … `)` lines. Args fill the template's `%key%`
//! placeholders, inline or as *slot lines*. A slot line is a whole line that is
//! only `%key%`. It repeats once per value, and vanishes when the key is absent.
//! The spliced text goes in verbatim, and nothing rescans it. Once every directive
//! has expanded, a leftover `@@` line or `%token%` anywhere is a fatal error.
//!
//! A template body, or a source, may wrap a run of lines in `@@foreach(key)` …
//! `@@end`. The block repeats once per value of `key`, blank-line separated,
//! with `%key%` bound to that one value. A template's region draws its values
//! from the call. A source has no call, so its values come from a definition in
//! scope. A source region is stamped before any call expands, so a directive
//! line inside one repeats first and expands after, once per instance.
//!
//! `@@define key(v1; v2; …)` binds a key at file scope. Any call whose template
//! references `key` but whose args omit it draws the definition's values, as if
//! the call had passed them. `@@use(name)` imports the same kind of definitions
//! from a shared defs file (`<templates-dir>/defs/name.baf`), which holds
//! nothing else. Both forms are position-independent, so a first pass collects
//! them and drops their lines. After that an imported definition matches an
//! in-file one, bar the origin kept for error messages.
//!
//! One call may opt out of a definition with the pair `key=@none`. That call
//! alone loses the key, as if nothing had provided it, so its slot lines vanish.
//! This is an opt-out, not an override — no form replaces a definition's values.
//!
//! A template may in turn open with a block of `@@default key(…)` lines naming
//! its own fallbacks. One precedence follows: the call's args, else a
//! definition, else the template's default. Unlike a definition, a default no
//! call ever reaches is not an error — the corpus decides whether it is needed,
//! not any one file.

use std::fmt;
use std::path::{Path, PathBuf};

/// Template names the grammar forbids in a call. `define`/`use`/`default` head
/// their own directives. `foreach`/`end` delimit iteration regions.
const RESERVED: [&str; 5] = ["foreach", "end", "define", "use", "default"];

/// The arg value that suppresses a key. No definition may hold it as a value, so
/// the two readings never collide.
const SUPPRESS: &[u8] = b"@none";

/// The `key=value` pairs of one directive, in call order. Duplicate keys
/// accumulate their values, since one key may fill a multi-value slot. `used`
/// drives the "provided but never referenced" check.
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
                suppressed: false,
            }),
        }
    }

    /// Record an `@none` pair. The entry holds no value. It exists only to drop
    /// the key from this call: [`resolve`] stops at it, so the definition it
    /// opts out of never reaches the template.
    fn suppress(&mut self, key: String) {
        self.entries.push(Entry {
            key,
            values: Vec::new(),
            used: false,
            suppressed: true,
        });
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }

    fn is_suppressed(&self, key: &str) -> bool {
        self.entries.iter().any(|e| e.key == key && e.suppressed)
    }

    fn first_unused(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| !e.used)
    }
}

struct Entry {
    key: String,
    values: Vec<Vec<u8>>,
    used: bool,
    suppressed: bool,
}

/// The file's definition bindings — a fallback value source for calls that omit
/// a key. In-file `@@define`s and `@@use` imports feed it alike. Drawing on a
/// definition marks it `used`. One no call or region ever draws on is a fatal
/// error, so a stale definition never passes silently.
struct Definitions {
    entries: Vec<Def>,
}

impl Definitions {
    fn new() -> Definitions {
        Definitions {
            entries: Vec::new(),
        }
    }

    /// Record a definition, rejecting a second one for the same key. `from`
    /// names the defs file it came from, or is `None` for an in-file
    /// `@@define`, and labels the collision message. Every in-file insert
    /// precedes the imports, so the older entry is never the more remote one.
    fn insert(
        &mut self,
        key: String,
        values: Vec<Vec<u8>>,
        from: Option<String>,
    ) -> Result<(), ErrorKind> {
        if let Some(prior) = self.entries.iter().find(|d| d.key == key) {
            return Err(match (&prior.from, &from) {
                (None, None) => ErrorKind::DuplicateDefine { key },
                (first, second) => ErrorKind::DefineConflict {
                    key,
                    first: origin(first),
                    second: origin(second),
                },
            });
        }
        self.entries.push(Def {
            key,
            values,
            used: false,
            from,
        });
        Ok(())
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|d| d.key == key)
    }

    /// The values bound to `key`, marking the definition consumed.
    fn consume(&mut self, key: &str) -> Option<&[Vec<u8>]> {
        let idx = self.index_of(key)?;
        let def = self.entries.get_mut(idx)?;
        def.used = true;
        Some(&def.values)
    }

    fn first_unused(&self) -> Option<&Def> {
        self.entries.iter().find(|d| !d.used)
    }
}

struct Def {
    key: String,
    values: Vec<Vec<u8>>,
    used: bool,
    from: Option<String>,
}

/// One `@@default` from a template's opening block: the values a call uses when
/// neither its args nor a definition provide the key. It carries no `used` flag,
/// because a default no call reaches is not an error, unlike a definition.
struct Binding {
    key: String,
    values: Vec<Vec<u8>>,
}

/// One `@@foreach` … `@@end` span: the key it iterates and the line indices of
/// its two delimiters.
struct Region {
    key: String,
    open: usize,
    close: usize,
}

/// Which file a region error is about. A template names itself in the message.
/// A source needs no prefix, since the enclosing [`PreprocessError`] names it.
#[derive(Clone, Debug)]
enum Site {
    Template(String),
    Source,
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Site::Template(name) => write!(f, "@@{name}: "),
            Site::Source => Ok(()),
        }
    }
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
            ErrorKind::UnknownTemplate { io, .. } | ErrorKind::MissingDefsFile { io, .. } => {
                Some(io)
            }
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
    MissingDefsFile {
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
    DuplicateDefine {
        key: String,
    },
    DefineConflict {
        key: String,
        first: String,
        second: String,
    },
    UnterminatedBinding {
        head: &'static str,
        key: String,
    },
    DefineCollision {
        name: String,
        key: String,
        from: Option<String>,
    },
    DefineUnused {
        key: String,
        from: Option<String>,
    },
    DuplicateUse {
        name: String,
    },
    NonDefineContent {
        name: String,
        line: String,
    },
    EmptyDefsFile {
        name: String,
    },
    ReservedValue {
        head: &'static str,
        key: String,
    },
    SuppressMixed {
        name: String,
        key: String,
    },
    SuppressRepeated {
        name: String,
        key: String,
    },
    SuppressUnbound {
        name: String,
        key: String,
    },
    SuppressInline {
        name: String,
        key: String,
    },
    SuppressIterated {
        name: String,
        key: String,
    },
    SuppressUnreferenced {
        name: String,
        key: String,
    },
    DefaultInSource {
        line: String,
    },
    DefaultMisplaced {
        name: String,
        line: String,
    },
    DuplicateDefault {
        name: String,
        key: String,
    },
    DefaultUnreferenced {
        name: String,
        key: String,
    },
    MalformedForeach {
        site: Site,
        line: String,
    },
    NestedRegion {
        site: Site,
        key: String,
    },
    UnterminatedRegion {
        site: Site,
        key: String,
    },
    UnopenedRegion {
        site: Site,
    },
    EmptyRegion {
        site: Site,
        key: String,
    },
    RegionKeyAbsent {
        name: String,
        key: String,
    },
    IteratedOutsideRegion {
        name: String,
        key: String,
    },
    RegionKeyUndefined {
        key: String,
    },
    DirectiveInRegion {
        line: String,
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
            ErrorKind::MissingDefsFile { name, path, io } => {
                write!(f, "@@use({name}): cannot read {}: {io}", path.display())
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
            ErrorKind::DuplicateDefine { key } => {
                write!(f, "duplicate @@define for key '{key}'")
            }
            ErrorKind::DefineConflict { key, first, second } => {
                write!(f, "key '{key}' is defined by both {first} and {second}")
            }
            ErrorKind::UnterminatedBinding { head, key } => {
                write!(f, "unterminated multi-line @@{head} {key}")
            }
            ErrorKind::DefineCollision { name, key, from } => {
                write!(
                    f,
                    "@@{name}: key '{key}' is also set by {}; provide it in one place only",
                    origin(from)
                )
            }
            ErrorKind::DefineUnused { key, from } => {
                write!(f, "{} {key} is used by no call", origin(from))
            }
            ErrorKind::DuplicateUse { name } => {
                write!(f, "duplicate @@use({name})")
            }
            ErrorKind::NonDefineContent { name, line } => {
                write!(f, "defs file {name}: non-define content: {line}")
            }
            ErrorKind::EmptyDefsFile { name } => {
                write!(f, "@@use({name}): defs file defines nothing")
            }
            ErrorKind::ReservedValue { head, key } => {
                write!(f, "@@{head} {key}: '@none' is reserved as a value")
            }
            ErrorKind::SuppressMixed { name, key } => {
                write!(f, "@@{name}: key '{key}' is given both a value and @none")
            }
            ErrorKind::SuppressRepeated { name, key } => {
                write!(f, "@@{name}: repeated @none for key '{key}'")
            }
            ErrorKind::SuppressUnbound { name, key } => {
                write!(
                    f,
                    "@@{name}: @none for key '{key}', which has no definition or default to suppress"
                )
            }
            ErrorKind::SuppressInline { name, key } => {
                write!(f, "@@{name}: suppressed key '{key}' is required inline")
            }
            ErrorKind::SuppressIterated { name, key } => {
                write!(
                    f,
                    "@@{name}: suppressed key '{key}' is required by @@foreach"
                )
            }
            ErrorKind::SuppressUnreferenced { name, key } => {
                write!(f, "@@{name}: @none key '{key}' is not referenced by a slot")
            }
            ErrorKind::DefaultInSource { line } => {
                write!(f, "@@default is valid only in a template: {line}")
            }
            ErrorKind::DefaultMisplaced { name, line } => {
                write!(f, "@@{name}: @@default outside the opening block: {line}")
            }
            ErrorKind::DuplicateDefault { name, key } => {
                write!(f, "@@{name}: duplicate @@default for key '{key}'")
            }
            ErrorKind::DefaultUnreferenced { name, key } => {
                write!(
                    f,
                    "@@{name}: @@default key '{key}' is not referenced by the template"
                )
            }
            ErrorKind::MalformedForeach { site, line } => {
                write!(f, "{site}malformed @@foreach line: {line}")
            }
            ErrorKind::NestedRegion { site, key } => {
                write!(f, "{site}nested @@foreach region inside @@foreach({key})")
            }
            ErrorKind::UnterminatedRegion { site, key } => {
                write!(f, "{site}unterminated @@foreach({key}) region")
            }
            ErrorKind::UnopenedRegion { site } => {
                write!(f, "{site}@@end without @@foreach")
            }
            ErrorKind::EmptyRegion { site, key } => {
                write!(f, "{site}empty @@foreach({key}) region")
            }
            ErrorKind::RegionKeyAbsent { name, key } => {
                write!(f, "@@{name}: @@foreach key '{key}' is not provided")
            }
            ErrorKind::IteratedOutsideRegion { name, key } => {
                write!(
                    f,
                    "@@{name}: iterated key '{key}' referenced outside its @@foreach region"
                )
            }
            ErrorKind::RegionKeyUndefined { key } => {
                write!(f, "@@foreach key '{key}' has no definition in scope")
            }
            ErrorKind::DirectiveInRegion { line } => {
                write!(
                    f,
                    "@@define/@@use/@@default inside a @@foreach region: {line}"
                )
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
    guard_regions(&lines)?;

    // Pass 1: collect every `@@define` and `@@use` (both position-independent
    // and file-scoped) and mark their lines so pass 2 drops them. It reads the
    // imports last, so a key clash reports the import as the newcomer.
    let mut defs = Definitions::new();
    let mut uses: Vec<String> = Vec::new();
    let mut consumed = vec![false; lines.len()];
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let stripped = strip_leading_ws(split_newline(line).0);
        if is_binding(stripped, b"default") {
            // A default belongs to a template's opening block. In a source it
            // can only be a stray. (The keyless `@@default(…)` is an ordinary
            // call, which the reserved-name check catches.)
            return Err(ErrorKind::DefaultInSource {
                line: lossy(stripped),
            });
        } else if is_directive(stripped, b"define") {
            let (key, values, end) = parse_binding(&lines, i, "define")?;
            defs.insert(key, values, None)?;
            for slot in consumed.get_mut(i..=end).unwrap_or_default() {
                *slot = true;
            }
            i = end + 1;
        } else if let Some(name) = parse_paren_name(stripped, b"@@use(") {
            if uses.contains(&name) {
                return Err(ErrorKind::DuplicateUse { name });
            }
            uses.push(name);
            if let Some(slot) = consumed.get_mut(i) {
                *slot = true;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    for name in &uses {
        import_defs(templates_dir, name, &mut defs)?;
    }

    // Pass 2: stamp the source's own iteration regions over a buffer the
    // definition lines have already left — a region must repeat script text, not
    // them. Pass 3 takes the calls, so a directive inside a region is by then
    // one ordinary call per instance, its `%key%` already resolved.
    let mut staged = Vec::with_capacity(source.len());
    for (n, &line) in lines.iter().enumerate() {
        if !consumed.get(n).copied().unwrap_or(false) {
            staged.extend_from_slice(line);
        }
    }
    let staged = expand_source_regions(&staged, &mut defs)?;
    let lines = physical_lines(&staged);

    // Pass 3: expand each call (drawing on the definitions), pass the rest through.
    let mut out = Vec::with_capacity(staged.len());
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let (content, newline) = split_newline(line);
        let stripped = strip_leading_ws(content);
        if !stripped.starts_with(b"@@") {
            out.extend_from_slice(line);
            i += 1;
            continue;
        }

        // A directive replaces its whole line. The expansion drops the line's
        // indentation but keeps its newline, so neighboring blocks stay apart.
        match classify(stripped)? {
            Directive::Bare(name) => {
                out.extend_from_slice(&expand_call(name, Args::new(), &mut defs, templates_dir)?);
                out.extend_from_slice(newline);
                i += 1;
            }

            Directive::OneLine(name, inner) => {
                let args = parse_args(split_pairs(inner), name)?;
                out.extend_from_slice(&expand_call(name, args, &mut defs, templates_dir)?);
                out.extend_from_slice(newline);
                i += 1;
            }

            Directive::MultiOpen(name) => {
                let (segments, close) = gather_multiline(&lines, i + 1, name)?;
                let args = parse_args(segments, name)?;
                out.extend_from_slice(&expand_call(name, args, &mut defs, templates_dir)?);
                out.extend_from_slice(split_newline(lines[close]).1);
                i = close + 1;
            }
        }
    }

    if let Some(def) = defs.first_unused() {
        return Err(ErrorKind::DefineUnused {
            key: def.key.clone(),
            from: def.from.clone(),
        });
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
        // A one-line call ends with `)`. Empty `()` is malformed, not a no-args
        // call — the bare form omits the parens entirely.
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

/// Collect the `key=value;` argument lines of a multi-line call, from `start` to
/// the lone `)` line whose index this returns. Every arg line ends with `;`, and
/// the call needs at least one.
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

        // An arg line is `pair ;`. A value cannot contain `;`, so the final byte
        // is the separator. Anything else in the span is a fatal error.
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

        // Suppression is an opt-out, not an override. Within one call, `@none`
        // pairs with neither a value for the same key (in either order) nor a
        // second `@none`.
        let suppress = value == SUPPRESS;
        match args.entries.iter().find(|e| e.key == key) {
            Some(prior) if prior.suppressed && suppress => {
                return Err(ErrorKind::SuppressRepeated {
                    name: name.to_owned(),
                    key: key.to_owned(),
                });
            }
            Some(prior) if prior.suppressed || suppress => {
                return Err(ErrorKind::SuppressMixed {
                    name: name.to_owned(),
                    key: key.to_owned(),
                });
            }
            _ => {}
        }

        match suppress {
            true => args.suppress(key.to_owned()),
            false => args.push(key.to_owned(), value.to_vec()),
        }
    }

    Ok(args)
}

/// Whether a stripped line heads the directive `head`. The name must match in
/// full, so a template whose name merely starts with it (`@@definers`) stays an
/// ordinary call. A malformed head matches too, so the caller can report the
/// specific error over "malformed directive".
fn is_directive(stripped: &[u8], head: &[u8]) -> bool {
    let after = match stripped.strip_prefix(b"@@") {
        Some(after) => after,
        None => return false,
    };
    let name_len = after.iter().take_while(|b| is_name(b)).count();
    after.get(..name_len) == Some(head)
}

/// Whether a stripped line opens the spaced binding form `@@<head> key(…)`. The
/// space separates a `@@define`/`@@default` directive from a call of that name,
/// which carries its parens straight after the name.
fn is_binding(stripped: &[u8], head: &[u8]) -> bool {
    matches!(
        stripped
            .strip_prefix(b"@@")
            .and_then(|r| r.strip_prefix(head)),
        Some([b' ', ..])
    )
}

/// Parse the `@@<head> key(…)` binding at line `i`, the shape `@@define` and
/// `@@default` share. Returns the key, its values, and the last line index the
/// directive spans. That is its own line, or the closing `)` of the multi-line
/// form.
fn parse_binding(
    lines: &[&[u8]],
    i: usize,
    head: &'static str,
) -> Result<(String, Vec<Vec<u8>>, usize), ErrorKind> {
    let line = lines.get(i).copied().unwrap_or_default();
    let stripped = strip_leading_ws(split_newline(line).0);
    let rest = stripped
        .strip_prefix(b"@@")
        .and_then(|r| r.strip_prefix(head.as_bytes()))
        .unwrap_or_default();

    // Exactly one space, then the key, then `(`.
    let after_space = match rest.split_first() {
        Some((&b' ', tail)) => tail,
        _ => return Err(malformed(stripped)),
    };
    let key_len = after_space.iter().take_while(|b| is_name(b)).count();
    let key = match std::str::from_utf8(after_space.get(..key_len).unwrap_or_default()) {
        Ok(key) if key_len > 0 => key,
        _ => return Err(malformed(stripped)),
    };
    let inside = match after_space.get(key_len..).and_then(<[u8]>::split_first) {
        Some((&b'(', tail)) => tail,
        _ => return Err(malformed(stripped)),
    };

    // `(` last on the line opens the multi-line form. Otherwise `)` closes it here.
    if inside.is_empty() {
        let (values, end) = gather_binding_values(lines, i + 1, head, key)?;
        return Ok((key.to_owned(), values, end));
    }

    let content = match inside.split_last() {
        Some((&b')', content)) if !content.is_empty() => content,
        _ => return Err(malformed(stripped)),
    };
    let mut values = Vec::new();
    for segment in content.split(|&b| b == b';') {
        let value = trim_ws(segment);
        if value.is_empty() {
            return Err(malformed(stripped));
        }
        if value == SUPPRESS {
            return Err(ErrorKind::ReservedValue {
                head,
                key: key.to_owned(),
            });
        }
        values.push(value.to_vec());
    }
    Ok((key.to_owned(), values, i))
}

/// Collect the `value;` lines of a multi-line binding, from `start` to the lone
/// `)` line whose index this returns. Every value line ends with `;`, and the
/// binding needs at least one. Mirrors [`gather_multiline`].
fn gather_binding_values(
    lines: &[&[u8]],
    start: usize,
    head: &'static str,
    key: &str,
) -> Result<(Vec<Vec<u8>>, usize), ErrorKind> {
    let mut values = Vec::new();
    let mut j = start;
    loop {
        let line = lines.get(j).ok_or_else(|| ErrorKind::UnterminatedBinding {
            head,
            key: key.to_owned(),
        })?;
        let stripped = strip_leading_ws(split_newline(line).0);
        if stripped == b")" {
            if values.is_empty() {
                return Err(malformed(stripped));
            }
            return Ok((values, j));
        }

        match stripped.split_last() {
            Some((&b';', value)) => {
                let value = trim_ws(value);
                if value.is_empty() {
                    return Err(malformed(stripped));
                }
                if value == SUPPRESS {
                    return Err(ErrorKind::ReservedValue {
                        head,
                        key: key.to_owned(),
                    });
                }
                values.push(value.to_vec());
            }
            _ => return Err(malformed(stripped)),
        }

        j += 1;
    }
}

/// The lone name a directive line carries after `prefix`: `prefix` ws* name ws*
/// `)`, the shape `@@use(name)` and `@@foreach(key)` share. `None` if the line
/// does not match.
fn parse_paren_name(stripped: &[u8], prefix: &[u8]) -> Option<String> {
    let after = strip_leading_ws(stripped.strip_prefix(prefix)?);
    let name_len = after.iter().take_while(|b| is_name(b)).count();
    if name_len == 0 {
        return None;
    }
    let name = std::str::from_utf8(after.get(..name_len)?).ok()?;
    (strip_leading_ws(after.get(name_len..)?) == b")").then(|| name.to_owned())
}

/// Read `<templates-dir>/defs/<name>.baf` and add its definitions to `defs`
/// under the origin `name`. A defs file holds only definitions, blank lines, and
/// `//` comments. Any other content — a template call, a nested `@@use` — is a
/// fatal error, as is a file that defines nothing.
fn import_defs(templates_dir: &Path, name: &str, defs: &mut Definitions) -> Result<(), ErrorKind> {
    let path = templates_dir.join("defs").join(name).with_extension("baf");
    let content = std::fs::read(&path).map_err(|io| ErrorKind::MissingDefsFile {
        name: name.to_owned(),
        path,
        io,
    })?;

    let lines = physical_lines(&content);
    let mut count = 0;
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let stripped = strip_leading_ws(split_newline(line).0);
        if stripped.is_empty() || stripped.starts_with(b"//") {
            i += 1;
        } else if is_directive(stripped, b"define") {
            let (key, values, end) = parse_binding(&lines, i, "define")?;
            defs.insert(key, values, Some(name.to_owned()))?;
            count += 1;
            i = end + 1;
        } else {
            return Err(ErrorKind::NonDefineContent {
                name: name.to_owned(),
                line: lossy(stripped),
            });
        }
    }

    if count == 0 {
        return Err(ErrorKind::EmptyDefsFile {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// Load the template for one directive and apply its args: iteration regions,
/// then slot lines, then inline placeholders, then the unused-key check. Returns
/// the replacement text (no trailing newline — the directive line contributes
/// its own).
fn expand_call(
    name: &str,
    mut args: Args,
    defs: &mut Definitions,
    templates_dir: &Path,
) -> Result<Vec<u8>, ErrorKind> {
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

    // A template ends with exactly one LF. The splice text drops it, so the
    // directive line's own newline is the single separator that survives. The
    // read precedes the arg checks because a suppression pair's legality turns
    // on whether the template defaults the key.
    let splice = template.strip_suffix(b"\n").unwrap_or(&template);
    let (defaults, splice) = split_defaults(splice, name)?;

    for entry in &args.entries {
        match (entry.suppressed, defs.index_of(&entry.key)) {
            // A key comes from the call or a file definition, never both. (A
            // default is the exception the other way: overriding one is the point.)
            (false, Some(idx)) => {
                return Err(ErrorKind::DefineCollision {
                    name: name.to_owned(),
                    key: entry.key.clone(),
                    from: defs.entries.get(idx).and_then(|d| d.from.clone()),
                });
            }
            // Suppression needs something to opt out of: absence is already the
            // default, which would make the pair dead syntax.
            (true, None) if !defaults.iter().any(|b| b.key == entry.key) => {
                return Err(ErrorKind::SuppressUnbound {
                    name: name.to_owned(),
                    key: entry.key.clone(),
                });
            }
            _ => {}
        }
    }

    // Precedence runs args, then a definition, then the template's own default.
    // A default applies only where both higher sources are silent. A suppression
    // pair fills the args slot, so it blocks both lower sources at once.
    for binding in &defaults {
        if args.index_of(&binding.key).is_none() && defs.index_of(&binding.key).is_none() {
            for value in &binding.values {
                args.push(binding.key.clone(), value.clone());
            }
        }
    }

    let (after_regions, iterated) = expand_regions(splice, &mut args, defs, name)?;
    let after_slots = expand_slots(&after_regions, &mut args, defs, &iterated, name)?;
    let expanded = expand_inline(&after_slots, &mut args, defs, &iterated, name)?;

    // A suppressed key counts as used by the slot line it deletes. With no such
    // slot, the call opted out of something the template never asked for.
    match args.first_unused() {
        Some(entry) if entry.suppressed => Err(ErrorKind::SuppressUnreferenced {
            name: name.to_owned(),
            key: entry.key.clone(),
        }),
        Some(entry) => Err(ErrorKind::UnusedKey {
            name: name.to_owned(),
            key: entry.key.clone(),
        }),
        None => Ok(expanded),
    }
}

/// Split a template's opening `@@default` block from its splice text, returning
/// the bindings and the body they precede. The block runs from the first line to
/// the first line that is neither a default nor blank. The blank lines inside
/// and after it belong to the block, so the body never opens with the separator.
fn split_defaults<'a>(splice: &'a [u8], name: &str) -> Result<(Vec<Binding>, &'a [u8]), ErrorKind> {
    let lines = physical_lines(splice);
    let mut defaults: Vec<Binding> = Vec::new();
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let stripped = strip_leading_ws(split_newline(line).0);
        if is_binding(stripped, b"default") {
            let (key, values, end) = parse_binding(&lines, i, "default")?;
            if defaults.iter().any(|b| b.key == key) {
                return Err(ErrorKind::DuplicateDefault {
                    name: name.to_owned(),
                    key,
                });
            }
            defaults.push(Binding { key, values });
            i = end + 1;
        } else if stripped.is_empty() && !defaults.is_empty() {
            i += 1;
        } else {
            break;
        }
    }

    let consumed: usize = lines
        .get(..i)
        .unwrap_or_default()
        .iter()
        .map(|l| l.len())
        .sum();
    let body = splice.get(consumed..).unwrap_or_default();

    for line in body.split(|&b| b == b'\n') {
        if is_binding(strip_leading_ws(line), b"default") {
            return Err(ErrorKind::DefaultMisplaced {
                name: name.to_owned(),
                line: lossy(line),
            });
        }
    }
    // A default for a key the body never mentions could never apply, so it is
    // dead syntax rather than a harmless spare.
    for binding in &defaults {
        if !references(body, &binding.key) {
            return Err(ErrorKind::DefaultUnreferenced {
                name: name.to_owned(),
                key: binding.key.clone(),
            });
        }
    }

    Ok((defaults, body))
}

/// Whether a template body mentions `key` at all, as a `%key%` placeholder
/// (slot or inline) or as the header of an `@@foreach` region.
fn references(body: &[u8], key: &str) -> bool {
    let mut i = 0;
    while i < body.len() {
        match placeholder_at(body, i) {
            Some((found, _)) if found == key => return true,
            Some((_, end)) => i = end,
            None => i += 1,
        }
    }

    body.split(|&b| b == b'\n')
        .any(|line| parse_paren_name(strip_leading_ws(line), b"@@foreach(").as_deref() == Some(key))
}

/// Replace each of the source's own `@@foreach(key)` … `@@end` regions, the
/// delimiters included, with one instance of its body per value of `key`. A
/// blank line separates the instances, and `%key%` holds one value in each.
///
/// A source has no call and no template, so a definition in scope is the only
/// value source. A region consumes the definition it draws on. A `%key%` outside
/// every region of that key stays put, for the postcondition to reject.
fn expand_source_regions(source: &[u8], defs: &mut Definitions) -> Result<Vec<u8>, ErrorKind> {
    let lines = physical_lines(source);
    let mut out = Vec::with_capacity(source.len());
    let mut i = 0;

    while let Some(region) = next_region(&lines, i, &Site::Source)? {
        for &line in lines.get(i..region.open).unwrap_or_default() {
            out.extend_from_slice(line);
        }

        let values = match defs.consume(&region.key) {
            Some(values) => values.to_vec(),
            None => return Err(ErrorKind::RegionKeyUndefined { key: region.key }),
        };
        for (n, value) in values.iter().enumerate() {
            if n > 0 {
                out.push(b'\n');
            }
            for &body in lines.get(region.open + 1..region.close).unwrap_or_default() {
                out.extend_from_slice(&substitute(body, &region.key, value));
            }
        }

        i = region.close + 1;
    }
    for &line in lines.get(i..).unwrap_or_default() {
        out.extend_from_slice(line);
    }

    Ok(out)
}

/// Reject a `@@define`, `@@use` or `@@default` line inside a source region.
/// Those are position-independent: pass 1 collects them file-wide and drops
/// their lines before any region stamps, so one written inside would leave the
/// region silently rather than repeat. This check runs before that pass, the
/// last point at which the line is still in place. [`next_region`] takes a
/// malformed span.
fn guard_regions(lines: &[&[u8]]) -> Result<(), ErrorKind> {
    let mut inside = false;
    for &line in lines {
        let stripped = strip_leading_ws(split_newline(line).0);
        if is_directive(stripped, b"foreach") {
            inside = true;
        } else if stripped == b"@@end" {
            inside = false;
        } else if inside
            && (is_binding(stripped, b"define")
                || is_binding(stripped, b"default")
                || stripped.starts_with(b"@@use("))
        {
            return Err(ErrorKind::DirectiveInRegion {
                line: lossy(stripped),
            });
        }
    }

    Ok(())
}

/// As [`expand_source_regions`], for a template's regions, where the values come
/// from the call instead. Returns the rewritten splice text and the keys the
/// regions consumed.
fn expand_regions(
    splice: &[u8],
    args: &mut Args,
    defs: &mut Definitions,
    name: &str,
) -> Result<(Vec<u8>, Vec<String>), ErrorKind> {
    let lines: Vec<&[u8]> = splice.split(|&b| b == b'\n').collect();
    let site = Site::Template(name.to_owned());
    let mut out = Vec::with_capacity(splice.len());
    let mut iterated: Vec<String> = Vec::new();
    let mut first = true;
    let mut i = 0;

    while let Some(region) = next_region(&lines, i, &site)? {
        for &line in lines.get(i..region.open).unwrap_or_default() {
            push_line(&mut out, &mut first, line);
        }

        // Caught here rather than by the absent-key error below, which would
        // blame the template for what the call did.
        if args.is_suppressed(&region.key) {
            return Err(ErrorKind::SuppressIterated {
                name: name.to_owned(),
                key: region.key,
            });
        }

        // Unlike a slot, an iterated key has no absent form: no number of
        // instances would be right.
        let values = match resolve(args, defs, &region.key) {
            Some(values) => values.to_vec(),
            None => {
                return Err(ErrorKind::RegionKeyAbsent {
                    name: name.to_owned(),
                    key: region.key,
                });
            }
        };
        for (n, value) in values.iter().enumerate() {
            if n > 0 {
                push_line(&mut out, &mut first, b"");
            }
            // A slot line of the iterated key is `<ws>%key%`, so one value put
            // in place keeps exactly the slot's indentation.
            for &body in lines.get(region.open + 1..region.close).unwrap_or_default() {
                push_line(&mut out, &mut first, &substitute(body, &region.key, value));
            }
        }

        if !iterated.contains(&region.key) {
            iterated.push(region.key);
        }
        i = region.close + 1;
    }
    for &line in lines.get(i..).unwrap_or_default() {
        push_line(&mut out, &mut first, line);
    }

    Ok((out, iterated))
}

/// The next region in `lines` at or after `from`, `None` once none remains. The
/// lines skipped on the way must be ordinary text — a stray `@@end` is an error.
/// One grammar gives region shape at both sites, so one scanner serves both.
fn next_region(lines: &[&[u8]], from: usize, site: &Site) -> Result<Option<Region>, ErrorKind> {
    let mut open = from;
    let stripped = loop {
        let stripped = match lines.get(open) {
            Some(&line) => strip_leading_ws(split_newline(line).0),
            None => return Ok(None),
        };
        if is_directive(stripped, b"foreach") {
            break stripped;
        }
        if stripped == b"@@end" {
            return Err(ErrorKind::UnopenedRegion { site: site.clone() });
        }
        open += 1;
    };

    let key =
        parse_paren_name(stripped, b"@@foreach(").ok_or_else(|| ErrorKind::MalformedForeach {
            site: site.clone(),
            line: lossy(stripped),
        })?;

    // Regions do not nest, so the first `@@end` closes this one and an opener
    // before it is an error rather than an inner region.
    let mut close = open + 1;
    loop {
        let inner = match lines.get(close) {
            Some(&line) => strip_leading_ws(split_newline(line).0),
            None => {
                return Err(ErrorKind::UnterminatedRegion {
                    site: site.clone(),
                    key,
                });
            }
        };
        if inner == b"@@end" {
            break;
        }
        if is_directive(inner, b"foreach") {
            return Err(ErrorKind::NestedRegion {
                site: site.clone(),
                key,
            });
        }
        close += 1;
    }
    if close == open + 1 {
        return Err(ErrorKind::EmptyRegion {
            site: site.clone(),
            key,
        });
    }

    Ok(Some(Region { key, open, close }))
}

/// Replace every `%key%` in one line with `value`, leaving other keys'
/// placeholders for the later passes.
fn substitute(line: &[u8], key: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    while let Some(&b) = line.get(i) {
        match placeholder_at(line, i) {
            Some((found, end)) => {
                match found == key {
                    true => out.extend_from_slice(value),
                    false => out.extend_from_slice(line.get(i..end).unwrap_or_default()),
                }
                i = end;
            }
            None => {
                out.push(b);
                i += 1;
            }
        }
    }

    out
}

/// Reject a reference to a key some region already consumed. Its values belong
/// to the instances, so a slot or `%key%` outside every region of that key has
/// no value left to take.
fn reject_iterated(iterated: &[String], key: &str, name: &str) -> Result<(), ErrorKind> {
    match iterated.iter().any(|k| k == key) {
        true => Err(ErrorKind::IteratedOutsideRegion {
            name: name.to_owned(),
            key: key.to_owned(),
        }),
        false => Ok(()),
    }
}

/// Replace each slot line (`<ws>%key%` alone) with one line per value at the
/// slot's indentation, or delete it when the key is absent. Non-slot lines pass
/// through unchanged.
fn expand_slots(
    splice: &[u8],
    args: &mut Args,
    defs: &mut Definitions,
    iterated: &[String],
    name: &str,
) -> Result<Vec<u8>, ErrorKind> {
    let mut out = Vec::with_capacity(splice.len());
    let mut first = true;

    for line in splice.split(|&b| b == b'\n') {
        match slot_key(line) {
            Some((indent_len, key)) => {
                reject_iterated(iterated, key, name)?;
                if let Some(values) = resolve(args, defs, key) {
                    let indent = line.get(..indent_len).unwrap_or_default().to_vec();
                    for value in values {
                        let mut expanded = indent.clone();
                        expanded.extend_from_slice(value);
                        push_line(&mut out, &mut first, &expanded);
                    }
                }
                // Absent key: emit nothing, so the line and its separator vanish.
            }

            None => push_line(&mut out, &mut first, line),
        }
    }

    Ok(out)
}

/// Replace each remaining `%key%` with its single value. A provided key is
/// marked used. An unprovided one stays for the postcondition to reject. A
/// multi-value key used inline is a fatal error.
fn expand_inline(
    buf: &[u8],
    args: &mut Args,
    defs: &mut Definitions,
    iterated: &[String],
    name: &str,
) -> Result<Vec<u8>, ErrorKind> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while let Some(&b) = buf.get(i) {
        if let Some((key, end)) = placeholder_at(buf, i) {
            reject_iterated(iterated, key, name)?;
            // As for a region: a suppressed key here would otherwise reach the
            // postcondition as an unresolved placeholder, naming the template
            // rather than the call.
            if args.is_suppressed(key) {
                return Err(ErrorKind::SuppressInline {
                    name: name.to_owned(),
                    key: key.to_owned(),
                });
            }
            match resolve(args, defs, key) {
                Some(values) if values.len() > 1 => {
                    return Err(ErrorKind::InlineMultiValue {
                        name: name.to_owned(),
                        key: key.to_owned(),
                    });
                }

                Some(values) => {
                    out.extend_from_slice(values.first().map(Vec::as_slice).unwrap_or_default());
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

/// Values bound to `key` — the call's args first, else a file definition —
/// marking whichever source it consumed. `None` if neither provides it: a slot
/// then vanishes, and an inline `%key%` stays for the postcondition to reject.
fn resolve<'a>(args: &'a mut Args, defs: &'a mut Definitions, key: &str) -> Option<&'a [Vec<u8>]> {
    if let Some(idx) = args.index_of(key) {
        args.entries[idx].used = true;
        // An `@none` pair stops the lookup short of the definition — this is
        // where the key becomes absent. Only a slot reaches it. A region or an
        // inline use of a suppressed key fails earlier.
        if args.entries[idx].suppressed {
            return None;
        }
        return Some(&args.entries[idx].values);
    }
    defs.consume(key)
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

/// Append one line to a buffer of LF-joined lines (no trailing LF). `first`
/// tracks an empty buffer, so no separator leads the first line.
fn push_line(out: &mut Vec<u8>, first: &mut bool, line: &[u8]) {
    if !*first {
        out.push(b'\n');
    }
    *first = false;
    out.extend_from_slice(line);
}

/// Split a physical line into its content and its terminator (`b"\n"` or empty).
fn split_newline(line: &[u8]) -> (&[u8], &[u8]) {
    match line.strip_suffix(b"\n") {
        Some(content) => (content, line.get(content.len()..).unwrap_or_default()),
        None => (line, b""),
    }
}

/// Split one-line args (`inner` between the parens) into pair segments on `;`.
/// A value never contains `;`, so the split is exact.
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

/// Name a definition's origin for a collision message: the `@@use` that imported
/// it, or the in-file `@@define`.
fn origin(from: &Option<String>) -> String {
    match from {
        Some(name) => format!("@@use({name})"),
        None => "@@define".to_owned(),
    }
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
                let path = dir.join(name).with_extension("baf");
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).expect("create fragment dir");
                }
                fs::write(path, body).expect("write fragment");
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
        // The splice drops the fragment's own trailing newline and keeps the
        // marker line's, so one blank line survives on either side.
        assert_eq!(out, b"A\n\nIF\n  True()\nEND\n\nB\n");
    }

    #[test]
    fn an_indented_directive_expands_but_a_mid_line_at_at_does_not() {
        // v2 discards leading blanks, so an indented marker is a directive. A
        // mid-line `@@` (first non-blank is `D`) stays for the compiler.
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
        assert!(kind(t.expand(b"@@use\n")).contains("reserved directive name @@use"));
        // v4 makes `@@foreach` a source form too, so a bare one is a malformed
        // region opener — the pointed error, ahead of the reserved-name backstop.
        assert!(kind(t.expand(b"@@foreach\n")).contains("malformed @@foreach line"));
        assert!(kind(t.expand(b"@@slot()\n")).contains("malformed directive"));
        assert!(kind(t.expand(b"@@slot(x=)\n")).contains("empty value for key 'x'"));
        assert!(kind(t.expand(b"@@none(x=1)\n")).contains("key 'x' is not used"));
        assert!(kind(t.expand(b"@@inline(x=1; x=2)\n")).contains("multi-value key 'x'"));
        // A required inline key nobody provides reaches the postcondition.
        assert!(kind(t.expand(b"@@inline\n")).contains("unresolved placeholder"));
        assert!(kind(t.expand(b"@@t(\n  a=1;\n")).contains("unterminated multi-line call @@t"));
    }

    #[test]
    fn a_define_feeds_a_key_the_call_omits() {
        // The call provides `verb`. `giveup` (a multi-value slot) comes from the
        // file definition. The one-line and multi-line define forms agree, and
        // the define's own lines leave no trace.
        let t = Templates::with(&[("t", b"IF\n  %verb%\nTHEN\n  %giveup%\nEND\n")]);
        let expect = b"IF\n  Detect\nTHEN\n  A\n  B\nEND\n";

        let multi = b"@@define giveup(\n  A;\n  B;\n)\n@@t(verb=Detect)\n";
        assert_eq!(t.expand(multi).expect("multi-line define"), expect);

        let one = b"@@define giveup(A; B)\n@@t(verb=Detect)\n";
        assert_eq!(t.expand(one).expect("one-line define"), expect);
    }

    #[test]
    fn a_define_is_lazy_and_position_independent() {
        // A template that never references the key raises no unused-key error,
        // as long as some other call consumes the definition. The define may sit
        // after the call that uses it.
        let t = Templates::with(&[("uses", b"X(%k%)\n"), ("ignores", b"Y\n")]);
        let src = b"@@ignores\n@@uses\n@@define k(v)\n";
        assert_eq!(t.expand(src).expect("lazy define"), b"Y\nX(v)\n");
    }

    #[test]
    fn define_misuse_is_rejected() {
        let t = Templates::with(&[("t", b"%k%\n"), ("none", b"Z\n")]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        // Two definitions for one key.
        assert!(
            kind(t.expand(b"@@define k(a)\n@@define k(b)\n@@t\n"))
                .contains("duplicate @@define for key 'k'")
        );
        // Call passes a key that also has a definition.
        assert!(kind(t.expand(b"@@define k(a)\n@@t(k=b)\n")).contains("also set by @@define"));
        // A definition no call ever draws on.
        assert!(kind(t.expand(b"@@define k(a)\n@@none\n")).contains("used by no call"));
        // A multi-line define that reaches the end with no closing `)`.
        assert!(
            kind(t.expand(b"@@define k(\n  a;\n")).contains("unterminated multi-line @@define k")
        );
        // A span line that is neither `value;` nor the lone `)`.
        assert!(kind(t.expand(b"@@define k(\n  a\n)\n@@t\n")).contains("malformed directive"));
        // Zero-value define.
        assert!(kind(t.expand(b"@@define k()\n@@t\n")).contains("malformed directive"));
    }

    #[test]
    fn an_imported_define_feeds_a_key_the_call_omits() {
        // `@@use` takes the `giveup` definition from a shared defs file. It fills
        // the template's slot exactly as an in-file define would. The use line
        // and the defs file leave no trace in the output.
        let t = Templates::with(&[
            ("t", b"IF\n  %verb%\nTHEN\n  %giveup%\nEND\n"),
            (
                "defs/shared",
                b"// shared across scripts\n@@define giveup(\n  A;\n  B;\n)\n",
            ),
        ]);
        assert_eq!(
            t.expand(b"@@use(shared)\n@@t(verb=Detect)\n")
                .expect("import"),
            b"IF\n  Detect\nTHEN\n  A\n  B\nEND\n"
        );
    }

    #[test]
    fn use_misuse_is_rejected() {
        let t = Templates::with(&[
            ("t", b"%k%\n"),
            ("none", b"Z\n"),
            ("defs/a", b"@@define k(x)\n"),
            ("defs/b", b"@@define k(y)\n"),
            ("defs/empty", b"// nothing here\n"),
            ("defs/bad", b"@@define k(x)\nCallSomething()\n"),
        ]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        // Missing defs file.
        assert!(kind(t.expand(b"@@use(nope)\n@@t\n")).contains("cannot read"));
        // The same defs file imported twice.
        assert!(kind(t.expand(b"@@use(a)\n@@use(a)\n@@t\n")).contains("duplicate @@use(a)"));
        // A defs file line that is neither a definition, blank, nor a comment.
        assert!(kind(t.expand(b"@@use(bad)\n@@t\n")).contains("non-define content"));
        // A defs file that binds nothing.
        assert!(kind(t.expand(b"@@use(empty)\n@@t\n")).contains("defines nothing"));
        // A key bound both in-file and by an import.
        assert!(
            kind(t.expand(b"@@use(a)\n@@define k(z)\n@@t\n"))
                .contains("defined by both @@define and @@use(a)")
        );
        // The same key from two different imports.
        assert!(
            kind(t.expand(b"@@use(a)\n@@use(b)\n@@t\n"))
                .contains("defined by both @@use(a) and @@use(b)")
        );
        // An imported definition no call ever draws on.
        assert!(kind(t.expand(b"@@use(a)\n@@none\n")).contains("used by no call"));
    }

    #[test]
    fn a_none_pair_opts_one_call_out_of_a_definition() {
        // Both calls draw on the same definition. The second suppresses it, so
        // its slot line goes exactly as if nothing had provided the key.
        let t = Templates::with(&[("t", b"IF\n  %guard%\n  Global()\nEND\n")]);
        assert_eq!(
            t.expand(b"@@define guard(A)\n@@t\n@@t(guard=@none)\n")
                .expect("suppressed"),
            b"IF\n  A\n  Global()\nEND\nIF\n  Global()\nEND\n"
        );
    }

    #[test]
    fn none_misuse_is_rejected() {
        let t = Templates::with(&[
            ("t", b"IF\n  %k%\nEND\n"),
            ("plain", b"Z\n"),
            ("inline", b"Cast(%k%)\n"),
            ("region", b"@@foreach(k)\nX(%k%)\n@@end\n"),
        ]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        // Absence is already the default, so the call needs a definition (or a
        // template default) to opt out of.
        assert!(kind(t.expand(b"@@t(k=@none)\n")).contains("no definition or default"));
        // Not an override: a value and `@none` for one key, in either order,
        // and no repeats.
        let mixed = "both a value and @none";
        assert!(kind(t.expand(b"@@define k(a)\n@@t(k=@none; k=b)\n")).contains(mixed));
        assert!(kind(t.expand(b"@@define k(a)\n@@t(k=b; k=@none)\n")).contains(mixed));
        assert!(
            kind(t.expand(b"@@define k(a)\n@@t(k=@none; k=@none)\n")).contains("repeated @none")
        );
        // The template must reference the key, and only by a slot.
        assert!(kind(t.expand(b"@@define k(a)\n@@plain(k=@none)\n")).contains("not referenced"));
        assert!(kind(t.expand(b"@@define k(a)\n@@inline(k=@none)\n")).contains("required inline"));
        assert!(
            kind(t.expand(b"@@define k(a)\n@@region(k=@none)\n")).contains("required by @@foreach")
        );
        // Reserved as a definition value, in-file and imported alike, since one
        // parse path serves both.
        assert!(kind(t.expand(b"@@define k(@none)\n@@t\n")).contains("reserved as a value"));
        // Suppression is not consumption: the definition still needs a real user.
        assert!(kind(t.expand(b"@@define k(a)\n@@t(k=@none)\n")).contains("used by no call"));
    }

    #[test]
    fn a_default_supplies_a_key_no_higher_source_provides() {
        let t = Templates::with(&[(
            "t",
            b"@@default guard(A)\n@@default verb(\n  Detect;\n)\n\nIF\n  %guard%\nTHEN\n  %verb%\nEND\n",
        )]);
        let bare = b"IF\n  A\nTHEN\n  Detect\nEND\n";
        // The block itself leaves no trace, one-line and multi-line forms alike.
        assert_eq!(t.expand(b"@@t\n").expect("defaults"), bare);
        // Args outrank a default, since overriding one is the point. A
        // definition outranks it too, and the default it displaces is no error.
        assert_eq!(
            t.expand(b"@@t(guard=B)\n").expect("arg wins"),
            b"IF\n  B\nTHEN\n  Detect\nEND\n"
        );
        assert_eq!(
            t.expand(b"@@define guard(C)\n@@t\n").expect("define wins"),
            b"IF\n  C\nTHEN\n  Detect\nEND\n"
        );
        // `@none` blocks the default as it blocks a definition.
        assert_eq!(
            t.expand(b"@@t(guard=@none)\n").expect("suppressed default"),
            b"IF\nTHEN\n  Detect\nEND\n"
        );

        // An `@@foreach` header alone counts as referencing the key.
        let r = Templates::with(&[("r", b"@@default k(1; 2)\n\n@@foreach(k)\nX\n@@end\n")]);
        assert_eq!(r.expand(b"@@r\n").expect("region default"), b"X\n\nX\n");
    }

    #[test]
    fn default_misuse_is_rejected() {
        let t = Templates::with(&[
            ("t", b"@@default k(a)\n\n%k%\n"),
            ("dup", b"@@default k(a)\n@@default k(b)\n\n%k%\n"),
            ("late", b"%k%\n@@default k(a)\n"),
            ("dead", b"@@default j(a)\n\n%k%\n"),
            ("reserved", b"@@default k(@none)\n\n%k%\n"),
            ("empty", b"@@default k()\n\n%k%\n"),
            ("unterm", b"@@default k(\n  a;\n"),
            ("defs/d", b"@@default k(a)\n"),
        ]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        // A default belongs to a template's opening block — not to a source, not
        // to a defs file, not to the body below it.
        assert!(kind(t.expand(b"@@default k(a)\n@@t\n")).contains("valid only in a template"));
        assert!(kind(t.expand(b"@@use(d)\n@@t\n")).contains("non-define content"));
        assert!(kind(t.expand(b"@@late\n")).contains("outside the opening block"));
        // The keyless form is an ordinary call, so it fails as a reserved name.
        assert!(kind(t.expand(b"@@default(k=a)\n")).contains("reserved directive name @@default"));
        assert!(kind(t.expand(b"@@dup\n")).contains("duplicate @@default for key 'k'"));
        // A default the body never mentions could never apply.
        assert!(kind(t.expand(b"@@dead\n")).contains("@@default key 'j' is not referenced"));
        assert!(kind(t.expand(b"@@reserved\n")).contains("reserved as a value"));
        assert!(kind(t.expand(b"@@empty\n")).contains("malformed directive"));
        assert!(kind(t.expand(b"@@unterm\n")).contains("unterminated multi-line @@default k"));
    }

    #[test]
    fn a_foreach_region_repeats_its_block_per_value() {
        // The region repeats once per value of `who`, blank-line separated, with
        // `%who%` bound to that value alone. A non-iterated key inside the region
        // acts as anywhere else: its slot expands its full list in every
        // instance.
        let t = Templates::with(&[(
            "t",
            b"IF\n@@foreach(who)\n  See(%who%)\n  %extra%\n@@end\nEND\n",
        )]);
        assert_eq!(
            t.expand(b"@@t(who=A; who=B; extra=X)\n").expect("region"),
            b"IF\n  See(A)\n  X\n\n  See(B)\n  X\nEND\n"
        );
        assert_eq!(
            t.expand(b"@@t(who=A; who=B; extra=X; extra=Y)\n")
                .expect("region"),
            b"IF\n  See(A)\n  X\n  Y\n\n  See(B)\n  X\n  Y\nEND\n"
        );
    }

    #[test]
    fn every_region_of_a_key_iterates_the_whole_list() {
        // Two regions for one key, mirroring multi-slot behavior. The values may
        // come from a definition, and one region consumes the key for the whole
        // template, so no unused-key error follows.
        let t = Templates::with(&[(
            "t",
            b"@@foreach(k)\nP(%k%)\n@@end\n@@foreach(k)\nQ(%k%)\n@@end\n",
        )]);
        assert_eq!(
            t.expand(b"@@define k(1; 2)\n@@t\n").expect("two regions"),
            b"P(1)\n\nP(2)\nQ(1)\n\nQ(2)\n"
        );
    }

    #[test]
    fn foreach_misuse_is_rejected() {
        let t = Templates::with(&[
            ("nest", b"@@foreach(k)\n@@foreach(j)\nX\n@@end\n@@end\n"),
            ("unterm", b"@@foreach(k)\nX\n"),
            ("stray", b"X\n@@end\n"),
            ("empty", b"@@foreach(k)\n@@end\n"),
            ("bad", b"@@foreach k\n"),
            ("absent", b"@@foreach(k)\nX(%k%)\n@@end\n"),
            ("slot", b"@@foreach(k)\nX(%k%)\n@@end\n%k%\n"),
            ("inline", b"@@foreach(k)\nX(%k%)\n@@end\nY(%k%)\n"),
        ]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);

        assert!(kind(t.expand(b"@@nest(k=1; j=2)\n")).contains("nested @@foreach region"));
        assert!(kind(t.expand(b"@@unterm(k=1)\n")).contains("unterminated @@foreach(k) region"));
        assert!(kind(t.expand(b"@@stray\n")).contains("@@end without @@foreach"));
        assert!(kind(t.expand(b"@@empty(k=1)\n")).contains("empty @@foreach(k) region"));
        assert!(kind(t.expand(b"@@bad\n")).contains("malformed @@foreach line"));
        // A region whose key the call never provides has no instance count.
        assert!(kind(t.expand(b"@@absent\n")).contains("@@foreach key 'k' is not provided"));
        // The region spends the iterated key, so a slot or inline use outside it
        // has nothing left to take.
        assert!(kind(t.expand(b"@@slot(k=1)\n")).contains("iterated key 'k' referenced outside"));
        assert!(kind(t.expand(b"@@inline(k=1)\n")).contains("iterated key 'k' referenced outside"));
    }

    #[test]
    fn a_source_region_repeats_its_block_per_defined_value() {
        // A source has no call and no template, so the values come from a
        // definition, in-file or imported alike, and the region consumes it.
        let t = Templates::with(&[
            ("t", b"Cast(%who%)\n"),
            ("defs/party", b"@@define pn(1; 2)\n"),
        ]);
        let src = b"@@define pn(1; 2)\nA\n@@foreach(pn)\nSee(Player%pn%)\n@@end\nB\n";
        assert_eq!(
            t.expand(src).expect("region"),
            b"A\nSee(Player1)\n\nSee(Player2)\nB\n"
        );
        assert_eq!(
            t.expand(b"@@use(party)\n@@foreach(pn)\nX(%pn%)\n@@end\n")
                .expect("imported"),
            b"X(1)\n\nX(2)\n"
        );

        // A directive inside a region stamps first and expands after, so each
        // instance is an ordinary call with its `%pn%` already resolved.
        assert_eq!(
            t.expand(b"@@define pn(1; 2)\n@@foreach(pn)\n@@t(who=Player%pn%)\n@@end\n")
                .expect("call in region"),
            b"Cast(Player1)\n\nCast(Player2)\n"
        );
    }

    #[test]
    fn source_region_misuse_is_rejected() {
        let t = Templates::with(&[("t", b"Z\n")]);
        let kind = |r: Result<Vec<u8>, PreprocessError>| format!("{}", r.expect_err("fatal").kind);
        let def = "@@define k(1; 2)\n".as_bytes();
        let src = |body: &str| [def, body.as_bytes()].concat();

        assert!(
            kind(t.expand(&src("@@foreach(k)\n@@foreach(j)\nX\n@@end\n@@end\n")))
                .contains("nested @@foreach region")
        );
        assert!(
            kind(t.expand(&src("@@foreach(k)\nX\n"))).contains("unterminated @@foreach(k) region")
        );
        assert!(kind(t.expand(&src("X\n@@end\n"))).contains("@@end without @@foreach"));
        assert!(
            kind(t.expand(&src("@@foreach(k)\n@@end\n"))).contains("empty @@foreach(k) region")
        );
        assert!(
            kind(t.expand(&src("@@foreach k\nX\n@@end\n"))).contains("malformed @@foreach line")
        );
        // Template defaults cannot reach here — there is no call and no template.
        assert!(
            kind(t.expand(b"@@foreach(j)\nX\n@@end\n"))
                .contains("@@foreach key 'j' has no definition in scope")
        );
        // A position-independent directive would leave the region instead of
        // repeating, so the guard rejects it before pass 1 collects it.
        for inner in ["@@define j(a)", "@@use(shared)", "@@default j(a)"] {
            let region = format!("@@foreach(k)\n{inner}\nX\n@@end\n");
            assert!(
                kind(t.expand(&src(&region))).contains("inside a @@foreach region"),
                "{inner}"
            );
        }
        // The wrapping message names the source, so no `@@template:` prefix
        // leads these — unlike their template-side twins.
        assert!(
            kind(t.expand(&src("@@foreach(k)\n@@end\n"))).starts_with("empty @@foreach(k) region")
        );
    }

    #[test]
    fn a_template_injected_directive_is_rejected() {
        let t = Templates::with(&[("bad", b"@@other\n")]);
        let err = format!("{}", t.expand(b"@@bad\n").expect_err("fatal").kind);
        assert!(err.contains("unexpanded directive remains"), "{err}");
    }
}
