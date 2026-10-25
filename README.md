# iediff

A tool to browse and compare Infinity Engine game resources. Developed against
Baldur's Gate: Enhanced Edition and Baldur's Gate II: Enhanced Edition, and so
against EET, which merges both into one BG2:EE install.

## Goals

- Parse the essential Infinity Engine file formats.
- View a single resource in human-readable or machine-readable form.
- Diff two versions of the same resource in either form.

## Status

Decodes these formats, on top of the KEY/BIF/TLK index layer: 2DA, IDS, EFF, ITM,
SPL, CRE, PRO, STO, ARE, DLG, GAM, BCS. It finds every other resource type and
reports its type and size.

## Build

```sh
cargo build
cargo run
```

Rust and the standard library only. No external crates, no unsafe code.

## Usage

```sh
iediff <name>[.ext]       # print a resource (a bare or inexact name searches by prefix)
iediff <name>.ext --uses  # find what references this resource
iediff strref:<n>         # resolve a dialog.tlk string by index
iediff strref:<n> --uses  # find what references this string
iediff --edges            # dump every reference in the index, one per line
iediff --file <path>      # print a file by path, skipping the index
iediff --stats            # summarize the KEY index and override directory
iediff --help             # usage guide
```

Options:

- `--raw` — machine-readable `key=value` output.
- `--biff` — skip the override directory and read the archived original.
- `--file` — read the argument as a filesystem path, typed by its extension.
  Referenced names still resolve against the game directory. A `.baf` compiles to a `.bcs`.
- `--type <ext>` — with `--uses` or `--edges`, scan only one referring type.
- `--out <dir>` — with `--file` on a `.baf`, write the `.bcs` into `<dir>`, which
  must already exist, instead of beside the source.
