//! The BTOR2 witness format → a REPLAYED [`BmcTrace`] (mununu#744).
//!
//! `btormc`, `pono` and rIC3 (`--witness`) print a falsifying run in the BTOR2 witness format:
//! `sat`, `b<i>` (which `bad` is falsified), `#0` followed by `<state-index> <bits>` lines (every
//! state's initial value, in declaration order), then one `@k` block per frame with
//! `<input-index> <bits>` lines, and `.`. Later `#k` state blocks are optional in the format and are
//! ignored here: the replay recomputes every state frame from the initial frame and the inputs
//! through the concrete simulator ([`simulate_one_step`]), so a witness whose inputs do not lead
//! where it claims is refused, never shipped; where the falsified `bad` operand carries a symbol it
//! is read at the last frame too.
//!
//! engine: rIC3 `-e ic3 --witness` (bit-level IC3/PDR, subprocess — the same engine the portfolio
//! member runs) as the witness finder under the rescue slice; native BMC (z3 QF_BV, owned) as the
//! fallback and the `--owned-only` path. No verdict is taken from here — the portfolio decided it;
//! this is the shallow path that explains it, which an unrolling does not always find in time
//! (am2901: rIC3's IC3 prints the depth-16 witness in 0.2 s; its own BMC is still at depth 13 after
//! 120 s, native BMC found nothing in 900 s).
#![deny(clippy::unwrap_used)]

use std::collections::HashMap;
use std::time::Duration;

use crate::adapter::btor2::ast::{Btor2File, Node};
use crate::adapter::btor2::bit_blast::{simulate_one_step, simulate_one_step_observe};
use crate::adapter::btor2::cex_value::CexValue;
use crate::adapter::btor2::native_bmc::{
    BmcError, BmcOutcome, BmcTrace, bmc_bad_reachable_witness,
};
use crate::adapter::btor2::{emit, parser};
use crate::adapter::btormc::McVerdict;
use crate::adapter::ric3;

/// A parsed BTOR2 witness: the falsified `bad`, every state's initial value and the input
/// assignment of each frame (`frames[j]` drives the transition out of frame `j`; the last block is
/// what `bad` read). Indices are declaration positions among the file's `state` / `input` lines;
/// values are the MSB-first bit strings the tool printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Btor2Witness {
    pub bad: usize,
    pub init: Vec<(usize, String)>,
    pub frames: Vec<Vec<(usize, String)>>,
}

/// Parse a tool's stdout for the witness block. Lines before `sat` (rIC3's `[INFO]` log, its bare
/// `SAT` verdict line) are skipped; everything after `.` is ignored.
pub fn parse_btor2_witness(text: &str) -> Result<Btor2Witness, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if !lines.by_ref().any(|l| l == "sat") {
        return Err("no `sat` line — not a BTOR2 witness".to_string());
    }
    let bad_line = lines.next().ok_or("the witness ends after `sat`")?;
    let bad = bad_line
        .strip_prefix('b')
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| format!("expected `b<index>` after `sat`, found `{bad_line}`"))?;
    enum Section {
        None,
        Init,
        StateFrame,
        Inputs,
    }
    let mut section = Section::None;
    let mut init = Vec::new();
    let mut frames: Vec<Vec<(usize, String)>> = Vec::new();
    let mut terminated = false;
    for line in lines {
        if line == "." {
            terminated = true;
            break;
        }
        if let Some(k) = line.strip_prefix('#') {
            let k: usize = k
                .parse()
                .map_err(|_| format!("bad state-frame header `{line}`"))?;
            section = if k == 0 {
                Section::Init
            } else {
                Section::StateFrame
            };
            continue;
        }
        if let Some(k) = line.strip_prefix('@') {
            let k: usize = k
                .parse()
                .map_err(|_| format!("bad input-frame header `{line}`"))?;
            if k != frames.len() {
                return Err(format!(
                    "input frame `@{k}` out of order (expected `@{}`)",
                    frames.len()
                ));
            }
            frames.push(Vec::new());
            section = Section::Inputs;
            continue;
        }
        let mut parts = line.split_whitespace();
        let idx = parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| format!("bad assignment line `{line}`"))?;
        let value = parts
            .next()
            .ok_or_else(|| format!("assignment without a value: `{line}`"))?;
        if value.starts_with('[') {
            return Err(format!(
                "array assignment `{line}` — there is no array-state replay"
            ));
        }
        if !value.bytes().all(|b| b == b'0' || b == b'1') {
            return Err(format!("non-binary value in `{line}`"));
        }
        match section {
            Section::Init => init.push((idx, value.to_string())),
            Section::Inputs => {
                if let Some(frame) = frames.last_mut() {
                    frame.push((idx, value.to_string()));
                }
            }
            // A `#k` state block (k > 0) is advisory: the replay recomputes it.
            Section::StateFrame => {}
            Section::None => {
                return Err(format!("assignment `{line}` before any frame header"));
            }
        }
    }
    if !terminated {
        return Err("unterminated witness (no `.`)".to_string());
    }
    if frames.is_empty() {
        return Err("a witness with no input frame".to_string());
    }
    Ok(Btor2Witness { bad, init, frames })
}

