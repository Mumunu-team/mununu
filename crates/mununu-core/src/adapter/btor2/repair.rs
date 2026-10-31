//! Repair candidates for a counterexample (2026-10-30, `sv verify-auto --localize`).
//!
//! The question, with the counterexample's INPUTS fixed: which register's next-state function, if
//! it were free to take any value at the trace's cycles, would let the property survive the whole
//! trace? Each such register is a **repair candidate** — the place a one-register change could
//! remove this counterexample — reported as a candidate, never as a cause. This is SAT-based design
//! debugging (Smith, Veneris et al., TCAD 2005) and BugAssist (Jose & Majumdar, PLDI 2011) at
//! register granularity, bounded to the trace's length.
//!
//! **Model, per candidate `r`.** The design with the row's violation as a `bad` (the safety
//! witness monitor the path itself came from, [`crate::adapter::reach_rescue::safety_bad_model`])
//! with [`super::fault::inject_faults`]'s override mux on `r`'s `next` (`en` / `val` as free
//! inputs, a budget, no warm-up), a cycle counter, one `constraint (cycle != t) || (in == v_t)` per
//! design input and trace frame — the inputs replay the trace, the override inputs stay free — and
//! the `bad` lines folded into a `seen` latch; the one `bad` of the query is `cycle == k && !seen
//! && !bad_now`, `k` the trace's last frame — where the monitor's `bad` fired (its depth only gates
//! the first frames after reset): reaching it means a value sequence for `r` exists under which
//! the trace's inputs no longer violate the property. The query ends where the trace ends: a frame
//! beyond it would have free inputs, and a violation THEY cause is not this one.
//!
//! **Which repair is reported.** Budgets climb — one frame's change, two, three, unbounded — and
//! for one frame the frames are tried from the last backwards, so the report names the LATEST
//! single-frame change that still avoids the violation (deterministic, and the most telling: a
//! register upstream can repair by steering the design elsewhere early, which is a correct answer
//! and not where the bug is). A larger budget keeps the solver's choice of frames.
//!
//! **engine:** native BMC (in-process Z3, QF_BV; [`super::native_bmc::bmc_bad_reachable_witness`]),
//! depth = the trace's last frame, one query per candidate register, override budget and (for one
//! frame) frame; role: the decider of the repair-existence query. The Z3 model's `en` / `val` per
//! frame name WHEN the repair acts and WHAT it writes.
//!
//! A register the property reads directly is always a candidate (overriding what is measured
//! "repairs" anything); the caller reports those in their own bucket. Display only: no verdict is
//! read or changed.

use super::ast::{Btor2File, ConstValue, Line, Nid, Node, Op, Operand, Sort};
use super::cex_value::CexValue;
use super::fault::{FaultBound, FaultSite, FaultSpec, inject_faults, rebuild};
use super::native_bmc::{BmcOutcome, bmc_bad_reachable_witness};
use std::collections::HashMap;

/// Symbol of the query's cycle counter.
pub const CYCLE_SYMBOL: &str = "mununu_repair_cycle";
/// Symbol of the latch that remembers a `bad` of the design already fired.
pub const SEEN_SYMBOL: &str = "mununu_repair_bad_seen";
/// The most candidate registers one row queries; beyond it the localisation is `partial`.
pub const MAX_CANDIDATES: usize = 64;
/// The override budgets tried after the single-frame search, in order; unbounded last.
pub const WIDER_BUDGETS: [FaultBound; 3] = [
    FaultBound::AtMost(2),
    FaultBound::AtMost(3),
    FaultBound::Unbounded,
];

/// A repair query for one site.
#[derive(Debug, Clone)]
pub struct RepairQuery {
    pub model: Btor2File,
    /// The override's enable input (one bit per frame in the BMC witness).
    pub en_input: String,
    /// The override's value input.
    pub val_input: String,
    /// The depth the query runs at: the trace's last frame.
    pub k: u32,
}

