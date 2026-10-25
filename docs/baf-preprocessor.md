# The BAF preprocessor

ImprovedAnvil writes its creature scripts in BAF with one addition: a line that
starts with `@@` names a _template_. Before the script compiles, the
preprocessor replaces each such line with the text of that template. Templates
take values, share values across files, repeat blocks, and switch lines on and
off. This document explains each feature with a small example. Each feature
builds on the one before it.

`iediff --templates <dir>` runs this preprocessor. The mod's own WeiDU code
(`lib/tpa/baf_preprocessor.tpa`) runs the same one at install time. Both follow
one normative grammar, `baf-preprocessor-grammar.md`, and must produce the same
output byte for byte. This document is the plain-English companion to that
grammar. When the two disagree, the grammar wins.

## Terms

- **Source**: the `.baf` file you compile. It can contain `@@` lines.
- **Template**: a file `<dir>/<name>.baf`. The line `@@name` splices it.
- **Templates directory** (`<dir>`): the folder that holds the templates.
  ImprovedAnvil uses `scripts/_templates/`.
- **Defs file**: a file `<dir>/defs/<name>.baf` that holds only shared values.
- **Directive**: a line whose first non-blank characters are `@@`. A directive
  is always a whole line. It never shares a line with BAF text.
- **Key** and **value**: a name and the text it stands for, as in `key=value`.
  Keys are lowercase letters, digits, and `_`.
- **Placeholder**: `%key%` inside a template. A value replaces it.

## Run it

```sh
iediff --file S!hlkang.baf --templates ImprovedAnvil/scripts/_templates --out build
```

iediff expands the directives, then compiles the result to `build/S!hlkang.bcs`.
The expanded text is never written to disk. To inspect it, decompile the
result:

```sh
iediff --file build/S!hlkang.bcs
```

A bad directive stops the compile with an error that names the source file and
the construct. The preprocessor never guesses.

## 1. Splice a template: `@@name`

The simplest form. A template is a block of BAF. A source splices it with one
line.

```
// _templates/retreat.baf
IF
  HPLT(Myself,20)
THEN
  RESPONSE #100
    MoveToObject(Player1)
END
```

```
// source
@@retreat
```

The output is the template text, exactly as written. Many scripts can share one
block this way. A change to the template reaches all of them.

Rules:

- The template file must exist. `@@retreat` with no `retreat.baf` is an error.
- The preprocessor makes one pass, top to bottom. It does not look inside the
  spliced text again. A `@@` line inside a template is an error, not a call.
- The names `foreach`, `end`, `define`, `use`, and `default` are reserved. They
  are directives in their own right, described below. No template can take
  these names.

## 2. Pass values: `@@name(key=value)`

A template can leave gaps. A call fills them. There are two kinds of gap.

An **inline placeholder** is `%key%` inside a line. The value replaces it in
place.

A **slot line** is a line that holds nothing but `%key%` and leading blanks.
The slot line becomes one line per value. With no value, the slot line
disappears.

```
// _templates/attack.baf
IF
  %attack_guard%
  See(%attack_target%)
THEN
  RESPONSE #100
    Attack(%attack_target%)
END
```

Here `attack_guard` is a slot line and `attack_target` is inline. Pass values
in parentheses, separated by `;`:

```
// source
@@attack(attack_target=NearestEnemyOf(Myself); attack_guard=Global("Awake","LOCALS",1))
```

```
// output
IF
  Global("Awake","LOCALS",1)
  See(NearestEnemyOf(Myself))
THEN
  RESPONSE #100
    Attack(NearestEnemyOf(Myself))
END
```

Pass a key twice to give it two values. A slot line then becomes two lines, in
call order. The slot keeps its indentation. Long calls can span several lines.
Put `(` at the end of the first line, one `key=value;` per line, and `)` alone
on the last line. Every value line ends in `;`, also the last one.

```
// source
@@attack(
  attack_guard=Global("Awake","LOCALS",1);
  attack_guard=!HPLT(Myself,20);
  attack_target=NearestEnemyOf(Myself);
)
```

