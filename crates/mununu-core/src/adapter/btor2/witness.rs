//! The BTOR2 witness format → a REPLAYED [`BmcTrace`] (mununu#744).
//!
//! `btormc`, `pono` and rIC3 (`--witness`) print a falsifying run in the BTOR2 witness format:
//! `sat`, `b<i>` (which `bad` is falsified), `#0` followed by `<state-index> <bits>` lines (every
//! state's initial value, in declaration order), then one `@k` block per frame with
//! `<input-index> <bits>` lines, and `.`. The replay recomputes every state frame from the initial
//! frame and the inputs through the concrete simulator ([`simulate_one_step_probe`]) and checks
//! what the tool claims rather than taking its word: the `#0` frame against every `init`, every
//! `constraint` on every frame, a later `#k` state block against the simulator (a register without
//! `next` is free, and takes the block's value), and the falsified `bad` operand, read by its node
//! at the last frame. A witness that fails any of these is refused, never shipped.
//!
//! engine: rIC3 `-e ic3 --witness` (bit-level IC3/PDR, subprocess — the same engine the portfolio
//! member runs) as the witness finder under the rescue slice; native BMC (z3 QF_BV, owned) as the
//! fallback and the `--owned-only` path. No verdict is taken from here — the portfolio decided it;
//! this is the shallow path that explains it, which an unrolling does not always find in time
//! (am2901: rIC3's IC3 prints the depth-16 witness in 0.2 s; its own BMC is still at depth 13 after
//! 120 s, native BMC found nothing in 900 s).
#![deny(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Operand};
use crate::adapter::btor2::bit_blast::{ProbedStep, eval_const_value, simulate_one_step_probe};
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
    /// State assignments the tool printed at frames `k > 0` (`#k` blocks), in order. The replay
    /// takes them for a register without `next` (it is free there) and checks them against the
    /// simulator for every other register.
    pub states: Vec<(usize, Vec<(usize, String)>)>,
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
    let mut states: Vec<(usize, Vec<(usize, String)>)> = Vec::new();
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
                states.push((k, Vec::new()));
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
            Section::StateFrame => {
                if let Some((_, block)) = states.last_mut() {
                    block.push((idx, value.to_string()));
                }
            }
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
    Ok(Btor2Witness {
        bad,
        init,
        frames,
        states,
    })
}

struct Cell {
    /// The simulator's key: the symbol, else the simulator's own `st_n<nid>` / `in_n<nid>`.
    key: String,
    /// The symbol, when the cell has one — only named cells enter the trace, as in native BMC.
    name: Option<String>,
    width: u32,
}

