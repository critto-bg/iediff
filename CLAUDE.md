# CLAUDE.md

Guidance for working in this repo. See `README.md` for project intent.

## What this is

`iediff` parses, prints, and diffs Infinity Engine resource files, developed and
tested against a real BG2:EE install. See `README.md` for goals.

## Mod-specific features

Some features serve a *specific mod*, not the engine or the wider toolchain: they
implement one mod's own build convention rather than a game file format or a
WeiDU feature. (Contrast the BAF compiler's `--using`, which is core WeiDU — a
mod merely provided the tra files to test it against.) So far the only such
feature is `--templates`: ImprovedAnvil's `@@name` preprocessor in
`src/preprocess.rs`, mirroring `game/ImprovedAnvil/lib/tpa/baf_preprocessor.tpa`.
Both implementations follow one normative grammar, `baf-preprocessor-grammar.md`,
so any divergence is a bug in one of them. Keep this code identifiable and
loosely coupled to the engine core — as it grows it may be extracted into a
separate binary. Verify it against its own oracle (the mod's own staged output
under `game/ImprovedAnvil/workspace/baf`), not just the engine.

## Hard constraints

- **Rust, standard library only.** No external crates — this is educational.
  Keep code simple, safe, and idiomatic.
- **Robust error handling, no panics.** No `unwrap()`/`expect()`/`panic!`,
  no indexing that can panic, no `unsafe`. Parsers take untrusted bytes:
  return `Result` with a descriptive error type and bounds-check every read.
  (`unwrap_or`/`unwrap_or_else`/`unwrap_or_default` are fine — they can't panic.)
- **Incremental.** Add formats and features one at a time; don't scaffold ahead
  of need.
- **Brevity.** In docs, output, and comments alike.

## Comments

Comments explain _why_, not _what_ — the code already says what it does.

- State each fact once. Put the rationale at the definition; don't restate it at
  every call site. A `[link]` beats a repeated paragraph.
- A short note beats a long one. Cut narration of the obvious, redundant
  examples, and hedging.
- Keep the load-bearing "why": a non-obvious format quirk, a decision that could
  look wrong, an invariant a reader must not break.

## File structure

Every source file follows this order, top to bottom:

1. Module doc comment (`//!`) and any inner attributes (`#![…]`).
2. `mod` / `pub mod` declarations.
3. `use` statements, grouped std → external crates → local (`crate::`), and
   alphabetized within each group. (External is usually empty — stdlib only.)
4. Constants and statics.
5. Types, each **immediately followed by its own impls** — `struct Foo`, then
   `impl Foo`, then the next type. Not all types first and all impls after.
   - Order types by importance: the module's central type first, supporting
     types (error enums, small records) after.
   - Derive attributes sit directly above their type.
   - Within a type's impls: the inherent `impl Foo` first, then trait impls
     (`Display`, `Error`, `From`, …). A trait impl lives with its `Self` type —
     `impl From<Bar> for Foo` goes with `Foo`, not `Bar`.
6. Free functions, most-public/most-important first, most-internal last.
7. `#[cfg(test)] mod tests` at the very bottom; its contents follow 1–6, with
   all setup (helpers, fixtures) before the `#[test]` cases.

Two cross-cutting rules for ordering methods (in 5) and free functions (in 6):

- **Visibility first:** `pub` → `pub(crate)` → private.
- **Cohesion as the tiebreaker:** within a visibility tier, group by role
  (construct → transform → consume/emit) so related items stay together. A small
  private helper used by one method may sit right after it rather than at the end.

## Layout

- `src/main.rs` — crate root: shared, format-agnostic building blocks
  (`Reader`, `Truncated`, `StrrefUse`, `ResrefUse`, `parse_asciiz`,
  `describe_tag`) plus the CLI driver.
- `src/resource.rs` — `ResourceManager`: the coordinator that resolves a
  resource through the KEY and serves its bytes from a lazily-opened, cached
  BIF. One resource at a time: resolve, read, decode.
- `src/search.rs` — finding resources: by name prefix, and by what references
  them (`--uses`). Reaches the index through two `pub(crate)` methods on
  `ResourceManager` (`index_entries`, `read_for_scan`) and owns the per-format
  dispatch.
- `src/script.rs` — the script-call `Vocabulary`: `ACTION.IDS` / `TRIGGER.IDS`
  signatures read from the install, plus the curated meaning of each parameter
  name (which `S:ResRef*` is an item, which is a variable), so a call's argument
  can be typed. Also the reverse maps (name→signature, symbol→value) the BAF
  compiler needs. The calls live in the closed-source executable, but their
  numeric ids and the value tables they point at (`SPELL.IDS`, `OBJECT.IDS`) are
  read from the game, since mods extend the latter.
- `src/formats/bcs.rs` — compiled scripts: the raw parse/encode layer (byte-
  exact), plus decompilation to BAF text using the vocabulary.
- `src/formats/baf.rs` — the BAF compiler: textual script back to the record
  form, the inverse of the bcs decompiler.
- `src/fixture.rs` — test-only. Writes a synthetic game tree (KEY + BIF +
  `override/`) to a temp dir, so search tests can assert exact answers without
  depending on the local install, which shifts with every mod. Between them its
  resources name every type in `REFERENCEABLE_KINDS` and cover every format in
  `SCANNED_TYPES`; the search tests assert that coverage, so adding a format or
  a reference type fails the suite until the fixture gains a witness.
- `src/formats/` — one submodule per resource format (`key`, `bif`, …). Each
  owns its own error type that wraps the shared `Truncated` via `From`.
- `md/` — symlink: IESDP format docs. Authoritative reference for every format.
  Start at `md/iesdp/file_formats/index.md`; conventions in
  `md/iesdp/file_formats/general.md`.
- `game/` — symlink: BG2:EE install used as test data.
- Both symlinks are git-ignored and local-only, so no test may depend on them:
  each format's unit tests build their own byte fixtures, and whole-install
  tests use `src/fixture.rs`.

## How the game data fits together

All multi-byte integers are **little-endian**. Core types (`md/iesdp/file_formats/general.md`):
`resref` = 8-byte resource name (NUL-padded); `strref` = 32-bit index into TLK;
`word` = u16, `dword` = u32.

Resource lookup is a two-level index:

1. **KEY** (`game/chitin.key`, format `key_v1.md`) — header lists BIF entries
   and resource entries. Each resource entry = `resref` + 16-bit type + 32-bit
   locator. Locator bits: 31-20 = BIF (source) index, 19-14 = tileset index,
   13-0 = file index within the BIF.
2. **BIF** (`game/data/*.bif`, format `bif_v1.md`) — archive; file entries are
   matched by the low bits of the locator. (EE ships plain `BIFF V1`; compressed
   `BIFC`/`BIF ` variants also documented.)
3. **TLK** (`game/lang/<locale>/dialog.tlk`, format `tlk_v1.md`) — resolves a
   `strref` to its display string.

The **override** directory (`game/override/`) sits in front of this index. It
holds loose files named `<resref>.<ext>`; any resource present there shadows the
archived copy, so resolution checks override first, then falls back to KEY→BIF.
This is how mods edit resources (the updated file is dropped into override) and
how they add wholly new ones (which exist only in override, with no KEY entry).
`ResourceManager` honours this priority by default; `--biff` skips override to
reach the untouched original.

Each resource type then has its own format under `md/iesdp/file_formats/ie_formats/`
(e.g. `cre_v1.md`, `itm_v1.md`, `2da.md`). Many formats are versioned; read the
4-byte signature + version from the header before parsing. Some text formats
(2DA, IDS) may be XOR-encrypted (see `encryption.md`).

A natural build order: KEY → BIF → TLK → individual resource formats, since the
first three are needed to find and name anything.

## BCS / BAF verification

BCS is a compiled script (an ASCII token stream); BAF is its textual form. The
codec was verified against the install's own scripts and WeiDU (the community
compiler used as an oracle), by three round-trip metrics.

