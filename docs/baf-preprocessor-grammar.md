# BAF preprocessor grammar (syntax v4.2)

This document is normative. Two implementations follow it: `S!BAF_COMPILE` in
WeiDU (`lib/tpa/baf_preprocessor.tpa`) and iediff (`src/preprocess.rs`,
`--templates`). Both must match it exactly. A difference between them is a bug
in one of them. Conformance test: for every preprocessed source in the corpus,
both implementations produce byte-identical output.

For an introduction with examples, read `baf-preprocessor.md`.

## Input model

- A source file is LF-terminated text. CR is not permitted.
- A template is the file `<templates-dir>/<name>.baf`. It must end with exactly
  one trailing LF.
- The **splice text** of a template is the file content minus its default block
  (see `@@default`) and minus the final LF.
- Expansion is one top-to-bottom pass over the source lines. The preprocessor
  does not rescan spliced text. There is no recursion.
- The template names `foreach`, `end`, `define`, `use`, and `default` are
  reserved. A call to one of them is a fatal error.

## Directive lines

A line whose first non-`ws` characters are `@@` is a directive line. The
preprocessor discards the leading `ws`. The rest of the line must match:

```
call   = "@@" name args? EOL
name   = [a-z0-9_]+
args   = "(" pair ( ";" pair )* ")"        ; ")" is the last char on the line
pair   = ws* key ws* "=" value
key    = [a-z0-9_]+
value  = any chars except ";" and EOL      ; leading/trailing ws trimmed
ws     = " " | TAB
```

- A directive line that does not match `call` is a fatal error.
- A directive never shares a line with BAF text.
- Duplicate keys are permitted. The preprocessor keeps the value order.
- `@@name()` is malformed. The no-args form is the bare `@@name`.
- A value that is empty after trimming is a fatal error.

### Multi-line calls

A call with args can span several lines:

```
mcall  = mopen marg* mclose
mopen  = ws* "@@" name "(" EOL          ; "(" is the last char on the line
marg   = ws* pair ";" EOL               ; every arg line ends with ";",
                                        ;   including the last
mclose = ws* ")" EOL                    ; ")" alone on its line
```

- A multi-line call means the same as the one-line call with the same pairs in
  the same order. An implementation may canonicalize `mcall` to `call` before
  expansion.
- The two forms do not mix in one call. A one-line call closes its `)` on the
  opening line. A multi-line call puts nothing after `(` and closes with a lone
  `)` line.
- Zero `marg` lines is malformed, as `@@name()` is.
- An `mopen` with no `mclose` line is a fatal error (unterminated call).
- An inner line that is neither `marg` nor `mclose` is a fatal error. A missing
  trailing `;` is the common case.

## Definitions (`@@define`)

A definition binds a key to a list of values at file scope. It replaces the
same key in the args of each call:

```
def    = ws* "@@define" " " key "(" value ( ";" value )* ")" EOL
mdef   = mdopen mdval mdval* mdclose        ; multi-line form
mdopen = ws* "@@define" " " key "(" EOL     ; "(" is the last char
mdval  = ws* value ";" EOL                  ; every value line ends in ";",
                                            ;   including the last
mdclose= ws* ")" EOL                        ; ")" alone on its line
```

- Exactly one space separates `@@define` and `key`. Values follow the same
  trimming and non-empty rules as call args.
- A definition is pure sugar. When a call's template references `key` and the
  call's args do not provide it, the definition applies exactly as if the call
  had passed `key=value1; key=value2; ...`. Slot, inline, and multi-value rules
  do not change.
- A definition is file-scoped. At most one definition per key per file. A
  second one is a fatal error.
- Definitions are position-independent within the file. Convention, not
  enforced: place them before first use.
- A call that passes a key which also has a definition in the file is a fatal
  error. There is no override and no merge.
- Definitions are lazy. A call whose template does not reference `key` is not
  affected, and the definition causes no unused-key error.
- A definition that no call in the file consumes is a fatal error.
- The preprocessor deletes the definition lines from the output.
- `@@define key()` is malformed. An `mdef` with no `mdval` line, an
  unterminated `mdef`, and an inner line that is neither `mdval` nor `mdclose`
  are fatal errors, as for `mcall`.

## Imported definitions (`@@use`)

A source can import definitions from a _defs file_:

```
use    = ws* "@@use(" ws* name ws* ")" EOL
```

