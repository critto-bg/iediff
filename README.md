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

Parses and prints these formats: 2DA, IDS, EFF, ITM, SPL, CRE, PRO, STO, ARE,
DLG, BCS (atop the KEY/BIF/TLK index layer). Other resource types are located
and summarised (type and size) but not yet decoded. Diffing is not implemented
yet.

## Build

```sh
cargo build
cargo run
```

Rust only, standard library only — no external crates, no unsafe code.

## Usage

```sh
iediff <name>[.ext]       # print a resource; a bare/inexact name runs a prefix search
iediff <name>.ext --uses  # find what references this resource
iediff strref:<n>         # resolve a dialog.tlk string by index
iediff strref:<n> --uses  # find what references this string
iediff --file <path>      # print a file by path, bypassing the index
iediff --stats            # summarise the KEY index and override directory
iediff --help             # usage guide
```

Options:

- `--raw` — machine-readable `key=value` output.
- `--biff` — bypass the override directory and read the archived original.
- `--file` — read the argument as a filesystem path (type from its extension);
  referenced names still resolve against the game directory, handy for inspecting
  a modded resource before it is copied into `override`. A `.baf` is compiled to
  a `.bcs` beside it (or into `--out <dir>`).
- `--type <ext>` — with `--uses`, scan only one referring type.
- `--out <dir>` — with `--file` on a `.baf`, write the compiled `.bcs` into
  `<dir>` (which must exist) instead of beside the source.
- `--using <tra>` — with `--file` on a `.baf`, resolve `@N` string references
  from this translation file (WeiDU's `USING` clause). Repeatable; a later file
  wins a shared `@N`.
- `--templates <dir>` — with `--file` on a `.baf`, expand `@@name` directive
  lines from `<dir>/name.baf` (and `@@use(name)` definition imports from
  `<dir>/defs/name.baf`) before compiling (an ImprovedAnvil mod convention).

It runs against `./chitin.key` when present (so it works from a game root),
otherwise `game/chitin.key`.

## Finding references (`--uses`)

`--uses` answers the reverse question: _what points at this?_ It reads and parses
every candidate resource and inspects it, rather than relying on any pre-built
index. Each hit shows the field(s) carrying the reference (`item[12]`,
`actor[3].cre`, `block[0].CreateCreature.NewObject`) and where the resource
resolved from.

The query must name a type, since one resref can exist as several (`AR0602` is
both an area and a script). The target need not exist — a dangling reference is
searchable too. A query for a type nothing can name (IDS, TIS, MOS, …) says so,
rather than reporting "no references found", which would read as a finding.

Two independent axes govern what can be found. The **referring** formats — those
scanned — are the decoded ones: EFF, ITM, SPL, CRE, PRO, STO, ARE, DLG, BCS. The
**target** types they can name:

| Target | Named by                                                  |
| ------ | --------------------------------------------------------- |
| ITM    | CRE / ARE / STO inventories and item pools, effects       |
| SPL    | known/memorized spells, store cures, projectiles, effects |
| CRE    | ARE actors and spawns, effects, script `CreateCreature`   |
| DLG    | CRE / ARE `dialog`, store rumours, DLG `next_dlg`         |
| BCS    | CRE and ARE script slots, effect #82, script calls        |
| ARE    | ARE neighbours, region destinations, effect #186          |
| PRO    | ARE projectile traps                                      |
| EFF    | effects that apply another effect file                    |
| 2DA    | effects taking a custom spell/monster table               |
| BAM    | icons, animations, effect animations                      |
| VVC    | effect and sequencer animations                           |
| WAV    | spell/area/projectile sounds, effect #174                 |
| BMP    | portraits, palettes, effect #107                          |
| WED    | ARE tilemap                                               |
| MVE    | effect #152                                               |

The axes are independent: BCS is both a target and a referrer, while STO is only
a referrer — it names items, spells and dialogues, but nothing names a store, so
it is not a target.

**Effect opcodes** are covered in full — every resref an effect can carry is
resolved to its type, across standalone EFFs and the effects embedded in items,
spells and creatures. Where the engine itself accepts more than one type ("the
spell or item in the Resource field", or an animation that is a VVC or a BAM),
every reading is offered as a candidate to judge.

**Scripts** — a compiled BCS, or a DLG's trigger and action blocks — are
decompiled and each call's arguments typed by what the _call_ takes, from the
`ACTION.IDS` / `TRIGGER.IDS` signatures. These are exact matches:
`CreateCreature("cutthroat")` names a creature, `GiveItemCreate("sw1h01",…)` an
item. A spell named by number is resolved too (`ForceSpell(target,2001)` →
`SPWI001`), a reference that appears nowhere as a resref string. Typing the
argument by its call resolves the engine's namespace collisions —
`AreaCheck("AR0602")` names the area, not the same-named script; a variable
scoped by area name references neither.

**2DA/IDS tables** are the one source scanned as plain text: a bare token says
nothing about what it names, so any line carrying the resref (or, for
`strref:<n>`, the number) is reported for any query type, as a candidate.

`strref:<n> --uses` works the same way over string references, covering the
strref fields of every decoded format and the strref-bearing script calls
(`DisplayString`, journal entries).

## Scripts (BCS / BAF)

Printing a compiled script decompiles it to BAF — the textual form WeiDU uses —
with each call's arguments resolved to their symbolic names, and end-of-line
comments giving a strref's text and sound, or an item / spell / creature's
display name:

```baf
IF
  Entered([ANYONE])
THEN
  RESPONSE #100
    DisplayString(LastTrigger,14381)  // ~Trap Sprung~ [AMB_D05C]
    ForceSpell(LastTrigger,TRAP_FIREBALL)
END
```

A full BAF compiler goes the other way: `iediff --file x.baf` writes the compiled
`x.bcs`, and the round trip `compile(decompile(bcs)) == bcs` holds byte for byte
on every script in the install that round-trips at all.

Mod scripts often carry `@N` string references in place of a strref, defined in
a companion `.tra` translation file. Pass those with `--using` to compile such a
script offline:

```sh
iediff --file S!0002.baf --using setup.tra --using druidq.tra --out build
```

Each `@N` is resolved to its text and matched against an existing `dialog.tlk`
string (read-only — a string the game does not already hold is an error, not a
new entry). Output is byte-identical to WeiDU's for the same tra set.

Some mods also preprocess their BAF with their own directives. `--templates`
supports one such convention (ImprovedAnvil's): a whole line `@@name` is replaced
by `<dir>/name.baf` before compiling, so a shared fragment lives in one place. A
directive may pass `@@name(key=value; …)` args (also written over several lines)
that fill the template's `%key%` slot lines and inline placeholders. A template
may repeat a whole block per value by wrapping it in `@@foreach(key)` … `@@end`.
A file-scoped `@@define key(…)` supplies a key that calls omit, and `@@use(name)`
imports such definitions from a shared `<dir>/defs/name.baf`, so a value shared
across scripts lives in one place. A template may open with `@@default key(…)`
lines of its own — the last resort when neither the call nor a definition
provides the key — and a single call opts out of either with `key=@none`. An
unknown template, a malformed directive, or a leftover placeholder is an error.

```sh
iediff --file S!hlkang.baf --templates ImprovedAnvil/scripts/_templates --out build
```

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
- **override** — a loose-file directory that shadows the index, where mods drop
  edited or new resources. Resolution checks it first; `--biff` skips it.

From there, each resource type (CRE, ITM, SPL, ARE, DLG, 2DA, BAM, …) has its own
format. See `md/file_formats/` for the specifications.