/// Build the repair query for `site` from `design` (the model with the row's violation as a
/// `bad`), the counterexample's input frames (`inputs[t]` drives the transition out of frame `t`;
/// the last frame is where the `bad` fired), the override budget (how many frames the override may
/// act in) and, optionally, the one frame it may act at.
pub fn repair_query(
    design: &Btor2File,
    inputs: &[Vec<(String, CexValue)>],
    site: &str,
    bound: FaultBound,
    at_frame: Option<u32>,
) -> Result<RepairQuery, String> {
    let k = u32::try_from(
        inputs
            .len()
            .checked_sub(1)
            .ok_or("the trace has no frame")?,
    )
    .map_err(|_| "the trace is too long for a bounded query".to_string())?;
    let spec = FaultSpec {
        sites: vec![FaultSite {
            register: site.to_string(),
        }],
        bound,
        warmup: 0,
    };
    let (model, injected) = inject_faults(design, &spec)?;
    let inj = injected.first().ok_or("no site was injected")?;
    let mut lines: Vec<Line> = model.lines.clone();
    let mut next_nid: Nid = lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut fresh = |node: Node, lines: &mut Vec<Line>| -> Nid {
        let n = next_nid;
        next_nid += 1;
        lines.push(Line {
            nid: n,
            node,
            immediates: Vec::new(),
            source_line: 0,
        });
        n
    };
    let op = |sort: Nid, op: Op, args: Vec<Operand>| Node::Op {
        sort,
        op,
        args,
        symbol: None,
    };
    let pos = |nids: &[Nid]| nids.iter().map(|n| Operand(*n)).collect::<Vec<_>>();
    let bool_sort = lines
        .iter()
        .find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width: 1 },
            } => Some(l.nid),
            _ => None,
        })
        .unwrap_or_else(|| {
            fresh(
                Node::Sort {
                    sort: Sort::BitVec { width: 1 },
                },
                &mut lines,
            )
        });
    // The cycle counter: wide enough to hold k + 1 without wrapping.
    let w = (32 - (k + 2).leading_zeros()).max(1);
    let cyc_sort = fresh(
        Node::Sort {
            sort: Sort::BitVec { width: w },
        },
        &mut lines,
    );
    let c0 = fresh(
        Node::Const {
            sort: cyc_sort,
            value: ConstValue::Zero,
        },
        &mut lines,
    );
    let c1 = fresh(
        Node::Const {
            sort: cyc_sort,
            value: ConstValue::One,
        },
        &mut lines,
    );
    let cycle = fresh(
        Node::State {
            sort: cyc_sort,
            symbol: Some(CYCLE_SYMBOL.into()),
        },
        &mut lines,
    );
    fresh(
        Node::Init {
            sort: cyc_sort,
            state: cycle,
            value: Operand(c0),
        },
        &mut lines,
    );
    let inc = fresh(op(cyc_sort, Op::Add, pos(&[cycle, c1])), &mut lines);
    fresh(
        Node::Next {
            sort: cyc_sort,
            state: cycle,
            value: Operand(inc),
        },
        &mut lines,
    );
    // The inputs by symbol, with their sort.
    let input_by_name: HashMap<String, (Nid, Nid)> = lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Input {
                sort,
                symbol: Some(s),
            } => Some((s.clone(), (l.nid, *sort))),
            _ => None,
        })
        .collect();
    let mut pinned = 0usize;
    let mut pins: Vec<Node> = Vec::new();
    for (t, frame) in inputs.iter().enumerate() {
        let ct = fresh(
            Node::Const {
                sort: cyc_sort,
                value: ConstValue::Dec(t as i128),
            },
            &mut lines,
        );
        let at_t = fresh(op(bool_sort, Op::Eq, pos(&[cycle, ct])), &mut lines);
        let not_at_t = fresh(op(bool_sort, Op::Not, pos(&[at_t])), &mut lines);
        for (name, value) in frame {
            if name.starts_with("mununu_") {
                continue; // the override's own inputs stay free
            }
            let Some((nid, sort)) = input_by_name.get(name.as_str()).copied() else {
                continue;
            };
            let cv = fresh(
                Node::Const {
                    sort,
                    value: ConstValue::Hex(value.to_hex().trim_start_matches("0x").to_string()),
                },
                &mut lines,
            );
            let eq = fresh(op(bool_sort, Op::Eq, pos(&[nid, cv])), &mut lines);
            let ok = fresh(op(bool_sort, Op::Or, pos(&[not_at_t, eq])), &mut lines);
            pins.push(Node::Constraint {
                signal: Operand(ok),
            });
            pinned += 1;
        }
    }
    if pinned == 0 {
        return Err("the trace names no input of the model: nothing to fix".to_string());
    }
    // The one frame the override may act at: `en` only when `cycle == t0`.
    if let Some(t0) = at_frame {
        let (en, _) = input_by_name
            .get(inj.en_input.as_str())
            .copied()
            .ok_or("the override's enable input is missing")?;
        let ct = fresh(
            Node::Const {
                sort: cyc_sort,
                value: ConstValue::Dec(t0 as i128),
            },
            &mut lines,
        );
        let at = fresh(op(bool_sort, Op::Eq, pos(&[cycle, ct])), &mut lines);
        let not_en = fresh(op(bool_sort, Op::Not, pos(&[en])), &mut lines);
        let only = fresh(op(bool_sort, Op::Or, pos(&[at, not_en])), &mut lines);
        pins.push(Node::Constraint {
            signal: Operand(only),
        });
    }
    for p in pins {
        fresh(p, &mut lines);
    }
    // The design's `bad`s become a seen-latch; the query's bad is "survived to k".
    let bads: Vec<Operand> = lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Bad { signal } => Some(*signal),
            _ => None,
        })
        .collect();
    if bads.is_empty() {
        return Err("the model has no `bad`: nothing to repair".to_string());
    }
    lines.retain(|l| !matches!(l.node, Node::Bad { .. }));
    let mut any = bads[0];
    for b in &bads[1..] {
        let n = fresh(op(bool_sort, Op::Or, vec![any, *b]), &mut lines);
        any = Operand(n);
    }
    let seen = fresh(
        Node::State {
            sort: bool_sort,
            symbol: Some(SEEN_SYMBOL.into()),
        },
        &mut lines,
    );
    let zero = fresh(
        Node::Const {
            sort: bool_sort,
            value: ConstValue::Zero,
        },
        &mut lines,
    );
    fresh(
        Node::Init {
            sort: bool_sort,
            state: seen,
            value: Operand(zero),
        },
        &mut lines,
    );
    let seen_next = fresh(op(bool_sort, Op::Or, vec![Operand(seen), any]), &mut lines);
    fresh(
        Node::Next {
            sort: bool_sort,
            state: seen,
            value: Operand(seen_next),
        },
        &mut lines,
    );
    let ck = fresh(
        Node::Const {
            sort: cyc_sort,
            value: ConstValue::Dec(k as i128),
        },
        &mut lines,
    );
    let at_k = fresh(op(bool_sort, Op::Eq, pos(&[cycle, ck])), &mut lines);
    let not_seen = fresh(op(bool_sort, Op::Not, pos(&[seen])), &mut lines);
    let not_any = fresh(op(bool_sort, Op::Not, vec![any]), &mut lines);
    let a = fresh(op(bool_sort, Op::And, pos(&[at_k, not_seen])), &mut lines);
    let survived = fresh(op(bool_sort, Op::And, pos(&[a, not_any])), &mut lines);
    fresh(
        Node::Bad {
            signal: Operand(survived),
        },
        &mut lines,
    );
    Ok(RepairQuery {
        model: rebuild(lines),
        en_input: inj.en_input.clone(),
        val_input: inj.val_input.clone(),
        k,
    })
}

