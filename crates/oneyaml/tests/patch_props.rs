//! Seeded property tests of patches against a plain `Value` model of RFC 6902
//! and RFC 7396: a patch that fails leaves the bytes as they were, and one that
//! succeeds leaves text that reads as the model's result. `ONEYAML_FUZZ_CASES`
//! sets the number of cases.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, Mapping, MergePatch, Patch, PatchErrorKind, PatchOp, Settings, Value};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, p: usize) -> bool {
        self.below(100) < p
    }
}

const KEYS: [&str; 5] = ["a", "b", "c", "d", "e"];
const STRS: [&str; 10] = [
    "x", "hello", "yes", "1", "", "a: b", "#x", "null", "-", "l1\nl2",
];

fn scalar_text(r: &mut Rng) -> String {
    match r.below(8) {
        0 => "~".into(),
        1 => format!("{}", r.below(10)),
        2 => "x".into(),
        3 => "'q'".into(),
        4 => "hello world".into(),
        5 => "\"dq\"".into(),
        6 => "true".into(),
        _ => "zz".into(),
    }
}

fn flow(r: &mut Rng, depth: usize) -> String {
    if depth == 0 || r.chance(50) {
        return scalar_text(r);
    }
    if r.chance(50) {
        let n = r.below(3);
        let mut ks: Vec<&str> = KEYS.to_vec();
        let items: Vec<String> = (0..n)
            .map(|_| {
                let k = ks.remove(r.below(ks.len()));
                format!("{k}: {}", flow(r, depth - 1))
            })
            .collect();
        format!("{{{}}}", items.join(", "))
    } else {
        let n = r.below(3);
        let items: Vec<String> = (0..n).map(|_| flow(r, depth - 1)).collect();
        format!("[{}]", items.join(", "))
    }
}

// A block mapping at `ind`.
fn block_map(r: &mut Rng, ind: usize, depth: usize, out: &mut String) {
    let n = 1 + r.below(3);
    let mut ks: Vec<&str> = KEYS.to_vec();
    for _ in 0..n {
        let k = ks.remove(r.below(ks.len()));
        out.push_str(&" ".repeat(ind));
        out.push_str(k);
        out.push(':');
        block_value(r, ind, depth, out);
    }
}
fn block_value(r: &mut Rng, ind: usize, depth: usize, out: &mut String) {
    let c = if depth == 0 { 0 } else { r.below(4) };
    match c {
        0 => {
            out.push(' ');
            out.push_str(&flow(r, depth.min(2)));
            if r.chance(20) {
                out.push_str("  # c");
            }
            out.push('\n');
        }
        1 => {
            out.push('\n');
            block_map(r, ind + 2, depth - 1, out);
        }
        2 => {
            out.push('\n');
            block_seq(r, ind + 2, depth - 1, out);
        }
        _ => {
            if r.chance(30) {
                out.push_str("  # c");
            }
            out.push('\n');
            block_seq(r, ind, depth - 1, out);
        }
    }
}
fn block_seq(r: &mut Rng, ind: usize, depth: usize, out: &mut String) {
    let n = 1 + r.below(3);
    for _ in 0..n {
        out.push_str(&" ".repeat(ind));
        out.push('-');
        if depth > 0 && r.chance(30) {
            out.push('\n');
            block_map(r, ind + 2, depth - 1, out);
        } else {
            out.push(' ');
            out.push_str(&flow(r, depth.min(2)));
            out.push('\n');
        }
    }
}
fn gen_doc(r: &mut Rng) -> String {
    let mut s = String::new();
    if r.chance(20) {
        s.push_str("# head\n\n");
    }
    match r.below(5) {
        0 => {
            s.push_str(&flow(r, 3));
            s.push('\n');
        }
        1 => block_seq(r, 0, 3, &mut s),
        _ => block_map(r, 0, 3, &mut s),
    }
    s
}

fn gen_value(r: &mut Rng, depth: usize, nulls: bool) -> Value {
    let c = if depth == 0 { r.below(4) } else { r.below(7) };
    match c {
        0 => {
            if nulls {
                Value::Null
            } else {
                Value::from(7)
            }
        }
        1 => Value::from(r.below(10) as i64),
        2 => Value::from(STRS[r.below(STRS.len())]),
        3 => Value::from(r.chance(50)),
        4 | 5 => {
            let mut m = Mapping::new();
            for _ in 0..r.below(4) {
                m.insert(
                    Value::from(KEYS[r.below(5)]),
                    gen_value(r, depth - 1, nulls),
                );
            }
            Value::from(m)
        }
        _ => Value::from(
            (0..r.below(3))
                .map(|_| gen_value(r, depth - 1, nulls))
                .collect::<Vec<_>>(),
        ),
    }
}