/// Replay a parsed witness on `file`: the initial frame from `#0`, every later state frame
/// recomputed through the concrete simulator ([`simulate_one_step_probe`]) from the previous frame
/// and that frame's inputs. Every frame is checked rather than taken on the tool's word: the `#0`
/// values satisfy every `init` (a state the witness leaves out of `#0` takes its constant `init`,
/// else 0); every `constraint` holds on every frame; a `#k` value the witness
/// gives for a register WITH `next` agrees with the simulator, and a register WITHOUT `next` takes
/// it (the register is free there; left out, it holds its value — one of its free choices); and
/// the falsified `bad`'s operand is true at the last frame, read by its node, whether or not it has
/// a symbol. Refuses, with the reason, a witness naming a state or input the file does not have, a
/// value whose width is not the cell's, a cell the simulator's word cannot carry (> 128 bits), or
/// an array state.
pub fn trace_from_witness(file: &Btor2File, w: &Btor2Witness) -> Result<BmcTrace, String> {
    let symbols = parser::collect_symbols(file);
    let mut states: Vec<Cell> = Vec::new();
    let mut inputs: Vec<Cell> = Vec::new();
    let mut key_of: HashMap<Nid, (String, u32)> = HashMap::new();
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
        key_of.insert(l.nid, (key.clone(), width));
        let cell = Cell { key, name, width };
        if is_state {
            states.push(cell);
        } else {
            inputs.push(cell);
        }
    }
    let bad: Operand = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Bad { signal } => Some(*signal),
            _ => None,
        })
        .nth(w.bad)
        .ok_or_else(|| {
            format!(
                "the witness falsifies bad #{}, which the file does not declare",
                w.bad
            )
        })?;
    // (state key, state width, the `init` operand) of every `init` line.
    let inits: Vec<(String, u32, Operand)> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, value, .. } => {
                let (key, width) = key_of.get(state)?;
                Some((key.clone(), *width, *value))
            }
            _ => None,
        })
        .collect();
    let with_next: HashSet<Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    let nextless: HashSet<String> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { .. } if !with_next.contains(&l.nid) => {
                key_of.get(&l.nid).map(|(key, _)| key.clone())
            }
            _ => None,
        })
        .collect();
    let probes: Vec<Nid> = std::iter::once(bad.nid())
        .chain(inits.iter().map(|(_, _, v)| v.nid()))
        .collect();
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
    // An operand's value at the current cycle: the probed node, negated and masked to `width`.
    let operand_value = |step: &ProbedStep, op: Operand, width: u32| -> Result<u128, String> {
        let raw = step
            .probed
            .get(&op.nid())
            .copied()
            .ok_or_else(|| format!("node {} was not evaluated by the simulator", op.nid()))?;
        let mask = if width >= 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        Ok(if op.is_negated() {
            !raw & mask
        } else {
            raw & mask
        })
    };

    // Frame 0 — a state the tool left out takes its constant `init` when it has one, else 0 (the
    // simulator's `setundef -zero` convention); a relational `init` is checked after the first
    // step like every other, so an omitted state it does not allow at 0 is still refused.
    let mut regs: HashMap<String, u128> = HashMap::new();
    for (idx, bits) in &w.init {
        let cell = states
            .get(*idx)
            .ok_or_else(|| format!("state index {idx} out of range ({} states)", states.len()))?;
        let what = format!("state {}", cell.key);
        regs.insert(cell.key.clone(), parse_bits(bits, cell.width, &what)?);
    }
    let consts: HashMap<Nid, &ConstValue> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Const { value, .. } => Some((l.nid, value)),
            _ => None,
        })
        .collect();
    for (key, width, value) in &inits {
        if regs.contains_key(key) {
            continue;
        }
        let Some(c) = consts.get(&value.nid()) else {
            continue;
        };
        let v = eval_const_value(c, *width)
            .ok()
            .and_then(|bv| bv.to_u128())
            .ok_or_else(|| {
                format!("state `{key}`: its `init` constant does not fit {width} bits")
            })?;
        let mask = if *width >= 128 {
            u128::MAX
        } else {
            (1u128 << *width) - 1
        };
        let v = if value.is_negated() {
            !v & mask
        } else {
            v & mask
        };
        regs.insert(key.clone(), v);
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
        let step = simulate_one_step_probe(file, &regs, &ins, &probes)
            .map_err(|e| format!("replay at frame {j}: {e:?}"))?;
        if !step.admissible {
            return Err(format!("a `constraint` fails at frame {j}"));
        }
        if j == 0 {
            for (key, width, value) in &inits {
                let want = operand_value(&step, *value, *width)?;
                let got = regs.get(key).copied().unwrap_or(0);
                if got != want {
                    return Err(format!(
                        "state `{key}` is {got} at #0; its `init` requires {want}"
                    ));
                }
            }
        }
        if j < last {
            regs = step.next_state;
            let k = j + 1;
            for (idx, bits) in w
                .states
                .iter()
                .filter(|(at, _)| *at == k)
                .flat_map(|(_, a)| a)
            {
                let cell = states.get(*idx).ok_or_else(|| {
                    format!(
                        "state index {idx} out of range ({} states) at #{k}",
                        states.len()
                    )
                })?;
                let v = parse_bits(bits, cell.width, &format!("state {} at #{k}", cell.key))?;
                if nextless.contains(&cell.key) {
                    regs.insert(cell.key.clone(), v);
                } else {
                    let computed = regs.get(&cell.key).copied().unwrap_or(0);
                    if computed != v {
                        return Err(format!(
                            "the witness gives state `{}` = {v} at #{k}; the replay computes {computed}",
                            cell.key
                        ));
                    }
                }
            }
            state_frames.push(frame_of(&states, &regs));
        } else {
            let fired = operand_value(&step, bad, 1)? == 1;
            if !fired {
                return Err(format!(
                    "the replayed last frame does not make bad #{} true",
                    w.bad
                ));
            }
        }
    }
    Ok(BmcTrace {
        states: state_frames,
        inputs: input_frames,
    })
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

/// The trace a `violated` verdict ships with, or why it ships without one (mununu#1045). Exactly
/// one of `trace` and `unavailable_reason` is set.
#[derive(Debug, Clone)]
pub struct ShippedWitness {
    pub trace: Option<BmcTrace>,
    /// Set ONLY when native BMC searched every frame up to the bound and found no path to `bad`:
    /// the counterexample another engine found is deeper than `--witness-max-k`. A stop on a
    /// budget, an encode failure or a missing model is NOT this (it says nothing about depth).
    pub unavailable_within_k: Option<u32>,
    /// Why there is no trace, in words; `None` exactly when `trace` is set.
    pub unavailable_reason: Option<String>,
}