- The defs file is `<templates-dir>/defs/<name>.baf`. A missing file is a
  fatal error.
- A defs file contains only `def`/`mdef` directives, blank lines, and comment
  lines (first non-`ws` characters `//`). All other content is a fatal error.
  This includes `@@use` itself (no transitive imports) and template calls.
- A defs file that contributes zero definitions is a fatal error.
- Imported definitions enter the using file's definition table exactly as if
  written in that file: same laziness, same call-arg collision rule, same
  strictness.
- At least one call in the using file must consume every definition in every
  used file. Otherwise it is a fatal error. Defs files are therefore
  single-purpose, not grab-bag bundles.
- There is no shadowing and no merging. A key defined both in-file and by an
  import, or by two imports, is a fatal error. So is a duplicate within one
  defs file.
- A file that imports the same name more than once is a fatal error.
- `@@use` lines are position-independent. The preprocessor deletes them from
  the output. Convention, not enforced: top of file, before any definition.
- A defs file is not a template. It has no splice text and is never callable.
  Template calls cannot reach files under `<templates-dir>/defs/`.

## Suppression pairs (`@none`)

A call can opt out of an in-scope definition or a template default:

- A `pair` whose value is exactly `@none` after trimming is a _suppression
  pair_. `@none` is reserved as a value. A definition value of `@none`,
  in-file or in a defs file, is a fatal error.
- Effect: the call excludes the key. Neither the in-scope definition nor the
  template default applies to it. The key's slot lines delete. Each template
  line with an inline `%key%` occurrence of the key also deletes, whole (step
  1b).
- A suppression pair requires a definition for the key in scope (in-file or
  imported) or a default for the key in the call's template. Otherwise it is a
  fatal error. With nothing to suppress, the pair is dead syntax.
- In one call, a suppression pair together with a value pair for the same key
  is a fatal error. A repeated suppression pair is a fatal error.
- Suppression is not override. A call that needs values different from the
  definition must live in a file without that definition.
- The call's template must reference the suppressed key, via slot lines or
  inline. A suppression pair for a key that the template does not reference is
  a fatal error. So is one for a key that the template iterates with
  `@@foreach`. Both are rejected directly, so the error names the suppression.
- Suppression is not consumption. The definition-consumed-by-no-call rule
  counts only calls that materialize the definition. A definition that every
  referencing call suppresses is a fatal error.
- Suppression can also arise without a pair. A key whose template default is
  `@none`, and which no call arg or definition provides, is suppressed with the
  same effects (see `@@default`).

## Default values (`@@default`), template files only

A template can declare fallback values for keys it references:

```
dflt   = ws* "@@default" " " key "(" value ( ";" value )* ")" EOL
mdflt  = multi-line form: exactly `mdef` with "@@default" for "@@define"
```

- `@@default` is valid only in template files, and only in the template's
  **default block**. The default block is a run of `dflt`/`mdflt` directives
  that starts at the first line of the file. Blank lines between and after
  them are permitted. The block and the blank lines after it are not part of
  the splice text.
- An `@@default` directive anywhere else is a fatal error: later in a
  template, in a source, or in a defs file. In sources, `default` is also a
  reserved call name.
- Values follow the same trimming and non-empty rules as `@@define`. At most
  one `@@default` per key per template. A second one is a fatal error.
- **`@none` defaults.** `@@default key(@none)` declares an _optional_ key.
  When neither the call's args nor a definition in scope provides the key, the
  key is suppressed, exactly as if the call had passed `key=@none`. `@none` is
  legal only as the sole default value. `@none` mixed with other values is a
  fatal error. A call may still pass `key=@none` explicitly. That is redundant
  but legal. An `@none` default satisfies the definition-or-default
  requirement of a suppression pair. A `@@foreach` key with an effective
  `@none` default is a fatal error, because an empty region is not
  expressible.
- The template's splice text must reference each defaulted key: slot, inline,
  or `@@foreach`. An unreferenced default is dead syntax and a fatal error.
- **Provision precedence.** For each key that a call's template references,
  exactly the first applicable source _provides_ the key:
  1. the call's args,
  2. a definition in scope (in-file `@@define` or `@@use` import),
  3. the template's default.
     A suppression pair blocks sources 2 and 3. The key is unprovided and its
     lines delete. A call arg never conflicts with a default. The
     call-arg-vs-definition collision rule and all definition-vs-definition rules
     do not change.