Counts are per *stored copy*, not per name: a script the override layer shadows
is two different byte sequences, and both must round-trip. The local install
holds 5535 — a total mods change, so it is the last sweep's denominator, not a
constant.

- **M1** — `encode(parse(bcs)) == bcs`, byte for byte. Holds for all 5535.
- **M2** — our decompiled BAF matches WeiDU's, call for call (~99.4%); the rest
  is WeiDU sugar we render equivalently.
- **M3** — `compile(decompile(bcs)) == bcs` through the full BAF compiler. Holds
  for 5522/5535, both from the comment-free BAF and from the commented text the
  CLI prints — the two must agree, since a comment that escapes its line turns
  into code (see [`one_line`](src/formats/bcs.rs)).

Exceptions that are **normal, not bugs**:

- The 13 scripts M3 cannot round-trip are BioWare movement scripts carrying
  uninitialised junk in slots no call reads. WeiDU cannot round-trip them
  either; M1 still holds, and we preserve the junk in a `// unmodelled` comment.
- A few end-of-line comments differ from WeiDU: a trailing space in a name, and
  death-variable creature names passed as string arguments, which we don't
  resolve (they need a curated call list).

### WeiDU (the oracle)

`game/weidu` is the community compiler. **Run it from the game root** — it
resolves `chitin.key` and `lang/` relative to the working directory, and always
pass `--out <dir>` so it writes there rather than polluting the game tree.

- Decompile: `./weidu --out <dir> file.bcs` → `<dir>/file.baf`. Output carries
  `// ~strref~` comments by default; add `--nocom` to drop them (for structural
  diffs).
- Compile: `./weidu --out <dir> file.baf` → `<dir>/file.bcs`.
- Extract archived resources: `./weidu --out <dir> --biff-get '<regexp>'`, e.g.
  `'^.*\.BCS$'`.
- `--parse-check <TYPE> <file>` returns 0 if the file parses (`TYPE` = `BAF`,
  `D`, …).

## Workflow

- `cargo build` / `cargo run` to compile and run.
- Verify binary parsing against the real files under `game/` and cross-check
  field offsets with the matching doc in `md/`.
- Commit messages are a single one-line summary — no body, no `Co-Authored-By`
  or other trailers.
