//! Compositional Labeled Transition System (CLTS) module.

use bitvec::prelude::*;
use smallvec::SmallVec;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

// Default reservation sizes keep builder allocations amortised even for large
// benchmarks while staying small enough for unit tests.
const DEFAULT_STATE_RESERVE: usize = 256;
const DEFAULT_TRANSITION_RESERVE: usize = 512;
const DEFAULT_LABEL_RESERVE: usize = 128;
const DEFAULT_VARIABLE_RESERVE: usize = 64;
const DEFAULT_STATE_SET_POOL_RESERVE: usize = 8;

/// Simple pool that recycles `Vec<String>` scratch buffers.
#[derive(Debug, Clone, Default)]
pub(crate) struct StringVecPool {
    pool: Vec<Vec<String>>,
}

impl StringVecPool {
    /// Returns an empty vector whose capacity is at least `hint`.
    pub(crate) fn acquire(&mut self, hint: usize) -> Vec<String> {
        self.pool
            .pop()
            .map(|mut vec| {
                if vec.capacity() < hint {
                    vec.reserve(hint - vec.capacity());
                }
                vec
            })
            .unwrap_or_else(|| Vec::with_capacity(hint))
    }

    /// Clears and returns the vector to the pool for future reuse.
    pub(crate) fn release(&mut self, mut vec: Vec<String>) {
        vec.clear();
        self.pool.push(vec);
    }
}

// EXP-0004: removed `grow_capacity()` (~20% increments) in favor of
// Vec::push's native amortized doubling. The 20% wrapper produced more,
// smaller reallocations than Vec's default strategy (e.g., ~25 reallocs
// to reach 1M states vs ~12 with doubling). It also coordinated parallel
// Vecs into a single reserve call, which is no longer needed when each
// Vec doubles independently. Callers who know their final size should
// pre-allocate explicitly via `reserve_states` / `reserve_transitions`.

/// Records symbols into a shared symbol universe, marking the cache as dirty.
///
/// This helper is shared by label and variable stores, both of which maintain:
/// - a `symbol_set` to track uniqueness,
/// - a `symbols` vector as a cached, sortable universe,
/// - and a `symbols_dirty` flag to avoid redundant sorting until `build()`.
fn record_symbols_into_universe(
    symbol_set: &mut HashSet<String>,
    symbols: &mut Vec<String>,
    symbols_dirty: &mut bool,
    payload: &[String],
) {
    for symbol in payload {
        if symbol_set.insert(symbol.clone()) {
            symbols.push(symbol.clone());
            *symbols_dirty = true;
        }
    }
}

/// Bundles the data required to expose a symbol universe and its bitset view.
///
/// Both label and variable stores represent their sets as `Arc<[String]>` and
/// expose a bitset view over a shared universe. This structure captures the
/// shared representation:
/// - `sets`: canonicalised string sets (labels or variables),
/// - `bitsets`: bitvec view over the symbol universe,
/// - `symbols`: sorted universe of all symbols,
/// - `symbol_index`: mapping from symbol → bit index.
#[derive(Debug)]
struct SymbolUniverse {
    sets: Vec<Arc<[String]>>,
    bitsets: Vec<BitVec<usize, Lsb0>>,
    symbols: Vec<String>,
    symbol_index: HashMap<String, usize>,
}

/// Builds a symbol index and bitset representation for a collection of string sets.
///
/// This helper centralises the logic for:
/// - sorting the symbol universe (when `symbols_dirty` is `true`),
/// - building a `symbol_index` map,
/// - and constructing per-set bitvectors.
fn build_symbol_index_and_bitsets(
    sets: Vec<Arc<[String]>>,
    mut symbols: Vec<String>,
    symbols_dirty: bool,
) -> SymbolUniverse {
    if symbols_dirty {
        symbols.sort();
    }

    let symbol_index: HashMap<String, usize> = symbols
        .iter()
        .enumerate()
        .map(|(idx, symbol)| (symbol.clone(), idx))
        .collect();

    let bitsets: Vec<BitVec<usize, Lsb0>> = sets
        .iter()
        .map(|items| {
            let mut bits = bitvec![usize, Lsb0; 0; symbols.len()];
            for item in items.iter() {
                if let Some(&idx) = symbol_index.get(item) {
                    bits.set(idx, true);
                }
            }
            bits
        })
        .collect();

    SymbolUniverse {
        sets,
        bitsets,
        symbols,
        symbol_index,
    }
}

/// Default storage type for state identifiers.
///
/// This type alias can be redefined at the application level to use different
/// identifier widths (e.g., `u16` for memory-constrained systems, `u64` for
/// very large systems). However, note that:
///
/// - The Context DSL realization layer uses these defaults
/// - Persistence format assumes `u32` identifiers
/// - Third-party code may depend on these defaults
///
/// For custom types without changing defaults, use `Clts<u16, u16>` or
/// `Clts<u64, u64>` directly. See `docs/identifier_width_customization.md`
/// for detailed guidance.
pub type DefaultStateIdx = u32;

/// Default storage type for label identifiers.
///
/// See [`DefaultStateIdx`] for details on customization options.
pub type DefaultLabelIdx = u32;

/// Trait implemented by integer types that can safely back CLTS identifiers.
pub trait IdStorage: Copy + Eq + Ord + Hash + std::fmt::Debug + 'static {
    fn try_from_usize(value: usize) -> Option<Self>;
    fn to_usize(self) -> usize;
}

macro_rules! impl_id_storage {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IdStorage for $ty {
                fn try_from_usize(value: usize) -> Option<Self> {
                    if value <= <$ty>::MAX as usize {
                        Some(value as $ty)
                    } else {
                        None
                    }
                }

                fn to_usize(self) -> usize {
                    self as usize
                }
            }
        )*
    };
}

impl_id_storage!(u16, u32, u64, usize);

/// Identifier assigned to a state within a CLTS instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateId<S: IdStorage>(S);

impl<S: IdStorage> StateId<S> {
    fn new(index: usize) -> Option<Self> {
        S::try_from_usize(index).map(Self)
    }

    /// Construct a StateId from a raw `usize` index. Returns `None` if the
    /// index doesn't fit the underlying storage (e.g., out of range for
    /// `DefaultStateIdx = u32`). Use when you need to convert an index
    /// from a bitset/raw lookup back into a typed StateId.
    pub fn from_index(index: usize) -> Option<Self> {
        Self::new(index)
    }

    pub fn raw(self) -> S {
        self.0
    }

    pub fn index(self) -> usize {
        self.0.to_usize()
    }
}

/// Identifier assigned to a label entry managed by the label store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId<L: IdStorage>(L);

impl<L: IdStorage> LabelId<L> {
    fn new(index: usize) -> Option<Self> {
        L::try_from_usize(index).map(Self)
    }

    /// Returns the raw label identifier.
    ///
    /// # Coverage Status
    /// Covered by test: `label_id_raw_accessor`
    pub fn raw(self) -> L {
        self.0
    }

    pub fn index(self) -> usize {
        self.0.to_usize()
    }
}

/// Errors produced by CLTS builders or lookup routines.
#[derive(Debug, thiserror::Error)]
pub enum CltsError {
    /// Requested state name is unknown to the CLTS.
    #[error("unknown state: {0}")]
    UnknownState(String),
    /// The chosen identifier storage type cannot encode the requested value.
    #[error("{kind} identifier overflow (value {value})")]
    IdOverflow { kind: &'static str, value: usize },
    /// Composition error: controllable or internal actions are shared between automata.
    #[error("composition error: {0}")]
    CompositionError(String),
}

/// Result alias specialised for CLTS-related fallible operations.
pub type CltsResult<T> = std::result::Result<T, CltsError>;

/// Classification of label controllability.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum LabelControllability {
    /// Controllable: The system can choose to take this action.
    Controllable,
    /// Internal: Internal system action, not visible to other automata.
    /// Internal actions are mutually exclusive between automata in composition.
    Internal,
    /// Uncontrollable: The environment chooses this action (e.g., input signals).
    Uncontrollable,
}

/// R.1 — Kleene tristate value for 3-valued state-predicate labellings
/// and 3-valued mu-calculus verdicts. Per the KMTS architecture
/// (`docs/design/native-sv-abstraction.md` §6.3, `docs/design/kmts-theory.md` §2):
///
/// - `KleeneT` — the predicate / formula evaluates to *true* on
///   every concretisation of this abstract state.
/// - `KleeneF` — the predicate / formula evaluates to *false* on
///   every concretisation.
/// - `KleeneBot` — concretisations disagree; the abstraction is
///   too coarse to give a definite answer here. Refinement
///   (CEGAR, R.5) demotes `KleeneBot` to `KleeneT` or `KleeneF`.
///
/// The prefix avoids collision with Rust's `bool` (`True`/`False`)
/// mental model and makes match arms unambiguous across the
/// thousands of touch-points this enum will see in the evaluator.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Tristate {
    KleeneT,
    KleeneF,
    KleeneBot,
}

impl Tristate {
    /// Lift a 2-valued Boolean into the tristate domain. A
    /// Sharp-everywhere KMTS only ever produces `KleeneT` /
    /// `KleeneF` values; this helper is how legacy 2-valued
    /// adapters seed the 3-valued labelling vacuously when running
    /// through a `KleeneDomain` evaluator.
    pub fn from_bool(b: bool) -> Self {
        if b {
            Tristate::KleeneT
        } else {
            Tristate::KleeneF
        }
    }

    /// Convenience for the common interpretation: `KleeneT` is the
    /// only "definite truth" verdict. `KleeneF` and `KleeneBot`
    /// both return `false`.
    pub fn is_true(self) -> bool {
        matches!(self, Tristate::KleeneT)
    }

    /// Whether this verdict is definite (not `KleeneBot`).
    pub fn is_definite(self) -> bool {
        !matches!(self, Tristate::KleeneBot)
    }
}

/// R.1 + R.4.5 — KMTS transition modality
/// (`docs/design/native-sv-abstraction.md` §6.3 + §6.11).
///
/// **Standard KMTS (R.1; Larsen–Thomsen 1988):** every transition is
/// either in *both* may and must relations (`Sharp`) or in *only* may
/// (`MayOnly`). The mixed-transition-system generalisation of
/// Dams–Gerth–Grumberg TOPLAS 1997 would allow a "must without may"
/// variant; mununu does not produce one (the predicate-image lifter
/// at R.2/R.2.5 always satisfies `must ⊆ may`).
///
/// **Generalized KMTS (R.4.5; Shoham–Grumberg LMCS 2007):** to make
/// refinement *monotone* on alternating fixpoints, a must-transition
/// targets a **set** of abstract states (a hyper-target) rather than
/// a single state. A refinement realizes the must-transition by
/// hitting *some* element of the target set — the additional
/// freedom is what defeats the over-commitment that plain-KMTS
/// must-transitions suffer under refinement. `Sharp` is the
/// singleton-target degeneracy of `MustHyperOnly`; we keep it as a
/// separate variant so the common case stays cheap and the data
/// model carries the may/must split structurally.
///
/// **Strict additivity:** every existing adapter and the R.2 lifter
/// produce `Sharp` (or `MayOnly` under UF abstraction) transitions —
/// `MustHyperOnly` is reserved for the R.5/R.5b CEGAR + UF-abstraction
/// path that the R.4.5 ↔ R.5.0 ↔ R.5 ↔ R.5b sequence delivers. The
/// `BoolDomain` evaluator (R.1) treats every transition as Sharp;
/// the `KleeneDomain` evaluator (R.3) reads `MayOnly`; the
/// `KleeneDomain` R.4.5 widening also reads `MustHyperOnly`.
///
/// **Memory note:** the hyper-target set is `Box<SmallVec<…>>` so
/// `Sharp` and `MayOnly` stay 9 bytes (discriminant + pointer slot,
/// unused for the unit variants). At the cost of one indirection for
/// the rare `MustHyperOnly` case we avoid inflating every Sharp /
/// MayOnly transition by ~32 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TransitionModality<S: IdStorage> {
    /// In both `may` and `must` relations with a singleton must-target
    /// (the transition's `target` field). Equivalent under GKMTS to
    /// `MustHyperOnly` with `targets = SmallVec::from([target])`; we
    /// keep it as a separate variant for the cheap default case.
    Sharp,
    /// In `may` only: the over-approximation admits the transition
    /// (an existential SMT query under the UF-abstracted relation
    /// found a witness), but no must-witness backs it.
    MayOnly,
    /// R.4.5 — In both `may` and `must` relations with a **hyper-target**
    /// set: every concrete refinement of the source state has a
    /// must-successor in some element of the target set. The
    /// transition's `target` field still carries the "principal"
    /// target (the first element of `targets` by convention), so
    /// existing single-target adjacency-list queries continue to work
    /// for Sharp / MayOnly behaviour; consumers that respect the
    /// hyper-must semantics read the full `targets` set.
    ///
    /// The Box keeps the enum size at 9 bytes; only the rare
    /// `MustHyperOnly` case pays for the SmallVec allocation.
    MustHyperOnly(Box<smallvec::SmallVec<[StateId<S>; 4]>>),
}

impl<S: IdStorage> Default for TransitionModality<S> {
    /// Existing adapters and hand-written CTXDSL produce
    /// Sharp-everywhere KMTSes. New adapters opt into `MayOnly` /
    /// `MustHyperOnly` explicitly via the modality-aware builder.
    fn default() -> Self {
        TransitionModality::Sharp
    }
}

impl<S: IdStorage> TransitionModality<S> {
    /// Per-axis conjunction merge for parallel composition
    /// (Larsen–Larsen–Wąsowski FoSSaCS 2007 — standard KMTS;
    /// Shoham–Grumberg LMCS 2007 §3 — GKMTS hyper-must extension;
    /// `docs/design/native-sv-abstraction.md` §6.5 + §6.11;
    /// `docs/design/kmts-theory.md` §5).
    ///
    /// A composed transition has each capability iff *both* sides
    /// have a transition with that capability on the synchronizing
    /// label:
    ///
    /// ```text
    /// has_may (left ⊗ right) = has_may (left)  ∧ has_may (right)
    /// has_must(left ⊗ right) = has_must(left) ∧ has_must(right)
    /// ```
    ///
    /// The 3×3 merge table (post-R.4.5):
    ///
    /// ```text
    ///                Sharp          MayOnly  MustHyperOnly
    /// Sharp          Sharp          MayOnly  MustHyperOnly*
    /// MayOnly        MayOnly        MayOnly  MayOnly
    /// MustHyperOnly  MustHyperOnly* MayOnly  MustHyperOnly†
    ///
    /// * The composed MustHyperOnly's target set is the Cartesian
    ///   product of the two sides' must-target sets, constructed by
    ///   the composition layer at product-state-build time.
    ///   `Sharp` participates as a singleton-target hyper-must.
    /// † Cartesian product of both hyper-target sets.
    /// ```
    ///
    /// Because the target-set Cartesian product requires access to
    /// the product-state index (which lives at the composition layer),
    /// this method returns a **placeholder** `MustHyperOnly` with an
    /// empty target set — the composition layer is responsible for
    /// computing the actual targets via [`Self::is_must`] and the
    /// `merge_with_hyper_targets` helper. For Sharp / MayOnly merges
    /// the result is exact.
    ///
    /// This is the **silent-soundness-bug fix** flagged in the
    /// architecture doc §6.5 R.1 audit, extended in R.4.5 to handle
    /// hyper-must monotonicity per §6.11.
    pub fn merge(&self, other: &TransitionModality<S>) -> TransitionModality<S> {
        // Capability conjunction: composed transition has must iff both sides
        // have must (Sharp or MustHyperOnly each carry must capability).
        let lhs_must = self.is_must();
        let rhs_must = other.is_must();
        let lhs_hyper = matches!(self, TransitionModality::MustHyperOnly(_));
        let rhs_hyper = matches!(other, TransitionModality::MustHyperOnly(_));
        match (lhs_must, rhs_must) {
            (true, true) => {
                if lhs_hyper || rhs_hyper {
                    // Composition layer computes the Cartesian product.
                    TransitionModality::MustHyperOnly(Box::default())
                } else {
                    TransitionModality::Sharp
                }
            }
            _ => TransitionModality::MayOnly,
        }
    }

    /// Whether this transition is in the `may` relation. Every
    /// well-formed modality has `may` capability under the
    /// standard-KMTS invariant `must ⊆ may`.
    pub fn has_may(&self) -> bool {
        true
    }

    /// Whether this transition is in the `must` relation. True for
    /// `Sharp` (singleton must-target) and `MustHyperOnly` (hyper
    /// must-target set); false for `MayOnly`.
    pub fn has_must(&self) -> bool {
        matches!(
            self,
            TransitionModality::Sharp | TransitionModality::MustHyperOnly(_)
        )
    }

    /// Convenience alias for `has_must()` used by the composition
    /// merge logic.
    pub fn is_must(&self) -> bool {
        self.has_must()
    }

    /// R.4.5 — The hyper-must target set, if this is a `MustHyperOnly`
    /// transition. `None` for `Sharp` (whose must-target is the
    /// transition's `target` field — singleton) and `MayOnly` (no
    /// must capability).
    pub fn hyper_targets(&self) -> Option<&[StateId<S>]> {
        match self {
            TransitionModality::MustHyperOnly(targets) => Some(targets.as_slice()),
            _ => None,
        }
    }

    /// R.4.5 — Construct a `MustHyperOnly` from an explicit target
    /// set. Used by the R.5 CEGAR loop and the R.5b UF-abstraction
    /// path when the predicate-image SMT query returns multiple
    /// admissible successors per source predicate cube.
    pub fn must_hyper(targets: smallvec::SmallVec<[StateId<S>; 4]>) -> Self {
        TransitionModality::MustHyperOnly(Box::new(targets))
    }

