//! `fuzz-seed`: builds the seed corpora and replays inputs by hand.
//!
//! ```text
//! fuzz-seed seed <out-dir> [<migration-dir>]   # raw seeds, one dir per target
//! fuzz-seed run <target> <file>...              # run one target on files
//! ```
//!
//! Seeds: every yaml-test-suite input, the benchmark corpus
//! (`bench::corpus`, scale 1), the adversarial corpus
//! (`test_suite::adversarial`), and the migration corpus (the files that
//! `cargo run -p oracles --example dump_migration_corpus` writes), each
//! cut to [`MAX_SEED`] bytes; the `from_slice` seeds are the suite and
//! migration texts in every encoding with and without a BOM, and the
//! `render` seeds are texts with a diagnostic tail. `fuzz.sh seed`
//! minimises them into `corpus/<target>` (libFuzzer `-merge=1`).
#![forbid(unsafe_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Seeds longer than this are cut (at a `char` boundary): the fuzzer runs
/// with a smaller `-max_len` anyway, and the shape is in the prefix.
const MAX_SEED: usize = 32 * 1024;

fn cut(s: &str) -> &str {
    let mut end = s.len().min(MAX_SEED);
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or_default()
}

fn write(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{:016x}", h.finish())), bytes)
}

fn texts(migration: Option<&Path>) -> Result<(Vec<Vec<u8>>, Vec<String>), String> {
    let mut raw = Vec::new();
    let mut small = Vec::new();
    let suite = test_suite::load_suite(&test_suite::default_data_dir())
        .map_err(|e| format!("suite: {e}"))?;
    for case in suite {
        if let Ok(t) = String::from_utf8(case.input.clone()) {
            small.push(t);
        }
        raw.push(case.input);
    }
    if let Some(dir) = migration {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let bytes = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
            if let Ok(t) = String::from_utf8(bytes.clone()) {
                small.push(t);
            }
            raw.push(bytes);
        }
    }
    for case in bench::corpus::CASES {
        raw.push(cut(&case.generate(1)).as_bytes().to_vec());
    }
    for a in test_suite::adversarial::corpus() {
        raw.push(cut(&a.input).as_bytes().to_vec());
    }
    Ok((raw, small))
}

fn seed(out: &Path, migration: Option<&Path>) -> Result<(), String> {
    let (raw, small) = texts(migration)?;
    let io = |e: std::io::Error| e.to_string();
    for target in [
        "parse",
        "consistency",
        "serde",
        "edit",
        "ser",
        "tree_edit",
        "cap",
        "handles",
    ] {
        for bytes in &raw {
            write(&out.join(target), bytes).map_err(io)?;
        }
    }
    let encodings = [
        oneyaml::Encoding::Utf8,
        oneyaml::Encoding::Utf16Le,
        oneyaml::Encoding::Utf16Be,
        oneyaml::Encoding::Utf32Le,
        oneyaml::Encoding::Utf32Be,
    ];
    for bytes in &raw {
        write(&out.join("from_slice"), bytes).map_err(io)?;
    }
    for t in &small {
        let with_bom = format!("\u{feff}{t}");
        for enc in encodings {
            for body in [t.as_str(), with_bom.as_str()] {
                if let Ok(bytes) = oneyaml_syntax::encode(body, enc) {
                    write(&out.join("from_slice"), &bytes).map_err(io)?;
                }
            }
        }
    }
    for bad in [
        &b"a: \xff\xfe\n"[..],
        b"\xef\xbb\xbf\x80",
        b"\xff\xfe\x00\xd8",
        b"a: \xff\n--- b: \xfe\n",
        b"\xfe\n--- [\xff, \xc3]\n...\n# \x80\n--- \xff\n",
    ] {
        write(&out.join("from_slice"), bad).map_err(io)?;
    }
    // Render: the text, NUL, then a diagnostic tail (other source, code,
    // message, primary span, label, secondaries, help, suggestions).
    let tail: &[u8] = b"\x03a\nb\x05\x02msg\x02\x01\x00\x03\x05\x00\x01lbl\x03\x02\x09\x00\x00\x00\x00\x00\x01x\x01\x02hi\x01\x02\x02\x03\x00\x02\x02\x00\x01y";
    for t in small.iter().take(400) {
        let mut bytes = t.as_bytes().to_vec();
        bytes.push(0);
        bytes.extend_from_slice(tail);
        write(&out.join("render"), &bytes).map_err(io)?;
    }
    Ok(())
}

fn run(target: &str, files: &[String]) -> Result<(), String> {
    let f = fuzz::target(target).ok_or_else(|| format!("no target {target}"))?;
    for file in files {
        let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        eprintln!("{target} {file} ({} bytes)", bytes.len());
        f(&bytes);
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.split_first() {
        Some((cmd, rest)) if cmd == "seed" => match rest {
            [out] => seed(&PathBuf::from(out), None),
            [out, migration] => seed(&PathBuf::from(out), Some(Path::new(migration))),
            _ => Err("usage: fuzz-seed seed <out-dir> [<migration-dir>]".to_owned()),
        },
        Some((cmd, rest)) if cmd == "run" => match rest.split_first() {
            Some((target, files)) => run(target, files),
            None => Err("usage: fuzz-seed run <target> <file>...".to_owned()),
        },
        _ => Err(
            "usage: fuzz-seed seed <out-dir> [<migration-dir>] | run <target> <file>...".to_owned(),
        ),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
