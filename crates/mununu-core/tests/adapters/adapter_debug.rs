//! Debug test: writes the TLSF adapter's CTXDSL for four private SYNTCOMP demos to a temp
//! directory, for comparison with the CLI's output.
//!
//! Requires the `syntcomp` feature. The fixtures live in the private sibling checkout
//! (`../mununu-private` from the WORKSPACE root, not from this crate — mununu#1125); when that
//! tree is absent the test says so and skips, instead of failing on a path that exists nowhere.
#![cfg(feature = "syntcomp")]

use std::path::{Path, PathBuf};

use mununu_core::adapter::tlsf::TlsfAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};

/// `<workspace>/../mununu-private/examples/syntcomp/tlsf`, or the reason it is not there.
fn private_tlsf_dir() -> Result<PathBuf, String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = workspace.join("../mununu-private/examples/syntcomp/tlsf");
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!(
            "the private fixtures are not beside this checkout ({})",
            dir.display()
        ))
    }
}

#[test]
fn write_debug_ctxdsl() {
    let dir = match private_tlsf_dir() {
        Ok(d) => d,
        Err(why) => {
            eprintln!("SKIP: {why}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join(format!("mununu1125-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("a temp directory for the CTXDSL");
    for demo in ["14", "17", "18", "19"] {
        let path = dir.join(format!("lilydemo{demo}.tlsf"));
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let options = AdapterOptions::default();
        let output = TlsfAdapter::translate(&source, &options)
            .unwrap_or_else(|e| panic!("lilydemo{demo}.tlsf did not translate: {e}"));
        let out_path = out_dir.join(format!("lilydemo{demo}_adapter.ctxdsl"));
        std::fs::write(&out_path, &output.ctxdsl).expect("write the CTXDSL");
        eprintln!(
            "Written {} ({} bytes)",
            out_path.display(),
            output.ctxdsl.len()
        );
    }
}