    /// R.4.5 close-out — Return the modality's effective must-target
    /// set, given a fallback singleton target. Used by the composition
    /// layer to compute the Cartesian-product hyper-target set on the
    /// synchronizing path:
    ///
    /// - `Sharp` → `[fallback]` (single must-target stored externally
    ///   on `Transition::target`).
    /// - `MustHyperOnly(targets)` → a clone of the explicit hyper-
    ///   target slice.
    /// - `MayOnly` → empty (no must capability).
    ///
    /// The fallback parameter exists so the caller doesn't need to
    /// branch on `Sharp` themselves — pass the transition's principal
    /// `target()` and the helper returns the right shape.
    pub fn must_target_set(&self, fallback: StateId<S>) -> smallvec::SmallVec<[StateId<S>; 4]> {
        match self {
            TransitionModality::Sharp => smallvec::smallvec![fallback],
            TransitionModality::MustHyperOnly(targets) => (**targets).clone(),
            TransitionModality::MayOnly => smallvec::SmallVec::new(),
        }
    }
}

/// Transition entry stored in adjacency lists.
///
/// The labels are staged in a `SmallVec` so the hot path stays stack-allocated
/// for the common case of one or two labels per edge. This keeps the
/// optimisation local to `clts` without leaking the `SmallVec` dependency to
/// callers.
///
/// R.1 — Every transition carries a [`TransitionModality`] for KMTS
/// composition + 3-valued evaluation. Existing 2-valued CLTSes
/// produce `Sharp` everywhere (the default); KMTS-aware adapters
/// produce `MayOnly` for over-approximation edges. The `BoolDomain`
/// evaluator ignores this field and treats every transition as
/// Sharp; the `KleeneDomain` evaluator (R.3) reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition<S: IdStorage, L: IdStorage> {
    target: StateId<S>,
    labels: SmallVec<[LabelId<L>; 4]>,
    modality: TransitionModality<S>,
}

impl<S: IdStorage, L: IdStorage> Transition<S, L> {
    /// Returns the destination state of this transition.
    pub fn target(&self) -> StateId<S> {
        self.target
    }

    /// Returns the labels associated with this transition.
    pub fn labels(&self) -> &[LabelId<L>] {
        &self.labels
    }

    /// R.1 — Returns the KMTS modality of this transition. Sharp by
    /// default for existing (2-valued) CLTSes; `MayOnly` for
    /// KMTS-aware adapters that produce over-approximation edges.
    pub fn modality(&self) -> &TransitionModality<S> {
        &self.modality
    }

    /// Checks if this transition is controllable based on its labels.
    ///
    /// A transition is controllable if ALL its labels are controllable or internal.
    /// This method derives controllability from label controllability.
    ///
    /// # Arguments
    /// * `clts` - The CLTS instance containing label controllability information
    ///
    /// # Returns
    /// `true` if the transition is controllable, `false` otherwise.
    pub fn is_controllable(&self, clts: &Clts<S, L>) -> bool {
        // Epsilon transitions (no labels) are always uncontrollable
        if self.labels.is_empty() {
            return false;
        }
        // Transition is controllable if ALL labels are controllable or internal
        self.labels.iter().all(|&label_id| {
            clts.is_controllable_label(label_id) || clts.is_internal_label(label_id)
        })
    }

    /// Checks if this transition is uncontrollable based on its labels.
    ///
    /// A transition is uncontrollable if ANY of its labels are uncontrollable.
    /// This method derives controllability from label controllability.
    ///
    /// # Arguments
    /// * `clts` - The CLTS instance containing label controllability information
    ///
    /// # Returns
    /// `true` if the transition is uncontrollable, `false` otherwise.
    pub fn is_uncontrollable(&self, clts: &Clts<S, L>) -> bool {
        // Epsilon transitions (no labels) are always uncontrollable
        if self.labels.is_empty() {
            return true;
        }
        // Transition is uncontrollable if ANY label is uncontrollable
        self.labels
            .iter()
            .any(|&label_id| clts.is_uncontrollable_label(label_id))
    }
}

/// Internal pool for recycling `BitVec` buffers used by `StateSet` instances.
///
/// This pool reduces allocation overhead during μ-calculus fixpoint iterations by
/// reusing bit vectors across multiple state set operations. When a `StateSet` is
/// dropped, its underlying `BitVec` is returned to this pool for reuse.
///
/// # Bit Representation
///
/// Each bit in the vector corresponds to a state in the CLTS:
/// - Bit at index `i` is set (`true`) if state with index `i` is in the set
/// - Bit at index `i` is unset (`false`) if state with index `i` is not in the set
///
/// The pool uses `BitVec<usize, Lsb0>` which stores bits efficiently (one bit per
/// state) and provides fast bitwise operations for set operations (union, intersection, etc.).
#[derive(Debug)]
struct StateSetPoolInner {
    state_count: usize,
    pooled: Mutex<Vec<BitVec<usize, Lsb0>>>,
}

impl StateSetPoolInner {
    fn new(state_count: usize) -> Self {
        Self {
            state_count,
            pooled: Mutex::new(Vec::with_capacity(DEFAULT_STATE_SET_POOL_RESERVE)),
        }
    }

    /// Acquires a bit vector from the pool, or allocates a new one if the pool is empty.
    ///
    /// The returned bit vector is guaranteed to have length `state_count` and all bits
    /// are cleared (set to `false`). This ensures a clean state for new state set operations.
    ///
    /// # Bit Layout
    /// The bit vector uses `Lsb0` ordering (least significant bit first), which is the
    /// standard layout for `bitvec` and provides efficient bitwise operations.
    fn acquire(&self) -> BitVec<usize, Lsb0> {
        let mut pooled = self.pooled.lock().expect("state-set pool mutex poisoned");
        match pooled.pop() {
            Some(mut bits) => {
                bits.fill(false);
                bits
            }
            None => BitVec::repeat(false, self.state_count),
        }
    }

    /// Returns a bit vector to the pool for reuse.
    ///
    /// The bit vector is resized to match `state_count` if needed, all bits are cleared,
    /// and it's added back to the pool. This allows the buffer to be reused in subsequent
    /// `acquire()` calls, avoiding repeated allocations during fixpoint iterations.
    ///
    /// # Safety
    /// The mutex may be poisoned if a thread panicked while holding the lock. In that case,
    /// this function will panic with a descriptive error message.
    fn release(&self, mut bits: BitVec<usize, Lsb0>) {
        if bits.len() != self.state_count {
            bits.resize(self.state_count, false);
        }
        bits.fill(false);
        let mut pooled = self.pooled.lock().expect("state-set pool mutex poisoned");
        pooled.push(bits);
    }

    fn state_count(&self) -> usize {
        self.state_count
    }
}

/// Reusable bitset sized to a CLTS state space.
///
/// Instances are obtained via [`Clts::state_set`] and return their underlying
/// buffer to the CLTS-level pool when dropped. This keeps μ-calculus fixpoint
/// iterations from repeatedly allocating large `BitVec`s during inner loops.
///
/// # Bit Operations
///
/// This type provides efficient set operations using bitwise operations:
/// - **Insert/Remove**: O(1) bit manipulation
/// - **Contains**: O(1) bit check
/// - **Union/Intersection**: O(n/word_size) bitwise operations via `bits()` access
/// - **Iteration**: Efficient iteration over set bits using `iter_ones()`
///
/// # Bit Representation
///
/// Each bit at index `i` represents whether state with index `i` is in the set:
/// - `bits[i] = true` → state `i` is in the set
/// - `bits[i] = false` → state `i` is not in the set
///
/// The underlying `BitVec<usize, Lsb0>` stores bits efficiently (one bit per state)
/// and provides fast bitwise operations for set operations.
///
/// # Example: Bitwise Operations
///
/// ```rust
/// use mununu_core::clts::Clts;
/// use bitvec::prelude::*;
///
/// # let mut builder = Clts::builder();
/// # builder.state("s0");
/// # builder.state("s1");
/// # builder.state("s2");
/// # let clts = builder.build().unwrap();
/// # let s0 = clts.state_id("s0").unwrap();
/// # let s1 = clts.state_id("s1").unwrap();
/// # let s2 = clts.state_id("s2").unwrap();
/// let mut set1 = clts.state_set();
/// let mut set2 = clts.state_set();
/// set1.insert(s0);
/// set1.insert(s1);
/// set2.insert(s1);
/// set2.insert(s2);
///
/// // Set operations using bitwise operations
/// let mut union = set1.bits().to_bitvec();  // Copy for union
/// union |= set2.bits();  // Union
/// let mut intersection = set1.bits().to_bitvec();  // Copy for intersection
/// intersection &= set2.bits();  // Intersection
/// let mut difference = set1.bits().to_bitvec();  // Copy for difference
/// for i in 0..difference.len() {
///     if set2.bits().get(i).is_some_and(|b| *b) {
///         difference.set(i, false);  // Remove elements in set2
///     }
/// }
/// ```
#[derive(Debug)]
pub struct StateSet<S: IdStorage> {
    bits: BitVec<usize, Lsb0>,
    pool: Arc<StateSetPoolInner>,
    marker: PhantomData<S>,
}

impl<S: IdStorage> StateSet<S> {
    /// Number of states represented by this bitset.
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// Returns `true` when no states are marked.
    pub fn is_empty(&self) -> bool {
        self.bits.not_any()
    }

    /// Sets the bit for `state`, returning `true` when it was previously unset.
    ///
    /// This is an O(1) operation that sets the bit at index `state.index()` to `true`.
    /// Returns `true` if the state was not previously in the set, `false` if it was already present.
    ///
    /// # Bit Operation
    /// Equivalent to: `bits[state.index()] = true`
    pub fn insert(&mut self, state: StateId<S>) -> bool {
        let idx = state.index();
        let was_set = self.bits.get(idx).is_some_and(|bit| *bit);
        self.bits.set(idx, true);
        !was_set
    }

    /// Clears the bit for `state`, returning `true` when it was previously set.
    ///
    /// This is an O(1) operation that sets the bit at index `state.index()` to `false`.
    /// Returns `true` if the state was previously in the set, `false` if it was not present.
    ///
    /// # Bit Operation
    /// Equivalent to: `bits[state.index()] = false`
    pub fn remove(&mut self, state: StateId<S>) -> bool {
        let idx = state.index();
        let was_set = self.bits.get(idx).is_some_and(|bit| *bit);
        self.bits.set(idx, false);
        was_set
    }

    /// Returns `true` when the bit for `state` is set.
    ///
    /// This is an O(1) operation that checks if the bit at index `state.index()` is `true`.
    ///
    /// # Bit Operation
    /// Equivalent to: `bits[state.index()] == true`
    pub fn contains(&self, state: StateId<S>) -> bool {
        let idx = state.index();
        self.bits.get(idx).is_some_and(|bit| *bit)
    }

    /// Clears all bits.
    pub fn clear(&mut self) {
        self.bits.fill(false);
    }

    /// Fills all bits with the provided value.
    pub fn fill(&mut self, value: bool) {
        self.bits.fill(value);
    }

    /// Returns an iterator over all marked states.
    pub fn iter(&self) -> impl Iterator<Item = StateId<S>> + '_ {
        self.bits.iter_ones().filter_map(|idx| StateId::new(idx))
    }

    /// Extends the set with additional states from the iterator.
    pub fn extend<I>(&mut self, states: I)
    where
        I: IntoIterator<Item = StateId<S>>,
    {
        for state in states {
            self.insert(state);
        }
    }

    /// Copies bits from another `StateSet`.
    ///
    /// This performs a bitwise copy of all bits from `other` into `self`. Both sets
    /// must have the same length (same CLTS state count).
    ///
    /// # Bit Operation
    /// Equivalent to: `self.bits = other.bits.clone()`
    pub fn copy_from(&mut self, other: &StateSet<S>) {
        self.copy_from_bits(other.bits());
    }

    /// Copies bits from an external bit slice. Length must match.
    ///
    /// This allows copying bits from external sources (e.g., from μ-calculus evaluation
    /// results). The length of `other` must match `self.bits.len()`.
    ///
    /// # Bit Operation
    /// Equivalent to: `self.bits.clone_from_bitslice(other)`
    ///
    /// # Panics
    /// In debug builds, panics if `other.len() != self.bits.len()`.
    pub fn copy_from_bits(&mut self, other: &BitSlice<usize, Lsb0>) {
        debug_assert_eq!(
            other.len(),
            self.bits.len(),
            "bit lengths must match when copying into state set"
        );
        self.bits.clone_from_bitslice(other);
    }

    /// Returns an immutable bit-slice view for bitwise operations.
    ///
    /// This provides direct access to the underlying bit representation, enabling
    /// efficient bitwise operations (union, intersection, difference) without copying.
    ///
    /// # Bitwise Operations
    /// ```rust
    /// // Union: set1 | set2
    /// // Intersection: set1 & set2
    /// // Difference: set1 & !set2
    /// // Complement: !set1
    /// ```
    ///
    /// # Example
    /// ```rust
    /// use mununu_core::clts::Clts;
    /// use bitvec::prelude::*;
    ///
    /// # let mut builder = Clts::builder();
    /// # builder.state("s0");
    /// # builder.state("s1");
    /// # let clts = builder.build().unwrap();
    /// # let s0 = clts.state_id("s0").unwrap();
    /// # let s1 = clts.state_id("s1").unwrap();
    /// let mut set1 = clts.state_set();
    /// let mut set2 = clts.state_set();
    /// set1.insert(s0);
    /// set2.insert(s1);
    /// let mut union = set1.bits().to_bitvec();
    /// union |= set2.bits();
    /// let mut intersection = set1.bits().to_bitvec();
    /// intersection &= set2.bits();
    /// ```
    pub fn bits(&self) -> &BitSlice<usize, Lsb0> {
        self.bits.as_bitslice()
    }

    /// Returns a mutable bit-slice view for in-place bitwise operations.
    ///
    /// This allows modifying the bits directly, which is useful for in-place set
    /// operations (e.g., `set1.bits_mut() |= set2.bits()`).
    ///
    /// # Example
    /// ```rust
    /// use mununu_core::clts::Clts;
    /// use bitvec::prelude::*;
    ///
    /// # let mut builder = Clts::builder();
    /// # builder.state("s0");
    /// # builder.state("s1");
    /// # let clts = builder.build().unwrap();
    /// # let s0 = clts.state_id("s0").unwrap();
    /// # let s1 = clts.state_id("s1").unwrap();
    /// let mut set1 = clts.state_set();
    /// let mut set2 = clts.state_set();
    /// set1.insert(s0);
    /// set2.insert(s1);
    /// // In-place union
    /// *set1.bits_mut() |= set2.bits();
    /// ```
    pub fn bits_mut(&mut self) -> &mut BitSlice<usize, Lsb0> {
        self.bits.as_mut_bitslice()
    }
}

impl<S: IdStorage> Drop for StateSet<S> {
    fn drop(&mut self) {
        let bits = std::mem::take(&mut self.bits);
        self.pool.release(bits);
    }
}

/// Label store collecting unique label payloads.
#[derive(Debug, Clone)]
pub struct LabelStore<L: IdStorage> {
    entries: Vec<Arc<[String]>>,
    bitsets: Vec<BitVec<usize, Lsb0>>,
    _symbols: Vec<String>,
    symbol_index: HashMap<String, usize>,
    marker: PhantomData<L>,
}

impl<L: IdStorage> Default for LabelStore<L> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            bitsets: Vec::new(),
            _symbols: Vec::new(),
            symbol_index: HashMap::new(),
            marker: PhantomData,
        }
    }
}

impl<L: IdStorage> LabelStore<L> {
    fn get(&self, id: LabelId<L>) -> Option<&[String]> {
        self.entries.get(id.index()).map(|labels| labels.as_ref())
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &[String]> {
        self.entries.iter().map(|labels| labels.as_ref())
    }

    fn bitset(&self, id: LabelId<L>) -> Option<LabelBitSet<'_>> {
        self.bitsets.get(id.index()).map(|bits| LabelBitSet {
            bits,
            index: &self.symbol_index,
        })
    }
}

/// Mutable label store used while building a CLTS instance.
///
/// `LabelStoreBuilder` provides string interning for label symbols, ensuring that
/// semantically equivalent label sets (e.g., `["a", "b"]` and `["b", "a"]`) map to
/// the same canonical `LabelId`. This deduplication reduces memory usage and enables
/// efficient label-based operations during CLTS construction.
///
/// # Usage
///
/// The builder is typically accessed through `CltsBuilder::labels()`:
///
/// ```rust
/// use mununu_core::clts::Clts;
///
/// let mut builder = Clts::builder();
/// let label_id = builder.labels().intern(["action", "signal"]).unwrap();
/// ```
///
/// For contexts managing multiple CLTSs with shared label alphabets, you can clone
/// a `LabelStoreBuilder` and pass it to `CltsBuilder::with_label_store()` to ensure
/// label IDs remain consistent across CLTSs.
///
/// # Methods
///
/// - [`intern()`](Self::intern): Interns an iterator of symbols, creating a new label
///   or returning an existing one. Use this when you have a temporary collection.
/// - [`intern_in_place()`](Self::intern_in_place): Interns a mutable `Vec<String>`,
///   taking ownership and clearing it. Use this when you have a reusable buffer to
///   avoid extra allocations.
///
/// # Example: Choosing between `intern()` and `intern_in_place()`
///
/// ```rust
/// use mununu_core::clts::Clts;
///
/// let mut builder = Clts::builder();
///
/// // Use intern() for temporary collections
/// let label1 = builder.labels().intern(["a", "b"]).unwrap();
///
/// // Use intern_in_place() when you have a reusable buffer
/// let mut buffer = vec!["x".to_string(), "y".to_string()];
/// let label2 = builder.labels().intern_in_place(&mut buffer).unwrap();
/// // buffer is now empty and can be reused
/// buffer.push("z".to_string());
/// let label3 = builder.labels().intern_in_place(&mut buffer).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct LabelStoreBuilder<L: IdStorage> {
    entries: Vec<Arc<[String]>>,
    entry_map: HashMap<Arc<[String]>, LabelId<L>>,
    // EXP-0004: entry_capacity_hint removed; Vec/HashMap handle growth.
    // Tracks which alphabet symbols have already been interned. This lets us
    // avoid recomputing the universe during `build()`.
    symbol_set: HashSet<String>,
    // Sorted-on-demand cache of the alphabet universe. `symbols_dirty`
    // indicates whether the vector needs a fresh sort.
    symbols: Vec<String>,
    symbols_dirty: bool,
    buffer_pool: StringVecPool,
}