/// One register a repair query found: when the override acts and what it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairFound {
    pub register: String,
    pub cycles: Vec<usize>,
    /// Hex, one per cycle.
    pub values: Vec<String>,
    /// The smallest override budget that repairs (1 = one frame's change suffices, at the latest
    /// frame where it does); `None` when only an unbounded override does.
    pub budget: Option<u32>,
}

/// The localisation of one counterexample over its candidate registers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Localization {
    pub found: Vec<RepairFound>,
    /// Registers queried (≤ [`MAX_CANDIDATES`]).
    pub queried: usize,
    /// Candidates beyond the cap, not queried.
    pub skipped_by_cap: usize,
    /// Candidates whose query could not be built or run, with the reason.
    pub errors: Vec<(String, String)>,
}

/// The override's frames and values in a BMC witness.
fn overrides(trace: &super::native_bmc::BmcTrace, q: &RepairQuery) -> (Vec<usize>, Vec<String>) {
    let mut cycles = Vec::new();
    let mut values = Vec::new();
    for (t, frame) in trace.inputs.iter().enumerate() {
        let en = frame
            .iter()
            .find(|(n, _)| *n == q.en_input)
            .and_then(|(_, v)| v.as_u64());
        if en == Some(1) {
            cycles.push(t);
            values.push(
                frame
                    .iter()
                    .find(|(n, _)| *n == q.val_input)
                    .map(|(_, v)| v.to_hex())
                    .unwrap_or_else(|| "?".to_string()),
            );
        }
    }
    (cycles, values)
}

