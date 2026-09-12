//! R-F5.4.2 (2026-07-03) — the **symbolic predicate-cube engine**: compute the
//! per-cube 3-valued verdicts of a mu-calculus property via the R-F5.3/R-F5.4.1
//! symbolic BDD relation, instead of the explicit `predicate_cube_lift`
//! (`O(2^2|P|)` SMT edge construction + `2^|P|` cube materialisation).
//!
//! This is the integration seam between the symbolic engine
//! ([`crate::adapter::btor2::symbolic_bitblast`]) and the predicate-cube
//! surface the CEGAR / verify-auto path speaks in ([`PredicateSpec`] +
//! `compound_exprs` + a mu-calculus [`Formula`]). It:
//!
//! 1. bit-blasts the BTOR2 transition function to BDDs (R-F5.3a),
//! 2. compiles each [`PredicateSpec`] (or its compound
//!    [`PredicateExpr`](crate::adapter::btor2::predicate_expr::PredicateExpr))
//!    to a predicate BDD (R-F5.3b),
//! 3. builds the abstract may/must relation (R-F5.3c/d),
//! 4. evaluates the formula by BDD image/preimage + μ/ν fixpoint (R-F5.4.1),
//! 5. tallies the `{T, F, ⊥}` verdict over the **feasible** cubes (the cubes the
//!    explicit lift would materialise as abstract states).
//!
//! The verdict at each feasible cube is the same one `evaluate_tri` produces on
//! the explicitly-lifted KMTS (pinned by the R-F5.4.1 differential) — but built
//! without a single per-cube-pair SMT query, which is the H.H.c unblock.

use std::collections::HashMap;

use crate::adapter::btor2::predicate_expr::PredicateExpr;
use crate::adapter::btor2::symbolic_bitblast::{BddBitBlaster, MustSemantics};
use crate::adapter::{AdapterError, AdapterErrorKind};
use crate::mu_calculus::Formula;
use crate::mu_calculus::trit::Trit;

pub use crate::adapter::btor2::kmts_lift::PredicateSpec;

/// The per-cube verdict tally from the symbolic engine.
#[derive(Debug, Clone)]
pub struct SymbolicCubeVerdicts {
    /// Number of predicates `k` (the cube space is `2^k`, of which the feasible
    /// subset is materialisable).
    pub num_predicates: usize,
    /// `(cube_index, verdict)` for every **feasible** cube, ascending by index.
    /// Cube `c`'s bit `i` is the truth of `predicates[i]`.
    pub cube_verdicts: Vec<(usize, Trit)>,
    /// Count of definite-true (`KleeneT`) feasible cubes.
    pub definite_true: usize,
    /// Count of definite-false (`KleeneF`) feasible cubes.
    pub definite_false: usize,
    /// Count of indefinite (`KleeneBot`, ⊥) feasible cubes.
    pub bottom: usize,
}

fn ir_err(message: String) -> AdapterError {
    AdapterError {
        // mununu#542 — a budget abstention (NODE / ITERATION / WALL-CLOCK / OxiDD OutOfMemory)
        // must NOT arrive as an IR defect: `verify_auto`'s generic error arm maps that to
        // `Skipped`, which no CI gate fails on. Classify it so it lands as `Unknown`.
        kind: crate::adapter::classify_engine_error(&message, AdapterErrorKind::IrConsistencyError),
        message: format!("adapter/btor2/symbolic_engine: {message}"),
        location: None,
    }
}

/// Compile a [`PredicateSpec`] to a [`PredicateExpr`]: a compound expression
/// (keyed by the spec's `name` in `compound_exprs`) takes precedence; otherwise
/// the simple `register == value` atom.
fn spec_to_expr(
    spec: &PredicateSpec,
    compound_exprs: &HashMap<String, PredicateExpr>,
) -> PredicateExpr {
    compound_exprs
        .get(&spec.name)
        .cloned()
        .unwrap_or_else(|| PredicateExpr::eq(spec.register.clone(), spec.value))
}