impl<L: IdStorage> LabelStoreBuilder<L> {
    fn record_symbols(&mut self, payload: &[String]) {
        record_symbols_into_universe(
            &mut self.symbol_set,
            &mut self.symbols,
            &mut self.symbols_dirty,
            payload,
        );
    }

    /// Interns the provided symbol set and returns a canonical label identifier.
    ///
    /// The symbols are sorted and deduplicated so that semantically equivalent
    /// sets map to a single entry. This routine is shared by standalone CLTS
    /// builders and context-managed builders alike, making it the central place
    /// for alphabet deduplication. Choose a label index type large enough for the
    /// expected number of unique labels or an [`CltsError::IdOverflow`] will be
    /// returned.
    ///
    /// # When to use
    ///
    /// Use this method when you have a temporary collection (iterator, array, etc.)
    /// that you don't need to reuse. For reusable buffers, prefer [`intern_in_place()`](Self::intern_in_place)
    /// to avoid extra allocations.
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Temporary collection - use intern()
    /// let label = builder.labels().intern(["action", "signal"]).unwrap();
    /// ```
    pub fn intern<I, S>(&mut self, symbols: I) -> CltsResult<LabelId<L>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut buffer = self.buffer_pool.acquire(DEFAULT_LABEL_RESERVE);
        buffer.extend(symbols.into_iter().map(|s| s.as_ref().to_owned()));
        buffer.sort();
        buffer.dedup();

        if let Some(&id) = self.entry_map.get(buffer.as_slice()) {
            self.buffer_pool.release(buffer);
            return Ok(id);
        }
        let next = self.entries.len();
        let id = LabelId::new(next).ok_or(CltsError::IdOverflow {
            kind: "label",
            value: next,
        })?;
        let owned = std::mem::take(&mut buffer);
        let arc: Arc<[String]> = Arc::from(owned.into_boxed_slice());
        self.record_symbols(arc.as_ref());
        self.entry_map.insert(Arc::clone(&arc), id);
        self.entries.push(arc);
        self.buffer_pool.release(buffer);
        Ok(id)
    }

    /// Interns a mutable vector of symbols, leaving it empty for reuse.
    ///
    /// This method takes ownership of the vector's contents and clears it, allowing
    /// the buffer to be reused for subsequent intern operations. This avoids the
    /// allocation overhead of creating temporary collections.
    ///
    /// # When to use
    ///
    /// Use this method when you have a reusable `Vec<String>` buffer that you want
    /// to clear and reuse multiple times. For temporary collections, use [`intern()`](Self::intern)
    /// instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// let mut buffer = Vec::new();
    ///
    /// // Reuse the same buffer for multiple labels
    /// buffer.push("a".to_string());
    /// buffer.push("b".to_string());
    /// let label1 = builder.labels().intern_in_place(&mut buffer).unwrap();
    ///
    /// buffer.push("x".to_string());
    /// let label2 = builder.labels().intern_in_place(&mut buffer).unwrap();
    /// // buffer is now empty and ready for reuse
    /// ```
    pub fn intern_in_place(&mut self, symbols: &mut Vec<String>) -> CltsResult<LabelId<L>> {
        symbols.sort();
        symbols.dedup();

        if let Some(&id) = self.entry_map.get(symbols.as_slice()) {
            symbols.clear();
            return Ok(id);
        }
        let owned = std::mem::take(symbols);
        let arc: Arc<[String]> = Arc::from(owned.into_boxed_slice());
        let next = self.entries.len();
        let id = LabelId::new(next).ok_or(CltsError::IdOverflow {
            kind: "label",
            value: next,
        })?;
        self.record_symbols(arc.as_ref());
        self.entry_map.insert(Arc::clone(&arc), id);
        self.entries.push(arc);
        Ok(id)
    }

    pub(crate) fn absorb<'a, I>(&mut self, entries: I) -> CltsResult<()>
    where
        I: IntoIterator<Item = &'a [String]>,
    {
        for entry in entries {
            self.intern(entry.iter().map(|s| s.as_str()))?;
        }
        Ok(())
    }

    fn build(self) -> LabelStore<L> {
        let LabelStoreBuilder {
            entries,
            entry_map: _,
            symbol_set: _,
            symbols,
            symbols_dirty,
            buffer_pool: _,
        } = self;

        let SymbolUniverse {
            sets,
            bitsets,
            symbols,
            symbol_index,
        } = build_symbol_index_and_bitsets(entries, symbols, symbols_dirty);

        LabelStore {
            entries: sets,
            bitsets,
            _symbols: symbols,
            symbol_index,
            marker: PhantomData,
        }
    }
}

impl<L: IdStorage> Default for LabelStoreBuilder<L> {
    fn default() -> Self {
        Self {
            entries: Vec::with_capacity(DEFAULT_LABEL_RESERVE),
            entry_map: HashMap::with_capacity(DEFAULT_LABEL_RESERVE),
            // Tracks which alphabet symbols have already been interned. This lets us
            // avoid recomputing the universe during `build()`.
            symbol_set: HashSet::with_capacity(DEFAULT_LABEL_RESERVE),
            // Sorted-on-demand cache of the alphabet universe. `symbols_dirty`
            // indicates whether the vector needs a fresh sort.
            symbols: Vec::with_capacity(DEFAULT_LABEL_RESERVE),
            symbols_dirty: false,
            buffer_pool: StringVecPool::default(),
        }
    }
}

/// Type alias for pre-computed uncontrollable transition groups.
///
/// Maps a canonical key (sorted vector of uncontrollable label IDs) to a list of
/// indices into the `outgoing[state]` vector.
type UncontrollableGroups<L> = Vec<HashMap<SmallVec<[LabelId<L>; 4]>, Vec<usize>>>;

/// Concrete CLTS data structure.
///
/// Instances are immutable: construction flows through [`CltsBuilder`] and any
/// modification requires rehydrating a new builder (e.g. via a future
/// `CltsBuilder::from_clts`). This keeps adjacency caches and identifier maps in
/// sync without scattered mutation logic.
#[derive(Debug, Clone)]
pub struct Clts<S: IdStorage = DefaultStateIdx, L: IdStorage = DefaultLabelIdx> {
    state_names: Vec<String>,
    // Maps state names to state IDs.
    state_map: HashMap<String, StateId<S>>,
    initial_states: HashSet<StateId<S>>,
    outgoing: Vec<Vec<Transition<S, L>>>,
    // Maps state IDs to incoming transitions.
    incoming: Vec<Vec<Transition<S, L>>>,
    // Stores the labels used in the CLTS.
    labels: LabelStore<L>,
    // Stores the variables used in the CLTS.
    variables: VariableStore,
    // Maps state IDs to variable sets.
    state_variables: Vec<VariableSetId>,
    /// Structured variable-value pairs per state, index-aligned with state IDs.
    /// Populated by adapters that enumerate states from cross-product domains
    /// (SV Kripke, extraction). Enables structured predicate matching that avoids
    /// the underscore-delimiter ambiguity of state name parsing.
    state_valuations: Vec<Option<BTreeMap<String, String>>>,
    /// R.1 — 3-valued AP labellings per state (`docs/design/native-sv-abstraction.md` §6.3).
    /// Outer `Option` distinguishes "no 3-valued labelling configured"
    /// (the legacy 2-valued path; the `BoolDomain` evaluator uses
    /// `state_variables` / `state_valuations` directly) from "3-valued
    /// labelling populated by a KMTS-aware adapter" (each entry maps
    /// `(state_id, predicate_name)` to a [`Tristate`] verdict).
    ///
    /// Adapters that do not produce 3-valued labellings leave this as
    /// `None`; the `KleeneDomain` evaluator (R.3) falls back to the
    /// 2-valued state-variable bitsets in that case, yielding verdicts
    /// in `{KleeneT, KleeneF}` only (never `KleeneBot`).
    state_3valued_predicates: Option<BTreeMap<(StateId<S>, String), Tristate>>,
    // Pool of state sets.
    state_set_pool: Arc<StateSetPoolInner>,
    /// Explicit controllability classification for each label ID.
    /// This is set during CLTS construction based on explicit declarations
    /// or inferred from transition kinds.
    label_controllability: HashMap<LabelId<L>, LabelControllability>,
    /// Set of label IDs that are uncontrollable (derived from label_controllability).
    uncontrollable_alphabet: HashSet<LabelId<L>>,
    /// Set of label IDs that are controllable (derived from label_controllability).
    controllable_alphabet: HashSet<LabelId<L>>,
    /// Set of label IDs that are internal (derived from label_controllability).
    /// Internal actions are mutually exclusive between automata in composition.
    internal_alphabet: HashSet<LabelId<L>>,
    /// Pre-computed groups of transitions by uncontrollable labels, per state.
    ///
    /// For each state, this maps a canonical key (sorted vector of uncontrollable label IDs)
    /// to a list of indices into the `outgoing[state]` vector.
    ///
    /// This allows O(1) access to transitions grouped by uncontrollable labels,
    /// eliminating the need to recompute groups during μ-calculus evaluation.
    uncontrollable_groups: UncontrollableGroups<L>,
}

impl<S: IdStorage, L: IdStorage> Clts<S, L> {
    /// Creates a new builder for constructing a CLTS instance with explicit identifier types.
    pub fn builder_with_storage() -> CltsBuilder<S, L> {
        CltsBuilder::default()
    }

    /// Returns the number of states in the CLTS.
    pub fn state_count(&self) -> usize {
        self.state_names.len()
    }

    /// Returns the set of initial states.
    pub fn initial_states(&self) -> &HashSet<StateId<S>> {
        &self.initial_states
    }

    /// Resolves the identifier of the state with the provided name.
    ///
    /// # Coverage Status
    /// Covered by test: `state_id_error_handling`
    pub fn state_id(&self, name: &str) -> CltsResult<StateId<S>> {
        self.state_map
            .get(name)
            .copied()
            .ok_or_else(|| CltsError::UnknownState(name.to_owned()))
    }

    /// Returns the outgoing transitions of the given state.
    pub fn outgoing(&self, state: StateId<S>) -> &[Transition<S, L>] {
        &self.outgoing[state.index()]
    }

    /// Returns the incoming transitions of the given state.
    pub fn incoming(&self, state: StateId<S>) -> &[Transition<S, L>] {
        &self.incoming[state.index()]
    }

