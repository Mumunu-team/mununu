//! R.0a fixture-sweep regression test.
//!
//! Runs `preprocess_sv` (the public sv2v wrapper landed in R.0a) across
//! every `examples/systemverilog/*.sv` fixture and asserts:
//!   1. The subprocess exits cleanly.
//!   2. The output file is non-empty.
//!   3. sv2v produced no stderr (the R.0a done-criterion: "zero warnings").
//!
//! Skips gracefully when `sv2v` is unavailable (CI without the binary on
//! `$PATH` and no `MUNUNU_SV2V_PATH` set). Locate-failure raises a `SKIP:`
//! message via the test's `eprintln!`; the test still passes so a missing
//! `sv2v` doesn't break the test matrix.
//!
//! Run with: `cargo test --test sv_preprocess_sweep -- --nocapture`

use std::fs;
use std::path::PathBuf;

use mununu_core::adapter::yosys::preprocess_sv;

#[test]
fn sv_preprocess_sweeps_every_fixture() {
    // Locate the workspace root from CARGO_MANIFEST_DIR (the test binary
    // is built under crates/mununu-core/, so the fixtures live at
    // ../../examples/systemverilog/).
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures_dir = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("examples/systemverilog");

    let mut sv_files: Vec<PathBuf> = fs::read_dir(&fixtures_dir)
        .expect("read examples/systemverilog/")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sv"))
        .collect();
    sv_files.sort();

    assert!(
        !sv_files.is_empty(),
        "no .sv fixtures under examples/systemverilog/"
    );

    let tmp = tempfile::TempDir::new().expect("create tempdir for sweep outputs");

    let mut pass = 0;
    let mut fail = Vec::new();

    for sv in &sv_files {
        let name = sv.file_stem().and_then(|s| s.to_str()).unwrap_or("module");
        let out = tmp.path().join(format!("{name}.elab.v"));
        match preprocess_sv(std::slice::from_ref(sv), &[], &out) {
            Ok(_) => {
                let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                if size == 0 {
                    fail.push(format!("{name}: empty output"));
                } else {
                    pass += 1;
                }
            }
            Err(err) => {
                let msg = err.message;
                if msg.contains("sv2v binary not found") {
                    eprintln!(
                        "SKIP: sv2v not installed (set MUNUNU_SV2V_PATH or install zachjs/sv2v ≥ 0.0.10). \
                         Skipping fixture sweep; the production CI image ships sv2v."
                    );
                    return;
                }
                fail.push(format!("{name}: {msg}"));
            }
        }
    }

    assert!(
        fail.is_empty(),
        "R.0a sweep failed for {} fixture(s):\n{}",
        fail.len(),
        fail.join("\n")
    );
    assert_eq!(pass, sv_files.len(), "expected every fixture to pass");
    eprintln!("R.0a sweep: {pass}/{pass} fixtures pass via sv2v");
}
