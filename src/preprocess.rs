//! ImprovedAnvil BAF preprocessor (`--templates`): `@@name` directive lines
//! expanded from a directory of template fragments before the source reaches
//! the compiler.
//!
//! The normative grammar is `baf-preprocessor-grammar.md` (syntax v3); this
//! mirrors the mod's own `lib/tpa/baf_preprocessor.tpa`, and the two MUST
//! produce byte-identical expansions.
//!
//! A directive is a whole line whose first non-blank characters are `@@`. It
//! names a template and carries optional `key=value` args — on the one line, or
//! spread over several `@@name(` … `)` lines. Args fill the template's `%key%`
//! *slot lines* (a whole line that is only `%key%`, repeated once per value and
//! deleted when the key is absent) and its inline `%key%` placeholders. The
//! spliced text is inserted verbatim and never rescanned; after every directive
//! is expanded, a leftover `@@` line or `%token%` anywhere is a fatal error.
//!
//! A template body may wrap a run of lines in `@@foreach(key)` … `@@end`: the
//! block is emitted once per value of `key`, blank-line separated, with `%key%`
//! bound to that one value. Regions are a template-only form — in a source
//! `foreach` and `end` stay reserved names.
//!
//! `@@define key(v1; v2; …)` binds a key at file scope: any call whose template
//! references `key` but whose args omit it draws the definition's values, as if
//! the call had passed them. `@@use(name)` imports the same kind of definitions
//! from a shared defs file (`<templates-dir>/defs/name.baf`), which holds only
//! definitions. Both are collected in a first pass (they are position-
//! independent), then their own lines are dropped from the output; an imported
//! definition is thereafter indistinguishable from an in-file one, bar the
//! origin kept for error messages.

use std::fmt;
use std::path::{Path, PathBuf};

/// Template names the grammar forbids in a call. `define`/`use` head their own
/// directives; `foreach`/`end` delimit a template's iteration regions.
const RESERVED: [&str; 4] = ["foreach", "end", "define", "use"];

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

/// The file's definition bindings — a fallback value source for calls that omit
/// a key. Fed by in-file `@@define`s and `@@use`d imports alike; `used` is set
/// when some call draws on a definition, and one left unused by every call is a
/// fatal error, so a stale definition never passes silently.
struct Definitions {
    entries: Vec<Def>,
}

impl Definitions {
    fn new() -> Definitions {
        Definitions {
            entries: Vec::new(),
        }
    }