    /// Returns an iterator over the states of the CLTS in insertion order.
    ///
    /// # Coverage Status
    /// Covered by test: `states_iterator`
    pub fn states(&self) -> impl Iterator<Item = StateId<S>> + '_ {
        self.state_names
            .iter()
            .enumerate()
            .filter_map(|(idx, _)| StateId::new(idx))
    }

    /// Returns an iterator over all states paired with their outgoing transitions.
    ///
    /// This helper captures a very common pattern across the codebase:
    /// iterating over `clts.states()` and then calling `clts.outgoing(state)`
    /// for each state. Centralising it here avoids repeating that pattern and
    /// keeps transition access consistently routed through the CLTS API.
    pub fn state_outgoing_pairs(
        &self,
    ) -> impl Iterator<Item = (StateId<S>, &[Transition<S, L>])> + '_ {
        self.states()
            .map(move |state| (state, &self.outgoing[state.index()] as &[Transition<S, L>]))
    }

    /// Returns the original name associated with the provided state identifier.
    ///
    /// # Coverage Status
    /// Covered by test: `state_name_retrieval`
    pub fn state_name(&self, state: StateId<S>) -> Option<&str> {
        self.state_names
            .get(state.index())
            .map(|name| name.as_str())
    }

    /// Provides access to the label payload referenced by a label identifier.
    ///
    /// # Coverage Status
    /// Covered by test: `label_payload_access`
    pub fn label_payload(&self, label: LabelId<L>) -> Option<&[String]> {
        self.labels.get(label)
    }

    /// Returns a canonical bitset view for the given label handle.
    ///
    /// This allows inclusion/intersection checks without converting back to
    /// string payloads.
    pub fn label_bitset(&self, label: LabelId<L>) -> Option<LabelBitSet<'_>> {
        self.labels.bitset(label)
    }

    /// Returns a deduplicated list of all alphabet symbols (controllable, internal, and uncontrollable).
    ///
    /// This method returns the union of all labels used in the CLTS, regardless of controllability.
    /// For explicit controllability queries, use `controllable_alphabet()`, `internal_alphabet()`,
    /// or `uncontrollable_alphabet()`.
    pub fn alphabet(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut alphabet = Vec::new();

        // Use pre-computed alphabet sets instead of iterating through all transitions
        // This is more efficient and uses the source of truth
        for &label_id in self.controllable_alphabet() {
            if let Some(payload) = self.labels.get(label_id) {
                for symbol in payload {
                    if seen.insert(symbol.clone()) {
                        alphabet.push(symbol.clone());
                    }
                }
            }
        }
        for &label_id in self.internal_alphabet() {
            if let Some(payload) = self.labels.get(label_id) {
                for symbol in payload {
                    if seen.insert(symbol.clone()) {
                        alphabet.push(symbol.clone());
                    }
                }
            }
        }
        for &label_id in self.uncontrollable_alphabet() {
            if let Some(payload) = self.labels.get(label_id) {
                for symbol in payload {
                    if seen.insert(symbol.clone()) {
                        alphabet.push(symbol.clone());
                    }
                }
            }
        }

        alphabet
    }

    /// Returns the controllability classification for a label.
    pub fn label_controllability(&self, label: LabelId<L>) -> Option<LabelControllability> {
        self.label_controllability.get(&label).copied()
    }

    /// Returns the set of uncontrollable label IDs.
    ///
    /// A label is uncontrollable if it is explicitly marked as uncontrollable
    /// or appears on at least one uncontrollable transition.
    pub fn uncontrollable_alphabet(&self) -> &HashSet<LabelId<L>> {
        &self.uncontrollable_alphabet
    }

    /// Returns the set of controllable label IDs.
    ///
    /// A label is controllable if it is explicitly marked as controllable
    /// or appears on controllable transitions and is not marked as internal.
    pub fn controllable_alphabet(&self) -> &HashSet<LabelId<L>> {
        &self.controllable_alphabet
    }

    /// Returns the set of internal label IDs.
    ///
    /// Internal actions are mutually exclusive between automata in composition.
    pub fn internal_alphabet(&self) -> &HashSet<LabelId<L>> {
        &self.internal_alphabet
    }

    /// Returns transitions grouped by uncontrollable labels for a given state.
    ///
    /// This is pre-computed during CLTS construction for O(1) access.
    /// The returned map keys are canonical (sorted) vectors of uncontrollable label IDs,
    /// and values are indices into `self.outgoing(state)`.
    ///
    /// # Example
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// builder.state("s0").initial("s0");
    /// builder.state("s1");
    /// let clts = builder.build().unwrap();
    /// let state = clts.state_id("s0").unwrap();
    ///
    /// let groups = clts.transitions_grouped_by_uncontrollable_labels(state);
    /// for (uncontrollable_labels, transition_indices) in groups {
    ///     // Process all transitions that share these uncontrollable labels
    ///     for &idx in transition_indices {
    ///         let transition = &clts.outgoing(state)[idx];
    ///         // ...
    ///     }
    /// }
    /// ```
    pub fn transitions_grouped_by_uncontrollable_labels(
        &self,
        state: StateId<S>,
    ) -> &HashMap<SmallVec<[LabelId<L>; 4]>, Vec<usize>> {
        &self.uncontrollable_groups[state.index()]
    }

    /// Checks if a label ID is uncontrollable.
    pub fn is_uncontrollable_label(&self, label: LabelId<L>) -> bool {
        self.uncontrollable_alphabet.contains(&label)
    }

    /// Checks if a label ID is controllable.
    pub fn is_controllable_label(&self, label: LabelId<L>) -> bool {
        self.controllable_alphabet.contains(&label)
    }

    /// Checks if a label ID is internal.
    pub fn is_internal_label(&self, label: LabelId<L>) -> bool {
        self.internal_alphabet.contains(&label)
    }

    /// Returns all variable names in the CLTS universe.
    pub fn variables(&self) -> Vec<String> {
        self.variables.all()
    }

    pub fn state_variables(&self, state: StateId<S>) -> Vec<String> {
        let id = self.state_variables[state.index()];
        self.variables.get(id)
    }

    /// Returns the canonical bitset view of the variables associated with a state.
    ///
    /// Algorithms can use this to run subset/intersection checks directly.
    pub fn state_variable_bitset(&self, state: StateId<S>) -> VariableBitSet<'_> {
        let id = self.state_variables[state.index()];
        self.variables.bitset(id)
    }

    /// Returns the structured valuation (variable-name → display-value pairs) for a state,
    /// if one was provided during construction.
    ///
    /// Adapters that enumerate states from cross-product domains (SV Kripke, extraction)
    /// populate these valuations. Native CTXDSL states typically have `None`.
    pub fn state_valuation(&self, state: StateId<S>) -> Option<&BTreeMap<String, String>> {
        self.state_valuations
            .get(state.index())
            .and_then(|v| v.as_ref())
    }

    /// Returns `true` if any state has a structured valuation attached.
    pub fn has_valuations(&self) -> bool {
        self.state_valuations.iter().any(|v| v.is_some())
    }

    /// R.1 — Returns the 3-valued labelling of `predicate` at `state`,
    /// if a KMTS-aware adapter has populated the
    /// [`Clts::state_3valued_predicates`] map. Returns `None` when no
    /// 3-valued labelling was configured (the legacy 2-valued path)
    /// or when this particular `(state, predicate)` pair is missing
    /// (the `KleeneDomain` evaluator interprets that as
    /// `Tristate::KleeneBot` — the abstraction is too coarse here).
    pub fn state_3valued_predicate(&self, state: StateId<S>, predicate: &str) -> Option<Tristate> {
        self.state_3valued_predicates
            .as_ref()
            .and_then(|m| m.get(&(state, predicate.to_string())))
            .copied()
    }

    /// R-MM — Returns every 3-valued predicate labelling attached to
    /// `state`, as `(predicate_name, verdict)` pairs in predicate-name
    /// order (empty when none are set).
    ///
    /// The per-`(state, predicate)` map is private; composition needs to
    /// *enumerate* a state's predicates (not just query one by name) to
    /// carry them onto product states, so this accessor exposes the
    /// per-state slice. Backed by a `BTreeMap` range, so it touches only
    /// the entries for `state`, not the whole map.
    pub fn state_3valued_predicate_entries(&self, state: StateId<S>) -> Vec<(&str, Tristate)> {
        match &self.state_3valued_predicates {
            Some(map) => map
                .range((state, String::new())..)
                .take_while(move |((s, _), _)| *s == state)
                .map(|((_, name), verdict)| (name.as_str(), *verdict))
                .collect(),
            None => Vec::new(),
        }
    }

    /// R.1 — Returns `true` if any 3-valued predicate labelling has
    /// been populated. `KleeneDomain` callers can branch on this to
    /// fall back to the 2-valued `state_variables` path when the
    /// CLTS came from a legacy adapter.
    pub fn has_3valued_predicates(&self) -> bool {
        self.state_3valued_predicates
            .as_ref()
            .is_some_and(|m| !m.is_empty())
    }

    /// Returns `true` if `name` is a 3-valued predicate labelled on any
    /// state — i.e. a formula atom referencing it is resolvable by the
    /// `KleeneDomain` evaluator's `predicate_bits` (which reads
    /// [`Clts::state_3valued_predicate`] by name). IR-track P3.3 uses this
    /// so the realize-time formula-predicate validation accepts the
    /// predicate-cube's Kleene labels (round-tripped through CTXDSL's
    /// `predicates_3v` blocks) as referenceable predicates.
    pub fn has_3valued_predicate_named(&self, name: &str) -> bool {
        self.state_3valued_predicates
            .as_ref()
            .is_some_and(|m| m.keys().any(|(_, n)| n == name))
    }

    /// Borrows a reusable state-set bit vector sized to this CLTS instance.
    ///
    /// The returned [`StateSet`] clears its contents upon `Drop` and returns the
    /// underlying buffer to the internal pool, keeping allocation churn flat
    /// during fixpoint computations.
    pub fn state_set(&self) -> StateSet<S> {
        let bits = self.state_set_pool.acquire();
        StateSet {
            bits,
            pool: Arc::clone(&self.state_set_pool),
            marker: PhantomData,
        }
    }

    /// Returns the number of bits backed by the pooled state sets.
    pub fn state_set_len(&self) -> usize {
        self.state_set_pool.state_count()
    }

    /// Computes a structural hash of this CLTS. Two CLTS instances that are
    /// structurally equivalent (same states, transitions, label/variable
    /// stores) will produce the same digest.
    ///
    /// # Hashing Strategy
    ///
    /// The hash is computed from the structural components of the CLTS:
    /// 1. **State names**: Ordered list of state names
    /// 2. **Initial states**: Bitset representation of initial states
    /// 3. **Label bitsets**: Bitset representation of all label symbol sets
    /// 4. **Variable bitsets**: Bitset representation of all variable sets
    /// 5. **State variables**: Bitset representation of variables per state
    /// 6. **Transitions**: Fingerprints (target state + sorted label bitsets) per state
    ///
    /// This ensures that structurally equivalent CLTSs (same graph structure, labels,
    /// and variables) produce the same hash, regardless of construction order or internal
    /// representation details.
    ///
    /// # Hash Algorithm
    /// Uses Rust's `DefaultHasher` (SipHash-1-3), which provides good distribution
    /// and resistance to hash collision attacks.
    ///
    /// # Coverage Status
    /// Covered by test: `structural_hash_consistency`
    pub fn structural_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash_structure(&mut hasher);
        hasher.finish()
    }

    /// Checks structural equality against another CLTS instance.
    ///
    /// Two CLTSs are considered structurally equal if they have:
    /// 1. **Same state names**: Identical ordered list of state names
    /// 2. **Same initial states**: Same bitset of initial states
    /// 3. **Same label bitsets**: Identical bitset representation of all labels
    /// 4. **Same variable bitsets**: Identical bitset representation of all variable sets
    /// 5. **Same state variables**: Identical bitset representation of variables per state
    /// 6. **Same transitions**: Identical transition fingerprints (target + sorted label bitsets) per state
    ///
    /// # Equality Strategy
    ///
    /// The comparison uses bitset representations for efficient comparison:
    /// - **Initial states**: Compared as bitsets (bitwise equality)
    /// - **Labels/Variables**: Compared as bitsets (bitwise equality)
    /// - **Transitions**: Compared as sorted fingerprints (target state + sorted label bitsets)
    ///
    /// This ensures that structurally equivalent CLTSs are considered equal, regardless
    /// of construction order or internal representation details.
    ///
    /// # Performance
    /// O(n + m + l + v) where:
    /// - n = number of states
    /// - m = number of transitions
    /// - l = number of labels
    /// - v = number of variable sets
    ///
    /// # Coverage Status
    /// Covered by test: `structural_eq_comparison`
    pub fn structural_eq(&self, other: &Self) -> bool {
        if self.state_names != other.state_names {
            return false;
        }
        if self.initial_state_bits() != other.initial_state_bits() {
            return false;
        }
        if self.labels.bitsets != other.labels.bitsets {
            return false;
        }
        if self.variables.bitsets != other.variables.bitsets {
            return false;
        }

        for idx in 0..self.state_names.len() {
            let state = StateId::new(idx).unwrap();
            if self.state_variable_bitset(state).bits() != other.state_variable_bitset(state).bits()
            {
                return false;
            }

            if self.transition_fingerprints(idx) != other.transition_fingerprints(idx) {
                return false;
            }
        }

        true
    }

    /// Hashes the structural components of the CLTS into the provided hasher.
    ///
    /// This is the internal implementation used by `structural_hash()`. It hashes:
    /// 1. State names (ordered)
    /// 2. Initial states (as bitset)
    /// 3. Label bitsets (all label symbol sets)
    /// 4. Variable bitsets (all variable sets)
    /// 5. State variables (bitset per state)
    /// 6. Transition fingerprints (target + sorted label bitsets per state)
    ///
    /// # Hash Ordering
    /// The order of hashing is deterministic and ensures that structurally equivalent
    /// CLTSs produce the same hash regardless of construction order.
    fn hash_structure<H: Hasher>(&self, state: &mut H) {
        self.state_names.hash(state);
        self.initial_state_bits().hash(state);
        self.labels.bitsets.hash(state);
        self.variables.bitsets.hash(state);
        for idx in 0..self.state_names.len() {
            let state_id = StateId::new(idx).unwrap();
            self.state_variable_bitset(state_id).bits().hash(state);
            self.transition_fingerprints(idx).hash(state);
        }
    }

    /// Converts the set of initial states into a bitset representation.
    ///
    /// This creates a bitset where bit `i` is set if state with index `i` is an initial state.
    /// Used for efficient comparison and hashing of initial state sets.
    ///
    /// # Bit Representation
    /// - `bits[i] = true` → state `i` is initial
    /// - `bits[i] = false` → state `i` is not initial
    fn initial_state_bits(&self) -> BitVec<usize, Lsb0> {
        let mut bits = bitvec![usize, Lsb0; 0; self.state_names.len()];
        for state in &self.initial_states {
            bits.set(state.index(), true);
        }
        bits
    }

    /// Computes a fingerprint of the transitions outgoing from a given state.
    ///
    /// A transition fingerprint consists of:
    /// - **Target state**: The index of the destination state
    /// - **Label bitsets**: The bitset representation of each label on the transition,
    ///   sorted for canonical ordering
    ///
    /// The fingerprints are sorted to ensure deterministic ordering for equality and hashing.
    /// This allows comparing transitions regardless of the order they were added.
    ///
    /// # Fingerprint Structure
    /// Each `TransitionFingerprint` contains:
    /// - `target`: State index of the destination
    /// - `labels`: Sorted vector of label bitsets (one bitset per label)
    ///
    /// # Use Cases
    /// - **Equality checking**: Compare transitions between CLTSs
    /// - **Hashing**: Include transitions in structural hash
    /// - **Canonical representation**: Normalize transition order for comparison
    ///
    /// # Performance
    /// O(t * l) where t = number of transitions, l = average labels per transition
    fn transition_fingerprints(&self, state_idx: usize) -> Vec<TransitionFingerprint> {
        let state_id = StateId::new(state_idx).unwrap();
        let mut data: Vec<TransitionFingerprint> = self
            .outgoing(state_id)
            .iter()
            .map(|transition| {
                let mut labels: Vec<BitVec<usize, Lsb0>> = transition
                    .labels()
                    .iter()
                    .filter_map(|label_id| {
                        self.label_bitset(*label_id).map(|bs| bs.bits().to_bitvec())
                    })
                    .collect();
                labels.sort();
                TransitionFingerprint {
                    target: transition.target().index(),
                    labels,
                }
            })
            .collect();
        data.sort();
        data
    }

    pub(crate) fn label_entries(&self) -> impl Iterator<Item = &[String]> {
        self.labels.entries()
    }
}

impl<S: IdStorage, L: IdStorage> Drop for Clts<S, L> {
    /// Custom drop implementation to avoid stack overflow when dropping large CLTSs.
    ///
    /// The default drop implementation can cause stack overflow when dropping CLTSs with
    /// many states (e.g., 2000+ states) because it recursively drops deeply nested structures:
    /// - `outgoing: Vec<Vec<Transition>>` - one Vec per state
    /// - `incoming: Vec<Vec<Transition>>` - one Vec per state
    /// - `uncontrollable_groups: Vec<HashMap<...>>` - one HashMap per state
    ///
    /// This implementation manually clears these structures in chunks to avoid deep recursion.
    fn drop(&mut self) {
        // Use smaller chunks to reduce stack depth even further
        const CHUNK_SIZE: usize = 50;

        // Clear outgoing transitions in chunks
        // Use take to move the Vec out, then drop it separately
        let mut outgoing = std::mem::take(&mut self.outgoing);
        for chunk in outgoing.chunks_mut(CHUNK_SIZE) {
            for vec in chunk {
                vec.clear();
            }
        }
        drop(outgoing);

        // Clear incoming transitions in chunks
        let mut incoming = std::mem::take(&mut self.incoming);
        for chunk in incoming.chunks_mut(CHUNK_SIZE) {
            for vec in chunk {
                vec.clear();
            }
        }
        drop(incoming);

        // Clear uncontrollable_groups in chunks
        let mut uncontrollable_groups = std::mem::take(&mut self.uncontrollable_groups);
        for chunk in uncontrollable_groups.chunks_mut(CHUNK_SIZE) {
            for map in chunk {
                map.clear();
            }
        }
        drop(uncontrollable_groups);

        // Clear other potentially large structures
        std::mem::take(&mut self.state_names);
        std::mem::take(&mut self.state_map);
        std::mem::take(&mut self.initial_states);
        std::mem::take(&mut self.state_variables);
        std::mem::take(&mut self.state_valuations);
        std::mem::take(&mut self.label_controllability);
        std::mem::take(&mut self.uncontrollable_alphabet);
        std::mem::take(&mut self.controllable_alphabet);
        std::mem::take(&mut self.internal_alphabet);

        // The remaining structures (labels, variables, state_set_pool) should be safe to drop
        // as they don't have the same deep nesting pattern
    }
}

impl Clts<DefaultStateIdx, DefaultLabelIdx> {
    /// Creates a standalone builder using the default identifier widths.
    ///
    /// Prefer this for focused unit tests or one-off CLTS construction when
    /// shared label interning is unnecessary. To build CLTSs that participate in
    /// a `Context`, call [`Context::new_clts_builder`](crate::context::Context::new_clts_builder)
    /// so label handles stay aligned across the registry.
    pub fn builder() -> CltsBuilder<DefaultStateIdx, DefaultLabelIdx> {
        CltsBuilder::default()
    }

    /// R-MM — Rebuilds this CLTS with label symbols renamed per `rename`.
    ///
    /// Every symbol in every transition's label payload is mapped through
    /// `rename` (symbols absent from the map are kept verbatim), then the
    /// renamed symbol set is re-interned. The state space is unchanged;
    /// only label payloads differ.
    ///
    /// This is the load-bearing primitive for the **KMTS multi-module
    /// composition driver** (R-MM): each per-module KMTS lifted from a
    /// submodule's BTOR2 carries its *port* names as labels, but two
    /// instances connected by a shared net must rendezvous on the *net*
    /// name. `relabel` rewrites each instance's port labels to the
    /// connected-net names so [`crate::composition::compose`]'s
    /// name-equality rendezvous fires on the shared nets.
    ///
    /// Unlike [`crate::composition::hide::hide_labels`] (which splits a
    /// multi-label edge into one edge per label and only carries
    /// controllability), `relabel` preserves the **exact edge structure**:
    /// a multi-label edge stays one edge, and crucially the transition's
    /// [`TransitionModality`] (may / must / hyper-must) is carried through
    /// rather than collapsed to `Sharp` — relabelling a KMTS must not
    /// silently fabricate must-witnesses. Structured state valuations and
    /// 3-valued predicate labellings are copied verbatim (the abstract
    /// AP labels the composed property reads).
    pub fn relabel(
        &self,
        rename: &HashMap<String, String>,
    ) -> CltsResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
        let mut builder = Clts::builder();

        // 1. Copy states (names, initial flags, variable name-sets).
        let mut state_mapping: HashMap<StateId<DefaultStateIdx>, StateId<DefaultStateIdx>> =
            HashMap::new();
        for state in self.states() {
            let name = self.state_name(state).unwrap_or("state").to_owned();
            if let Some(new_id) = builder.state_with_name(name) {
                if self.initial_states().contains(&state) {
                    builder.initial_state_id(new_id);
                }
                let vars = self.state_variables(state);
                if !vars.is_empty() {
                    builder.with_variables_for_state(new_id, vars.iter().map(|s| s.as_str()));
                }
                state_mapping.insert(state, new_id);
            }
        }

        // 2. Copy transitions — rename label symbols, preserve the
        //    multi-label edge shape AND the modality (may/must).
        for state in self.states() {
            let &source_new = match state_mapping.get(&state) {
                Some(id) => id,
                None => continue,
            };
            for transition in self.outgoing(state) {
                let &target_new = match state_mapping.get(&transition.target()) {
                    Some(id) => id,
                    None => continue,
                };
                let mut new_labels: SmallVec<[LabelId<DefaultLabelIdx>; 4]> = SmallVec::new();
                for &label_id in transition.labels() {
                    let payload = match self.label_payload(label_id) {
                        Some(p) => p,
                        None => continue,
                    };
                    let renamed: Vec<String> = payload
                        .iter()
                        .map(|s| rename.get(s).cloned().unwrap_or_else(|| s.clone()))
                        .collect();
                    let new_label_id = builder
                        .labels()
                        .intern(renamed.iter().map(|s| s.as_str()))?;
                    if let Some(ctrl) = self.label_controllability(label_id) {
                        builder.set_label_controllability(new_label_id, ctrl);
                    }
                    new_labels.push(new_label_id);
                }
                builder.transition_ids_with_modality(
                    source_new,
                    &new_labels,
                    target_new,
                    transition.modality().clone(),
                );
            }
        }

        // 3. Copy structured state valuations (display-only abstract values).
        for state in self.states() {
            if let (Some(&new_id), Some(valuation)) =
                (state_mapping.get(&state), self.state_valuation(state))
            {
                builder.with_valuation_for_state(new_id, valuation.clone());
            }
        }

        // 4. Copy 3-valued predicate labellings (the AP labels the
        //    KleeneDomain evaluator reads on the composed product).
        if let Some(map) = &self.state_3valued_predicates {
            for ((state, predicate), verdict) in map {
                if let Some(&new_id) = state_mapping.get(state) {
                    builder.with_3valued_predicate(new_id, predicate.clone(), *verdict);
                }
            }
        }

        builder.build()
    }
}

