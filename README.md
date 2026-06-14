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

Early bootstrap. No commands implemented yet.

## Build

```sh
cargo build
cargo run
```

Rust only, standard library only — no external crates (this is an educational
project).

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