struct Cell {
    /// The simulator's key: the symbol, else the simulator's own `st_n<nid>` / `in_n<nid>`.
    key: String,
    /// The symbol, when the cell has one — only named cells enter the trace, as in native BMC.
    name: Option<String>,
    width: u32,
}

/// Replay a parsed witness on `file`: the initial frame from `#0`, every later state frame
/// recomputed through [`simulate_one_step`] from the previous frame and that frame's inputs.
/// Refuses, with the reason, a witness naming a state or input the file does not have, a value
/// whose width is not the cell's, a cell the simulator's word cannot carry (> 128 bits), an array
/// state, or — when the falsified `bad` operand carries a symbol — one whose last frame does not
/// make it true.
pub fn trace_from_witness(file: &Btor2File, w: &Btor2Witness) -> Result<BmcTrace, String> {
    let symbols = parser::collect_symbols(file);
    let mut states: Vec<Cell> = Vec::new();
    let mut inputs: Vec<Cell> = Vec::new();
    for l in &file.lines {
        let (sort, is_state) = match &l.node {
            Node::State { sort, .. } => (*sort, true),
            Node::Input { sort, .. } => (*sort, false),
            _ => continue,
        };
        let width = parser::bv_width(file, sort).ok_or_else(|| {
            format!(
                "node {} is not a bit-vector — an array state has no replay",
                l.nid
            )
        })?;
        if width > 128 {
            return Err(format!(
                "node {} is {width} bits wide; the concrete simulator carries 128",
                l.nid
            ));
        }
        let name = symbols.get(&l.nid).cloned();
        let key = name
            .clone()
            .unwrap_or_else(|| format!("{}{}", if is_state { "st_n" } else { "in_n" }, l.nid));
        let cell = Cell { key, name, width };
        if is_state {
            states.push(cell);
        } else {
            inputs.push(cell);
        }
    }
    let parse_bits = |bits: &str, width: u32, what: &str| -> Result<u128, String> {
        if bits.len() != width as usize {
            return Err(format!(
                "{what}: value `{bits}` has {} bits, the cell {width}",
                bits.len()
            ));
        }
        u128::from_str_radix(bits, 2).map_err(|e| format!("{what}: `{bits}`: {e}"))
    };
    let frame_of = |cells: &[Cell], vals: &HashMap<String, u128>| -> Vec<(String, CexValue)> {
        let mut out: Vec<(String, CexValue)> = cells
            .iter()
            .filter_map(|c| {
                let name = c.name.clone()?;
                let value = vals.get(&c.key).copied().unwrap_or(0);
                Some((name, CexValue::from_u128(value, c.width)))
            })
            .collect();
        out.sort();
        out
    };

    // Frame 0 — a state the tool left out is 0, the simulator's `setundef -zero` convention.
    let mut regs: HashMap<String, u128> = HashMap::new();
    for (idx, bits) in &w.init {
        let cell = states
            .get(*idx)
            .ok_or_else(|| format!("state index {idx} out of range ({} states)", states.len()))?;
        let what = format!("state {}", cell.key);
        regs.insert(cell.key.clone(), parse_bits(bits, cell.width, &what)?);
    }
    let mut state_frames = vec![frame_of(&states, &regs)];
    let mut input_frames = Vec::with_capacity(w.frames.len());
    let last = w.frames.len().saturating_sub(1);
    for (j, frame) in w.frames.iter().enumerate() {
        let mut ins: HashMap<String, u128> = HashMap::new();
        for (idx, bits) in frame {
            let cell = inputs.get(*idx).ok_or_else(|| {
                format!("input index {idx} out of range ({} inputs)", inputs.len())
            })?;
            let what = format!("input {}", cell.key);
            ins.insert(cell.key.clone(), parse_bits(bits, cell.width, &what)?);
        }
        input_frames.push(frame_of(&inputs, &ins));
        if j < last {
            regs = simulate_one_step(file, &regs, &ins)
                .map_err(|e| format!("replay at frame {j}: {e:?}"))?;
            state_frames.push(frame_of(&states, &regs));
        } else {
            check_bad_at(file, w.bad, &regs, &ins)?;
        }
    }
    Ok(BmcTrace {
        states: state_frames,
        inputs: input_frames,
    })
}