// ---- RFC 7396 model ----
fn merge_model(target: Value, patch: &Value) -> Value {
    let Value::Mapping(pm) = patch else {
        return patch.clone();
    };
    let mut t = match target {
        Value::Mapping(m) => m,
        _ => Mapping::new(),
    };
    for (k, v) in pm.iter() {
        if v.is_null() {
            t.remove(k);
        } else {
            let old = t.remove(k).unwrap_or(Value::Null);
            t.insert(k.clone(), merge_model(old, v));
        }
    }
    Value::from(t)
}

// ---- RFC 6902 model ----
fn idx(tok: &str) -> Option<usize> {
    if tok.is_empty()
        || !tok.bytes().all(|b| b.is_ascii_digit())
        || (tok.len() > 1 && tok.starts_with('0'))
    {
        None
    } else {
        tok.parse().ok()
    }
}
fn get<'a>(v: &'a Value, toks: &[String]) -> Option<&'a Value> {
    let mut cur = v;
    for t in toks {
        cur = match cur {
            Value::Mapping(m) => m.get(&Value::from(t.as_str()))?,
            Value::Sequence(s) => s.get(idx(t)?)?,
            _ => return None,
        };
    }
    Some(cur)
}
fn get_mut<'a>(v: &'a mut Value, toks: &[String]) -> Option<&'a mut Value> {
    let mut cur = v;
    for t in toks {
        cur = match cur {
            Value::Mapping(m) => m.get_mut(&Value::from(t.as_str()))?,
            Value::Sequence(s) => {
                let i = idx(t)?;
                s.get_mut(i)?
            }
            _ => return None,
        };
    }
    Some(cur)
}
fn m_add(doc: &mut Value, toks: &[String], v: Value) -> Result<(), ()> {
    let Some((last, before)) = toks.split_last() else {
        *doc = v;
        return Ok(());
    };
    match get_mut(doc, before).ok_or(())? {
        Value::Mapping(m) => {
            m.insert(Value::from(last.as_str()), v);
            Ok(())
        }
        Value::Sequence(s) => {
            if last == "-" {
                s.push(v);
                Ok(())
            } else {
                let i = idx(last).ok_or(())?;
                s.insert_at(i, v).map_err(|_| ())
            }
        }
        _ => Err(()),
    }
}
fn m_remove(doc: &mut Value, toks: &[String]) -> Result<Value, ()> {
    let Some((last, before)) = toks.split_last() else {
        return Err(());
    };
    match get_mut(doc, before).ok_or(())? {
        Value::Mapping(m) => m.remove(&Value::from(last.as_str())).ok_or(()),
        Value::Sequence(s) => {
            let i = idx(last).ok_or(())?;
            s.remove_at(i).ok_or(())
        }
        _ => Err(()),
    }
}
fn toks(p: &str) -> Vec<String> {
    oneyaml::Pointer::parse(p).unwrap().tokens().to_vec()
}
fn m_op(doc: &mut Value, op: &PatchOp) -> Result<(), ()> {
    match op {
        PatchOp::Add { path, value } => m_add(doc, &toks(path), value.clone()),
        PatchOp::Remove { path } => m_remove(doc, &toks(path)).map(|_| ()),
        PatchOp::Replace { path, value } => {
            let t = toks(path);
            let slot = get_mut(doc, &t).ok_or(())?;
            *slot = value.clone();
            Ok(())
        }
        PatchOp::Move { from, path } => {
            let (f, t) = (toks(from), toks(path));
            if t.len() > f.len() && t.starts_with(&f) {
                return Err(());
            }
            let v = m_remove_or_root(doc, &f)?;
            m_add(doc, &t, v)
        }
        PatchOp::Copy { from, path } => {
            let v = get(doc, &toks(from)).ok_or(())?.clone();
            m_add(doc, &toks(path), v)
        }
        PatchOp::Test { path, value } => {
            if get(doc, &toks(path)).ok_or(())? == value {
                Ok(())
            } else {
                Err(())
            }
        }
        _ => Err(()),
    }
}
fn m_remove_or_root(doc: &mut Value, f: &[String]) -> Result<Value, ()> {
    if f.is_empty() {
        Ok(doc.clone())
    } else {
        m_remove(doc, f)
    }
}