#[derive(Debug)]
/// Builder type used to construct [`Clts`] instances.
pub struct CltsBuilder<S: IdStorage = DefaultStateIdx, L: IdStorage = DefaultLabelIdx> {
    state_names: Vec<String>,
    state_map: HashMap<String, StateId<S>>,
    // Determines whether the state identifier space has overflowed.
    state_overflow: Option<usize>,
    initial_states: HashSet<StateId<S>>,
    transitions: Vec<TransitionSpec<S, L>>,
    labels: LabelStoreBuilder<L>,
    variables: VariableStoreBuilder,
    state_variables: Vec<VariableSetId>,
    /// Structured variable-value pairs per state, index-aligned.
    state_valuations: Vec<Option<BTreeMap<String, String>>>,
    /// R.1 — staged 3-valued predicate labellings; lifted into the
    /// built [`Clts::state_3valued_predicates`] field unchanged.
    /// `None` is the default; KMTS-aware adapters populate via
    /// [`CltsBuilder::with_3valued_predicate`].
    state_3valued_predicates: Option<BTreeMap<(StateId<S>, String), Tristate>>,
    // EXP-0004: capacity-hint fields removed. Vec::push handles growth
    // by amortized doubling; explicit pre-sizing goes through
    // `reserve_states` / `reserve_transitions`.
    // Explicit controllability classification for labels, set via `set_label_controllability`.
    /// If not set, controllability is inferred from transition kinds during build.
    label_controllability: HashMap<LabelId<L>, LabelControllability>,
    /// mununu#592 — labels DECLARED part of the alphabet whether or not a transition uses them
    /// (a CTXDSL `controllable { label x; }` with no `x` edge). See
    /// [`CltsBuilder::declare_in_alphabet`].
    declared_labels: HashSet<LabelId<L>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransitionSpec<S: IdStorage, L: IdStorage> {
    from: StateId<S>,
    to: StateId<S>,
    labels: SmallVec<[LabelId<L>; 4]>,
    /// R.1 — KMTS modality. `Sharp` by default for the legacy
    /// 2-valued path; `MayOnly` for KMTS-aware adapters that opt
    /// in via [`CltsBuilder::transition_ids_with_modality`].
    /// R.4.5 adds `MustHyperOnly` for GKMTS-aware adapters that
    /// produce hyper-must transitions (R.5 / R.5b paths).
    modality: TransitionModality<S>,
}

impl<S: IdStorage, L: IdStorage> CltsBuilder<S, L> {
    /// Inserts a state by owned name, returning its identifier when successful
    /// or `None` if the identifier space overflows.
    fn insert_state_owned(&mut self, name: String) -> Option<StateId<S>> {
        if let Some(&id) = self.state_map.get(name.as_str()) {
            return Some(id);
        }

        let next_index = self.state_names.len();
        match StateId::new(next_index) {
            Some(id) => {
                self.state_variables.push(self.variables.empty());
                self.state_valuations.push(None);
                self.state_map.insert(name.clone(), id);
                self.state_names.push(name);
                Some(id)
            }
            None => {
                self.state_overflow.get_or_insert(next_index);
                None
            }
        }
    }

    // EXP-0004: ensure_*_capacity removed in favor of Vec::push's
    // amortized doubling. Each parallel Vec (state_names, state_variables,
    // state_valuations, transitions) grows independently when push hits
    // capacity. Callers who know their final size pre-allocate via
    // `reserve_states` / `reserve_transitions`.

    /// Adds a state to the CLTS if it does not already exist.
    ///
    /// This method is designed for fluent builder-style chaining and returns `&mut Self`
    /// to allow method chaining. It accepts any type that can be converted to `&str`.
    ///
    /// # When to use
    ///
    /// Use this method when you want to add states in a fluent builder pattern and don't
    /// need the state identifier immediately. For other use cases, see:
    /// - [`state_with_name()`](Self::state_with_name): When you already have an owned `String`
    /// - [`state_id_or_insert()`](Self::state_id_or_insert): When you need the state identifier
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Fluent chaining - use state()
    /// builder.state("s0").state("s1").initial("s0");
    /// ```
    pub fn state<Str: AsRef<str>>(&mut self, name: Str) -> &mut Self {
        let owned = name.as_ref().to_owned();
        let _ = self.insert_state_owned(owned);
        self
    }

    /// Inserts a state with the provided owned name and returns its identifier.
    ///
    /// This helper avoids re-hashing the same string when callers already have
    /// ownership of the string, such as benchmarks that materialise names in
    /// buffers or when processing state names from external sources.
    ///
    /// # When to use
    ///
    /// Use this method when you already have an owned `String` and want to avoid
    /// the allocation overhead of `to_owned()`. For other use cases, see:
    /// - [`state()`](Self::state): For fluent builder-style chaining with `&str`-like types
    /// - [`state_id_or_insert()`](Self::state_id_or_insert): When you need the identifier but have `&str`
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Already have an owned String - use state_with_name()
    /// let state_name = format!("state_{}", 42);
    /// if let Some(state_id) = builder.state_with_name(state_name) {
    ///     // Use state_id for further operations
    /// }
    /// ```
    pub fn state_with_name(&mut self, name: String) -> Option<StateId<S>> {
        self.insert_state_owned(name)
    }

    /// Ensures a state exists and returns its identifier.
    ///
    /// The state is created if it was not already present. This method accepts any
    /// type that can be converted to `&str` and always returns the state identifier.
    ///
    /// # When to use
    ///
    /// Use this method when you need the state identifier and have a `&str`-like type.
    /// For other use cases, see:
    /// - [`state()`](Self::state): For fluent builder-style chaining when you don't need the ID
    /// - [`state_with_name()`](Self::state_with_name): When you already have an owned `String`
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Need the state ID - use state_id_or_insert()
    /// if let Some(state_id) = builder.state_id_or_insert("s0") {
    ///     builder.initial_state_id(state_id);
    ///     // Use state_id for transitions, etc.
    /// }
    /// ```
    pub fn state_id_or_insert<Str: AsRef<str>>(&mut self, name: Str) -> Option<StateId<S>> {
        self.insert_state_owned(name.as_ref().to_owned())
    }

    /// Reserves additional capacity for state-centric data structures. Useful
    /// when the expected state count is known ahead of time.
    pub fn reserve_states(&mut self, additional: usize) -> &mut Self {
        self.state_names.reserve(additional);
        self.state_variables.reserve(additional);
        self.state_valuations.reserve(additional);
        self.state_map.reserve(additional);
        self.initial_states.reserve(additional);
        self
    }

    /// Reserves additional capacity for staged transitions.
    pub fn reserve_transitions(&mut self, additional: usize) -> &mut Self {
        self.transitions.reserve(additional);
        self
    }

    /// Starts from an externally prepared label store builder.
    ///
    /// Contexts that manage a shared alphabet can supply a pre-populated
    /// `LabelStoreBuilder`, allowing multiple CLTS instances to reuse the same
    /// interned label universe. The provided builder is moved into the CLTS
    /// builder, so contexts typically clone their canonical builder before
    /// handing it off when they need to keep interning labels for later CLTSs.
    pub fn with_label_store(label_store: LabelStoreBuilder<L>) -> Self {
        Self {
            labels: label_store,
            ..Self::default()
        }
    }

    /// Marks a state as initial, creating it if necessary.
    ///
    /// This method is designed for fluent builder-style chaining and accepts any type
    /// that can be converted to `&str`. The state is created if it doesn't exist.
    ///
    /// # When to use
    ///
    /// Use this method when you want to mark states as initial in a fluent builder pattern
    /// and have the state name as a string reference. For other use cases, see:
    /// - [`initial_state_id()`](Self::initial_state_id): When you already have a `StateId`
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Fluent chaining - use initial()
    /// builder.state("s0").initial("s0").state("s1");
    /// ```
    pub fn initial<Str: AsRef<str>>(&mut self, name: Str) -> &mut Self {
        let owned = name.as_ref().to_owned();
        if let Some(id) = self.insert_state_owned(owned) {
            self.initial_states.insert(id);
        }
        self
    }

    /// Marks a state as initial using an existing identifier.
    ///
    /// This method avoids a hash map lookup when you already have the state identifier,
    /// which is useful when building states programmatically or when you've obtained
    /// the identifier from a previous operation.
    ///
    /// Callers are responsible for ensuring the identifier refers to a state in
    /// this builder; a `debug_assert!` guards misuse in debug builds.
    ///
    /// # When to use
    ///
    /// Use this method when you already have a `StateId` (e.g., from `state_with_name()`
    /// or `state_id_or_insert()`). For other use cases, see:
    /// - [`initial()`](Self::initial): When you have the state name as a string reference
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// // Already have the state ID - use initial_state_id()
    /// if let Some(state_id) = builder.state_id_or_insert("s0") {
    ///     builder.initial_state_id(state_id);  // Avoids hash lookup
    /// }
    /// ```
    pub fn initial_state_id(&mut self, state: StateId<S>) -> &mut Self {
        debug_assert!(state.index() < self.state_variables.len());
        self.initial_states.insert(state);
        self
    }

    /// Binds a set of variables to the named state, creating the state if needed.
    pub fn with_variables<Str, Var, I>(&mut self, name: Str, vars: I) -> &mut Self
    where
        Str: AsRef<str>,
        Var: AsRef<str>,
        I: IntoIterator<Item = Var>,
    {
        let owned = name.as_ref().to_owned();
        if let Some(state_id) = self.insert_state_owned(owned) {
            let mut buffer = self.variables.buffer_pool.acquire(DEFAULT_VARIABLE_RESERVE);
            buffer.extend(vars.into_iter().map(|v| v.as_ref().to_owned()));
            let set_id = self.variables.intern_in_place(&mut buffer);
            if let Some(slot) = self.state_variables.get_mut(state_id.index()) {
                *slot = set_id;
            }
            self.variables.buffer_pool.release(buffer);
        }
        self
    }

    /// Binds a set of variables directly using the state's identifier.
    ///
    /// This avoids a secondary hash-map lookup when the caller already holds
    /// the canonical identifier.
    pub fn with_variables_for_state<Var, I>(&mut self, state: StateId<S>, vars: I) -> &mut Self
    where
        Var: AsRef<str>,
        I: IntoIterator<Item = Var>,
    {
        debug_assert!(state.index() < self.state_variables.len());
        let mut buffer = self.variables.buffer_pool.acquire(DEFAULT_VARIABLE_RESERVE);
        buffer.extend(vars.into_iter().map(|v| v.as_ref().to_owned()));
        let set_id = self.variables.intern_in_place(&mut buffer);
        if let Some(slot) = self.state_variables.get_mut(state.index()) {
            *slot = set_id;
        }
        self.variables.buffer_pool.release(buffer);
        self
    }

    /// Attaches a structured valuation (variable-name → display-value map) to a state.
    ///
    /// Adapters that enumerate states from cross-product domains (e.g., SV Kripke,
    /// extraction) use this to record the variable values that define each state.
    /// This enables structured predicate matching that avoids the underscore-delimiter
    /// ambiguity of state name parsing.
    pub fn with_valuation_for_state(
        &mut self,
        state: StateId<S>,
        valuation: BTreeMap<String, String>,
    ) -> &mut Self {
        debug_assert!(state.index() < self.state_valuations.len());
        if let Some(slot) = self.state_valuations.get_mut(state.index()) {
            *slot = Some(valuation);
        }
        self
    }

    /// R.1 — Attaches a 3-valued labelling for `(state, predicate)`
    /// to the staged CLTS. Used by KMTS-aware adapters (R.2+ BTOR2
    /// lifter) when an abstract state's predicate verdict is
    /// `KleeneT` / `KleeneF` (sharp) or `KleeneBot` (uncertain).
    ///
    /// Repeated calls on the same `(state, predicate)` pair
    /// overwrite the previous value — adapters layering refinement
    /// steps should write the *final* verdict for each pair before
    /// `build()`.
    pub fn with_3valued_predicate(
        &mut self,
        state: StateId<S>,
        predicate: impl Into<String>,
        verdict: Tristate,
    ) -> &mut Self {
        let map = self
            .state_3valued_predicates
            .get_or_insert_with(BTreeMap::new);
        map.insert((state, predicate.into()), verdict);
        self
    }

    /// Adds a transition between two named states with the provided labels.
    /// Transition controllability is derived from label controllability.
    pub fn transition<Str: AsRef<str>>(
        &mut self,
        from: Str,
        labels: &[LabelId<L>],
        to: Str,
    ) -> &mut Self {
        let from_owned = from.as_ref().to_owned();
        let to_owned = to.as_ref().to_owned();
        if let (Some(from_id), Some(to_id)) = (
            self.insert_state_owned(from_owned),
            self.insert_state_owned(to_owned),
        ) {
            self.transition_ids(from_id, labels, to_id);
        }
        self
    }

    /// Adds a transition using state identifiers directly.
    ///
    /// Transitions are staged and later copied into adjacency lists when
    /// [`CltsBuilder::build`] is called.
    /// Transition controllability is derived from label controllability.
    ///
    /// R.1 — The transition is created with `TransitionModality::Sharp`
    /// (the default; both `may` and `must` capabilities). KMTS-aware
    /// callers that need to mark a transition as `MayOnly` use
    /// [`CltsBuilder::transition_ids_with_modality`] instead.
    pub fn transition_ids(
        &mut self,
        from: StateId<S>,
        labels: &[LabelId<L>],
        to: StateId<S>,
    ) -> &mut Self {
        self.transition_ids_with_modality(from, labels, to, TransitionModality::Sharp)
    }

    /// R.1 — Like [`CltsBuilder::transition_ids`] but takes an
    /// explicit [`TransitionModality`] so KMTS-aware adapters (the
    /// future R.2 BTOR2 lifter) can mark over-approximation edges
    /// as `MayOnly`. Default-modality (Sharp) callers use the
    /// shorter [`CltsBuilder::transition_ids`].
    pub fn transition_ids_with_modality(
        &mut self,
        from: StateId<S>,
        labels: &[LabelId<L>],
        to: StateId<S>,
        modality: TransitionModality<S>,
    ) -> &mut Self {
        debug_assert!(from.index() < self.state_variables.len());
        debug_assert!(to.index() < self.state_variables.len());
        let small_vec: SmallVec<[LabelId<L>; 4]> = labels.iter().copied().collect();
        self.transitions.push(TransitionSpec {
            from,
            to,
            labels: small_vec,
            modality,
        });
        self
    }

    /// Provides mutable access to the label store builder.
    pub fn labels(&mut self) -> &mut LabelStoreBuilder<L> {
        &mut self.labels
    }

    /// Sets the controllability classification for a label.
    ///
    /// This should be called before building the CLTS to explicitly declare
    /// label controllability. If not set, controllability is inferred from
    /// transition kinds during build.
    ///
    /// # Example
    /// ```rust
    /// use mununu_core::clts::{Clts, LabelControllability};
    ///
    /// let mut builder = Clts::builder();
    /// let label = builder.labels().intern(["input_signal"]).unwrap();
    /// builder.set_label_controllability(label, LabelControllability::Uncontrollable);
    /// ```
    pub fn set_label_controllability(
        &mut self,
        label: LabelId<L>,
        controllability: LabelControllability,
    ) -> &mut Self {
        self.label_controllability.insert(label, controllability);
        self
    }

    /// mununu#592 — declare `label` part of this CLTS's alphabet whether or not a transition
    /// uses it. The alphabet sets are otherwise built from the transitions, so a label an
    /// automaton DECLARES (a CTXDSL `controllable { label x; }`) but never offers would vanish —
    /// and under composition a partner would fire it freely, where an alphabet-based parallel
    /// composition BLOCKS an action a member's alphabet holds and the member does not offer.
    /// Its classification comes from [`Self::set_label_controllability`] (controllable by
    /// default, like any other label).
    pub fn declare_in_alphabet(&mut self, label: LabelId<L>) -> &mut Self {
        self.declared_labels.insert(label);
        self
    }

