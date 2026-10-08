//! Property tests for composition.
//!
//! - `sync_commutativity`: `compose(a, b, sync)` and `compose(b, a, sync)`
//!   produce CLTSs with the same state count and edge count. Full
//!   isomorphism is harder to assert efficiently; equal cardinalities is
//!   the necessary condition that catches gross regressions.
//!
//! Future additions (followups, not landed this sitting):
//! - Sync ⊆ Async ⊆ Superset transition containment (modulo independent-
//!   action interleavings).
//! - Idempotence: `compose(a, a, sync)` projects onto a's diagonal.

use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, StateId};
use mununu_core::composition::{CompositionOptions, CompositionSemantics, compose};
use mununu_core::test_support::RandomClts;

use proptest::prelude::*;

fn edge_count(clts: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> usize {
    (0..clts.state_count())
        .map(|s| clts.outgoing(StateId::from_index(s).unwrap()).len())
        .sum()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32),
        ..ProptestConfig::default()
    })]

    /// Synchronous composition is commutative up to state renaming. Two
    /// CLTSs are not literally isomorphic in general (state ID assignment
    /// is order-dependent on which side is `left`), but the *number* of
    /// reachable product states and the *total transition count* must
    /// agree. A regression that, say, drops half the transitions on one
    /// argument order will surface here immediately.
    #[test]
    fn sync_commutativity(
        seed_a in any::<u64>(),
        seed_b in any::<u64>(),
        states_a in 3usize..=12,
        states_b in 3usize..=12,
    ) {
        // mununu's composition checker rejects shared CONTROLLABLE actions
        // but allows shared UNCONTROLLABLE ones. Mark every label
        // uncontrollable so two random CLTSs with overlapping alphabets are
        // composable.
        let a = RandomClts::new(seed_a)
            .with_states(states_a)
            .with_density(0.30)
            .with_alphabet(3)
            .with_uncontrollable_prefix(3)
            .build();
        let b = RandomClts::new(seed_b)
            .with_states(states_b)
            .with_density(0.30)
            .with_alphabet(3)
            .with_uncontrollable_prefix(3)
            .build();

        let opts = CompositionOptions::new(CompositionSemantics::Synchronous);
        let ab = compose(&a, &b, &opts).expect("compose ab");
        let ba = compose(&b, &a, &opts).expect("compose ba");

        prop_assert_eq!(
            ab.state_count(),
            ba.state_count(),
            "sync composition state count not commutative"
        );
        prop_assert_eq!(
            edge_count(&ab),
            edge_count(&ba),
            "sync composition edge count not commutative"
        );
    }
}