    /// Record a definition, rejecting a second one for the same key. `from` is
    /// the defs file it was imported from, or `None` for an in-file `@@define`;
    /// it origins the collision message (all in-file inserts precede imports, so
    /// the older entry is never the more-remote one).
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
    UnterminatedDefine {
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
    MalformedForeach {
        name: String,
        line: String,
    },
    NestedRegion {
        name: String,
        key: String,
    },
    UnterminatedRegion {
        name: String,
        key: String,
    },
    UnopenedRegion {
        name: String,
    },
    EmptyRegion {
        name: String,
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
            ErrorKind::UnterminatedDefine { key } => {
                write!(f, "unterminated multi-line @@define {key}")
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
            ErrorKind::MalformedForeach { name, line } => {
                write!(f, "@@{name}: malformed @@foreach line: {line}")
            }
            ErrorKind::NestedRegion { name, key } => {
                write!(
                    f,
                    "@@{name}: nested @@foreach region inside @@foreach({key})"
                )
            }
            ErrorKind::UnterminatedRegion { name, key } => {
                write!(f, "@@{name}: unterminated @@foreach({key}) region")
            }
            ErrorKind::UnopenedRegion { name } => {
                write!(f, "@@{name}: @@end without @@foreach")
            }
            ErrorKind::EmptyRegion { name, key } => {
                write!(f, "@@{name}: empty @@foreach({key}) region")
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

    // Pass 1: collect every `@@define` and `@@use` (both position-independent
    // and file-scoped) and mark their lines so pass 2 drops them. Imports are
    // read after all in-file definitions, so a key clash reports the import as
    // the newcomer.
    let mut defs = Definitions::new();
    let mut uses: Vec<String> = Vec::new();
    let mut consumed = vec![false; lines.len()];
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let stripped = strip_leading_ws(split_newline(line).0);
        if is_directive(stripped, b"define") {
            let (key, values, end) = parse_define(&lines, i)?;
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

    // Pass 2: expand each call (drawing on the definitions), pass the rest through.
    let mut out = Vec::with_capacity(source.len());
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        if *consumed.get(i).unwrap_or(&false) {
            i += 1;
            continue;
        }

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

/// Whether a stripped directive line heads the directive `head` — its name is
/// exactly that, so a template whose name merely starts with it (`@@definers`)
/// stays an ordinary call. Matches a malformed head too, so the specific error
/// can be reported over "malformed directive".
fn is_directive(stripped: &[u8], head: &[u8]) -> bool {
    let after = match stripped.strip_prefix(b"@@") {
        Some(after) => after,
        None => return false,
    };
    let name_len = after.iter().take_while(|b| is_name(b)).count();
    after.get(..name_len) == Some(head)
}

/// Parse an `@@define key(…)` starting at line `i` (already known to head a
/// define). Returns the key, its values, and the index of the last line the
/// directive spans (its own line, or the closing `)` line for the multi-line
/// form).
fn parse_define(lines: &[&[u8]], i: usize) -> Result<(String, Vec<Vec<u8>>, usize), ErrorKind> {
    let line = lines.get(i).copied().unwrap_or_default();
    let stripped = strip_leading_ws(split_newline(line).0);
    let rest = stripped.strip_prefix(b"@@define").unwrap_or_default();

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

    // `(` last on the line opens the multi-line form; otherwise `)` closes it here.
    if inside.is_empty() {
        let (values, end) = gather_define_values(lines, i + 1, key)?;
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
        values.push(value.to_vec());
    }
    Ok((key.to_owned(), values, i))
}

/// Collect the `value;` lines of a multi-line `@@define`, from `start` to the
/// lone `)` line whose index is returned. Every value line ends with `;` and
/// there must be at least one — mirrors [`gather_multiline`].
fn gather_define_values(
    lines: &[&[u8]],
    start: usize,
    key: &str,
) -> Result<(Vec<Vec<u8>>, usize), ErrorKind> {
    let mut values = Vec::new();
    let mut j = start;
    loop {
        let line = lines.get(j).ok_or_else(|| ErrorKind::UnterminatedDefine {
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
                values.push(value.to_vec());
            }
            _ => return Err(malformed(stripped)),
        }

        j += 1;
    }
}

/// The lone name a directive line carries after `prefix` — the shape shared by
/// `@@use(name)` and `@@foreach(key)`: `prefix` ws* name ws* `)`. `None` if the
/// line does not match.
fn parse_paren_name(stripped: &[u8], prefix: &[u8]) -> Option<String> {
    let after = strip_leading_ws(stripped.strip_prefix(prefix)?);
    let name_len = after.iter().take_while(|b| is_name(b)).count();
    if name_len == 0 {
        return None;
    }
    let name = std::str::from_utf8(after.get(..name_len)?).ok()?;
    (strip_leading_ws(after.get(name_len..)?) == b")").then(|| name.to_owned())
}

/// Read `<templates-dir>/defs/<name>.baf` and fold its definitions into `defs`
/// (origin `name`). A defs file holds only definitions, blank lines, and `//`
/// comments; any other content — a template call, a nested `@@use` — is a fatal
/// error, as is one contributing no definitions.
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
            let (key, values, end) = parse_define(&lines, i)?;
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

    // A key comes from the call or a file definition, never both.
    for entry in &args.entries {
        if let Some(idx) = defs.index_of(&entry.key) {
            return Err(ErrorKind::DefineCollision {
                name: name.to_owned(),
                key: entry.key.clone(),
                from: defs.entries.get(idx).and_then(|d| d.from.clone()),
            });
        }
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
    let (after_regions, iterated) = expand_regions(splice, &mut args, defs, name)?;
    let after_slots = expand_slots(&after_regions, &mut args, defs, &iterated, name)?;
    let expanded = expand_inline(&after_slots, &mut args, defs, &iterated, name)?;

    match args.first_unused() {
        Some(key) => Err(ErrorKind::UnusedKey {
            name: name.to_owned(),
            key: key.to_owned(),
        }),
        None => Ok(expanded),
    }
}

/// Step 0: replace each `@@foreach(key)` … `@@end` region (the delimiters
/// included) with one instance of its body per value of `key`, in value order
/// and separated by a blank line, `%key%` bound to that one value. Returns the
/// rewritten splice text and the keys the regions consumed.
fn expand_regions(
    splice: &[u8],
    args: &mut Args,
    defs: &mut Definitions,
    name: &str,
) -> Result<(Vec<u8>, Vec<String>), ErrorKind> {
    let lines: Vec<&[u8]> = splice.split(|&b| b == b'\n').collect();
    let mut out = Vec::with_capacity(splice.len());
    let mut iterated: Vec<String> = Vec::new();
    let mut first = true;
    let mut i = 0;

    while let Some(&line) = lines.get(i) {
        let stripped = strip_leading_ws(line);
        if !is_directive(stripped, b"foreach") {
            if stripped == b"@@end" {
                return Err(ErrorKind::UnopenedRegion {
                    name: name.to_owned(),
                });
            }
            push_line(&mut out, &mut first, line);
            i += 1;
            continue;
        }

        let key = parse_paren_name(stripped, b"@@foreach(").ok_or_else(|| {
            ErrorKind::MalformedForeach {
                name: name.to_owned(),
                line: lossy(stripped),
            }
        })?;

        // Regions do not nest, so the first `@@end` closes this one and an
        // opener before it is an error rather than an inner region.
        let mut close = i + 1;
        loop {
            let inner = lines
                .get(close)
                .copied()
                .ok_or_else(|| ErrorKind::UnterminatedRegion {
                    name: name.to_owned(),
                    key: key.clone(),
                })?;
            let inner = strip_leading_ws(inner);
            if inner == b"@@end" {
                break;
            }
            if is_directive(inner, b"foreach") {
                return Err(ErrorKind::NestedRegion {
                    name: name.to_owned(),
                    key,
                });
            }
            close += 1;
        }
        if close == i + 1 {
            return Err(ErrorKind::EmptyRegion {
                name: name.to_owned(),
                key,
            });
        }

        // Unlike a slot, an iterated key has no absent form: there is no
        // sensible number of instances to emit.
        let values = match resolve(args, defs, &key) {
            Some(values) => values.to_vec(),
            None => {
                return Err(ErrorKind::RegionKeyAbsent {
                    name: name.to_owned(),
                    key,
                });
            }
        };
        for (n, value) in values.iter().enumerate() {
            if n > 0 {
                push_line(&mut out, &mut first, b"");
            }
            // A slot line of the iterated key is `<ws>%key%`, so substituting a
            // single value in place leaves exactly the slot's indentation.
            for &body in lines.get(i + 1..close).unwrap_or_default() {
                push_line(&mut out, &mut first, &substitute(body, &key, value));
            }
        }

        if !iterated.contains(&key) {
            iterated.push(key);
        }
        i = close + 1;
    }

    Ok((out, iterated))
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

/// Reject a reference to a key some region already consumed: its values belong
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
/// marked used; an unprovided one is left for the postcondition to reject; a
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
/// marking whichever source consumed. `None` if neither provides it (a slot then
/// vanishes; an inline `%key%` is left for the postcondition to reject).
fn resolve<'a>(args: &'a mut Args, defs: &'a mut Definitions, key: &str) -> Option<&'a [Vec<u8>]> {
    if let Some(idx) = args.index_of(key) {
        args.entries[idx].used = true;
        return Some(&args.entries[idx].values);
    }
    if let Some(idx) = defs.index_of(key) {
        defs.entries[idx].used = true;
        return Some(&defs.entries[idx].values);
    }
    None
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

/// Append one line to a buffer of LF-joined lines (no trailing LF), tracking
/// whether the buffer is still empty so no separator leads the first line.
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
    fn a_define_feeds_a_key_the_call_omits() {
        // The call provides `verb`; `giveup` (a multi-value slot) comes from the
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
        // A template that never references the key is unaffected — no unused-key
        // error — as long as some other call consumes the definition. The define
        // may sit after the call that uses it.
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
        // Multi-line define running off the end with no closing `)`.
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
        // `@@use` pulls the `giveup` definition from a shared defs file; it fills
        // the template's slot exactly as an in-file define would, and both the
        // use line and the defs file leave no trace in the output.
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
    fn a_foreach_region_repeats_its_block_per_value() {
        // The region is emitted once per value of `who`, blank-line separated,
        // with `%who%` bound to that value alone. A non-iterated key inside the
        // region behaves as anywhere else: its slot expands its full list in
        // every instance.
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
        // Two regions for one key, mirroring multi-slot behaviour; the values may
        // come from a definition, and one region consumes the key for the whole
        // template (no unused-key error).
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
        // The iterated key is spent by the region: a slot or inline use outside it
        // has nothing left to take.
        assert!(kind(t.expand(b"@@slot(k=1)\n")).contains("iterated key 'k' referenced outside"));
        assert!(kind(t.expand(b"@@inline(k=1)\n")).contains("iterated key 'k' referenced outside"));
    }

    #[test]
    fn a_template_injected_directive_is_rejected() {
        let t = Templates::with(&[("bad", b"@@other\n")]);
        let err = format!("{}", t.expand(b"@@bad\n").expect_err("fatal").kind);
        assert!(err.contains("unexpanded directive remains"), "{err}");
    }
}
