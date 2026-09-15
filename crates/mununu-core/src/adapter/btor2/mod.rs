//! BTOR2 (word-level bit-vector verification IR) adapter.
//!
//! Reads a BTOR2 file — produced by Yosys (`write_btor`), Pono, AVR, BtorMC,
//! or hand-authored — and translates it into CTXDSL via the shared adapter IR.
//!
//! # Phase 1 scope
//!
//! - **Supported operators** — see [`ast::Op::is_blastable`].
//! - **Bounded state space** — total state-bit width ≤ [`bit_blast::MAX_STATE_BITS`].
//!   Designs above that are rejected; the recommended path is compose-and-decompose
//!   (Phase 3) before BTOR2 hand-off to an external symbolic engine.
//! - **Properties** — `bad`, `constraint`, `fair`, `justice` are translated to
//!   safety / liveness μ-calculus formulas. `output` is informational only.
//!
//! # Out of scope (Phase 1)
//!
//! - Array sorts (`read`, `write`).
//! - Modular / signed division (`sdiv`, `udiv`, `srem`, `smod`, `urem`).
//! - Overflow detectors (`saddo`, `uaddo`, `smulo`, ...).
//! - Multi-clock designs.

pub mod abs_safety;
pub mod antecedent_shadow;
pub mod array_prophecy;
pub mod ast;
pub mod bad_monitor;
pub mod bit_blast;
pub mod bit_interaction;
pub mod bv;
pub mod cegar;
pub mod concrete_oracle;
pub mod dep_graph;
pub mod emit;
pub mod kmts_lift;
pub mod l2s_monitor;
pub(crate) mod model_facts;
pub mod mutate;
pub mod native_bmc;
#[cfg(feature = "boolector")]
pub mod native_boolector;
pub mod native_interp;
pub mod native_spacer;
pub mod parser;
pub mod pin;
pub mod predicate_expr;
pub mod r_s8_encoder;
pub mod refine;
pub mod reset_init;
pub mod shadow;
pub mod smt_must_edge;
pub mod symbolic_bitblast;
pub mod symbolic_engine;
pub mod term_backend;

pub use cegar::{
    CegarIteration, CegarOptions, CegarTermination, CegarTrace, PredicateSource, cegar_refine_loop,
};
pub use kmts_lift::{
    EagerLazyLift, KmtsLiftLazy, KmtsLiftOptions, KmtsLiftResult, LazyExpansionEdge, LazyLift,
    LiftedPredicate, NullLazyLift, PredicateCubeLiftOptions, PredicateCubeLiftResult,
    PredicateSpec, lift_btor2_to_kmts, lift_predicate_cube, materialize_clts_from_lazy,
    predicate_cube_lift,
};
pub use shadow::{augment_with_past_shadows, can_shadow};

use super::{AdapterError, AdapterOptions, AdapterOutput, FormatAdapter};

/// BTOR2 adapter implementing [`FormatAdapter`].
pub struct Btor2Adapter;

impl FormatAdapter for Btor2Adapter {
    fn detect(content: &str) -> bool {
        // BTOR2 has no magic bytes; detect heuristically: at least one line
        // matching `<int> sort bitvec <int>` near the top.
        for line in content.lines().take(64) {
            let trimmed = match line.find(';') {
                Some(p) => &line[..p],
                None => line,
            }
            .trim();
            if trimmed.is_empty() {
                continue;
            }
            let toks: Vec<&str> = trimmed.split_whitespace().collect();
            if toks.len() >= 4
                && toks[0].parse::<i64>().is_ok()
                && toks[1] == "sort"
                && toks[2] == "bitvec"
            {
                return true;
            }
            // First non-empty non-comment line must be a NID; if not, give up.
            if toks[0].parse::<i64>().is_err() {
                return false;
            }
        }
        false
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        // §Phase 10 stage 3.b (2026-06-12) — UF-mode routing
        // dispatch. When the sidecar declares one or more memories
        // with `abstraction: uf`, the BTOR2 adapter routes through
        // `uf_lift_to_adapter_output` (which calls
        // `predicate_cube_lift` internally) instead of
        // `bit_blast::translate`. The bit-blaster fundamentally
        // cannot process Read/Write on array-sorted operands
        // (it enumerates concrete state cubes); the predicate-
        // cube path operates symbolically and integrates with
        // R.5b's UF wrapping infrastructure. See
        // docs/design/phase10-uf-routing.md for the full design.
        //
        // Strict additivity: the dispatch is a no-op for every
        // existing fixture (no `abstraction: uf` declarations
        // anywhere in the workspace today). Stage 3.b ships the
        // routing decision + a stub destination; stage 3.c
        // populates the actual UF lift.
        if requires_uf_routing(content, options) {
            return uf_lift_to_adapter_output(content, options);
        }
        bit_blast::translate(content, options)
    }
}