    /// Finalises the CLTS, returning an error if the configuration is invalid.
    ///
    /// This method constructs the final CLTS instance from the builder's internal
    /// state. It performs the following steps:
    /// 1. Validates the state identifier space to ensure no overflow has occurred.
    /// 2. Pre-sizes the adjacency lists to avoid frequent reallocations during the tight inner loop.
    /// 3. Builds the explicit controllability map from the builder's internal state.
    /// 4. Builds the alphabet sets based on the label controllability classifications.
    /// 5. Pre-computes the uncontrollable groups for each state.
    /// 6. Returns the constructed CLTS instance.
    ///
    /// # Errors
    ///
    /// This method returns an error if the state identifier space has overflowed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    ///
    /// let mut builder = Clts::builder();
    /// builder.state("s0").initial("s0");
    /// builder.state("s1");
    /// let input_label = builder.labels().intern(["input"]).expect("input label");
    /// let output_label = builder.labels().intern(["output"]).expect("output label");
    /// let internal_label = builder.labels().intern(["internal"]).expect("internal label");
    /// builder.transition("s0", &[input_label], "s1");
    /// builder.transition("s0", &[output_label], "s1");
    /// builder.transition("s0", &[internal_label], "s1");
    ///
    /// let clts = builder.build().unwrap();
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics if the state identifier space has overflowed.
    ///
    /// # Safety
    ///
    /// This method is safe to call.
    pub fn build(self) -> CltsResult<Clts<S, L>> {
        if let Some(value) = self.state_overflow {
            return Err(CltsError::IdOverflow {
                kind: "state",
                value,
            });
        }

        let state_count = self.state_names.len();
        let mut outgoing_counts = vec![0usize; state_count];
        let mut incoming_counts = vec![0usize; state_count];
        for spec in &self.transitions {
            outgoing_counts[spec.from.index()] += 1;
            incoming_counts[spec.to.index()] += 1;
        }

        // Pre-size adjacency lists so we can push transitions without triggering
        // additional allocations during the tight inner loop below.
        let mut outgoing: Vec<Vec<Transition<S, L>>> = outgoing_counts
            .into_iter()
            .map(Vec::with_capacity)
            .collect();
        let mut incoming: Vec<Vec<Transition<S, L>>> = incoming_counts
            .into_iter()
            .map(Vec::with_capacity)
            .collect();
        for spec in &self.transitions {
            outgoing[spec.from.index()].push(Transition {
                target: spec.to,
                labels: spec.labels.clone(),
                modality: spec.modality.clone(),
            });
            incoming[spec.to.index()].push(Transition {
                target: spec.from,
                labels: spec.labels.clone(),
                modality: spec.modality.clone(),
            });
        }

        let label_store = self.labels.build();
        let variable_store = self.variables.build();
        let state_set_pool = Arc::new(StateSetPoolInner::new(state_count));

        // Step 1: Build explicit controllability map
        let label_controllability: HashMap<LabelId<L>, LabelControllability> =
            self.label_controllability.clone();

        // Step 2: Build alphabet sets based on label controllability classifications
        // Collect all labels used in transitions, plus (mununu#592) the labels DECLARED in the
        // alphabet without a transition (`declare_in_alphabet`).
        let mut all_labels: HashSet<LabelId<L>> = HashSet::new();
        for spec in &self.transitions {
            for &label_id in &spec.labels {
                all_labels.insert(label_id);
            }
        }
        all_labels.extend(self.declared_labels.iter().copied());

        let mut uncontrollable_alphabet: HashSet<LabelId<L>> = HashSet::new();
        let mut controllable_alphabet: HashSet<LabelId<L>> = HashSet::new();
        let mut internal_alphabet: HashSet<LabelId<L>> = HashSet::new();

        // Build alphabet sets based on final controllability classifications
        for label_id in all_labels {
            match label_controllability.get(&label_id) {
                Some(LabelControllability::Uncontrollable) => {
                    uncontrollable_alphabet.insert(label_id);
                }
                Some(LabelControllability::Controllable) => {
                    controllable_alphabet.insert(label_id);
                }
                Some(LabelControllability::Internal) => {
                    internal_alphabet.insert(label_id);
                }
                None => {
                    // Default to controllable if not explicitly set
                    controllable_alphabet.insert(label_id);
                }
            }
        }

        // Step 3: Pre-compute uncontrollable groups for each state
        // Group transitions by their label sets for Skolem paradigm.
        // All transitions sharing the same label set are grouped together, regardless of controllability.
        // This ensures that for all uncontrollable choices (transitions in a group), we can check
        // if there exists at least one controllable choice (also in the same group) that satisfies.
        let mut uncontrollable_groups: UncontrollableGroups<L> =
            (0..state_count).map(|_| HashMap::new()).collect();

        for (state_idx, transitions) in outgoing.iter().enumerate() {
            for (trans_idx, transition) in transitions.iter().enumerate() {
                // Group by label set: all transitions with the same labels are in the same group
                // This is the key for Skolem paradigm: transitions sharing labels must be considered together
                let mut label_set: SmallVec<[LabelId<L>; 4]> = SmallVec::new();

                // Collect all labels (or empty for epsilon)
                for &label_id in transition.labels() {
                    label_set.push(label_id);
                }

                // Sort for canonical key (by index for consistent ordering)
                label_set.sort_by_key(|a| a.index());

                // Group all transitions that share the same label set
                // Note: We only group transitions that have at least one uncontrollable label OR are epsilon,
                // as these are the transitions that need Skolem paradigm handling.
                // Transitions with only controllable labels don't need grouping (they're always available).
                let should_group = transition.labels().is_empty() // Epsilon transitions
                    || transition.labels().iter().any(|&label_id| {
                        uncontrollable_alphabet.contains(&label_id)
                    });

                if should_group {
                    uncontrollable_groups[state_idx]
                        .entry(label_set)
                        .or_default()
                        .push(trans_idx);
                }
            }
        }

        Ok(Clts {
            state_names: self.state_names,
            state_map: self.state_map,
            initial_states: self.initial_states,
            outgoing,
            incoming,
            labels: label_store,
            variables: variable_store,
            state_variables: self.state_variables,
            state_valuations: self.state_valuations,
            // R.1 — 3-valued predicate labellings are populated only
            // by KMTS-aware adapters (R.2+); legacy builders leave
            // this `None`, and the `KleeneDomain` evaluator falls
            // back to the 2-valued state_variables in that case.
            state_3valued_predicates: self.state_3valued_predicates,
            state_set_pool,
            label_controllability,
            uncontrollable_alphabet,
            controllable_alphabet,
            internal_alphabet,
            uncontrollable_groups,
        })
    }
}

impl<S: IdStorage, L: IdStorage> Default for CltsBuilder<S, L> {
    fn default() -> Self {
        Self {
            state_names: Vec::with_capacity(DEFAULT_STATE_RESERVE),
            state_map: HashMap::with_capacity(DEFAULT_STATE_RESERVE),
            state_overflow: None,
            initial_states: HashSet::with_capacity(DEFAULT_STATE_RESERVE),
            transitions: Vec::with_capacity(DEFAULT_TRANSITION_RESERVE),
            labels: LabelStoreBuilder::default(),
            variables: VariableStoreBuilder::default(),
            state_variables: Vec::with_capacity(DEFAULT_STATE_RESERVE),
            state_valuations: Vec::with_capacity(DEFAULT_STATE_RESERVE),
            state_3valued_predicates: None,
            label_controllability: HashMap::new(),
            declared_labels: HashSet::new(),
        }
    }
}

/// Identifier referencing an interned variable set inside `VariableStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariableSetId(usize);

/// Mutable variable store used while building a CLTS instance.
///
/// `VariableStoreBuilder` provides string interning for variable sets, ensuring that
/// semantically equivalent variable sets (e.g., `["x", "y"]` and `["y", "x"]`) map to
/// the same canonical `VariableSetId`. This deduplication reduces memory usage and enables
/// efficient variable-based operations during CLTS construction.
///
/// # Usage
///
/// The builder is typically accessed through `CltsBuilder::with_variables()` or directly
/// when binding variables to states:
///
/// ```rust
/// use mununu_core::clts::Clts;
///
/// let mut builder = Clts::builder();
/// builder.state("s0").with_variables("s0", ["x", "y"]);
/// ```
///
/// # Methods
///
/// - [`intern()`](Self::intern): Interns an iterator of variable names, creating a new
///   variable set or returning an existing one. Use this when you have a temporary collection.
/// - [`intern_in_place()`](Self::intern_in_place): Interns a mutable `Vec<String>`,
///   taking ownership and clearing it. Use this when you have a reusable buffer to
///   avoid extra allocations.
///
/// # Example: Choosing between `intern()` and `intern_in_place()`
///
/// ```rust
/// use mununu_core::clts::VariableStoreBuilder;
///
/// let mut builder = VariableStoreBuilder::default();
///
/// // Use intern() for temporary collections
/// let vars1 = builder.intern(["x", "y"]);
///
/// // Use intern_in_place() when you have a reusable buffer
/// let mut buffer = vec!["a".to_string(), "b".to_string()];
/// let vars2 = builder.intern_in_place(&mut buffer);
/// // buffer is now empty and can be reused
/// # let _ = (vars1, vars2);
/// ```
#[derive(Debug, Clone)]
pub struct VariableStoreBuilder {
    sets: Vec<Arc<[String]>>,
    index: HashMap<Arc<[String]>, VariableSetId>,
    // EXP-0004: set_capacity_hint removed; Vec/HashMap handle growth.
    // Mirrors the label-store cache: collect the variable universe once and
    // lazily sort it so `build()` does not rebuild the data from scratch every
    // time.
    symbol_set: HashSet<String>,
    // Sorted-on-demand cache of the alphabet universe. `symbols_dirty`
    // indicates whether the vector needs a fresh sort.
    symbols: Vec<String>,
    symbols_dirty: bool,
    buffer_pool: StringVecPool,
}

impl Default for VariableStoreBuilder {
    fn default() -> Self {
        Self {
            sets: Vec::with_capacity(DEFAULT_VARIABLE_RESERVE),
            index: HashMap::with_capacity(DEFAULT_VARIABLE_RESERVE),
            // Mirrors the label-store cache: collect the variable universe once and
            // lazily sort it so `build()` does not rebuild the data from scratch every
            // time.
            symbol_set: HashSet::with_capacity(DEFAULT_VARIABLE_RESERVE),
            // Sorted-on-demand cache of the alphabet universe. `symbols_dirty`
            // indicates whether the vector needs a fresh sort.
            symbols: Vec::with_capacity(DEFAULT_VARIABLE_RESERVE),
            symbols_dirty: false,
            buffer_pool: StringVecPool::default(),
        }
    }
}

impl VariableStoreBuilder {
    fn empty(&mut self) -> VariableSetId {
        self.intern(std::iter::empty::<&str>())
    }

    // EXP-0004: ensure_set_capacity removed. Vec::push and HashMap::insert
    // handle growth by doubling.

    fn record_symbols(&mut self, payload: &[String]) {
        record_symbols_into_universe(
            &mut self.symbol_set,
            &mut self.symbols,
            &mut self.symbols_dirty,
            payload,
        );
    }

    /// Interns the provided variable names and returns a canonical variable set identifier.
    ///
    /// The variables are sorted and deduplicated so that semantically equivalent
    /// sets map to a single entry.
    ///
    /// # When to use
    ///
    /// Use this method when you have a temporary collection (iterator, array, etc.)
    /// that you don't need to reuse. For reusable buffers, prefer [`intern_in_place()`](Self::intern_in_place)
    /// to avoid extra allocations.
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::VariableStoreBuilder;
    ///
    /// let mut builder = VariableStoreBuilder::default();
    /// // Temporary collection - use intern()
    /// let vars = builder.intern(["x", "y", "z"]);
    /// # let _ = vars;
    /// ```
    pub fn intern<Var>(&mut self, vars: impl IntoIterator<Item = Var>) -> VariableSetId
    where
        Var: AsRef<str>,
    {
        let mut buffer = self.buffer_pool.acquire(DEFAULT_VARIABLE_RESERVE);
        buffer.extend(vars.into_iter().map(|v| v.as_ref().to_owned()));
        buffer.sort();
        buffer.dedup();

        if let Some(&id) = self.index.get(buffer.as_slice()) {
            self.buffer_pool.release(buffer);
            return id;
        }
        let id = VariableSetId(self.sets.len());
        let owned = std::mem::take(&mut buffer);
        let arc: Arc<[String]> = Arc::from(owned.into_boxed_slice());
        self.record_symbols(arc.as_ref());
        self.index.insert(Arc::clone(&arc), id);
        self.sets.push(arc);
        self.buffer_pool.release(buffer);
        id
    }

    /// Interns the provided buffer in place, clearing it for subsequent reuse.
    ///
    /// This method takes ownership of the vector's contents and clears it, allowing
    /// the buffer to be reused for subsequent intern operations. This avoids the
    /// allocation overhead of creating temporary collections.
    ///
    /// # When to use
    ///
    /// Use this method when you have a reusable `Vec<String>` buffer that you want
    /// to clear and reuse multiple times. For temporary collections, use [`intern()`](Self::intern)
    /// instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::VariableStoreBuilder;
    ///
    /// let mut builder = VariableStoreBuilder::default();
    /// let mut buffer = Vec::new();
    ///
    /// // Reuse the same buffer for multiple variable sets
    /// buffer.push("x".to_string());
    /// buffer.push("y".to_string());
    /// let vars1 = builder.intern_in_place(&mut buffer);
    ///
    /// buffer.push("z".to_string());
    /// let vars2 = builder.intern_in_place(&mut buffer);
    /// // buffer is now empty and ready for reuse
    /// # let _ = (vars1, vars2);
    /// ```
    pub fn intern_in_place(&mut self, vars: &mut Vec<String>) -> VariableSetId {
        vars.sort();
        vars.dedup();

        if let Some(&id) = self.index.get(vars.as_slice()) {
            vars.clear();
            return id;
        }
        let owned = std::mem::take(vars);
        let arc: Arc<[String]> = Arc::from(owned.into_boxed_slice());
        let id = VariableSetId(self.sets.len());
        self.record_symbols(arc.as_ref());
        self.index.insert(Arc::clone(&arc), id);
        self.sets.push(arc);
        id
    }

    fn build(self) -> VariableStore {
        let VariableStoreBuilder {
            sets,
            index: _,
            symbol_set: _,
            symbols,
            symbols_dirty,
            buffer_pool: _,
        } = self;

        let SymbolUniverse {
            sets,
            bitsets,
            symbols,
            symbol_index,
        } = build_symbol_index_and_bitsets(sets, symbols, symbols_dirty);

        VariableStore {
            sets,
            bitsets,
            symbols,
            symbol_index,
        }
    }
}

/// Canonicalised collection of variable sets and accompanying bitset view.
#[derive(Debug, Clone, Default)]
pub struct VariableStore {
    sets: Vec<Arc<[String]>>,
    bitsets: Vec<BitVec<usize, Lsb0>>,
    symbols: Vec<String>,
    symbol_index: HashMap<String, usize>,
}

impl VariableStore {
    fn all(&self) -> Vec<String> {
        self.symbols.clone()
    }

    fn get(&self, id: VariableSetId) -> Vec<String> {
        self.sets
            .get(id.0)
            .map(|arc| arc.as_ref().to_vec())
            .unwrap_or_default()
    }

    fn bitset(&self, id: VariableSetId) -> VariableBitSet<'_> {
        VariableBitSet {
            bits: &self.bitsets[id.0],
            index: &self.symbol_index,
        }
    }
}

/// Wrapper over a label's bitset payload providing lookup helpers.
///
/// Each label can contain multiple symbols (e.g., `["action", "signal"]`). This type
/// provides efficient membership testing using a bitset representation where each bit
/// corresponds to a symbol in the label alphabet.
///
/// # Bit Representation
///
/// The bitset uses one bit per symbol in the label alphabet:
/// - Bit at index `i` is set if symbol at position `i` in the sorted alphabet is in the label
/// - The `symbol_index` HashMap maps symbol names to their bit positions
///
/// This enables O(1) membership testing after an initial O(log n) HashMap lookup.
pub struct LabelBitSet<'a> {
    bits: &'a BitVec<usize, Lsb0>,
    index: &'a HashMap<String, usize>,
}

impl<'a> LabelBitSet<'a> {
    /// Returns `true` when the symbol is present in the label's alphabet set.
    ///
    /// This performs a lookup in the symbol index (O(log n) or O(1) amortized) and
    /// then checks the corresponding bit (O(1)). Overall, this is an efficient O(1)
    /// amortized operation.
    ///
    /// # Bit Operation
    /// 1. Look up symbol index: `idx = symbol_index[symbol]`
    /// 2. Check bit: `bits[idx] == true`
    pub fn test(&self, symbol: &str) -> bool {
        self.index
            .get(symbol)
            .and_then(|&idx| self.bits.get(idx))
            .is_some_and(|bit| *bit)
    }

    /// Returns the underlying bit representation for bitwise operations.
    ///
    /// This provides direct access to the bitset, enabling efficient bitwise operations
    /// (e.g., checking if two labels share symbols: `label1.bits() & label2.bits()`).
    pub fn bits(&self) -> &BitSlice<usize, Lsb0> {
        self.bits.as_bitslice()
    }
}

/// Wrapper over a state's variable bitset with convenience predicates.
///
/// Each state can have multiple variables (e.g., `["x", "y"]`). This type provides
/// efficient membership testing and subset/superset checks using a bitset representation
/// where each bit corresponds to a variable in the variable alphabet.
///
/// # Bit Representation
///
/// The bitset uses one bit per variable in the variable alphabet:
/// - Bit at index `i` is set if variable at position `i` in the sorted alphabet is in the state
/// - The `symbol_index` HashMap maps variable names to their bit positions
///
/// This enables O(1) membership testing and efficient subset/superset operations.
pub struct VariableBitSet<'a> {
    bits: &'a BitVec<usize, Lsb0>,
    index: &'a HashMap<String, usize>,
}

impl<'a> VariableBitSet<'a> {
    /// Returns `true` when the symbol belongs to the state's valuation set.
    pub fn contains(&self, symbol: &str) -> bool {
        self.index
            .get(symbol)
            .and_then(|&idx| self.bits.get(idx))
            .is_some_and(|bit| *bit)
    }

    /// Returns `true` when all bits set in `other` are also set in `self`.
    ///
    /// This checks if `self` is a superset of `other` (i.e., `other ⊆ self`).
    /// The operation is efficient: it iterates only over set bits in `other` and
    /// checks if they're also set in `self`.
    ///
    /// # Bit Operation
    /// Equivalent to: `(self.bits() & other.bits()) == other.bits()`
    /// or: `!other.bits().any(|(idx, bit)| bit && !self.bits()[idx])`
    ///
    /// # Performance
    /// O(k) where k is the number of set bits in `other`, which is typically much
    /// smaller than the total number of variables.
    pub fn is_superset(&self, other: &VariableBitSet<'_>) -> bool {
        other
            .bits
            .iter_ones()
            .all(|idx| self.bits.get(idx).is_some_and(|bit| *bit))
    }