- Defaults are per template. The same key can default to different values in
  different templates. A call is affected only by its own template's defaults.
- Defaults have no consumption requirement. Whether a default ever
  materializes is a property of the whole corpus, not of one file. A default
  that no call consumes is not an error.
- Slot, inline, and multi-value rules apply to default values exactly as to
  passed values.

## Iteration regions (`@@foreach`), template files

A template body can mark a run of lines for per-value repetition:

```
freg   = fopen fline+ fclose
fopen  = ws* "@@foreach(" ws* key ws* ")" EOL
fline  = any line that is neither fopen nor fclose
fclose = ws* "@@end" EOL
```

- `foreach` and `end` are reserved template names. Sources use the same line
  forms as region delimiters (see source-level regions).
- The _region_ is the lines strictly between `fopen` and `fclose`. A region
  with zero lines is a fatal error.
- Regions do not nest. An `fopen` inside an open region, an `fclose` with no
  open region, and an unterminated region are fatal errors.
- A template can contain several regions, including several for the same key.
  Each region iterates the full value list. This mirrors multi-slot behavior.

## Expansion of one call

The following steps, in this order, transform the template's splice text. The
result replaces the directive line verbatim. The directive's own indentation
is discarded.

0. **Iteration regions.** For each region, top to bottom:
   - The call must provide the region's `key` with at least one value, via
     args, definition, or template default. An absent key is a fatal error.
   - N _instances_ of the region text replace the region, including its
     `fopen` and `fclose` lines, in value order, with one blank line between
     consecutive instances. In instance i, `key` is bound to value_i alone.
     Slot lines of `key` and inline `%key%` occurrences inside the region
     resolve as if the call had provided `key` with that single value.
   - After step 0 the key is _consumed_. It takes no further part in steps 1
     to 2. A slot line or `%key%` occurrence for an iterated key outside every
     region of that key is a fatal error.
1. **Slot lines.** A template line that consists of optional `ws`, then
   `%key%`, then end of line, is a _slot_ for `key`.
   - Key provided with n values: the slot line becomes n lines. Each is the
     slot's leading `ws` plus value_i, in call order.
   - Key not provided: the slot line deletes entirely, including its LF.
   - The same key can appear in several slots. Each slot expands with all the
     key's values.
     1b. **Suppressed-key line deletion.** Every remaining line that contains a
     `%key%` occurrence of a suppressed key deletes entirely, including its LF.
   - Reference counting comes first, deletion second. A suppressed key with at
     least one occurrence counts as referenced, also when another suppressed
     key deletes its line.
   - The other keys on a deleted line take no part in step 2 and consume
     nothing. A call arg that only a deleted line references is an unused key
     (fatal). A definition that only deleted lines reference stays unconsumed.
   - This is how a template makes a parameterized line optional. The caller
     passes `key=@none` and the line disappears.
2. **Inline placeholders.** The key's value replaces each remaining `%key%`
   occurrence where `key` was provided. Such a key with more than one value is
   a fatal error.
3. **Unused keys.** A provided key that no region, slot, or inline occurrence
   consumes is a fatal error.

Non-iterated keys behave identically inside and outside regions. A multi-value
slot key inside a region expands its full value list in every instance. An
inline occurrence of a multi-value key inside a region is fatal, as usual.

## Source-level iteration regions (`@@foreach`), source files

A source file can mark a run of lines for per-value repetition:

```
sreg   = sopen sline+ sclose      ; same line forms as freg
sopen  = ws* "@@foreach(" ws* key ws* ")" EOL
sline  = any line that is neither sopen nor sclose
sclose = ws* "@@end" EOL
```

- The key must have a definition in scope: an in-file `@@define` or a `@@use`
  import. There are no call args at source level, and template defaults do
  not apply. A region whose key has no definition in scope is a fatal error.
- The _region_ is the lines strictly between `sopen` and `sclose`. A region
  with zero lines is a fatal error.
- Regions do not nest. An `sopen` inside an open region, an `sclose` with no
  open region, and an unterminated region are fatal errors.
- N _instances_ of the region text replace the region, including its `sopen`
  and `sclose` lines, in value order, with one blank line between consecutive
  instances. In instance i, value_i replaces each `%key%` occurrence in the
  region text. Other `%token%` sequences stay unchanged.
