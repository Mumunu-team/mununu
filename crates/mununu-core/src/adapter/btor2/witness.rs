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
use crate::adapter::reach_portfolio::ReachOutcome;
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

/// Where a replayed witness ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayEnd {
    /// The witness's last frame, which must make the falsified `bad` true (the format's meaning).
    LastFrame,
    /// The first frame that makes the falsified `bad` true; later frames are dropped. For a
    /// witness of a STICKY-bad model (`ric3-bmc`, mununu#997): its last frame makes `seen ∨ bad`
    /// true, and the original `bad` fired at some earlier frame of the same run.
    FirstFiring,
}

/// The simulator's own width refusal (a cell wider than its 128-bit word), the one replay error
/// that is "unavailable" rather than a failed replay (mununu#1143).
const WIDTH_REFUSAL: &str = "the concrete simulator carries 128";

/// mununu#1143 — whether a replay error is the simulator's width refusal ([`WIDTH_REFUSAL`]).
pub(crate) fn is_width_refusal(why: &str) -> bool {
    why.contains(WIDTH_REFUSAL)
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
    replay_witness(file, w, ReplayEnd::LastFrame)
}

/// [`trace_from_witness`] ending at the FIRST frame that makes the falsified `bad` true
/// ([`ReplayEnd::FirstFiring`]): the trace of the original design behind a sticky-bad witness.
/// Refused when no frame of the witness fires `bad`.
pub fn trace_from_witness_prefix(file: &Btor2File, w: &Btor2Witness) -> Result<BmcTrace, String> {
    replay_witness(file, w, ReplayEnd::FirstFiring)
}

/// [`trace_from_witness`] with the end rule as a parameter; both checks above apply either way.
pub fn replay_witness(
    file: &Btor2File,
    w: &Btor2Witness,
    end: ReplayEnd,
) -> Result<BmcTrace, String> {
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
                "node {} is {width} bits wide; {WIDTH_REFUSAL}",
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
            .map_err(|e| format!("replay at frame {j}: {e}"))?;
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
        let fired = operand_value(&step, bad, 1)? == 1;
        if end == ReplayEnd::FirstFiring && fired {
            // The original `bad` fires here: the trace ends at this frame (its inputs are what
            // `bad` read), whatever the witness goes on to say.
            break;
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
        } else if !fired {
            return Err(match end {
                ReplayEnd::LastFrame => {
                    format!("the replayed last frame does not make bad #{} true", w.bad)
                }
                ReplayEnd::FirstFiring => {
                    format!("no replayed frame (0..={last}) makes bad #{} true", w.bad)
                }
            });
        }
    }
    Ok(BmcTrace {
        states: state_frames,
        inputs: input_frames,
    })
}

