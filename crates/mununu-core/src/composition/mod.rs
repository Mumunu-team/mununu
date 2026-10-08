//! Composition primitives for CLTS instances.
//!
//! The implementation intentionally mirrors the semantics documented in
//! `docs/clts_spec.md`: shared alphabet elements synchronise, while independent
//! actions interleave depending on the selected composition mode. The concrete
//! logic will arrive in a later turn; for now we sketch the public API and
//! cover the expected behaviour with failing tests to guide the implementation.

mod controllability;
pub mod hide;
#[cfg(test)]
mod labels;
pub mod minimize;
mod transition;

use crate::clts::{
    Clts, CltsBuilder, CltsResult, DefaultLabelIdx, DefaultStateIdx, LabelControllability, StateId,
    Transition, TransitionModality, Tristate,
};
use crate::composition::controllability::ControllabilityChecker;
use crate::composition::transition::TransitionKeyBuilder;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;

/// Key for state variables.
type StateKey = (usize, usize);
/// Key for state pairs.
type StatePairKey = (usize, usize, usize, usize);
/// Key for label pairs.
type LabelPairKey = (usize, usize);

/// Scratch arenas that deduplicate label/variable vectors while composing.
///
/// The product builder frequently merges identical `(left, right)` states and
/// label unions; interning the resulting `Vec<String>` payloads keeps allocator
/// pressure low and allows cheap comparisons via pointer identity.
#[derive(Default)]
struct ProductStateArena {
    // Cache for interned labels.
    intern: RefCell<HashMap<Vec<String>, Rc<Vec<String>>>>,
    // Cache for interned state variables.
    state_cache: RefCell<HashMap<StateKey, Rc<Vec<String>>>>,
    // Cache for unioned state variables.
    state_union_cache: RefCell<HashMap<StatePairKey, Rc<Vec<String>>>>,
    // Cache for unioned label pairs.
    label_union_cache: RefCell<HashMap<LabelPairKey, Rc<Vec<String>>>>,
}

impl ProductStateArena {
    /// Interns a vector of labels and returns a reference-counted vector of strings.
    fn intern_labels(&self, mut labels: Vec<String>) -> Rc<Vec<String>> {
        labels.sort();
        labels.dedup();
        let mut intern = self.intern.borrow_mut();
        if let Some(existing) = intern.get(&labels) {
            return Rc::clone(existing);
        }
        let rc = Rc::new(labels.clone());
        intern.insert(labels, Rc::clone(&rc));
        rc
    }

    /// Returns the labels for a transition.
    ///
    /// This method collects all labels referenced by the transition and returns
    /// a reference-counted vector of strings. The labels are sorted and deduplicated
    /// to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `clts`: The CLTS instance.
    /// * `transition`: The transition.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn transition_labels(
        &self,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        transition: &Transition<DefaultStateIdx, DefaultLabelIdx>,
    ) -> Rc<Vec<String>> {
        let mut labels = Vec::new();
        for label_id in transition.labels() {
            if let Some(payload) = clts.label_payload(*label_id) {
                labels.extend(payload.iter().cloned());
            }
        }
        self.intern_labels(labels)
    }

    /// Returns the variables for a state.
    ///
    /// This method collects all variables referenced by the state and returns
    /// a reference-counted vector of strings. The variables are sorted and deduplicated
    /// to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `clts`: The CLTS instance.
    /// * `state`: The state.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn state_variables(
        &self,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        state: StateId<DefaultStateIdx>,
    ) -> Rc<Vec<String>> {
        let key = (clts as *const _ as usize, state.index());
        if let Some(existing) = self.state_cache.borrow().get(&key) {
            return Rc::clone(existing);
        }
        let vars = clts.state_variables(state);
        let rc = self.intern_labels(vars);
        self.state_cache.borrow_mut().insert(key, Rc::clone(&rc));
        rc
    }

    /// Merges the variables for two states.
    ///
    /// This method merges the variables for two states and returns a reference-counted
    /// vector of strings. The variables are sorted and deduplicated to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `left`: The left CLTS instance.
    /// * `l_state`: The left state.
    /// * `right`: The right CLTS instance.
    /// * `r_state`: The right state.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn merge_state_variables(
        &self,
        left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        l_state: StateId<DefaultStateIdx>,
        right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        r_state: StateId<DefaultStateIdx>,
    ) -> Rc<Vec<String>> {
        let key = (
            left as *const _ as usize,
            l_state.index(),
            right as *const _ as usize,
            r_state.index(),
        );
        if let Some(existing) = self.state_union_cache.borrow().get(&key) {
            return Rc::clone(existing);
        }
        let left_vars = self.state_variables(left, l_state);
        let right_vars = self.state_variables(right, r_state);
        let rc = self.union_from_slices(left_vars.as_ref(), right_vars.as_ref());
        self.state_union_cache
            .borrow_mut()
            .insert(key, Rc::clone(&rc));
        rc
    }

    /// Merges the labels for two transitions.
    ///
    /// This method merges the labels for two transitions and returns a reference-counted
    /// vector of strings. The labels are sorted and deduplicated to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `left`: The left transition.
    /// * `right`: The right transition.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn union_labels(&self, left: &Rc<Vec<String>>, right: &Rc<Vec<String>>) -> Rc<Vec<String>> {
        let left_ptr = Rc::as_ptr(left) as usize;
        let right_ptr = Rc::as_ptr(right) as usize;
        let key = if left_ptr <= right_ptr {
            (left_ptr, right_ptr)
        } else {
            (right_ptr, left_ptr)
        };
        if let Some(existing) = self.label_union_cache.borrow().get(&key) {
            return Rc::clone(existing);
        }
        let rc = self.union_from_slices(left.as_ref(), right.as_ref());
        self.label_union_cache
            .borrow_mut()
            .insert(key, Rc::clone(&rc));
        rc
    }

    /// Merges the labels for two transitions from slices.
    ///
    /// This method merges the labels for two transitions from slices and returns a reference-counted
    /// vector of strings. The labels are sorted and deduplicated to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `left`: The left transition.
    /// * `right`: The right transition.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn union_from_slices(&self, left: &[String], right: &[String]) -> Rc<Vec<String>> {
        if left.is_empty() && right.is_empty() {
            return self.intern_labels(Vec::new());
        }
        let mut merged = Vec::with_capacity(left.len() + right.len());
        merged.extend(left.iter().cloned());
        merged.extend(right.iter().cloned());
        self.intern_labels(merged)
    }

    /// Intersects the labels with a set of shared labels.
    ///
    /// This method intersects the labels with a set of shared labels and returns a reference-counted
    /// vector of strings. The labels are sorted and deduplicated to ensure a canonical form.
    ///
    /// # Parameters
    ///
    /// * `labels`: The labels.
    /// * `shared`: The shared labels.
    ///
    /// # Returns
    ///
    /// A reference-counted vector of strings.
    fn intersection_with_set(
        &self,
        labels: &Rc<Vec<String>>,
        shared: &BTreeSet<String>,
    ) -> Rc<Vec<String>> {
        let filtered: Vec<String> = labels
            .iter()
            .filter(|label| shared.contains(*label))
            .cloned()
            .collect();
        self.intern_labels(filtered)
    }
}

/// Builder responsible for constructing and interning product states.
#[derive(Default)]
struct ProductStateBuilder {
    state_map:
        HashMap<(StateId<DefaultStateIdx>, StateId<DefaultStateIdx>), StateId<DefaultStateIdx>>,
}

