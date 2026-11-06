//! mununu#867 — the API's CEGAR routes defaulted the may-edge relation to the sampled
//! under-approximation (one canonical representative per cube), which can miss a may-successor;
//! the CLI defaults to the sound all-pairs relation. On the design below the sampled relation
//! misses the only path to `bad` and the initial cell came back definite-true: a wrong `holds`.
//!
//! Its own top-level integration test (target `cegar_may_edge_default_867`), outside `handlers.rs`
//! where the fix lives, so `make verify-repro FEATURES=api` builds the parent without it; it calls
//! only the handler the parent already has.

use axum::Json;
use mununu_core::api::handlers::btor2_cegar_handler;
use serde_json::json;

/// A 2-bit `x`, init 0, steps 0 → 2 → 3 and stays; `bad = (x == 3)` is reached at depth 2. With
/// predicates `x0 = (x == 0)` and `x3 = (x == 3)`, the cube where both are false holds 1 and 2;
/// the sampler simulates only its representative `x = 1` (a self-loop) and misses `2 → 3`.
const SKIP_TO_BAD: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n\
     5 const 1 10\n6 const 1 11\n7 sort bitvec 1\n8 eq 7 3 2\n9 eq 7 3 5\n\
     10 ite 1 9 6 3\n11 ite 1 8 5 10\n12 next 1 3 11\n13 eq 7 3 6\n14 bad 13\n";

mod cegar_may_edge_default_867 {
    use super::*;

    #[tokio::test]
    async fn the_default_cegar_request_never_proves_a_violated_box_property() {
        let request = serde_json::from_value(json!({
            "content": SKIP_TO_BAD,
            "formula": "nu X. ((! x3) && ([] X))",
            "predicates": [
                { "name": "x0", "register": "x", "value": 0 },
                { "name": "x3", "register": "x", "value": 3 },
            ],
            "max_iterations": 4,
        }))
        .expect("a well-formed request");
        let Json(out) = btor2_cegar_handler(Json(request))
            .await
            .expect("the CEGAR route runs");
        // The initial cell: `x == 0`, so `x0` true and `x3` false.
        let initial = |c: &&mununu_core::api::models::WitnessCellView| {
            c.valuation.get("x0") == Some(&true) && c.valuation.get("x3") == Some(&false)
        };
        assert!(
            out.violating_cells.iter().any(|c| initial(&c))
                || out.undecided_cells.iter().any(|c| initial(&c)),
            "the initial cell is violated at depth 2; it must be FALSE or undecided, never a \
             definite TRUE: violating {:?}, undecided {:?}, warnings {:?}",
            out.violating_cells,
            out.undecided_cells,
            out.warnings
        );
    }
}
