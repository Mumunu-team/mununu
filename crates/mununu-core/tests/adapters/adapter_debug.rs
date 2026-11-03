//! Debug test: writes adapter output CTXDSL to /tmp for CLI comparison.
//!
//! Requires the `syntcomp` feature and access to the mununu-private repo.
#![cfg(feature = "syntcomp")]

use mununu_core::adapter::tlsf::TlsfAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};

#[test]
fn write_debug_ctxdsl() {
    for demo in ["14", "17", "18", "19"] {
        let path = format!(
            "{}/../mununu-private/examples/syntcomp/tlsf/lilydemo{demo}.tlsf",
            env!("CARGO_MANIFEST_DIR")
        );
        let source = std::fs::read_to_string(&path).unwrap();
        let options = AdapterOptions::default();
        let output = TlsfAdapter::translate(&source, &options).unwrap();
        let out_path = format!("/tmp/lilydemo{demo}_adapter.ctxdsl");
        std::fs::write(&out_path, &output.ctxdsl).unwrap();
        eprintln!("Written {out_path} ({} bytes)", output.ctxdsl.len());
    }
}
