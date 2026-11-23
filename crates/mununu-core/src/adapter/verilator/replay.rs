//! Counterexample replay under Verilator: a testbench generated from a verify-auto trace.
//!
//! > Source of truth: [`counterexample_testbench`] — surface: (CLI+API), `sv verify-auto
//! > --emit-testbench DIR` and `emit_testbench: true` on `POST /api/v1/sv/verify-auto`.
//!
//! The engines decide on the lifted model; this replays their trace on the RTL with an
//! independent simulator, compiled from the user's sources. The testbench holds the reset for one
//! cycle (the lift's reset state is the state after one held-reset cycle), checks that state,
//! then for each step drives the trace's inputs, ticks the clock once and compares every register
//! the trace names with the next state. Under `--assert` Verilator also evaluates the design's
//! own (and bound) assertions, so a safety counterexample shows the assertion firing at the
//! reported cycle.
//!
//! **Name mapping.** A register is read as the port `dut->NAME` when it is a top-level port, and
//! otherwise through `--public-flat-rw` as `dut->rootp->TOP__DOT__a__DOT__b` for the flat name
//! `a.b`. Under the inputs-in-the-state lift an input sampled at cycle `i` is a state cell named
//! after the port (`wr_en`) and the next choice is the input `wr_en$next`; the testbench drives
//! port `p` at cycle `i` from the sampled cell when the state carries it, else from `inputs[i]`.
//! Cells the RTL does not have — `$past` shadows, the assumption and fault monitors, slices — are
//! not compared, and a value wider than 64 bits is skipped (reported in the testbench header).
//!
//! **Names are written, never quoted.** Every name of the design that reaches `tb.cpp`,
//! the Makefile or `replay.gtkw` is first held to a grammar the reader of that file takes as one
//! token: the top module and the sources ([`bench_refusal`], a Makefile word), the clock and the
//! reset (the same refusal — a C++ member of the model), an input port ([`plain_name`]) and a
//! register ([`readable_register`]). A design-wide name outside its grammar means no bench, with
//! the reason; a single port or register outside it is left out and listed in `skipped`.

use crate::adapter::btor2::cex_value::CexValue;
use crate::adapter::slang::verify_auto::ExactCounterexample;

/// The generated files and what they check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBench {
    /// `tb.cpp` — the Verilator C++ testbench.
    pub tb_cpp: String,
    /// `Makefile` — `make` builds and runs it (`VERILATOR=… make` to override the binary).
    pub makefile: String,
    /// How many (cycle, register) comparisons the testbench makes.
    pub checks: usize,
    /// Cells left out of the comparison and why (synthetic, wider than 64 bits).
    pub skipped: Vec<String>,
}

/// What the testbench needs about the design beyond the trace.
#[derive(Debug, Clone)]
pub struct ReplayDesign<'a> {
    pub top: &'a str,
    pub clock: &'a str,
    /// The reset input and its ACTIVE value (`rst_n` active 0).
    pub reset: Option<(&'a str, u64)>,
    /// The RTL sources, as the Makefile passes them to Verilator (bound SVA files included).
    pub sources: &'a [String],
}

/// A cell the testbench cannot compare: one the verifier added ([`crate::trace::is_monitor_cell`]),
/// or a yosys-internal name Verilator does not expose.
fn synthetic(name: &str) -> bool {
    crate::trace::is_monitor_cell(name) || name.contains('$')
}

fn accessor(top: &str, name: &str, ports: &std::collections::BTreeSet<String>) -> String {
    if ports.contains(name) {
        format!("dut->{name}")
    } else {
        format!("dut->rootp->{top}__DOT__{}", name.replace('.', "__DOT__"))
    }
}

/// A name the bench writes as one member of the Verilated model (`dut->NAME`): a letter or `_`,
/// then letters, digits and `_`. The clock, the reset and every driven input port are held to
/// it; an escaped identifier (`\a-b `) is legal SystemVerilog and is outside it on purpose.
fn plain_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A register's flat name the bench can read back and print: [`plain_name`]s joined by `.`
/// (`u_sub.cnt`), with element indices at the end (`mem[3]`, as Verilator indexes an unpacked
/// array). It is written twice — inside a C string literal and as an expression — so nothing
/// outside this grammar reaches either.
fn readable_register(name: &str) -> bool {
    let mut path = name;
    while let Some((head, index)) = path.strip_suffix(']').and_then(|s| s.rsplit_once('[')) {
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        path = head;
    }
    path.split('.').all(plain_name)
}

/// Why no replay bench can be generated for a design, whatever the counterexample
/// ([`bench_refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchRefusal {
    /// The top module's name is outside what the Makefile takes. Nothing in the design to change.
    Top(String),
    /// These source paths are outside what the Makefile takes.
    Sources(Vec<String>),
    /// The clock's name is not a port name the bench can drive ([`plain_name`]). The caller
    /// named it, so the caller can name another.
    Clock(String),
    /// The pinned reset's name is not a port name the bench can drive. It comes from the lift,
    /// so there is nothing to pass instead.
    Reset(String),
}

/// What the caller's surface offers for the refusals that have a remedy: the CLI's sources are
/// files and its clock a flag, the API's are names and a field of the request.
#[derive(Debug, Clone, Copy)]
pub struct BenchRemedies<'a> {
    /// For [`BenchRefusal::Sources`].
    pub sources: &'a str,
    /// For [`BenchRefusal::Clock`].
    pub clock: &'a str,
}