    /// Returns the underlying bit representation.
    pub fn bits(&self) -> &BitSlice<usize, Lsb0> {
        self.bits.as_bitslice()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct TransitionFingerprint {
    target: usize,
    labels: Vec<BitVec<usize, Lsb0>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    // ---- R.1 — TransitionModality merge truth table ----
    //
    // The 3-case square the architecture doc §6.5 enumerates as the
    // corollary of the per-axis-conjunction rule
    // `has_may(L⊗R)=has_may(L)∧has_may(R); has_must(L⊗R)=has_must(L)∧has_must(R)`.
    // Standard-KMTS's `must ⊆ may` invariant eliminates the
    // hypothetical `MustOnly` rows; that is why the table is 3 cases,
    // not 6.

    #[test]
    fn modality_merge_sharp_and_sharp_is_sharp() {
        let sharp: TransitionModality<DefaultStateIdx> = TransitionModality::Sharp;
        assert_eq!(
            sharp.merge(&sharp),
            TransitionModality::Sharp,
            "both sides have may AND must; composed has both"
        );
    }

    #[test]
    fn modality_merge_sharp_and_mayonly_is_mayonly() {
        // Sharp ⊗ MayOnly: both have may, only left has must,
        // therefore the composed has may but not must.
        let sharp: TransitionModality<DefaultStateIdx> = TransitionModality::Sharp;
        let mayonly: TransitionModality<DefaultStateIdx> = TransitionModality::MayOnly;
        assert_eq!(sharp.merge(&mayonly), TransitionModality::MayOnly);
        // Symmetric: MayOnly ⊗ Sharp.
        assert_eq!(mayonly.merge(&sharp), TransitionModality::MayOnly);
    }

    #[test]
    fn modality_merge_mayonly_and_mayonly_is_mayonly() {
        // Both have may; neither has must.
        let mayonly: TransitionModality<DefaultStateIdx> = TransitionModality::MayOnly;
        assert_eq!(mayonly.merge(&mayonly), TransitionModality::MayOnly);
    }

    #[test]
    fn modality_merge_is_commutative() {
        let candidates: Vec<TransitionModality<DefaultStateIdx>> =
            vec![TransitionModality::Sharp, TransitionModality::MayOnly];
        for a in &candidates {
            for b in &candidates {
                assert_eq!(
                    a.merge(b),
                    b.merge(a),
                    "merge must be commutative for KMTS composition; counter-example: {a:?} ⊗ {b:?}"
                );
            }
        }
    }

    #[test]
    fn modality_merge_is_idempotent_per_value() {
        let candidates: Vec<TransitionModality<DefaultStateIdx>> =
            vec![TransitionModality::Sharp, TransitionModality::MayOnly];
        for m in &candidates {
            assert_eq!(
                m.merge(m),
                m.clone(),
                "self-merge must be the identity; counter-example: {m:?}"
            );
        }
    }

    #[test]
    fn transition_default_modality_is_sharp() -> TestResult {
        // Strict additivity invariant: every transition built via
        // `transition_ids` (the legacy entry point) emerges as Sharp.
        // KMTS-aware adapters opt into MayOnly via
        // `transition_ids_with_modality`.
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").initial("s0");
        let lbl = builder.labels().intern(["a"])?;
        let s0 = builder.state_id_or_insert("s0").expect("s0 inserted");
        let s1 = builder.state_id_or_insert("s1").expect("s1 inserted");
        builder.transition_ids(s0, &[lbl], s1);
        let clts = builder.build()?;
        let trans = clts.outgoing(s0);
        assert_eq!(trans.len(), 1);
        assert_eq!(
            trans[0].modality(),
            &TransitionModality::<DefaultStateIdx>::Sharp,
            "legacy transition_ids must default to Sharp"
        );
        Ok(())
    }

    #[test]
    fn transition_ids_with_modality_preserves_chosen_modality() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").initial("s0");
        let lbl = builder.labels().intern(["a"])?;
        let s0 = builder.state_id_or_insert("s0").expect("s0 inserted");
        let s1 = builder.state_id_or_insert("s1").expect("s1 inserted");
        builder.transition_ids_with_modality(s0, &[lbl], s1, TransitionModality::MayOnly);
        let clts = builder.build()?;
        let trans = clts.outgoing(s0);
        assert_eq!(trans.len(), 1);
        assert_eq!(
            trans[0].modality(),
            &TransitionModality::<DefaultStateIdx>::MayOnly
        );
        Ok(())
    }

    #[test]
    fn state_3valued_predicate_round_trips() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        let s0 = builder.state_id_or_insert("s0").expect("s0 inserted");
        builder
            .with_3valued_predicate(s0, "p", Tristate::KleeneT)
            .with_3valued_predicate(s0, "q", Tristate::KleeneBot);
        let clts = builder.build()?;
        assert_eq!(
            clts.state_3valued_predicate(s0, "p"),
            Some(Tristate::KleeneT)
        );
        assert_eq!(
            clts.state_3valued_predicate(s0, "q"),
            Some(Tristate::KleeneBot)
        );
        assert_eq!(clts.state_3valued_predicate(s0, "absent"), None);
        assert!(clts.has_3valued_predicates());
        Ok(())
    }

    #[test]
    fn state_3valued_predicate_absent_by_default() -> TestResult {
        // Strict additivity: legacy adapters that never call
        // `with_3valued_predicate` leave the field at `None`. The
        // KleeneDomain evaluator (R.3) treats that as the cue to fall
        // back to the 2-valued state_variables bitset.
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        let clts = builder.build()?;
        assert!(!clts.has_3valued_predicates());
        assert_eq!(
            clts.state_3valued_predicate(clts.state_id("s0")?, "p"),
            None
        );
        Ok(())
    }

    #[test]
    fn relabel_renames_labels_preserving_modality_valuation_and_predicates() -> TestResult {
        let mut builder = Clts::builder();
        let s0 = builder.state_with_name("s0".to_string()).expect("s0");
        let s1 = builder.state_with_name("s1".to_string()).expect("s1");
        builder.initial_state_id(s0);

        // A may-only edge labelled with the *port* name (the case R-MM
        // exists for) plus a sharp self-loop on an untouched label.
        let port = builder.labels().intern(["port_valid"]).expect("port label");
        let tick = builder.labels().intern(["tick"]).expect("tick label");
        builder.set_label_controllability(port, LabelControllability::Uncontrollable);
        builder.transition_ids_with_modality(s0, &[port], s1, TransitionModality::MayOnly);
        builder.transition_ids(s1, &[tick], s1); // Sharp self-loop

        // Abstract metadata the composed property reads — must survive.
        let mut valuation: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        valuation.insert("state".to_string(), "ACTIVE".to_string());
        builder.with_valuation_for_state(s1, valuation);
        builder.with_3valued_predicate(s1, "ready", Tristate::KleeneBot);

        let clts = builder.build()?;

        // Rename the port label to the connected-net name.
        let mut rename: HashMap<String, String> = HashMap::new();
        rename.insert("port_valid".to_string(), "net_3".to_string());
        let renamed = clts.relabel(&rename)?;

        // State space is unchanged; initial flag preserved.
        assert_eq!(renamed.state_count(), clts.state_count());
        assert!(!renamed.initial_states().is_empty());

        // The port label is renamed; the untouched label is kept.
        let alphabet = renamed.alphabet();
        assert!(
            alphabet.contains(&"net_3".to_string()),
            "renamed label present"
        );
        assert!(
            !alphabet.contains(&"port_valid".to_string()),
            "old label gone"
        );
        assert!(
            alphabet.contains(&"tick".to_string()),
            "untouched label kept"
        );

        // Modality preserved: the renamed edge is still MayOnly — relabel
        // must NOT fabricate a Sharp (must) witness.
        let s0_new = renamed.state_id("s0")?;
        let may_edge = renamed
            .outgoing(s0_new)
            .iter()
            .find(|t| {
                t.labels().iter().any(|&l| {
                    renamed
                        .label_payload(l)
                        .is_some_and(|p| p.iter().any(|s| s == "net_3"))
                })
            })
            .expect("renamed may-edge present");
        assert_eq!(*may_edge.modality(), TransitionModality::MayOnly);

        // Structured valuation + 3-valued predicate carried through.
        let s1_new = renamed.state_id("s1")?;
        assert_eq!(
            renamed
                .state_valuation(s1_new)
                .and_then(|v| v.get("state"))
                .map(|s| s.as_str()),
            Some("ACTIVE")
        );
        assert_eq!(
            renamed.state_3valued_predicate(s1_new, "ready"),
            Some(Tristate::KleeneBot)
        );
        Ok(())
    }

    #[test]
    fn relabel_identity_map_is_structure_preserving() -> TestResult {
        // An empty rename map leaves every label untouched — relabel is a
        // faithful structural copy (the driver applies it per instance even
        // when a module's ports already match net names).
        let mut builder = Clts::builder();
        let s0 = builder.state_with_name("s0".to_string()).expect("s0");
        let s1 = builder.state_with_name("s1".to_string()).expect("s1");
        builder.initial_state_id(s0);
        let a = builder.labels().intern(["a"]).expect("a");
        builder.transition_ids(s0, &[a], s1);

        let clts = builder.build()?;
        let copy = clts.relabel(&HashMap::new())?;

        assert_eq!(copy.state_count(), clts.state_count());
        assert_eq!(copy.alphabet(), clts.alphabet());
        assert_eq!(copy.outgoing(copy.state_id("s0")?).len(), 1);
        Ok(())
    }

    #[test]
    fn tristate_helpers() {
        assert_eq!(Tristate::from_bool(true), Tristate::KleeneT);
        assert_eq!(Tristate::from_bool(false), Tristate::KleeneF);
        assert!(Tristate::KleeneT.is_true());
        assert!(!Tristate::KleeneF.is_true());
        assert!(!Tristate::KleeneBot.is_true());
        assert!(Tristate::KleeneT.is_definite());
        assert!(Tristate::KleeneF.is_definite());
        assert!(!Tristate::KleeneBot.is_definite());
    }

    #[test]
    fn transition_modality_has_may_invariant() {
        // The standard-KMTS invariant `must ⊆ may` means every
        // representable transition has may.
        let sharp: TransitionModality<DefaultStateIdx> = TransitionModality::Sharp;
        let mayonly: TransitionModality<DefaultStateIdx> = TransitionModality::MayOnly;
        assert!(sharp.has_may());
        assert!(mayonly.has_may());
        // Sharp has must (singleton-target); MayOnly does not.
        assert!(sharp.has_must());
        assert!(!mayonly.has_must());
        // R.4.5 — MustHyperOnly also has must (hyper-target set).
        let hyper: TransitionModality<DefaultStateIdx> =
            TransitionModality::must_hyper(smallvec::smallvec![
                StateId::from_index(0).expect("idx 0 fits"),
                StateId::from_index(1).expect("idx 1 fits"),
            ]);
        assert!(hyper.has_may());
        assert!(hyper.has_must());
        assert_eq!(hyper.hyper_targets().map(|t| t.len()), Some(2));
    }

    #[test]
    fn builds_single_state_clts() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");

        let clts = builder.build()?;

        assert_eq!(clts.state_count(), 1, "expected exactly one state");
        assert!(clts.initial_states().contains(&clts.state_id("s0")?));