/// [`bad_reachable_witness_routed`] under the caller's wall clock (mununu#1045): `deadline` (the
/// remainder of `--timeout-ms` / the request's `timeout_ms`) is installed as this thread's
/// [`run_budget`](crate::adapter::run_budget), which native BMC polls per depth and clamps each
/// query to, and the rIC3 member's slice never exceeds it. `None` keeps the previous unbounded
/// behaviour. Every way of ending without a trace names its cause.
///
/// engine: rIC3 `-e ic3 --witness` (subprocess, bit-level IC3, under min(rescue slice, time left))
/// then native BMC (owned, z3 QF_BV unrolling to `max_k`, wall = time left); role: witness
/// re-derivation after the portfolio's `Reachable` — the verdict is never changed here.
pub fn witness_for_violation(
    file: &Btor2File,
    max_k: u32,
    deadline: Option<std::time::Instant>,
) -> ShippedWitness {
    use crate::adapter::run_budget::{Budget, enter};
    let none = |reason: String, within: Option<u32>| ShippedWitness {
        trace: None,
        unavailable_within_k: within,
        unavailable_reason: Some(reason),
    };
    let budget = match deadline {
        Some(d) => {
            let left = d.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return none(
                    "the time budget was spent by the verdict; no witness search ran".to_string(),
                    None,
                );
            }
            Budget::with_ms(u64::try_from(left.as_millis()).unwrap_or(u64::MAX))
        }
        None => Budget::unbounded(),
    };
    let _guard = enter(&budget);
    match bad_reachable_witness_routed(file, max_k) {
        Ok((BmcOutcome::Violated { .. }, Some(t))) => ShippedWitness {
            trace: Some(t),
            unavailable_within_k: None,
            unavailable_reason: None,
        },
        Ok((BmcOutcome::Violated { depth }, None)) => none(
            format!("native BMC reached `bad` at depth {depth} but the solver returned no model"),
            None,
        ),
        Ok((BmcOutcome::NoCexWithin { k }, _)) => none(
            format!(
                "native BMC found no path to `bad` within {k} cycles; the counterexample the \
                 verdict rests on is deeper (raise --witness-max-k)"
            ),
            Some(k),
        ),
        Err(e) => none(format!("the witness search stopped: {e}"), None),
    }
}

