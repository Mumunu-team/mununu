//! The one counterexample shape of 0.5 ([`Trace`]), shared by the CLI and the HTTP API and by
//! every verb that returns a path. Ungated (the CLI and the core diagnostics serialize it too);
//! `crate::api::models` re-exports it.

use serde::{Deserialize, Serialize};

/// A generated counterexample-replay testbench ([`crate::adapter::verilator::replay`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct TestbenchView {
    /// `tb.cpp`.
    pub tb_cpp: String,
    /// `Makefile`; it names the sources as the request named them.
    pub makefile: String,
    /// (cycle, register) comparisons the testbench makes.
    pub checks: usize,
    /// Cells left out of the comparison, with the reason.
    pub skipped: Vec<String>,
}

/// One register's concrete value in a counterexample state. mununu#723 — a cell wider than 64
/// bits was silently absent from the trace (OpenTitan edn's 109-bit `tl_i`, the bus write behind
/// the fatal alert). `value` is unchanged for every cell that fits 64 bits — exactly the cells
/// that were listed before — and `null` for a wider value; `hex` and `bits` are always present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct CexCellView {
    pub register: String,
    /// The value when it fits 64 bits, else `null` (read `hex`).
    pub value: Option<u64>,
    /// `0x` + the value zero-padded to the cell's width — the form that always holds it.
    pub hex: String,
    /// The cell's width in bits.
    pub bits: u32,
}

/// A cell the verifier added to the lifted model rather than one the design declares: the
/// property monitor's history latches (`mununu_wit_…`), `$past` shadows (`x__past`), fault-rewrite
/// state (`mununu_fault_…`), next-frame inputs (`x$next`) and the bit / popcount expansions of a
/// relational atom. A trace lists these under [`TraceStep::monitor`], not beside the design's
/// registers.
pub fn is_monitor_cell(name: &str) -> bool {
    name.starts_with("mununu_")
        || name.starts_with("__mununu")
        || name.contains("__past")
        || name.contains("__bits")
        || name.contains("__popcount")
        || name.ends_with("$next")
}

/// How the lift names what a counterexample shows: the top-level alias of a register yosys named
/// through a bound checker's port (`u_x_sva.state_q` for the design's `state_q`), and the clock
/// nets, which the model steps implicitly and which a trace therefore never lists as inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceNames {
    /// Hierarchical register name → the top-level net on the same bits with the same leaf name.
    pub aliases: std::collections::BTreeMap<String, String>,
    pub clocks: Vec<String>,
}

impl TraceNames {
    /// One state's cells, split into the design's registers (under their top-level alias when the
    /// state does not already name it) and the [monitor cells](is_monitor_cell).
    pub fn split_state(&self, state: &[CexCellView]) -> (Vec<CexCellView>, Vec<CexCellView>) {
        let (monitor, design): (Vec<_>, Vec<_>) = state
            .iter()
            .cloned()
            .partition(|c| is_monitor_cell(&c.register));
        let named: std::collections::HashSet<&str> =
            design.iter().map(|c| c.register.as_str()).collect();
        let design = design
            .iter()
            .map(|c| match self.aliases.get(&c.register) {
                Some(alias) if !named.contains(alias.as_str()) => CexCellView {
                    register: alias.clone(),
                    ..c.clone()
                },
                _ => c.clone(),
            })
            .collect();
        (design, monitor)
    }

    /// One cycle's inputs without the clocks.
    pub fn inputs(&self, inputs: &[CexCellView]) -> Vec<CexCellView> {
        inputs
            .iter()
            .filter(|c| !self.clocks.contains(&c.register))
            .cloned()
            .collect()
    }

    /// `trace` with every step's state split and its inputs filtered (see [`Self::split_state`]).
    pub fn tidy(&self, mut trace: Trace) -> Trace {
        for step in &mut trace.steps {
            let (design, monitor) = self.split_state(&step.state);
            step.state = design;
            step.monitor = monitor;
            step.inputs = self.inputs(&step.inputs);
        }
        trace
    }
}

/// What a [`Trace`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum TraceKind {
    /// Reset → the failing step (a safety violation).
    Finite,
    /// Reset → a prefix → a cycle repeated forever (`loop_start` marks it): a liveness stall.
    Lasso,
    /// Reset → a closed trap the target is never reached from again (a refuted `AG EF`).
    Trap,
    /// No path: the `unreachable_target` atoms are never reached from reset (a refuted `EF`).
    Unreachable,
    /// Reset → the injections of a fault attack that reaches its target (`steps[].faults`).
    Attack,
}

/// One fault injection on a [`TraceStep`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct TraceFault {
    /// The register overwritten.
    pub site: String,
    /// The value written, when it fits 64 bits.
    pub value: Option<u64>,
    /// The value in hexadecimal, at any width.
    pub hex: String,
}