/// Read the falsified `bad` at the last frame when its operand carries a symbol — its own line's
/// (an `Op` alias, a state, an input), which the simulator resolves by name; a witness whose last
/// frame does not make it true is refused. An unnamed operand (a monitor's `bad` wire is an
/// internal node) cannot be observed by name and is accepted on the tool's word — the verdict was
/// the portfolio's anyway, and every transition was replayed.
fn check_bad_at(
    file: &Btor2File,
    bad: usize,
    regs: &HashMap<String, u128>,
    ins: &HashMap<String, u128>,
) -> Result<(), String> {
    let Some(signal) = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Bad { signal } => Some(*signal),
            _ => None,
        })
        .nth(bad)
    else {
        return Err(format!(
            "the witness falsifies bad #{bad}, which the file does not declare"
        ));
    };
    let name = file.lookup(signal.nid()).and_then(|l| match &l.node {
        Node::Op {
            symbol: Some(s), ..
        }
        | Node::State {
            symbol: Some(s), ..
        }
        | Node::Input {
            symbol: Some(s), ..
        } => Some(s.clone()),
        _ => None,
    });
    let Some(name) = name else {
        return Ok(());
    };
    let step = simulate_one_step_observe(file, regs, ins, std::slice::from_ref(&name))
        .map_err(|e| format!("observing `{name}` at the last frame: {e:?}"))?;
    let Some(&v) = step.observed.get(&name) else {
        return Ok(());
    };
    if (v & 1 == 1) != signal.is_negated() {
        Ok(())
    } else {
        Err(format!(
            "the replayed last frame does not make bad #{bad} (`{name}`) true"
        ))
    }
}

/// [`bmc_bad_reachable_witness`] with a bit-level member asked first (mununu#744): when rIC3 is
/// present, its IC3 runs with `--witness` under the rescue slice and the parsed witness is replayed
/// before it ships. Any failure on that path — no tool, a timeout, an unusable witness, a replay
/// that disagrees — falls through to native BMC, which is also the `--owned-only` behaviour,
/// unchanged. The depth reported for a routed witness is the witness's own; `max_k` bounds only
/// the native search.
pub fn bad_reachable_witness_routed(
    file: &Btor2File,
    max_k: u32,
) -> Result<(BmcOutcome, Option<BmcTrace>), BmcError> {
    if let Some(trace) = ric3_witness(file) {
        let depth = u32::try_from(trace.states.len().saturating_sub(1)).unwrap_or(u32::MAX);
        return Ok((BmcOutcome::Violated { depth }, Some(trace)));
    }
    bmc_bad_reachable_witness(file, max_k)
}