/// R-F5.4.2 — evaluate `formula` over the predicate cube abstraction of the
/// BTOR2 design, symbolically. Returns the per-feasible-cube `{T, F, ⊥}`
/// verdict tally.
///
/// `must_semantics` selects the must-edge form ([`MustSemantics::ForallExists`]
/// is the canonical KMTS must-edge and the right default). The formula must be
/// in the audited-sound fragment (`True`/`False`, predicates, `!`/`&&`/`||`,
/// `mu`/`nu`, `[]`/`<>` — bare or guarded by `req_cur`/`forb_cur`/`req_next`/
/// `forb_next` state predicates, R-F5.5c); a label / controllability / step-bounded
/// guard is a hard error (out-of-fragment over a predicate cube).
pub fn symbolic_cube_verdicts(
    btor2_content: &str,
    predicates: &[PredicateSpec],
    compound_exprs: &HashMap<String, PredicateExpr>,
    formula: &Formula,
    must_semantics: MustSemantics,
) -> Result<SymbolicCubeVerdicts, AdapterError> {
    let file = crate::adapter::btor2::parser::parse(btor2_content).map_err(|mut e| {
        e.message = format!("adapter/btor2/symbolic_engine: {}", e.message);
        e
    })?;

    let exprs: Vec<PredicateExpr> = predicates
        .iter()
        .map(|spec| spec_to_expr(spec, compound_exprs))
        .collect();
    let names: Vec<String> = predicates.iter().map(|s| s.name.clone()).collect();

    // R-F5.6 — restrict the bit-blast to the predicates' cone of influence (out-of-cone leaves
    // pinned to constants), mirroring the exact engine. The predicates are already canonicalized
    // (`resolve_predicate_registers`), so the cone seeds match the bit-blaster's cell symbols; an
    // empty predicate set keeps the full-design behaviour.
    let mut seed_regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &exprs {
        crate::adapter::btor2::symbolic_bitblast::collect_predicate_registers(e, &mut seed_regs);
    }
    let seed_atoms: Vec<String> = seed_regs.into_iter().collect();
    let keep = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));
    let bb = BddBitBlaster::build_with_keep(&file, keep.as_ref()).map_err(ir_err)?;

    let rel = bb
        .abstract_relation(&exprs, Some(must_semantics))
        .map_err(ir_err)?;
    let verdict = rel.evaluate(formula, &names).map_err(ir_err)?;

    let mut cube_verdicts = Vec::new();
    let (mut t, mut f, mut b) = (0usize, 0usize, 0usize);
    for cube in rel.feasible_cubes() {
        let v = rel.verdict_at(&verdict, cube);
        match v {
            Trit::True => t += 1,
            Trit::False => f += 1,
            Trit::Unknown => b += 1,
        }
        cube_verdicts.push((cube, v));
    }

    Ok(SymbolicCubeVerdicts {
        num_predicates: predicates.len(),
        cube_verdicts,
        definite_true: t,
        definite_false: f,
        bottom: b,
    })
}

/// R-F5.5a (2026-07-03) — like [`symbolic_cube_verdicts`], but derives the
/// compound cube-dimension predicates from an [`AdapterOptions`] sidecar (the
/// `compound_predicates` block, e.g. `cnt >= 2`) rather than taking an explicit
/// `compound_exprs` map. This is the surface entry point the CLI (`--sidecar`)
/// and API use so `--engine symbolic` handles inequality / relational
/// predicates — the real sysrst `cnt_clr` / H.H counter-bound residual.
///
/// **Derived / combinational** compound predicates (H.E.2/H.F — per-cube SMT
/// labels, not cube dimensions) are a hard error: the symbolic path has no
/// per-cube-label computation yet; those cases must use `--engine explicit`.
pub fn symbolic_cube_verdicts_from_options(
    btor2_content: &str,
    initial_predicates: &[PredicateSpec],
    options: &crate::adapter::AdapterOptions,
    formula: &Formula,
    must_semantics: MustSemantics,
) -> Result<SymbolicCubeVerdicts, AdapterError> {
    let (predicates, compound) = derive_sidecar_compounds(initial_predicates, options)?;
    symbolic_cube_verdicts(
        btor2_content,
        &predicates,
        &compound,
        formula,
        must_semantics,
    )
}