/// One step of a [`Trace`]: the state at `cycle` and what drives the step out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct TraceStep {
    pub cycle: usize,
    /// Register cells (RTL); empty for a CTXDSL step, which names its state in `state_name`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<CexCellView>,
    /// The CTXDSL state's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_name: Option<String>,
    /// The input assignment of this cycle (RTL), so the trace can be replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<CexCellView>,
    /// Faults injected in this cycle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faults: Vec<TraceFault>,
    /// The labels of the transition out of this step (CTXDSL).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Cells the verifier added to the model (see [`is_monitor_cell`]): the property monitor's
    /// latches, `$past` shadows, fault-rewrite state. Kept for debugging, apart from `state`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitor: Vec<CexCellView>,
}

/// Replay artefacts attached to a [`Trace`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct TraceReplay {
    /// The Verilator testbench (`--emit-testbench` / `emit_testbench`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testbench: Option<TestbenchView>,
}

/// The one counterexample shape of 0.5, shared by every verb that returns a path: RTL rows fill
/// `state` / `inputs` / `faults`, CTXDSL rows fill `state_name` / `labels`. A two-player
/// strategy stays its own object, and the counterstrategy graph stays a view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct Trace {
    pub kind: TraceKind,
    pub steps: Vec<TraceStep>,
    /// For a `lasso`: the index into `steps` the last step loops back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_start: Option<usize>,
    /// For `unreachable`: the target atoms no path reaches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreachable_target: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<TraceReplay>,
}

/// Register cells of one state, in the wire shape (`value` when it fits 64 bits, `hex` always).
pub fn cex_cells(
    state: &[(String, crate::adapter::btor2::cex_value::CexValue)],
) -> Vec<CexCellView> {
    state
        .iter()
        .map(|(register, value)| CexCellView {
            register: register.clone(),
            value: value.as_u64(),
            hex: value.to_hex(),
            bits: value.bits(),
        })
        .collect()
}

impl Trace {
    /// The finite path of a native BMC witness (states with their inputs).
    pub fn from_bmc(trace: &crate::adapter::btor2::native_bmc::BmcTrace) -> Trace {
        Trace::from_cells(
            trace.states.iter().map(|s| cex_cells(s)).collect(),
            Vec::new(),
            trace.inputs.iter().map(|s| cex_cells(s)).collect(),
            Vec::new(),
            false,
        )
    }

    /// The environment's starvation lasso of an unrealizable recurrence game. The lasso records
    /// values, not register widths, so each cell's `bits` is its value's bit length.
    pub fn from_stall_lasso(lasso: &crate::adapter::btor2::symbolic_bitblast::StallLasso) -> Trace {
        let states = |rows: &[std::collections::BTreeMap<String, u128>]| -> Vec<Vec<CexCellView>> {
            rows.iter()
                .map(|st| {
                    st.iter()
                        .map(|(register, v)| CexCellView {
                            register: register.clone(),
                            value: u64::try_from(*v).ok(),
                            hex: format!("0x{v:x}"),
                            bits: (128 - v.leading_zeros()).max(1),
                        })
                        .collect()
                })
                .collect()
        };
        Trace::from_cells(
            states(&lasso.prefix),
            states(&lasso.cycle),
            states(&lasso.inputs),
            Vec::new(),
            false,
        )
    }

    /// A CEGAR counter-trace over abstract cells: each step's state lists the predicates as
    /// 1-bit cells (the cell's valuation), `kind` is `trap` when the path ends in a trap.
    pub fn from_cegar(ct: &crate::adapter::btor2::cegar::CounterTrace) -> Trace {
        let steps = ct
            .steps
            .iter()
            .map(|c| {
                c.valuation
                    .iter()
                    .map(|(p, v)| CexCellView {
                        register: p.clone(),
                        value: Some(u64::from(*v)),
                        hex: format!("0x{}", u8::from(*v)),
                        bits: 1,
                    })
                    .collect()
            })
            .collect();
        Trace::from_cells(steps, Vec::new(), Vec::new(), Vec::new(), ct.ends_in_trap)
    }

    /// A fault attack: one step per cycle up to `depth` (or the last injection), each with the
    /// injections made in it. The adversary search records the injections, not the states.
    pub fn attack(
        depth: Option<usize>,
        events: &[crate::adapter::btor2::fault::FaultEvent],
    ) -> Trace {
        let last = depth
            .or_else(|| events.iter().map(|e| e.cycle).max())
            .unwrap_or(0);
        let steps = (0..=last)
            .map(|cycle| TraceStep {
                cycle,
                state: Vec::new(),
                state_name: None,
                inputs: Vec::new(),
                faults: events
                    .iter()
                    .filter(|e| e.cycle == cycle)
                    .map(|e| TraceFault {
                        site: e.site.clone(),
                        value: e.value.as_u64(),
                        hex: e.value.to_hex(),
                    })
                    .collect(),
                labels: Vec::new(),
                monitor: Vec::new(),
            })
            .collect();
        Trace {
            kind: TraceKind::Attack,
            steps,
            loop_start: None,
            unreachable_target: Vec::new(),
            replay: None,
        }
    }