impl BenchRefusal {
    /// The reason, in the user's names. A source path and a clock name have a remedy and it
    /// differs per surface, so the caller gives its own in `remedies`; a top or reset name has
    /// none and says whose limit it is.
    pub fn message(&self, remedies: &BenchRemedies<'_>) -> String {
        let source_remedy = remedies.sources;
        match self {
            BenchRefusal::Top(top) => format!(
                "the top module name `{top}` cannot be written into the bench's Makefile (it \
                 takes letters, digits and `_`) — a mununu limitation; nothing to change in the \
                 design"
            ),
            BenchRefusal::Sources(bad) => format!(
                "the bench's Makefile takes a source path made of ASCII letters, digits and \
                 `_` `-` `.` `/` only, and {} not: {} — {source_remedy}",
                if bad.len() == 1 {
                    "this one is"
                } else {
                    "these are"
                },
                bad.iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            BenchRefusal::Clock(clock) => format!(
                "the clock `{clock}` is not a port name the bench can drive (a letter or `_`, \
                 then letters, digits and `_`) — {}",
                remedies.clock
            ),
            BenchRefusal::Reset(reset) => format!(
                "the reset `{reset}` is not a port name the bench can drive (a letter or `_`, \
                 then letters, digits and `_`) — a mununu limitation; nothing to change in the \
                 design"
            ),
        }
    }
}

/// Why no replay bench can be generated for `design`, whatever the counterexample — `None` when
/// one can. `top` and the source names are written into a Makefile that `make` runs, so both
/// stay inside a grammar `make` reads as one word (`$` is make syntax, so a `$` the module-name
/// grammar allows is refused here too). The clock and the reset are written into `tb.cpp` as
/// members of the model, so each is one [`plain_name`]: the clock is the caller's to name
/// (`--tb-clock`, `tb_clock`) and a bench that could not drive it would replay nothing.
///
/// This is a property of the design's names, not of a trace, so a caller asks it ONCE, before
/// the properties, and reports the answer where it would have reported the bench
/// (`trace.replay.not_generated`; the CLI's `replay testbench: not written — …`). The source
/// case is reachable: the lift stages its sources under names of its own, so a design under
/// `my project/` lifts and yields a counterexample, while the bench has to name the files where
/// they are (mununu#1176 — it used to end in a log line and a report that said there was no
/// counterexample with a path).
pub fn bench_refusal(design: &ReplayDesign<'_>) -> Option<BenchRefusal> {
    if !crate::adapter::yosys::is_valid_module_name(design.top) || design.top.contains('$') {
        return Some(BenchRefusal::Top(design.top.to_string()));
    }
    let bad: Vec<String> = design
        .sources
        .iter()
        .filter(|s| !crate::adapter::yosys::is_valid_source_name(s))
        .cloned()
        .collect();
    if !bad.is_empty() {
        return Some(BenchRefusal::Sources(bad));
    }
    if !plain_name(design.clock) {
        return Some(BenchRefusal::Clock(design.clock.to_string()));
    }
    match design.reset {
        Some((reset, _)) if !plain_name(reset) => Some(BenchRefusal::Reset(reset.to_string())),
        _ => None,
    }
}

/// Why an input the trace drives is left undriven (the `skipped` entry after the name).
const UNWRITABLE_INPUT: &str = "an input the testbench cannot drive (its name is not letters, \
     digits and `_`) — a mununu limitation";
/// Why a register the trace names is not compared (the `skipped` entry after the name).
const UNREADABLE_REGISTER: &str = "a register the testbench cannot read (its name is not \
     letters, digits, `_`, `.` and a final `[N]`) — a mununu limitation";

/// Whether `cex` carries a path a bench could replay (an unreachable-target witness has none).
pub fn has_path(cex: &ExactCounterexample) -> bool {
    !(cex.prefix.is_empty() && cex.cycle.is_empty())
}

/// Generate the replay testbench for `cex` (see the module doc). `None` when the counterexample
/// carries no path (an unreachable-target witness has nothing to replay), and when
/// [`bench_refusal`] refuses the design — a caller that reports "nothing to replay" asks
/// `bench_refusal` first, so the two cases are never told as one.
pub fn counterexample_testbench(
    design: &ReplayDesign<'_>,
    cex: &ExactCounterexample,
) -> Option<ReplayBench> {
    let states: Vec<&Vec<(String, CexValue)>> = cex.prefix.iter().chain(cex.cycle.iter()).collect();
    if !has_path(cex) {
        return None;
    }
    // Security gate: the generator stays safe on its own. The reason reaches the report through
    // `bench_refusal`, which every caller asks before the first property.
    if bench_refusal(design).is_some() {
        return None;
    }
    let top = design.top;
    let mut skipped: Vec<String> = Vec::new();
    // Input ports: every input name the trace drives, with `$next` stripped. One the bench
    // cannot write as a member of the model is left undriven and said so.
    let mut ports: std::collections::BTreeSet<String> = cex
        .inputs
        .iter()
        .flatten()
        .map(|(n, _)| n.trim_end_matches("$next").to_string())
        .filter(|n| !n.starts_with("mununu_") && !n.contains("__"))
        .collect();
    ports.retain(|n| {
        let writable = plain_name(n);
        if !writable {
            skipped.push(format!("{n}: {UNWRITABLE_INPUT}"));
        }
        writable
    });
    ports.remove(design.clock);
    if let Some((r, _)) = design.reset {
        ports.remove(r);
    }
    let value = |v: &CexValue, what: &str, skipped: &mut Vec<String>| -> Option<u64> {
        let out = v.as_u64();
        if out.is_none() && !skipped.iter().any(|s| s.starts_with(what)) {
            skipped.push(format!("{what}: wider than 64 bits"));
        }
        out
    };

    let mut tb = String::new();
    tb.push_str(
        "// Generated by mununu `sv verify-auto --emit-testbench`: replays the counterexample\n",
    );
    tb.push_str(
        "// on the RTL under Verilator. `make` builds and runs it; the last line says MATCH or\n",
    );
    tb.push_str(
        "// MISMATCH, and a failing assertion of the design is reported by Verilator (--assert).\n",
    );
    tb.push_str(
        "// `make trace` builds it with --trace and -DMUNUNU_TRACE: the same run also writes\n",
    );
    tb.push_str("// replay.vcd (every signal, two time units per cycle) for GTKWave.\n");
    tb.push_str("#include \"verilated.h\"\n");
    tb.push_str("#ifdef MUNUNU_TRACE\n#include \"verilated_vcd_c.h\"\n#endif\n");
    tb.push_str(&format!(
        "#include \"V{top}.h\"\n#include \"V{top}___024root.h\"\n"
    ));
    tb.push_str("#include <cstdio>\n#include <cstdint>\n\n");
    tb.push_str(&format!(
        "static V{top}* dut;\nstatic int mismatches = 0;\n"
    ));
    tb.push_str("#ifdef MUNUNU_TRACE\nstatic VerilatedVcdC* tfp = nullptr;\nstatic vluint64_t sim_time = 0;\n");
    tb.push_str("static void dump() { if (tfp) tfp->dump(sim_time++); }\n#else\nstatic void dump() {}\n#endif\n");
    tb.push_str(&format!(
        "static void tick() {{ dut->{c} = 0; dut->eval(); dump(); dut->{c} = 1; dut->eval(); dump(); }}\n",
        c = design.clock
    ));
    tb.push_str("static void check(int cyc, const char* name, uint64_t got, uint64_t want) {\n");
    tb.push_str("    if (got != want) {\n");
    tb.push_str("        ++mismatches;\n");
    tb.push_str(
        "        std::printf(\"cycle %d: %s = 0x%llx, trace says 0x%llx\\n\", cyc, name,\n",
    );
    tb.push_str("                    (unsigned long long)got, (unsigned long long)want);\n");
    tb.push_str("    }\n}\n\n");
    tb.push_str("int main(int argc, char** argv) {\n");
    tb.push_str("    Verilated::commandArgs(argc, argv);\n");
    tb.push_str(&format!("    dut = new V{top}();\n"));
    tb.push_str(
        "#ifdef MUNUNU_TRACE\n    Verilated::traceEverOn(true);\n    tfp = new VerilatedVcdC;\n",
    );
    tb.push_str("    dut->trace(tfp, 99);\n    tfp->open(\"replay.vcd\");\n#endif\n");
    tb.push_str(&format!("    dut->{} = 0;\n", design.clock));
    let mut checks = 0usize;
    let drive = |tb: &mut String, i: usize, skipped: &mut Vec<String>| {
        for p in &ports {
            // The value at cycle `i`: the sampled cell of state `i` (inputs in the state), else the
            // plain input of step `i`. A `p$next` input is cycle `i + 1`'s value, which state
            // `i + 1` carries — never cycle `i`'s.
            let sampled = states.get(i).and_then(|s| s.iter().find(|(n, _)| n == p));
            let chosen = sampled.or_else(|| {
                cex.inputs
                    .get(i)
                    .and_then(|inp| inp.iter().find(|(n, _)| n == p))
            });
            if let Some((_, v)) = chosen
                && let Some(x) = value(v, p, skipped)
            {
                tb.push_str(&format!("    dut->{p} = {x}ull;\n"));
            }
        }
    };
    if let Some((r, active)) = design.reset {
        tb.push_str(&format!("    dut->{r} = {active};\n"));
        drive(&mut tb, 0, &mut skipped);
        tb.push_str("    tick();\n");
        tb.push_str(&format!(
            "    dut->{r} = {};\n",
            if active == 0 { 1 } else { 0 }
        ));
    }
    let compare = |tb: &mut String,
                   cyc: usize,
                   state: &Vec<(String, CexValue)>,
                   skipped: &mut Vec<String>,
                   checks: &mut usize| {
        for (name, v) in state {
            if synthetic(name) || ports.contains(name) || name == design.clock {
                if synthetic(name) && !skipped.iter().any(|s| s.starts_with(name.as_str())) {
                    skipped.push(format!("{name}: not an RTL signal"));
                }
                continue;
            }
            if !readable_register(name) {
                skipped.push(format!("{name}: {UNREADABLE_REGISTER}"));
                continue;
            }
            if let Some(x) = value(v, name, skipped) {
                tb.push_str(&format!(
                    "    check({cyc}, \"{name}\", (uint64_t){}, {x}ull);\n",
                    accessor(top, name, &ports)
                ));
                *checks += 1;
            }
        }
    };
    compare(&mut tb, 0, states[0], &mut skipped, &mut checks);
    for i in 0..states.len().saturating_sub(1) {
        drive(&mut tb, i, &mut skipped);
        tb.push_str("    tick();\n");
        compare(&mut tb, i + 1, states[i + 1], &mut skipped, &mut checks);
    }
    tb.push_str("    std::printf(mismatches ? \"MISMATCH (%d)\\n\" : \"MATCH\\n\", mismatches);\n");
    tb.push_str("    std::fflush(stdout);\n");
    tb.push_str(
        "    // One more edge so the design's assertions sample the final state: a safety\n",
    );
    tb.push_str(
        "    // violation is reported here by Verilator (--assert) as `Assertion failed`.\n",
    );
    tb.push_str("    tick();\n");
    tb.push_str("#ifdef MUNUNU_TRACE\n    if (tfp) { tfp->close(); delete tfp; }\n#endif\n");
    tb.push_str("    delete dut;\n    return mismatches ? 1 : 0;\n}\n");
    skipped.sort();
    skipped.dedup();
    if !skipped.is_empty() {
        // One comment line per entry, whatever the name holds.
        let header: String = skipped
            .iter()
            .map(|s| {
                format!(
                    "// not compared: {}\n",
                    s.replace(|c: char| c.is_control(), "?")
                )
            })
            .collect();
        tb = header + &tb;
    }

    let makefile = format!(
        "# Generated by mununu `sv verify-auto --emit-testbench`.\n\
         VERILATOR ?= verilator\n\
         SOURCES = {srcs}\n\n\
         run: obj_dir/V{top}\n\t./obj_dir/V{top}\n\n\
         obj_dir/V{top}: tb.cpp $(SOURCES)\n\
         \t\"$(VERILATOR)\" --cc --exe --build --assert --public-flat-rw -Wno-fatal \\\n\
         \t  --top-module {top} $(SOURCES) tb.cpp -o V{top}\n\n\
         # The same replay, writing replay.vcd for GTKWave (gtkwave replay.vcd).\n\
         trace: obj_dir_trace/V{top}\n\t./obj_dir_trace/V{top}; test -s replay.vcd && echo \"wrote replay.vcd\"\n\n\
         obj_dir_trace/V{top}: tb.cpp $(SOURCES)\n\
         \t\"$(VERILATOR)\" --cc --exe --build --assert --public-flat-rw -Wno-fatal --trace \\\n\
         \t  -CFLAGS -DMUNUNU_TRACE --Mdir obj_dir_trace \\\n\
         \t  --top-module {top} $(SOURCES) tb.cpp -o V{top}\n\n\
         clean:\n\trm -rf obj_dir obj_dir_trace replay.vcd\n\n.PHONY: run trace clean\n",
        srcs = design.sources.join(" ")
    );
    Some(ReplayBench {
        tb_cpp: tb,
        makefile,
        checks,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: u64, bits: u32) -> CexValue {
        CexValue::from_u128(x as u128, bits)
    }

    /// The testbench resets, checks the reset state, drives each step's inputs (a sampled input
    /// cell takes precedence over `$next`), compares every RTL register after each tick, and
    /// leaves the synthetic cells out.
    #[test]
    fn a_trace_becomes_a_reset_drive_compare_testbench() {
        let cex = ExactCounterexample {
            prefix: vec![
                vec![
                    ("state_q".into(), v(0, 3)),
                    ("go".into(), v(1, 1)),
                    ("state_q__past".into(), v(0, 3)),
                ],
                vec![("state_q".into(), v(1, 3)), ("go".into(), v(0, 1))],
                vec![("u_sub.cnt".into(), v(2, 4)), ("state_q".into(), v(2, 3))],
            ],
            cycle: vec![],
            inputs: vec![
                vec![("go$next".into(), v(0, 1))],
                vec![("go$next".into(), v(1, 1))],
            ],
            unreachable_target: vec![],
        };
        let sources = vec!["/abs/rod.sv".to_string(), "/abs/rod_sva.sv".to_string()];
        let b = counterexample_testbench(
            &ReplayDesign {
                top: "rod",
                clock: "clk",
                reset: Some(("rst_n", 0)),
                sources: &sources,
            },
            &cex,
        )
        .unwrap();
        assert!(b.tb_cpp.contains("dut->rst_n = 0;"), "{}", b.tb_cpp);
        assert!(
            b.tb_cpp.contains("dut->go = 1ull;"),
            "the sampled cell drives cycle 0"
        );
        assert!(b.tb_cpp.contains(
            "check(2, \"u_sub.cnt\", (uint64_t)dut->rootp->rod__DOT__u_sub__DOT__cnt, 2ull);"
        ));
        assert!(!b.tb_cpp.contains("state_q__past\", (uint64_t)"));
        assert_eq!(b.checks, 4, "state_q ×3 and u_sub.cnt");
        assert!(b.skipped.iter().any(|s| s.starts_with("state_q__past")));
        assert!(b.makefile.contains("--assert --public-flat-rw"));
        assert!(b.makefile.contains("/abs/rod.sv /abs/rod_sva.sv"));
    }

    /// `make trace` is the same replay built with `--trace` and `-DMUNUNU_TRACE`, in its own
    /// `obj_dir_trace`, writing `replay.vcd`; the plain `make` neither needs the VCD library nor
    /// writes a file, so the two targets stay independent and the default replay stays fast.
    #[test]
    fn the_makefile_has_a_trace_target_and_the_testbench_dumps_a_vcd_under_it() {
        let cex = ExactCounterexample {
            prefix: vec![
                vec![("state_q".into(), v(0, 3))],
                vec![("state_q".into(), v(1, 3))],
            ],
            cycle: vec![],
            inputs: vec![vec![("go$next".into(), v(1, 1))]],
            unreachable_target: vec![],
        };
        let sources = vec!["/abs/rod.sv".to_string()];
        let b = counterexample_testbench(
            &ReplayDesign {
                top: "rod",
                clock: "clk",
                reset: None,
                sources: &sources,
            },
            &cex,
        )
        .unwrap();
        assert!(
            b.makefile.contains("trace: obj_dir_trace/Vrod"),
            "{}",
            b.makefile
        );
        assert!(
            b.makefile
                .contains("--trace \\\n\t  -CFLAGS -DMUNUNU_TRACE --Mdir obj_dir_trace")
        );
        assert!(b.makefile.contains(".PHONY: run trace clean"));
        assert!(
            b.tb_cpp
                .contains("#ifdef MUNUNU_TRACE\n#include \"verilated_vcd_c.h\"")
        );
        assert!(b.tb_cpp.contains("tfp->open(\"replay.vcd\")"));
        assert!(
            b.tb_cpp
                .contains("dut->eval(); dump(); dut->clk = 1; dut->eval(); dump();")
        );
        assert!(b.tb_cpp.contains("if (tfp) { tfp->close(); delete tfp; }"));
    }

    #[test]
    fn an_unreachable_target_has_nothing_to_replay() {
        let cex = ExactCounterexample {
            prefix: vec![],
            cycle: vec![],
            inputs: vec![],
            unreachable_target: vec!["state_q == 5".into()],
        };
        assert!(
            counterexample_testbench(
                &ReplayDesign {
                    top: "t",
                    clock: "clk",
                    reset: None,
                    sources: &[]
                },
                &cex
            )
            .is_none()
        );
    }

    fn one_step() -> ExactCounterexample {
        ExactCounterexample {
            prefix: vec![
                vec![("state_q".into(), v(0, 3))],
                vec![("state_q".into(), v(1, 3))],
            ],
            cycle: vec![],
            inputs: vec![vec![("go$next".into(), v(1, 1))]],
            unreachable_target: vec![],
        }
    }

    /// mununu#1176 — the cases of "no bench for this design", each with its reason: a source
    /// path the Makefile cannot name carries the path and the caller's own remedy; a top name it
    /// cannot write says whose limit it is; a design it can write is not refused. The generator
    /// refuses what `bench_refusal` refuses, and a caller tells "no path" from "refused" with
    /// `has_path`.
    #[test]
    fn x1176_a_design_the_makefile_cannot_name_is_refused_with_its_reason() {
        fn design<'a>(top: &'a str, sources: &'a [String]) -> ReplayDesign<'a> {
            ReplayDesign {
                top,
                clock: "clk",
                reset: None,
                sources,
            }
        }
        let spaced = &[
            "/abs/rod.sv".to_string(),
            "/abs/my project/rod_sva.sv".to_string(),
            "/abs/caf\u{e9}/pkg.sv".to_string(),
        ][..];
        let plain = &["/abs/rod.sv".to_string()][..];
        let remedies = BenchRemedies {
            sources: "copy them",
            clock: "name another",
        };

        let r = bench_refusal(&design("rod", spaced)).expect("a spaced source path");
        assert_eq!(
            r,
            BenchRefusal::Sources(vec![
                "/abs/my project/rod_sva.sv".to_string(),
                "/abs/caf\u{e9}/pkg.sv".to_string()
            ])
        );
        let line = r.message(&remedies);
        assert!(line.contains("`/abs/my project/rod_sva.sv`"), "{line}");
        assert!(line.contains("these are not"), "{line}");
        assert!(line.ends_with("— copy them"), "{line}");
        assert!(!line.contains("/abs/rod.sv`"), "only the paths at fault");

        let r = bench_refusal(&design("rod$1", plain)).expect("a `$` in the top name");
        assert_eq!(r, BenchRefusal::Top("rod$1".to_string()));
        let line = r.message(&remedies);
        assert!(line.contains("a mununu limitation"), "{line}");
        assert!(
            !line.contains("copy them"),
            "no remedy exists for a top name"
        );

        assert_eq!(bench_refusal(&design("rod", plain)), None);

        let cex = one_step();
        assert!(has_path(&cex));
        assert!(counterexample_testbench(&design("rod", spaced), &cex).is_none());
        assert!(counterexample_testbench(&design("rod$1", plain), &cex).is_none());
        assert!(counterexample_testbench(&design("rod", plain), &cex).is_some());
        assert!(!has_path(&ExactCounterexample {
            prefix: vec![],
            cycle: vec![],
            inputs: vec![],
            unreachable_target: vec!["state_q == 5".into()],
        }));
    }

    /// The bench drives the clock as a member of the Verilated model, so a clock name that is
    /// not a port name yields no bench at all (it used to be written into `tb.cpp` as given).
    #[test]
    fn a_clock_name_that_is_not_a_port_name_yields_no_bench() {
        let sources = vec!["/abs/rod.sv".to_string()];
        for clock in OUTSIDE_NAMES {
            let bench = counterexample_testbench(
                &ReplayDesign {
                    top: "rod",
                    clock,
                    reset: None,
                    sources: &sources,
                },
                &one_step(),
            );
            assert!(bench.is_none(), "clock {clock:?} was written into a bench");
        }
    }

    /// Names no bench file may carry: each has a character that ends the token the name is
    /// written as — a C++ member, a C string literal, a line of the save file.
    const OUTSIDE_NAMES: [&str; 10] = [
        "",
        "1clk",
        "clk x",
        "clk;",
        "clk=0",
        "a\"b",
        "a\\",
        "a\nb",
        "a(0)",
        "caf\u{e9}",
    ];

    /// Every name of the design that reaches a generated file, by where it comes from, with
    /// what happens to one outside its grammar: the top and a source (mununu#1176), the clock
    /// and the reset (no bench, with the reason — the clock's names the caller's own remedy,
    /// the reset's says whose limit it is), an input port and a register (left out, listed in
    /// `skipped`, the rest of the bench intact), and the same names in `replay.gtkw`. A new
    /// place a name is written adds its row here.
    #[test]
    fn every_name_the_bench_writes_is_held_to_its_grammar() {
        let sources = vec!["/abs/rod.sv".to_string()];
        let remedies = BenchRemedies {
            sources: "copy them",
            clock: "name another",
        };
        let design = |clock: &'static str, reset: Option<(&'static str, u64)>| ReplayDesign {
            top: "rod",
            clock,
            reset,
            sources: &sources,
        };
        // A name of that list appears in the `// not compared:` header (one comment line each, no
        // control character) and nowhere else in a generated file.
        let clean = |text: &str, outside: &str, what: &str| {
            if outside.is_empty() {
                return;
            }
            let body: Vec<&str> = text
                .lines()
                .filter(|l| !l.starts_with("// not compared: "))
                .collect();
            assert!(
                !body.join("\n").contains(outside),
                "{what} {outside:?} reached the generated text:\n{text}"
            );
            if outside.chars().any(char::is_control) {
                assert!(
                    !text.contains(outside),
                    "{what} {outside:?} is written with its control character:\n{text}"
                );
            }
        };
        for bad in OUTSIDE_NAMES {
            // The clock: refused, and the reason ends in the caller's remedy.
            let d = design(bad, None);
            let r = bench_refusal(&d).expect("a clock outside the grammar");
            assert_eq!(r, BenchRefusal::Clock(bad.to_string()));
            assert!(r.message(&remedies).ends_with("— name another"));
            assert_eq!(gtkwave_save(&d, &one_step()), "");

            // The reset: refused, with no remedy offered.
            let d = design("clk", Some((bad, 0)));
            let r = bench_refusal(&d).expect("a reset outside the grammar");
            assert_eq!(r, BenchRefusal::Reset(bad.to_string()));
            let line = r.message(&remedies);
            assert!(line.contains("a mununu limitation"), "{line}");
            assert!(!line.contains("name another"), "{line}");
            assert!(counterexample_testbench(&d, &one_step()).is_none());

            // An input port and a register: the bench is written without them and says so.
            let cex = ExactCounterexample {
                prefix: vec![
                    vec![("state_q".into(), v(0, 3)), (bad.to_string(), v(1, 1))],
                    vec![("state_q".into(), v(1, 3)), (bad.to_string(), v(0, 1))],
                ],
                cycle: vec![],
                inputs: vec![vec![
                    ("go$next".into(), v(1, 1)),
                    (format!("{bad}$next"), v(1, 1)),
                    (bad.to_string(), v(1, 1)),
                ]],
                unreachable_target: vec![],
            };
            let d = design("clk", Some(("rst_n", 0)));
            let b = counterexample_testbench(&d, &cex).expect("the rest of the bench");
            clean(&b.tb_cpp, bad, "a trace name");
            assert_eq!(b.checks, 2, "state_q at both cycles, nothing else");
            assert!(
                b.skipped.iter().any(|s| s.starts_with(&format!("{bad}: "))),
                "{bad:?} is listed: {:?}",
                b.skipped
            );
            assert_eq!(
                b.tb_cpp
                    .lines()
                    .filter(|l| l.starts_with("// not compared: "))
                    .count(),
                b.skipped.len(),
                "one comment line per skipped entry, whatever the name holds"
            );
            clean(&gtkwave_save(&d, &cex), bad, "a trace name");
        }

        // The grammar's own edges: what a register name may be, and what a port name may be.
        for (name, register, port) in [
            ("clk", true, true),
            ("_q0", true, true),
            ("u_sub.cnt", true, false),
            ("mem[3]", true, false),
            ("u.mem[3][10]", true, false),
            ("gen[0].q", false, false),
            ("mem[]", false, false),
            ("mem[1:0]", false, false),
            ("mem[i]", false, false),
            ("a..b", false, false),
            (".a", false, false),
            ("a]", false, false),
        ] {
            assert_eq!(readable_register(name), register, "register {name:?}");
            assert_eq!(plain_name(name), port, "port {name:?}");
        }

        // A design inside every grammar is not refused, and its save file is written.
        let d = design("clk", Some(("rst_n", 0)));
        assert_eq!(bench_refusal(&d), None);
        assert!(gtkwave_save(&d, &one_step()).contains("TOP.rod.clk"));
    }

    /// mununu#1176 — the Verilator binary is one shell word in both recipes, so a
    /// `MUNUNU_VERILATOR_PATH` under a directory with a space runs (it used to split at the
    /// space and the row said the bench did not reach its verdict).
    #[test]
    fn x1176_the_makefile_runs_the_verilator_binary_as_one_word() {
        let sources = vec!["/abs/rod.sv".to_string()];
        let b = counterexample_testbench(
            &ReplayDesign {
                top: "rod",
                clock: "clk",
                reset: None,
                sources: &sources,
            },
            &one_step(),
        )
        .expect("a bench");
        assert_eq!(
            b.makefile.matches("\t\"$(VERILATOR)\" --cc ").count(),
            2,
            "{}",
            b.makefile
        );
        assert_eq!(
            b.makefile.matches("$(VERILATOR)").count(),
            2,
            "no unquoted use is left: {}",
            b.makefile
        );
    }
}

// -------------------------------------------------------------------------------------------------
// Running the bench (2026-10-29). `--replay` / `replay: true` build and run what `--emit-testbench`
// wrote, so the report carries the simulator's verdict instead of leaving `make` to the reader;
// `replay.gtkw` opens the VCD on the trace's own signals.

/// One mismatching comparison the bench printed: `cycle N: NAME = 0xGOT, trace says 0xWANT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayMismatch {
    pub cycle: u32,
    pub register: String,
    pub got: String,
    pub expected: String,
}

