//! `ModelFacts` — the lazy, model-anchored derivation cache (verification-execution-planner
//! Phase 0.a).
//!
//! The predicate-cube edge relations (may / must / hyper-must) and the recoverability
//! ranking each re-derive structural facts about the SAME fixed BTOR2 model — most
//! visibly the theory selection `detect_btor2_memories(file)` inside
//! [`crate::adapter::btor2::kmts_lift::encode_design_for_lift`], which runs once per
//! `encode_*` call (three-plus times per lift, and again on every CEGAR refinement
//! iteration). Those facts are pure functions of the model and do not change while the
//! predicate set is refined, so recomputing them is wasted work.
//!
//! `ModelFacts` borrows a parsed model and computes each derivation **on first access,
//! memoized** (`OnceLock`). It is the first increment of the plan's lazy derivation
//! layer: cheap structural facts live here now; the heavier engine projections
//! (bit-blast / cube / CHC) join the same memo in later increments. Anchored to
//! `Btor2File` (the real shared waist today); the anchor migrates to STS-IR when the hub
//! subsumes it. A model transform (cutpoint / config-pin) produces a *new* model and
//! hence a fresh cache, so the memo never goes stale.

use std::sync::OnceLock;

use crate::adapter::btor2::ast::{Btor2File, Nid};
use crate::adapter::btor2::bit_blast::{MemoryCellMeta, detect_btor2_memories};
use crate::adapter::btor2::dep_graph::cone_leaf_nids;
use crate::adapter::btor2::symbolic_bitblast::effective_bitblast_cap;
use crate::adapter::sidecar::predicate_image::btor2_encode::{
    Btor2SmtView, EncodeError, encode_design_with_theory,
};
use crate::adapter::sidecar::predicate_image::theory::Theory;
use crate::adapter::sts_ir::BtorSts;

/// A pinnable primary INPUT — a free `input` leaf `(nid, name, width)`. The candidate
/// surface for auto-config-value (P2.2b): pinning one to a constant removes its `width` bits
/// from every cone it feeds, shrinking the exact engine's bit-blast toward the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputFact {
    pub nid: Nid,
    pub name: String,
    pub width: u32,
}

/// Lazy, memoized structural derivations of one BTOR2 model. Cheap to construct
/// (`new` stores the borrow and empty cells); each accessor computes its fact at most
/// once for the lifetime of the value.
pub(crate) struct ModelFacts<'m> {
    model: &'m Btor2File,
    memories: OnceLock<Vec<MemoryCellMeta>>,
    theory: OnceLock<Theory>,
    total_bits: OnceLock<Option<u32>>,
    free_inputs: OnceLock<Vec<InputFact>>,
    stride_counters: OnceLock<Vec<(Nid, u32)>>,
}

/// mununu#929 — the narrowest counter the diameter proxy reports. A `W`-bit counter bounds the
/// exact engine's fixpoint at about `2^W` preimage steps; at 12 bits that is 4,096 steps, which
/// at the 1–10 ms a step the corpus shows is where a row crosses the 10-second band. Measured on
/// either side (the 18-design corpus, 2026-11-09): 54 rows with 2–6-bit counters, slowest
/// 0.28 s; 12 in-cap rows with a 22-bit counter, 4 of them 28–163 s; the 15-bit power-up
/// counter of `docs/verifying-rtl.md` §`--param` abstains. No row between 7 and 21 bits has
/// been measured, so the cut inside that gap is reasoned, not fitted.
pub(crate) const DIAMETER_BOUND_MIN_WIDTH: u32 = 12;