impl ProductStateBuilder {
    /// Ensures the product state `(l_state, r_state)` exists in the builder and
    /// returns its canonical identifier.
    ///
    /// # Parameters
    ///
    /// * `arena`: The product state arena.
    /// * `builder`: The CLTS builder.
    /// * `left`: The left CLTS instance.
    /// * `right`: The right CLTS instance.
    /// * `l_state`: The left state.
    /// * `r_state`: The right state.
    ///
    /// # Returns
    ///
    /// The canonical identifier of the product state.
    fn ensure_state(
        &mut self,
        arena: &ProductStateArena,
        builder: &mut CltsBuilder<DefaultStateIdx, DefaultLabelIdx>,
        left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        l_state: StateId<DefaultStateIdx>,
        r_state: StateId<DefaultStateIdx>,
    ) -> StateId<DefaultStateIdx> {
        if let Some(&id) = self.state_map.get(&(l_state, r_state)) {
            return id;
        }

        let left_label = left
            .state_name(l_state)
            .map(|name| name.to_owned())
            .unwrap_or_else(|| format!("s{}", l_state.index()));
        let right_label = right
            .state_name(r_state)
            .map(|name| name.to_owned())
            .unwrap_or_else(|| format!("s{}", r_state.index()));
        let composed_name = format!("{}|{}", left_label, right_label);

        let state_id = builder
            .state_with_name(composed_name)
            .expect("state identifier overflow");
        if left.initial_states().contains(&l_state) && right.initial_states().contains(&r_state) {
            builder.initial_state_id(state_id);
        }

        let composed_vars = arena.merge_state_variables(left, l_state, right, r_state);
        if !composed_vars.is_empty() {
            builder.with_variables_for_state(state_id, composed_vars.iter().map(|s| s.as_str()));
        }

        // R-MM-2 — Carry structured valuations + 3-valued predicate
        // labellings through composition. The native explicit-state path
        // drops these at the product boundary (it merges only variable
        // *names*), so properties on a composed design cannot reference
        // abstract values. KMTS composition must preserve them, or the
        // "composed properties become first-class" benefit is vacuous.
        //
        // Module signal namespaces are normally disjoint, so the union is
        // collision-free in practice. On a key collision we stay sound:
        // valuations take the left side deterministically (display-only
        // metadata); predicate verdicts that DISAGREE collapse to
        // KleeneBot — never fabricate a definite verdict from conflicting
        // modules. The driver (R-MM-4) module-qualifies signal names so
        // collisions do not arise on the real multi-module path.
        let l_val = left.state_valuation(l_state);
        let r_val = right.state_valuation(r_state);
        if l_val.is_some() || r_val.is_some() {
            let mut merged: BTreeMap<String, String> = BTreeMap::new();
            if let Some(rv) = r_val {
                merged.extend(rv.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if let Some(lv) = l_val {
                // Left wins on collision.
                merged.extend(lv.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if !merged.is_empty() {
                builder.with_valuation_for_state(state_id, merged);
            }
        }

        let l_preds = left.state_3valued_predicate_entries(l_state);
        let r_preds = right.state_3valued_predicate_entries(r_state);
        if !l_preds.is_empty() || !r_preds.is_empty() {
            let mut merged: BTreeMap<String, Tristate> = BTreeMap::new();
            for (name, verdict) in r_preds {
                merged.insert(name.to_string(), verdict);
            }
            for (name, verdict) in l_preds {
                merged
                    .entry(name.to_string())
                    .and_modify(|existing| {
                        if *existing != verdict {
                            *existing = Tristate::KleeneBot;
                        }
                    })
                    .or_insert(verdict);
            }
            for (name, verdict) in merged {
                builder.with_3valued_predicate(state_id, name, verdict);
            }
        }

        self.state_map.insert((l_state, r_state), state_id);
        state_id
    }
}

/// Returns `true` when two sorted label vectors share at least one element.
/// The vectors emitted by the arena are already deduplicated, so a linear merge
/// check keeps the hot path cache-friendly.
///
/// # Parameters
///
/// * `left`: The left labels.
/// * `right`: The right labels.
///
/// # Returns
///
/// `true` when two sorted label vectors share at least one element.
fn labels_have_intersection(left: &[String], right: &[String]) -> bool {
    let mut i = 0;
    let mut j = 0;
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

// (controllability helpers moved to `composition::controllability`)

/// Test-only wrapper around the shared transition-label collector.
#[cfg(test)]
fn transition_label_set(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    transition: &Transition<DefaultStateIdx, DefaultLabelIdx>,
) -> BTreeSet<String> {
    crate::composition::labels::collect_transition_labels(clts, transition)
}

/// Computes the full alphabet referenced by the provided CLTS.
///
/// This delegates to the CLTS-level `alphabet()` helper, which already
/// aggregates symbols across controllable, internal, and uncontrollable
/// alphabets. The result is then normalised into a `BTreeSet` for callers
/// that rely on deterministic ordering.
/// mununu#570 — a multi-label transition that can never fire under this composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadCompoundAction {
    /// The compound action's full label set, as written.
    pub labels: Vec<String>,
    /// Its projection onto the shared alphabet — the set a partner would have to match exactly.
    pub shared_projection: Vec<String>,
}

/// mununu#570 — find compound actions that no partner transition can match.
///
/// **Why this is worth a static check.** A multi-label transition is a synchronisation vector: it
/// pairs only with a partner whose shared-alphabet projection is SET-IDENTICAL. An author who
/// writes `on label a, label b` meaning "either may happen" gets an action that can never fire,
/// and the automaton freezes. Measured on a real 7-automaton model: 19 such self-loops reduced
/// the composition to ONE reachable state, and a safety property "held" vacuously.
///
/// The existing 1-reachable-state warning catches only the total collapse. **This catches the
/// partial case** — where the state space merely shrinks and the verdicts are still wrong with
/// nothing saying so — which is the more dangerous of the two.
///
/// Pure and cheap: two passes over the transition lists, no product construction. A compound whose
/// shared projection is EMPTY is not reported — its labels are local to this automaton, so it
/// interleaves freely and is perfectly fine.
pub fn dead_compound_actions(
    left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    shared_alphabet: &BTreeSet<String>,
) -> Vec<DeadCompoundAction> {
    let project = |clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
                   t: &Transition<DefaultStateIdx, DefaultLabelIdx>|
     -> Vec<String> {
        let mut v: Vec<String> = t
            .labels()
            .iter()
            .filter_map(|id| clts.label_payload(*id))
            .flat_map(|p| p.iter().cloned())
            .filter(|l| shared_alphabet.contains(l))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let all_labels = |clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
                      t: &Transition<DefaultStateIdx, DefaultLabelIdx>|
     -> Vec<String> {
        let mut v: Vec<String> = t
            .labels()
            .iter()
            .filter_map(|id| clts.label_payload(*id))
            .flat_map(|p| p.iter().cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    };

    // Every projection the partner can offer, anywhere in the automaton.
    let matchable = |clts: &Clts<DefaultStateIdx, DefaultLabelIdx>| -> BTreeSet<Vec<String>> {
        let mut out = BTreeSet::new();
        for s in clts.states() {
            for t in clts.outgoing(s).iter() {
                out.insert(project(clts, t));
            }
        }
        out
    };

    let mut found = Vec::new();
    for (this, other) in [(left, right), (right, left)] {
        let partner = matchable(other);
        let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
        for s in this.states() {
            for t in this.outgoing(s).iter() {
                let labels = all_labels(this, t);
                if labels.len() < 2 {
                    continue; // only a COMPOUND action can be unsatisfiable this way
                }
                let proj = project(this, t);
                // An empty projection means every label is local: it interleaves, never blocked.
                if proj.is_empty() || partner.contains(&proj) || !seen.insert(proj.clone()) {
                    continue;
                }
                found.push(DeadCompoundAction {
                    labels,
                    shared_projection: proj,
                });
            }
        }
    }
    found
}

fn collect_alphabet(clts: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> BTreeSet<String> {
    clts.alphabet().into_iter().collect()
}

/// Registers a product state if it has not been seen and pushes it onto the BFS
/// queue for later processing.
///
/// # Parameters
///
/// * `product`: The product state builder.
/// * `arena`: The product state arena.
/// * `builder`: The CLTS builder.
/// * `left`: The left CLTS instance.
/// * `right`: The right CLTS instance.
/// * `l_state`: The left state.
/// * `r_state`: The right state.
/// * `queue`: The queue.
/// * `discovered`: The discovered states.
///
/// # Returns
///
/// The canonical identifier of the product state.
#[allow(clippy::too_many_arguments)]
fn enqueue_state(
    product: &mut ProductStateBuilder,
    arena: &ProductStateArena,
    builder: &mut CltsBuilder<DefaultStateIdx, DefaultLabelIdx>,
    left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    l_state: StateId<DefaultStateIdx>,
    r_state: StateId<DefaultStateIdx>,
    queue: &mut VecDeque<(StateId<DefaultStateIdx>, StateId<DefaultStateIdx>)>,
    discovered: &mut HashSet<(StateId<DefaultStateIdx>, StateId<DefaultStateIdx>)>,
) -> StateId<DefaultStateIdx> {
    let id = product.ensure_state(arena, builder, left, right, l_state, r_state);
    if discovered.insert((l_state, r_state)) {
        queue.push_back((l_state, r_state));
    }
    id
}

/// Composition strategies supported by the CLTS product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionSemantics {
    /// Shared alphabet elements must fire together; independent actions are
    /// collapsed into a single joint step.
    Synchronous,
    /// Shared alphabet elements fire together; independent actions may
    /// interleave in either order as well as proceed jointly.
    ///
    /// SOUNDNESS: over-approx — independent labels can fire freely without
    /// fairness constraints. One side can idle indefinitely while the other
    /// progresses. Sound for safety (extra interleavings are conservative),
    /// unsound for liveness (apparent progress may not exist in reality if
    /// fairness is assumed but not enforced).
    Asynchronous,
    /// Superset semantics combine the union step with both permutations even
    /// when actions are independent.
    Superset,
}

/// Placeholder options structure in case additional flags are required later.
#[derive(Debug, Clone)]
pub struct CompositionOptions {
    pub semantics: CompositionSemantics,
    /// mununu#589 — the synchronisation vectors of the WHOLE composition (every multi-label
    /// transition on any member), so a pairwise fold can give two partners of one vector their
    /// joint step before the vector's owner is folded in. See [`synchronisation_vectors`] and
    /// the rendezvous rule in [`compose`].
    pub sync_vectors: Vec<BTreeSet<String>>,
}

impl CompositionOptions {
    /// Creates a new set of composition options bound to the requested
    /// semantics.
    pub fn new(semantics: CompositionSemantics) -> Self {
        Self {
            semantics,
            sync_vectors: Vec::new(),
        }
    }

    /// mununu#589 — the composition's synchronisation vectors, collected over every member it
    /// will fold (see [`synchronisation_vectors`]).
    pub fn with_sync_vectors(mut self, vectors: Vec<BTreeSet<String>>) -> Self {
        self.sync_vectors = vectors;
        self
    }
}

/// mununu#589 — the synchronisation vectors a CLTS carries: the label SET of every transition
/// with two or more labels. A composition collects these over all its members before folding
/// them pairwise, so [`compose`] can let two partners of a vector take their joint step even
/// when the vector's owner comes later in the fold.
pub fn synchronisation_vectors(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
) -> BTreeSet<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for s in clts.states() {
        for t in clts.outgoing(s).iter() {
            let labels: BTreeSet<String> = t
                .labels()
                .iter()
                .filter_map(|id| clts.label_payload(*id))
                .flat_map(|p| p.iter().cloned())
                .collect();
            if labels.len() >= 2 {
                out.insert(labels);
            }
        }
    }
    out
}

/// mununu#589 — is `union` (two edges' labels, no label in common) a piece of one of the
/// composition's synchronisation vectors? Then the two edges are partners of that vector and get
/// a joint step in the product, besides their interleavings.
fn completes_a_vector_piece(vectors: &[BTreeSet<String>], union: &[String]) -> bool {
    vectors.iter().any(|v| union.iter().all(|l| v.contains(l)))
}

/// Compose two CLTS instances according to the requested semantics.
///
/// The resulting CLTS contains only reachable product states, mirroring the
/// semantics set out in `docs/clts_spec.md`.
///
/// # Parameters
///
/// * `left`: The left CLTS instance.
/// * `right`: The right CLTS instance.
/// * `options`: The composition options.
///
/// # Returns
///
/// The composed CLTS instance.
///
/// # Errors
///
/// Returns an error if the composition is invalid.
pub fn compose(
    left: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    right: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    options: &CompositionOptions,
) -> CltsResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
    // Validate mutual exclusivity of controllable and internal actions.
    ControllabilityChecker::validate_composition(left, right)?;

    let left_alphabet = collect_alphabet(left);
    let right_alphabet = collect_alphabet(right);
    let shared_alphabet: BTreeSet<String> = left_alphabet
        .intersection(&right_alphabet)
        .cloned()
        .collect();

    // mununu#592 — a label declared controllable in one member and merely USED by the other
    // composes as UNCONTROLLABLE: the composed step carries the partner's uncontrollable reading
    // (`composed_has_uncontrollable_labels`), so a controller that could decline `go` in the
    // member alone cannot in the product, and a realizability verdict flips with no message.
    // That is the sound reading (the partner's step is not the controller's to withhold), but
    // it must be loud; the modelling consequence is to keep a controllable decision and its
    // effect in one automaton, or to expose the effect through a follow-up uncontrollable label.
    for (this, other, this_side, other_side) in [
        (left, right, "left", "right"),
        (right, left, "right", "left"),
    ] {
        let this_ctrl =
            ControllabilityChecker::collect_label_names(this, this.controllable_alphabet());
        let other_alphabet = collect_alphabet(other);
        let mut demoted: Vec<&String> = this_ctrl
            .iter()
            .filter(|l| other_alphabet.contains(*l))
            .collect();
        demoted.sort();
        for label in demoted {
            tracing::warn!(
                "[mununu#592] label `{label}` is controllable in the {this_side} member but the \
                 {other_side} member carries it too, so in the composition it is UNCONTROLLABLE: \
                 a controller cannot withhold a step the partner takes. Keep a controllable \
                 decision and its effect in one automaton, or expose the effect through a \
                 follow-up uncontrollable label."
            );
        }
    }

    // mununu#570 — a compound action no partner transition can match can NEVER fire, and its
    // automaton is frozen at that state. Warn at composition time; this is a static check over
    // the two label sets, with no state-space exploration.
    for side in dead_compound_actions(left, right, &shared_alphabet) {
        tracing::warn!(
            "[mununu#570] compound action {{{}}} can never fire: no transition in the partner \
             automaton projects to the same set over the shared alphabet {{{}}}. A multi-label \
             transition is a SYNCHRONISATION VECTOR — it fires on the whole set together, not on \
             any one label. If these are independent alternatives, write them as separate \
             single-label transitions.",
            side.labels.join(", "),
            side.shared_projection.join(", "),
        );
    }
    let mut product_builder = ProductStateBuilder::default();
    let mut builder = Clts::builder();
    let mut pending_transitions = HashSet::new();
    let mut queue = VecDeque::new();
    let mut discovered = HashSet::new();
    let mut visited = HashSet::new();

    let left_initials: Vec<_> = left.initial_states().iter().copied().collect();
    let right_initials: Vec<_> = right.initial_states().iter().copied().collect();

    // Coverage Status: Covered by test `composition_with_empty_initial_states`
    if left_initials.is_empty() || right_initials.is_empty() {
        // If either side has no initial states, return an empty CLTS.
        return Clts::builder().build();
    }

    let arena = ProductStateArena::default();

    for &l_init in &left_initials {
        for &r_init in &right_initials {
            enqueue_state(
                &mut product_builder,
                &arena,
                &mut builder,
                left,
                right,
                l_init,
                r_init,
                &mut queue,
                &mut discovered,
            );
        }
    }

    // Main composition loop.
    while let Some((l_state, r_state)) = queue.pop_front() {
        if !visited.insert((l_state, r_state)) {
            continue;
        }

        let composed_id =
            product_builder.ensure_state(&arena, &mut builder, left, right, l_state, r_state);

        let left_out = left.outgoing(l_state);
        let right_out = right.outgoing(r_state);

        for lt in left_out.iter() {
            let left_labels = arena.transition_labels(left, lt);
            let left_shared_global = arena.intersection_with_set(&left_labels, &shared_alphabet);
            let mut matched = false;
            let mut left_perm_state: Option<StateId<DefaultStateIdx>> = None;
            let mut right_perm_targets: HashSet<StateId<DefaultStateIdx>> = HashSet::new();

            for rt in right_out.iter() {
                let right_labels = arena.transition_labels(right, rt);
                let right_shared_global =
                    arena.intersection_with_set(&right_labels, &shared_alphabet);
                if left_shared_global != right_shared_global {
                    continue;
                }

                // Fast path: intersect using the arena's sorted vectors so we avoid
                // allocating temporary `BTreeSet`s for every pair of transitions.
                let shared_actual_empty =
                    !labels_have_intersection(left_labels.as_ref(), right_labels.as_ref());
                let union_labels_set = arena.union_labels(&left_labels, &right_labels);

                // R.1 — KMTS modality merge per §6.5. Synchronising
                // steps use the pointwise meet `lt ⊗ rt`; interleaving
                // steps preserve the single contributing side's modality.
                //
                // R.4.5 close-out — `merge` returns a placeholder
                // `MustHyperOnly(empty)` whenever the synchronizing
                // pair includes a hyper-must; the composition layer
                // populates the actual hyper-target set as the
                // Cartesian product of both sides' must-target sets
                // (each `(l_target, r_target)` projected through
                // `enqueue_state`). The principal target (the first
                // element by convention) matches the existing
                // `enqueue_state(lt.target(), rt.target())` call so
                // single-target queries keep working.
                let sync_modality = lt.modality().merge(rt.modality());
                match options.semantics {
                    CompositionSemantics::Synchronous => {
                        let union_target = enqueue_state(
                            &mut product_builder,
                            &arena,
                            &mut builder,
                            left,
                            right,
                            lt.target(),
                            rt.target(),
                            &mut queue,
                            &mut discovered,
                        );
                        let has_uncontrollable =
                            ControllabilityChecker::composed_has_uncontrollable_labels(
                                lt, rt, left, right,
                            );

                        // R.4.5 close-out — populate the Cartesian-
                        // product hyper-target set when the merged
                        // modality is the placeholder `MustHyperOnly(empty)`.
                        // For `Sharp` / `MayOnly` results the merge
                        // already produced the final shape; no work.
                        let final_modality =
                            if matches!(sync_modality, TransitionModality::MustHyperOnly(_)) {
                                let lt_targets = lt.modality().must_target_set(lt.target());
                                let rt_targets = rt.modality().must_target_set(rt.target());
                                // Cartesian product; the principal target
                                // (union_target) goes first to preserve
                                // the §clts MustHyperOnly convention that
                                // `targets[0] == transition.target()`.
                                let mut product: smallvec::SmallVec<[StateId<DefaultStateIdx>; 4]> =
                                    smallvec::smallvec![union_target];
                                for &l_tgt in &lt_targets {
                                    for &r_tgt in &rt_targets {
                                        if l_tgt == lt.target() && r_tgt == rt.target() {
                                            // Already added as principal.
                                            continue;
                                        }
                                        let product_id = enqueue_state(
                                            &mut product_builder,
                                            &arena,
                                            &mut builder,
                                            left,
                                            right,
                                            l_tgt,
                                            r_tgt,
                                            &mut queue,
                                            &mut discovered,
                                        );
                                        product.push(product_id);
                                    }
                                }
                                TransitionModality::must_hyper(product)
                            } else {
                                sync_modality
                            };

                        pending_transitions.insert(TransitionKeyBuilder::create_key(
                            composed_id,
                            union_target,
                            Rc::clone(&union_labels_set),
                            has_uncontrollable,
                            final_modality,
                        ));
                        matched = true;
                    }
                    CompositionSemantics::Asynchronous => {
                        if shared_actual_empty {
                            // mununu#589 — the n-ary rendezvous. Two edges with no label in
                            // common interleave; but if together they are a piece of a
                            // synchronisation vector the composition carries, they are two
                            // PARTNERS of that vector and may also step JOINTLY — the step the
                            // vector's owner will synchronise with when it is folded in later.
                            // Without this, an owner folded after its partners finds no joint
                            // `{a, b}` step to pair with and the vector never fires; whether a
                            // joint action could happen then depended on the members' fold
                            // order (their names, on the `verify` path). The joint step carries
                            // labels the owner shares, so it can only survive the owner's fold
                            // synchronised with the owner's vector; the interleavings below are
                            // kept, as before, for the owner's single-label edges.
                            if !options.sync_vectors.is_empty()
                                && completes_a_vector_piece(
                                    &options.sync_vectors,
                                    union_labels_set.as_slice(),
                                )
                            {
                                let joint_target = enqueue_state(
                                    &mut product_builder,
                                    &arena,
                                    &mut builder,
                                    left,
                                    right,
                                    lt.target(),
                                    rt.target(),
                                    &mut queue,
                                    &mut discovered,
                                );
                                let has_uncontrollable =
                                    ControllabilityChecker::composed_has_uncontrollable_labels(
                                        lt, rt, left, right,
                                    );
                                pending_transitions.insert(TransitionKeyBuilder::create_key(
                                    composed_id,
                                    joint_target,
                                    Rc::clone(&union_labels_set),
                                    has_uncontrollable,
                                    sync_modality.clone(),
                                ));
                            }
                            let left_target = left_perm_state.unwrap_or_else(|| {
                                let name = enqueue_state(
                                    &mut product_builder,
                                    &arena,
                                    &mut builder,
                                    left,
                                    right,
                                    lt.target(),
                                    r_state,
                                    &mut queue,
                                    &mut discovered,
                                );
                                left_perm_state = Some(name);
                                name
                            });
                            pending_transitions.insert(TransitionKeyBuilder::create_key(
                                composed_id,
                                left_target,
                                Rc::clone(&left_labels),
                                lt.is_uncontrollable(left),
                                lt.modality().clone(),
                            ));

                            if right_perm_targets.insert(rt.target()) {
                                let right_target = enqueue_state(
                                    &mut product_builder,
                                    &arena,
                                    &mut builder,
                                    left,
                                    right,
                                    l_state,
                                    rt.target(),
                                    &mut queue,
                                    &mut discovered,
                                );
                                pending_transitions.insert(TransitionKeyBuilder::create_key(
                                    composed_id,
                                    right_target,
                                    Rc::clone(&right_labels),
                                    rt.is_uncontrollable(right),
                                    rt.modality().clone(),
                                ));
                            }
                            matched = true;
                        } else {
                            let union_target = enqueue_state(
                                &mut product_builder,
                                &arena,
                                &mut builder,
                                left,
                                right,
                                lt.target(),
                                rt.target(),
                                &mut queue,
                                &mut discovered,
                            );
                            let has_uncontrollable =
                                ControllabilityChecker::composed_has_uncontrollable_labels(
                                    lt, rt, left, right,
                                );
                            pending_transitions.insert(TransitionKeyBuilder::create_key(
                                composed_id,
                                union_target,
                                Rc::clone(&union_labels_set),
                                has_uncontrollable,
                                sync_modality,
                            ));
                            matched = true;
                        }
                    }
                    CompositionSemantics::Superset => {
                        // Coverage Status: Superset composition is tested, but edge cases need more coverage
                        // TODO: Add tests for superset composition with various label combinations
                        let union_target = enqueue_state(
                            &mut product_builder,
                            &arena,
                            &mut builder,
                            left,
                            right,
                            lt.target(),
                            rt.target(),
                            &mut queue,
                            &mut discovered,
                        );
                        let has_uncontrollable =
                            ControllabilityChecker::composed_has_uncontrollable_labels(
                                lt, rt, left, right,
                            );
                        pending_transitions.insert(TransitionKeyBuilder::create_key(
                            composed_id,
                            union_target,
                            Rc::clone(&union_labels_set),
                            has_uncontrollable,
                            sync_modality,
                        ));
                        if shared_actual_empty {
                            let left_target = left_perm_state.unwrap_or_else(|| {
                                let name = enqueue_state(
                                    &mut product_builder,
                                    &arena,
                                    &mut builder,
                                    left,
                                    right,
                                    lt.target(),
                                    r_state,
                                    &mut queue,
                                    &mut discovered,
                                );
                                left_perm_state = Some(name);
                                name
                            });
                            pending_transitions.insert(TransitionKeyBuilder::create_key(
                                composed_id,
                                left_target,
                                Rc::clone(&left_labels),
                                lt.is_uncontrollable(left),
                                lt.modality().clone(),
                            ));

                            if right_perm_targets.insert(rt.target()) {
                                let right_target = enqueue_state(
                                    &mut product_builder,
                                    &arena,
                                    &mut builder,
                                    left,
                                    right,
                                    l_state,
                                    rt.target(),
                                    &mut queue,
                                    &mut discovered,
                                );
                                pending_transitions.insert(TransitionKeyBuilder::create_key(
                                    composed_id,
                                    right_target,
                                    Rc::clone(&right_labels),
                                    rt.is_uncontrollable(right),
                                    rt.modality().clone(),
                                ));
                            }
                        }
                        matched = true;
                    }
                }
            }

            if matches!(
                options.semantics,
                CompositionSemantics::Asynchronous | CompositionSemantics::Superset
            ) && !matched
                && left_shared_global.is_empty()
            {
                let left_target = left_perm_state.unwrap_or_else(|| {
                    enqueue_state(
                        &mut product_builder,
                        &arena,
                        &mut builder,
                        left,
                        right,
                        lt.target(),
                        r_state,
                        &mut queue,
                        &mut discovered,
                    )
                });
                pending_transitions.insert(TransitionKeyBuilder::create_key(
                    composed_id,
                    left_target,
                    Rc::clone(&left_labels),
                    lt.is_uncontrollable(left),
                    lt.modality().clone(),
                ));
            }
        }

        if matches!(
            options.semantics,
            CompositionSemantics::Asynchronous | CompositionSemantics::Superset
        ) {
            for rt in right_out.iter() {
                let right_labels = arena.transition_labels(right, rt);
                let right_shared_global =
                    arena.intersection_with_set(&right_labels, &shared_alphabet);
                if right_shared_global.is_empty() {
                    let right_target = enqueue_state(
                        &mut product_builder,
                        &arena,
                        &mut builder,
                        left,
                        right,
                        l_state,
                        rt.target(),
                        &mut queue,
                        &mut discovered,
                    );
                    pending_transitions.insert(TransitionKeyBuilder::create_key(
                        composed_id,
                        right_target,
                        Rc::clone(&right_labels),
                        rt.is_uncontrollable(right),
                        rt.modality().clone(),
                    ));
                }
            }
        }
    }

    let mut pending_vec: Vec<_> = pending_transitions.into_iter().collect();
    pending_vec.sort_by(|a, b| {
        (
            a.source.index(),
            a.target.index(),
            a.labels.as_slice(),
            a.has_uncontrollable_labels,
            &a.modality,
        )
            .cmp(&(
                b.source.index(),
                b.target.index(),
                b.labels.as_slice(),
                b.has_uncontrollable_labels,
                &b.modality,
            ))
    });

    for key in pending_vec {
        let label_id = {
            let labels = builder.labels();
            labels.intern(key.labels.iter().map(|s| s.as_str()))?
        };
        // Set label controllability if the transition has uncontrollable labels
        if key.has_uncontrollable_labels {
            builder.set_label_controllability(label_id, LabelControllability::Uncontrollable);
        }
        // R.1 — Use modality-aware transition builder. For
        // Sharp-everywhere KMTSes (every legacy adapter today), the
        // modality is always Sharp and this is identical to
        // `transition_ids`. KMTS-aware adapters propagate `MayOnly`
        // through composition per §6.5's per-axis-conjunction merge.
        builder.transition_ids_with_modality(key.source, &[label_id], key.target, key.modality);
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clts::TransitionModality;
    use std::collections::{BTreeSet, HashMap};

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn singleton(
        label: &[&str],
        vars: &[&str],
    ) -> TestResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
        let mut builder = Clts::builder();
        builder.state("s0");
        builder.initial("s0");
        builder.state("s1");
        if !label.is_empty() {
            let id = builder.labels().intern(label.to_vec())?;
            builder.transition("s0", &[id], "s1");
        }
        if !vars.is_empty() {
            builder.with_variables("s1", vars.to_vec());
        }
        builder.build().map_err(|err| err.into())
    }

    fn outgoing_labels(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        state: &str,
    ) -> TestResult<Vec<(String, BTreeSet<String>)>> {
        let state_id = clts.state_id(state)?;
        Ok(clts
            .outgoing(state_id)
            .iter()
            .map(|transition| {
                let target = clts
                    .state_name(transition.target())
                    .unwrap_or("?")
                    .to_owned();
                let labels = transition_label_set(clts, transition);
                (target, labels)
            })
            .collect())
    }

    fn label_set(symbols: &[&str]) -> BTreeSet<String> {
        symbols.iter().map(|s| s.to_string()).collect()
    }

    /// mununu#570 — the detector reproduces the filed repro's row 1 exactly.
    ///
    /// `Multi` carries ONE compound action `{a,b}`; the partner offers `{a}` and `{b}` as
    /// separate edges. Neither projects to `{a,b}`, so the compound can never fire and `Multi`
    /// freezes — which is how a real 7-automaton model collapsed to one reachable state with a
    /// safety property holding vacuously.
    #[test]
    fn dead_compound_action_is_detected_when_no_partner_matches_the_set() -> TestResult {
        let mut lb = Clts::builder();
        let a = lb.labels().intern(["a"])?;
        let b = lb.labels().intern(["b"])?;
        lb.set_label_controllability(a, crate::clts::LabelControllability::Uncontrollable);
        lb.set_label_controllability(b, crate::clts::LabelControllability::Uncontrollable);
        lb.state("S0").initial("S0");
        lb.state("S1");
        lb.transition("S0", &[a, b], "S1"); // ONE action: {a,b}
        let left = lb.build()?;

        let mut rb = Clts::builder();
        let ra = rb.labels().intern(["a"])?;
        let rbl = rb.labels().intern(["b"])?;
        rb.set_label_controllability(ra, crate::clts::LabelControllability::Uncontrollable);
        rb.set_label_controllability(rbl, crate::clts::LabelControllability::Uncontrollable);
        rb.state("U0").initial("U0");
        rb.transition("U0", &[ra], "U0"); // two SEPARATE edges
        rb.transition("U0", &[rbl], "U0");
        let right = rb.build()?;

        let shared = label_set(&["a", "b"]);
        let dead = dead_compound_actions(&left, &right, &shared);
        assert_eq!(
            dead.len(),
            1,
            "the compound {{a,b}} is unmatchable and must be reported: {dead:?}"
        );
        assert_eq!(dead[0].labels, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            dead[0].shared_projection,
            vec!["a".to_string(), "b".to_string()]
        );
        Ok(())
    }

    /// mununu#570 — the three rows that DO fire must not be reported. A false positive here would
    /// train readers to ignore the warning, which is worse than not having it.
    #[test]
    fn a_matchable_or_local_compound_is_not_reported() -> TestResult {
        let build_multi = || -> CltsResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
            let mut lb = Clts::builder();
            let a = lb.labels().intern(["a"])?;
            let b = lb.labels().intern(["b"])?;
            lb.set_label_controllability(a, crate::clts::LabelControllability::Uncontrollable);
            lb.set_label_controllability(b, crate::clts::LabelControllability::Uncontrollable);
            lb.state("S0").initial("S0");
            lb.state("S1");
            lb.transition("S0", &[a, b], "S1");
            lb.build()
        };

        // Row 4 — the partner offers the SAME compound. Matchable.
        let mut rb = Clts::builder();
        let ra = rb.labels().intern(["a"])?;
        let rbl = rb.labels().intern(["b"])?;
        rb.set_label_controllability(ra, crate::clts::LabelControllability::Uncontrollable);
        rb.set_label_controllability(rbl, crate::clts::LabelControllability::Uncontrollable);
        rb.state("U0").initial("U0");
        rb.transition("U0", &[ra, rbl], "U0");
        let compound_partner = rb.build()?;
        assert!(
            dead_compound_actions(&build_multi()?, &compound_partner, &label_set(&["a", "b"]))
                .is_empty(),
            "row 4 fires — a set-identical partner must not be flagged"
        );

        // Row 2 — the partner only knows `b`, so `a` is LOCAL to Multi and the projection is
        // `{b}`, which the partner offers. Fires, and must not be flagged.
        let mut rb2 = Clts::builder();
        let rb2b = rb2.labels().intern(["b"])?;
        rb2.set_label_controllability(rb2b, crate::clts::LabelControllability::Uncontrollable);
        rb2.state("U0").initial("U0");
        rb2.transition("U0", &[rb2b], "U0");
        let b_only = rb2.build()?;
        assert!(
            dead_compound_actions(&build_multi()?, &b_only, &label_set(&["b"])).is_empty(),
            "row 2 fires — a partial projection the partner offers must not be flagged"
        );
        Ok(())
    }

    #[test]
    fn synchronous_composition_merges_shared_alphabet() -> TestResult {
        // Use different controllable labels for each automaton (mutual exclusivity requirement)
        // Shared labels for synchronization should be uncontrollable
        let mut left_builder = Clts::builder();
        let left_req = left_builder.labels().intern(["req"])?;
        left_builder
            .set_label_controllability(left_req, crate::clts::LabelControllability::Uncontrollable);
        left_builder.state("s0").initial("s0");
        left_builder.state("s1");
        left_builder.transition("s0", &[left_req], "s1");
        let left = left_builder.build()?;

        let mut right_builder = Clts::builder();
        let right_req = right_builder.labels().intern(["req"])?;
        let right_ack = right_builder.labels().intern(["ack"])?;
        right_builder.set_label_controllability(
            right_req,
            crate::clts::LabelControllability::Uncontrollable,
        );
        right_builder.state("s0").initial("s0");
        right_builder.state("s1");
        right_builder.transition("s0", &[right_req, right_ack], "s1");
        let right = right_builder.build()?;

        let options = CompositionOptions::new(CompositionSemantics::Synchronous);

        let product = compose(&left, &right, &options)?;
        let arcs = outgoing_labels(&product, "s0|s0")?;
        assert_eq!(arcs.len(), 1);
        assert_eq!(arcs[0].0, "s1|s1");
        assert_eq!(arcs[0].1, label_set(&["ack", "req"]));
        Ok(())
    }

    #[test]
    fn asynchronous_composition_interleaves_independent_actions() -> TestResult {
        // Use different controllable labels for each automaton (mutual exclusivity requirement)
        let left = singleton(&["produce"], &[])?;
        let right = singleton(&["consume"], &[])?;
        let options = CompositionOptions::new(CompositionSemantics::Asynchronous);

        let product = compose(&left, &right, &options)?;
        let arcs = outgoing_labels(&product, "s0|s0")?;
        let map: HashMap<_, _> = arcs.into_iter().collect();
        assert_eq!(map.len(), 2);
        assert_eq!(map["s1|s0"], label_set(&["produce"]));
        assert_eq!(map["s0|s1"], label_set(&["consume"]));
        Ok(())
    }

    #[test]
    fn superset_composition_includes_union_and_permutations() -> TestResult {
        // Use different controllable labels for each automaton (mutual exclusivity requirement)
        let left = singleton(&["a"], &["x"])?;
        let right = singleton(&["b"], &["y"])?;
        let options = CompositionOptions::new(CompositionSemantics::Superset);

        let product = compose(&left, &right, &options)?;
        let arcs = outgoing_labels(&product, "s0|s0")?;
        let map: HashMap<_, _> = arcs.into_iter().collect();
        assert_eq!(map.len(), 3);
        assert_eq!(map["s1|s0"], label_set(&["a"]));
        assert_eq!(map["s0|s1"], label_set(&["b"]));
        assert_eq!(map["s1|s1"], label_set(&["a", "b"]));

        let vars = {
            let id = product.state_id("s1|s1")?;
            let mut collected = product.state_variables(id);
            collected.sort();
            collected
        };
        assert_eq!(vars, vec!["x".to_string(), "y".to_string()]);
        Ok(())
    }

    #[test]
    fn composition_preserves_controllability() -> TestResult {
        // Test that controllability information is preserved during composition
        let mut left_builder = Clts::builder();
        left_builder.state("s0");
        left_builder.initial("s0");
        left_builder.state("s1");
        let s0 = left_builder.state_id_or_insert("s0").unwrap();
        let s1 = left_builder.state_id_or_insert("s1").unwrap();
        let input_label = left_builder.labels().intern(["input"])?;
        left_builder.set_label_controllability(input_label, LabelControllability::Uncontrollable);
        left_builder.transition_ids(s0, &[input_label], s1);
        let left = left_builder.build()?;

        let mut right_builder = Clts::builder();
        right_builder.state("t0");
        right_builder.initial("t0");
        right_builder.state("t1");
        let t0 = right_builder.state_id_or_insert("t0").unwrap();
        let t1 = right_builder.state_id_or_insert("t1").unwrap();
        let output_label = right_builder.labels().intern(["output"])?;
        right_builder.transition_ids(t0, &[output_label], t1);
        let right = right_builder.build()?;

        let options = CompositionOptions::new(CompositionSemantics::Asynchronous);
        let composed = compose(&left, &right, &options)?;

        // Find transitions in the composed CLTS
        let s0_t0 = composed.state_id("s0|t0")?;
        let transitions: Vec<_> = composed.outgoing(s0_t0).iter().collect();

        // The composed CLTS should have transitions, and independent transitions
        // should preserve their controllability
        let has_uncontrollable = transitions.iter().any(|t| t.is_uncontrollable(&composed));
        let has_controllable = transitions.iter().any(|t| t.is_controllable(&composed));

        // Since left has uncontrollable and right has controllable, both should be present
        // in asynchronous composition (they interleave independently)
        assert!(
            has_uncontrollable && has_controllable,
            "Composed CLTS should preserve controllability: found uncontrollable={}, controllable={}",
            has_uncontrollable,
            has_controllable
        );

        Ok(())
    }

    #[test]
    fn composition_skips_unreachable_pairs() -> TestResult {
        let mut left = Clts::builder();
        left.state("s0");
        left.initial("s0");
        left.state("s1");
        left.state("dead");
        let label = left.labels().intern(["tick"])?;
        left.transition("s0", &[label], "s1");
        let left = left.build()?;

        let right = singleton(&[], &[])?;
        let options = CompositionOptions::new(CompositionSemantics::Synchronous);

        let product = compose(&left, &right, &options)?;
        let names: Vec<_> = product
            .states()
            .map(|state| product.state_name(state).unwrap_or("?").to_owned())
            .collect();
        assert!(!names.iter().any(|name| name.contains("dead")));
        Ok(())
    }

    #[test]
    fn composition_with_empty_initial_states() -> TestResult {
        // Test composition when one CLTS has no initial states (line 358)
        let mut left = Clts::builder();
        left.state("s0"); // No initial state set
        let left_clts = left.build()?;

        let mut right = Clts::builder();
        right.state("t0").initial("t0");
        let right_clts = right.build()?;

        let options = CompositionOptions::new(CompositionSemantics::Synchronous);

        let composed = compose(&left_clts, &right_clts, &options)?;
        // Should return empty CLTS when one side has no initial states
        assert_eq!(composed.state_count(), 0);

        // Test reverse case
        let mut left2 = Clts::builder();
        left2.state("s0").initial("s0");
        let left_clts2 = left2.build()?;

        let mut right2 = Clts::builder();
        right2.state("t0"); // No initial state
        let right_clts2 = right2.build()?;

        let composed2 = compose(&left_clts2, &right_clts2, &options)?;
        assert_eq!(composed2.state_count(), 0);

        Ok(())
    }

    #[test]
    fn superset_composition_handles_label_permutations() -> TestResult {
        // Test superset composition edge cases (lines 486-494, 496-501, 503)
        let mut left = Clts::builder();
        let label_a = left.labels().intern(["a"])?;
        let label_b = left.labels().intern(["b"])?;
        left.state("s0").initial("s0");
        left.state("s1");
        left.transition("s0", &[label_a], "s1");
        left.transition("s0", &[label_b], "s1");
        let left_clts = left.build()?;

        let mut right = Clts::builder();
        let label_c = right.labels().intern(["c"])?;
        right.state("t0").initial("t0");
        right.state("t1");
        right.transition("t0", &[label_c], "t1");
        let right_clts = right.build()?;

        let options = CompositionOptions::new(CompositionSemantics::Superset);

        let composed = compose(&left_clts, &right_clts, &options)?;
        // Superset composition should include all label combinations
        assert!(composed.state_count() > 0);

        // Verify transitions exist for various label combinations
        // State names use format "left|right"
        let s0_t0 = composed.state_id("s0|t0")?;
        let outgoing = composed.outgoing(s0_t0);
        assert!(!outgoing.is_empty());

        Ok(())
    }

    #[test]
    fn composition_handles_unreachable_states() -> TestResult {
        // Test composition with unreachable states (line 381)
        let mut left = Clts::builder();
        left.state("s0").initial("s0");
        left.state("s1"); // Unreachable from s0
        let left_clts = left.build()?;

        let mut right = Clts::builder();
        right.state("t0").initial("t0");
        right.state("t1"); // Unreachable from t0
        let right_clts = right.build()?;

        let options = CompositionOptions::new(CompositionSemantics::Synchronous);

        let composed = compose(&left_clts, &right_clts, &options)?;
        // Should only include reachable states (s0|t0)
        assert_eq!(composed.state_count(), 1);
        assert!(composed.state_id("s0|t0").is_ok());
        assert!(composed.state_id("s1|t1").is_err());

        Ok(())
    }

    // ---- R.1 — KMTS modality merge end-to-end via compose() ----
    //
    // The 3×3 corollary table from `docs/design/native-sv-abstraction.md` §6.5,
    // exercised at the composition level rather than the standalone-method
    // level (which is covered by the unit tests in `clts/mod.rs::tests`).

    /// Helper: build a single-transition CLTS s0 -a-> s1 with the
    /// given modality. The label is marked `Uncontrollable` so that
    /// the composition operator's SCT-style validation does not
    /// reject the test on "controllable actions shared between
    /// automata" (the default classification, `Controllable`, is
    /// the rejected case when both sides share the same label name).
    fn one_edge_with_modality(
        label: &str,
        modality: TransitionModality<DefaultStateIdx>,
    ) -> TestResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").initial("s0");
        let id = builder.labels().intern([label])?;
        builder.set_label_controllability(id, LabelControllability::Uncontrollable);
        let s0 = builder.state_id_or_insert("s0").expect("s0 inserted");
        let s1 = builder.state_id_or_insert("s1").expect("s1 inserted");
        builder.transition_ids_with_modality(s0, &[id], s1, modality);
        builder.build().map_err(|e| e.into())
    }

    /// Helper: pull the modality of the unique outgoing transition
    /// from `state` in the composed CLTS.
    fn first_outgoing_modality(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        state: &str,
    ) -> TestResult<TransitionModality<DefaultStateIdx>> {
        let sid = clts.state_id(state)?;
        let trans = clts.outgoing(sid);
        assert_eq!(
            trans.len(),
            1,
            "expected exactly one outgoing transition from {state}; got {}",
            trans.len()
        );
        Ok(trans[0].modality().clone())
    }

    #[test]
    fn compose_synchronizing_sharp_and_sharp_yields_sharp() -> TestResult {
        // Both sides have may AND must on label "a"; composed has both.
        let left = one_edge_with_modality("a", TransitionModality::Sharp)?;
        let right = one_edge_with_modality("a", TransitionModality::Sharp)?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        assert_eq!(
            first_outgoing_modality(&composed, "s0|s0")?,
            TransitionModality::Sharp,
        );
        Ok(())
    }

    #[test]
    fn compose_synchronizing_sharp_and_mayonly_yields_mayonly() -> TestResult {
        // Both have may; only left has must; composed has may but not must.
        let left = one_edge_with_modality("a", TransitionModality::Sharp)?;
        let right = one_edge_with_modality("a", TransitionModality::MayOnly)?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        assert_eq!(
            first_outgoing_modality(&composed, "s0|s0")?,
            TransitionModality::MayOnly,
        );
        Ok(())
    }

    #[test]
    fn compose_synchronizing_mayonly_and_sharp_yields_mayonly() -> TestResult {
        // Symmetric of the previous case — merge is commutative.
        let left = one_edge_with_modality("a", TransitionModality::MayOnly)?;
        let right = one_edge_with_modality("a", TransitionModality::Sharp)?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        assert_eq!(
            first_outgoing_modality(&composed, "s0|s0")?,
            TransitionModality::MayOnly,
        );
        Ok(())
    }

    #[test]
    fn compose_synchronizing_mayonly_and_mayonly_yields_mayonly() -> TestResult {
        // Both have may; neither has must.
        let left = one_edge_with_modality("a", TransitionModality::MayOnly)?;
        let right = one_edge_with_modality("a", TransitionModality::MayOnly)?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        assert_eq!(
            first_outgoing_modality(&composed, "s0|s0")?,
            TransitionModality::MayOnly,
        );
        Ok(())
    }

    // ---- R-MM-2 — valuations + 3-valued predicates survive composition ----

    /// Builds a 2-state CLTS whose initial `s0` carries a structured
    /// valuation + one 3-valued predicate, with a single shared-label edge.
    fn valued_one_edge(
        label: &str,
        valuation: &[(&str, &str)],
        predicate: (&str, Tristate),
    ) -> TestResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").initial("s0");
        let id = builder.labels().intern([label])?;
        builder.set_label_controllability(id, LabelControllability::Uncontrollable);
        let s0 = builder.state_id_or_insert("s0").expect("s0 inserted");
        let s1 = builder.state_id_or_insert("s1").expect("s1 inserted");
        builder.transition_ids(s0, &[id], s1);
        let val: BTreeMap<String, String> = valuation
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        builder.with_valuation_for_state(s0, val);
        builder.with_3valued_predicate(s0, predicate.0, predicate.1);
        builder.build().map_err(|e| e.into())
    }

    #[test]
    fn compose_carries_valuations_and_predicates_to_product() -> TestResult {
        // The native explicit-state path drops valuations/predicates at the
        // product boundary; KMTS composition must carry them so a composed
        // property can reference abstract values from both modules.
        let left = valued_one_edge(
            "clk",
            &[("prod_state", "SENDING")],
            ("prod_sent", Tristate::KleeneT),
        )?;
        let right = valued_one_edge(
            "clk",
            &[("cons_state", "RECV")],
            ("cons_ready", Tristate::KleeneBot),
        )?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;

        let prod = composed.state_id("s0|s0")?;

        // Valuations from BOTH modules are present on the product state.
        let val = composed
            .state_valuation(prod)
            .expect("product carries a valuation");
        assert_eq!(val.get("prod_state").map(|s| s.as_str()), Some("SENDING"));
        assert_eq!(val.get("cons_state").map(|s| s.as_str()), Some("RECV"));

        // 3-valued predicates from BOTH modules survive with their verdicts.
        assert_eq!(
            composed.state_3valued_predicate(prod, "prod_sent"),
            Some(Tristate::KleeneT)
        );
        assert_eq!(
            composed.state_3valued_predicate(prod, "cons_ready"),
            Some(Tristate::KleeneBot)
        );
        assert!(composed.has_valuations());
        assert!(composed.has_3valued_predicates());
        Ok(())
    }

    #[test]
    fn compose_predicate_disagreement_collapses_to_kleenebot() -> TestResult {
        // Same predicate name in both modules with conflicting verdicts must
        // NOT fabricate a definite verdict — it collapses to KleeneBot
        // (sound: the composed product is genuinely uncertain there).
        let left = valued_one_edge("clk", &[("v", "1")], ("p", Tristate::KleeneT))?;
        let right = valued_one_edge("clk", &[("v", "2")], ("p", Tristate::KleeneF))?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        let prod = composed.state_id("s0|s0")?;
        assert_eq!(
            composed.state_3valued_predicate(prod, "p"),
            Some(Tristate::KleeneBot)
        );
        // Valuation collision: left wins deterministically.
        assert_eq!(
            composed
                .state_valuation(prod)
                .and_then(|m| m.get("v"))
                .map(|s| s.as_str()),
            Some("1")
        );
        Ok(())
    }

    // ---- R.4.5 — 3-way Sharp / MayOnly / MustHyperOnly composition matrix ----
    //
    // Per the §10.1 R.4.5 done-criterion (and the §Phase 5 §5.4 hyper-
    // must extension), composition of two transitions with capabilities
    // drawn from {Sharp, MayOnly, MustHyperOnly} follows the per-axis
    // conjunction merge rule. Sharp = singleton-target must; MustHyperOnly
    // = N-target must (N ≥ 1); MayOnly = no must capability.
    //
    // For interleaving steps (async; only one side moves), the contributing
    // side's modality survives unchanged on the composed edge — no merge
    // happens. The R.4.5 close-out (2026-05-27) populates the Cartesian
    // product directly in the synchronizing path (verified by
    // `compose_synchronizing_hyper_must_populates_cartesian_product` and
    // `compose_synchronizing_sharp_and_hyper_must_promotes_to_hyper`);
    // the interleaving path's hyper-must preservation is verified below.

    #[test]
    fn compose_async_interleaving_preserves_hyper_must_modality() -> TestResult {
        // Build a CLTS where the left side has a single-state MustHyperOnly
        // transition. The right side has a Sharp transition on a disjoint
        // label; under async composition the left's hyper-must survives
        // unchanged on its interleaving edge.
        let mut left_builder = Clts::builder();
        left_builder.state("s0").state("s1").initial("s0");
        let l_lbl = left_builder.labels().intern(["a"])?;
        left_builder.set_label_controllability(l_lbl, LabelControllability::Uncontrollable);
        let l_s0 = left_builder.state_id_or_insert("s0").expect("s0 inserted");
        let l_s1 = left_builder.state_id_or_insert("s1").expect("s1 inserted");
        // MustHyperOnly with two targets — fabricated; the data model
        // accepts any target set, the composition rule preserves the
        // singleton case on interleaving.
        let hyper_targets: smallvec::SmallVec<[StateId<DefaultStateIdx>; 4]> =
            smallvec::smallvec![l_s1];
        left_builder.transition_ids_with_modality(
            l_s0,
            &[l_lbl],
            l_s1,
            TransitionModality::must_hyper(hyper_targets),
        );
        let left = left_builder.build()?;

        let right = one_edge_with_modality("b", TransitionModality::Sharp)?;

        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Asynchronous),
        )?;
        let sid = composed.state_id("s0|s0")?;
        let trans = composed.outgoing(sid);
        assert_eq!(
            trans.len(),
            2,
            "expected 2 interleaving outgoing edges (left's hyper-must on 'a', right's Sharp on 'b')"
        );
        // The hyper-must edge must survive interleaving unchanged.
        let has_hyper = trans
            .iter()
            .any(|t| matches!(t.modality(), TransitionModality::MustHyperOnly(_)));
        assert!(
            has_hyper,
            "interleaving must preserve the source side's MustHyperOnly modality; \
             actual modalities: {:?}",
            trans.iter().map(|t| t.modality()).collect::<Vec<_>>()
        );
        // The Sharp edge from the right side also survives.
        let has_sharp = trans
            .iter()
            .any(|t| matches!(t.modality(), TransitionModality::Sharp));
        assert!(
            has_sharp,
            "interleaving must preserve the right side's Sharp modality"
        );
        Ok(())
    }

    #[test]
    fn compose_synchronizing_hyper_must_populates_cartesian_product() -> TestResult {
        // R.4.5 close-out — when both sides are `MustHyperOnly`,
        // the composed transition's hyper-target set must be the
        // Cartesian product of the two sides' must-target sets,
        // projected through `enqueue_state` to product StateIds.
        //
        // Build:
        //   left:  s0 -a-> hyper{s1, s2}
        //   right: t0 -a-> hyper{u1, u2}
        // Expected composed transition from "s0|t0" on "a" with
        // modality MustHyperOnly whose target set = {
        //   s1|u1, s1|u2, s2|u1, s2|u2 } (4 product states).
        // The principal target (carried on the transition's `target`
        // field) is the (lt.target(), rt.target()) pair — by
        // convention `targets[0]`.
        let mut left_builder = Clts::builder();
        left_builder
            .state("s0")
            .state("s1")
            .state("s2")
            .initial("s0");
        let l_lbl = left_builder.labels().intern(["a"])?;
        left_builder.set_label_controllability(l_lbl, LabelControllability::Uncontrollable);
        let l_s0 = left_builder.state_id_or_insert("s0").expect("s0 inserted");
        let l_s1 = left_builder.state_id_or_insert("s1").expect("s1 inserted");
        let l_s2 = left_builder.state_id_or_insert("s2").expect("s2 inserted");
        let l_targets: smallvec::SmallVec<[StateId<DefaultStateIdx>; 4]> =
            smallvec::smallvec![l_s1, l_s2];
        left_builder.transition_ids_with_modality(
            l_s0,
            &[l_lbl],
            l_s1,
            TransitionModality::must_hyper(l_targets),
        );
        let left = left_builder.build()?;

        let mut right_builder = Clts::builder();
        right_builder
            .state("t0")
            .state("u1")
            .state("u2")
            .initial("t0");
        let r_lbl = right_builder.labels().intern(["a"])?;
        right_builder.set_label_controllability(r_lbl, LabelControllability::Uncontrollable);
        let r_t0 = right_builder.state_id_or_insert("t0").expect("t0 inserted");
        let r_u1 = right_builder.state_id_or_insert("u1").expect("u1 inserted");
        let r_u2 = right_builder.state_id_or_insert("u2").expect("u2 inserted");
        let r_targets: smallvec::SmallVec<[StateId<DefaultStateIdx>; 4]> =
            smallvec::smallvec![r_u1, r_u2];
        right_builder.transition_ids_with_modality(
            r_t0,
            &[r_lbl],
            r_u1,
            TransitionModality::must_hyper(r_targets),
        );
        let right = right_builder.build()?;

        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        let sid = composed.state_id("s0|t0")?;
        let trans = composed.outgoing(sid);
        assert_eq!(
            trans.len(),
            1,
            "synchronizing on shared label 'a' should produce one composed transition"
        );
        let modality = trans[0].modality();
        let hyper = match modality {
            TransitionModality::MustHyperOnly(targets) => &**targets,
            other => panic!("expected MustHyperOnly composed modality, got {other:?}"),
        };
        assert_eq!(
            hyper.len(),
            4,
            "Cartesian product of left {{s1, s2}} × right {{u1, u2}} must have 4 elements; got {}",
            hyper.len()
        );
        // Principal target must be the first element by convention.
        assert_eq!(
            trans[0].target(),
            hyper[0],
            "principal target must match hyper_targets[0] per the §clts convention"
        );
        // Every product target must be reachable as a real composed state.
        let target_names: Vec<String> = hyper
            .iter()
            .map(|&id| composed.state_name(id).unwrap_or("<unknown>").to_string())
            .collect();
        let expected: std::collections::HashSet<&str> =
            ["s1|u1", "s1|u2", "s2|u1", "s2|u2"].into_iter().collect();
        let actual: std::collections::HashSet<&str> =
            target_names.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            actual, expected,
            "Cartesian product targets must match {{s1|u1, s1|u2, s2|u1, s2|u2}}; got {target_names:?}"
        );
        Ok(())
    }

    #[test]
    fn compose_synchronizing_sharp_and_hyper_must_promotes_to_hyper() -> TestResult {
        // R.4.5 close-out — Sharp ⊗ MustHyperOnly promotes the
        // composed modality to MustHyperOnly. The left side's
        // singleton must-target contributes a single dimension to
        // the Cartesian product; the right side's hyper-target set
        // contributes the rest. Result has |left_must| × |right_hyper|
        // = 1 × 2 = 2 product targets.
        let left = one_edge_with_modality("a", TransitionModality::Sharp)?;

        let mut right_builder = Clts::builder();
        right_builder
            .state("t0")
            .state("u1")
            .state("u2")
            .initial("t0");
        let r_lbl = right_builder.labels().intern(["a"])?;
        right_builder.set_label_controllability(r_lbl, LabelControllability::Uncontrollable);
        let r_t0 = right_builder.state_id_or_insert("t0").expect("t0 inserted");
        let r_u1 = right_builder.state_id_or_insert("u1").expect("u1 inserted");
        let r_u2 = right_builder.state_id_or_insert("u2").expect("u2 inserted");
        let r_targets: smallvec::SmallVec<[StateId<DefaultStateIdx>; 4]> =
            smallvec::smallvec![r_u1, r_u2];
        right_builder.transition_ids_with_modality(
            r_t0,
            &[r_lbl],
            r_u1,
            TransitionModality::must_hyper(r_targets),
        );
        let right = right_builder.build()?;

        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Synchronous),
        )?;
        let sid = composed.state_id("s0|t0")?;
        let trans = composed.outgoing(sid);
        assert_eq!(
            trans.len(),
            1,
            "synchronizing on shared label 'a' should produce one composed transition"
        );
        let hyper = match trans[0].modality() {
            TransitionModality::MustHyperOnly(targets) => &**targets,
            other => panic!(
                "expected MustHyperOnly composed modality (Sharp ⊗ MustHyperOnly promotes), got {other:?}"
            ),
        };
        assert_eq!(
            hyper.len(),
            2,
            "Cartesian product of {{s1}} × {{u1, u2}} must have 2 elements"
        );
        Ok(())
    }

    #[test]
    fn compose_3way_matrix_capabilities_match_truth_table() -> TestResult {
        // The 3×3 capability-conjunction merge table from
        // `clts/mod.rs::TransitionModality::merge` is verified here at
        // the composition layer. Synchronizing edges (shared label "a")
        // produce a composed transition whose modality is the merge
        // of the two side's modalities.
        //
        // Note: this test covers Sharp / MayOnly merges only — the
        // hyper-target Cartesian product the synchronizing case
        // produces under R.4.5 close-out is verified by
        // `compose_synchronizing_hyper_must_populates_cartesian_product`
        // and `compose_synchronizing_sharp_and_hyper_must_promotes_to_hyper`
        // above. Here we only verify the KIND of the composed
        // modality (Sharp / MayOnly) matches the table.
        type ModalityCheck = fn(&TransitionModality<DefaultStateIdx>) -> bool;
        let is_sharp: ModalityCheck = |m| matches!(m, TransitionModality::Sharp);
        let is_mayonly: ModalityCheck = |m| matches!(m, TransitionModality::MayOnly);
        let table: [(
            TransitionModality<DefaultStateIdx>,
            TransitionModality<DefaultStateIdx>,
            ModalityCheck,
            &'static str,
        ); 3] = [
            (
                TransitionModality::Sharp,
                TransitionModality::Sharp,
                is_sharp,
                "Sharp ⊗ Sharp = Sharp",
            ),
            (
                TransitionModality::Sharp,
                TransitionModality::MayOnly,
                is_mayonly,
                "Sharp ⊗ MayOnly = MayOnly",
            ),
            (
                TransitionModality::MayOnly,
                TransitionModality::MayOnly,
                is_mayonly,
                "MayOnly ⊗ MayOnly = MayOnly",
            ),
        ];
        for (lm, rm, expected_check, case) in table.iter() {
            let left = one_edge_with_modality("a", lm.clone())?;
            let right = one_edge_with_modality("a", rm.clone())?;
            let composed = compose(
                &left,
                &right,
                &CompositionOptions::new(CompositionSemantics::Synchronous),
            )?;
            let modality = first_outgoing_modality(&composed, "s0|s0")?;
            assert!(expected_check(&modality), "{case}: got {:?}", modality);
        }
        Ok(())
    }

    #[test]
    fn compose_async_interleaving_preserves_single_side_modality() -> TestResult {
        // Interleaving step: only one side moves; that side's
        // modality survives unchanged on the composed transition.
        let left = one_edge_with_modality("a", TransitionModality::Sharp)?;
        let right = one_edge_with_modality("b", TransitionModality::MayOnly)?;
        let composed = compose(
            &left,
            &right,
            &CompositionOptions::new(CompositionSemantics::Asynchronous),
        )?;
        // The composed CLTS has two interleaving paths from s0|s0:
        //   s0|s0 -a-> s1|s0  (modality from left = Sharp)
        //   s0|s0 -b-> s0|s1  (modality from right = MayOnly)
        let sid = composed.state_id("s0|s0")?;
        let trans = composed.outgoing(sid);
        assert_eq!(trans.len(), 2, "expected two interleaving outgoing edges");
        let mut modalities: Vec<TransitionModality<DefaultStateIdx>> =
            trans.iter().map(|t| t.modality().clone()).collect();
        modalities.sort();
        assert_eq!(
            modalities,
            vec![TransitionModality::Sharp, TransitionModality::MayOnly],
            "left's Sharp and right's MayOnly must both survive on their respective interleaving edges"
        );
        Ok(())
    }
}