- `--using <tra>` — with `--file` on a `.baf`, resolve `@N` string references
  from this translation file (WeiDU's `USING` clause). Repeatable. A later file
  wins a shared `@N`.
- `--templates <dir>` — with `--file` on a `.baf`, expand `@@name` directive
  lines from `<dir>/name.baf` first (an ImprovedAnvil mod convention).

It reads `./chitin.key` when that file exists, so it works from a game root.
Otherwise it reads `game/chitin.key`.

## Finding references (`--uses`)

`--uses` answers the reverse question: _what points at this?_ It parses every
candidate resource and reads its fields, with no pre-built index. Each hit names
the fields that carry the reference (`item[12]`, `actor[3].cre`,
`block[0].CreateCreature.NewObject`) and the layer it came from.

The query must name a type, because one resref can exist as several: `AR0602` is
both an area and a script. The target need not exist, because a dangling
reference is worth searching for. A query for a type nothing can name (IDS, TIS,
CHU) says so, rather than reporting "no references found".

Two axes decide what a search finds. The **referring** formats are the decoded
ones: EFF, ITM, SPL, CRE, PRO, STO, ARE, DLG, GAM, BCS. These are the **target**
types they name:

| Target | Named by                                                                       |
| ------ | ------------------------------------------------------------------------------ |
| ITM    | CRE / ARE / STO / GAM inventories, effects                                     |
| SPL    | known, memorized and quick spells, store cures, projectiles, effects           |
| CRE    | ARE actors and spawns, the GAM roster and familiars, effects, `CreateCreature` |
| DLG    | CRE / ARE `dialog`, store rumors, DLG `next_dlg`                               |
| BCS    | CRE and ARE script slots, effect #82, script calls                             |
| ARE    | ARE neighbors, region destinations, GAM party positions, effect #186           |
| PRO    | ARE projectile traps                                                           |
| EFF    | effects that apply another effect file                                         |
| STO    | script `StartStore`                                                            |
| 2DA    | effects with a custom spell or monster table                                   |
| BAM    | icons, animations, effect animations                                           |
| VVC    | effect and sequencer animations                                                |
| WAV    | spell, area and projectile sounds, effect #174                                 |
| BMP    | portraits, palettes, effect #107                                               |
| WED    | ARE tilemap                                                                    |
| MVE    | effect #152                                                                    |
| MOS    | script `LeaveAreaLUA` loading screens                                          |
| WMP    | script `SetWorldmap`, the GAM worldmap                                         |

The axes are independent. BCS is both a target and a referrer, while GAM is only
a referrer: nothing names a game state.

**Effect opcodes** are covered in full: the search resolves every resref an
effect carries, in standalone EFFs and in the effects inside items, spells and
creatures. The engine sometimes accepts more than one
type, as in "the spell or item in the Resource field". The search then offers
every reading as a candidate.

**Scripts** — a compiled BCS, or a DLG trigger and action block — decompile
first. The `ACTION.IDS` and `TRIGGER.IDS` signatures then type each argument by
what the call takes, so the matches are exact. A spell named by number resolves
too (`ForceSpell(target,2001)` → `SPWI001`), though that resref appears nowhere
in the bytes. Typing by the call also settles the engine's namespace collisions:
`AreaCheck("AR0602")` names the area, not the script of the same name.

**2DA and IDS tables** are the one source read as plain text. A bare token says
nothing about what it names. So any line carrying the resref, or the number for
`strref:<n>`, is a candidate under any query type.

**A game state** (`baldur.gam` and every save game) names each NPC of its roster
by script name, which BioWare's own convention makes the creature's resref. Both
GAM versions are read: BG2:EE ships `V2.0`, BG:EE still ships BG1's `V1.1`. The
engine marks an NPC it has already spawned by overwriting the first letter with
`*`, and those names match nothing. A save also carries each party member's whole
CRE inline, and those references appear under `party[0].cre.…`.

`strref:<n> --uses` works the same way for strings, over the strref fields of
every decoded format and the script calls that take one.

## Dumping the whole graph (`--edges`)

`--edges` runs the same extraction with no target to filter by. It writes one
tab-separated record per reference. The columns are source, source type, field
path, target, target type, hard or soft, target resolves (1 or 0), and source
layer. It truncates nothing and writes no header, so filtering the dump by target
reproduces `--uses` for that target. The tests assert that.

```sh
iediff --edges | awk -F'\t' '$7 == 0'  # every dangling reference in the install
iediff baldur.gam --edges              # just one resource's references
```

Records come out sorted by source type, source, field path, then target, a fixed
total order that `sort -c` accepts.

One case breaks the match with `--uses`. A 2DA or IDS token carries no type, so
the dump can only ask the index what each token names. A table that names a
resource which does not exist therefore contributes nothing, and only a typed
field reports a dangling reference.

## Scripts (BCS / BAF)

Printing a compiled script decompiles it to BAF, the textual form WeiDU uses.
Each argument resolves to its symbolic name. End-of-line comments give a strref's
text and sound, or the display name of an item, spell or creature:

```baf
IF
  Entered([ANYONE])
THEN
  RESPONSE #100
    DisplayString(LastTrigger,14381)  // ~Trap Sprung~ [AMB_D05C]
    ForceSpell(LastTrigger,TRAP_FIREBALL)
END
```

A full BAF compiler goes the other way: `iediff --file x.baf` writes `x.bcs`. The
round trip `compile(decompile(bcs)) == bcs` holds byte for byte on every script
in the install that round-trips at all.

Mod scripts often carry an `@N` string reference in place of a strref, defined in
a companion `.tra` file. Pass those files with `--using`:

```sh
iediff --file S!0002.baf --using setup.tra --using druidq.tra --out build
```

Each `@N` resolves to its text, which must already exist in `dialog.tlk`. A
string the game does not hold is an error, not a new entry. The output matches
WeiDU's byte for byte.

### BAF preprocessor

The `--templates` flag provides support for Improved Anvil's experimental BAF
preprocessor: a whole line `@@name` expands to `<dir>/name.baf` before compiling.

The directives are:

- `@@name(key=value; …)` — arguments that fill the template's `%key%` slot lines
  and inline placeholders, over several lines if needed.
- `@@foreach(key)` … `@@end` — repeat the wrapped block once per value.
- `@@define key(…)` — a file-wide value for a key the calls omit, and the values
  `@@foreach` iterates over.
- `@@use(name)` — import those definitions from a shared `<dir>/defs/name.baf`.
- `@@default key(…)` — a template's own fallback, used when neither the call nor
  a definition gives the key.
- `key=@none` — one call opts out of a definition or a default.

An unknown template, a malformed directive, or a leftover placeholder is an
error. See `docs/baf-preprocessor.md` for in-depth explanation of each directive
with examples.

```sh
iediff --file S!hlkang.baf --templates ImprovedAnvil/scripts/_templates --out build
```

## Local setup

The repo root expects three git-ignored symlinks:

- `game` → a BG2:EE installation. It provides `chitin.key`, `data/*.bif` and
  `lang/<locale>/dialog.tlk`.
- `gamebg1` → a BG:EE installation, to test the same code against the other
  game. Run the binary from that root to use it.
- `md` → a local checkout of the [IESDP](https://github.com/gibberlings3/iesdp)
  format documentation.

## Data model (overview)

A central index locates every resource, and archives hold the data:

- **KEY** (`chitin.key`) — maps each 8-character `resref` and 16-bit type to a
  locator: which BIF holds the resource, and at what index.
- **BIF** (`data/*.bif`) — archives that hold the resource data.
- **TLK** (`lang/<locale>/dialog.tlk`) — the string table. A resource names a
  string by its `strref`.
- **override** — a loose-file directory that shadows the index, where mods drop
  edited or new resources. Lookup checks it first. `--biff` skips it.

Each resource type then has its own format. See `md/file_formats/` for the
specifications.