        Ok(())
    }

    #[test]
    fn adds_labeled_transition() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");

        let label = builder.labels().intern(["a"])?;
        builder.transition("s0", &[label], "s1");

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let outgoing = clts.outgoing(s0);

        assert_eq!(outgoing.len(), 1, "expected a single outgoing transition");
        assert!(outgoing[0].labels().contains(&label));

        Ok(())
    }

    #[test]
    fn tracks_incoming_transitions() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");

        let label_ab = builder.labels().intern(["a", "b"])?;
        let label_c = builder.labels().intern(["c"])?;
        builder.transition("s0", &[label_ab], "s1");
        builder.transition("s2", &[label_c], "s1");

        let clts = builder.build()?;
        let s1 = clts.state_id("s1")?;
        let incoming = clts.incoming(s1);

        assert_eq!(incoming.len(), 2, "expected two incoming transitions");
        let labels: Vec<_> = incoming.iter().flat_map(|t| t.labels()).copied().collect();
        assert!(labels.contains(&label_ab));
        assert!(labels.contains(&label_c));

        Ok(())
    }

    #[test]
    fn iterates_states_in_insertion_order() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("alpha").state("beta").state("gamma");
        let clts = builder.build()?;

        let state_names: Vec<_> = clts
            .states()
            .map(|state| clts.state_name(state).unwrap().to_owned())
            .collect();

        assert_eq!(
            state_names,
            vec!["alpha", "beta", "gamma"],
            "unexpected state iteration order"
        );

        Ok(())
    }

    #[test]
    fn reuses_canonical_label_ids() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");

        let ab_first = builder.labels().intern(["a", "b"])?;
        let ab_second = builder.labels().intern(["b", "a"])?;
        builder.transition("s0", &[ab_first], "s1");
        builder.transition("s1", &[ab_second], "s0");

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;

        assert_eq!(ab_first, ab_second, "label interning was not canonical");
        assert_eq!(clts.outgoing(s0)[0].labels(), &[ab_first]);
        assert_eq!(clts.outgoing(s1)[0].labels(), &[ab_second]);

        Ok(())
    }

    #[test]
    fn builder_returns_consistent_state_handles() -> TestResult {
        let mut builder = Clts::builder();
        let s0 = builder
            .state_with_name("s0".to_owned())
            .expect("state id available");
        let s0_again = builder.state_id_or_insert("s0").expect("state id exists");
        assert_eq!(s0.raw(), s0_again.raw());

        builder.initial_state_id(s0);
        builder.with_variables_for_state(s0, ["v0"]);
        let label = builder.labels().intern(["tick"])?;
        builder.transition_ids(s0, &[label], s0);

        let clts = builder.build()?;
        assert_eq!(clts.state_count(), 1);
        assert!(clts.initial_states().contains(&s0));
        Ok(())
    }

    #[test]
    fn label_store_intern_in_place_reuses_buffer() -> TestResult {
        let mut labels = LabelStoreBuilder::<DefaultLabelIdx>::default();
        let mut buffer = vec!["beta".to_owned(), "alpha".to_owned(), "alpha".to_owned()];

        let id_first = labels.intern_in_place(&mut buffer)?;
        assert!(buffer.is_empty(), "buffer should be empty for reuse");

        buffer.extend(["alpha".to_owned(), "beta".to_owned()]);
        let id_second = labels.intern_in_place(&mut buffer)?;
        assert!(buffer.is_empty(), "buffer was not cleared for reuse");
        assert_eq!(id_first.index(), id_second.index());

        Ok(())
    }

    #[test]
    fn variable_store_intern_in_place_reuses_buffer() {
        let mut builder = VariableStoreBuilder::default();
        let mut vars = vec!["y".to_owned(), "x".to_owned(), "x".to_owned()];

        let first = builder.intern_in_place(&mut vars);
        assert!(vars.is_empty());

        vars.extend(["x".to_owned(), "y".to_owned()]);
        let second = builder.intern_in_place(&mut vars);
        assert!(vars.is_empty());
        assert_eq!(first, second);
    }

    #[test]
    fn maintains_struct_of_arrays_alignment() -> TestResult {
        let mut builder = Clts::builder();
        for idx in 0..16 {
            builder.state(format!("s{idx}"));
        }
        builder.labels().intern(["x"])?;
        for idx in 0..15 {
            let from = format!("s{idx}");
            let to = format!("s{}", idx + 1);
            let label = builder.labels().intern(["x"])?;
            builder.transition(from, &[label], to);
        }

        let clts = builder.build()?;
        let states: Vec<_> = clts.states().collect();

        // Find first state (incoming == 0, outgoing == 1)
        let first_state = states
            .iter()
            .find(|&&state| clts.incoming(state).is_empty())
            .expect("should have a state with no incoming transitions");
        assert!(
            clts.incoming(*first_state).is_empty(),
            "first state should have no incoming transitions"
        );
        assert_eq!(
            clts.outgoing(*first_state).len(),
            1,
            "first state should have exactly one outgoing transition"
        );

        // Find last state (incoming == 1, outgoing == 0)
        let last_state = states
            .iter()
            .find(|&&state| clts.outgoing(state).is_empty())
            .expect("should have a state with no outgoing transitions");
        assert_eq!(
            clts.incoming(*last_state).len(),
            1,
            "last state should have exactly one incoming transition"
        );
        assert_eq!(
            clts.outgoing(*last_state).len(),
            0,
            "last state should have no outgoing transitions"
        );

        // All states in between: incoming == 1, outgoing == 1
        for state in &states {
            if *state != *first_state && *state != *last_state {
                assert_eq!(
                    clts.incoming(*state).len(),
                    1,
                    "middle state should have exactly one incoming transition"
                );
                assert_eq!(
                    clts.outgoing(*state).len(),
                    1,
                    "middle state should have exactly one outgoing transition"
                );
            }
        }

        Ok(())
    }

    #[test]
    fn controllable_successors_reported() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");

        let controllable_label = builder.labels().intern(["ctrl"])?;
        let uncontrollable_label = builder.labels().intern(["input"])?;
        builder
            .set_label_controllability(uncontrollable_label, LabelControllability::Uncontrollable);

        builder.transition("s0", &[controllable_label], "s1");
        builder.transition("s0", &[uncontrollable_label], "s2");

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;
        let s2 = clts.state_id("s2")?;

        // Test that we can filter outgoing transitions by controllability
        let controllable: Vec<_> = clts
            .outgoing(s0)
            .iter()
            .filter(|t| t.is_controllable(&clts))
            .map(|t| t.target())
            .collect();
        let uncontrollable: Vec<_> = clts
            .outgoing(s0)
            .iter()
            .filter(|t| t.is_uncontrollable(&clts))
            .map(|t| t.target())
            .collect();

        assert_eq!(controllable, vec![s1]);
        assert_eq!(uncontrollable, vec![s2]);

        // Test bitset construction on-demand
        let mut controllable_bits = bitvec![usize, Lsb0; 0; clts.state_names.len()];
        for transition in clts.outgoing(s0) {
            if transition.is_controllable(&clts) {
                controllable_bits.set(transition.target().index(), true);
            }
        }
        let mut uncontrollable_bits = bitvec![usize, Lsb0; 0; clts.state_names.len()];
        for transition in clts.outgoing(s0) {
            if transition.is_uncontrollable(&clts) {
                uncontrollable_bits.set(transition.target().index(), true);
            }
        }
        assert!(controllable_bits.get(s1.index()).unwrap());
        assert!(uncontrollable_bits.get(s2.index()).unwrap());
        Ok(())
    }

    #[test]
    fn assigns_state_variables() -> TestResult {
        let mut builder = Clts::builder();
        builder.with_variables("s0", ["v0", "v1"]);
        builder.with_variables("s1", ["v1"]);

        let clts = builder.build()?;
        let vars: Vec<_> = clts.variables();
        assert!(vars.contains(&"v0".to_string()));
        assert!(vars.contains(&"v1".to_string()));

        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;
        assert_eq!(
            clts.state_variables(s0),
            vec!["v0".to_string(), "v1".to_string()]
        );
        assert_eq!(clts.state_variables(s1), vec!["v1".to_string()]);

        Ok(())
    }

    #[test]
    fn reuses_variable_handles() -> TestResult {
        let mut builder = Clts::builder();
        builder.with_variables("s0", ["temp", "flag"]);
        builder.with_variables("s1", ["flag", "temp"]);

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;
        assert_eq!(clts.state_variables(s0), clts.state_variables(s1));

        Ok(())
    }

    #[test]
    fn exposes_label_bitsets() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0");
        let label = builder.labels().intern(["alpha", "beta"])?;
        builder.transition("s0", &[label], "s0");
        let clts = builder.build()?;

        let bitset = clts.label_bitset(label).expect("label bitset should exist");
        assert!(bitset.test("alpha"));
        assert!(bitset.test("beta"));
        assert!(!bitset.test("gamma"));
        assert!(!bitset.test("missing"));

        Ok(())
    }

    #[test]
    fn exposes_variable_bitsets() -> TestResult {
        let mut builder = Clts::builder();
        builder.with_variables("s0", ["v0", "v1"]);
        builder.with_variables("s1", ["v1"]);

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;

        let b0 = clts.state_variable_bitset(s0);
        let b1 = clts.state_variable_bitset(s1);

        assert!(b0.contains("v0"));
        assert!(b0.contains("v1"));
        assert!(!b0.contains("v2"));
        assert!(!b0.contains("missing"));
        assert!(b1.contains("v1"));
        assert!(!b1.contains("v0"));
        assert!(b0.is_superset(&b1));

        Ok(())
    }

    #[test]
    fn variable_bitset_superset_detects_gaps() -> TestResult {
        let mut builder = Clts::builder();
        builder.with_variables("s0", ["v0", "v1"]);
        builder.with_variables("s1", ["v1", "v2"]);

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let s1 = clts.state_id("s1")?;

        let b0 = clts.state_variable_bitset(s0);
        let b1 = clts.state_variable_bitset(s1);

        assert!(!b0.is_superset(&b1));
        assert!(!b1.is_superset(&b0));

        Ok(())
    }

    #[test]
    fn state_set_pool_reuses_zeroed_buffers() -> TestResult {
        let mut builder = Clts::builder();
        builder
            .state("s0")
            .state("s1")
            .transition("s0", &[], "s1")
            .initial("s0");
        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;

        {
            let mut set = clts.state_set();
            assert_eq!(set.len(), clts.state_count());
            assert!(set.insert(s0));
            assert!(set.contains(s0));
        }

        let set = clts.state_set();
        assert_eq!(set.len(), clts.state_count());
        assert!(!set.contains(s0));

        Ok(())
    }

    #[test]
    fn label_id_raw_accessor() -> TestResult {
        // Test LabelId::raw() accessor method (line 131)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");

        let label = builder.labels().intern(["test_label"])?;
        let raw_id = label.raw();

        // Verify raw() returns the underlying storage type
        assert_eq!(raw_id, label.raw());
        assert_eq!(label.index(), raw_id.to_usize());

        Ok(())
    }

    #[test]
    fn grow_capacity_zero_current() {
        // Test grow_capacity with zero current capacity (line 60)
        // This is tested indirectly through builder growth, but let's verify behavior
        let mut builder = Clts::builder();
        // Start with empty builder (current = 0)
        builder.state("s0");
        let clts = builder.build().expect("should build");
        assert_eq!(clts.state_count(), 1);
    }

    #[test]
    fn grow_capacity_small_values() {
        // Test grow_capacity with small values (should grow by at least 1)
        let mut builder = Clts::builder();
        // Add states to trigger capacity growth
        for i in 0..10 {
            builder.state(format!("s{}", i));
        }
        let clts = builder.build().expect("should build");
        assert_eq!(clts.state_count(), 10);
    }

    #[test]
    fn grow_capacity_large_values() {
        // Test grow_capacity with large values (should grow by ~20%)
        let mut builder = Clts::builder();
        // Add many states to trigger multiple capacity growths
        for i in 0..100 {
            builder.state(format!("s{}", i));
        }
        let clts = builder.build().expect("should build");
        assert_eq!(clts.state_count(), 100);
    }

    #[test]
    fn state_name_retrieval() -> TestResult {
        // Test state_name() method (line 626)
        let mut builder = Clts::builder();
        builder.state("alpha").state("beta").state("gamma");

        let clts = builder.build()?;
        let alpha_id = clts.state_id("alpha")?;
        let beta_id = clts.state_id("beta")?;
        let gamma_id = clts.state_id("gamma")?;

        assert_eq!(clts.state_name(alpha_id), Some("alpha"));
        assert_eq!(clts.state_name(beta_id), Some("beta"));
        assert_eq!(clts.state_name(gamma_id), Some("gamma"));

        // Test with invalid state ID
        let invalid_id = StateId::new(999);
        if let Some(id) = invalid_id {
            assert_eq!(clts.state_name(id), None);
        }

        Ok(())
    }

    #[test]
    fn label_payload_access() -> TestResult {
        // Test label_payload() method (line 634)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");

        let label_single = builder.labels().intern(["tick"])?;
        let label_multi = builder.labels().intern(["sync", "ack"])?;

        let clts = builder.build()?;

        let payload_single = clts.label_payload(label_single);
        assert!(payload_single.is_some());
        assert_eq!(payload_single.unwrap(), &["tick"]);

        let payload_multi = clts.label_payload(label_multi);
        assert!(payload_multi.is_some());
        // Labels may be canonicalized/sorted, so check contents
        let payload = payload_multi.unwrap();
        assert_eq!(payload.len(), 2);
        assert!(payload.contains(&"sync".to_string()));
        assert!(payload.contains(&"ack".to_string()));

        Ok(())
    }

    #[test]
    fn structural_hash_consistency() -> TestResult {
        // Test structural_hash() method (line 745)
        let mut builder1 = Clts::builder();
        builder1.state("s0").state("s1").initial("s0");
        let label = builder1.labels().intern(["tick"])?;
        builder1.transition("s0", &[label], "s1");
        let clts1 = builder1.build()?;

        let mut builder2 = Clts::builder();
        builder2.state("s0").state("s1").initial("s0");
        let label2 = builder2.labels().intern(["tick"])?;
        builder2.transition("s0", &[label2], "s1");
        let clts2 = builder2.build()?;

        // Structurally equivalent CLTSs should have the same hash
        assert_eq!(clts1.structural_hash(), clts2.structural_hash());

        // Different CLTSs should have different hashes
        let mut builder3 = Clts::builder();
        builder3.state("s0").state("s1").state("s2").initial("s0");
        let clts3 = builder3.build()?;
        assert_ne!(clts1.structural_hash(), clts3.structural_hash());

        Ok(())
    }

    #[test]
    fn structural_eq_comparison() -> TestResult {
        // Test structural_eq() method (line 752)
        let mut builder1 = Clts::builder();
        builder1.state("s0").state("s1").initial("s0");
        let label = builder1.labels().intern(["tick"])?;
        builder1.transition("s0", &[label], "s1");
        let clts1 = builder1.build()?;

        let mut builder2 = Clts::builder();
        builder2.state("s0").state("s1").initial("s0");
        let label2 = builder2.labels().intern(["tick"])?;
        builder2.transition("s0", &[label2], "s1");
        let clts2 = builder2.build()?;

        // Structurally equivalent CLTSs should be equal
        assert!(clts1.structural_eq(&clts2));
        assert!(clts2.structural_eq(&clts1));

        // Different CLTSs should not be equal
        let mut builder3 = Clts::builder();
        builder3.state("s0").state("s1").state("s2").initial("s0");
        let clts3 = builder3.build()?;
        assert!(!clts1.structural_eq(&clts3));
        assert!(!clts3.structural_eq(&clts1));

        // CLTS with different initial states should not be equal
        let mut builder4 = Clts::builder();
        builder4.state("s0").state("s1").initial("s1");
        let label4 = builder4.labels().intern(["tick"])?;
        builder4.transition("s0", &[label4], "s1");
        let clts4 = builder4.build()?;
        assert!(!clts1.structural_eq(&clts4));

        Ok(())
    }

    #[test]
    fn builder_with_controllable_and_uncontrollable_transitions() -> TestResult {
        // Test builder with both controllable and uncontrollable transitions
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");

        let label_ctrl = builder.labels().intern(["ctrl"])?;
        let label_input = builder.labels().intern(["input"])?;

        let s0_id = builder.state_id_or_insert("s0").unwrap();
        let s1_id = builder.state_id_or_insert("s1").unwrap();
        let s2_id = builder.state_id_or_insert("s2").unwrap();

        builder.set_label_controllability(label_input, LabelControllability::Uncontrollable);
        builder.transition_ids(s0_id, &[label_ctrl], s1_id);
        builder.transition_ids(s0_id, &[label_input], s2_id);

        let clts = builder.build()?;
        let s0 = clts.state_id("s0")?;
        let outgoing = clts.outgoing(s0);

        assert_eq!(outgoing.len(), 2);
        // Verify both transition kinds are present
        let has_controllable = outgoing.iter().any(|t| t.is_controllable(&clts));
        let has_uncontrollable = outgoing.iter().any(|t| t.is_uncontrollable(&clts));
        assert!(has_controllable);
        assert!(has_uncontrollable);

        Ok(())
    }

    #[test]
    fn state_id_error_handling() {
        // Test state_id() error handling (line 600)
        let mut builder = Clts::builder();
        builder.state("s0");
        let clts = builder.build().expect("should build");

        // Valid state should succeed
        assert!(clts.state_id("s0").is_ok());

        // Invalid state should return error
        let result = clts.state_id("nonexistent");
        assert!(result.is_err());
        match result {
            Err(CltsError::UnknownState(name)) => {
                assert_eq!(name, "nonexistent");
            }
            _ => panic!("expected UnknownState error"),
        }
    }

    #[test]
    fn explicit_label_controllability() -> TestResult {
        // Test explicit controllability setting
        let mut builder = Clts::builder();
        let input_label = builder.labels().intern(["input"])?;
        let output_label = builder.labels().intern(["output"])?;
        let internal_label = builder.labels().intern(["internal"])?;

        builder.set_label_controllability(input_label, LabelControllability::Uncontrollable);
        builder.set_label_controllability(output_label, LabelControllability::Controllable);
        builder.set_label_controllability(internal_label, LabelControllability::Internal);

        builder.state("s0").initial("s0");
        builder.state("s1");
        builder.transition("s0", &[input_label], "s1");
        builder.transition("s0", &[output_label], "s1");
        builder.transition("s0", &[internal_label], "s1");

        let clts = builder.build()?;

        // Verify controllability classifications
        assert_eq!(
            clts.label_controllability(input_label),
            Some(LabelControllability::Uncontrollable)
        );
        assert_eq!(
            clts.label_controllability(output_label),
            Some(LabelControllability::Controllable)
        );
        assert_eq!(
            clts.label_controllability(internal_label),
            Some(LabelControllability::Internal)
        );

        // Verify alphabet sets
        assert!(clts.is_uncontrollable_label(input_label));
        assert!(clts.is_controllable_label(output_label));
        assert!(clts.is_internal_label(internal_label));

        assert!(clts.uncontrollable_alphabet().contains(&input_label));
        assert!(clts.controllable_alphabet().contains(&output_label));
        assert!(clts.internal_alphabet().contains(&internal_label));

        Ok(())
    }

    #[test]
    fn default_label_controllability() -> TestResult {
        // Test that labels default to controllable if not explicitly set
        // This verifies the default behavior during build, not inference from transitions
        let mut builder = Clts::builder();
        let controllable_label = builder.labels().intern(["ctrl"])?;
        let uncontrollable_label = builder.labels().intern(["input"])?;

        builder.state("s0").initial("s0");
        builder.state("s1");
        let s0 = builder.state_id_or_insert("s0").unwrap();
        let s1 = builder.state_id_or_insert("s1").unwrap();

        // Only set uncontrollable_label explicitly
        builder
            .set_label_controllability(uncontrollable_label, LabelControllability::Uncontrollable);
        builder.transition_ids(s0, &[controllable_label], s1);
        builder.transition_ids(s0, &[uncontrollable_label], s1);

        let clts = builder.build()?;

        // Verify explicit controllability is preserved
        assert_eq!(
            clts.label_controllability(uncontrollable_label),
            Some(LabelControllability::Uncontrollable)
        );

        // Unset labels return None from label_controllability(), but default to controllable during build
        // Verify this by checking the alphabet sets and transition controllability
        assert_eq!(
            clts.label_controllability(controllable_label),
            None, // Not explicitly set, so returns None
        );

        // But transitions using it are controllable (default behavior)
        let transition = clts
            .outgoing(s0)
            .iter()
            .find(|t| t.labels().contains(&controllable_label))
            .unwrap();
        assert!(transition.is_controllable(&clts));

        // And it's in the controllable alphabet
        assert!(clts.controllable_alphabet().contains(&controllable_label));

        Ok(())
    }

    #[test]
    fn precomputed_uncontrollable_groups() -> TestResult {
        // Test that uncontrollable groups are pre-computed correctly
        let mut builder = Clts::builder();
        let input_a = builder.labels().intern(["input_a"])?;
        let input_b = builder.labels().intern(["input_b"])?;
        let action_x = builder.labels().intern(["action_x"])?;

        builder.set_label_controllability(input_a, LabelControllability::Uncontrollable);
        builder.set_label_controllability(input_b, LabelControllability::Uncontrollable);
        builder.set_label_controllability(action_x, LabelControllability::Controllable); // Explicitly mark as controllable

        builder.state("s0").initial("s0");
        builder.state("s1");
        builder.state("s2");
        builder.state("s3");

        let s0 = builder.state_id_or_insert("s0").unwrap();
        let s1 = builder.state_id_or_insert("s1").unwrap();
        let s2 = builder.state_id_or_insert("s2").unwrap();
        let s3 = builder.state_id_or_insert("s3").unwrap();

        // Group 1: transitions sharing input_a (uncontrollable)
        builder.set_label_controllability(input_a, LabelControllability::Uncontrollable);
        builder.transition_ids(s0, &[input_a, action_x], s1);
        builder.transition_ids(s0, &[input_a], s2); // Shares input_a but also has action_x

        // Group 2: transitions sharing input_b (uncontrollable)
        builder.set_label_controllability(input_b, LabelControllability::Uncontrollable);
        builder.transition_ids(s0, &[input_b], s3);

        let clts = builder.build()?;

        let s0 = clts.state_id("s0")?;
        let groups = clts.transitions_grouped_by_uncontrollable_labels(s0);

        // With new grouping logic: transitions are grouped by their full label set
        // So we get 3 groups:
        // 1. [input_a, action_x] - transition with both labels
        // 2. [input_a] - transition with just input_a
        // 3. [input_b] - transition with input_b
        assert_eq!(
            groups.len(),
            3,
            "Expected 3 groups (by full label sets), found: {:?}",
            groups.keys().collect::<Vec<_>>()
        );

        // Verify groups contain correct transitions
        let mut found_input_a_action_x_group = false;
        let mut found_input_a_only_group = false;
        let mut found_input_b_group = false;

        for (label_set, transition_indices) in groups {
            if label_set.len() == 2 && label_set.contains(&input_a) && label_set.contains(&action_x)
            {
                found_input_a_action_x_group = true;
                assert_eq!(
                    transition_indices.len(),
                    1,
                    "Group [input_a, action_x] should have 1 transition"
                );
            } else if label_set.len() == 1 && label_set[0] == input_a {
                found_input_a_only_group = true;
                assert_eq!(
                    transition_indices.len(),
                    1,
                    "Group [input_a] should have 1 transition"
                );
            } else if label_set.len() == 1 && label_set[0] == input_b {
                found_input_b_group = true;
                assert_eq!(
                    transition_indices.len(),
                    1,
                    "Group [input_b] should have 1 transition"
                );
            }
        }

        assert!(
            found_input_a_action_x_group,
            "Should have group [input_a, action_x]"
        );
        assert!(found_input_a_only_group, "Should have group [input_a]");
        assert!(found_input_b_group, "Should have group [input_b]");

        Ok(())
    }

    #[test]
    fn states_iterator() -> TestResult {
        // Test states() iterator (line 618)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");

        let clts = builder.build()?;
        let states: Vec<_> = clts.states().collect();

        assert_eq!(states.len(), 3);
        // Verify all states are present
        let state_names: Vec<_> = states.iter().filter_map(|s| clts.state_name(*s)).collect();
        assert!(state_names.contains(&"s0"));
        assert!(state_names.contains(&"s1"));
        assert!(state_names.contains(&"s2"));

        Ok(())
    }
}