fn ric3_witness(file: &Btor2File) -> Option<BmcTrace> {
    let bin = ric3::locate_ric3().ok()?;
    // A disabled rescue slice (`MUNUNU_RESCUE_SLICE_MS=0`) disables this member too; under a
    // caller's budget (mununu#1045) the slice never outlives it.
    let slice_ms = crate::adapter::run_budget::rescue_slice_ms()?;
    let slice_ms = match crate::adapter::run_budget::current().remaining_ms() {
        Some(0) => return None,
        Some(left) => slice_ms.min(left),
        None => slice_ms,
    };
    let slice = Duration::from_millis(slice_ms);
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

    /// `bad` here is an unnamed internal node (`and s i`): the replay reads it by node and refuses
    /// a last frame that does not fire it (the tool's word is not enough).
    #[test]
    fn a_witness_whose_last_frame_does_not_fire_an_unnamed_bad_is_refused() {
        let file = parser::parse(DESIGN).unwrap();
        let w = parse_btor2_witness("sat\nb0\n#0\n0 0\n1 0000\n@0\n0 1\n@1\n0 0\n.\n").unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(err.contains("does not make bad #0 true"), "{err}");
        // The same witness with `i` high at the end replays.
        let ok = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        trace_from_witness(&file, &ok).expect("the firing witness replays");
    }

    #[test]
    fn a_frame_that_breaks_a_constraint_is_refused() {
        let text = format!("{DESIGN}17 not 1 7\n18 constraint 17\n");
        let file = parser::parse(&text).unwrap();
        let w = parse_btor2_witness(RIC3_SAMPLE).unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(err.contains("`constraint` fails at frame 0"), "{err}");
    }

    #[test]
    fn an_initial_frame_that_violates_an_init_is_refused() {
        let file = parser::parse(DESIGN).unwrap();
        // `s` starts at 1 where its `init` says 0.
        let w = parse_btor2_witness("sat\nb0\n#0\n0 1\n1 0000\n@0\n0 1\n@1\n0 1\n.\n").unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(
            err.contains("`s` is 1 at #0; its `init` requires 0"),
            "{err}"
        );
    }

    /// `f` has no `next`: the replay takes its `#k` value (it is free), and holds it when the
    /// witness gives none — here holding 0 never fires `f ∧ i`.
    #[test]
    fn a_register_without_next_follows_the_witness_frame_values() {
        let text = "1 sort bitvec 1\n2 state 1 f\n3 zero 1\n4 init 1 2 3\n5 input 1 i\n\
                    6 and 1 2 5\n7 bad 6\n";
        let file = parser::parse(text).unwrap();
        let w = parse_btor2_witness("sat\nb0\n#0\n0 0\n@0\n0 1\n#1\n0 1\n@1\n0 1\n.\n").unwrap();
        assert_eq!(w.states, vec![(1, vec![(0, "1".to_string())])]);
        let trace = trace_from_witness(&file, &w).expect("the free register follows #1");
        assert_eq!(
            trace.states[1],
            vec![("f".to_string(), CexValue::from_u128(1, 1))]
        );
        let held = parse_btor2_witness("sat\nb0\n#0\n0 0\n@0\n0 1\n@1\n0 1\n.\n").unwrap();
        let err = trace_from_witness(&file, &held).unwrap_err();
        assert!(err.contains("does not make bad #0 true"), "{err}");
    }

    /// `cnt` starts at 3; a witness that leaves `cnt` out of `#0` takes that `init`, not 0 (an
    /// honest witness may omit a state whose value its `init` fixes).
    #[test]
    fn a_state_left_out_of_frame_0_takes_its_constant_init() {
        let text = DESIGN.replace("10 zero 8\n", "10 constd 8 3\n");
        let file = parser::parse(&text).unwrap();
        let w = parse_btor2_witness("sat\nb0\n#0\n0 0\n@0\n0 1\n@1\n0 1\n.\n").unwrap();
        let t = trace_from_witness(&file, &w).expect("the omitted state takes its init");
        assert_eq!(
            t.states[0],
            vec![
                ("cnt".to_string(), CexValue::from_u64(3, 4)),
                ("s".to_string(), CexValue::from_u64(0, 1)),
            ]
        );
        assert_eq!(
            t.states[1][0],
            ("cnt".to_string(), CexValue::from_u64(4, 4))
        );
    }

    #[test]
    fn a_frame_value_that_contradicts_a_next_is_refused() {
        let file = parser::parse(DESIGN).unwrap();
        // `s` has `next 1`, so `#1` must say 1; the witness says 0.
        let w = parse_btor2_witness("sat\nb0\n#0\n0 0\n1 0000\n@0\n0 1\n#1\n0 0\n@1\n0 1\n.\n")
            .unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(
            err.contains("state `s` = 0 at #1; the replay computes 1"),
            "{err}"
        );
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

    /// A 16-bit counter whose `bad` (`cnt == 3000`) is 3000 steps deep: native BMC unrolls frame
    /// by frame, so a short deadline lands mid-search.
    const DEEP: &str = "1 sort bitvec 16\n2 sort bitvec 1\n3 state 1 cnt\n4 zero 1\n5 init 1 3 4\n\
                        6 one 1\n7 add 1 3 6\n8 next 1 3 7\n9 constd 1 3000\n10 eq 2 3 9\n11 bad 10\n";

    /// mununu#1045 — the caller's deadline bounds the witness search (a CLI call with
    /// `--timeout-ms 300000 --witness` ran > 1,500 s here), and the stop is named as a budget
    /// stop, never as "no path within the bound".
    #[test]
    fn the_witness_search_stops_at_the_callers_deadline_and_says_so() {
        let file = parser::parse(DEEP).unwrap();
        let t0 = std::time::Instant::now();
        let w = witness_for_violation(&file, 3000, Some(t0 + Duration::from_millis(1)));
        let elapsed = t0.elapsed();
        assert!(w.trace.is_none(), "a 3000-deep path in 1 ms");
        assert_eq!(
            w.unavailable_within_k, None,
            "a budget stop is not a searched bound"
        );
        let reason = w.unavailable_reason.unwrap();
        assert!(reason.contains("budget"), "{reason}");
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
        // Unbounded, the same search ships the path: the stop above was the deadline's.
        let full = witness_for_violation(&file, 3000, None);
        assert_eq!(full.trace.map(|t| t.states.len()), Some(3001));
        assert_eq!(full.unavailable_reason, None);
    }

    /// The depth bound itself searched without a path: `unavailable_within_k` names it.
    #[test]
    fn a_bound_searched_without_a_path_names_the_bound() {
        let file = parser::parse(DEEP).unwrap();
        let w = witness_for_violation(&file, 5, None);
        assert!(w.trace.is_none());
        assert_eq!(w.unavailable_within_k, Some(5));
        let reason = w.unavailable_reason.unwrap();
        assert!(reason.contains("within 5 cycles"), "{reason}");
    }

    #[test]
    fn a_budget_spent_by_the_verdict_runs_no_search() {
        let file = parser::parse(DEEP).unwrap();
        let w = witness_for_violation(&file, 3000, Some(std::time::Instant::now()));
        assert!(w.trace.is_none());
        assert_eq!(w.unavailable_within_k, None);
        assert!(w.unavailable_reason.unwrap().contains("spent"));
    }
}