/// What the bench's output says, read from `make` / `make trace` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayParse {
    /// `Some(true)` on a `MATCH` line, `Some(false)` on `MISMATCH (n)`, `None` when the bench never
    /// reached its verdict (a compile error, a missing tool).
    pub matched: Option<bool>,
    pub mismatches: usize,
    pub first_mismatch: Option<ReplayMismatch>,
    /// Verilator's `Assertion failed` lines (`%Error: ` stripped): the design's own assertion
    /// firing on the final state.
    pub assertions: Vec<String>,
    /// `make trace` printed `wrote replay.vcd`.
    pub vcd_written: bool,
}

/// Parse the bench's output. Pure, so the lines the bench prints and the lines the report reads
/// are pinned against each other (the seam).
pub fn parse_replay_output(text: &str) -> ReplayParse {
    let mut p = ReplayParse::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line == "MATCH" {
            p.matched = Some(true);
        } else if let Some(rest) = line.strip_prefix("MISMATCH (") {
            p.matched = Some(false);
            p.mismatches = rest.trim_end_matches(')').trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("cycle ") {
            if p.first_mismatch.is_none()
                && let Some((cyc, tail)) = rest.split_once(": ")
                && let Some((reg, vals)) = tail.split_once(" = ")
                && let Some((got, want)) = vals.split_once(", trace says ")
                && let Ok(cycle) = cyc.trim().parse::<u32>()
            {
                p.first_mismatch = Some(ReplayMismatch {
                    cycle,
                    register: reg.trim().to_string(),
                    got: got.trim().to_string(),
                    expected: want.trim().to_string(),
                });
            }
        } else if line.contains("Assertion failed") {
            p.assertions
                .push(line.trim_start_matches("%Error: ").to_string());
        } else if line == "wrote replay.vcd" {
            p.vcd_written = true;
        }
    }
    p
}