- Region expansion runs after `@@use`/`@@define` collection and before call
  expansion. A directive call line inside a region is stamped per instance and
  then expands normally. This includes a call whose args contain `%key%`. The
  value is already substituted when the call expands.
- `@@define`, `@@use`, and `@@default` lines inside a region are a fatal
  error. They are position-independent, so they would otherwise hoist out of
  the region silently instead of repeating.
- A file can contain several regions, including several for one key. Each
  region iterates the full value list.
- A region consumes its key's definition. The definition-consumed-by-no-call
  rule counts region use as consumption. One definition can feed both regions
  and calls in the same file.
- A `%key%` occurrence outside every region of that key has no dedicated
  error. It survives to the unresolved-placeholder postcondition.

## Postconditions (whole expanded file)

After all calls expand, it is a fatal error if the output contains:

- a line whose first non-`ws` characters are `@@` (an unexpanded, malformed,
  or template-injected directive), or
- an occurrence of `%[a-z0-9_]+%` (an unresolved placeholder). This is also
  how a missing required inline key surfaces.

So `@@` at start of line and `%token%` sequences are reserved in preprocessed
sources and in templates.

## Errors

All errors are fatal. Each must name the source file and the construct.

- unknown template (file `<templates-dir>/<name>.baf` missing)
- malformed directive line
- empty value in args
- inline placeholder bound to a multi-value key
- provided key unused by the template
- `@@define`:
  - duplicate definition for a key
  - call arg that collides with a definition
  - definition consumed by no call
  - malformed or unterminated `@@define`
- `@@use`:
  - missing defs file
  - duplicate `@@use` of one name
  - non-define content in a defs file
  - defs file with zero definitions
  - key collision between imports, or between an import and an in-file
    definition
  - imported definition consumed by no call
- `@none`:
  - key with neither a definition in scope nor a default in the call's
    template
  - `@none` mixed with a value pair for the same key, or repeated
  - suppressed key not referenced by the call's template, or iterated with
    `@@foreach`
  - `@none` as a definition value
- `@@default`:
  - `@@default` outside a template's default block
  - duplicate `@@default` for a key
  - defaulted key not referenced by the template
  - empty value in `@@default`
  - `@none` mixed with other values in `@@default`
  - malformed or unterminated multi-line `@@default`
- template regions:
  - nested, unterminated, unopened, or empty `@@foreach` region
  - malformed `@@foreach` line
  - call on a template with a region whose key is not provided
  - iterated key referenced outside its region(s)
- source-level regions:
  - nested, unterminated, or empty region
  - `@@end` without `@@foreach`
  - malformed `@@foreach` line
  - region key with no definition in scope
  - `@@define`/`@@use`/`@@default` inside a region
- postcondition violations above

## v1 compatibility

v1 (bare `@@name`, verbatim splice) is the subset of v2 with no args and a
template without slots or placeholders. All v1 sources and templates remain
valid, with one relaxation: a template may contain slot lines. A bare call on
such a template deletes them. This is how optional guards default to absent.

## Version history

- v4.2 (2026-08-07): `@none` defaults. `@@default key(@none)`, sole value
  only, declares an optional key. Unprovided means suppressed.
- v4.1 (2026-08-07): inline `@none` line deletion (step 1b). Before, a
  suppression pair for an inline-referenced key was a fatal error.
- v4 (2026-08-04): source-level `@@foreach(key)`…`@@end` regions,
  definition-fed. Calls are legal inside regions. `@@define`/`@@use`/
  `@@default` inside a region is fatal.
- v3.2 (2026-07-29): `@@default`. Provision precedence: call args >
  definitions > defaults. `@none` also suppresses defaults. `default`
  reserved.
- v3.1 (2026-07-29): `@none` suppression pairs. `@none` reserved as a value.
- v3 (2026-07-28): `@@foreach(key)`…`@@end` regions in template bodies.
- v2.2 (2026-07-27): `@@use`. `use` reserved.
- v2.1 (2026-07-26): `@@define`. `define` reserved.
- v2 (2026-07-25): args (one-line and multi-line), slot lines, inline
  placeholders, postconditions.
- v1 (2026-07-25): bare `@@name` whole-line splice.

## Deliberately out of scope

No evaluation, no conditionals, no recursion, no mid-line directives. Also not
defined: nested regions, tuple/multi-axis iteration, author-controlled
instance separators, and literal value headers on source-level regions
(`@@foreach(p=1;…)`). v4 chose definition-fed keys instead.
