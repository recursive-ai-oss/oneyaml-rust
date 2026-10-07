# Migrating from `serde_yaml`

`serde_yaml` is archived; `serde_yaml_ng` and `serde_norway` are its
maintained forks. This guide moves code from any of them to `oneyaml`.
Where it says `serde_yaml`, it means all three: they share one code base
(libyaml-based, version 0.9 API).

Every example below compiles and runs as a test (the guide is a doc test of
the unpublished `oracles` crate: `cargo test -p oracles --doc`). Every claim
about how `oneyaml` and `serde_yaml` differ is checked by the compat
differential, `crates/oracles/tests/compat_differential.rs`, against
`serde_yaml_ng` 0.10.0 (see [Divergences](#divergences)).

## Contents

1. [The `use` swap](#the-use-swap)
2. [`Compat::serde_yaml()`](#compatserde_yaml)
3. [`Value`](#value)
4. [Errors](#errors)
5. [Duplicate keys](#duplicate-keys)
6. [Merge keys (`<<`)](#merge-keys-)
7. [YAML 1.1 behaviours](#yaml-11-behaviours)
8. [Enums](#enums)
   * [Tagged scalar variants: `resolve_tagged_variant_content`](#tagged-scalar-variants-resolve_tagged_variant_content)
9. [`from_value` / `to_value`, `Mapping`, `Sequence`](#from_value--to_value-mapping-sequence)
10. [Several documents](#several-documents)
11. [Limits](#limits)
12. [Divergences](#divergences)
13. [Not (yet) in `oneyaml`](#not-yet-in-oneyaml)

## The `use` swap

The entry points keep `serde_yaml`'s names, so most code
migrates by replacing the crate name:

| `serde_yaml` | `oneyaml` |
|---|---|
| `from_str`, `from_slice`, `from_reader` | the same, plus `*_with(.., &Settings)`; `from_slice` needs `DeserializeOwned` (see below) |
| `to_string`, `to_writer` | the same, plus `*_with(.., &Settings)` |
| `from_value`, `to_value` | the same, plus `*_with` |
| `Value`, `Mapping`, `Number`, `Sequence` | the same names (see [`Value`](#value)) |
| `value::Tag`, `value::TaggedValue` | `Tag`, `TaggedValue` |
| `Deserializer::from_str` (iterate for several documents) | the same |
| `Serializer::new(writer)` | the same; `Serializer::with_settings` |
| `Error`, `Location` | the same names; `Error` also has a `code()` |

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Config {
    name: String,
    port: u16,
    tags: Vec<String>,
}

let src = "name: web\nport: 8080\ntags:\n- api\n- internal\n";
let config: Config = oneyaml::from_str(src)?; // was serde_yaml::from_str
assert_eq!(config.port, 8080);

// oneyaml's own layout indents sequences under their key ...
assert_eq!(
    oneyaml::to_string(&config)?,
    "name: web\nport: 8080\ntags:\n  - api\n  - internal\n"
);
// ... and `Compat::serde_yaml()` writes what serde_yaml writes.
let compat = oneyaml::Compat::serde_yaml();
assert_eq!(oneyaml::to_string_with(&config, &compat)?, src);
# Ok::<(), oneyaml::Error>(())
```

`from_slice` decodes its bytes first (UTF-8, UTF-16, UTF-32, with a BOM),
so unlike `serde_yaml::from_slice` it cannot lend `&str` fields out of
the input: its target is `DeserializeOwned`. To borrow, check the bytes
are UTF-8 (`std::str::from_utf8`) and use `from_str`.

Plain scalars read into string fields with their source text, as in
`serde_yaml`: `port: 8080` reads into a `String` as `"8080"`, and
`version: 1.10` as `"1.10"` (not `"1.1"`). A quoted scalar never reads
into a number or bool (`'8080'` into `u16` is an error), again as in
`serde_yaml`.

## `Compat::serde_yaml()`

A preset `Settings` value for migration. Everything it
does is an opt-in departure from YAML 1.2, off by default:

| | Default | `Compat::serde_yaml()` |
|---|---|---|
| Duplicate keys | error `Y1013` | last wins, warning `Y1014` |
| `0b1010`, `-0x10`, `+0x10` | strings (not YAML 1.2) | integers, as `serde_yaml` reads them (`yaml11_int_forms`) |
| `!Square 2.0` into an enum | a tagged string: no `f64` content | `Square(2.0)`, read and written as `serde_yaml` does (`resolve_tagged_variant_content`, see [Enums](#enums)) |
| Output layout | sequences indented under their key | `serde_yaml`'s layout, byte for byte where possible (`Layout::SerdeYaml`) |
| Limits | `Limits::default()` | `Limits::default()` (presets never change limits) |

Refine a preset like any `Settings`:

```rust
use oneyaml::{Compat, DuplicateKeys, Value};

let strict = Compat::serde_yaml().duplicate_keys(DuplicateKeys::Error);
assert!(oneyaml::from_str_with::<Value>("a: 1\na: 2\n", &strict).is_err());

let v: Value = oneyaml::from_str_with("mask: 0b1010\n", &Compat::serde_yaml())?;
assert_eq!(v["mask"].as_u64(), Some(10));
let v: Value = oneyaml::from_str("mask: 0b1010\n")?; // YAML 1.2: a string
assert_eq!(v["mask"].as_str(), Some("0b1010"));
# Ok::<(), oneyaml::Error>(())
```

Output follows `serde_yaml`'s layout except where `serde_yaml`'s output
does not read back correctly. The layout differences are listed, with
reasons, in `crates/oracles/tests/serde_yaml_compat.rs` (for example,
U+2028 in a single-quoted string); the differences in how strings are written (which
ones are quoted) and in how input is read are listed in
`crates/oracles/tests/compat_differential.rs` (see
[Divergences](#divergences)).

## `Value`

`oneyaml::Value` has the same variants as `serde_yaml::Value` (`Null`,
`Bool`, `Number`, `String`, `Sequence`, `Mapping`, `Tagged`), mapping keys
of any `Value`, and insertion order. The differences:

- **Tags are kept.** `serde_yaml` keeps only local tags (`!thing`); it drops
  `!!set`, `!!omap`, `!!binary`, and tags from `%TAG` directives. oneyaml
  keeps every tag the schema does not resolve. Core tags that the schema
  does resolve (`!!str`, `!!int`, `!!float`, `!!bool`, `!!null`, `!!map`,
  `!!seq`) decide the type and are not kept. A tag's text is the full tag:
  `Tag::new("!x")` and `Tag::new("x")` are different tags (in `serde_yaml`
  they are equal).
- **A custom-tagged scalar is a string.** `!celsius 21` is the tagged
  *string* `"21"`: a custom tag gives the schema no licence to read it as a
  number. `serde_yaml` resolves the content (`21`).
- **The non-specific tag `!` makes a string** (YAML 1.2 §6.9.1): `! 12` is
  the string `"12"`, not a value tagged `!`.
- **Numbers of any size.** `Number` holds any integer, beyond 64 and 128
  bits; `serde_yaml`'s `Value` refuses integers wider than 64 bits.
  Equality is by value, whatever the radix: `0x10 == 16`.
- **`Sequence` is a type, not `Vec<Value>`.** It has non-panicking
  `get`/`push`/`insert_at`/`remove_at`/`iter`/`len`, and `as_slice()`,
  `into_vec()`, `From<Vec<Value>>` to reach a `Vec`.
- **Indexing never panics, and there is no `IndexMut`.** `v["a"]["b"]`
  reads (a missing entry is `Null`, as in `serde_yaml`); to write, use
  `get_mut`, `as_mapping_mut`, `as_sequence_mut`.

```rust
use oneyaml::{Mapping, Number, Tag, Value};

let v: Value = oneyaml::from_str(
    "set: !!set {a: ~}\ntemp: !celsius 21\nhex: 0x10\nbig: 123456789012345678901234567890\n",
)?;
let set = v["set"].as_tagged().expect("kept");
assert_eq!(set.tag, "tag:yaml.org,2002:set"); // as serde_yaml's `Tag == str`
let temp = v["temp"].as_tagged().expect("kept");
assert_eq!((temp.tag.to_string(), temp.value.as_str()), ("!celsius".to_owned(), Some("21")));
assert_eq!(temp.tag, Tag::new("!celsius"));
assert_eq!(v["hex"], Value::Number(Number::from(16)));
assert_eq!(v["big"].as_number().map(|n| n.to_string()).as_deref(), Some("123456789012345678901234567890"));
assert!(v["missing"]["deeper"].is_null());

// Writing: through `get_mut` / `as_mapping_mut`.
let mut v = v;
if let Some(m) = v.as_mapping_mut() {
    m.insert(Value::from("added"), Value::from(true));
}
*v.get_mut("hex").expect("present") = Value::from("sixteen");
assert_eq!(v["hex"].as_str(), Some("sixteen"));
assert_eq!(v["added"].as_bool(), Some(true));
assert_eq!(v.as_mapping().map(Mapping::len), Some(5));
# Ok::<(), oneyaml::Error>(())
```

## Errors

`oneyaml::Error` is one opaque type, like `serde_yaml::Error`. It also
carries a stable code (`Y####`, never reused; `Y0xxx` syntax, `Y1xxx`
schema, `Y2xxx` limits, `Y3xxx` serde), the path to the failing node, a
span, and warnings. `Display` is compact; `render(src)` gives a
rustc-style snippet (no dependencies); with the `miette` feature, `Error`
is a `miette::Diagnostic`.

```rust
use oneyaml::{Value, code};

let src = "a: 1\na: 2\n";
let e = oneyaml::from_str::<Value>(src).unwrap_err();
assert_eq!(e.code(), code::DUPLICATE_KEY);
assert_eq!(e.to_string(), "duplicate key `a` (2:1)");
let loc = e.location().expect("located");
assert_eq!((loc.line(), loc.column()), (2, 1)); // 1-based, as in serde_yaml
assert!(e.render(src).contains("first defined here"));

#[derive(serde::Deserialize, Debug)]
struct Server { port: u16 }
let e = oneyaml::from_str::<Server>("port: 99999\n").unwrap_err();
assert_eq!(e.path().map(|p| p.to_string()).as_deref(), Some("port"));
```

`from_str` ignores warnings (as `serde_yaml` has none). To see them, use
`from_str_with_diagnostics`:

```rust
use oneyaml::{Compat, Value};

let (v, warnings) =
    oneyaml::from_str_with_diagnostics::<Value>("a: 1\na: 2\n", &Compat::serde_yaml())?;
assert_eq!(v["a"].as_u64(), Some(2));
assert_eq!(warnings[0].code, oneyaml::code::DUPLICATE_KEY_LAST_WINS);
# Ok::<(), oneyaml::Error>(())
```

## Duplicate keys

YAML requires unique keys, so a duplicate is an error by default (`Y1013`,
with a "first defined here" label). `serde_yaml` also refuses duplicates in
`Value` and in structs, but its typed maps (`HashMap`, `BTreeMap`) keep the
last value. `Compat::serde_yaml()` (or `.duplicate_keys(DuplicateKeys::LastWins)`)
keeps the last value everywhere — `Value`, structs, and maps — and always
records a warning (`Y1014`).

## Merge keys (`<<`)

YAML 1.2 has no merge keys: by default `<<` is an ordinary string key, as
in `serde_yaml`. `serde_yaml` merges only through `Value::apply_merge`, so
typed reads need a detour (`from_str::<Value>`, `apply_merge`,
`from_value`), which loses the source text of scalars (a `1` then no longer
reads into a `String` field). oneyaml merges while reading, into any
target, with one setting:

```rust
use serde::Deserialize;
use std::collections::BTreeMap;
use oneyaml::Settings;

#[derive(Deserialize)]
struct Service { image: String, restart: String, environment: BTreeMap<String, String> }
#[derive(Deserialize)]
struct Compose { services: BTreeMap<String, Service> }

let src = "\
x-common: &common
  restart: unless-stopped
  environment: {TZ: UTC, WORKERS: 4}
services:
  web:
    <<: *common
    image: nginx
";
let c: Compose = oneyaml::from_str_with(src, &Settings::new().merge_keys(true))?;
assert_eq!(c.services["web"].restart, "unless-stopped");
assert_eq!(c.services["web"].environment["WORKERS"], "4");
# Ok::<(), oneyaml::Error>(())
```

Merges through aliases count toward the alias budget (`Limits`). In a
`Value`, the mapping's own keys come first and merged keys follow
(`apply_merge` moves the mapping's last key into the `<<` slot instead).

## YAML 1.1 behaviours

`serde_yaml` resolves plain scalars close to the YAML 1.2 Core schema, as
oneyaml does: `yes`, `no`, `on`, `off` are strings; `12:30` is a string;
`1_000` is a string; `0o17` is 15. Three differences remain (see
[Divergences](#divergences)):

- **A leading zero**: YAML 1.2 Core reads `0644` as the decimal integer
  644; `serde_yaml` reads it as the string `"0644"`. Quote such values
  (`'0644'`) if they are strings, or write `0o644` for octal.
- `-0o17` is an integer in `serde_yaml` and a string in oneyaml (a signed
  `0o` is neither YAML 1.1 nor 1.2).
- A float that overflows, such as `1e400`: YAML 1.2 Core reads it as
  infinity; `serde_yaml` reads it as the string `"1e400"`. (Written as a
  string, oneyaml quotes it, `'1e400'`, where `serde_yaml` writes it plain.)

Reading `debug: yes` into a `bool` field is therefore an error
(`Y3000`). The error says why: ``invalid type: string "yes", expected a
boolean; YAML 1.2 reads `yes` as a string (its booleans are `true` and
`false`)``, with help to write `true` or to turn on the 1.1 booleans below,
and a suggested fix that writes `true`.

Whatever the settings, oneyaml writes a string so that it reads back as
the same string under the settings it was written with: with
`yaml11_booleans(true)`, the string `no` is written `'no'`; with
`sexagesimal(true)`, `12:30` is written `'12:30'` (in both layouts).

Files written for YAML 1.1 readers (PyYAML, older tools) need the 1.1
behaviours, each a named setting, all together in `Compat::yaml_1_1()`
(checked against PyYAML in `crates/oracles/tests/compat_yaml11.rs`):

```rust
use oneyaml::{Compat, Settings, Value};

let src = "enabled: yes\nmode: 0644\ntime: 1:30\nsize: 1_000\nmask: 0b1010\n";
let v: Value = oneyaml::from_str_with(src, &Compat::yaml_1_1())?;
assert_eq!(v["enabled"].as_bool(), Some(true));  // yaml11_booleans
assert_eq!(v["mode"].as_u64(), Some(0o644));     // legacy_octal
assert_eq!(v["time"].as_u64(), Some(90));        // sexagesimal
assert_eq!(v["size"].as_u64(), Some(1000));      // digit_separators
assert_eq!(v["mask"].as_u64(), Some(10));        // yaml11_int_forms

// One behaviour at a time:
let v: Value = oneyaml::from_str_with(src, &Settings::new().yaml11_booleans(true))?;
assert_eq!(v["enabled"].as_bool(), Some(true));
assert_eq!(v["mode"].as_u64(), Some(644));
# Ok::<(), oneyaml::Error>(())
```

`%YAML 1.1` documents are read as YAML 1.2 with a warning;
`Settings::honor_version_directive(true)` switches such documents to
`Compat::yaml_1_1()`.

## Enums

`serde_yaml` 0.9 writes and reads enum variants as tags (`!Circle {r: 1}`);
it refuses the map form (`Circle: {r: 1}`) unless the field uses
`serde_yaml::with::singleton_map`. oneyaml reads both forms. By default it
writes a tag for a variant around a collection or a string (`!Circle`,
`!Named x`), and the map form for a variant around a number, bool, or null
(`Square: 2.0`), because under YAML 1.2 a tagged scalar is a string (see
`Value`):

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Shape { Unit, Circle { r: f64 }, Pair(i32, i32), Named(String), Square(f64) }

let tags = "[Unit, !Circle {r: 1.5}, !Pair [1, 2], !Named x]";
let maps = "[Unit, Circle: {r: 1.5}, Pair: [1, 2], Named: x]";
let a: Vec<Shape> = oneyaml::from_str(tags)?;
let b: Vec<Shape> = oneyaml::from_str(maps)?;
assert_eq!(a, b);

assert_eq!(oneyaml::to_string(&Shape::Circle { r: 1.5 })?, "!Circle\nr: 1.5\n");
assert_eq!(oneyaml::to_string(&Shape::Square(2.0))?, "Square: 2.0\n");
// By default a tagged number is a tagged *string* (see `Value`), so
// `!Square 2.0`, which serde_yaml writes, does not read into an `f64`:
assert!(oneyaml::from_str::<Shape>("!Square 2.0").is_err());
# Ok::<(), oneyaml::Error>(())
```

The variant name of a tag is its text without the leading `!`. A tag
written with a `%TAG` handle (`%TAG !e! tag:example.com,2000:` then
`!e!point`) names the variant `tag:example.com,2000:point`; give the
variant that name with `#[serde(rename = "…")]`. When the tag names one of
the enum's variants, the read costs only the tag's own suffix, however long
the `%TAG` prefix is. When it names none (a `#[serde(other)]` variant, a
hand-written visitor, or `deserialize_any`), the name is handed over as one
string: prefix and suffix are copied, at O(the tag's length) per use.

### Tagged scalar variants: `resolve_tagged_variant_content`

`serde_yaml` writes a newtype variant around a number, bool, or null as a
tagged scalar (`!Square 2.0`, `!Enabled true`, `!Maybe null`). YAML 1.2
leaves the meaning of a local tag such as `!Square` to the application
(§6.9.2), and oneyaml's schema gives it none: the node is the tagged string
`2.0`. `Settings::resolve_tagged_variant_content(true)`, on in
`Compat::serde_yaml()` only, is the named, opt-in departure that lets the
application (your enum) decide:

* **Reading.** When a *typed enum* reads a *plain* scalar with a *local*
  tag (`!Name`) as its variant `Name`, the content is resolved as an
  untagged plain scalar under the Core schema (with the document's YAML 1.1
  settings, e.g. `yaml11_int_forms`): `!Square 2.0` is `Square(2.0)`,
  `!Enabled true` is `Enabled(true)`. A string target still gets the text
  (`!Named 123` is `Named("123")`).
* **Unchanged.** `Value` keeps `Tagged("!Square", String("2.0"))`, and
  untyped reads (`deserialize_any`) see the tagged string. Quoted content
  stays a string (`!Square '2.0'` is text), and global or
  `%TAG`-expanded tags never resolve. `from_value_with` and
  `NodeRef::deserialize` read with the same setting (a `Value` keeps no
  scalar style, so `from_value_with` resolves the content of a tagged
  string whatever its quoting was: see
  [`from_value`](#from_value--to_value-mapping-sequence)).
* **Writing.** Under `Layout::SerdeYaml` read back with the setting (as in
  `Compat::serde_yaml()`), such variants are written `!Name value`, byte
  for byte as `serde_yaml` writes them; without the setting they keep the
  map form, which reads back. `to_value` builds the map form in every case
  (`Value` has no enums).

```rust
use oneyaml::{Compat, Settings, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Shape { Square(f64), Enabled(bool), Named(String) }

let compat = Compat::serde_yaml();
let shapes: Vec<Shape> =
    oneyaml::from_str_with("[!Square 2.0, !Enabled true, !Named 123]", &compat)?;
assert_eq!(
    shapes,
    [Shape::Square(2.0), Shape::Enabled(true), Shape::Named("123".into())]
);
assert_eq!(oneyaml::to_string_with(&Shape::Square(2.0), &compat)?, "!Square 2.0\n");

// Only typed enums change: the `Value` is still the tagged string.
let v: Value = oneyaml::from_str_with("!Square 2.0", &compat)?;
assert_eq!(v.untag_ref().as_str(), Some("2.0"));
// Quoted content is text.
assert!(oneyaml::from_str_with::<Shape>("!Square '2.0'", &compat).is_err());
// The setting alone, on top of any other settings:
let s = Settings::new().resolve_tagged_variant_content(true);
assert_eq!(oneyaml::from_str_with::<Shape>("!Square 2", &s)?, Shape::Square(2.0));
# Ok::<(), oneyaml::Error>(())
```

`serde_yaml::with::singleton_map` (and `singleton_map_recursive`) are
generic serde adapters, so they keep working with oneyaml if you keep the
dependency for them; oneyaml has no `with` module of its own. Without
them, oneyaml already reads the map form they write.

## `from_value` / `to_value`, `Mapping`, `Sequence`

```rust
use std::collections::BTreeMap;
use oneyaml::{Mapping, Sequence, Value};

let v = oneyaml::to_value(&BTreeMap::from([("port", 8080)]))?;
let back: BTreeMap<String, u16> = oneyaml::from_value(v.clone())?;
assert_eq!(back["port"], 8080);

let mut m = Mapping::new();
m.insert(Value::from("k"), Value::from(1));
assert_eq!(m.get(&Value::from("k")), Some(&Value::from(1)));
assert_eq!(m.get_str("k"), Some(&Value::from(1)));
assert!(m.contains_key(&Value::from("k")));
assert_eq!(m.remove(&Value::from("k")), Some(Value::from(1)));

let mut s = Sequence::from(vec![Value::from(1)]);
s.push(Value::from(2));
assert_eq!(s.get(1), Some(&Value::from(2)));
assert_eq!(s.get(9), None); // never panics
let plain: Vec<Value> = s.into_vec();
assert_eq!(plain.len(), 2);
# Ok::<(), oneyaml::Error>(())
```

Two differences from `from_str`, both because a `Value` keeps only the
meaning of a scalar, not how it was written:

* As in `serde_yaml`, a `Value` has no source text, so `from_value` reads
  a number into a `String` field only if it was a string (a `Value` read
  from `port: 8080` holds the number 8080).
* Under `resolve_tagged_variant_content` (see
  [Enums](#tagged-scalar-variants-resolve_tagged_variant_content)),
  `from_value_with` resolves the content of every tagged string a typed
  enum reads, quoted or not: `from_str_with` refuses `!Square '2.0'` as a
  `Square(f64)`, but the `Value` read from it, `Tagged("!Square", "2.0")`,
  is the same as the one read from `!Square 2.0`, and `from_value_with`
  reads it as `Square(2.0)`. It resolves to a bool or a number only, never
  to null: `!Opt 'null'` and `!Opt null` are the same `Value`, so
  `from_value_with` reads both as `Opt(Some("null"))` into an
  `Option<String>` (text is never lost), and `!Maybe null` into an
  `Option<u8>` is an error there (`from_str_with` gives `Maybe(None)`).

```rust
use oneyaml::{Compat, Value};

#[derive(Debug, PartialEq, serde::Deserialize)]
enum Shape { Square(f64) }

let compat = Compat::serde_yaml();
assert!(oneyaml::from_str_with::<Shape>("!Square '2.0'", &compat).is_err());
let v: Value = oneyaml::from_str_with("!Square '2.0'", &compat)?;
assert_eq!(oneyaml::from_value_with::<Shape>(v, &compat)?, Shape::Square(2.0));

#[derive(Debug, PartialEq, serde::Deserialize)]
enum E { Opt(Option<String>) }
// Never resolved to null through a `Value`:
let v: Value = oneyaml::from_str_with("!Opt 'null'", &compat)?;
assert_eq!(oneyaml::from_value_with::<E>(v, &compat)?, E::Opt(Some("null".into())));
# Ok::<(), oneyaml::Error>(())
```

## Several documents

As in `serde_yaml`, `from_str` reads exactly one document (`Y3009` for
more), and iterating a `Deserializer` reads each:

```rust
use serde::Deserialize;

let docs: Vec<oneyaml::Value> = oneyaml::Deserializer::from_str("a: 1\n---\nb: 2\n")
    .map(oneyaml::Value::deserialize)
    .collect::<Result<_, _>>()?;
assert_eq!(docs.len(), 2);

// A stream with no document (empty, or only comments) has zero documents
// (YAML 1.2 §9.2); serde_yaml's iterator yields one null document.
assert_eq!(oneyaml::Deserializer::from_str("# nothing\n").count(), 0);
// `from_str` agrees with serde_yaml: an empty input is null.
assert!(oneyaml::from_str::<oneyaml::Value>("")?.is_null());
# Ok::<(), oneyaml::Error>(())
```

To write several documents, call `serialize` once per document on one
`Serializer` (it writes the `---` separators), as with `serde_yaml`.

## Limits

`serde_yaml` has a fixed nesting limit of 128. oneyaml's limits are
settings: depth, alias expansion ("billion laughs"), document
size, scalar length, and document count. `Limits::default()` suits
configuration files (depth 1,024); for input from the network use
`Limits::untrusted()` (depth 128, a tighter alias budget, 16 MiB documents).
Compat presets never change limits.

```rust
use oneyaml::{Compat, Limits, Value};

let untrusted = Compat::serde_yaml().limits(Limits::untrusted());
let bomb = "a: &a [x, x, x, x, x, x, x, x, x, x]\n\
            b: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a, *a]\n\
            c: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b, *b]\n\
            d: &d [*c, *c, *c, *c, *c, *c, *c, *c, *c, *c]\n\
            e: &e [*d, *d, *d, *d, *d, *d, *d, *d, *d, *d]\n\
            f: [*e, *e, *e, *e, *e, *e, *e, *e, *e, *e]\n";
let e = oneyaml::from_str_with::<Value>(bomb, &untrusted).unwrap_err();
assert_eq!(e.code(), oneyaml::code::ALIAS_BUDGET_EXCEEDED);
assert!(e.to_string().contains("alias"));
```

Recursive Rust types are bounded separately by `Limits::max_typed_depth`
(128 by default), because serde recurses once per level.

## Divergences

Where `oneyaml` with `Compat::serde_yaml()` and `serde_yaml_ng` disagree,
the reason is one of these. The list is exact:
`crates/oracles/tests/compat_differential.rs` reads and writes the
yaml-test-suite, the bench corpus, and a migration corpus of Kubernetes,
GitHub Actions, docker-compose and config files with both libraries,
writes some 700 scalar-looking strings (and serde_yaml_ng must read each of
our outputs back as the same string), and fails on any disagreement not listed there (and on a listed one that stops
occurring). Output-layout divergences are listed in
`crates/oracles/tests/serde_yaml_compat.rs`.

| Divergence | What differs | Why |
|---|---|---|
| `rejects_valid` | `serde_yaml` refuses valid YAML 1.2: tabs as separation, `:` next to flow keys, empty keys, unknown or 1.3 directives, bare `...`, multi-line flow keys, `!!null ''` | libyaml is a YAML 1.1 parser; oneyaml passes the yaml-test-suite (correctness first) |
| `accepts_invalid` | `serde_yaml` reads input the suite marks invalid, or reports the error one document late | correctness first |
| `misreads_valid` | both read it, `serde_yaml`'s value is not the spec's (`?foo` in flow, anchors ending in `:`, trailing-space lines in block scalars) | correctness first |
| `tags_kept` | `!!set`, `!!omap`, `!!binary`, global tags are kept | `Value` keeps tags |
| `non_specific_tag` | `! 12` is the string `"12"` | YAML 1.2 §6.9.1 |
| `tagged_scalar_value` | an untyped read (`Value`) keeps `!Square 2.0` as the tagged string; typed enums agree (`resolve_tagged_variant_content`) | a custom tag gives the schema no licence to resolve |
| `enum_map_form` | oneyaml reads `Circle: {r: 1}`; `serde_yaml` needs `singleton_map` | reading more forms is compatible |
| `empty_stream` | an empty stream has zero documents for the `Deserializer` iterator | YAML 1.2 §9.2 |
| `duplicate_keys` | last wins (with a warning) in `Value` and structs | `Compat::serde_yaml()` sets `DuplicateKeys::LastWins`; oneyaml always warns about the duplicate |
| `leading_zero_int` | `0644` is 644, not `"0644"` | YAML 1.2 Core §10.3.2 |
| `signed_0o` | `-0o17` is a string | neither YAML 1.1 nor 1.2 |
| `float_overflow` | `1e400` is infinity, not a string | Core float pattern |
| `wide_integers` | integers beyond 64 bits read | `Number` holds any integer |
| `recursion_limit` | nesting deeper than 128 reads | `max_depth` 1,024 by default; presets never change limits |
| `merge_order` | key order after a merge | `apply_merge`'s swap-remove vs own-keys-first |
| `merge_via_value` | a merged `1` reads into a `String` | merging while reading keeps source text |
| `merge_key_quoted` | the strings `<<` and `=` are written quoted (`'<<': ...`), in every layout | written plain, a string key `<<` would merge into its parent in every merge-key reader |
| `core_reads_a_number` | the string `1e400` or a wide `0x...` is written quoted | YAML 1.2 reads the plain form as a number; output must read back |
| `root_indicator`, `serde_yaml_write_error` | output | `serde_yaml`'s output would not read back, or it fails to write |

## Not (yet) in `oneyaml`

- `Deserializer::from_slice` / `from_reader` (use `from_slice`/`from_reader`
  for one document, or read into a `String` and use `Deserializer::from_str`).
- `Mapping::entry` and `IndexMut` for `Value` (use `get_mut`,
  `as_mapping_mut`, `Mapping::insert`).
- `Value::apply_merge` (use `Settings::merge_keys(true)` when reading).
- A `with` module (`singleton_map`, `singleton_map_recursive`): keep
  `serde_yaml`'s, which are format-independent.