```
// output
IF
  Global("Awake","LOCALS",1)
  !HPLT(Myself,20)
  See(NearestEnemyOf(Myself))
THEN
  RESPONSE #100
    Attack(NearestEnemyOf(Myself))
END
```

Leave a slot key out, and its line goes away. This is how a template offers an
optional trigger:

```
// source
@@attack(attack_target=NearestEnemyOf(Myself))
```

```
// output
IF
  See(NearestEnemyOf(Myself))
THEN
  RESPONSE #100
    Attack(NearestEnemyOf(Myself))
END
```

Rules:

- A value runs from `=` to the next `;` or the closing `)`. It can hold spaces,
  commas, quotes, and parentheses. It cannot hold `;`. Blanks at both ends are
  trimmed. An empty value is an error.
- An inline placeholder takes exactly one value. Pass two values for an inline
  key and the call fails.
- A slot key can take any number of values. The same key can fill several slot
  lines. Each slot line expands with all the values.
- Leave an inline key out and the `%key%` stays in the text. The preprocessor
  reports it as an unresolved placeholder. So inline keys are required and slot
  keys are optional, unless a default applies (section 6).
- Every key you pass must be used by the template. An unused key is an error.
  This catches typos in key names.
- `@@attack()` with nothing inside is an error. For a call without values,
  write the bare form `@@attack`.
- One-line and multi-line forms do not mix. A one-line call closes `)` on the
  same line. A multi-line call has nothing after `(` and closes with a lone `)`.

## 3. Share values in a file: `@@define`

Many calls in one script often pass the same guard. A definition states the
value once for the whole file:

```
// source
@@define attack_guard(Global("Awake","LOCALS",1))

@@attack(attack_target=NearestEnemyOf(Myself))

@@attack(attack_target=Player1)
```

Both calls get `attack_guard`, as if each had passed it. Give several values
with `;` between them. The multi-line form works as for calls: `(` at the end
of the first line, one `value;` per line, `)` alone on the last. The definition
line itself leaves no trace in the output.

A definition is only sugar for a repeated argument. Slot, inline, and
multi-value rules do not change.

Rules:

- One definition per key per file. A second `@@define` for the same key is an
  error.
- A definition applies only to calls whose template uses the key. Other calls
  ignore it. In the example above, a `@@retreat` call in the same file is not
  affected.
- A call cannot pass a key that the file also defines. There is no override and
  no merge. To give one call a different value, move it to a file without the
  definition, or see `@none` (section 7).
- At least one call must use the definition. A definition that no call uses is
  an error. This catches definitions left behind after a cleanup.
- Definitions can stand anywhere in the file. By convention, put them at the
  top, before the first call.

## 4. Share values across files: `@@use`

A defs file holds definitions that many scripts need. It lives under
`<dir>/defs/` and contains only `@@define` lines, blank lines, and `//`
comments.

```
// _templates/defs/guards.baf
// shared guard: the creature only fights once it is awake
@@define attack_guard(Global("Awake","LOCALS",1))
```

```
// source
@@use(guards)

@@attack(attack_target=NearestEnemyOf(Myself))

@@attack(attack_target=Player1)
```

The output is the same as in section 3. An imported definition behaves exactly
like one written in the file. All the rules of section 3 apply to it.

Rules:

- The defs file must exist, and it must define at least one key.
- A defs file cannot contain template calls or `@@use` lines. There are no
  chained imports.
- Every definition in every imported file must be used by a call in the
  importing file. Keep each defs file to one purpose, so a script can import
  it without unused keys.
- No key can come from two places. A key defined in-file and imported, or
  imported from two files, is an error.
- Import a name once. A second `@@use` of the same name is an error.
- A defs file is not a template. `@@guards` does not splice it.

## 5. Repeat a block: `@@foreach` … `@@end`

Slot lines repeat one line per value. A region repeats a whole block.

### In a template

Wrap the lines in `@@foreach(key)` and `@@end`. The block appears once per
value of `key`, in order, with one blank line between copies. Inside each copy,
`%key%` stands for that one value.

