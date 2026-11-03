//! Performance benchmarks for adapter translation pipelines.
//!
//! These tests are marked `#[ignore]` so they don't run in the normal test suite.
//! Run with: `cargo test --test adapter_perf -- --ignored --nocapture`

use std::time::Instant;

/// Generate a TLSF spec with the given number of input and output signals.
fn gen_tlsf(n_inputs: usize, n_outputs: usize) -> String {
    let inputs: Vec<String> = (0..n_inputs).map(|i| format!("i{i}")).collect();
    let outputs: Vec<String> = (0..n_outputs).map(|i| format!("o{i}")).collect();
    let guarantees: Vec<String> = inputs
        .iter()
        .zip(outputs.iter())
        .map(|(i, o)| format!("    G ({i} -> F {o});"))
        .collect();
    format!(
        "INFO {{ TITLE: \"perf_test_{}x{}\" }}\nMAIN {{\n  INPUTS {{ {} }}\n  OUTPUTS {{ {} }}\n  GUARANTEES {{\n{}\n  }}\n}}",
        n_inputs,
        n_outputs,
        inputs
            .iter()
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join(" "),
        outputs
            .iter()
            .map(|s| format!("{s};"))
            .collect::<Vec<_>>()
            .join(" "),
        guarantees.join("\n"),
    )
}

#[test]
#[ignore]
fn perf_tlsf_scaling() {
    use mununu_core::adapter::tlsf::TlsfAdapter;
    use mununu_core::adapter::{AdapterOptions, FormatAdapter};

    let configs = [(2, 2), (3, 3), (4, 4), (5, 5)];

    eprintln!();
    eprintln!("TLSF adapter performance (translate only):");
    eprintln!("{:<12} {:>12} {:>12}", "signals", "states", "time_ms");
    eprintln!("{}", "-".repeat(38));

    for (n_in, n_out) in configs {
        let spec = gen_tlsf(n_in, n_out);
        let options = AdapterOptions::default();

        let start = Instant::now();
        let result = TlsfAdapter::translate(&spec, &options);
        let elapsed = start.elapsed();

        match result {
            Ok(output) => {
                eprintln!(
                    "{:<12} {:>12} {:>12.2}",
                    n_in + n_out,
                    output.source_info.state_count,
                    elapsed.as_secs_f64() * 1000.0,
                );
            }
            Err(e) => {
                eprintln!("  {}x{}: ERROR: {}", n_in, n_out, e);
            }
        }
    }
    eprintln!();
}