/// The bench, built and run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRun {
    pub parse: ReplayParse,
    /// `replay.vcd`'s size when `make trace` wrote it.
    pub vcd_bytes: Option<u64>,
    /// The bench's own lines — verdict, mismatches, assertions, the VCD line — for the report.
    pub log: String,
    /// Everything `make` printed, for `replay.log`.
    pub output: String,
    pub verilator_version: String,
}

/// Build and run the bench in `dir`: `make`, then `make trace` when `with_trace`. `Err` names what
/// stopped it — Verilator or `make` missing, a compile error — so the report says
/// `replay: unavailable — …` rather than nothing. An explicit `MUNUNU_VERILATOR_PATH` reaches the
/// Makefile as `VERILATOR=`.
///
/// A `dir` whose path has whitespace is refused before `make` starts
/// ([`crate::adapter::verilator::build_dir_refusal`], mununu#1176): Verilator's own build refuses
/// it, and its message names a `make` variable, not anything the user passed. The caller that
/// chose the directory names the remedy (the CLI's `--emit-testbench`, the server's
/// `MUNUNU_LIFT_STAGING_ROOT`); this is the backstop for one that did not ask first.
pub fn run_replay(dir: &std::path::Path, with_trace: bool) -> Result<ReplayRun, String> {
    if let Some(why) = crate::adapter::verilator::build_dir_refusal(dir) {
        return Err(why);
    }
    let verilator = crate::adapter::verilator::locate_verilator().map_err(|e| e.message)?;
    let run = |target: Option<&str>| -> Result<String, String> {
        let mut cmd = std::process::Command::new("make");
        cmd.arg("-C").arg(dir);
        if let Some(t) = target {
            cmd.arg(t);
        }
        if verilator.path.is_absolute() {
            cmd.arg(format!("VERILATOR={}", verilator.path.display()));
        }
        // mununu#1196 — through the adapter's one waiting helper: no wall (mununu#1223), and a
        // cancelled run ends the build or the simulation `make` started.
        let (_status, stdout, stderr) = crate::adapter::run_to_end(&mut cmd)
            .map_err(|e| format!("make: {e} (is `make` installed?)"))?;
        Ok(format!("{stdout}{stderr}"))
    };
    let mut output = run(None)?;
    let mut parse = parse_replay_output(&output);
    if parse.matched.is_none() {
        let tail: Vec<&str> = output.lines().rev().take(6).collect();
        return Err(format!(
            "the bench did not reach its verdict (verilator {}): {}",
            verilator.version,
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        ));
    }
    let mut vcd_bytes = None;
    if with_trace {
        let t = run(Some("trace"))?;
        let pt = parse_replay_output(&t);
        if pt.vcd_written {
            vcd_bytes = std::fs::metadata(dir.join("replay.vcd"))
                .ok()
                .map(|m| m.len());
        }
        parse.vcd_written = pt.vcd_written;
        output.push_str(&t);
    }
    let log = output
        .lines()
        .map(str::trim)
        .filter(|l| {
            *l == "MATCH"
                || l.starts_with("MISMATCH (")
                || l.starts_with("cycle ")
                || l.contains("Assertion failed")
                || *l == "wrote replay.vcd"
        })
        .map(|l| l.trim_start_matches("%Error: ").to_string())
        // `make trace` re-runs the bench, so its verdict line repeats: keep one.
        .fold(Vec::<String>::new(), |mut acc, l| {
            if acc.last() != Some(&l) {
                acc.push(l);
            }
            acc
        })
        .join("\n");
    Ok(ReplayRun {
        parse,
        vcd_bytes,
        log,
        output,
        verilator_version: verilator.version,
    })
}