/// Derive the full cube-dimension predicate set (initial + non-derived sidecar
/// `compound_predicates`) + the compound-expr map. A derived/combinational
/// compound (per-cube SMT label) is a hard error — the symbolic path has no
/// per-cube-label computation yet.
fn derive_sidecar_compounds(
    initial_predicates: &[PredicateSpec],
    options: &crate::adapter::AdapterOptions,
) -> Result<(Vec<PredicateSpec>, HashMap<String, PredicateExpr>), AdapterError> {
    let mut predicates = initial_predicates.to_vec();
    let mut compound: HashMap<String, PredicateExpr> = HashMap::new();
    for (spec, expr, is_derived) in
        crate::adapter::btor2::cegar::sidecar_compound_predicates(options)
    {
        if is_derived {
            return Err(ir_err(format!(
                "the derived/combinational predicate `{}` (a per-cube SMT label, not a cube \
                 dimension) is not supported by the symbolic engine yet — use `--engine explicit`",
                spec.name
            )));
        }
        compound.insert(spec.name.clone(), expr);
        predicates.push(spec);
    }
    Ok((predicates, compound))
}

/// R-F5.5b — how a [`symbolic_cegar_refine`] run terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolicCegarTermination {
    /// No ⊥ cube remains — every feasible cube has a definite verdict.
    Converged,
    /// The iteration / cube-count cap was reached with ⊥ cubes still present.
    BoundedIterationsReached,
    /// WP proposed no new predicate (cone-of-influence exhausted) with ⊥ present.
    PredicateSourceExhausted,
}

/// R-F5.5b — one refinement-iteration record (verdict summary at the start of
/// the iteration, before any predicate for it was added).
#[derive(Debug, Clone)]
pub struct SymbolicCegarIteration {
    pub iteration: usize,
    pub predicate_count: usize,
    pub definite_true: usize,
    pub definite_false: usize,
    pub bottom: usize,
}

/// R-F5.5b — the result of a symbolic CEGAR refinement run.
#[derive(Debug, Clone)]
pub struct SymbolicCegarResult {
    /// Per-iteration verdict records (iteration 0 = the initial evaluation).
    pub iterations: Vec<SymbolicCegarIteration>,
    /// Predicate set at termination (initial + sidecar compounds + WP-added).
    pub final_predicates: Vec<PredicateSpec>,
    /// The final 3-valued verdict tally.
    pub final_verdicts: SymbolicCubeVerdicts,
    pub terminated_with: SymbolicCegarTermination,
}

/// Cube-count cap for the symbolic refinement loop: the per-iteration verdict
/// tally enumerates `2^|P|` cubes, so `|P|` is bounded to keep that tractable
/// (`2^16 = 65 536`). The symbolic *relation* stays compact regardless; only the
/// tally enumerates — a fully-symbolic feasible-cube count (BDD `SatCount`) is a
/// later optimisation.
const MAX_SYMBOLIC_CUBE_BITS: usize = 16;