fn paths(v: &Value, prefix: String, out: &mut Vec<String>) {
    out.push(prefix.clone());
    match v {
        Value::Mapping(m) => {
            for (k, x) in m.iter() {
                if let Some(k) = k.as_str() {
                    paths(x, format!("{prefix}/{k}"), out);
                }
            }
        }
        Value::Sequence(s) => {
            for (i, x) in s.iter().enumerate() {
                paths(x, format!("{prefix}/{i}"), out);
            }
        }

        _ => {}
    }
}
fn gen_path(r: &mut Rng, model: &Value) -> String {
    let mut ps = Vec::new();
    paths(model, String::new(), &mut ps);
    let base = ps[r.below(ps.len())].clone();
    match r.below(5) {
        0 => format!("{base}/{}", KEYS[r.below(5)]),
        1 => format!("{base}/-"),
        2 => format!("{base}/{}", r.below(4)),
        _ => base,
    }
}
fn gen_op(r: &mut Rng, model: &Value) -> PatchOp {
    let p = gen_path(r, model);
    match r.below(6) {
        0 => PatchOp::add(p, gen_value(r, 2, true)),
        1 => PatchOp::remove(p),
        2 => PatchOp::replace(p, gen_value(r, 2, true)),
        3 => PatchOp::move_value(gen_path(r, model), p),
        4 => PatchOp::copy(gen_path(r, model), p),
        _ => PatchOp::test(
            p,
            get(model, &toks(&gen_path(r, model)))
                .cloned()
                .unwrap_or(Value::Null),
        ),
    }
}

fn rereads_the_same(doc: &Document) -> Result<(), String> {
    let text = doc.to_string();
    match (Document::parse(text.clone()), doc.to_value()) {
        (Ok(re), Ok(v)) => {
            if re.to_value().ok() == Some(v) {
                Ok(())
            } else {
                Err(format!("the text reads back differently:\n{text}"))
            }
        }
        (Err(e), _) => {
            if doc.has_errors() {
                Ok(())
            } else {
                Err(format!("the text does not parse ({e}):\n{text:?}"))
            }
        }
        (_, Err(_)) => Ok(()),
    }
}