```
// _templates/attack_each.baf
@@foreach(attack_target)
IF
  %attack_guard%
  See(%attack_target%)
THEN
  RESPONSE #100
    Attack(%attack_target%)
END
@@end
```

```
// source
@@use(guards)

@@attack_each(attack_target=NearestEnemyOf(Myself); attack_target=SecondNearestEnemyOf(Myself))
```

```
// output
IF
  Global("Awake","LOCALS",1)
  See(NearestEnemyOf(Myself))
THEN
  RESPONSE #100
    Attack(NearestEnemyOf(Myself))
END

IF
  Global("Awake","LOCALS",1)
  See(SecondNearestEnemyOf(Myself))
THEN
  RESPONSE #100
    Attack(SecondNearestEnemyOf(Myself))
END
```

Note that `attack_target` is inline, yet the call passes two values. The region
hands each copy one value, so the inline rule holds inside the region.

### In a source

A source has no call to take values from. Its region reads a definition
instead, in-file or imported. The common case is one block per party member:

```
// _templates/defs/party.baf
@@define pn(1;2;3;4;5;6)
```

```
// source
@@use(party)

@@foreach(pn)
IF
  See(Player%pn%)
  HPLT(Player%pn%,10)
THEN
  RESPONSE #100
    Attack(Player%pn%)
END
@@end
```

The output is six blocks, `Player1` through `Player6`, with a blank line
between them.

A source region can also contain a call. The region stamps the call once per
value first, then each copy expands as a normal call:

```
// source
@@use(guards)
@@use(party)

@@foreach(pn)
@@attack(attack_target=Player%pn%)
@@end
```

This gives six `attack` blocks, each with the shared guard and its own player.

Rules:

- A region cannot be empty, and regions do not nest. An `@@end` without an open
  region, or an `@@foreach` without an `@@end`, is an error.
- A template can hold several regions, also several for the same key. Each one
  repeats over the full value list.
- In a template, the call must provide the region's key with at least one
  value. An absent key is an error, because an empty region cannot be
  expressed.
- In a template, a key that a region iterates belongs to that region. A `%key%`
  for it outside every region of that key is an error.
- Keys other than the region's key behave the same inside and outside the
  region. A slot line for a multi-value key inside a region expands to all its
  values in every copy.
- In a source, the region's key must have a definition in scope. A region
  counts as use of that definition. One definition can feed both regions and
  calls in the same file.
- In a source, `@@define`, `@@use`, and `@@default` cannot stand inside a
  region.

## 6. Fallback values in a template: `@@default`

A template can supply its own value for a key, used when the call does not
pass it and no definition supplies it. Defaults stand at the top of the
template, before any BAF, one `@@default key(values)` per line. Blank lines
between and after them are fine. The output never contains them.

```
// _templates/attack_dflt.baf
@@default attack_target(NearestEnemyOf(Myself))

IF
  %attack_guard%
  See(%attack_target%)
THEN
  RESPONSE #100
    Attack(%attack_target%)
END
```

```
// source
@@use(guards)

@@attack_dflt

@@attack_dflt(attack_target=Player1)
```

The first call attacks the nearest enemy. The second attacks `Player1`. Both
get the guard from the defs file.

A key gets its value from exactly one source. The order is:

1. the call's own arguments,
2. a definition in scope (`@@define` or `@@use`),
3. the template's default.

A call argument never conflicts with a default. To override the default is the
point. The rules between arguments and definitions do not change.

Rules:

- `@@default` is legal only in a template, only at the top. Anywhere else, in
  a template, a source, or a defs file, it is an error.
- One default per key per template. The template must use every key it
  defaults.
- Defaults are per template. Two templates can default the same key to
  different values. A call sees only the defaults of its own template.
- A default has no use requirement. Unlike a definition, a default that no
  call ever relies on is fine.

## 7. Switch a key off: `@none`

A definition applies to every call that uses the key. One call can opt out by
passing `key=@none`. The key then counts as not provided. Its slot lines
disappear. Every line that holds an inline `%key%` for it disappears as well,
whole.

```
// source
@@use(guards)

@@attack_dflt(attack_guard=@none)

@@attack_dflt
```