/// A GTKWave save file for `replay.vcd`: the clock, the trace's inputs and the registers it names,
/// in trace order, under `TOP.<top>.` as Verilator dumps them (`--public-flat-rw` puts every design
/// signal there; a hierarchical name keeps its dots). Vectors show in decimal, as the report prints
/// them. Open with `gtkwave replay.vcd replay.gtkw` from the bench's directory.
///
/// It is written beside a bench, so it lists what the bench could name: a signal outside
/// [`readable_register`] is left out (each signal is one line of the file), and a design
/// [`bench_refusal`] refuses has no save file (the empty string).
pub fn gtkwave_save(design: &ReplayDesign<'_>, cex: &ExactCounterexample) -> String {
    if bench_refusal(design).is_some() {
        return String::new();
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut inputs: Vec<(String, u32)> = Vec::new();
    for (name, v) in cex.inputs.first().into_iter().flatten() {
        if name == design.clock
            || synthetic(name)
            || !readable_register(name)
            || !seen.insert(name.clone())
        {
            continue;
        }
        inputs.push((name.clone(), v.bits()));
    }
    let mut regs: Vec<(String, u32)> = Vec::new();
    for (name, v) in cex.prefix.first().into_iter().flatten() {
        if synthetic(name) || !readable_register(name) || !seen.insert(name.clone()) {
            continue;
        }
        regs.push((name.clone(), v.bits()));
    }
    let sig = |name: &str, bits: u32| -> String {
        if bits <= 1 {
            format!("@28\nTOP.{}.{name}\n", design.top)
        } else {
            format!("@24\nTOP.{}.{name}[{}:0]\n", design.top, bits - 1)
        }
    };
    let mut s = String::new();
    s.push_str("[*]\n[*] GTKWave Analyzer save file, generated by mununu `sv verify-auto --emit-testbench`:\n");
    s.push_str("[*] the clock, the trace's inputs and the registers it names. Open: gtkwave replay.vcd replay.gtkw\n[*]\n");
    s.push_str("[dumpfile] \"replay.vcd\"\n[timestart] 0\n[size] 1400 800\n[pos] -1 -1\n");
    s.push_str(&format!(
        "[treeopen] TOP.\n[treeopen] TOP.{}.\n[sst_width] 240\n[signals_width] 280\n[sst_expanded] 1\n",
        design.top
    ));
    s.push_str(&sig(design.clock, 1));
    if !inputs.is_empty() {
        s.push_str("@200\n-inputs\n");
        for (n, b) in &inputs {
            s.push_str(&sig(n, *b));
        }
    }
    if !regs.is_empty() {
        s.push_str("@200\n-registers\n");
        for (n, b) in &regs {
            s.push_str(&sig(n, *b));
        }
    }
    s.push_str("[pattern_trace] 1\n[pattern_trace] 0\n");
    s
}

#[cfg(test)]
mod run_tests {
    use super::*;

    /// mununu#1176 — a build directory whose path has whitespace is refused before `make` (or
    /// the search for Verilator) starts, with Verilator's limit named instead of the tail of its
    /// `make` output.
    #[test]
    fn x1176_a_replay_in_a_directory_with_whitespace_is_refused_before_make_starts() {
        let dir = std::env::temp_dir().join("mununu x1176 replay");
        let err = run_replay(&dir, true).expect_err("refused");
        assert!(err.contains("whitespace"), "{err}");
        assert!(err.contains("a limit of Verilator's build"), "{err}");
        assert!(!dir.exists(), "nothing was created");
    }

    /// The lines the bench prints are the lines the report reads: MATCH, MISMATCH with its first
    /// cycle line, Verilator's assertion line, the VCD line.
    #[test]
    fn the_bench_output_parses_into_verdict_mismatch_assertion_and_vcd() {
        let ok = parse_replay_output(
            "verilator: building\nMATCH\n%Error: p_sva.sv:14: Assertion failed in TOP.p.u_sva: a_one\nwrote replay.vcd\n",
        );
        assert_eq!(ok.matched, Some(true));
        assert_eq!(ok.mismatches, 0);
        assert_eq!(
            ok.assertions,
            vec!["p_sva.sv:14: Assertion failed in TOP.p.u_sva: a_one".to_string()]
        );
        assert!(ok.vcd_written);
        let bad = parse_replay_output(
            "cycle 3: state_q = 0x2, trace says 0x1\ncycle 4: lof_q = 0x0, trace says 0x1\nMISMATCH (2)\n",
        );
        assert_eq!(bad.matched, Some(false));
        assert_eq!(bad.mismatches, 2);
        assert_eq!(
            bad.first_mismatch,
            Some(ReplayMismatch {
                cycle: 3,
                register: "state_q".into(),
                got: "0x2".into(),
                expected: "0x1".into()
            })
        );
        assert!(!bad.vcd_written);
        let none = parse_replay_output(
            "%Error: Cannot find file containing module: 'x'\nmake: *** [obj_dir/Vx] Error 1\n",
        );
        assert_eq!(
            none.matched, None,
            "no verdict line means the bench never ran"
        );
    }

    /// The save file lists the clock, then the inputs, then the registers, under `TOP.<top>.`, with
    /// vectors ranged and in decimal and 1-bit signals in binary; synthetic cells and the clock
    /// among the inputs are left out.
    #[test]
    fn the_gtkwave_save_file_lists_the_traces_signals_in_order() {
        use crate::adapter::btor2::cex_value::CexValue;
        let cell = |n: &str, v: u64, b: u32| (n.to_string(), CexValue::from_u64(v, b));
        let cex = ExactCounterexample {
            prefix: vec![vec![
                cell("state_q", 1, 2),
                cell("start_a", 1, 1),
                cell("mununu_wit_0", 0, 1),
                cell("u_sva.seen", 0, 1),
            ]],
            cycle: Vec::new(),
            inputs: vec![vec![
                cell("clk", 0, 1),
                cell("flow", 0, 1),
                cell("start_cmd", 1, 1),
            ]],
            unreachable_target: Vec::new(),
        };
        let sources = vec!["pump_pair.sv".to_string()];
        let design = ReplayDesign {
            top: "pump_pair",
            clock: "clk",
            reset: None,
            sources: &sources,
        };
        let gtkw = gtkwave_save(&design, &cex);
        let signals: Vec<&str> = gtkw
            .lines()
            .filter(|l| l.starts_with("TOP.") || l.starts_with('-'))
            .collect();
        assert_eq!(
            signals,
            vec![
                "TOP.pump_pair.clk",
                "-inputs",
                "TOP.pump_pair.flow",
                "TOP.pump_pair.start_cmd",
                "-registers",
                "TOP.pump_pair.state_q[1:0]",
                "TOP.pump_pair.start_a",
                "TOP.pump_pair.u_sva.seen"
            ]
        );
        assert!(
            gtkw.contains("@24\nTOP.pump_pair.state_q[1:0]"),
            "a vector shows in decimal"
        );
        assert!(
            gtkw.contains("@28\nTOP.pump_pair.start_a"),
            "a bit shows in binary"
        );
        assert!(
            !gtkw.contains("mununu_wit_0"),
            "a monitor cell has no RTL signal"
        );
        assert!(gtkw.contains("[dumpfile] \"replay.vcd\""));
    }
}