    /// A trace from register-cell states: `prefix` then `cycle` (a lasso when `cycle` is
    /// non-empty), `inputs[i]` driving step `i`; `trap` marks a reset → trap path.
    pub fn from_cells(
        prefix: Vec<Vec<CexCellView>>,
        cycle: Vec<Vec<CexCellView>>,
        inputs: Vec<Vec<CexCellView>>,
        unreachable_target: Vec<String>,
        trap: bool,
    ) -> Trace {
        let loop_start = (!cycle.is_empty()).then_some(prefix.len());
        let mut inputs = inputs.into_iter();
        let steps: Vec<TraceStep> = prefix
            .into_iter()
            .chain(cycle)
            .enumerate()
            .map(|(i, state)| TraceStep {
                cycle: i,
                state,
                state_name: None,
                inputs: inputs.next().unwrap_or_default(),
                faults: Vec::new(),
                labels: Vec::new(),
                monitor: Vec::new(),
            })
            .collect();
        let kind = if !unreachable_target.is_empty() && steps.is_empty() {
            TraceKind::Unreachable
        } else if loop_start.is_some() {
            TraceKind::Lasso
        } else if trap {
            TraceKind::Trap
        } else {
            TraceKind::Finite
        };
        Trace {
            kind,
            steps,
            loop_start,
            unreachable_target,
            replay: None,
        }
    }
}

impl Trace {
    /// A CTXDSL path: `states` by name, `labels[i]` on the step out of `states[i]`, a lasso when
    /// `loop_start` names the state the last one steps back to.
    pub fn from_states(states: &[String], labels: &[String], loop_start: Option<usize>) -> Trace {
        let steps = states
            .iter()
            .enumerate()
            .map(|(i, name)| TraceStep {
                cycle: i,
                state: Vec::new(),
                state_name: Some(name.clone()),
                inputs: Vec::new(),
                faults: Vec::new(),
                labels: labels.get(i).into_iter().cloned().collect(),
                monitor: Vec::new(),
            })
            .collect();
        Trace {
            kind: if loop_start.is_some() {
                TraceKind::Lasso
            } else {
                TraceKind::Finite
            },
            steps,
            loop_start,
            unreachable_target: Vec::new(),
            replay: None,
        }
    }
}

/// `serialize_with` helpers that write the core CTXDSL diagnostics (paths kept as state-name
/// lists) in the [`Trace`] shape, so the CLI and the API emit one form.
pub mod ser {
    use super::Trace;
    use serde::Serializer;

    /// An optional state-name path as an optional finite trace.
    pub fn state_path_opt<S: Serializer>(
        path: &Option<Vec<String>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let t = path.as_ref().map(|p| Trace::from_states(p, &[], None));
        serde::Serialize::serialize(&t, s)
    }

    /// State-name paths as finite traces.
    pub fn state_paths<S: Serializer>(paths: &[Vec<String>], s: S) -> Result<S::Ok, S::Error> {
        let t: Vec<Trace> = paths
            .iter()
            .map(|p| Trace::from_states(p, &[], None))
            .collect();
        serde::Serialize::serialize(&t, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(register: &str, value: u64) -> CexCellView {
        CexCellView {
            register: register.into(),
            value: Some(value),
            hex: format!("0x{value:x}"),
            bits: 3,
        }
    }

    /// Every cell the lift, the property monitor or the fault rewrite adds is a monitor cell; a
    /// design register is not, including a for-generate name (`gen$1.q`) or a sampled input.
    #[test]
    fn monitor_cells_are_the_ones_the_verifier_adds() {
        for name in [
            "mununu_wit_3",
            "mununu_fault_cnt",
            "__mununu_oob__",
            "running__past",
            "x__bits_2",
            "req__popcount",
            "flow$next",
        ] {
            assert!(is_monitor_cell(name), "{name}");
        }
        for name in ["state_q", "u_core.cnt_q", "gen$1.q", "flow"] {
            assert!(!is_monitor_cell(name), "{name}");
        }
    }

    /// An alias never collides with a register the state already names, and the clock leaves
    /// the inputs while every other input stays.
    #[test]
    fn an_alias_yields_to_a_register_already_named_and_clocks_leave_the_inputs() {
        let names = TraceNames {
            aliases: [
                ("u.a".to_string(), "a".to_string()),
                ("u.b".to_string(), "b".to_string()),
            ]
            .into(),
            clocks: vec!["clk".into()],
        };
        let (design, monitor) = names.split_state(&[cell("u.a", 1), cell("a", 2), cell("u.b", 3)]);
        let regs: Vec<_> = design.iter().map(|c| c.register.as_str()).collect();
        assert_eq!(regs, ["u.a", "a", "b"]);
        assert!(monitor.is_empty());
        let inputs = names.inputs(&[cell("clk", 0), cell("go", 1)]);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].register, "go");
    }
}
