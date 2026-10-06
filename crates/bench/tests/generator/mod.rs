//! A structured generator of merge-heavy YAML for the consistency checks
//! (merge keys with every value shape, complex keys
//! containing `<<`, anchored `<<` keys and values, aliases of `<<` used as
//! keys, shared merge DAGs, anchor redefinition, duplicates, and the odd
//! invalid merge or undefined alias. Deterministic (a seeded xorshift).
//!
//! Test code: the recursion here is bounded by `MAX_DEPTH`, not by input.

/// A small deterministic PRNG (xorshift64*), so runs are reproducible
/// without dependencies.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % (n.max(1) as u64)) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

const MAX_DEPTH: usize = 4;
/// Few names, so redefinitions are common (§3.2.2.2).
const NAMES: &[&str] = &["a0", "a1", "a2", "a3", "a4"];
const KEYS: &[&str] = &[
    "a", "b", "c", "x", "k", "target", "'a'", "!!str b", "1", "~",
];
const SCALARS: &[&str] = &[
    "1", "2", "a", "b", "x", "'<<'", "~", "true", "!!str 1", "<<",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A scalar anchor (`&n a`).
    Scalar,
    /// An anchored `<<` scalar: a merge key wherever it is a key.
    Merge,
    Map,
    Seq,
}

/// One document being generated: the anchors completed so far.
struct Doc<'r> {
    rng: &'r mut Rng,
    anchors: Vec<(&'static str, Kind)>,
}

