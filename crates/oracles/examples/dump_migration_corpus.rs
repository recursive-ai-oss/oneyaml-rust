//! Writes the migration corpus (`tests/compat_corpus`) as one
//! file per input, for the fuzz crate's seeds:
//!
//! ```text
//! cargo run -p oracles --example dump_migration_corpus -- <out-dir>
//! ```

// Test tooling: the corpus module asserts (it is the differential's).
#[allow(dead_code, clippy::panic, clippy::unwrap_used, clippy::expect_used)]
#[path = "../tests/compat_corpus/mod.rs"]
mod compat_corpus;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .ok_or("usage: dump_migration_corpus <out-dir>")?;
    std::fs::create_dir_all(&out)?;
    for input in compat_corpus::INPUTS {
        std::fs::write(
            std::path::Path::new(&out).join(format!("{}.yaml", input.name)),
            input.src,
        )?;
    }
    Ok(())
}