/// The view of a register that `nid` is, if any: the register's own nid when `nid` is the
/// register or a `uext` / `sext` / `slice` / one-constant-arm `ite` chain over it. Bounded by
/// `depth` so a pathological chain cannot recurse without end.
fn register_view(file: &Btor2File, nid: Nid, depth: u32) -> Option<Nid> {
    use crate::adapter::btor2::ast::{Node, Op};
    let node = &file.lookup(nid)?.node;
    match node {
        Node::State { .. } => Some(nid),
        _ if depth == 0 => None,
        Node::Op {
            op: Op::Uext | Op::Sext | Op::Slice,
            args,
            ..
        } => register_view(file, args.first()?.nid(), depth - 1),
        Node::Op {
            op: Op::Ite, args, ..
        } if args.len() >= 3 => {
            let (then, els) = (args[1].nid(), args[2].nid());
            let is_const = |n: Nid| {
                crate::adapter::btor2::bit_blast::resolve_btor2_constant(file, n).is_some()
            };
            if is_const(els) {
                register_view(file, then, depth - 1)
            } else if is_const(then) {
                register_view(file, els, depth - 1)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// mununu#929 — see [`ModelFacts::stride_counters`]. Walks each register's next-value DAG
/// through operator nodes (stopping at states, inputs and constants) and looks for an
/// `add` / `sub` with an operand that is a view of that register.
fn detect_stride_counters(file: &Btor2File) -> Vec<(Nid, u32)> {
    use crate::adapter::btor2::ast::{Node, Op};
    use crate::adapter::btor2::parser;
    const VIEW_DEPTH: u32 = 8;
    let mut out = Vec::new();
    for line in &file.lines {
        let Node::State { sort, .. } = &line.node else {
            continue;
        };
        let Some(width) = parser::bv_width(file, *sort).filter(|&w| w >= 2) else {
            continue;
        };
        let cnt = line.nid;
        let Some(next) = parser::find_next_value_operand(file, cnt) else {
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![next.nid()];
        let mut steps = false;
        while let Some(nid) = stack.pop() {
            if !seen.insert(nid) {
                continue;
            }
            let Some(Node::Op { op, args, .. }) = file.lookup(nid).map(|l| &l.node) else {
                continue; // a state, input or constant leaf ends this branch
            };
            if matches!(op, Op::Add | Op::Sub)
                && args
                    .iter()
                    .any(|a| register_view(file, a.nid(), VIEW_DEPTH) == Some(cnt))
            {
                steps = true;
                break;
            }
            stack.extend(args.iter().map(|a| a.nid()));
        }
        if steps {
            out.push((cnt, width));
        }
    }
    out
}

impl<'m> ModelFacts<'m> {
    pub(crate) fn new(model: &'m Btor2File) -> Self {
        Self {
            model,
            memories: OnceLock::new(),
            theory: OnceLock::new(),
            total_bits: OnceLock::new(),
            free_inputs: OnceLock::new(),
            stride_counters: OnceLock::new(),
        }
    }

    /// The design's state + input leaf cells (canonical seam `BtorSts::leaf_cells`), or an
    /// empty vec if the model is malformed (facts are best-effort, never fatal).
    /// mununu#577 — the leaf cells, or `None` when the model cannot be analysed.
    ///
    /// **This used to be `.unwrap_or_default()`, and that was the defect.** `BtorSts::leaf_cells`
    /// refuses a model it cannot describe — an array-sorted `state`, say: *"NID 198: state has
    /// non-bitvec sort"*. Defaulting that `Err` to an EMPTY vec did not mean "no leaves"; it meant
    /// **"this design is zero bits wide"**, and every fact derived from it inherited the lie:
    /// `total_bits` → 0, `cone_bits` → 0, so `cone_bits <= cap` was trivially true and the planner
    /// reported `cone 0b ≤ 40b cap → exact decides definitely` for a nine-register design.
    ///
    /// A design with a memory in it is the common case, not an edge case, so this fired on real
    /// consumer RTL and made the routing telemetry confidently wrong.
    fn leaf_cells(&self) -> Option<Vec<crate::adapter::sts_ir::LeafCell>> {
        BtorSts::new(self.model).leaf_cells().ok()
    }

    /// Whole-design bit-blast width — the sum of ALL state+input leaf widths (the size the
    /// exact engine faces when NO cone restriction applies, `keep = None`). Memoized.
    /// mununu#577 — `None` when the leaves cannot be computed, never a silent `0`. A caller that
    /// treats "unknown width" as "zero width" concludes the design fits every cap.
    pub(crate) fn total_bits(&self) -> Option<u32> {
        *self.total_bits.get_or_init(|| {
            self.leaf_cells()
                .map(|cells| cells.iter().map(|c| c.width).sum())
        })
    }

    /// The exact engine's cone-of-influence bit width for a property's `seed_atoms` (the
    /// register names from
    /// [`crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms`]) — the register+input
    /// bits it must bit-blast. Mirrors the engine's keep-set cone (`cone_leaf_nids`); an
    /// empty atom set ⇒ the whole design (`keep = None`). Not memoized (varies per atom set).
    pub(crate) fn cone_bits(&self, seed_atoms: &[String]) -> Option<u32> {
        if seed_atoms.is_empty() {
            return self.total_bits();
        }
        // mununu#602 — the engine's cone is BIT-level where it can be (a slice of a register
        // keeps the slice); the diagnostic must count what the engine bit-blasts, or it would
        // tell a consumer a property is over the cap that the engine just decided. Signal
        // granularity remains for a cone that reaches an array, as in the engine.
        if !crate::adapter::btor2::symbolic_bitblast::signal_level_coi_from_env()
            && let Some(cone) = crate::adapter::btor2::dep_graph::bit_cone(self.model, seed_atoms)
        {
            return Some(crate::adapter::btor2::dep_graph::bit_cone_bits(&cone));
        }
        let cone = cone_leaf_nids(self.model, seed_atoms);
        Some(
            self.leaf_cells()?
                .iter()
                .filter(|c| cone.contains(&c.nid))
                .map(|c| c.width)
                .sum(),
        )
    }

    /// `(cone_bits, cap)` for a property's seed atoms; `cone_bits > cap` is exactly the
    /// condition under which the exact engine bails to a `Skip` — the signal auto-config-value
    /// / auto-cutpoint act on.
    /// mununu#577 — `None` when the cone width is unknown. **Callers must not substitute `0`**:
    /// that is what made `cone_bits <= cap` trivially true on a design whose width had never been
    /// established, and turned a routing prediction into a false claim of decidability.
    pub(crate) fn cone_vs_cap(&self, seed_atoms: &[String]) -> Option<(u32, u32)> {
        let cb = self.cone_bits(seed_atoms)?;
        Some((cb, effective_bitblast_cap(cb)))
    }

    /// P2.2c (2026-07-28) — the CHEAP **diameter proxy**. The exact μ-engine's fixpoint cost is
    /// the state-space DIAMETER, not the cone bit-count: `compute_engine_wide` has a ~34-bit cone
    /// (well under the cap, so `cone_vs_cap` says "admit, cheap") yet a 2³² diameter that grinds
    /// ~20 s then abstains. Bit-count cannot see the diameter — `bit-count ≠ tractability`
    /// (P2.5-A) — with ONE cheap exception: a detected **down-counter** of width `W` in the
    /// property's cone bounds a reachable descent of up to 2^W steps. Returns the widest such
    /// in-cone counter width (= log₂ of the diameter estimate); `None` when the cone carries no
    /// detected counter.
    ///
    /// **SOUNDNESS / USE — this is a HEURISTIC upper bound, NOT a sound hard-skip predicate.** A
    /// wide in-cone counter does *not* guarantee the engine grinds: a property whose target is a
    /// NEARBY value (`EF(cnt == reachable_soon)`) converges in a few iterations regardless of `W`,
    /// so the proxy OVER-predicts that case. It answers "*could* this fixpoint be diameter-bound?"
    /// (yes iff a wide counter feeds the cone) — sound as an ADVISORY note and as a
    /// deadline-shortening input (a genuine decision still finishes under a short deadline; only a
    /// real grind is cut sooner), but UNSOUND as a hard skip. See `cone_counter_diameter_log2_*`.
    ///
    /// mununu#929 — the proxy reads [`Self::stride_counters`], not the b1 `detect_down_counter`
    /// it read until then. That detector is a SOUNDNESS gate for a lossy rewrite: it refuses a counter
    /// whose step is not a constant, whose add is wider than the register, or that is compared
    /// to anything but one constant. Those refusals are right for the rewrite and wrong for a
    /// cost estimate, and OpenTitan's `prim_count` trips all three, so its rows read "no
    /// counter" and were estimated in seconds while measuring up to 163 s.
    ///
    /// Only a counter of at least [`DIAMETER_BOUND_MIN_WIDTH`] bits counts. Reading every
    /// self-stepping register was tried first and refuted on the 18-design corpus: it flagged
    /// FIFO pointers and FSM counters of 2–6 bits on 54 rows that all decided in under 0.3 s,
    /// and the J2 score fell from 205 to 158 of 210. The 22-bit `prim_count` counter carries
    /// every row of that corpus over 10 s.
    pub(crate) fn cone_counter_diameter_log2(&self, seed_atoms: &[String]) -> Option<u32> {
        let counters: Vec<(Nid, u32)> = self
            .stride_counters()
            .iter()
            .copied()
            .filter(|&(_, w)| w >= DIAMETER_BOUND_MIN_WIDTH)
            .collect();
        if counters.is_empty() {
            return None;
        }
        if seed_atoms.is_empty() {
            return counters.iter().map(|&(_, w)| w).max();
        }
        let cone = cone_leaf_nids(self.model, seed_atoms);
        counters
            .iter()
            .filter(|(nid, _)| cone.contains(nid))
            .map(|&(_, w)| w)
            .max()
    }

    /// mununu#929 — every register that steps itself: `(nid, width)` for each `state` of width
    /// ≥ 2 whose next-value function contains an `add` / `sub` with an operand that is a VIEW of
    /// the register itself. A view is the register, a `uext` / `sext` / `slice` of a view, or a
    /// one-constant-arm `ite` of a view (yosys's async-reset mux). The step may be anything (an
    /// input, another register) and the register may be read anywhere.
    ///
    /// Advisory only, and that is why it may be this loose: it feeds the diameter note, the
    /// predicted engine and the J2 cost estimate, never a rewrite and never a verdict. A false
    /// positive (an accumulator, a counter that is never near its wrap) costs an over-estimate,
    /// the safe direction for J5 sizing; a false negative is the under-estimate #929 was.
    /// Memoized.
    pub(crate) fn stride_counters(&self) -> &[(Nid, u32)] {
        self.stride_counters
            .get_or_init(|| detect_stride_counters(self.model))
    }

    /// The pinnable primary-input surface — all free `input` leaves, widest first. Memoized.
    pub(crate) fn free_inputs(&self) -> &[InputFact] {
        self.free_inputs.get_or_init(|| {
            // mununu#577 — an unanalysable model yields NO pinnable inputs. That is the right
            // answer for this list (it drives "what could I pin", not a width claim), and it
            // fails safe: no auto-pin is attempted on a model we cannot describe.
            let mut v: Vec<InputFact> = self
                .leaf_cells()
                .unwrap_or_default()
                .iter()
                .filter(|c| !c.is_state)
                .map(|c| InputFact {
                    nid: c.nid,
                    name: c.name.clone(),
                    width: c.width,
                })
                .collect();
            // Widest-first (then by nid) — the greedy auto-config-value order (pin the input
            // that removes the most cone bits first).
            v.sort_by(|a, b| b.width.cmp(&a.width).then(a.nid.cmp(&b.nid)));
            v
        })
    }

    /// The free inputs that lie IN the cone of `seed_atoms` — the auto-config-value candidates
    /// (pinning an out-of-cone input cannot shrink this cone). Widest first.
    pub(crate) fn pinnable_cone_inputs(&self, seed_atoms: &[String]) -> Vec<InputFact> {
        if seed_atoms.is_empty() {
            return self.free_inputs().to_vec();
        }
        let cone = cone_leaf_nids(self.model, seed_atoms);
        self.free_inputs()
            .iter()
            .filter(|i| cone.contains(&i.nid))
            .cloned()
            .collect()
    }

    /// Inferred `$mem` / array memory cells (`detect_btor2_memories`), computed once.
    pub(crate) fn memories(&self) -> &[MemoryCellMeta] {
        self.memories
            .get_or_init(|| detect_btor2_memories(self.model))
    }

    /// The SMT theory the edge encoder should use — `BvUfArray` iff the model has any
    /// inferred memory, else `BvOnly`. Identical selection to `encode_design_for_lift`,
    /// but the underlying `detect_btor2_memories` runs at most once per model.
    pub(crate) fn theory(&self) -> Theory {
        *self.theory.get_or_init(|| {
            if self.memories().is_empty() {
                Theory::BvOnly
            } else {
                Theory::BvUfArray
            }
        })
    }

    /// Encode the design for the SMT edge relations, reusing the memoized theory.
    /// Verdict-equivalent to `encode_design_for_lift` (same theory, same encoder); the
    /// theory selection is not recomputed per call.
    pub(crate) fn encode(&self) -> Result<Btor2SmtView, EncodeError> {
        encode_design_with_theory(self.model, self.theory())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    #[test]
    fn model_facts_theory_matches_direct_detection_and_memoizes() {
        // A memory-free design ⇒ BvOnly; the memoized value equals a direct detection.
        const MEM_FREE: &str =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let file = parser::parse(MEM_FREE).expect("parse");
        let facts = ModelFacts::new(&file);
        assert!(
            facts.memories().is_empty(),
            "the toy design has no inferred memory"
        );
        assert_eq!(facts.theory(), Theory::BvOnly);
        // Second reads return the same memoized results (no divergence).
        assert!(facts.memories().is_empty());
        assert_eq!(facts.theory(), Theory::BvOnly);
        // The memoized memory set equals a fresh direct detection.
        assert_eq!(facts.memories().len(), detect_btor2_memories(&file).len());
    }

    // A design with an 8-bit counter `ctrl` (next = ctrl+1, self-contained cone), an 8-bit
    // register `other` fed by an 8-bit input `cfg`, and a 1-bit input `en`.
    const COST_FIXTURE: &str = "\
1 sort bitvec 8
2 sort bitvec 1
3 state 1 ctrl
4 one 1
5 add 1 3 4
6 next 1 3 5
7 input 1 cfg
8 input 2 en
9 state 1 other
10 next 1 9 7
";

    #[test]
    fn total_bits_sums_all_leaf_widths() {
        let file = parser::parse(COST_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        // ctrl(8) + cfg(8) + en(1) + other(8) = 25.
        assert_eq!(facts.total_bits(), Some(25));
        // Memoized: second read agrees.
        assert_eq!(facts.total_bits(), Some(25));
    }

    #[test]
    fn free_inputs_are_inputs_only_widest_first() {
        let file = parser::parse(COST_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        let names: Vec<(&str, u32)> = facts
            .free_inputs()
            .iter()
            .map(|i| (i.name.as_str(), i.width))
            .collect();
        // Only the two inputs (not the state registers), widest first.
        assert_eq!(names, vec![("cfg", 8), ("en", 1)]);
    }

    #[test]
    fn cone_bits_restricts_to_the_property_cone() {
        let file = parser::parse(COST_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        // Empty atoms ⇒ the whole design (matches the exact engine's keep = None).
        assert_eq!(facts.cone_bits(&[]), facts.total_bits());
        // `ctrl`'s cone is self-contained (next = ctrl+1) ⇒ just its own 8 bits; `cfg`/`en`/
        // `other` are excluded. Cone restriction genuinely shrinks the count.
        let ctrl_cone = facts.cone_bits(&["ctrl".to_string()]);
        assert!(
            ctrl_cone < facts.total_bits(),
            "ctrl's cone ({ctrl_cone:?}) must exclude the unrelated leaves"
        );
        assert_eq!(ctrl_cone, Some(8), "ctrl's cone is ctrl alone");
        // `other`'s cone pulls in `cfg` (other' = cfg) ⇒ 8 + 8 = 16, and `cfg` is a pinnable
        // in-cone input while none is in ctrl's cone.
        assert_eq!(facts.cone_bits(&["other".to_string()]), Some(16));
        assert_eq!(
            facts
                .pinnable_cone_inputs(&["other".to_string()])
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>(),
            vec!["cfg".to_string()]
        );
        assert!(facts.pinnable_cone_inputs(&["ctrl".to_string()]).is_empty());
    }

    /// mununu#577 REPRODUCER — an array-bearing design must not measure as ZERO bits.
    ///
    /// `BtorSts::leaf_cells` refuses a model with an array-sorted `state` ("NID N: state has
    /// non-bitvec sort"), and `ModelFacts::leaf_cells` used to `.unwrap_or_default()` that `Err`
    /// into an EMPTY vec. The consequence was not a missing fact but a FALSE one: `total_bits` and
    /// `cone_bits` both reported **0**, so `cone_bits <= cap` was trivially true and the planner
    /// announced `cone 0b ≤ 40b cap → exact decides definitely` for a nine-register consumer
    /// design. A design with a memory in it is the common case.
    ///
    /// The fix is that "unknown" is now `None`, distinct from `Some(0)`. This test fails on the
    /// parent commit, where both were `0`.
    #[test]
    fn an_unanalysable_model_reports_unknown_width_not_zero() {
        // A 4-bit counter PLUS an array-sorted state (a memory), which is what makes
        // `BtorSts::leaf_cells` refuse the model.
        const WITH_MEMORY: &str = "\
1 sort bitvec 4
2 sort bitvec 8
3 sort array 1 2
4 state 1 cnt
5 one 1
6 add 1 4 5
7 next 1 4 6
8 state 3 mem
";
        let file = crate::adapter::btor2::parser::parse(WITH_MEMORY).expect("parses");
        let facts = ModelFacts::new(&file);

        assert_eq!(
            facts.total_bits(),
            None,
            "an unanalysable model must report UNKNOWN width, never 0 — a 0 reads as \
             \"fits every cap\" to every downstream check"
        );
        // mununu#602 — a cone that provably never reaches the array HAS a width: the bit cone
        // of `cnt` is its 4 bits, which is a measured width, not the fabricated 0 this test
        // guards against. A cone that reads the array still has none.
        assert_eq!(
            facts.cone_vs_cap(&["cnt".to_string()]).map(|(w, _)| w),
            Some(4),
            "the bit cone of `cnt` excludes the memory, so its width is known"
        );
        const READS_MEMORY: &str = "\
1 sort bitvec 4
2 sort bitvec 8
3 sort array 1 2
4 state 1 cnt
5 one 1
6 add 1 4 5
7 next 1 4 6
8 state 3 mem
9 read 2 8 4 val
";
        let file = crate::adapter::btor2::parser::parse(READS_MEMORY).expect("parses");
        let facts = ModelFacts::new(&file);
        assert_eq!(
            facts.cone_vs_cap(&["val".to_string()]),
            None,
            "no cap comparison may be offered for a width that was never established"
        );
    }

    // An 8-bit down-counter `cnt` (reload-at-0) plus an INDEPENDENT 1-bit toggle `flag`. The
    // diameter proxy must return the counter width for a cone that reaches `cnt`, and `None` for
    // one that does not (`flag`), and must ignore the sub-2-bit toggle (not a counter).
    const COUNTER_FIXTURE: &str = "\
1 sort bitvec 16
2 sort bitvec 1
3 state 1 cnt
4 ones 1
5 zero 1
6 one 1
7 eq 2 3 5
8 sub 1 3 6
9 ite 1 7 4 8
10 next 1 3 9
11 init 1 3 4
12 state 2 flag
13 not 2 12
14 next 2 12 13
15 zero 2
16 init 2 12 15
";

    #[test]
    fn cone_counter_diameter_log2_flags_in_cone_counter() {
        let file = parser::parse(COUNTER_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        // The 16-bit down-counter is detected and lies in its own cone ⇒ diameter proxy = 16.
        assert_eq!(
            facts.cone_counter_diameter_log2(&["cnt".to_string()]),
            Some(16)
        );
        // Whole-design (empty atoms) also sees it.
        assert_eq!(facts.cone_counter_diameter_log2(&[]), Some(16));
        // Memoized detection agrees on a second read.
        assert_eq!(
            crate::adapter::btor2::bit_blast::detect_down_counter(&file).len(),
            1
        );
    }

    #[test]
    fn cone_counter_diameter_log2_excludes_out_of_cone_counter() {
        let file = parser::parse(COUNTER_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        // `flag` toggles independently of `cnt`, so the counter is NOT in its cone ⇒ None.
        // (The cone filter is what keeps the proxy property-specific, not design-global.)
        assert_eq!(
            facts.cone_counter_diameter_log2(&["flag".to_string()]),
            None
        );
    }

    #[test]
    fn a_counter_below_the_cut_is_a_stride_counter_but_not_diameter_bound() {
        // COST_FIXTURE's `ctrl` is a free-running 8-bit `+1` up-counter: not a b1
        // `detect_down_counter` match (no threshold), but a self-stepping register (mununu#929).
        // 2^8 steps is milliseconds for the exact engine (the corpus's 2–6-bit counters all
        // decided under 0.3 s; `examples/recoverability/compute_engine.sv`'s 8-bit `cnt` in
        // milliseconds), so the diameter proxy stays silent. `other` never steps itself.
        let file = parser::parse(COST_FIXTURE).expect("parse");
        let facts = ModelFacts::new(&file);
        assert!(
            crate::adapter::btor2::bit_blast::detect_down_counter(&file).is_empty(),
            "the b1 detector stays strict"
        );
        assert_eq!(facts.stride_counters(), &[(3, 8)]);
        assert_eq!(
            facts.cone_counter_diameter_log2(&["ctrl".to_string()]),
            None
        );
        assert_eq!(
            facts.cone_counter_diameter_log2(&["other".to_string()]),
            None
        );
    }

    // mununu#929 — the shape of OpenTitan `prim_count` as yosys lifts it: a 22-bit counter
    // stepped by an INPUT through a 23-bit add (`{1'b0, cnt} + {1'b0, step}`), sliced back, behind
    // a reset mux, and compared against a twin register rather than a constant. The b1 detector
    // refuses it on every one of those counts; the diameter proxy must not.
    const PRIM_COUNT_SHAPE: &str = "\
1 sort bitvec 1
2 sort bitvec 22
3 sort bitvec 23
4 input 1 rst_ni
5 input 1 incr_en
6 input 2 step
7 state 2 cnt
8 zero 2
9 ite 2 4 7 8
10 uext 3 9 1
11 uext 3 6 1
12 add 3 10 11
13 slice 2 12 3 0
14 ite 2 5 13 9
15 next 2 7 14
16 state 2 twin
17 sub 2 16 6
18 ite 2 5 17 16
19 next 2 16 18
20 eq 1 7 16
21 state 1 err
22 next 1 21 20
23 state 2 shadow
24 next 2 23 7
";

    #[test]
    fn cone_counter_diameter_log2_sees_prim_counts_counter() {
        // The #929 reproducer: only APIs the parent commit has, so it fails there (None).
        let file = parser::parse(PRIM_COUNT_SHAPE).expect("parse");
        let facts = ModelFacts::new(&file);
        assert!(
            crate::adapter::btor2::bit_blast::detect_down_counter(&file).is_empty(),
            "the b1 detector refuses this shape, which is the #929 miss"
        );
        assert_eq!(
            facts.cone_counter_diameter_log2(&["err".to_string()]),
            Some(22)
        );
    }

    #[test]
    fn stride_counters_are_the_registers_that_step_themselves() {
        let file = parser::parse(PRIM_COUNT_SHAPE).expect("parse");
        let facts = ModelFacts::new(&file);
        let mut found: Vec<_> = facts.stride_counters().to_vec();
        found.sort();
        // `cnt` (add through uext + slice + reset mux) and `twin` (a plain sub by an input);
        // `shadow` copies `cnt` without stepping and `err` is one bit.
        assert_eq!(found, vec![(7, 22), (16, 22)]);
    }
}