fn ric3_witness(file: &Btor2File) -> Option<BmcTrace> {
    let bin = ric3::locate_ric3().ok()?;
    // A disabled rescue slice (`MUNUNU_RESCUE_SLICE_MS=0`) disables this member too.
    let slice = Duration::from_millis(crate::adapter::run_budget::rescue_slice_ms()?);
    let btor2 = emit::emit_btor2(file);
    let (verdict, stdout) = match ric3::run_ric3_with_output(
        &bin,
        &btor2,
        ric3::DEFAULT_ENGINE,
        true,
        slice,
        None,
    ) {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(error = ?e, "rIC3 witness: the member failed; native BMC re-derives");
            return None;
        }
    };
    if verdict != McVerdict::Violated {
        return None;
    }
    match parse_btor2_witness(&stdout).and_then(|w| trace_from_witness(file, &w)) {
        Ok(trace) => Some(trace),
        Err(reason) => {
            tracing::warn!(%reason, "rIC3 witness: unusable; native BMC re-derives");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// rIC3 `-e bmc --witness` on `DESIGN`, verbatim (2026-10-26, mununu-sva).
    const RIC3_SAMPLE: &str = "[INFO ] bmc depth: 0\n[INFO ] bmc depth: 1\nSAT\n[INFO ] bmc found counter-example in depth 1\nsat\nb0\n#0\n0 0\n1 0000\n@0\n0 1\n@1\n0 1\n.\n";
    /// `s` is set after one step; `cnt` counts; `bad = s ∧ i`.
    const DESIGN: &str = "1 sort bitvec 1\n2 state 1 s\n3 zero 1\n4 init 1 2 3\n5 one 1\n6 next 1 2 5\n7 input 1 i\n8 sort bitvec 4\n9 state 8 cnt\n10 zero 8\n11 init 8 9 10\n12 one 8\n13 add 8 9 12\n14 next 8 9 13\n15 and 1 2 7\n16 bad 15\n";
    /// The same design with the `bad` operand named, so the last frame is checked.
    const DESIGN_NAMED_BAD: &str = "1 sort bitvec 1\n2 state 1 s\n3 zero 1\n4 init 1 2 3\n5 one 1\n6 next 1 2 5\n7 input 1 i\n8 sort bitvec 4\n9 state 8 cnt\n10 zero 8\n11 init 8 9 10\n12 one 8\n13 add 8 9 12\n14 next 8 9 13\n15 and 1 2 7 fire\n16 bad 15\n";

    #[test]
    fn the_witness_block_parses_after_the_tool_log() {
        let w = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        assert_eq!(w.bad, 0);
        assert_eq!(w.init, vec![(0, "0".to_string()), (1, "0000".to_string())]);
        assert_eq!(w.frames.len(), 2);
        assert_eq!(w.frames[1], vec![(0, "1".to_string())]);
    }

    #[test]
    fn a_replayed_witness_carries_every_frame_at_full_width() {
        let file = parser::parse(DESIGN).unwrap();
        let w = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        let t = trace_from_witness(&file, &w).unwrap();
        assert_eq!(t.states.len(), 2);
        assert_eq!(t.inputs.len(), 2);
        let frame = |c: u64, s: u64| {
            vec![
                ("cnt".to_string(), CexValue::from_u64(c, 4)),
                ("s".to_string(), CexValue::from_u64(s, 1)),
            ]
        };
        assert_eq!(t.states[0], frame(0, 0));
        assert_eq!(t.states[1], frame(1, 1));
        assert_eq!(
            t.inputs[1],
            vec![("i".to_string(), CexValue::from_u64(1, 1))]
        );
    }

    #[test]
    fn the_replayed_states_agree_with_native_bmc() {
        let file = parser::parse(DESIGN).unwrap();
        let w = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        let routed = trace_from_witness(&file, &w).unwrap();
        let (outcome, native) = bmc_bad_reachable_witness(&file, 5).unwrap();
        assert!(
            matches!(outcome, BmcOutcome::Violated { depth: 1 }),
            "{outcome:?}"
        );
        // The inputs of frame 0 are free (`bad` reads `i` only at the last frame); the states are
        // forced, so they are what the two witnesses must share.
        assert_eq!(routed.states, native.unwrap().states);
    }

    #[test]
    fn a_witness_whose_last_frame_does_not_fire_the_named_bad_is_refused() {
        let file = parser::parse(DESIGN_NAMED_BAD).unwrap();
        let lying = "sat\nb0\n#0\n0 0\n1 0000\n@0\n0 1\n@1\n0 0\n.\n";
        let w = parse_btor2_witness(lying).unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(err.contains("does not make bad #0"), "{err}");
        // The same frames with `i = 1` at the end are accepted.
        let honest = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        assert!(trace_from_witness(&file, &honest).is_ok());
    }

    #[test]
    fn malformed_witnesses_are_refused_with_the_reason() {
        let err = |s: &str| parse_btor2_witness(s).unwrap_err();
        assert!(err("SAT\n[INFO] no block\n").contains("no `sat`"));
        assert!(err("sat\nb0\n#0\n0 0\n@0\n0 1\n").contains("unterminated"));
        assert!(err("sat\nb0\n#0\n0 [0] 1\n@0\n0 1\n.\n").contains("array"));
        assert!(err("sat\nb0\n#0\n0 0\n@1\n0 1\n.\n").contains("out of order"));
        assert!(err("sat\nb0\n#0\n0 0\n.\n").contains("no input frame"));
        let file = parser::parse(DESIGN).unwrap();
        let w = parse_btor2_witness("sat\nb3\n#0\n0 0\n@0\n0 1\n.\n").unwrap();
        assert!(
            trace_from_witness(&file, &w)
                .unwrap_err()
                .contains("does not declare")
        );
        let w = parse_btor2_witness("sat\nb0\n#0\n7 0\n@0\n0 1\n.\n").unwrap();
        assert!(
            trace_from_witness(&file, &w)
                .unwrap_err()
                .contains("out of range")
        );
        let w = parse_btor2_witness("sat\nb0\n#0\n1 00\n@0\n0 1\n.\n").unwrap();
        assert!(
            trace_from_witness(&file, &w)
                .unwrap_err()
                .contains("has 2 bits")
        );
    }

    /// rIC3 `-e ic3 --witness` on HWMCC'20 `vis_arrays_am2901` (2026-10-26, mununu-sva), verbatim:
    /// 17 four-bit states, input frames `@0`..`@16` (the depth-16 counterexample); two of the 19
    /// inputs are absent from every frame (rIC3's preprocessing eliminated them — a left-out input
    /// is 0 on replay, the `setundef -zero` convention).
    #[test]
    fn the_am2901_witness_parses_as_seventeen_frames() {
        let w = parse_btor2_witness(include_str!("testdata/am2901_ric3_witness.txt")).unwrap();
        assert_eq!(w.bad, 0);
        assert_eq!(w.init.len(), 17);
        assert!(
            w.init.iter().all(|(_, bits)| bits.len() == 4),
            "{:?}",
            &w.init[..3]
        );
        assert_eq!(w.frames.len(), 17);
        let sizes: Vec<usize> = w.frames.iter().map(Vec::len).collect();
        assert!(sizes.iter().all(|&n| n == 17), "{sizes:?}");
    }

    /// mununu#744 — the row that motivated the routing: HWMCC'20 `vis_arrays_am2901`, a depth-16
    /// counterexample rIC3's IC3 prints in 0.2 s and native BMC did not find in 900 s.
    #[test]
    #[ignore = "needs rIC3 and MUNUNU_HWMCC_FLAT (the HWMCC'20 flat bv directory); mununu-sva"]
    fn e2e_ric3_finds_the_am2901_witness_within_the_slice() {
        let Ok(flat) = std::env::var("MUNUNU_HWMCC_FLAT") else {
            eprintln!("MUNUNU_HWMCC_FLAT unset — skipped");
            return;
        };
        let text = std::fs::read_to_string(format!("{flat}/vis_arrays_am2901.btor2")).unwrap();
        let file = parser::parse(&text).unwrap();
        let started = std::time::Instant::now();
        let (outcome, trace) = bad_reachable_witness_routed(&file, 17).unwrap();
        let elapsed = started.elapsed();
        assert!(
            matches!(outcome, BmcOutcome::Violated { .. }),
            "{outcome:?}"
        );
        let trace = trace.expect("a replayed witness");
        eprintln!(
            "am2901: depth {} in {:.1} s",
            trace.states.len() - 1,
            elapsed.as_secs_f64()
        );
        assert!(trace.states.len() >= 2);
        assert!(elapsed < Duration::from_secs(120), "took {elapsed:?}");
    }
}