impl Doc<'_> {
    fn define(&mut self, name: &'static str, kind: Kind) {
        self.anchors.retain(|(n, _)| *n != name);
        self.anchors.push((name, kind));
    }

    /// An alias to a completed anchor of one of `kinds` (rarely an
    /// undefined one).
    fn alias(&mut self, kinds: &[Kind]) -> Option<String> {
        if self.rng.chance(2) {
            return Some("*nowhere".to_owned());
        }
        let found: Vec<&str> = self
            .anchors
            .iter()
            .filter(|(_, k)| kinds.contains(k))
            .map(|(n, _)| *n)
            .collect();
        (!found.is_empty()).then(|| format!("*{}", found[self.rng.below(found.len())]))
    }

    fn name(&mut self) -> &'static str {
        self.rng.pick(NAMES)
    }

    fn scalar(&mut self) -> String {
        if self.rng.chance(15) {
            let n = self.name();
            let s = self.rng.pick(&["a", "<<", "b", "1"]);
            let kind = if s == "<<" { Kind::Merge } else { Kind::Scalar };
            self.define(n, kind);
            return format!("&{n} {s}");
        }
        self.rng.pick(SCALARS).to_owned()
    }

    /// A merge value: a mapping, a sequence of mappings, aliases, and
    /// sometimes an invalid shape.
    fn merge_value(&mut self, depth: usize) -> String {
        match self.rng.below(10) {
            0..=2 => self.map(depth + 1),
            3..=4 => {
                let n = 1 + self.rng.below(3);
                let items: Vec<String> = (0..n)
                    .map(|_| match self.alias(&[Kind::Map]) {
                        Some(a) if self.rng.chance(50) => a,
                        _ => self.map(depth + 1),
                    })
                    .collect();
                format!("[{}]", items.join(", "))
            }
            5..=7 => self
                .alias(&[Kind::Map, Kind::Seq])
                .unwrap_or_else(|| self.map(depth + 1)),
            8 => self.anchored(depth, true),
            _ => self.rng.pick(&["1", "[{a: 1}, 2]", "[]", "~"]).to_owned(),
        }
    }

    /// A key: plain, a merge key (plain, anchored, or an alias of an
    /// anchored `<<`), an alias, or a complex key (possibly with `<<`).
    fn key(&mut self, depth: usize) -> (String, bool) {
        match self.rng.below(12) {
            0..=4 => (self.rng.pick(KEYS).to_owned(), false),
            5..=6 => ("<<".to_owned(), true),
            7 => {
                let n = self.name();
                self.define(n, Kind::Merge);
                (format!("&{n} <<"), true)
            }
            8 => match self.alias(&[Kind::Merge, Kind::Scalar]) {
                Some(a) => (a, true),
                None => ("<<".to_owned(), true),
            },
            _ if depth < MAX_DEPTH => {
                let k = if self.rng.chance(60) {
                    self.map(depth + 1)
                } else {
                    self.seq(depth + 1)
                };
                (format!("? {k}"), false)
            }
            _ => (self.rng.pick(KEYS).to_owned(), false),
        }
    }

    fn entry(&mut self, depth: usize) -> String {
        let (key, maybe_merge) = self.key(depth);
        let value = if maybe_merge && self.rng.chance(85) {
            self.merge_value(depth)
        } else {
            self.node(depth + 1)
        };
        // `*a :`: a space, or the colon would be part of the alias name.
        format!("{key} : {value}")
    }

    fn map(&mut self, depth: usize) -> String {
        let n = if depth >= MAX_DEPTH {
            self.rng.below(2)
        } else {
            self.rng.below(4)
        };
        let entries: Vec<String> = (0..n).map(|_| self.entry(depth)).collect();
        format!("{{{}}}", entries.join(", "))
    }

    fn seq(&mut self, depth: usize) -> String {
        let n = self.rng.below(3);
        let items: Vec<String> = (0..n).map(|_| self.node(depth + 1)).collect();
        format!("[{}]", items.join(", "))
    }

    /// An anchored collection, defined once it is complete (an alias to
    /// its name inside it means the previous definition, or is recursive
    /// when the name is its own: both paths must agree either way).
    fn anchored(&mut self, depth: usize, map: bool) -> String {
        let n = self.name();
        let body = if map {
            self.map(depth + 1)
        } else {
            self.seq(depth + 1)
        };
        self.define(n, if map { Kind::Map } else { Kind::Seq });
        format!("&{n} {body}")
    }

    fn node(&mut self, depth: usize) -> String {
        if depth >= MAX_DEPTH {
            return self.scalar();
        }
        match self.rng.below(10) {
            0..=3 => self.scalar(),
            4..=5 => self.map(depth),
            6 => self.seq(depth),
            7..=8 => {
                let map = self.rng.chance(75);
                self.anchored(depth, map)
            }
            _ => self
                .alias(&[Kind::Map, Kind::Seq, Kind::Scalar, Kind::Merge])
                .unwrap_or_else(|| self.scalar()),
        }
    }

    /// A merge DAG sharing each level's source twice, under
    /// top-level keys `dN`.
    fn dag(&mut self, out: &mut String) {
        let levels = 1 + self.rng.below(9);
        out.push_str("z: &z {target: 1}\nd0: &d0 {a: 0}\n");
        for i in 1..=levels {
            let p = i - 1;
            let extra = if self.rng.chance(30) { ", k: 1" } else { "" };
            out.push_str(&format!("d{i}: &d{i} {{<<: [*d{p}, *d{p}]{extra}}}\n"));
        }
        out.push_str(&format!("top: {{<<: [*d{levels}, *z]}}\n"));
    }
}

/// One generated input: a block mapping of generated entries, sometimes
/// with a shared merge DAG, sometimes several documents.
pub fn generate(rng: &mut Rng) -> String {
    let docs = if rng.chance(10) { 2 } else { 1 };
    let mut out = String::new();
    for d in 0..docs {
        if d > 0 {
            out.push_str("---\n");
        }
        let mut doc = Doc {
            rng: &mut *rng,
            anchors: Vec::new(),
        };
        if doc.rng.chance(20) {
            doc.dag(&mut out);
        }
        let n = 1 + doc.rng.below(5);
        for i in 0..n {
            let (key, merge) = if doc.rng.chance(15) {
                ("<<".to_owned(), true)
            } else if doc.rng.chance(10) {
                (doc.rng.pick(KEYS).to_owned(), false)
            } else {
                (format!("k{i}"), false)
            };
            let value = if merge {
                doc.merge_value(0)
            } else {
                doc.node(0)
            };
            out.push_str(&format!("{key}: {value}\n"));
        }
    }
    out
}
