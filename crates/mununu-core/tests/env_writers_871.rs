//! mununu#871: the lib tests that write the process environment, enumerated.
//!
//! The environment is process-global and libtest runs a binary's tests in parallel, so a test
//! that sets a variable changes what every concurrent test reads. The #871 flake was one such
//! write: a test briefly set `MUNUNU_MAX_PROCESS_MEMORY_BYTES` to 1 GiB, under a suite RSS of
//! about 4.4 GiB, so a verify-auto test that hit a memory checkpoint inside that window
//! abstained. That writer is gone. The ones below remain, each one a window in which another
//! test sees a missing tool or writes its breadcrumbs into someone else's file. This list may
//! only shrink (mununu#880): a new writer fails here, and should inject its value instead.

use std::collections::BTreeMap;
use std::path::Path;

/// `(file under src/, number of std::env::set_var / remove_var calls)`.
const KNOWN_WRITERS: &[(&str, usize)] = &[
    ("adapter/btor2/bit_blast.rs", 3),   // MUNUNU_VERILATOR_PATH
    ("adapter/btor2/cegar.rs", 3),       // MUNUNU_CVC5_PATH, across a whole refinement loop
    ("adapter/btormc/mod.rs", 2),        // MUNUNU_BTORMC_PATH
    ("adapter/cvc5/mod.rs", 3),          // MUNUNU_CVC5_PATH
    ("adapter/partial_json.rs", 7),      // MUNUNU_VERIFY_AUTO_PARTIAL_JSON
    ("adapter/slang/mod.rs", 3),         // MUNUNU_SLANG_PATH
    ("adapter/slang/verify_auto.rs", 8), // MUNUNU_SHADOW_BTOR2_DUMP, the partial-JSON path
    ("adapter/verilator/mod.rs", 3),     // MUNUNU_VERILATOR_PATH
    ("adapter/yosys/mod.rs", 3),         // MUNUNU_SV2V_PATH
];

fn count_writers(dir: &Path, root: &Path, out: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            count_writers(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).expect("a source file is UTF-8");
            let n = text.matches("std::env::set_var(").count()
                + text.matches("std::env::remove_var(").count();
            if n > 0 {
                let rel = path.strip_prefix(root).expect("under src/");
                out.insert(rel.to_string_lossy().replace('\\', "/"), n);
            }
        }
    }
}

#[test]
fn no_new_lib_code_writes_the_process_environment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    count_writers(&root, &root, &mut found);
    let known: BTreeMap<String, usize> = KNOWN_WRITERS
        .iter()
        .map(|(f, n)| (f.to_string(), *n))
        .collect();
    let grown: Vec<_> = found
        .iter()
        .filter(|(f, n)| known.get(*f).is_none_or(|k| *n > k))
        .collect();
    assert!(
        grown.is_empty(),
        "new process-environment writes (racy under parallel tests, mununu#871); inject the value \
         instead: {grown:?}"
    );
    let shrunk: Vec<_> = known
        .iter()
        .filter(|(f, k)| found.get(*f).is_none_or(|n| n < k))
        .collect();
    assert!(
        shrunk.is_empty(),
        "a known writer went away or shrank; lower its entry in KNOWN_WRITERS: {shrunk:?}"
    );
}