/// §Phase 10 stage 3.b (2026-06-12) — decide whether this BTOR2 +
/// sidecar combination requires the UF-mode routing path. Returns
/// `true` when the BTOR2 contains at least one memory cell AND the
/// sidecar declares at least one of those cells with
/// `abstraction: uf`. Returns `false` otherwise — including when
/// the BTOR2 fails to parse (the downstream `bit_blast::translate`
/// will surface the parse error with a richer message than this
/// pre-check could provide).
///
/// **Pure**: no I/O. Re-parses the BTOR2 content; cost is bounded
/// by the lexer + scope walker (no SMT, no enumeration). For
/// memory-free fixtures the function short-circuits at the empty
/// `memory_cells` check, so the cost on the common path is one
/// parser run + one walk over the line list.
///
/// **Why no caching of the parsed file**: the routing decision
/// runs at the FormatAdapter trait boundary, BEFORE the
/// downstream lifter starts. The downstream lifter re-parses the
/// content (either path). The redundancy is mild and matches the
/// existing FormatAdapter::translate contract (consumes a `&str`).
/// A future refactor could thread the parsed `Btor2File` through
/// the dispatch to avoid the duplicate parse; deferred to keep
/// stage 3.b's diff small.
fn requires_uf_routing(content: &str, options: &AdapterOptions) -> bool {
    let Ok(file) = parser::parse(content) else {
        return false;
    };
    let memory_cells = bit_blast::detect_btor2_memories(&file);
    if memory_cells.is_empty() {
        return false;
    }
    let uf_nids = bit_blast::sidecar_uf_memory_nids(&memory_cells, options);
    !uf_nids.is_empty()
}