The first block has no guard line. The second has it.

A template can make a key optional by defaulting it to `@none`. Then an unset
key is off, and a call switches it on by passing a value:

```
// _templates/attack_dflt.baf
@@default attack_target(NearestEnemyOf(Myself))
@@default attack_hp(@none)

IF
  %attack_guard%
  See(%attack_target%)
  !HPLT(Myself,%attack_hp%)
THEN
  RESPONSE #100
    Attack(%attack_target%)
END
```

```
// source
@@use(guards)

@@attack_dflt

@@attack_dflt(attack_hp=30)
```

The first block has no `HPLT` line. The second has `!HPLT(Myself,30)`.

Rules:

- `@none` is reserved. It cannot be a definition value, and in a default it
  must stand alone.
- `@none` suppresses, it does not override. A call that needs a different
  value must stand in a file without the definition.
- `@none` needs something to switch off: a definition in scope or a default in
  the template. Otherwise it is an error.
- Do not pass `@none` and a value for the same key in one call. Do not pass
  `@none` twice.
- The template must use the key, by a slot line or inline. `@none` for a key
  the template never mentions is an error. `@none` for a key a `@@foreach`
  iterates is an error too.
- Suppression is not use. A definition that every call switches off still
  counts as unused, and that is an error.
- When a line disappears because of one `@none`, the other keys on that line
  do nothing. An argument that only that line used is then an unused key.

## How one call expands

The steps run in this order on the template text. The result replaces the
`@@` line. The indentation of the `@@` line itself is dropped.

1. **Regions.** Each `@@foreach` block becomes one copy per value.
2. **Slot lines.** Each becomes one line per value, or disappears.
3. **Suppressed lines.** Each line with a `%key%` of a switched-off key
   disappears.
4. **Inline placeholders.** The single value replaces each `%key%` left.
5. **Check.** Every key the call provided was used, or the call fails.

Before any call expands, the preprocessor collects all `@@define` and `@@use`
lines of the source, and then stamps the source's own `@@foreach` regions.

After every call has expanded, two things must be true of the whole file. No
line starts with `@@`. No `%key%` remains. Either one is an error. This means
`@@` at the start of a line and `%word%` are reserved in sources and templates.

## What the preprocessor does not do

- No conditions, no arithmetic, no evaluation of any kind.
- No recursion. Spliced text is never scanned again.
- No nested regions, and no iteration over two keys at once.
- No directive in the middle of a line.
- A source region cannot list its values in place. It always reads a
  definition.

These are decisions, not gaps. The grammar lists them under "Deliberately out
of scope".

## Errors

Every error is fatal. The message names the source file and the directive or
key. The most common ones:

| You see                                                 | Cause                                                         |
| ------------------------------------------------------- | ------------------------------------------------------------- |
| `unknown template @@x`                                  | No `<dir>/x.baf`.                                             |
| `malformed directive`                                   | Bad call syntax, for example `@@x()` or a missing `;`.        |
| `key 'k' is not used by the template`                   | Typo in a key name, or a leftover argument.                   |
| `multi-value key 'k' used as an inline placeholder`     | Two values for an inline key.                                 |
| `unresolved placeholder remains after expansion: %k%`   | An inline key with no value, or a stray `%word%`.             |
| `key 'k' is also set by @@define`                       | A call passed a key the file defines.                         |
| `@@define k is used by no call`                         | Dead definition. Remove it or the call that stopped using it. |
| `@@use(f) k is used by no call`                         | The defs file is too broad for this script.                   |
| `@none for key 'k', which has no definition or default` | Nothing to switch off.                                        |
| `@@foreach key 'k' is not provided`                     | A template region with no values from the call.               |
| `@@foreach key 'k' has no definition in scope`          | A source region with no `@@define` or `@@use` for it.         |

A value with a missing `;` between two pairs is not a syntax error. The
preprocessor reads `a=1 b=2` as one value for `a`. The compiler then rejects
the BAF.

## Reference

- Normative grammar: `baf-preprocessor-grammar.md` (syntax v4.2).
- iediff implementation: `src/preprocess.rs`.