/// Run the repair queries for each of `candidates` (registers, by the model's names), in order,
/// up to [`MAX_CANDIDATES`]: one frame's change at the latest frame that repairs, else the wider
/// budgets of [`WIDER_BUDGETS`].
pub fn localize(
    design: &Btor2File,
    inputs: &[Vec<(String, CexValue)>],
    candidates: &[String],
) -> Localization {
    let mut out = Localization::default();
    let Some(k) = inputs.len().checked_sub(1) else {
        return out;
    };
    for (i, reg) in candidates.iter().enumerate() {
        if i >= MAX_CANDIDATES {
            out.skipped_by_cap = candidates.len() - MAX_CANDIDATES;
            break;
        }
        out.queried += 1;
        // Every (budget, frame) attempt in order; the first that repairs is the report.
        let mut attempts: Vec<(FaultBound, Option<u32>)> = (0..=k)
            .rev()
            .map(|t| (FaultBound::AtMost(1), Some(t as u32)))
            .collect();
        attempts.extend(WIDER_BUDGETS.iter().map(|b| (*b, None)));
        let mut error: Option<String> = None;
        for (bound, at) in attempts {
            let q = match repair_query(design, inputs, reg, bound, at) {
                Ok(q) => q,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            };
            match bmc_bad_reachable_witness(&q.model, q.k) {
                Ok((BmcOutcome::Violated { .. }, trace)) => {
                    let (cycles, values) =
                        trace.as_ref().map(|t| overrides(t, &q)).unwrap_or_default();
                    out.found.push(RepairFound {
                        register: reg.clone(),
                        cycles,
                        values,
                        budget: bound.budget(),
                    });
                    break;
                }
                Ok((BmcOutcome::NoCexWithin { .. }, _)) => continue,
                Err(e) => {
                    error = Some(format!("{e:?}"));
                    break;
                }
            }
        }
        if let Some(e) = error {
            out.errors.push((reg.clone(), e));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slice fixture's engine model (`lane_arbiter.sv`, `MUNUNU_SHADOW_BTOR2_DUMP` of the
    /// 0.5.8 release binary): the design's registers, no `bad` of its own.
    const SHADOW_BTOR2: &str = include_str!("testdata/lane_arbiter.shadow.btor");

    /// The `alternates` row's mu-calculus (`ltl G (start_a -> X (!start_a W start_b))`), as the
    /// 0.5.8 transcript shows it translated.
    const ALTERNATES: &str = "(nu G1. (((!(start_a)) || ([] (nu W2. ((start_b || (!(start_a))) && \
                              (start_b || ([] W2)))))) && [] G1))";

    /// The measured counterexample's inputs: request, release, request — lane A granted twice.
    fn inputs() -> Vec<Vec<(String, CexValue)>> {
        let f = |ack: u64, rel: u64, req: u64| {
            vec![
                ("ack".to_string(), CexValue::from_u64(ack, 1)),
                ("clk".to_string(), CexValue::from_u64(0, 1)),
                ("rel".to_string(), CexValue::from_u64(rel, 1)),
                ("req".to_string(), CexValue::from_u64(req, 1)),
            ]
        };
        vec![f(0, 1, 1), f(0, 1, 1), f(0, 0, 1), f(1, 1, 1)]
    }

    /// The engine's model with the row's violation as a `bad`: the same monitor the safety
    /// witness pass searches.
    fn design() -> Btor2File {
        let f = crate::mu_calculus::parser::parse(ALTERNATES).expect("the formula parses");
        let (text, _depth) = crate::adapter::reach_rescue::safety_bad_model(SHADOW_BTOR2, &f, true)
            .expect("the monitor builds");
        super::super::parser::parse(&text).expect("the monitored model parses")
    }

    /// The design alone (no `bad`), for the refusal test.
    fn bare_design() -> Btor2File {
        super::super::parser::parse(SHADOW_BTOR2).expect("the fixture parses")
    }

    /// The pinned frames replay the trace: whatever the override does, the design inputs of
    /// frames 0..=3 in the query's witness are the counterexample's. (Measured 2026-10-30: the
    /// monitor alone violates at depth 3, the trace's last frame, where the query ends.)
    #[test]
    fn the_pinned_inputs_replay_the_trace() {
        let d = design();
        let (o, t) = bmc_bad_reachable_witness(&d, 6).expect("bmc runs");
        assert_eq!(
            o,
            BmcOutcome::Violated { depth: 3 },
            "the monitored model violates at 3"
        );
        assert!(t.is_some());
        let q = repair_query(&d, &inputs(), "state_q", FaultBound::Unbounded, None)
            .expect("query builds");
        assert_eq!(q.k, 3);
        let (o, t) = bmc_bad_reachable_witness(&q.model, q.k).expect("bmc runs");
        assert!(matches!(o, BmcOutcome::Violated { .. }), "{o:?}");
        let t = t.expect("a witness");
        for (j, frame) in inputs().iter().enumerate() {
            for (name, v) in frame {
                if name == "clk" {
                    continue;
                }
                let got = t.inputs[j]
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, w)| w.as_u64())
                    .unwrap_or_else(|| panic!("input {name} at frame {j}"));
                assert_eq!(got, v.as_u64(), "frame {j}: {name} is pinned to the trace");
            }
        }
    }

    /// Which registers repair the alternation counterexample, and where their change acts.
    #[test]
    fn the_candidates_and_the_latest_frame_each_can_act_at() {
        let d = design();
        let loc = localize(
            &d,
            &inputs(),
            &[
                "stall_q".into(),
                "state_q".into(),
                "start_a".into(),
                "start_b".into(),
            ],
        );
        assert!(loc.errors.is_empty(), "{:?}", loc.errors);
        assert_eq!(loc.queried, 4);
        assert_eq!(loc.skipped_by_cap, 0);
        let by = |r: &str| {
            loc.found
                .iter()
                .find(|f| f.register == r)
                .unwrap_or_else(|| panic!("{r} among {:?}", loc.found))
        };
        // `state_q`: one frame's change suffices. The latest frame it can act at is 1 — the value
        // entering frame 2 — because the `start_a` pulse at frame 3 is computed from the
        // combinational `next` at frame 2 (not from the register), so overriding what the
        // register takes at frame 3 cannot stop it; from a frame-2 state other than IDLE, `next`
        // is not LANE_A and the pulse does not come.
        let st = by("state_q");
        assert_eq!(st.budget, Some(1), "{st:?}");
        assert_eq!(st.cycles, vec![1], "{st:?}");
        assert_ne!(st.values[0], "0x1", "any state but LANE_A: {:?}", st.values);
        // `stall_q` repairs too, by steering the design elsewhere (measured 2026-10-30): forced to
        // STALL_LIMIT - 1 at reset, the fault arm fires on the first stalled cycle, the arbiter
        // parks in FAULT and the second grant never happens — a correct answer to the question
        // asked, and a register UPSTREAM of the bug, which is why the caller ranks candidates by
        // how close to the violation their change acts: `state_q` (frame 1) before `stall_q`
        // (frame 0).
        let stall = by("stall_q");
        assert_eq!(stall.budget, Some(1), "{stall:?}");
        assert_eq!(stall.cycles, vec![0], "{stall:?}");
        assert_eq!(stall.values, vec!["0x4".to_string()], "{stall:?}");
        // The pulses the property reads are candidates too — overriding what is measured
        // "repairs" anything; the caller buckets them.
        assert_eq!(by("start_a").budget, Some(1), "{:?}", by("start_a"));
        assert_eq!(by("start_b").budget, Some(1), "{:?}", by("start_b"));
    }

    #[test]
    fn a_site_that_is_not_a_register_and_a_model_without_bad_are_refused() {
        let d = design();
        let e = repair_query(
            &d,
            &inputs(),
            "no_such_register",
            FaultBound::Unbounded,
            None,
        )
        .unwrap_err();
        assert!(e.contains("not a register"), "{e}");
        let e = repair_query(
            &bare_design(),
            &inputs(),
            "state_q",
            FaultBound::Unbounded,
            None,
        )
        .unwrap_err();
        assert!(e.contains("no `bad`"), "{e}");
        let e = repair_query(&d, &[], "state_q", FaultBound::Unbounded, None).unwrap_err();
        assert!(e.contains("no frame"), "{e}");
    }
}