/// How many cases a test runs: the default, or `ONEYAML_FUZZ_CASES`.
fn case_count(default: usize) -> usize {
    std::env::var("ONEYAML_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A report of at most a few failures, so a broken run stays readable.
fn assert_no_failures(what: &str, failures: &[String], ok: usize, total: usize) {
    assert!(
        failures.is_empty(),
        "{what}: {} of {total} cases failed (first {} shown):\n{}",
        failures.len(),
        failures.len().min(5),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n=====\n")
    );
    // The generators must reach the interesting cases, not only refusals.
    assert!(
        ok * 5 >= total,
        "{what}: only {ok} of {total} cases applied"
    );
}

#[test]
fn rfc6902_results_match_a_value_model() {
    let total = case_count(1_500);
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut ok, mut edit_refusals, mut both_err) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for it in 0..total {
        let src = gen_doc(&mut r);
        let Ok(mut doc) = Document::parse(src.clone()) else {
            continue;
        };
        let Ok(model0) = doc.to_value() else { continue };
        let mut model = model0.clone();
        let mut ops = Vec::new();
        let mut model_ok = true;
        for _ in 0..1 + r.below(4) {
            let mut op = gen_op(&mut r, &model);
            // Most operations should be ones the model accepts, so that the
            // successes reach every operation; a fifth are left as drawn.
            if !r.chance(20) {
                for _ in 0..8 {
                    if m_op(&mut model.clone(), &op).is_ok() {
                        break;
                    }
                    op = gen_op(&mut r, &model);
                }
            }
            if model_ok && m_op(&mut model, &op).is_err() {
                model_ok = false;
            }
            ops.push(op);
            if !model_ok {
                break;
            }
        }
        let result = doc.apply_patch(&Patch::new(ops.clone()));
        let context = format!("case {it}\nsource:\n{src}ops: {ops:?}");
        match (model_ok, result) {
            (true, Ok(())) => match (doc.to_value(), rereads_the_same(&doc)) {
                (Ok(got), Ok(())) if got == model => ok += 1,
                (got, reread) => failures.push(format!(
                    "{context}\nresult:\n{doc}\nwanted {model:?}\ngot {got:?}\n{reread:?}"
                )),
            },
            (true, Err(e)) => {
                if doc.to_string() != src {
                    failures.push(format!("{context}\nthe failed patch changed the bytes"));
                } else if e.kind() == PatchErrorKind::Edit {
                    // The edit layer may decline a change the model can make.
                    edit_refusals += 1;
                } else {
                    failures.push(format!("{context}\nrefused: {e}"));
                }
            }
            (false, Ok(())) => failures.push(format!("{context}\nshould have failed:\n{doc}")),
            (false, Err(_)) => {
                both_err += 1;
                if doc.to_string() != src {
                    failures.push(format!("{context}\nthe failed patch changed the bytes"));
                }
            }
        }
    }
    assert!(both_err > 0 && edit_refusals * 10 <= total);
    assert_no_failures("RFC 6902", &failures, ok, total);
}

#[test]
fn rfc7396_results_match_a_value_model() {
    let total = case_count(1_500);
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let (mut ok, mut edit_refusals) = (0usize, 0usize);
    let mut failures = Vec::new();
    for it in 0..total {
        let src = gen_doc(&mut r);
        let Ok(mut doc) = Document::parse(src.clone()) else {
            continue;
        };
        let Ok(model0) = doc.to_value() else { continue };
        let patch = gen_value(&mut r, 3, true);
        let want = merge_model(model0, &patch);
        let context = format!("case {it}\nsource:\n{src}patch: {patch:?}");
        match doc.apply_merge_patch(&MergePatch::new(patch)) {
            Ok(()) => match (doc.to_value(), rereads_the_same(&doc)) {
                (Ok(got), Ok(())) if got == want => ok += 1,
                (got, reread) => failures.push(format!(
                    "{context}\nresult:\n{doc}\nwanted {want:?}\ngot {got:?}\n{reread:?}"
                )),
            },
            Err(e) => {
                if doc.to_string() != src {
                    failures.push(format!("{context}\nthe failed patch changed the bytes"));
                } else if e.kind() == PatchErrorKind::Edit {
                    edit_refusals += 1;
                } else {
                    failures.push(format!("{context}\nrefused: {e}"));
                }
            }
        }
    }
    assert!(edit_refusals * 10 <= total);
    assert_no_failures("RFC 7396", &failures, ok, total);
}

/// A mapping of `n` members `k0: 0` ... and a merge patch that deletes most of
/// them, sets some, and names many that are not there.
#[test]
fn wide_null_heavy_merge_patches_match_a_value_model() {
    let n = case_count(1_200).min(4_000);
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    for round in 0..3 {
        let mut src = String::new();
        for i in 0..n {
            src.push_str(&format!("k{i}: {i}  # c{i}\n"));
        }
        let mut doc = Document::parse(src.clone()).unwrap();
        let model0 = doc.to_value().unwrap();
        let mut patch = Mapping::new();
        for i in 0..n * 2 {
            let v = match r.below(6) {
                0 => Value::from(format!("new{i}")),
                1 => {
                    let mut inner = Mapping::new();
                    inner.insert(Value::from("a"), Value::Null);
                    inner.insert(Value::from("b"), Value::from(1));
                    Value::from(inner)
                }
                _ => Value::Null,
            };
            patch.insert(Value::from(format!("k{}", r.below(n * 2))), v);
            let _ = i;
        }
        let patch = Value::from(patch);
        let want = merge_model(model0, &patch);
        doc.apply_merge_patch(&MergePatch::new(patch.clone()))
            .unwrap();
        assert_eq!(doc.to_value().unwrap(), want, "round {round}");
        rereads_the_same(&doc).unwrap();
        // The same patch on a target that is not an object: the patch with its
        // nulls dropped.
        let mut scalar = Document::parse("5\n").unwrap();
        scalar
            .apply_merge_patch(&MergePatch::new(patch.clone()))
            .unwrap();
        assert_eq!(scalar.to_value().unwrap(), merge_model(Value::Null, &patch));
    }
}

/// With `merge_keys` on, a key merged in by `<<` is read like an own key, and
/// a change that would land on it alone is refused. Every combination of a
/// token and an operation is checked against a table.
#[test]
fn merge_key_members_are_read_and_never_written() {
    const SRC: &str = "base: &b {x: 1, y: 2}\nm:\n  <<: *b\n  y: 20\n  z: 3\n";
    let settings = Settings::new().merge_keys(true);
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    for it in 0..case_count(400) {
        // x: merged only. y: own, hiding a merged one. z: own. w: nowhere.
        let token = ["x", "y", "z", "w"][r.below(4)];
        let own: [(&str, i64); 2] = [("y", 20), ("z", 3)];
        let merged: [(&str, i64); 2] = [("x", 1), ("y", 2)];
        let resolved = |own_now: &[(&str, i64)], t: &str| {
            own_now
                .iter()
                .chain(merged.iter())
                .find(|(k, _)| *k == t)
                .map(|(_, v)| *v)
        };
        let before = resolved(&own, token);
        let is_merged_only = !own.iter().any(|(k, _)| *k == token) && before.is_some();
        let mut doc = Document::parse_with(SRC, &settings).unwrap();
        let path = format!("/m/{token}");
        let op = r.below(6);
        let (result, own_after): (Result<(), oneyaml::PatchError>, Vec<(&str, i64)>) = match op {
            0 => (
                doc.apply_patch(&Patch::new(vec![PatchOp::test(
                    path.clone(),
                    Value::from(before.unwrap_or(-1)),
                )])),
                own.to_vec(),
            ),
            1 => (
                doc.apply_patch(&Patch::new(vec![PatchOp::replace(
                    path.clone(),
                    Value::from(99),
                )])),
                own.iter()
                    .map(|(k, v)| (*k, if *k == token { 99 } else { *v }))
                    .collect(),
            ),
            2 => (
                doc.apply_patch(&Patch::new(vec![PatchOp::remove(path.clone())])),
                own.iter().copied().filter(|(k, _)| *k != token).collect(),
            ),
            3 => {
                let mut after: Vec<(&str, i64)> =
                    own.iter().copied().filter(|(k, _)| *k != token).collect();
                after.push((token, 99));
                (
                    doc.apply_patch(&Patch::new(vec![PatchOp::add(
                        path.clone(),
                        Value::from(99),
                    )])),
                    after,
                )
            }
            4 => (
                doc.apply_merge_patch(
                    &MergePatch::from_json(&format!(r#"{{"m": {{"{token}": null}}}}"#)).unwrap(),
                ),
                own.iter().copied().filter(|(k, _)| *k != token).collect(),
            ),
            _ => {
                let mut after: Vec<(&str, i64)> =
                    own.iter().copied().filter(|(k, _)| *k != token).collect();
                after.push((token, 5));
                (
                    doc.apply_merge_patch(
                        &MergePatch::from_json(&format!(r#"{{"m": {{"{token}": 5}}}}"#)).unwrap(),
                    ),
                    after,
                )
            }
        };
        let context = format!("case {it}: token {token}, operation {op}");
        match op {
            0 => match before {
                Some(_) => result.expect(&context),
                None => assert_eq!(
                    result.unwrap_err().kind(),
                    PatchErrorKind::Path,
                    "{context}"
                ),
            },
            1 | 2 if before.is_none() => {
                assert_eq!(
                    result.unwrap_err().kind(),
                    PatchErrorKind::Path,
                    "{context}"
                );
                assert_eq!(doc.to_string(), SRC, "{context}");
            }
            4 if before.is_none() => {
                // Deleting what is not there is a no-op the RFC allows.
                result.expect(&context);
                assert_eq!(doc.to_string(), SRC, "{context}");
            }
            _ if is_merged_only => {
                assert_eq!(
                    result.unwrap_err().kind(),
                    PatchErrorKind::RefusedAlias,
                    "{context}"
                );
                assert_eq!(doc.to_string(), SRC, "{context}");
            }
            _ => {
                result.expect(&context);
                for t in ["x", "y", "z", "w"] {
                    let read = doc
                        .get_pointer(&format!("/m/{t}"))
                        .ok()
                        .and_then(|n| n.as_i64());
                    assert_eq!(read, resolved(&own_after, t), "{context}, reading {t}");
                }
                // The shared mapping is untouched.
                assert_eq!(
                    doc.get_pointer("/base/x").unwrap().as_i64(),
                    Some(1),
                    "{context}"
                );
                assert_eq!(
                    doc.get_pointer("/base/y").unwrap().as_i64(),
                    Some(2),
                    "{context}"
                );
            }
        }
    }
}
