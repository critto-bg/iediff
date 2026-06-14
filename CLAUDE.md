# CLAUDE.md

Guidance for working in this repo. See `README.md` for project intent.

## What this is

`iediff` parses, prints, and diffs Infinity Engine resource files, developed and
tested against a real BG2:EE install. See `README.md` for goals.

## Hard constraints

- **Rust, standard library only.** No external crates — this is educational.
  Keep code simple, safe, and idiomatic.
- **Robust error handling, no panics.** No `unwrap()`/`expect()`/`panic!`,
  no indexing that can panic, no `unsafe`. Parsers take untrusted bytes:
  return `Result` with a descriptive error type and bounds-check every read.
  (`unwrap_or`/`unwrap_or_else`/`unwrap_or_default` are fine — they can't panic.)
- **Incremental.** Add formats and features one at a time; don't scaffold ahead
  of need.
- Brevity in docs and output.

## Layout

- `src/` — the crate (currently just `main.rs`).
- `md/` — symlink: IESDP format docs. Authoritative reference for every format.
  Start at `md/file_formats/index.md`; conventions in
  `md/file_formats/general.md`.
- `game/` — symlink: BG2:EE install used as test data.
- Both symlinks are git-ignored and local-only. Later, small curated fixtures
  will be committed for unit tests.

## How the game data fits together

All multi-byte integers are **little-endian**. Core types (`md/file_formats/general.md`):
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

Each resource type then has its own format under `md/file_formats/ie_formats/`
(e.g. `cre_v1.md`, `itm_v1.md`, `2da.md`). Many formats are versioned; read the
4-byte signature + version from the header before parsing. Some text formats
(2DA, IDS) may be XOR-encrypted (see `encryption.md`).

A natural build order: KEY → BIF → TLK → individual resource formats, since the
first three are needed to find and name anything.

## Workflow

- `cargo build` / `cargo run` to compile and run.
- Verify binary parsing against the real files under `game/` and cross-check
  field offsets with the matching doc in `md/`.