/// R-F5.5b — symbolic CEGAR refinement loop. Evaluates the property over the
/// predicate-cube abstraction symbolically (no per-cube-pair SMT); on a ⊥
/// verdict, discovers a separating predicate via WP
/// ([`crate::adapter::btor2::cegar::wp_refine_predicates`]), adds it, and
/// re-evaluates — rebuilding the `AbstractRelation` each iteration (cheap; still
/// no SMT). Terminates on convergence (no ⊥), the iteration / cube-count cap, or
/// WP exhaustion.
///
/// `max_iterations = 0` reproduces the single-shot behaviour (evaluate once, no
/// refinement). Compound predicates are derived from the sidecar exactly as
/// [`symbolic_cube_verdicts_from_options`].
pub fn symbolic_cegar_refine(
    btor2_content: &str,
    initial_predicates: &[PredicateSpec],
    options: &crate::adapter::AdapterOptions,
    formula: &Formula,
    must_semantics: MustSemantics,
    max_iterations: usize,
) -> Result<SymbolicCegarResult, AdapterError> {
    let (mut predicates, compound) = derive_sidecar_compounds(initial_predicates, options)?;
    let mut iterations: Vec<SymbolicCegarIteration> = Vec::new();
    let mut iter = 0usize;

    loop {
        let verdicts = symbolic_cube_verdicts(
            btor2_content,
            &predicates,
            &compound,
            formula,
            must_semantics,
        )?;
        iterations.push(SymbolicCegarIteration {
            iteration: iter,
            predicate_count: predicates.len(),
            definite_true: verdicts.definite_true,
            definite_false: verdicts.definite_false,
            bottom: verdicts.bottom,
        });

        // Converged — every feasible cube is definite.
        if verdicts.bottom == 0 {
            return Ok(SymbolicCegarResult {
                iterations,
                final_predicates: predicates,
                final_verdicts: verdicts,
                terminated_with: SymbolicCegarTermination::Converged,
            });
        }
        // Iteration cap.
        if iter >= max_iterations {
            return Ok(SymbolicCegarResult {
                iterations,
                final_predicates: predicates,
                final_verdicts: verdicts,
                terminated_with: SymbolicCegarTermination::BoundedIterationsReached,
            });
        }
        // Discover a refining predicate (WP over the current predicate cone).
        let proposed =
            crate::adapter::btor2::cegar::wp_refine_predicates(&predicates, btor2_content);
        let fresh: Vec<PredicateSpec> = proposed
            .into_iter()
            .filter(|p| !predicates.iter().any(|q| q.name == p.name))
            .collect();
        if fresh.is_empty() {
            return Ok(SymbolicCegarResult {
                iterations,
                final_predicates: predicates,
                final_verdicts: verdicts,
                terminated_with: SymbolicCegarTermination::PredicateSourceExhausted,
            });
        }
        // Cube-count cap — don't grow the predicate set past the tally bound.
        if predicates.len() + fresh.len() > MAX_SYMBOLIC_CUBE_BITS {
            return Ok(SymbolicCegarResult {
                iterations,
                final_predicates: predicates,
                final_verdicts: verdicts,
                terminated_with: SymbolicCegarTermination::BoundedIterationsReached,
            });
        }
        predicates.extend(fresh);
        iter += 1;
    }
}

#[cfg(test)]
mod tests {
    /// mununu#542 — the WIRING: this module converts every engine `String` error, so a budget
    /// abstention must leave here already classified. `verify_auto`'s generic arm maps anything
    /// else to `Skipped`, and no CI gate fails on `skipped`.
    ///
    /// This is the half that was missing. `verify_auto` already mapped
    /// `ResourceBudgetExceeded → Unknown` (mununu#504), but nothing on the symbolic path ever
    /// produced that kind — `ir_err` stamped `IrConsistencyError` on everything.
    #[test]
    fn ir_err_classifies_a_budget_abstention_as_a_budget_error() {
        use crate::adapter::AdapterErrorKind;

        let e = super::ir_err(
            "symbolic bit-blaster: BDD arena exhausted (OxiDD OutOfMemory) — abstained on the \
             NODE budget; the cone does not compress to a tractable BDD"
                .to_string(),
        );
        assert_eq!(
            e.kind,
            AdapterErrorKind::ResourceBudgetExceeded,
            "an arena exhaustion is an abstention, not an IR defect — it must reach verify_auto \
             as `unknown`, never `skipped`"
        );

        // A genuine IR defect keeps its kind and so still maps to `Skipped` with its message.
        let d = super::ir_err("predicate register not found in the cone".to_string());
        assert_eq!(d.kind, AdapterErrorKind::IrConsistencyError);
    }

    use super::*;
    use crate::adapter::btor2::predicate_expr::CmpOp;
    use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, TransitionModality, Tristate};
    use crate::mu_calculus::evaluator::{Environment, evaluate_tri};

    // A 2-bit saturating counter with an enable — the R-F5.3/4 fixture.
    const SAT_COUNTER: &str = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 input 2 en
