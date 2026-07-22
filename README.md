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
DLG (atop the KEY/BIF/TLK index layer). ARE decodes every section — actors,
regions, containers/items, spawns, doors, ambients, animations, automap notes,
variables, tiled objects, projectile traps, songs, rest interruptions and the
vertex pool; only the opaque explored-cell bitmap is reported by size alone. DLG
decodes the state machine — actor lines, player transitions, and the trigger and
action scripts. Other resource types are located and summarised (type and size)
but not yet decoded. Diffing is not implemented yet.

## Build

```sh
cargo build
cargo run
```

Rust only, standard library only — no external crates, no unsafe code.

## Usage

```sh
iediff <name>[.ext]       # print a resource; a bare name or inexact match runs a prefix search
iediff <name>.ext --uses  # list the resources that reference this resource
iediff strref:<n>         # resolve a dialog.tlk string by index
iediff strref:<n> --uses  # list the resources that reference the string
iediff --file <path>      # print a discrete file by path, bypassing the index
iediff --stats            # summarise the KEY index and override directory
iediff --help             # usage guide
```

Options: `--raw` (machine-readable `key=value` output), `--biff` (bypass the
override directory and read the archived original), `--file` (read the argument
as a filesystem path, taking the type from its extension; referenced names still
resolve against the game directory — handy for inspecting modded resources before
they are copied into `override`).

The 2DA view labels each column with its 0-based index as WeiDU counts it —
column 0 is the row label — so `SET_2DA_ENTRY`/`READ_2DA_ENTRY` numbers read
straight off the dump.

`<name>.ext --uses` reports every resource that references that resource, with
the field(s) that carry the reference (`item[12]`, `actor[3].cre`,
`script_race`, `sale[26]`) and where each resource resolved from. The query must
name a type, since one resref can exist as several — and the target need not
exist, so a dangling reference is searchable too.

Two separate things govern what it can find. The **referring** formats — those
scanned for references — are the decoded binary ones: EFF, ITM, SPL, CRE, PRO,
STO, ARE, DLG. The **target** types they name in typed fields are:

| Target | Named by                                                 |
| ------ | -------------------------------------------------------- |
| ITM    | CRE `item[]`, ARE `item[]` / `key_item`, STO `sale[]`,   |
|        | ITM `replacement_item`, effect opcodes that create,      |
|        | remove or restrict an item                               |
| SPL    | CRE `known_spell[]` / `memorized_spell[]`, STO `cure[]`, |
|        | PRO spells, effect opcodes that cast, grant or remove    |
|        | a spell                                                  |
| CRE    | ARE `actor[].cre`, spawn points, rest interruptions,     |
|        | effect opcodes that summon, replace or polymorph         |
| DLG    | CRE `dialog`, ARE actor/door `dialog`, STO rumours,      |
|        | DLG `next_dlg`                                           |
| EFF    | effect opcodes that apply another effect file            |
| BCS    | CRE's five script slots, ARE area/actor/region/door      |
|        | scripts, effect opcode #82                               |
| ARE    | ARE neighbours, region `destination`, effect opcode #186 |
| PRO    | ARE `proj_trap[].projectile`                             |
| 2DA    | effect opcodes taking a custom spell/monster table       |
| BAM    | ITM/SPL icons, ARE/PRO animations, effect animations     |
| VVC    | effect animations, #256's sequencer graphic              |
| WAV    | SPL completion sound, ARE ambients/door sounds/song,     |
|        | PRO sounds, effect opcode #174                           |
| BMP    | CRE portraits, PRO `travel.palette`, effect opcode #107  |
| WED    | ARE `wed`                                                |
| MVE    | effect opcode #152                                       |

The two axes are independent: BCS is a target (creatures and areas name scripts)
but never a referrer, since script contents are not parsed; STO is the reverse.
Querying a type nothing names — IDS, STO, TIS, MOS — says so rather than
reporting "no references found", which would read as a finding.

Effect opcodes are covered in full: every resref field an effect can carry is
resolved to the type it names, across standalone EFFs and the effects embedded
in ITM, SPL and CRE. Where the engine itself accepts more than one type — "the
spell or item specified in the Resource field" (#318/#321/#324), or an animation
that may be a VVC or a BAM — every reading is offered, each as a candidate, so
searching either type finds it and neither is asserted as fact.

Beyond the typed fields, two sources are scanned as *text* and reported
separately as candidates, since nothing there types what a name refers to:

- **2DA/IDS tables** — any line where the resref appears as a whole
  whitespace-delimited token, row labels included (`ITEMEXCL.2DA` keys its rows
  by item resref; `RNDTRES.2DA` fills its cells with them).
- **DLG trigger and action scripts** — any quoted argument equal to the resref,
  reported with the call that takes it.

Script matches are typed by what the *call* takes, using the signatures in
`ACTION.IDS` / `TRIGGER.IDS`. A match is dropped only when that argument is
known to name some other type, or to name no resource at all; a call the table
cannot type still reports. Two things this buys:

- `AR0602` exists as both an area and a script. `AreaCheck("AR0602")` answers
  `ar0602.are --uses` and is correctly silent for `ar0602.bcs --uses`, which
  drops from ~264 spurious hits to none.
- `SetGlobal("done","AR0602",1)` scopes a variable *by* area name rather than
  referencing the area file, so it answers neither.

Table text cannot be typed this way — a bare token in a 2DA says nothing about
what it names — so those matches are still reported for any query type, and the
line number is all the context there is.

BCS/BAF scripts are still not scanned, so an empty result means "no typed field
and no scanned text names this", not "nothing in the game does".

`strref:<n> --uses` reports every resource that references string `<n>`, with the
field(s) that carry it and where each resource resolved from (`--biff` and
`--type <ext>` narrow the scan). The typed scan covers the decoded strref-bearing
formats (EFF, ITM, SPL, CRE, PRO, STO, ARE, DLG), including strrefs buried in item
/ spell / creature effects (the "display string" opcodes) and the actor lines,
player replies and journal entries of dialogues. Two sources are also scanned
heuristically and reported separately as candidates to judge (since nothing types
the number as a strref): 2DA/IDS text tables — any line where `<n>` appears as a
number — and DLG action scripts, where `<n>` is an argument to one of the
engine's strref-bearing actions (`DisplayString*`, journal entries, etc.). BCS/BAF scripts are not scanned.

Decoded formats: 2DA, IDS, EFF, ITM, SPL, CRE, PRO, STO, ARE, DLG. Other resource types are
located and summarised (type, size) but not parsed. Runs against `./chitin.key`
when present (so it works from a game root), else `game/chitin.key`. `--help`
lists the formats `--uses` searches and the target types it can find.

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