impl Btor2Witness {
    /// Drop every state assignment whose index is `n_states` or more — the states a model
    /// transform appended after the design's own (the sticky-bad `mununu_bad_seen_*` registers,
    /// `ric3::sticky_bads`), which the original file does not declare. Inputs are untouched: no
    /// transform appends one.
    pub fn drop_states_from(&mut self, n_states: usize) {
        self.init.retain(|(idx, _)| *idx < n_states);
        for (_, block) in &mut self.states {
            block.retain(|(idx, _)| *idx < n_states);
        }
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

/// The trace a `violated` verdict ships with, or why it ships without one (mununu#1045). Exactly
/// one of `trace` and `unavailable_reason` is set.
#[derive(Debug, Clone)]
pub struct ShippedWitness {
    pub trace: Option<BmcTrace>,
    /// N5.4 — the member whose counterexample `trace` is: a portfolio member that carried it out
    /// of the run, or `native-bmc` for the re-derivation. `None` exactly when `trace` is.
    pub by: Option<&'static str>,
    /// mununu#1057 — with `trace`: whether `by` established that no shorter counterexample
    /// exists ([`CarriedTrace::shortest`]; `native-bmc` always does, one query per depth).
    /// `None` when the member does not say, and without a trace.
    pub shortest: Option<bool>,
    /// Set ONLY when native BMC searched every frame up to the bound and found no path to `bad`:
    /// the counterexample another engine found is deeper than `--witness-max-k`. A stop on a
    /// budget, an encode failure or a missing model is NOT this (it says nothing about depth).
    pub unavailable_within_k: Option<u32>,
    /// Why there is no trace, in words; `None` exactly when `trace` is set.
    pub unavailable_reason: Option<String>,
    /// mununu#1076 — the members of the fallback search that PROVED `bad` unreachable under the
    /// `violated` verdict this witness is for: a soundness alarm (one of the two engines is
    /// wrong), and the caller never ships `violated` on it — the verdict becomes the
    /// contradiction one ([`apply_fallback_contradiction`]; a verify-auto row goes to `unknown`
    /// with `engine-contradiction` as its bottom reason). Set whether or not a trace was found
    /// beside it (a path contradicted by a proof is not evidence). Empty on a carried trace and
    /// on every path where no member proved unreachability.
    pub contradicted_by: Vec<&'static str>,
}

/// mununu#1076 — the report's contradiction rule applied to the witness fallback: when the
/// fallback's carriers include a member that proved `bad` unreachable, the outcome's verdict
/// becomes [`ReachVerdict::Contradiction`] (`unknown` on every surface, `contradiction: true`),
/// with that member in `unreachable_by` beside the members that found it reachable, so the
/// report names both sides; a trace found beside the proof is not shipped (it is not evidence),
/// and `unavailable_reason` says why. Returns the witness unchanged without such a member.
pub fn apply_fallback_contradiction(
    outcome: &mut ReachOutcome,
    witness: ShippedWitness,
) -> ShippedWitness {
    if witness.contradicted_by.is_empty() {
        return witness;
    }
    for m in &witness.contradicted_by {
        if !outcome.unreachable_by.contains(m) {
            outcome.unreachable_by.push(m);
        }
    }
    outcome.verdict = crate::adapter::reach_portfolio::ReachVerdict::Contradiction;
    ShippedWitness {
        trace: None,
        by: None,
        shortest: None,
        unavailable_within_k: None,
        unavailable_reason: Some(format!(
            "contradiction: {} proved `bad` unreachable under the `violated` verdict — a \
             soundness alarm (one of the two engines is wrong); the verdict is the contradiction \
             and no path ships on it",
            witness.contradicted_by.join(", ")
        )),
        contradicted_by: witness.contradicted_by,
    }
}

/// mununu#1076 — the fallback's reading of a second portfolio run (the seam the reproducer
/// drives): the carried trace when one was found, the notes for every member that carried
/// none, and the members that PROVED `bad` unreachable — the contradiction, named whether or
/// not a trace came with it.
fn fallback_view(
    again: ReachOutcome,
    members: &crate::adapter::reach_portfolio::ReachMembers,
    notes: &mut Vec<String>,
) -> (Option<ShippedWitness>, Vec<&'static str>) {
    // A carrier that PROVED `bad` unreachable here contradicts the verdict this trace is for:
    // the alarm reaches the report as the contradiction verdict, never as a log line alone.
    let contradicted: Vec<&'static str> = again.unreachable_by.clone();
    for m in &contradicted {
        tracing::error!(
            member = m,
            "witness fallback: a member proved `bad` unreachable under a `violated` verdict"
        );
    }
    if let Some(carried) = again.trace {
        return (
            Some(ShippedWitness {
                trace: Some(carried.trace),
                by: Some(carried.by),
                shortest: carried.shortest,
                unavailable_within_k: None,
                unavailable_reason: None,
                contradicted_by: contradicted.clone(),
            }),
            contradicted,
        );
    }
    for (m, why) in again
        .trace_replay_failed
        .iter()
        .map(|(m, why)| (m, format!("its witness did not replay ({why})")))
        .chain(
            again
                .refused
                .iter()
                .map(|(m, why)| (m, format!("refused ({why})"))),
        )
        .chain(
            again
                .abstained
                .iter()
                .map(|(m, why)| (m, format!("stopped ({why})"))),
        )
        .chain(
            again
                .undecided
                .iter()
                .map(|(m, why)| (m, format!("found nothing ({why})"))),
        )
    {
        notes.push(format!("{m}: {why}"));
    }
    for m in &contradicted {
        notes.push(format!(
            "{m}: proved `bad` unreachable — CONTRADICTION with the verdict (a soundness \
             alarm; one of the two engines is wrong)"
        ));
    }
    for m in members.names() {
        let named = |list: &[(&'static str, String)]| list.iter().any(|(n, _)| *n == m);
        if !again.reachable_by.contains(&m)
            && !contradicted.contains(&m)
            && !named(&again.trace_replay_failed)
            && !named(&again.refused)
            && !named(&again.abstained)
            && !named(&again.undecided)
        {
            notes.push(format!("{m}: no counterexample before the search ended"));
        } else if again.reachable_by.contains(&m) && !named(&again.trace_replay_failed) {
            notes.push(format!("{m}: found `bad` reachable but carried no trace"));
        }
    }
    (None, contradicted)
}

/// The trace a `violated` verdict ships with (N5.4): the counterexample the deciding member
/// carried out of the portfolio ([`ReachOutcome::trace`], replayed on the design before it got
/// there) — else, under the caller's wall clock (mununu#1045: `deadline`, the remainder of
/// `--timeout-ms` / the request's `timeout_ms`, installed as this thread's
/// [`run_budget`](crate::adapter::run_budget); `None` = unbounded), the fallback: the portfolio
/// again restricted to the trace-carrying members ([`ReachMembers::TRACE_CARRIERS`], minus every
/// member whose own witness already failed to replay — the same run would fail the same way),
/// then native BMC to `max_k`, the one step that can set `unavailable_within_k`. Every way of
/// ending without a trace names each member and its miss. A carried trace is the deciding
/// member's own, not necessarily the shallowest; native BMC's is the shallowest.
///
/// engine: the carried trace (none run); else `sat-bmc` / `ic3` (owned, CaDiCaL) and, where the
/// image has them, btormc / rIC3 `-e ic3 --witness` / rIC3 `-e bmc --bmc-dyn-step --witness`
/// (subprocess), in parallel under the time left, every witness replayed on the concrete
/// simulator; then native BMC (owned, z3 QF_BV unrolling to `max_k`, wall = time left). Role:
/// the trace behind the portfolio's `Reachable` — the verdict is never changed here.
pub fn witness_for_violation(
    file: &Btor2File,
    max_k: u32,
    deadline: Option<std::time::Instant>,
    outcome: &ReachOutcome,
    owned_only: bool,
) -> ShippedWitness {
    if let Some(carried) = &outcome.trace {
        return ShippedWitness {
            trace: Some(carried.trace.clone()),
            by: Some(carried.by),
            shortest: carried.shortest,
            unavailable_within_k: None,
            unavailable_reason: None,
            contradicted_by: Vec::new(),
        };
    }
    let run_carriers = outcome.verdict == crate::adapter::reach_portfolio::ReachVerdict::Reachable;
    search_witness_with(
        file,
        max_k,
        deadline,
        &outcome.trace_replay_failed,
        owned_only,
        run_carriers,
    )
}

/// The trace search behind a `bad` the caller already knows is reachable, with no carried trace
/// to ship (N5.4 seam, for the callers that decide on one model and search another: the
/// `sv verify-auto` safety witness over its bounded-witness monitor, the fault adversary's
/// injected design). The trace-carrying members ([`ReachMembers::TRACE_CARRIERS`] minus `skip`,
/// owned ones only under `owned_only`) race under the time left, every witness replayed on
/// `file`; then native BMC to `max_k`. The budget is a CHILD of this thread's current
/// [`run_budget`](crate::adapter::run_budget) (a `verify-auto` per-property budget bounds it as
/// well as `deadline`); every miss is named.
///
/// engine: as [`witness_for_violation`]'s fallback.
pub fn search_witness(
    file: &Btor2File,
    max_k: u32,
    deadline: Option<std::time::Instant>,
    skip: &[(&'static str, String)],
    owned_only: bool,
) -> ShippedWitness {
    search_witness_with(file, max_k, deadline, skip, owned_only, true)
}

/// mununu#1057 — the shortest path behind a shipped witness, for the consumers that read a
/// trace cycle by cycle (the `sv verify-auto` slice and `--localize`, the fault adversary's
/// attack): a trace whose carrier established it is the shortest (`shortest == Some(true)`)
/// ships as it is; any other (`sat-bmc`'s dynamic step answered a window with a deeper path;
/// `ic3` and the tools' traces are their own) is re-derived by native BMC bounded by the
/// trace's own depth — one query per depth, so the first path it finds is the first one, and
/// the bound guarantees one exists at or below it. A re-derivation that misses (its budget,
/// an encode failure) keeps the carried trace and says so in the log; the verdict is never
/// touched. Without a trace, unchanged.
///
/// engine: native BMC (owned, z3 QF_BV unrolling to the carried depth, under this thread's
/// run budget); role: the shortest trace behind a verdict another member decided.
pub fn shortest_path_for(file: &Btor2File, shipped: ShippedWitness) -> ShippedWitness {
    let Some(trace) = &shipped.trace else {
        return shipped;
    };
    if shipped.shortest == Some(true) {
        return shipped;
    }
    let depth = u32::try_from(trace.states.len().saturating_sub(1)).unwrap_or(u32::MAX);
    match bmc_bad_reachable_witness(file, depth) {
        Ok((BmcOutcome::Violated { depth: first }, Some(shortest))) => {
            tracing::info!(
                by = shipped.by,
                depth,
                first,
                "witness: the carried path was not established shortest; re-derived by native BMC"
            );
            ShippedWitness {
                trace: Some(shortest),
                by: Some("native-bmc"),
                shortest: Some(true),
                unavailable_within_k: None,
                unavailable_reason: None,
                contradicted_by: shipped.contradicted_by.clone(),
            }
        }
        other => {
            // Silence is a defect: the carried path ships, and the log says the shortest
            // one was not re-derived and why.
            tracing::warn!(
                by = shipped.by,
                depth,
                outcome = %match &other {
                    Ok((o, t)) => format!("{o:?}, model: {}", t.is_some()),
                    Err(e) => format!("{e:?}"),
                },
                "witness: the carried path is not established shortest and native BMC did \
                 not re-derive one within its depth; the carried path ships"
            );
            shipped
        }
    }
}

fn search_witness_with(
    file: &Btor2File,
    max_k: u32,
    deadline: Option<std::time::Instant>,
    skip: &[(&'static str, String)],
    owned_only: bool,
    run_carriers: bool,
) -> ShippedWitness {
    use crate::adapter::reach_portfolio::{ReachMembers, decide_reach_portfolio_with_members};
    use crate::adapter::run_budget::{Budget, current, enter};
    let none =
        |reason: String, within: Option<u32>, contradicted_by: Vec<&'static str>| ShippedWitness {
            trace: None,
            by: None,
            shortest: None,
            unavailable_within_k: within,
            unavailable_reason: Some(reason),
            contradicted_by,
        };
    // What the verdict's run already said about traces: the members whose witness failed to
    // replay are not run again (mununu#1045 review, point 2).
    let mut notes: Vec<String> = skip
        .iter()
        .map(|(m, why)| format!("{m}: its witness did not replay ({why}); not run again"))
        .collect();
    // The time left: the caller's deadline, inside whatever budget this thread already runs
    // under (a child never outlives its parent, so a `verify-auto` per-property budget still
    // bounds the search when `deadline` is `None`).
    let own_ms = deadline.map(|d| {
        u64::try_from(
            d.saturating_duration_since(std::time::Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    });
    let budget = Budget::child(&current(), own_ms);
    let left = budget.remaining_ms().map(std::time::Duration::from_millis);
    if budget.expired() || left.is_some_and(|l| l.is_zero()) {
        notes.push("the time budget was spent by the verdict; no witness search ran".into());
        return none(notes.join("; "), None, Vec::new());
    }
    let _guard = enter(&budget);
    // The fallback portfolio: every trace carrier whose witness has not already failed — owned
    // ones only under `--owned-only`. Only behind a `Reachable` verdict: there is nothing to
    // explain otherwise, and native BMC alone is the bounded re-derivation.
    let mut members = skip
        .iter()
        .fold(ReachMembers::TRACE_CARRIERS, |m, (failed, _)| {
            m.without(failed)
        });
    if owned_only {
        members = members
            .without("btormc")
            .without("ric3")
            .without("ric3-bmc");
    }
    // mununu#1076 — the members of the fallback that proved `bad` unreachable: carried on every
    // way out of here, with or without a trace.
    let mut contradicted: Vec<&'static str> = Vec::new();
    if run_carriers && !members.names().is_empty() {
        // The subprocess wall: the time left, else the members' default.
        let wall = left.unwrap_or(crate::adapter::btormc::DEFAULT_TIMEOUT);
        let again = decide_reach_portfolio_with_members(file, wall, None, members);
        let (shipped, named) = fallback_view(again, &members, &mut notes);
        contradicted = named;
        if let Some(shipped) = shipped {
            return shipped;
        }
    }
    // Last: native BMC from depth 0, the shallowest path when it reaches the depth.
    match bmc_bad_reachable_witness(file, max_k) {
        Ok((BmcOutcome::Violated { .. }, Some(t))) => ShippedWitness {
            trace: Some(t),
            by: Some("native-bmc"),
            // One query per depth from 0: every shorter depth was refuted.
            shortest: Some(true),
            unavailable_within_k: None,
            unavailable_reason: None,
            contradicted_by: contradicted,
        },
        Ok((BmcOutcome::Violated { depth }, None)) => {
            notes.push(format!(
                "native BMC reached `bad` at depth {depth} but the solver returned no model"
            ));
            none(notes.join("; "), None, contradicted)
        }
        Ok((BmcOutcome::NoCexWithin { k }, _)) => {
            notes.push(format!(
                "native BMC found no path to `bad` within {k} cycles; the counterexample the \
                 verdict rests on is deeper (raise --witness-max-k)"
            ));
            none(notes.join("; "), Some(k), contradicted)
        }
        Err(e) => {
            notes.push(format!("native BMC stopped: {e}"));
            none(notes.join("; "), None, contradicted)
        }
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

    /// mununu#1143 — the simulator's width refusal is told from every other replay error by
    /// its own text, so IC3 ships it as "unavailable" and nothing else.
    #[test]
    fn x1143_only_the_width_refusal_reads_as_unavailable() {
        assert!(is_width_refusal(&format!(
            "node 3 is 130 bits wide; {WIDTH_REFUSAL}"
        )));
        assert!(!is_width_refusal("state index 9 out of range"));
        assert!(!is_width_refusal(
            "node 3 is not a bit-vector — an array state has no replay"
        ));
    }

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

    /// N5.4 — the prefix mode ends the trace at the first frame that fires `bad`, whatever the
    /// witness goes on to say; the default mode requires the LAST frame to fire it.
    #[test]
    fn the_prefix_mode_ends_at_the_first_firing_frame() {
        let file = parser::parse(DESIGN).unwrap();
        // `bad = s ∧ i` fires at frame 1 (s = 1, i = 1); frame 2 has i = 0.
        let w = parse_btor2_witness("sat\nb0\n#0\n0 0\n1 0000\n@0\n0 1\n@1\n0 1\n@2\n0 0\n.\n")
            .unwrap();
        let err = trace_from_witness(&file, &w).unwrap_err();
        assert!(
            err.contains("last frame does not make bad #0 true"),
            "{err}"
        );
        let t = trace_from_witness_prefix(&file, &w).expect("fires at frame 1");
        assert_eq!(t.states.len(), 2);
        assert_eq!(t.inputs.len(), 2);
        assert_eq!(t.states[1][1], ("s".to_string(), CexValue::from_u64(1, 1)));
        // No frame fires: refused with the range it checked.
        let quiet = parse_btor2_witness("sat\nb0\n#0\n0 0\n1 0000\n@0\n0 0\n@1\n0 0\n.\n").unwrap();
        let err = trace_from_witness_prefix(&file, &quiet).unwrap_err();
        assert!(err.contains("no replayed frame (0..=1)"), "{err}");
    }

    /// N5.4 — `drop_states_from` removes the appended states' lines from `#0` and every `#k`
    /// block, and nothing else.
    #[test]
    fn dropping_appended_states_keeps_the_designs_lines() {
        let mut w = parse_btor2_witness(
            "sat\nb0\n#0\n0 0\n1 0000\n2 0\n@0\n0 1\n#1\n1 0001\n2 1\n@1\n0 1\n.\n",
        )
        .unwrap();
        w.drop_states_from(2);
        assert_eq!(w.init, vec![(0, "0".to_string()), (1, "0000".to_string())]);
        assert_eq!(w.states, vec![(1, vec![(1, "0001".to_string())])]);
        assert_eq!(w.frames.len(), 2);
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
        let w = witness_for_violation(
            &file,
            3000,
            Some(t0 + Duration::from_millis(1)),
            &ReachOutcome::default(),
            false,
        );
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
        let full = witness_for_violation(&file, 3000, None, &ReachOutcome::default(), false);
        assert_eq!(full.by, Some("native-bmc"));
        assert_eq!(full.trace.map(|t| t.states.len()), Some(3001));
        assert_eq!(full.unavailable_reason, None);
    }

    /// The depth bound itself searched without a path: `unavailable_within_k` names it.
    #[test]
    fn a_bound_searched_without_a_path_names_the_bound() {
        let file = parser::parse(DEEP).unwrap();
        let w = witness_for_violation(&file, 5, None, &ReachOutcome::default(), false);
        assert!(w.trace.is_none());
        assert_eq!(w.unavailable_within_k, Some(5));
        let reason = w.unavailable_reason.unwrap();
        assert!(reason.contains("within 5 cycles"), "{reason}");
    }

    #[test]
    fn a_budget_spent_by_the_verdict_runs_no_search() {
        let file = parser::parse(DEEP).unwrap();
        let w = witness_for_violation(
            &file,
            3000,
            Some(std::time::Instant::now()),
            &ReachOutcome::default(),
            false,
        );
        assert!(w.trace.is_none());
        assert_eq!(w.unavailable_within_k, None);
        assert!(w.unavailable_reason.unwrap().contains("spent"));
    }

    /// N5.4 — a trace the portfolio carried out ships as it is: no search runs, and `by` names
    /// the member.
    #[test]
    fn a_carried_trace_ships_without_a_search() {
        use crate::adapter::reach_portfolio::{CarriedTrace, ReachVerdict};
        let file = parser::parse(DEEP).unwrap();
        let carried = BmcTrace {
            states: vec![vec![], vec![]],
            inputs: vec![vec![], vec![]],
        };
        let outcome = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["sat-bmc"],
            trace: Some(CarriedTrace {
                by: "sat-bmc",
                trace: carried.clone(),
                shortest: Some(false),
            }),
            ..ReachOutcome::default()
        };
        // An already-spent deadline would refuse any search; the carried trace needs none.
        let w = witness_for_violation(&file, 1, Some(std::time::Instant::now()), &outcome, false);
        assert_eq!(w.by, Some("sat-bmc"));
        assert_eq!(w.trace, Some(carried));
        assert_eq!(w.unavailable_reason, None);
    }

    /// mununu#1057 — a shipped path its carrier did not establish as the shortest is re-derived
    /// by native BMC bounded by its depth; one the carrier did establish, one with no trace, and
    /// one whose depth native BMC cannot reach ship as they are. Two counters on one input:
    /// `a` reaches 3 after three steps of `i` high (a 3-step path), `b` reaches 6 after six
    /// steps of `i` low — the design on which `sat-bmc`'s window answers with either path.
    #[test]
    fn a_path_not_established_shortest_is_rederived_bounded_by_its_depth() {
        let two_counters = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 a\n5 init 1 4 3\n\
                            6 state 1 b\n7 init 1 6 3\n8 input 2 i\n9 one 1\n10 add 1 4 9\n\
                            11 add 1 6 9\n12 ite 1 8 10 4\n13 next 1 4 12\n14 ite 1 8 6 11\n\
                            15 next 1 6 14\n16 constd 1 3\n17 eq 2 4 16\n18 constd 1 6\n\
                            19 eq 2 6 18\n20 or 2 17 19\n21 bad 20\n";
        let file = parser::parse(two_counters).unwrap();
        // A 6-step path (frames 0..=6) the carrier did not establish as the shortest.
        let six_steps = BmcTrace {
            states: vec![vec![]; 7],
            inputs: vec![vec![]; 7],
        };
        let shipped = |trace: Option<BmcTrace>, shortest: Option<bool>| ShippedWitness {
            by: trace.as_ref().map(|_| "sat-bmc"),
            trace,
            shortest,
            unavailable_within_k: None,
            unavailable_reason: None,
            contradicted_by: Vec::new(),
        };
        let w = shortest_path_for(&file, shipped(Some(six_steps.clone()), Some(false)));
        assert_eq!((w.by, w.shortest), (Some("native-bmc"), Some(true)));
        let t = w.trace.expect("the re-derived path");
        assert_eq!(t.states.len(), 4, "{t:?}");
        // The same without a claim either way (`ic3`'s trace) is re-derived too.
        let w = shortest_path_for(&file, shipped(Some(six_steps.clone()), None));
        assert_eq!((w.by, w.shortest), (Some("native-bmc"), Some(true)));
        // Established shortest: ships as it is, no search.
        let w = shortest_path_for(&file, shipped(Some(six_steps.clone()), Some(true)));
        assert_eq!((w.by, w.shortest), (Some("sat-bmc"), Some(true)));
        assert_eq!(w.trace, Some(six_steps));
        // No trace: unchanged.
        let w = shortest_path_for(&file, shipped(None, None));
        assert_eq!((w.trace, w.by), (None, None));
        // A depth native BMC finds nothing within (a 1-step "path" on a design whose first
        // violation is at depth 3): the carried path ships, unchanged.
        let one_step = BmcTrace {
            states: vec![vec![]; 2],
            inputs: vec![vec![]; 2],
        };
        let w = shortest_path_for(&file, shipped(Some(one_step.clone()), Some(false)));
        assert_eq!((w.by, w.shortest), (Some("sat-bmc"), Some(false)));
        assert_eq!(w.trace, Some(one_step));
    }

    /// REPRODUCER (mununu#1076) — the fallback names the members that proved `bad` unreachable
    /// under the `violated` verdict it searches for, whether or not another member carried a
    /// trace beside them: with a trace the alarm used to be a log line only (the JSON carried no
    /// note at all); without one it was a note on a row still shipped `violated`.
    #[test]
    fn the_fallback_names_the_members_that_proved_bad_unreachable() {
        use crate::adapter::reach_portfolio::{CarriedTrace, ReachMembers, ReachVerdict};
        let t = BmcTrace {
            states: vec![vec![], vec![]],
            inputs: vec![vec![], vec![]],
        };
        let members = ReachMembers::TRACE_CARRIERS;
        // A trace from `sat-bmc` beside a proof of unreachability from `ic3`.
        let again = ReachOutcome {
            verdict: ReachVerdict::Contradiction,
            reachable_by: vec!["sat-bmc"],
            unreachable_by: vec!["ic3"],
            trace: Some(CarriedTrace {
                by: "sat-bmc",
                trace: t.clone(),
                shortest: Some(true),
            }),
            ..ReachOutcome::default()
        };
        let mut notes = Vec::new();
        let (shipped, named) = fallback_view(again, &members, &mut notes);
        assert_eq!(named, vec!["ic3"]);
        let shipped = shipped.expect("the trace still ships, marked");
        assert_eq!(
            (shipped.by, shipped.trace),
            (Some("sat-bmc"), Some(t.clone()))
        );
        assert_eq!(shipped.contradicted_by, vec!["ic3"]);
        // No trace: the alarm is named too, beside the other members' misses.
        let again = ReachOutcome {
            verdict: ReachVerdict::Unreachable,
            unreachable_by: vec!["ric3"],
            abstained: vec![("sat-bmc", "stopped".into())],
            ..ReachOutcome::default()
        };
        let mut notes = Vec::new();
        let (shipped, named) = fallback_view(again, &members, &mut notes);
        assert!(shipped.is_none());
        assert_eq!(named, vec!["ric3"]);
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("ric3: proved `bad` unreachable — CONTRADICTION")),
            "{notes:?}"
        );
        assert!(
            notes.iter().any(|n| n == "sat-bmc: stopped (stopped)"),
            "{notes:?}"
        );
        // The report's rule: the verdict becomes the contradiction, both sides named.
        let mut outcome = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["exact"],
            ..ReachOutcome::default()
        };
        let w = apply_fallback_contradiction(&mut outcome, shipped_with(vec!["ric3"], Some(t)));
        assert_eq!(outcome.verdict, ReachVerdict::Contradiction);
        assert_eq!(outcome.unreachable_by, vec!["ric3"]);
        assert_eq!(outcome.reachable_by, vec!["exact"]);
        // The path found beside the proof does not ship; the reason names the alarm.
        assert_eq!((w.trace, w.by), (None, None));
        assert!(
            w.unavailable_reason
                .as_deref()
                .is_some_and(|r| r.starts_with("contradiction: ric3 proved `bad` unreachable")),
            "{:?}",
            w.unavailable_reason
        );
        assert_eq!(w.contradicted_by, vec!["ric3"]);
        // And a no-op without one.
        let mut outcome = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["exact"],
            ..ReachOutcome::default()
        };
        let w = apply_fallback_contradiction(&mut outcome, shipped_with(Vec::new(), None));
        assert_eq!(outcome.verdict, ReachVerdict::Reachable);
        assert_eq!(w.unavailable_reason.as_deref(), Some("x"));
    }

    fn shipped_with(contradicted_by: Vec<&'static str>, trace: Option<BmcTrace>) -> ShippedWitness {
        ShippedWitness {
            by: trace.as_ref().map(|_| "sat-bmc"),
            trace,
            shortest: None,
            unavailable_within_k: None,
            unavailable_reason: Some("x".into()),
            contradicted_by,
        }
    }

    /// N5.4 — a member whose own witness failed to replay is not run again by the fallback, and
    /// the reason says so beside the fallback's own miss.
    #[test]
    fn a_failed_carry_is_not_retried_and_is_named() {
        use crate::adapter::reach_portfolio::ReachVerdict;
        let file = parser::parse(DEEP).unwrap();
        let outcome = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["sat-bmc"],
            trace_replay_failed: vec![("sat-bmc", "a `constraint` fails at frame 1".to_string())],
            ..ReachOutcome::default()
        };
        // Owned-only with `sat-bmc` struck out leaves `ic3` as the only carrier (the fallback set
        // is explicit, TRACE_CARRIERS); on the 3000-deep counter it runs to the deadline — 2 s
        // here, not the 60 s default wall it used to spend (#1049 review) — and its stop is
        // named beside the strike-out; the native re-derivation then has no time left, or
        // searches its bound of 5: either way no trace, and the reason says what each did.
        let t0 = std::time::Instant::now();
        let w = witness_for_violation(&file, 5, Some(t0 + Duration::from_secs(2)), &outcome, true);
        let elapsed = t0.elapsed();
        assert!(w.trace.is_none(), "{w:?}");
        let reason = w.unavailable_reason.unwrap();
        assert!(
            reason.contains("sat-bmc: its witness did not replay")
                && reason.contains("not run again"),
            "{reason}"
        );
        assert!(reason.contains("ic3:"), "{reason}");
        assert!(
            reason.contains("within 5 cycles") || reason.contains("budget"),
            "{reason}"
        );
        assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    }

    /// The witness search runs INSIDE the budget its thread already has (a `verify-auto`
    /// per-property budget, a rescue slice): with no `deadline` of its own it used to install an
    /// unbounded budget over it, and so ran the 3000-deep search to the end under a 1 ms outer
    /// budget.
    #[test]
    fn an_outer_run_budget_bounds_the_witness_search() {
        use crate::adapter::run_budget::{Budget, enter};
        let file = parser::parse(DEEP).unwrap();
        let outer = Budget::with_ms(1);
        let _guard = enter(&outer);
        std::thread::sleep(Duration::from_millis(5));
        let w = witness_for_violation(&file, 3000, None, &ReachOutcome::default(), false);
        assert!(w.trace.is_none(), "the outer budget was spent: {:?}", w.by);
        let reason = w.unavailable_reason.unwrap();
        assert!(reason.contains("budget"), "{reason}");
    }
}
