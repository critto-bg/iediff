# iediff

A utility to browse and compare Infinity Engine game resources (developed
against Baldur's Gate II: Enhanced Edition).

## Goals

- Parse the essential Infinity Engine file formats.
- View a single resource in human-readable or machine-readable form.
- Diff two versions of the same resource in either form.

Longer term, the tool will support programmatic patching of game resources via
diffs — including LLM-assisted modding, where a model can study a resource,
patch it, and verify the change.

## Status

Parses and prints these formats: 2DA, IDS, EFF, ITM, SPL, CRE, PRO, STO, ARE
(atop the KEY/BIF/TLK index layer). ARE decodes every section — actors, regions,
containers/items, spawns, doors, ambients, animations, automap notes, variables,
tiled objects, projectile traps, songs, rest interruptions and the vertex pool;
only the opaque explored-cell bitmap is reported by size alone. Other resource
types are located and summarised (type and size) but not yet decoded. Diffing is
not implemented yet.

## Build

```sh
cargo build
cargo run
```

Rust only, standard library only — no external crates (this is an educational
project).

## Usage

```sh
iediff <name>[.ext]   # print a resource; a bare name or inexact match runs a prefix search
iediff strref:<n>     # resolve a dialog.tlk string by index
iediff --file <path>  # print a discrete file by path, bypassing the index
iediff --stats        # summarise the KEY index and override directory
iediff --help         # usage guide
```

Options: `--raw` (machine-readable `key=value` output), `--biff` (bypass the
override directory and read the archived original), `--file` (read the argument
as a filesystem path, taking the type from its extension; referenced names still
resolve against the game directory — handy for inspecting modded resources before
they are copied into `override`).

The 2DA view labels each column with its 0-based index as WeiDU counts it —
column 0 is the row label — so `SET_2DA_ENTRY`/`READ_2DA_ENTRY` numbers read
straight off the dump.

Decoded formats: 2DA, IDS, EFF, ITM, SPL, CRE, PRO, STO, ARE. Other resource types are
located and summarised (type, size) but not parsed. Runs against `./chitin.key`
when present (so it works from a game root), else `game/chitin.key`.

## Local setup

Two git-ignored symlinks are expected in the repo root:

- `game` → a BG2:EE installation (provides `chitin.key`, `data/*.bif`,
  `lang/<locale>/dialog.tlk`).
- `md` → a local checkout of the [IESDP](https://github.com/gibberlings3/iesdp)
  format documentation.

## Data model (overview)

Resources are located through a central index and packed into archives:

- **KEY** (`chitin.key`) — maps each 8-char `resref` + 16-bit resource type to a
  locator: which BIF holds it and at what index.
- **BIF** (`data/*.bif`) — archives holding the actual resource data.
- **TLK** (`lang/<locale>/dialog.tlk`) — string table; resources reference
  strings by `strref`.

From there, each resource type (CRE, ITM, SPL, ARE, DLG, 2DA, BAM, …) has its
own format. See `md/file_formats/` for the specifications.