5 one 1
6 ones 1
7 add 1 3 5
8 eq 2 3 6
9 ite 1 8 3 7
10 ite 1 4 9 3
11 next 1 3 10
"#;

    /// Ground truth: build the explicit cube-KMTS over feasible cubes (from the
    /// concrete `simulate_one_step` relation) + AP labels, run `evaluate_tri`,
    /// and return the per-feasible-cube verdict list — fully independent of the
    /// symbolic engine under test.
    fn tri_oracle(
        btor2: &str,
        exprs: &[PredicateExpr],
        names: &[&str],
        states: &[(&str, u32)],
        formula_str: &str,
    ) -> Vec<(usize, Trit)> {
        use crate::adapter::btor2::bit_blast::simulate_one_step;
        use crate::adapter::btor2::parser;

        let file = parser::parse(btor2).expect("parse");
        let cube_of = |regs: &HashMap<String, u128>| -> usize {
            let mut c = 0usize;
            for (i, p) in exprs.iter().enumerate() {
                if p.eval(regs) {
                    c |= 1 << i;
                }
            }
            c
        };
        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        // Enumerate every concrete (register, input) → collect feasible cubes +
        // the may/must edges (∀∃ must: a cube-edge is must iff every state in the
        // source cube reaches the target under some input).
        let inputs: Vec<(String, u32)> = file_inputs(&file);
        let total_input_bits: u32 = inputs.iter().map(|(_, w)| *w).sum();
        let mut by_cube: HashMap<usize, Vec<std::collections::HashSet<usize>>> = HashMap::new();
        for scombo in 0..(1u128 << total_state_bits) {
            let mut regs: HashMap<String, u128> = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (scombo >> off) & mask);
                off += w;
            }
            let present = cube_of(&regs);
            let mut reach = std::collections::HashSet::new();
            for icombo in 0..(1u128 << total_input_bits) {
                let mut inps: HashMap<String, u128> = HashMap::new();
                let mut ioff = 0u32;
                for (name, w) in &inputs {
                    let mask = (1u128 << w) - 1;
                    inps.insert(name.clone(), (icombo >> ioff) & mask);
                    ioff += w;
                }
                let next = simulate_one_step(&file, &regs, &inps).expect("step");
                let mut rn = regs.clone();
                rn.extend(next);
                reach.insert(cube_of(&rn));
            }
            by_cube.entry(present).or_default().push(reach);
        }

        let feasible: Vec<usize> = {
            let mut v: Vec<usize> = by_cube.keys().copied().collect();
            v.sort_unstable();
            v
        };
        let idx: HashMap<usize, usize> =
            feasible.iter().enumerate().map(|(j, &c)| (c, j)).collect();

        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for j in 0..feasible.len() {
            b.state(format!("c{j}"));
        }
        b.initial("c0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..feasible.len())
            .map(|j| b.state_id_or_insert(format!("c{j}")).unwrap())
            .collect();
        for (j, &ci) in feasible.iter().enumerate() {
            for (i, nm) in names.iter().enumerate() {
                let v = if (ci >> i) & 1 == 1 {
                    Tristate::KleeneT
                } else {
                    Tristate::KleeneF
                };
                b.with_3valued_predicate(ids[j], *nm, v);
            }
        }
        for (&ci, reaches) in &by_cube {
            let src = ids[idx[&ci]];
            // may target: union of all reach sets; must target: in every reach set.
            let mut may: std::collections::HashSet<usize> = Default::default();
            for r in reaches {
                may.extend(r);
            }
            for &cj in &may {
                let is_must = reaches.iter().all(|r| r.contains(&cj));
                let m = if is_must {
                    TransitionModality::Sharp
                } else {
                    TransitionModality::MayOnly
                };
                b.transition_ids_with_modality(src, &[step], ids[idx[&cj]], m);
            }
        }
        let clts = b.build().expect("clts");

        let formula = crate::mu_calculus::parser::parse(formula_str).expect("formula");
        let env = Environment::new(clts.state_count());
        let tri = evaluate_tri(&formula, &clts, &env).expect("evaluate_tri");
        feasible
            .iter()
            .enumerate()
            .map(|(j, &c)| (c, tri.verdict_at(j)))
            .collect()
    }

    fn file_inputs(file: &crate::adapter::btor2::ast::Btor2File) -> Vec<(String, u32)> {
        use crate::adapter::btor2::ast::Node;
        use crate::adapter::btor2::parser::bv_width;
        let mut out = Vec::new();
        for line in &file.lines {
            if let Node::Input { sort, symbol } = &line.node {
                let w = bv_width(file, *sort).unwrap_or(0);
                let name = symbol
                    .clone()
                    .unwrap_or_else(|| format!("input_{}", line.nid));
                out.push((name, w));
            }
        }
        out
    }

    #[test]
    fn rf5_4_2_symbolic_engine_matches_tri_via_predicate_specs() {
        // Predicates through the real API: p = (cnt == 0) as a simple spec,
        // q = (cnt >= 2) as a compound expr keyed by name.
        let specs = vec![
            PredicateSpec {
                name: "p".to_string(),
                register: "cnt".to_string(),
                value: 0,
            },
            PredicateSpec {
                name: "q".to_string(),
                register: "cnt".to_string(),
                value: 2, // unused (compound below), kept for the API shape
            },
        ];
        let mut compound: HashMap<String, PredicateExpr> = HashMap::new();
        compound.insert(
            "q".to_string(),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        );
        let exprs = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];

        let formulas = [
            "[] p",
            "<> q",
            "nu X. (not q) and [] X",
            "mu X. q or <> X",
            "nu Y. (mu X. (q or <> X)) and [] Y",
        ];

        for formula_str in formulas {
            let formula = crate::mu_calculus::parser::parse(formula_str).expect("formula");
            let got = symbolic_cube_verdicts(
                SAT_COUNTER,
                &specs,
                &compound,
                &formula,
                MustSemantics::ForallExists,
            )
            .expect("symbolic verdicts");

            let want = tri_oracle(SAT_COUNTER, &exprs, &["p", "q"], &[("cnt", 2)], formula_str);

            assert_eq!(
                got.cube_verdicts, want,
                "symbolic engine ≠ evaluate_tri for `{formula_str}`"
            );
            // Tally consistency.
            let (mut t, mut f, mut b) = (0, 0, 0);
            for (_, v) in &got.cube_verdicts {
                match v {
                    Trit::True => t += 1,
                    Trit::False => f += 1,
                    Trit::Unknown => b += 1,
                }
            }
            assert_eq!(
                (got.definite_true, got.definite_false, got.bottom),
                (t, f, b),
                "tally consistent for `{formula_str}`"
            );
        }
    }

    /// R-F5.6 on the SYMBOLIC engine: a 47-bit design (2-bit `fsm` + 45-bit out-of-cone `wide`)
    /// exceeds the bit cap, but the predicate `p = (fsm==0)` has a 2-bit cone, so cone-of-
    /// influence lets `symbolic_cube_verdicts` DECIDE instead of erroring on the cap. Mirrors the
    /// exact engine's `rf5_6_coi_lifts_bit_cap_on_out_of_cone_datapath`.
    #[test]
    fn rf5_6_symbolic_coi_lifts_bit_cap() {
        const FSM_PLUS_WIDE: &str = r#"
1 sort bitvec 2
2 sort bitvec 45
3 state 1 fsm
4 one 1
5 add 1 3 4
6 next 1 3 5
7 zero 1
8 init 1 3 7
9 state 2 wide
10 one 2
11 add 2 9 10
12 next 2 9 11
"#;
        let specs = vec![PredicateSpec {
            name: "p".to_string(),
            register: "fsm".to_string(),
            value: 0,
        }];
        let compound: HashMap<String, PredicateExpr> = HashMap::new();
        let formula = crate::mu_calculus::parser::parse("mu X. p or <> X").expect("formula");
        // Full design is 2 + 45 = 47 bits (> the 40-bit cap); the cone of `fsm==0` is 2 bits, so
        // COI restricts the bit-blast and the call decides instead of returning a cap error.
        let got = symbolic_cube_verdicts(
            FSM_PLUS_WIDE,
            &specs,
            &compound,
            &formula,
            MustSemantics::ForallExists,
        )
        .expect("symbolic COI restricts to the 2-bit fsm cone => decides, not capped");
        assert!(
            got.definite_true + got.definite_false + got.bottom > 0,
            "COI-restricted symbolic engine produced cube verdicts",
        );
    }

    /// A `w`-bit saturating-free counter `r` with an enable: `r' = en ? r+1 : r`,
    /// plus `k` mutually-exclusive equality predicates `r == 0 .. r == k-1`.
    fn counter_btor2(w: u32) -> String {
        format!(
            "\n1 sort bitvec {w}\n2 sort bitvec 1\n3 state 1 r\n4 input 2 en\n5 one 1\n6 add 1 3 5\n7 ite 1 4 6 3\n8 next 1 3 7\n"
        )
    }

    fn eq_predicates(k: usize) -> (Vec<PredicateSpec>, HashMap<String, PredicateExpr>) {
        let specs = (0..k)
            .map(|i| PredicateSpec {
                name: format!("p{i}"),
                register: "r".to_string(),
                value: i as u64,
            })
            .collect();
        (specs, HashMap::new())
    }

    /// R-F5.4.3 — scaling measurement: the symbolic engine builds the abstract
    /// relation + evaluates at `|P|` up to 12 (a `2^24`-cube-pair space) in well
    /// under the explicit `SmtAllPairs` path's `O(2^2|P|)` SMT wall (the H.H.c
    /// "29 min, no output" at `|P| ≈ 12`). This test asserts completion + a
    /// consistent tally at each `|P|`, and prints the wall-clock so the win is
    /// visible with `--nocapture`. The bound (30 s) only trips on a catastrophic
    /// BDD blow-up — it is not a micro-benchmark.
    #[test]
    fn rf5_4_3_symbolic_scales_past_the_smt_wall() {
        use std::time::Instant;
        let formula = crate::mu_calculus::parser::parse("mu X. p0 or <> X").expect("formula");

        for &k in &[4usize, 8, 12] {
            let w = 12u32; // 4096 concrete states — plenty to inhabit the k cubes
            let btor2 = counter_btor2(w);
            let (specs, compound) = eq_predicates(k);

            let start = Instant::now();
            let got = symbolic_cube_verdicts(
                &btor2,
                &specs,
                &compound,
                &formula,
                MustSemantics::ForallExists,
            )
            .expect("symbolic verdicts");
            let elapsed = start.elapsed();

            // k mutually-exclusive eq-predicates ⇒ k singleton cubes + the
            // all-false cube = k+1 feasible cubes.
            assert_eq!(
                got.cube_verdicts.len(),
                k + 1,
                "|P|={k}: expected {} feasible cubes",
                k + 1
            );
            assert_eq!(
                got.definite_true + got.definite_false + got.bottom,
                got.cube_verdicts.len(),
                "|P|={k}: tally covers every feasible cube"
            );
            // p0 (r==0) is definitely satisfied by `mu X. p0 or <> X`.
            assert!(got.definite_true >= 1, "|P|={k}: EF(r==0) holds at r==0");

            eprintln!(
                "R-F5.4.3  |P|={k:>2}  ({} cube-pairs for the explicit SMT path)  \
                 symbolic: {:>8.3?}  T={} F={} ⊥={}",
                1u64 << (2 * k),
                elapsed,
                got.definite_true,
                got.definite_false,
                got.bottom
            );
            assert!(
                elapsed.as_secs() < 30,
                "|P|={k}: symbolic path took {elapsed:?} — investigate BDD blow-up"
            );
        }
    }

    // ---- R-F5.5a: compound predicates from the sidecar ----

    /// A sidecar `compound_predicates` inequality (`cnt >= 2`) becomes a cube
    /// dimension under `symbolic_cube_verdicts_from_options`, and the verdicts
    /// match `evaluate_tri` — the real inequality-predicate residual under
    /// `--engine symbolic`.
    #[test]
    fn rf5_5a_symbolic_from_options_derives_sidecar_compound() {
        use crate::adapter::AdapterOptions;
        let sidecar =
            r#"{"module":"m","compound_predicates":[{"name":"cnt_hi","expr":"cnt >= 2"}]}"#;
        let options = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let initial = vec![PredicateSpec {
            name: "p".to_string(),
            register: "cnt".to_string(),
            value: 0,
        }];
        let formula_str = "mu X. cnt_hi or <> X"; // EF (cnt >= 2)
        let formula = crate::mu_calculus::parser::parse(formula_str).expect("formula");
        let got = symbolic_cube_verdicts_from_options(
            SAT_COUNTER,
            &initial,
            &options,
            &formula,
            MustSemantics::ForallExists,
        )
        .expect("symbolic verdicts from sidecar");

        // Oracle over the same predicate set [cnt==0, cnt>=2] in the same order.
        let exprs = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        let want = tri_oracle(
            SAT_COUNTER,
            &exprs,
            &["p", "cnt_hi"],
            &[("cnt", 2)],
            formula_str,
        );
        assert_eq!(
            got.cube_verdicts, want,
            "sidecar-compound path ≠ evaluate_tri"
        );
        // cnt ∈ {0,1,2,3} ⇒ 3 feasible cubes ({p}, {neither}, {cnt_hi}).
        assert_eq!(got.cube_verdicts.len(), 3);
    }

    /// A **derived** compound predicate (per-cube SMT label) is rejected — the
    /// symbolic engine has no per-cube-label path yet.
    #[test]
    fn rf5_5a_symbolic_from_options_rejects_derived_predicate() {
        use crate::adapter::AdapterOptions;
        let sidecar = r#"{"module":"m","compound_predicates":[{"name":"d","expr":"cnt >= 2","derived":true}]}"#;
        let options = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let initial = vec![PredicateSpec {
            name: "p".to_string(),
            register: "cnt".to_string(),
            value: 0,
        }];
        let formula = crate::mu_calculus::parser::parse("[] p").expect("formula");
        let err = symbolic_cube_verdicts_from_options(
            SAT_COUNTER,
            &initial,
            &options,
            &formula,
            MustSemantics::ForallExists,
        )
        .expect_err("derived compound must be rejected");
        assert!(
            err.message.contains("derived"),
            "expected a derived-predicate rejection, got: {}",
            err.message
        );
    }

    // ---- R-F5.5b: symbolic CEGAR refinement loop ----

    /// `EF(cnt == 3)` on the saturating counter: the `{cnt != 3}` cubes are ⊥
    /// (they *may* reach cnt==3 via `en=1`, but not *must* — input
    /// nondeterminism), so the initial verdict carries ⊥. The refinement loop
    /// must record iterations, only ever grow the predicate set, and terminate
    /// with a valid reason (this ⊥ is a genuine thoroughness gap, so it does not
    /// converge — WP exhausts / caps).
    #[test]
    fn rf5_5b_symbolic_refine_terminates_and_grows_predicates() {
        use crate::adapter::AdapterOptions;
        let options = AdapterOptions::default();
        let initial = vec![PredicateSpec {
            name: "top".to_string(),
            register: "cnt".to_string(),
            value: 3,
        }];
        let formula = crate::mu_calculus::parser::parse("mu X. top or <> X").expect("formula");

        let result = symbolic_cegar_refine(
            SAT_COUNTER,
            &initial,
            &options,
            &formula,
            MustSemantics::ForallExists,
            8,
        )
        .expect("symbolic refine");

        assert!(!result.iterations.is_empty());
        assert_eq!(result.iterations[0].iteration, 0);
        assert!(
            result.iterations[0].bottom >= 1,
            "EF(cnt==3) starts with ⊥ cubes"
        );
        // Predicate count is non-decreasing (predicates are only ever added).
        for w in result.iterations.windows(2) {
            assert!(
                w[1].predicate_count >= w[0].predicate_count,
                "predicate set must not shrink"
            );
        }
        // The final verdict summary matches the last iteration record.
        assert_eq!(
            result.final_verdicts.bottom,
            result.iterations.last().unwrap().bottom
        );
        // Converged ⇒ no ⊥ remains; otherwise ⊥ persists (thoroughness gap).
        match result.terminated_with {
            SymbolicCegarTermination::Converged => {
                assert_eq!(result.final_verdicts.bottom, 0)
            }
            SymbolicCegarTermination::BoundedIterationsReached
            | SymbolicCegarTermination::PredicateSourceExhausted => {}
        }
    }

    /// `max_iterations = 0` reproduces the single-shot behaviour: exactly one
    /// evaluation, no refinement, the predicate set unchanged.
    #[test]
    fn rf5_5b_symbolic_refine_single_shot_when_max_zero() {
        use crate::adapter::AdapterOptions;
        let options = AdapterOptions::default();
        let initial = vec![PredicateSpec {
            name: "top".to_string(),
            register: "cnt".to_string(),
            value: 3,
        }];
        let formula = crate::mu_calculus::parser::parse("mu X. top or <> X").expect("formula");

        let result = symbolic_cegar_refine(
            SAT_COUNTER,
            &initial,
            &options,
            &formula,
            MustSemantics::ForallExists,
            0,
        )
        .expect("symbolic refine single-shot");

        assert_eq!(result.iterations.len(), 1, "single-shot = one evaluation");
        assert_eq!(result.final_predicates.len(), 1, "no predicate added");
    }
}