/// §Phase 10 stage 3.b (2026-06-12) — stub destination for the
/// UF-mode routing path. The dispatch is correct; the destination
/// is partial.
///
/// Stage 3.b ships the routing decision only. Stage 3.c will
/// populate this function with a call to
/// [`crate::adapter::btor2::kmts_lift::predicate_cube_lift`] +
/// the array-aware Z3 term construction in
/// [`crate::adapter::sidecar::predicate_image::all_smt`]
/// and [`crate::adapter::btor2::kmts_lift::evaluate_pure`].
///
/// Today it returns an `AdapterError` with a stage-3.c-specific
/// hint pointing the user at the design doc + the queued
/// implementation. This is intentionally distinct from the
/// pre-routing stage-3.a error message users got before stage
/// 3.b shipped — the stage-3.a message implied the bit-blaster
/// was the destination ("until stages 3.b + 3.c ship, switch to
/// `abstraction: havoc`"); the stage-3.b dispatch-then-error
/// message confirms the routing decision happened but the
/// destination is still a stub.
///
/// Integration tests pin the dispatch (the bit-blast path is NOT
/// reached) without relying on the stub's specific error message
/// content — the dispatch correctness is what stage 3.b proves.
fn uf_lift_to_adapter_output(
    _content: &str,
    _options: &AdapterOptions,
) -> Result<AdapterOutput, AdapterError> {
    Err(AdapterError {
        kind: super::AdapterErrorKind::UnsupportedConstruct,
        message: "§Phase 10 stage 3.b: UF-mode routing dispatched correctly, but the destination \
                  `uf_lift_to_adapter_output` is a stub — stage 3.c populates the actual \
                  `predicate_cube_lift` integration + Z3 Array theory term construction. \
                  See docs/design/phase10-uf-routing.md for the design + open questions. \
                  Until stage 3.c ships, switch the affected memory's sidecar declaration \
                  to `abstraction: havoc` for a sound over-approximating safety verdict."
            .to_string(),
        location: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_typical_btor2() {
        let src = "; comment\n1 sort bitvec 1\n2 input 1\n";
        assert!(Btor2Adapter::detect(src));
    }

    #[test]
    fn rejects_non_btor2() {
        let src = "module foo;\nendmodule\n";
        assert!(!Btor2Adapter::detect(src));
    }

    #[test]
    fn detects_via_format_adapter_trait() {
        let src = "1 sort bitvec 1\n2 zero 1\n";
        assert!(Btor2Adapter::detect(src));
        let out = Btor2Adapter::translate(src, &AdapterOptions::default()).expect("ok");
        assert_eq!(out.source_info.state_count, 1);
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 stage 3.b tests — UF-mode routing dispatch
    // ─────────────────────────────────────────────────────────────

    /// Minimal BTOR2 that declares one array-typed state cell.
    /// Mirrors the shape of `PHASE10_FIXTURE_WITH_MEMORY` from
    /// `bit_blast.rs` tests; reproduced here to keep the dispatch
    /// tests in the same module as the dispatch helper.
    const STAGE3B_BTOR2_WITH_MEMORY: &str = "1 sort bitvec 5\n\
                                              2 sort bitvec 8\n\
                                              3 sort array 1 2\n\
                                              4 state 3 rf_reg\n\
                                              5 input 1 raddr\n\
                                              6 read 2 4 5\n\
                                              7 bad 6\n";

    #[test]
    fn requires_uf_routing_returns_false_when_no_sidecar() {
        // Memory present, no sidecar → routing decision falls
        // through to bit-blast (which will emit the actionable
        // sidecar-template error from stage 1).
        assert!(!requires_uf_routing(
            STAGE3B_BTOR2_WITH_MEMORY,
            &AdapterOptions::default()
        ));
    }

    #[test]
    fn requires_uf_routing_returns_false_when_sidecar_omits_uf() {
        // Memory present, sidecar uses havoc → routing falls
        // through to bit-blast (which lifts via the stage 1b
        // havoc rewriter).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        assert!(!requires_uf_routing(STAGE3B_BTOR2_WITH_MEMORY, &opts));
    }

    #[test]
    fn requires_uf_routing_returns_true_when_sidecar_declares_uf() {
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "uf"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        assert!(requires_uf_routing(STAGE3B_BTOR2_WITH_MEMORY, &opts));
    }

    #[test]
    fn requires_uf_routing_returns_false_on_memory_free_btor2() {
        // No memory cells in the BTOR2 → short-circuit, regardless
        // of what the sidecar says.
        let src = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 3\n";
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": []
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        assert!(!requires_uf_routing(src, &opts));
    }

    #[test]
    fn requires_uf_routing_returns_false_on_malformed_btor2() {
        // Parse failure → short-circuit false (the downstream
        // bit-blast path surfaces the parse error with a better
        // message than this pre-check could).
        let src = "not actually btor2 at all\n";
        assert!(!requires_uf_routing(src, &AdapterOptions::default()));
    }

    #[test]
    fn translate_routes_through_uf_stub_when_uf_declared() {
        // The dispatch CORRECTLY reaches uf_lift_to_adapter_output
        // (the stub) instead of bit_blast::translate. The stub
        // returns an error whose message identifies stage 3.b's
        // dispatch + points at stage 3.c. The pre-stage-3.b
        // behaviour on this fixture was the bit-blast op-check
        // error (which mentions stage 1, the is_blastable check);
        // post-stage-3.b the error comes from the routing stub
        // (which mentions stage 3.b dispatch).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "uf"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let err = Btor2Adapter::translate(STAGE3B_BTOR2_WITH_MEMORY, &opts)
            .expect_err("UF stub must error until stage 3.c ships");
        assert!(
            err.message.contains("stage 3.b") || err.message.contains("3.b"),
            "stub error must identify stage 3.b dispatch; got: {}",
            err.message
        );
        assert!(
            err.message.contains("stage 3.c") || err.message.contains("3.c"),
            "stub error must point at stage 3.c; got: {}",
            err.message
        );
        // Confirm we did NOT reach the bit-blast path: that path's
        // error message mentions the is_blastable check / "Phase 1
        // bit-blaster", which the routing stub does not.
        assert!(
            !err.message.contains("Phase 1 bit-blaster"),
            "UF dispatch should bypass the bit-blast op-check; got: {}",
            err.message
        );
    }

    #[test]
    fn translate_uses_bit_blast_path_when_no_uf_declared() {
        // The dispatch routes to bit_blast::translate when the
        // sidecar declares havoc-only. The bit-blast path's stage
        // 1b havoc rewriter lifts the fixture end-to-end; we
        // assert the result is Ok.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let _out = Btor2Adapter::translate(STAGE3B_BTOR2_WITH_MEMORY, &opts)
            .expect("havoc path lifts end-to-end via the bit-blast destination");
    }
}
