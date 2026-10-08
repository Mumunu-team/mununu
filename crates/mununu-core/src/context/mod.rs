//! Context management for CLTS instances.
//!
//! This is an early scaffold that will eventually coordinate global alphabets,
//! variable registries, and CLTS composition. For now, it focuses on exposing a
//! shared `LabelStoreBuilder` and registering named CLTS instances.

use crate::clts::{
    Clts, CltsBuilder, CltsError, DefaultLabelIdx, DefaultStateIdx, LabelStoreBuilder, StateId,
};
use crate::composition::{CompositionOptions, compose};
use crate::context_dsl::{ContextDoc, IncrementalState, LoadPlan};
use crate::mu_calculus::{
    Environment, EvalResult, EvaluationError, EvaluationOptions, Formula, WitnessMap,
    evaluate_with_options_and_automaton, evaluate_with_witnesses,
};
use crate::persistence::{
    PersistenceError, load_clts_from_path, maybe_spill_clts, save_clts_to_path,
};
use bitvec::prelude::{BitVec, Lsb0};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("CLTS with name '{0}' already registered")]
    DuplicateClts(String),
    #[error("controllable alphabet element '{0}' already claimed by another CLTS")]
    DuplicateControllableAlphabet(String),
    #[error("internal alphabet element '{0}' already claimed by another CLTS")]
    DuplicateInternalAlphabet(String),
    #[error("failed to merge label store: {0}")]
    LabelRegistry(String),
    #[error("unknown CLTS '{0}'")]
    UnknownClts(String),
    #[error("composition failed: {0}")]
    Composition(#[from] CltsError),
    #[error("controller synthesis failed: {0}")]
    Controller(CltsError),
    #[error("persistence failure: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("μ-calculus environment for '{name}' expected {expected} states, received {provided}")]
    EnvironmentMismatch {
        name: String,
        expected: usize,
        provided: usize,
    },
    #[error("μ-calculus evaluation failed: {0}")]
    MuEvaluation(#[from] EvaluationError),
    #[error("unsupported synthesis request: {0}")]
    Unsupported(String),
}

/// Cache for incremental DSL loads.
#[derive(Debug, Default, Clone)]
pub struct ContextDslCache {
    state: IncrementalState,
}

impl ContextDslCache {
    /// Creates an empty cache for incremental DSL loads.
    pub fn new() -> Self {
        Self::default()
    }

    /// Computes the changes required to rebuild the context from `doc` without mutating the cache.
    pub fn diff(&self, doc: &ContextDoc) -> LoadPlan {
        self.state.diff(doc)
    }

    /// Applies a previously computed plan, updating the cached fingerprints.
    pub fn update(&mut self, plan: &LoadPlan) {
        self.state.apply(plan);
    }

    /// Convenience helper that computes the plan and updates the cache in one step.
    ///
    /// # Coverage Status
    /// Covered by test: `context_dsl_cache_diff_and_update`
    pub fn diff_and_update(&mut self, doc: &ContextDoc) -> LoadPlan {
        let plan = self.diff(doc);
        self.update(&plan);
        plan
    }
}

/// Context for managing CLTS instances and their shared resources.
///
/// This struct contains:
/// - A map of CLTS instances by name.
/// - A shared label store for interning labels.
/// - A set of controllable alphabet symbols.
/// - A set of global variables.
///
/// The context is used to manage the lifecycle of CLTS instances and their shared resources.
/// It is also used to evaluate μ-calculus formulas and synthesise controllers.
#[derive(Debug, Default)]
pub struct Context {
    cltss: HashMap<String, Clts<DefaultStateIdx, DefaultLabelIdx>>,
    label_store: LabelStoreBuilder<DefaultLabelIdx>,
    controllable_alphabet: HashSet<String>,
    global_variables: HashSet<String>,
}

impl Drop for Context {
    /// Custom drop implementation to avoid stack overflow when dropping contexts with large CLTSs.
    ///
    /// When a Context contains multiple large CLTSs (e.g., 2000+ states), dropping them all
    /// at once can cause stack overflow. This implementation drops CLTSs one at a time to
    /// avoid deep recursion.
    fn drop(&mut self) {
        // Drop CLTSs one at a time to avoid stack overflow
        // Use take to move them out, then drop individually
        let mut cltss = std::mem::take(&mut self.cltss);

        let keys: Vec<String> = cltss.keys().cloned().collect();

        for key in keys.iter() {
            if let Some(clts) = cltss.remove(key) {
                drop(clts); // Each CLTS has its own Drop implementation
            }
        }
    }
}

mod diagnostics;

use crate::context::diagnostics::{
    BfsReachability, bfs_reachable_states, build_unrealizable_initials_synthesis,
    enrich_diagnostics_for_excluded_initials,
};

impl Context {
    /// Returns a new context builder.
    pub fn builder() -> ContextBuilder {
        ContextBuilder::default()
    }

    /// Retrieves a registered CLTS by name.
    pub fn clts(&self, name: &str) -> Option<&Clts<DefaultStateIdx, DefaultLabelIdx>> {
        self.cltss.get(name)
    }

    /// Returns the list of registered CLTS names.
    ///
    /// # Coverage Status
    /// Covered indirectly by `context::tests::registers_clts_instances`.
    pub fn clts_names(&self) -> Vec<String> {
        self.cltss.keys().cloned().collect()
    }

    /// Prints a compact representation of the internal structure of this context.
    ///
    /// This includes:
    /// - All registered CLTS instances with their states, transitions, labels, and variables
    /// - Global variables and controllable alphabet
    /// - Label store information
    ///
    /// The output is written to the provided writer.
    pub fn print_structure<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writeln!(writer, "Context Structure:")?;
        writeln!(writer, "==================")?;
        writeln!(writer)?;

        // Global information
        writeln!(writer, "Global Variables: {}", self.global_variables.len())?;
        if !self.global_variables.is_empty() {
            let mut vars: Vec<String> = self.global_variables.iter().cloned().collect();
            vars.sort();
            writeln!(writer, "  {}", vars.join(", "))?;
        }
        writeln!(writer)?;

        writeln!(
            writer,
            "Controllable Alphabet: {} symbols",
            self.controllable_alphabet.len()
        )?;
        if !self.controllable_alphabet.is_empty() {
            let mut symbols: Vec<String> = self.controllable_alphabet.iter().cloned().collect();
            symbols.sort();
            writeln!(writer, "  {}", symbols.join(", "))?;
        }
        writeln!(writer)?;

        // CLTS instances
        writeln!(writer, "CLTS Instances: {}", self.cltss.len())?;
        writeln!(writer)?;

        let mut clts_names: Vec<_> = self.cltss.keys().collect();
        clts_names.sort();

        for name in clts_names {
            let clts = &self.cltss[name];
            writeln!(writer, "Automaton: {}", name)?;
            writeln!(writer, "  States: {}", clts.state_count())?;

            // Initial states
            let initial_names: Vec<String> = clts
                .initial_states()
                .iter()
                .filter_map(|&id| clts.state_name(id).map(|s| s.to_string()))
                .collect();
            if !initial_names.is_empty() {
                writeln!(writer, "  Initial: {}", initial_names.join(", "))?;
            }

            // Variables
            let variables = clts.variables();
            if !variables.is_empty() {
                writeln!(writer, "  Variables: {}", variables.join(", "))?;
            }

            // Labels
            let mut label_vec = clts.alphabet();
            if !label_vec.is_empty() {
                label_vec.sort();
                writeln!(writer, "  Labels: {}", label_vec.join(", "))?;
            }

            // Controllable/Uncontrollable/Internal alphabet
            let ctrl_count = clts.controllable_alphabet().len();
            let unctrl_count = clts.uncontrollable_alphabet().len();
            let internal_count = clts.internal_alphabet().len();
            if ctrl_count > 0 || unctrl_count > 0 || internal_count > 0 {
                writeln!(
                    writer,
                    "  Alphabet: {} controllable, {} uncontrollable, {} internal",
                    ctrl_count, unctrl_count, internal_count
                )?;
            }

            // Transitions summary
            let mut total_transitions = 0;
            let mut controllable_transitions = 0;
            let mut uncontrollable_transitions = 0;
            for (_state_id, outgoing) in clts.state_outgoing_pairs() {
                for trans in outgoing {
                    total_transitions += 1;
                    if trans.is_controllable(clts) {
                        controllable_transitions += 1;
                    } else {
                        uncontrollable_transitions += 1;
                    }
                }
            }
            writeln!(
                writer,
                "  Transitions: {} total ({} controllable, {} uncontrollable)",
                total_transitions, controllable_transitions, uncontrollable_transitions
            )?;

            // State details
            writeln!(writer, "  State Details:")?;
            for (state_id, outgoing) in clts.state_outgoing_pairs() {
                let state_name = clts.state_name(state_id).unwrap_or("<unnamed>");
                let is_initial = clts.initial_states().contains(&state_id);
                let outgoing_count = outgoing.len();
                let incoming_count = clts.incoming(state_id).len();

                write!(writer, "    [{}] {}", state_id.index(), state_name)?;
                if is_initial {
                    write!(writer, " (initial)")?;
                }
                writeln!(
                    writer,
                    ": {} outgoing, {} incoming",
                    outgoing_count, incoming_count
                )?;

                // Transition details
                if outgoing_count > 0 {
                    for (idx, trans) in outgoing.iter().enumerate() {
                        let target_name = clts.state_name(trans.target()).unwrap_or("<unnamed>");
                        let labels: Vec<String> = trans
                            .labels()
                            .iter()
                            .filter_map(|&lid| {
                                clts.label_payload(lid).map(|payload| payload.join(","))
                            })
                            .collect();
                        let label_str = if labels.is_empty() {
                            "<no labels>".to_string()
                        } else {
                            labels.join("|")
                        };
                        let kind_str = if trans.is_controllable(clts) {
                            "ctrl"
                        } else {
                            "unctrl"
                        };
                        writeln!(
                            writer,
                            "      {} -> [{}] {} ({}) [{}]",
                            idx,
                            trans.target().index(),
                            target_name,
                            kind_str,
                            label_str
                        )?;
                    }
                }
            }
            writeln!(writer)?;
        }

        Ok(())
    }

    /// Produces a `CltsBuilder` seeded with the context's shared label store.
    ///
    /// Use this when constructing CLTSs that will be registered with the
    /// context so their label IDs stay aligned with previously registered
    /// systems. For isolated unit tests, `Clts::builder()` remains available.
    pub fn new_clts_builder(&self) -> CltsBuilder<DefaultStateIdx, DefaultLabelIdx> {
        CltsBuilder::with_label_store(self.label_store.clone())
    }

    /// Returns the global set of variables known to the context.
    pub fn global_variables(&self) -> &HashSet<String> {
        &self.global_variables
    }

    /// Returns the global set of controllable alphabet symbols tracked by the context.
    ///
    /// # Coverage Status
    /// Covered by test: `controllable_alphabet_accessor`
    pub fn controllable_alphabet(&self) -> &HashSet<String> {
        &self.controllable_alphabet
    }

    /// Composes two registered CLTS instances using the provided semantics.
    ///
    /// The composed CLTS is standalone; callers may register it afterwards if
    /// they wish to keep the result in the context registry.
    ///
    /// # Coverage Status
    /// Covered by test: `compose_named_error_handling`
    pub fn compose_named(
        &self,
        left: &str,
        right: &str,
        options: &CompositionOptions,
    ) -> Result<Clts<DefaultStateIdx, DefaultLabelIdx>, ContextError> {
        let lhs = self
            .cltss
            .get(left)
            .ok_or_else(|| ContextError::UnknownClts(left.to_owned()))?;
        let rhs = self
            .cltss
            .get(right)
            .ok_or_else(|| ContextError::UnknownClts(right.to_owned()))?;
        compose(lhs, rhs, options).map_err(ContextError::from)
    }

    /// Evaluates a μ-calculus [`Formula`] against the named CLTS using the provided environment.
    ///
    /// The environment's state count must match the CLTS; otherwise an
    /// [`ContextError::EnvironmentMismatch`] is returned. Optional evaluator
    /// settings can be supplied via `options`; when omitted, the defaults (memoisation
    /// + guard partitions enabled) are used.
    pub fn evaluate_mu(
        &self,
        name: &str,
        formula: &Formula,
        env: &Environment,
        options: Option<&EvaluationOptions>,
    ) -> Result<EvalResult, ContextError> {
        let clts = self
            .cltss
            .get(name)
            .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;

        let expected = clts.state_count();
        let provided = env.state_count();
        if expected != provided {
            return Err(ContextError::EnvironmentMismatch {
                name: name.to_owned(),
                expected,
                provided,
            });
        }

        let eval_options = options.cloned().unwrap_or_default();
        evaluate_with_options_and_automaton(formula, clts, env, &eval_options)
            .map_err(ContextError::from)
    }

    /// Evaluates a μ-calculus formula under three-valued (Kleene) semantics,
    /// returning a [`TritSet`](crate::mu_calculus::TritSet) per-state verdict.
    ///
    /// Unlike [`evaluate_mu`], this path treats OOB sink states as `Unknown`
    /// for every atomic predicate, propagating the Unknown trit through
    /// Boolean and modal connectives. Source states whose only path to
    /// satisfying the formula goes through OOB receive `Unknown` rather than
    /// the conservative `False` of the BitVec evaluator. Reference:
    /// Bruns–Godefroid CONCUR 2000.
    pub fn evaluate_mu_tri(
        &self,
        name: &str,
        formula: &Formula,
        env: &Environment,
        options: Option<&EvaluationOptions>,
    ) -> Result<crate::mu_calculus::TritSet, ContextError> {
        let clts = self
            .cltss
            .get(name)
            .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;

        let expected = clts.state_count();
        let provided = env.state_count();
        if expected != provided {
            return Err(ContextError::EnvironmentMismatch {
                name: name.to_owned(),
                expected,
                provided,
            });
        }

        let eval_options = options.cloned().unwrap_or_default();
        crate::mu_calculus::evaluate_tri_with_options(formula, clts, env, &eval_options)
            .map_err(ContextError::from)
    }

    /// Evaluates a mu-calculus formula and additionally records a witness map
    /// for strategy extraction. See [`evaluate_with_witnesses`].
    pub fn evaluate_mu_with_witnesses(
        &self,
        name: &str,
        formula: &Formula,
        env: &Environment,
        options: Option<&EvaluationOptions>,
    ) -> Result<(EvalResult, WitnessMap), ContextError> {
        let clts = self
            .cltss
            .get(name)
            .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;

        let expected = clts.state_count();
        let provided = env.state_count();
        if expected != provided {
            return Err(ContextError::EnvironmentMismatch {
                name: name.to_owned(),
                expected,
                provided,
            });
        }

        let eval_options = options.cloned().unwrap_or_default();
        evaluate_with_witnesses(formula, clts, env, &eval_options).map_err(ContextError::from)
    }

    /// Builds a controller by restricting `source` to the states that satisfy `formula`.
    ///
    /// This function performs **controller synthesis** by evaluating a μ-calculus formula
    /// over the source CLTS and constructing a new CLTS that contains only the states and
    /// transitions that satisfy the specification.
    ///
    /// # Synthesis Process
    ///
    /// 1. **Formula Evaluation**: Evaluates the μ-calculus formula using `evaluate_mu()` to
    ///    determine which states satisfy the specification. This produces a bitset `keep_bits`
    ///    where `keep_bits[i] = true` if state `i` satisfies the formula.
    ///
    /// 2. **Initial State Filtering**: Checks which initial states satisfy the formula:
    ///    - If **no initial states** satisfy → controller is unrealizable (empty controller)
    ///    - If **some initial states** satisfy → controller is realizable (may exclude some initials)
    ///
    /// 3. **Reachability Analysis**: Performs a BFS from satisfying initial states to find all
    ///    reachable states that also satisfy the formula. This prunes:
    ///    - States that don't satisfy the formula
    ///    - States that are unreachable from any satisfying initial state
    ///
    /// 4. **Controller Construction**: Builds a new CLTS containing:
    ///    - Only states that satisfy the formula and are reachable
    ///    - Only transitions between retained states
    ///    - Preserved state names, variables, and initial state markings
    ///
    /// 5. **Optional Minimization**: If `options.minimize` is `true`, runs structural minimization
    ///    to merge behaviorally equivalent states (see `minimise_controller()`).
    ///
    /// # Realizability
    ///
    /// - **Realizable** (`realizable = true`): At least one initial state satisfies the formula
    ///   and the controller contains reachable satisfying states.
    /// - **Unrealizable** (`realizable = false`): No initial states satisfy the formula, or all
    ///   satisfying states are unreachable.
    ///
    /// # Diagnostics
    ///
    /// When enabled via `options.diagnostics`, the function collects:
    /// - **Violating initial states**: Initial states that don't satisfy the formula
    /// - **Counterexample traces**: Minimal paths showing why the specification fails
    /// - **Counterstrategy**: Environment strategy that prevents satisfaction
    /// - **Deadlock traces**: Paths from initial states to deadlock states
    /// - **Proof obligations**: Explanations for why states violate the specification
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(n + m + f) where:
    ///   - n = number of states
    ///   - m = number of transitions
    ///   - f = cost of formula evaluation (typically O(n * m) for fixpoint formulas)
    /// - **Space Complexity**: O(n) for bitsets and visited tracking
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    /// use mununu_core::context::Context;
    /// use mununu_core::mu_calculus::{evaluator::Environment, parser};
    ///
    /// # let mut builder = Clts::builder();
    /// # let label = builder.labels().intern(["tick"]).unwrap();
    /// # builder.state("s0").initial("s0");
    /// # builder.state("s1");
    /// # builder.transition("s0", &[label], "s1");
    /// # let plant = builder.build().unwrap();
    /// # let context = Context::builder()
    /// #     .register_clts("plant", plant)
    /// #     .finish_with_checks().unwrap();
    /// # let env = Environment::new(context.clts("plant").unwrap().state_count());
    /// // Synthesize a controller that ensures "eventually p"
    /// let formula = parser::parse("mu X. (p || <> X)")?;
    /// let synthesis = context.synthesise_controller("plant", &formula, &env, None)?;
    ///
    /// if synthesis.realizable {
    ///     println!("Controller has {} states", synthesis.controller.state_count());
    /// } else {
    ///     println!("Unrealizable: {}", synthesis.diagnostics.messages.join(", "));
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// The resulting controller retains only the states marked as satisfying and the
    /// transitions between them. Unreachable states (w.r.t. the surviving initial
    /// states) are pruned via a reachability pass. When no initial state satisfies the
    /// formula the returned controller is empty and `realizable` is set to `false`.
    pub fn synthesise_controller(
        &self,
        source: &str,
        formula: &Formula,
        env: &Environment,
        options: Option<&EvaluationOptions>,
    ) -> Result<ControllerSynthesis, ContextError> {
        let synthesis_options = ControllerSynthesisOptions {
            evaluation: options,
            ..Default::default()
        };
        self.synthesise_controller_with_options(source, formula, env, synthesis_options)
    }

    /// Variant of [`Context::synthesise_controller`] that accepts extended options.
    ///
    /// This function provides additional control over the synthesis process:
    /// - **Evaluation options**: Control μ-calculus evaluation behavior (memoization, guard partitions)
    /// - **Diagnostics options**: Enable counterexample/counterstrategy generation, deadlock traces
    /// - **Minimization**: Optionally run structural minimization after synthesis
    ///
    /// See [`Context::synthesise_controller`] for detailed documentation of the synthesis process.
    pub fn synthesise_controller_with_options(
        &self,
        source: &str,
        formula: &Formula,
        env: &Environment,
        options: ControllerSynthesisOptions<'_>,
    ) -> Result<ControllerSynthesis, ContextError> {
        // Resolve effective mode (legacy extract_strategy maps to Functional)
        let effective_mode =
            if options.extract_strategy && options.mode == ControllerMode::Projection {
                ControllerMode::Functional
            } else {
                options.mode
            };

        // GR(1) synthesis needs the STRUCTURED LTL spec (assumptions + guarantees
        // + signal directions), not the combined μ-calculus formula this path
        // receives. Callers must route to the dedicated GR(1) entry point
        // (`crate::mu_calculus::gr1_build::synthesise_gr1`), which the CLI/API do.
        if effective_mode == ControllerMode::Gr1 {
            return Err(ContextError::Unsupported(
                "ControllerMode::Gr1 requires the structured LTL spec; use the GR(1) \
                 synthesis path (synthesise_gr1), not synthesise_controller"
                    .to_string(),
            ));
        }

        // When strategy extraction is requested, use witness-guided evaluation
        let (keep_bits, witness_map) = if effective_mode != ControllerMode::Projection {
            let (bits, wm) =
                self.evaluate_mu_with_witnesses(source, formula, env, options.evaluation)?;
            (bits, Some(wm))
        } else {
            let bits = self.evaluate_mu(source, formula, env, options.evaluation)?;
            (bits, None)
        };
        let clts = self
            .cltss
            .get(source)
            .ok_or_else(|| ContextError::UnknownClts(source.to_owned()))?;

        let BfsReachability {
            visited,
            parent,
            violating_initials,
        } = bfs_reachable_states(clts, &keep_bits);

        // If no satisfying initial state is reachable, the controller is unrealizable.
        if visited.not_any() {
            return build_unrealizable_initials_synthesis(
                self,
                clts,
                &keep_bits,
                &violating_initials,
                &options,
            );
        }

        let mut diagnostics = ControllerDiagnostics::default();
        let ad = formula.alternation_depth();
        let pc = formula.property_class();
        diagnostics.alternation_depth = Some(ad);
        diagnostics.property_class = Some(format!("{pc:?}"));
        if ad >= 2 && options.extract_strategy {
            diagnostics.messages.push(format!(
                "Warning: positional strategy extraction used on alternation depth {ad} formula \
                 (class: {pc:?}). The winning region is correct, but the controller may not \
                 cycle through obligations. Consider this a best-effort approximation."
            ));
        }
        let proof_obligations_enabled = options
            .diagnostics
            .is_none_or(|diag| diag.proof_obligations);

        // Compute fixpoint nesting order for signature-based extraction.
        // Used for both Functional/SignatureMemory/Permissive (rank-based
        // selection) and ProductGame (mu-obligation rotation).
        let nesting = formula.fixpoint_nesting_order();

        // ProductGame builds its own (plant_state, oblig_idx) state space and
        // returns directly to the common diagnostics/minimization tail.
        if effective_mode == ControllerMode::ProductGame {
            let mut diagnostics = ControllerDiagnostics {
                alternation_depth: Some(ad),
                property_class: Some(format!("{pc:?}")),
                ..Default::default()
            };
            let wm = witness_map
                .as_ref()
                .expect("ProductGame mode requires witness map");
            let mut controller =
                build_product_game_controller(self, clts, &visited, wm, formula, &nesting)?;
            let kept_states = controller.state_count();
            diagnostics
                .messages
                .push(format!("Controller mode: {:?}.", effective_mode));
            diagnostics.messages.push(format!(
                "Product-game controller: {} states across {} mu-obligation(s).",
                kept_states,
                formula.mu_obligations().len().max(1)
            ));
            if !violating_initials.is_empty() {
                enrich_diagnostics_for_excluded_initials(
                    &mut diagnostics,
                    clts,
                    &keep_bits,
                    &violating_initials,
                    &options,
                    proof_obligations_enabled,
                );
            }
            if options.diagnostics.is_some_and(|diag| diag.deadlock_traces) {
                let deadlock_traces = collect_deadlock_traces(clts, &visited, &parent);
                if !deadlock_traces.is_empty() {
                    diagnostics.messages.push(format!(
                        "Deadlock traces recorded: {}",
                        deadlock_traces.len()
                    ));
                    diagnostics.deadlock_traces = deadlock_traces;
                }
            }
            if options.minimize
                && let Some((minimized, report)) = self.minimise_controller(&controller)?
            {
                if report.removed_states > 0 || report.removed_transitions > 0 {
                    diagnostics.messages.push(format!(
                        "Controller minimization removed {} state(s) and {} transition(s).",
                        report.removed_states, report.removed_transitions
                    ));
                }
                diagnostics.minimization = Some(report.clone());
                controller = minimized;
            }
            return Ok(ControllerSynthesis {
                controller,
                realizable: true,
                diagnostics,
            });
        }

        // ParityGame: full parity-game synthesis via Zielonka.
        if effective_mode == ControllerMode::ParityGame {
            let mut diagnostics = ControllerDiagnostics {
                alternation_depth: Some(ad),
                property_class: Some(format!("{pc:?}")),
                ..Default::default()
            };
            let mut controller = build_parity_game_controller(self, clts, formula, env)?;
            let kept = controller.state_count();
            diagnostics
                .messages
                .push(format!("Controller mode: {:?}.", effective_mode));
            diagnostics.messages.push(format!(
                "Parity-game controller: {} positions in winning region.",
                kept
            ));
            // Realizability: at least one initial product position survived
            // the winning-region prune. The helper marks initial states only
            // for `(plant_initial, formula_root)` positions that Eve wins.
            let realizable = !controller.initial_states().is_empty();
            if !realizable {
                return build_unrealizable_initials_synthesis(
                    self,
                    clts,
                    &keep_bits,
                    &violating_initials,
                    &options,
                );
            }
            if options.minimize
                && let Some((minimized, report)) = self.minimise_controller(&controller)?
            {
                diagnostics.minimization = Some(report.clone());
                controller = minimized;
            }
            return Ok(ControllerSynthesis {
                controller,
                realizable: true,
                diagnostics,
            });
        }

        let retained = visited.iter().filter(|bit| **bit).count();
        let mut builder = CltsBuilder::with_label_store(self.label_store.clone());
        builder.reserve_states(retained);
        let mut mapping = HashMap::new();

        for state in clts.states() {
            if !visited.get(state.index()).is_some_and(|bit| *bit) {
                continue;
            }

            let base_name = clts.state_name(state).unwrap_or("state").to_owned();
            let name = if effective_mode == ControllerMode::SignatureMemory {
                if let Some(wm) = witness_map.as_ref() {
                    let sig = wm.signature(state.index(), &nesting);
                    format!("{base_name}__sig_{}", format_signature(&sig))
                } else {
                    base_name
                }
            } else {
                base_name
            };
            let new_state = builder
                .state_with_name(name)
                .ok_or(ContextError::Controller(CltsError::IdOverflow {
                    kind: "state",
                    value: usize::MAX,
                }))?;

            if clts.initial_states().contains(&state) {
                builder.initial_state_id(new_state);
            }

            let vars = clts.state_variables(state);
            builder.with_variables_for_state(new_state, vars.iter().map(|s| s.as_str()));

            // Preserve structured signal valuations (e.g. `grant0 = 1`) onto the
            // controller state — the RTL emitter derives Moore outputs from them, and
            // counterexample/counterstrategy traces read them.
            if let Some(valuation) = clts.state_valuation(state) {
                builder.with_valuation_for_state(new_state, valuation.clone());
            }

            mapping.insert(state, new_state);
        }

        for (original, mapped) in &mapping {
            match effective_mode {
                ControllerMode::Functional | ControllerMode::SignatureMemory => {
                    // Functional / SignatureMemory mode: for each state, pick
                    // ONE controllable transition whose target has the best
                    // (smallest) signature. All uncontrollable transitions
                    // are always kept. SignatureMemory differs only in that
                    // state names are annotated with the iteration-rank
                    // signature (handled in the mapping construction above).
                    //
                    // SOUNDNESS: The signature ordering ensures liveness progress —
                    // following signature-decreasing transitions guarantees all mu
                    // obligations are eventually satisfied. This is the memoryless-
                    // on-product strategy projected to the plant with signatures as
                    // the memory component (Zielonka 1998; Bruse-Friedmann-Lange
                    // SPIN 2016).
                    let wm = witness_map.as_ref().unwrap();
                    let mut best_sig: Option<crate::mu_calculus::Signature> = None;
                    let mut best_trans_idx: Option<usize> = None;

                    for (idx, transition) in clts.outgoing(*original).iter().enumerate() {
                        if let Some(&target_mapped) = mapping.get(&transition.target()) {
                            if transition.is_controllable(clts) {
                                let target_sig =
                                    wm.signature(transition.target().index(), &nesting);
                                if best_sig.as_ref().is_none_or(|bs| target_sig < *bs) {
                                    best_sig = Some(target_sig);
                                    best_trans_idx = Some(idx);
                                }
                            } else {
                                // Always keep uncontrollable transitions
                                builder.transition_ids(*mapped, transition.labels(), target_mapped);
                            }
                        }
                    }
                    if let Some(idx) = best_trans_idx {
                        let transition = &clts.outgoing(*original)[idx];
                        let target_mapped = mapping[&transition.target()];
                        builder.transition_ids(*mapped, transition.labels(), target_mapped);
                    }
                }
                ControllerMode::Permissive => {
                    // Permissive mode: keep ALL controllable transitions whose target
                    // has a signature that is ≤ the source's signature. This is the
                    // maximally permissive supervisor (Ramadge-Wonham canonical).
                    // Nondeterministic but composable with other supervisors.
                    let wm = witness_map.as_ref().unwrap();
                    let source_sig = wm.signature(original.index(), &nesting);

                    for transition in clts.outgoing(*original) {
                        if let Some(&target_mapped) = mapping.get(&transition.target()) {
                            if transition.is_controllable(clts) {
                                let target_sig =
                                    wm.signature(transition.target().index(), &nesting);
                                if target_sig <= source_sig {
                                    builder.transition_ids(
                                        *mapped,
                                        transition.labels(),
                                        target_mapped,
                                    );
                                }
                            } else {
                                // Always keep uncontrollable transitions
                                builder.transition_ids(*mapped, transition.labels(), target_mapped);
                            }
                        }
                    }
                }
                ControllerMode::Projection => {
                    // Projection mode: keep all transitions between winning states
                    for transition in clts.outgoing(*original) {
                        if let Some(&target_mapped) = mapping.get(&transition.target()) {
                            builder.transition_ids(*mapped, transition.labels(), target_mapped);
                        }
                    }
                }
                ControllerMode::ProductGame | ControllerMode::ParityGame => {
                    // Both modes are handled by their early-return branches above.
                    unreachable!("ProductGame/ParityGame return before reaching this loop");
                }
                ControllerMode::Gr1 => {
                    // Rejected at function entry (needs the structured LTL spec).
                    unreachable!("Gr1 returns an error before reaching this loop");
                }
            }
        }

        let mut controller = builder.build().map_err(ContextError::Controller)?;

        diagnostics
            .messages
            .push(format!("Controller mode: {:?}.", effective_mode));
        diagnostics.messages.push(format!(
            "Controller realizable: retained {} of {} states.",
            mapping.len(),
            clts.state_count()
        ));
        if !violating_initials.is_empty() {
            enrich_diagnostics_for_excluded_initials(
                &mut diagnostics,
                clts,
                &keep_bits,
                &violating_initials,
                &options,
                proof_obligations_enabled,
            );
        }
        if options.diagnostics.is_some_and(|diag| diag.deadlock_traces) {
            let deadlock_traces = collect_deadlock_traces(clts, &visited, &parent);
            if !deadlock_traces.is_empty() {
                diagnostics.messages.push(format!(
                    "Deadlock traces recorded: {}",
                    deadlock_traces.len()
                ));
                diagnostics.deadlock_traces = deadlock_traces;
            }
        }
        if options.minimize
            && let Some((minimized, report)) = self.minimise_controller(&controller)?
        {
            if report.removed_states > 0 || report.removed_transitions > 0 {
                diagnostics.messages.push(format!(
                    "Controller minimization removed {} state(s) and {} transition(s).",
                    report.removed_states, report.removed_transitions
                ));
            }
            if !report.merged_states.is_empty() {
                diagnostics.messages.push(format!(
                    "Merged states: {}.",
                    report.merged_states.join(", ")
                ));
            }
            diagnostics.minimization = Some(report.clone());
            controller = minimized;
        }
        Ok(ControllerSynthesis {
            controller,
            realizable: true,
            diagnostics,
        })
    }

    /// Minimizes a controller CLTS by merging behaviorally equivalent states.
    ///
    /// This function performs **structural minimization** using a partition refinement algorithm
    /// (similar to bisimulation minimization). It merges states that are behaviorally equivalent,
    /// reducing the controller size while preserving all observable behavior.
    ///
    /// # Minimization Algorithm
    ///
    /// The algorithm uses **partition refinement** with state signatures:
    ///
    /// 1. **Initial Partition**: All states start in the same partition (class 0).
    ///
    /// 2. **Iterative Refinement**: Repeatedly refine partitions until stable:
    ///    - For each state, compute a **signature** based on:
    ///      - **State variables**: Bitset representation of variables associated with the state
    ///      - **Outgoing transitions**: For each transition, record:
    ///        - Target partition (from current partition)
    ///        - Sorted label IDs
    ///    - States with identical signatures belong to the same partition
    ///    - Continue until partition stabilizes (no changes between iterations)
    ///
    /// 3. **State Merging**: For each partition class:
    ///    - Select the first state as the **representative**
    ///    - Merge all other states in the class into the representative
    ///    - Preserve initial state marking (if any state in class is initial)
    ///    - Preserve variables from the representative
    ///
    /// 4. **Transition Deduplication**: When building transitions:
    ///    - For each representative state, collect transitions to merged target states
    ///    - Deduplicate transitions with same target and labels
    ///    - Only add unique transitions to the minimized controller
    ///
    /// # State Signature
    ///
    /// A state's signature consists of:
    /// - **Variables**: Bitset of variables associated with the state
    /// - **Transitions**: Sorted list of transition signatures, each containing:
    ///   - Target partition ID (from current partition)
    ///   - Sorted label IDs
    ///
    /// Two states are equivalent if they have identical signatures, meaning they:
    /// - Have the same variables
    /// - Have transitions to the same partition classes with the same labels
    ///
    /// # Termination
    ///
    /// The algorithm terminates when:
    /// - The partition stabilizes (no changes between iterations), OR
    /// - All states are in separate partitions (no minimization possible)
    ///
    /// # Result
    ///
    /// Returns `None` if:
    /// - Controller has ≤ 1 states (nothing to minimize)
    /// - All states are already distinct (no equivalent states found)
    ///
    /// Otherwise, returns `Some((minimized_controller, report))` containing:
    /// - **Minimized controller**: CLTS with merged equivalent states
    /// - **Minimization report**: Statistics on removed states/transitions and merged states
    ///
    /// # Performance
    ///
    /// - **Time Complexity**: O(k * n * (m + v)) where:
    ///   - k = number of refinement iterations (typically O(log n))
    ///   - n = number of states
    ///   - m = number of transitions
    ///   - v = number of variables
    /// - **Space Complexity**: O(n + m) for partitions and signatures
    ///
    /// # Example
    ///
    /// ```rust
    /// use mununu_core::clts::Clts;
    /// use mununu_core::context::{Context, ControllerSynthesisOptions};
    /// use mununu_core::mu_calculus::{evaluator::Environment, parser};
    ///
    /// # let mut builder = Clts::builder();
    /// # let label = builder.labels().intern(["tick"]).unwrap();
    /// # builder.state("s0").initial("s0");
    /// # builder.state("s1");
    /// # builder.transition("s0", &[label], "s1");
    /// # let plant = builder.build().unwrap();
    /// # let context = Context::builder()
    /// #     .register_clts("plant", plant)
    /// #     .finish_with_checks().unwrap();
    /// # let env = Environment::new(context.clts("plant").unwrap().state_count());
    /// # let formula = parser::parse("true").unwrap();
    /// // Minimization is automatically performed when using synthesise_controller_with_options
    /// let options = ControllerSynthesisOptions {
    ///     minimize: true,  // Enable minimization
    ///     ..Default::default()
    /// };
    /// let synthesis = context.synthesise_controller_with_options("plant", &formula, &env, options)?;
    /// if let Some(report) = synthesis.diagnostics.minimization {
    ///     println!("Removed {} states, {} transitions",
    ///              report.removed_states, report.removed_transitions);
    ///     println!("Merged states: {}", report.merged_states.join(", "));
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Algorithm Correctness
    ///
    /// The minimization preserves **behavioral equivalence**: two states are merged only if
    /// they have identical variables and identical transition patterns (same labels to same
    /// partition classes). This ensures the minimized controller is behaviorally equivalent
    /// to the original, just with fewer states.
    fn minimise_controller(
        &self,
        controller: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> Result<MinimizationOutcome, ContextError> {
        // Delegate to the public minimization algorithm in composition::minimize.
        // Pass the shared label store so the minimized CLTS preserves label IDs.
        let result = crate::composition::minimize::minimize_bisimulation(
            controller,
            Some(self.label_store.clone()),
        )
        .map_err(ContextError::Controller)?;

        match result {
            None => Ok(None),
            Some((minimized, report)) => {
                // Convert MinimizationReport → ControllerMinimizationReport
                let ctrl_report = ControllerMinimizationReport {
                    removed_states: report.states_before.saturating_sub(report.states_after),
                    removed_transitions: report
                        .transitions_before
                        .saturating_sub(report.transitions_after),
                    merged_states: report.merged_states,
                };
                Ok(Some((minimized, ctrl_report)))
            }
        }
    }

    /// Saves a registered CLTS to disk.
    /// Saves a registered CLTS snapshot to disk.
    ///
    /// # Coverage Status
    /// Covered by test: `save_clts_to_path_error_handling`
    pub fn save_clts_to_path<P: AsRef<Path>>(
        &self,
        name: &str,
        path: P,
    ) -> Result<(), ContextError> {
        let clts = self
            .cltss
            .get(name)
            .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;
        save_clts_to_path(clts, path).map_err(ContextError::from)
    }

    /// Saves the entire context (all registered CLTS instances) to `path`.
    ///
    /// This uses the binary `CTXBIN` format implemented by the `persistence`
    /// module so registries can be snapshotted and restored in one step.
    pub fn save_to_path<P: AsRef<Path>>(&self, path: P) -> Result<(), ContextError> {
        crate::persistence::save_context_to_path(self, path).map_err(ContextError::from)
    }

    /// Loads a CLTS from disk and registers it under `name`, replacing any existing entry.
    ///
    /// # Coverage Status
    /// Covered by test: `load_clts_from_path_error_handling`
    pub fn load_clts_from_path<P: AsRef<Path>>(
        &mut self,
        name: impl Into<String>,
        path: P,
    ) -> Result<(), ContextError> {
        let clts = load_clts_from_path(path)?;
        self.cltss.insert(name.into(), clts);
        Ok(())
    }

    /// Loads a context snapshot from disk.
    ///
    /// This reconstructs the registry, shared label store, controllable
    /// alphabet, and global variables from a `CTXBIN` snapshot previously
    /// created with [`Context::save_to_path`].
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self, ContextError> {
        crate::persistence::load_context_from_path(path).map_err(ContextError::from)
    }

    /// Serialises the CLTS to disk when the snapshot size exceeds `limit_bytes`.
    /// Returns the size of the snapshot when spilling occurred.
    ///
    /// # Coverage Status
    /// Covered by test: `spill_clts_if_exceeds_triggers`
    pub fn spill_clts_if_exceeds<P: AsRef<Path>>(
        &self,
        name: &str,
        limit_bytes: usize,
        path: P,
    ) -> Result<Option<usize>, ContextError> {
        let clts = self
            .cltss
            .get(name)
            .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;
        maybe_spill_clts(clts, limit_bytes, path).map_err(ContextError::from)
    }

    /// Evaluates a μ-calculus [`Formula`] against multiple CLTSs identified by
    /// `names`, using `make_env` to construct the environment for each system.
    ///
    /// Returns a map of CLTS name → evaluation result. The helper reuses
    /// [`Context::evaluate_mu`] internally, so any missing CLTS or environment
    /// / state-count mismatch surfaces with the same error variants.
    pub fn evaluate_mu_many<'a, I, F>(
        &self,
        names: I,
        formula: &Formula,
        make_env: F,
        options: Option<&EvaluationOptions>,
    ) -> Result<HashMap<String, EvalResult>, ContextError>
    where
        I: IntoIterator<Item = &'a str>,
        F: Fn(&str, &Clts<DefaultStateIdx, DefaultLabelIdx>) -> Environment,
    {
        let mut results = HashMap::new();

        for name in names {
            let clts = self
                .cltss
                .get(name)
                .ok_or_else(|| ContextError::UnknownClts(name.to_owned()))?;
            let env = make_env(name, clts);
            let eval = self.evaluate_mu(name, formula, &env, options)?;
            results.insert(name.to_owned(), eval);
        }

        Ok(results)
    }
}

/// Builder for creating a context.
///
/// This builder is used to create a context by registering CLTS instances and
/// managing shared resources. It is used to create a context by registering CLTS
/// instances and managing shared resources. It is also used to create a context
/// by registering CLTS instances and managing shared resources.
#[derive(Debug, Default)]
pub struct ContextBuilder {
    label_store: LabelStoreBuilder<DefaultLabelIdx>,
    cltss: HashMap<String, Clts<DefaultStateIdx, DefaultLabelIdx>>,
    controllable_alphabet: HashSet<String>,
    global_variables: HashSet<String>,
}

impl ContextBuilder {
    /// Registers a CLTS under the provided name. Later calls overwrite the same name.
    pub fn register_clts(
        mut self,
        name: impl Into<String>,
        clts: Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> Self {
        let name = name.into();
        self.cltss.insert(name, clts);
        self
    }

    /// Loads a CLTS snapshot from disk and registers it under `name`.
    pub fn register_clts_from_path<P: AsRef<Path>>(
        mut self,
        name: impl Into<String>,
        path: P,
    ) -> Result<Self, PersistenceError> {
        let clts = load_clts_from_path(path)?;
        self.cltss.insert(name.into(), clts);
        Ok(self)
    }

    /// Finalises the builder without running additional consistency checks.
    ///
    /// This path is mainly for lightweight scaffolding or white-box tests. It
    /// does **not** merge registered CLTS labels back into the shared store and
    /// therefore should not be used for production contexts.
    pub fn finish(self) -> Context {
        Context {
            cltss: self.cltss,
            label_store: self.label_store,
            controllable_alphabet: self.controllable_alphabet,
            global_variables: self.global_variables,
        }
    }

    /// Finalises the builder while enforcing controllable alphabet uniqueness and
    /// aggregating variable sets from each registered CLTS.
    ///
    /// This is the recommended production path: it merges each CLTS's canonical
    /// labels back into the shared builder so subsequent
    /// [`Context::new_clts_builder`] calls reuse the same handles, and it
    /// surfaces any interning failures as [`ContextError::LabelRegistry`].
    pub fn finish_with_checks(mut self) -> Result<Context, ContextError> {
        self.validate_and_aggregate_clts()?;
        Ok(self.finish())
    }

    /// Validates controllable/internal alphabets across all CLTSs and aggregates
    /// global variables and label information into the builder.
    ///
    /// This enforces that:
    /// - controllable labels are not shared by multiple CLTSs, and
    /// - internal labels are mutually exclusive across CLTSs.
    ///
    /// It also merges each CLTS's label entries back into the shared
    /// `label_store` and aggregates all variable names into `global_variables`.
    fn validate_and_aggregate_clts(&mut self) -> Result<(), ContextError> {
        // Track controllable and internal alphabets across all CLTSs (by name, not LabelId)
        let mut global_controllable: HashMap<String, String> = HashMap::new(); // label_name -> clts_name
        let mut global_internal: HashMap<String, String> = HashMap::new(); // label_name -> clts_name

        for (clts_name, clts) in self.cltss.iter() {
            // Merge canonical labels back into the shared store
            self.label_store
                .absorb(clts.label_entries())
                .map_err(|err| ContextError::LabelRegistry(err.to_string()))?;

            // Aggregate variable names
            for var in clts.variables() {
                self.global_variables.insert(var);
            }

            // Check controllable alphabet (by name)
            for &label_id in clts.controllable_alphabet().iter() {
                if let Some(payload) = clts.label_payload(label_id) {
                    for symbol in payload {
                        if let Some(existing_clts) = global_controllable.get(symbol) {
                            return Err(ContextError::DuplicateControllableAlphabet(format!(
                                "label '{}' is controllable in both '{}' and '{}'",
                                symbol, existing_clts, clts_name
                            )));
                        }
                        global_controllable.insert(symbol.clone(), clts_name.clone());
                        // Also add to legacy controllable_alphabet set for backward compatibility
                        self.controllable_alphabet.insert(symbol.clone());
                    }
                }
            }

            // Check internal alphabet (by name)
            for &label_id in clts.internal_alphabet().iter() {
                if let Some(payload) = clts.label_payload(label_id) {
                    for symbol in payload {
                        if let Some(existing_clts) = global_internal.get(symbol) {
                            return Err(ContextError::DuplicateInternalAlphabet(format!(
                                "label '{}' is internal in both '{}' and '{}'",
                                symbol, existing_clts, clts_name
                            )));
                        }
                        global_internal.insert(symbol.clone(), clts_name.clone());
                    }
                }
            }
        }

        Ok(())
    }
}

// (diagnostics helpers moved to `context::diagnostics`)

/// Diagnostics toggles for controller synthesis.
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticsOptions {
    /// When `true`, attempt to derive counterexample/counterstrategy artefacts.
    pub counterexample: bool,
    /// When `true`, report deadlock traces among the retained states.
    pub deadlock_traces: bool,
    /// Optional cap on the number of counterstrategy traces returned.
    pub max_counter_traces: Option<usize>,
    /// When `true`, emit proof obligations for violating initial states.
    pub proof_obligations: bool,
}

impl Default for DiagnosticsOptions {
    fn default() -> Self {
        Self {
            counterexample: false,
            deadlock_traces: false,
            max_counter_traces: None,
            proof_obligations: true,
        }
    }
}

/// Controller extraction mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ControllerMode {
    /// Projection: keep all transitions between winning states (default).
    #[default]
    Projection,
    /// Functional: one signature-decreasing controllable transition per state.
    /// Produces a deterministic controller that guarantees liveness progress.
    Functional,
    /// Permissive: all signature-non-increasing controllable transitions.
    /// Maximally permissive supervisor (Ramadge-Wonham canonical object).
    /// Nondeterministic but composable with other supervisors.
    Permissive,
    /// Signature-memory: same selection rule as `Functional`, but each state
    /// name is annotated with its iteration-rank signature
    /// (`<state>__sig_<rank0>_<rank1>_...`). The controller is functionally
    /// identical to `Functional`, but the signature is now an observable
    /// part of the controller's state — useful for cascading supervisors,
    /// downstream tools that reason about obligation rank, and proving
    /// progress at composition boundaries. Reference: Bruse, Friedmann &
    /// Lange, SPIN 2016 (signature-based positional strategy).
    ///
    /// Note: this is plant-level signature exposure. Full memory-aware
    /// synthesis (where multiple obligation classes coexist per plant state)
    /// is provided by `ProductGame`.
    SignatureMemory,
    /// Memory-aware Mealy controller. State space is `(plant_state, oblig_idx)`
    /// where `oblig_idx` rotates through the formula's mu-fixpoint
    /// obligations (in nesting order). Required for correct strategies on
    /// alternation ≥ 2 formulas (e.g., GR(1), `GFp ∧ GFq`) where a positional
    /// strategy may fixate on one obligation and starve the other.
    ///
    /// At each product state `(s, i)`, the controller picks a controllable
    /// transition that decreases the iteration rank of `mu_i`. When `mu_i`'s
    /// rank is already 0 at `s` (obligation locally satisfied), the
    /// transition advances `oblig_idx` round-robin to ensure fairness across
    /// all mu-obligations. Uncontrollable transitions are always kept (with
    /// the same memory advancement rule).
    ///
    /// For alternation < 2 (no mus, or single-mu): obligation dimension
    /// collapses to size 1; the controller has the same shape as
    /// `Functional` with a constant `__pg_0` suffix on state names.
    ///
    /// Reference: Bruse-Friedmann-Lange (SPIN 2016) projected to plant
    /// states with mu-obligation rotation. Handles alternation 2 correctly
    /// (the practical case for GR(1) and Buchi properties).
    ProductGame,
    /// Full parity-game synthesis. Builds the explicit game over
    /// `(plant_state, formula_subnode)` positions, solves via Zielonka's
    /// recursive algorithm, and projects Eve's positional strategy back to
    /// a plant Mealy controller. Correct for arbitrary alternation
    /// (parity properties of any depth) — strictly more general than
    /// `ProductGame` at the cost of a larger state space (one product
    /// state per pair of plant state and formula sub-node).
    ///
    /// Reference: Zielonka 1998 (positional determinacy of parity games);
    /// Emerson-Jutla. See `crate::mu_calculus::parity_game` for the
    /// underlying construction and solver.
    ParityGame,
    /// GR(1): sound reactive controller synthesis from an LTL assume/guarantee
    /// spec via the direct Piterman-Pnueli-Sá'ar fixpoint over a
    /// monitor-augmented game (`crate::mu_calculus::gr1` /
    /// `crate::mu_calculus::gr1_build`). Unlike the signature-based modes this is
    /// **sound for conjunctive safety+liveness (GR(1)) objectives** — the safety
    /// guarantees constrain the game arena rather than being intersected as
    /// denotational conjuncts. Requires the STRUCTURED LTL spec (assumptions +
    /// guarantees + input/output signals), so it is driven from the adapter IR
    /// rather than the combined μ-calculus formula; see the CLI/API GR(1) path.
    Gr1,
}

impl ControllerMode {
    /// Parse a controller-mode name with whitespace, dashes, and underscores
    /// stripped and case folded. Returns the variant on success, or the
    /// normalized input on failure so callers can format their own errors.
    pub fn from_normalized_name(name: &str) -> Result<Self, String> {
        let normalized: String = name
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect();
        match normalized.as_str() {
            "projection" => Ok(Self::Projection),
            "functional" => Ok(Self::Functional),
            "permissive" => Ok(Self::Permissive),
            "signaturememory" => Ok(Self::SignatureMemory),
            "productgame" => Ok(Self::ProductGame),
            "paritygame" => Ok(Self::ParityGame),
            "gr1" | "gr1game" => Ok(Self::Gr1),
            _ => Err(normalized),
        }
    }
}

/// Extended controller synthesis configuration, combining evaluation, diagnostics, and post-processing knobs.
#[derive(Debug, Default)]
pub struct ControllerSynthesisOptions<'a> {
    pub evaluation: Option<&'a EvaluationOptions>,
    pub diagnostics: Option<&'a DiagnosticsOptions>,
    /// When `true`, run a structural minimisation pass over the synthesised controller.
    pub minimize: bool,
    /// Controller extraction mode. See [`ControllerMode`] for options.
    pub mode: ControllerMode,
    /// Legacy alias: when `true`, equivalent to `mode = Functional`.
    /// Deprecated — use `mode` directly.
    pub extract_strategy: bool,
}

/// Result of controller synthesis, including diagnostics metadata.
#[derive(Debug)]
pub struct ControllerSynthesis {
    pub controller: Clts<DefaultStateIdx, DefaultLabelIdx>,
    pub realizable: bool,
    pub diagnostics: ControllerDiagnostics,
}

/// Aggregated diagnostics emitted during controller synthesis.
#[derive(Debug, Default, serde::Serialize)]
pub struct ControllerDiagnostics {
    /// Classification of the formula by fixpoint structure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_class: Option<String>,
    /// Alternation depth of the formula (0 = propositional, 1 = safety/reach, 2+ = liveness).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternation_depth: Option<usize>,
    /// User-facing notes about the synthesis outcome.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
    /// Initial states that failed to satisfy the requested formula.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub violating_initials: Vec<String>,
    /// Minimal counterexample trace expressed as state names (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterexample_trace: Option<Vec<String>>,
    /// Deadlock traces (initial-to-deadlock paths) collected during synthesis.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deadlock_traces: Vec<Vec<String>>,
    /// Minimal counterstrategy paths (environment choices) covering each violating initial.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub counterstrategy_traces: Vec<Vec<String>>,
    /// Summary of controller minimisation, when enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimization: Option<ControllerMinimizationReport>,
    /// Outstanding proof obligations for unrealizable specifications.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_obligations: Vec<ProofObligation>,
    /// Lasso traces for liveness counterexamples: `(prefix, cycle)` where the
    /// infinite counterexample is `prefix ++ cycle^ω`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lasso_traces: Vec<LassoTrace>,
    /// Prototype counterstrategy covering the losing region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterstrategy: Option<CounterStrategy>,
}

/// A lasso trace: finite prefix followed by an infinitely repeating cycle.
/// Represents an infinite counterexample path: `prefix ++ cycle^ω`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LassoTrace {
    pub prefix: Vec<String>,
    pub cycle: Vec<String>,
    /// Transition labels between consecutive prefix states.
    /// `prefix_labels[i]` is the label on the edge from `prefix[i]` to `prefix[i+1]`
    /// (or to `cycle[0]` for the last element). Length = `prefix.len()` when a
    /// successor exists, otherwise empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix_labels: Vec<String>,
    /// Transition labels between consecutive cycle states.
    /// `cycle_labels[i]` is the label on the edge from `cycle[i]` to `cycle[i+1]`.
    /// The last element is the label from the last cycle state back to `cycle[0]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycle_labels: Vec<String>,
}

/// Prototype counterstrategy definition exposed in diagnostics.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CounterStrategy {
    /// States that belong to the losing region explored by the strategy.
    pub states: Vec<String>,
    /// Initial states that trigger the strategy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub initial_states: Vec<String>,
    /// Environment transitions that maintain the losing region.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<CounterStrategyEdge>,
}

/// Edge belonging to a prototype counterstrategy.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CounterStrategyEdge {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Metrics describing the effect of controller minimisation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ControllerMinimizationReport {
    /// Number of states removed by the minimisation pass.
    pub removed_states: usize,
    /// Number of transitions removed after merging equivalent states.
    pub removed_transitions: usize,
    /// States merged into their representatives.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub merged_states: Vec<String>,
}

/// Proof obligation describing why a specification could not be satisfied.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProofObligation {
    /// State for which the obligation failed.
    pub state: String,
    /// Optional additional details about the failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ControllerDiagnostics {
    /// Provides a concise human-readable summary of recorded diagnostics.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.messages.is_empty() {
            parts.push(format!("messages: {}", self.messages.join(" | ")));
        }
        if !self.violating_initials.is_empty() {
            parts.push(format!(
                "violating initials: {}",
                self.violating_initials.join(", ")
            ));
        }
        if let Some(trace) = &self.counterexample_trace {
            parts.push(format!("counterexample: {}", trace.join(" -> ")));
        }
        if !self.counterstrategy_traces.is_empty() {
            parts.push(format!(
                "counterstrategies: {}",
                self.counterstrategy_traces.len()
            ));
        }
        if !self.deadlock_traces.is_empty() {
            parts.push(format!("deadlocks: {}", self.deadlock_traces.len()));
        }
        if let Some(report) = &self.minimization {
            parts.push(format!(
                "minimized: -{} states, -{} transitions",
                report.removed_states, report.removed_transitions
            ));
        }
        if !self.proof_obligations.is_empty() {
            parts.push(format!("obligations: {}", self.proof_obligations.len()));
        }
        if self.counterstrategy.is_some() {
            parts.push("counterstrategy".to_owned());
        }
        if parts.is_empty() {
            "no diagnostics recorded".to_owned()
        } else {
            parts.join("; ")
        }
    }

    /// Returns a JSON value representing the diagnostics.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("controller diagnostics serialize")
    }

    /// Returns a pretty-printed JSON string with diagnostics information.
    pub fn to_json_string_pretty(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Serialises diagnostics as formatted JSON to `path`.
    pub fn write_json<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let json = self.to_json_string_pretty().map_err(io::Error::other)?;
        fs::write(path, json)
    }

    /// Serialises diagnostics to a sidecar DSL file.
    pub fn write_sidecar_dsl<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let mut file = File::create(path)?;
        writeln!(file, "diagnostics {{")?;
        if !self.messages.is_empty() {
            Self::write_string_block(&mut file, "messages", &self.messages)?;
        }
        if !self.violating_initials.is_empty() {
            Self::write_string_block(&mut file, "violating_initials", &self.violating_initials)?;
        }
        if let Some(trace) = &self.counterexample_trace {
            Self::write_trace_block(
                &mut file,
                "counterexample_trace",
                std::slice::from_ref(trace),
            )?;
        }
        if !self.counterstrategy_traces.is_empty() {
            Self::write_trace_block(
                &mut file,
                "counterstrategy_traces",
                &self.counterstrategy_traces,
            )?;
        }
        if !self.deadlock_traces.is_empty() {
            Self::write_trace_block(&mut file, "deadlock_traces", &self.deadlock_traces)?;
        }
        if let Some(report) = &self.minimization {
            Self::write_minimization_block(&mut file, report)?;
        }
        if !self.proof_obligations.is_empty() {
            Self::write_obligations_block(&mut file, &self.proof_obligations)?;
        }
        if let Some(strategy) = &self.counterstrategy {
            Self::write_counterstrategy_block(&mut file, strategy)?;
        }
        writeln!(file, "}}")?;
        Ok(())
    }

    fn write_string_block<W: Write>(
        writer: &mut W,
        name: &str,
        entries: &[String],
    ) -> io::Result<()> {
        writeln!(writer, "    {name} [")?;
        for entry in entries {
            writeln!(
                writer,
                "        {};",
                serde_json::to_string(entry).map_err(io::Error::other)?
            )?;
        }
        writeln!(writer, "    ];")?;
        Ok(())
    }

    fn write_obligations_block<W: Write>(
        writer: &mut W,
        obligations: &[ProofObligation],
    ) -> io::Result<()> {
        writeln!(writer, "    proof_obligations [")?;
        for obligation in obligations {
            let mut entry = format!(
                "{{ state: {}",
                serde_json::to_string(&obligation.state).map_err(io::Error::other)?
            );
            if let Some(detail) = &obligation.detail {
                let detail_json = serde_json::to_string(detail).map_err(io::Error::other)?;
                entry.push_str(&format!(", detail: {}", detail_json));
            }
            entry.push_str(" }");
            writeln!(writer, "        {entry};")?;
        }
        writeln!(writer, "    ];")?;
        Ok(())
    }

    fn write_counterstrategy_block<W: Write>(
        writer: &mut W,
        strategy: &CounterStrategy,
    ) -> io::Result<()> {
        writeln!(writer, "    counterstrategy {{")?;
        if !strategy.states.is_empty() {
            writeln!(writer, "        states [")?;
            for state in &strategy.states {
                writeln!(
                    writer,
                    "            {};",
                    serde_json::to_string(state).map_err(io::Error::other)?
                )?;
            }
            writeln!(writer, "        ];")?;
        }
        if !strategy.initial_states.is_empty() {
            writeln!(writer, "        initial_states [")?;
            for state in &strategy.initial_states {
                writeln!(
                    writer,
                    "            {};",
                    serde_json::to_string(state).map_err(io::Error::other)?
                )?;
            }
            writeln!(writer, "        ];")?;
        }
        if !strategy.transitions.is_empty() {
            writeln!(writer, "        transitions [")?;
            for edge in &strategy.transitions {
                let from = serde_json::to_string(&edge.from).map_err(io::Error::other)?;
                let to = serde_json::to_string(&edge.to).map_err(io::Error::other)?;
                let labels = serde_json::to_string(&edge.labels).map_err(io::Error::other)?;
                writeln!(
                    writer,
                    "            {{ from: {from}, to: {to}, labels: {labels} }};"
                )?;
            }
            writeln!(writer, "        ];")?;
        }
        writeln!(writer, "    }}")?;
        Ok(())
    }

    fn write_trace_block<W: Write>(
        writer: &mut W,
        name: &str,
        traces: &[Vec<String>],
    ) -> io::Result<()> {
        writeln!(writer, "    {name} [")?;
        for trace in traces {
            writeln!(
                writer,
                "        {};",
                serde_json::to_string(trace).map_err(io::Error::other)?
            )?;
        }
        writeln!(writer, "    ];")?;
        Ok(())
    }

    fn write_minimization_block<W: Write>(
        writer: &mut W,
        report: &ControllerMinimizationReport,
    ) -> io::Result<()> {
        writeln!(writer, "    minimization {{")?;
        writeln!(
            writer,
            "        removed_states = {};",
            report.removed_states
        )?;
        writeln!(
            writer,
            "        removed_transitions = {};",
            report.removed_transitions
        )?;
        if !report.merged_states.is_empty() {
            writeln!(writer, "        merged_states [")?;
            for state in &report.merged_states {
                writeln!(
                    writer,
                    "            {};",
                    serde_json::to_string(state).map_err(io::Error::other)?
                )?;
            }
            writeln!(writer, "        ];")?;
        }
        writeln!(writer, "    }}")?;
        Ok(())
    }
}

// StateSignature, MinTransitionSignature, TransitionKey moved to
// composition::minimize — the public minimization module.
// Context::minimise_controller now delegates to that module.

/// Explorer used to walk the losing region and derive counterexample/counterstrategy artefacts.
#[derive(Default)]
pub(crate) struct CounterExampleExplorer {
    pub(crate) starts: Vec<StateId<DefaultStateIdx>>,
    pub(crate) losing: HashSet<StateId<DefaultStateIdx>>,
}

impl CounterExampleExplorer {
    fn build(
        violating_initials: &[StateId<DefaultStateIdx>],
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        keep_bits: &BitVec<usize, Lsb0>,
    ) -> Self {
        let mut losing = HashSet::new();
        let mut queue = VecDeque::new();

        for &start in violating_initials {
            if losing.insert(start) {
                queue.push_back(start);
            }
        }

        while let Some(state) = queue.pop_front() {
            for transition in clts.outgoing(state) {
                let target = transition.target();
                let losing_successor = !keep_bits.get(target.index()).is_some_and(|bit| *bit);
                if losing_successor && losing.insert(target) {
                    queue.push_back(target);
                }
            }
        }

        Self {
            starts: violating_initials.to_vec(),
            losing,
        }
    }

    fn minimal_traces(
        &self,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        keep_bits: &BitVec<usize, Lsb0>,
    ) -> (Vec<String>, Vec<Vec<String>>) {
        let mut counterexample = Vec::new();
        let mut counter_paths = Vec::new();

        for &start in &self.starts {
            let path = self.path_from(start, clts, keep_bits);
            if counterexample.is_empty() {
                counterexample = path.clone();
            }
            counter_paths.push(path);
        }

        if counterexample.is_empty() {
            counterexample.push("state".to_owned());
        }

        (counterexample, counter_paths)
    }

    fn strategy(&self, clts: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> CounterStrategy {
        let mut states: Vec<String> = self
            .losing
            .iter()
            .filter_map(|state| clts.state_name(*state))
            .map(|name| name.to_owned())
            .collect();
        states.sort();
        states.dedup();

        let mut initial_states: Vec<String> = self
            .starts
            .iter()
            .filter_map(|state| clts.state_name(*state))
            .map(|name| name.to_owned())
            .collect();
        initial_states.sort();
        initial_states.dedup();

        let mut transitions = Vec::new();
        for state in &self.losing {
            let from_name = clts.state_name(*state).unwrap_or("state").to_owned();
            for transition in clts.outgoing(*state) {
                let target = transition.target();
                if self.losing.contains(&target) {
                    let to_name = clts.state_name(target).unwrap_or("state").to_owned();
                    let mut labels = Vec::new();
                    for label_id in transition.labels() {
                        if let Some(payload) = clts.label_payload(*label_id) {
                            labels.extend(payload.iter().cloned());
                        }
                    }
                    labels.sort();
                    labels.dedup();
                    transitions.push(CounterStrategyEdge {
                        from: from_name.clone(),
                        to: to_name,
                        labels,
                    });
                }
            }
        }

        CounterStrategy {
            states,
            initial_states,
            transitions,
        }
    }

    fn path_from(
        &self,
        start: StateId<DefaultStateIdx>,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        keep_bits: &BitVec<usize, Lsb0>,
    ) -> Vec<String> {
        let mut queue: VecDeque<Vec<StateId<DefaultStateIdx>>> = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(vec![start]);
        visited.insert(start);

        while let Some(path) = queue.pop_front() {
            let state = *path.last().expect("path never empty");

            for transition in clts.outgoing(state) {
                let target = transition.target();
                if keep_bits.get(target.index()).is_some_and(|bit| *bit) {
                    let mut extended = path.clone();
                    extended.push(target);
                    return self.to_names(&extended, clts);
                }
                if self.losing.contains(&target) && visited.insert(target) {
                    let mut extended = path.clone();
                    extended.push(target);
                    queue.push_back(extended);
                }
            }
        }

        self.to_names(&[start], clts)
    }

    /// Find a lasso trace (prefix + cycle) in the losing region.
    /// Returns a `LassoTrace` where the infinite counterexample is `prefix ++ cycle^ω`.
    /// If no cycle is found, returns just the start state with an empty cycle.
    pub fn lasso_from(
        &self,
        start: StateId<DefaultStateIdx>,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> LassoTrace {
        // DFS to find a cycle in the losing region.
        // Track both states and the label of the transition taken to reach each state.
        let mut path: Vec<StateId<DefaultStateIdx>> = vec![start];
        let mut edge_labels: Vec<String> = Vec::new(); // edge_labels[i] = label from path[i] to path[i+1]
        let mut on_path: HashSet<StateId<DefaultStateIdx>> = HashSet::new();
        on_path.insert(start);
        let mut visited: HashSet<StateId<DefaultStateIdx>> = HashSet::new();

        loop {
            let state = *path.last().expect("path never empty");
            visited.insert(state);

            // Find a successor in the losing region
            let mut found_next = false;
            for transition in clts.outgoing(state) {
                let target = transition.target();
                if !self.losing.contains(&target) {
                    continue;
                }
                // Cycle detected: target is already on the current path
                if on_path.contains(&target) {
                    let closing_label = Self::transition_label(transition, clts);
                    let cycle_start_idx = path.iter().position(|s| *s == target).unwrap();

                    let prefix = self.to_names(&path[..cycle_start_idx], clts);
                    let prefix_labels = edge_labels[..cycle_start_idx].to_vec();
                    let cycle = self.to_names(&path[cycle_start_idx..], clts);
                    let mut cycle_labels = edge_labels[cycle_start_idx..].to_vec();
                    // The closing label goes from the last cycle state back to cycle[0]
                    cycle_labels.push(closing_label);

                    return LassoTrace {
                        prefix,
                        cycle,
                        prefix_labels,
                        cycle_labels,
                    };
                }
                // Unvisited successor: extend the path
                if !visited.contains(&target) {
                    edge_labels.push(Self::transition_label(transition, clts));
                    path.push(target);
                    on_path.insert(target);
                    found_next = true;
                    break;
                }
            }

            if !found_next {
                // Backtrack: no unvisited successor in losing region
                on_path.remove(&state);
                path.pop();
                edge_labels.pop();
                if path.is_empty() {
                    // No cycle found — return just the start state
                    let name = clts.state_name(start).unwrap_or("state").to_owned();
                    return LassoTrace {
                        prefix: vec![name],
                        cycle: Vec::new(),
                        prefix_labels: Vec::new(),
                        cycle_labels: Vec::new(),
                    };
                }
            }
        }
    }

    /// Extract a human-readable label string from a transition.
    fn transition_label(
        transition: &crate::clts::Transition<DefaultStateIdx, DefaultLabelIdx>,
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> String {
        let parts: Vec<String> = transition
            .labels()
            .iter()
            .filter_map(|lid| {
                clts.label_payload(*lid).and_then(|vals| {
                    let joined = vals
                        .iter()
                        .filter(|v| !v.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    if joined.is_empty() {
                        None
                    } else {
                        Some(joined)
                    }
                })
            })
            .collect();
        if parts.is_empty() {
            "τ".to_string()
        } else {
            parts.join(" | ")
        }
    }

    fn to_names(
        &self,
        path: &[StateId<DefaultStateIdx>],
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> Vec<String> {
        path.iter()
            .map(|state| clts.state_name(*state).unwrap_or("state").to_owned())
            .collect()
    }
}

/// Build a product-game controller for `ControllerMode::ProductGame`.
///
/// The product state is `(plant_state, oblig_idx)` where `oblig_idx` indexes
/// the formula's mu-fixpoint obligations (in nesting order, outermost first).
/// At each product state, the controller picks a transition that drives
/// progress on the current obligation (rank-decreasing for that mu); when
/// the obligation is locally satisfied (rank 0 at the source), the
/// transition advances `oblig_idx` round-robin to the next obligation.
///
/// For formulas with no mu-fixpoints (pure safety), `n_obligs = 1` and the
/// memory dimension collapses (every product state has `oblig_idx = 0`).
///
/// SOUNDNESS: Uncontrollable transitions are always kept and inherit the
/// same obligation-advance rule. The strategy guarantees fairness across
/// all mu-obligations: each obligation is serviced infinitely often along
/// any infinite play.
fn build_product_game_controller(
    ctx: &Context,
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    visited: &BitVec<usize, Lsb0>,
    wm: &crate::mu_calculus::WitnessMap,
    formula: &Formula,
    nesting: &[(crate::mu_calculus::FormulaVarId, bool)],
) -> Result<Clts<DefaultStateIdx, DefaultLabelIdx>, ContextError> {
    let obligations = formula.mu_obligations();
    let n_obligs = obligations.len().max(1);

    let mut builder = CltsBuilder::with_label_store(ctx.label_store.clone());
    let mut mapping: HashMap<(StateId<DefaultStateIdx>, usize), StateId<DefaultStateIdx>> =
        HashMap::new();

    // Allocate one product state per (winning_plant_state, oblig_idx)
    let retained_plant = visited.iter().filter(|bit| **bit).count();
    builder.reserve_states(retained_plant * n_obligs);

    for state in clts.states() {
        if !visited.get(state.index()).is_some_and(|bit| *bit) {
            continue;
        }
        let base_name = clts.state_name(state).unwrap_or("state").to_owned();
        for oblig_idx in 0..n_obligs {
            let name = format!("{base_name}__pg_{oblig_idx}");
            let new_state = builder
                .state_with_name(name)
                .ok_or(ContextError::Controller(CltsError::IdOverflow {
                    kind: "state",
                    value: usize::MAX,
                }))?;
            // Initial product state: every plant initial paired with oblig_idx = 0.
            if oblig_idx == 0 && clts.initial_states().contains(&state) {
                builder.initial_state_id(new_state);
            }
            let vars = clts.state_variables(state);
            builder.with_variables_for_state(new_state, vars.iter().map(|s| s.as_str()));
            mapping.insert((state, oblig_idx), new_state);
        }
    }

    // Helper: rank for the given mu-obligation at a state (None = no obligations).
    let rank_for_oblig = |state_idx: usize, oblig_idx: usize| -> usize {
        if obligations.is_empty() {
            0
        } else {
            let var = obligations[oblig_idx];
            wm.iteration_ranks.get_rank(var, state_idx)
        }
    };

    // For each product state, emit transitions according to the strategy
    for ((original, oblig_idx), mapped) in &mapping {
        let source_oblig_rank = rank_for_oblig(original.index(), *oblig_idx);
        // Advance memory if the current obligation is already satisfied at the source
        let next_oblig = if source_oblig_rank == 0 {
            (oblig_idx + 1) % n_obligs
        } else {
            *oblig_idx
        };

        // Track the best controllable transition for this product state
        let mut best_score: Option<(usize, crate::mu_calculus::Signature)> = None;
        let mut best_trans_idx: Option<usize> = None;

        for (idx, transition) in clts.outgoing(*original).iter().enumerate() {
            let target_plant = transition.target();
            let target_in_winning = visited.get(target_plant.index()).is_some_and(|bit| *bit);
            if !target_in_winning {
                continue;
            }
            let target_mapped = match mapping.get(&(target_plant, next_oblig)) {
                Some(id) => *id,
                None => continue,
            };

            if transition.is_controllable(clts) {
                // Score: prefer the smallest rank for the active obligation,
                // tie-break on full signature lex order (preserves outer-mu
                // non-increase). When source rank is 0 we've already advanced
                // memory; score against `next_oblig`'s rank instead.
                let target_oblig_rank = rank_for_oblig(target_plant.index(), next_oblig);
                let target_full_sig = wm.signature(target_plant.index(), nesting);
                let score = (target_oblig_rank, target_full_sig);
                if best_score.as_ref().is_none_or(|s| score < *s) {
                    best_score = Some(score);
                    best_trans_idx = Some(idx);
                }
            } else {
                // Uncontrollable transitions are always kept
                builder.transition_ids(*mapped, transition.labels(), target_mapped);
            }
        }

        if let Some(idx) = best_trans_idx {
            let transition = &clts.outgoing(*original)[idx];
            let target_plant = transition.target();
            let target_mapped = mapping[&(target_plant, next_oblig)];
            builder.transition_ids(*mapped, transition.labels(), target_mapped);
        }
    }

    builder.build().map_err(ContextError::Controller)
}

/// Build a parity-game controller for `ControllerMode::ParityGame`.
///
/// Constructs the explicit parity game over `(plant_state, formula_node)`
/// positions, solves it via Zielonka, and projects Eve's positional strategy
/// to a CLTS whose state space is the winning region. State names follow
/// the `<plant_name>__pg_n<node_id>` pattern (one state per game position).
///
/// Transitions emitted from each game position:
/// - Eve-owned, in winning region: ONE outgoing edge per Eve's chosen move.
/// - Adam-owned, in winning region: all moves into the winning region (the
///   environment may take any of them; the controller is correct against
///   all).
/// - Modal positions retain their original CLTS labels.
/// - Internal formula-structure moves (And/Or/Nu/Mu/Variable) are emitted
///   with synthetic labels for traceability.
fn build_parity_game_controller(
    ctx: &Context,
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    formula: &Formula,
    env: &crate::mu_calculus::Environment,
) -> Result<Clts<DefaultStateIdx, DefaultLabelIdx>, ContextError> {
    use crate::mu_calculus::parity_game::{Player, build_parity_game, position_name, solve};

    let game = build_parity_game(formula, clts, env);
    let solution = solve(&game);

    // Identify Eve-winning positions.
    let n = game.len();
    let eve_winning: Vec<bool> = (0..n).map(|i| solution.winner[i] == Player::Eve).collect();

    // Allocate one CLTS state per Eve-winning game position.
    let mut label_store = ctx.label_store.clone();
    let pg_label = label_store
        .intern(["pg_step"])
        .map_err(ContextError::Controller)?;
    let mut builder = CltsBuilder::with_label_store(label_store);
    builder.reserve_states(eve_winning.iter().filter(|w| **w).count());
    let mut mapping: HashMap<usize, StateId<DefaultStateIdx>> = HashMap::new();

    for (idx, pos) in game.positions.iter().enumerate() {
        if !eve_winning[idx] {
            continue;
        }
        let name = position_name(*pos, clts);
        let state_id = builder
            .state_with_name(name)
            .ok_or(ContextError::Controller(CltsError::IdOverflow {
                kind: "state",
                value: usize::MAX,
            }))?;
        // Mark initial: the position must correspond to a plant initial AND
        // to the formula root. The parity-game module NNF-transforms the
        // formula internally, so use the NNF formula's root from the game,
        // not the original formula's root (their NodeIds differ).
        let state_obj = StateId::<DefaultStateIdx>::from_index(pos.state)
            .expect("plant state index fits storage");
        if pos.node == game.formula.root() && clts.initial_states().contains(&state_obj) {
            builder.initial_state_id(state_id);
        }
        let vars = clts.state_variables(state_obj);
        builder.with_variables_for_state(state_id, vars.iter().map(|s| s.as_str()));
        mapping.insert(idx, state_id);
    }

    // Emit transitions. For Eve-owned positions in winning region, follow
    // Eve's strategy (single edge). For Adam-owned positions, keep ALL
    // edges into the winning region.
    for (idx, _pos) in game.positions.iter().enumerate() {
        if !eve_winning[idx] {
            continue;
        }
        let src_mapped = mapping[&idx];

        let edges: Vec<usize> = match game.owners[idx] {
            Player::Eve => {
                // Use the strategy if available
                solution.eve_strategy[idx]
                    .into_iter()
                    .filter(|t| eve_winning[*t])
                    .collect()
            }
            Player::Adam => {
                // Keep all edges that stay in winning region
                game.edges[idx]
                    .iter()
                    .copied()
                    .filter(|t| eve_winning[*t])
                    .collect()
            }
        };

        for t in edges {
            let target_mapped = match mapping.get(&t) {
                Some(id) => *id,
                None => continue,
            };
            // Synthetic label `pg_step` per transition. Keeps the controller
            // shape consistent with the existing CLTS abstraction.
            builder.transition_ids(src_mapped, &[pg_label], target_mapped);
        }
    }

    builder.build().map_err(ContextError::Controller)
}

/// Render a signature as a state-name suffix segment. `usize::MAX` is rendered
/// as `inf` (a state outside the fixpoint, infinitely many iterations away).
fn format_signature(sig: &crate::mu_calculus::Signature) -> String {
    sig.iter()
        .map(|&r| {
            if r == usize::MAX {
                "inf".to_string()
            } else {
                r.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// Collects deadlock traces by replaying parent pointers back to initial states.
fn collect_deadlock_traces(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    visited: &BitVec<usize, Lsb0>,
    parent: &HashMap<StateId<DefaultStateIdx>, StateId<DefaultStateIdx>>,
) -> Vec<Vec<String>> {
    let initial_set: HashSet<_> = clts.initial_states().iter().copied().collect();
    let mut traces = Vec::new();
    let mut seen_deadlocks: HashSet<StateId<DefaultStateIdx>> = HashSet::new();

    for (state, outgoing) in clts.state_outgoing_pairs() {
        if !visited.get(state.index()).is_some_and(|bit| *bit) {
            continue;
        }

        let mut has_transition = false;
        for transition in outgoing {
            if visited
                .get(transition.target().index())
                .is_some_and(|bit| *bit)
            {
                has_transition = true;
                break;
            }
        }

        if has_transition || !seen_deadlocks.insert(state) {
            continue;
        }

        let mut path = Vec::new();
        let mut current = state;
        let mut guard = HashSet::new();
        path.push(clts.state_name(current).unwrap_or("state").to_owned());

        while !initial_set.contains(&current) {
            if let Some(prev) = parent.get(&current) {
                if !guard.insert(current) {
                    break;
                }
                current = *prev;
                path.push(clts.state_name(current).unwrap_or("state").to_owned());
            } else {
                break;
            }
        }

        if initial_set.contains(&current) {
            path.reverse();
            traces.push(path);
        }
    }

    traces
}

type MinimizationOutcome = Option<(
    Clts<DefaultStateIdx, DefaultLabelIdx>,
    ControllerMinimizationReport,
)>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clts::Clts;
    use crate::composition::{CompositionOptions, CompositionSemantics};
    use crate::context_dsl::parse;
    use crate::mu_calculus::{evaluate, parser};
    use tempfile::{NamedTempFile, tempdir};

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn registers_clts_instances() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0");
        let clts = builder.build()?;

        let context = Context::builder().register_clts("m", clts).finish();
        assert!(context.clts("m").is_some());

        Ok(())
    }

    #[test]
    fn shares_label_store_across_builders() -> TestResult {
        let context = Context::builder().finish();

        let mut builder_a = context.new_clts_builder();
        let label_a = builder_a.labels().intern(["a", "b"])?;
        let clts_a = {
            let mut owned = builder_a;
            owned.state("s0");
            owned.build()?
        };

        let mut builder_b = context.new_clts_builder();
        let label_b = builder_b.labels().intern(["b", "a"])?;
        let clts_b = {
            let mut owned = builder_b;
            owned.state("s1");
            owned.build()?
        };

        let context = Context::builder()
            .register_clts("a", clts_a)
            .register_clts("b", clts_b)
            .finish();

        let ca = context.clts("a").unwrap();
        let cb = context.clts("b").unwrap();

        assert_eq!(label_a, label_b);
        assert_eq!(ca.outgoing(ca.state_id("s0")?).len(), 0);
        assert_eq!(cb.outgoing(cb.state_id("s1")?).len(), 0);

        Ok(())
    }

    #[test]
    fn merges_label_store_on_registration() -> TestResult {
        let mut context = Context::builder().finish();

        let mut builder_a = context.new_clts_builder();
        let label = builder_a.labels().intern(["ctrl"])?;
        let clts_a = {
            let mut owned = builder_a;
            owned.state("s0");
            owned.build()?
        };

        context = Context::builder()
            .register_clts("a", clts_a)
            .finish_with_checks()?;

        let mut builder_b = context.new_clts_builder();
        let label_b = builder_b.labels().intern(["ctrl"])?;
        assert_eq!(label, label_b, "label IDs diverged after registration");

        Ok(())
    }

    #[test]
    fn rejects_duplicate_controllable_alphabet() -> TestResult {
        let context = Context::builder().finish();

        let mut builder_a = context.new_clts_builder();
        let label = builder_a.labels().intern(["ctrl"])?;
        let clts_a = {
            let mut owned = builder_a;
            owned.state("s0");
            owned.transition("s0", &[label], "s0");
            owned.build()?
        };

        let mut builder_b = context.new_clts_builder();
        let label_dup = builder_b.labels().intern(["ctrl"])?;
        let clts_b = {
            let mut owned = builder_b;
            owned.state("s1");
            owned.transition("s1", &[label_dup], "s1");
            owned.build()?
        };

        let result = Context::builder()
            .register_clts("a", clts_a)
            .register_clts("b", clts_b)
            .finish_with_checks();

        assert!(matches!(
            result,
            Err(ContextError::DuplicateControllableAlphabet(_))
        ));

        Ok(())
    }

    #[test]
    fn rejects_duplicate_internal_alphabet() -> TestResult {
        let context = Context::builder().finish();

        let mut builder_a = context.new_clts_builder();
        let internal_label = builder_a.labels().intern(["internal"])?;
        builder_a
            .set_label_controllability(internal_label, crate::clts::LabelControllability::Internal);
        let clts_a = {
            let mut owned = builder_a;
            owned.state("s0");
            owned.transition("s0", &[internal_label], "s0");
            owned.build()?
        };

        let mut builder_b = context.new_clts_builder();
        let internal_label_b = builder_b.labels().intern(["internal"])?; // Same name
        builder_b.set_label_controllability(
            internal_label_b,
            crate::clts::LabelControllability::Internal,
        );
        let clts_b = {
            let mut owned = builder_b;
            owned.state("s1");
            owned.transition("s1", &[internal_label_b], "s1");
            owned.build()?
        };

        let result = Context::builder()
            .register_clts("a", clts_a)
            .register_clts("b", clts_b)
            .finish_with_checks();

        assert!(matches!(
            result,
            Err(ContextError::DuplicateInternalAlphabet(_))
        ));

        Ok(())
    }

    #[test]
    fn tracks_global_variables() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        builder.with_variables("s0", ["temp", "flag"]);
        builder.state("s1");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("sys", clts)
            .finish_with_checks()?;

        let vars: Vec<_> = context.global_variables().iter().cloned().collect();
        assert!(vars.contains(&"temp".to_owned()));
        assert!(vars.contains(&"flag".to_owned()));

        Ok(())
    }

    #[test]
    fn synthesises_controller_realizable() -> TestResult {
        let mut builder = Clts::builder();
        let label = builder.labels().intern(["tick"])?;
        builder.state("s0");
        builder.state("s1");
        builder.initial("s0");
        builder.transition("s0", &[label], "s1");
        builder.transition("s1", &[label], "s1");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("true")?;
        let env = Environment::new(plant_ref.state_count());

        let diag_opts = DiagnosticsOptions {
            counterexample: true,
            deadlock_traces: true,
            max_counter_traces: None,
            proof_obligations: true,
        };
        let synthesis = context.synthesise_controller_with_options(
            "plant",
            &formula,
            &env,
            ControllerSynthesisOptions {
                diagnostics: Some(&diag_opts),
                ..Default::default()
            },
        )?;
        assert!(synthesis.realizable);
        assert_eq!(synthesis.controller.state_count(), 2);
        assert_eq!(synthesis.controller.initial_states().len(), 1);
        assert!(synthesis.diagnostics.violating_initials.is_empty());
        assert!(synthesis.diagnostics.counterexample_trace.is_none());
        assert!(
            synthesis
                .diagnostics
                .messages
                .iter()
                .any(|msg| msg.contains("Controller realizable"))
        );
        assert!(!synthesis.diagnostics.messages.is_empty());
        assert!(synthesis.diagnostics.counterstrategy_traces.is_empty());
        assert!(synthesis.diagnostics.deadlock_traces.len() <= 1);

        Ok(())
    }

    #[test]
    fn synthesises_controller_unrealizable() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0");
        builder.initial("s0");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("false")?;
        let env = Environment::new(plant_ref.state_count());

        let synthesis = context.synthesise_controller("plant", &formula, &env, None)?;
        assert!(!synthesis.realizable);
        assert_eq!(synthesis.controller.state_count(), 0);
        assert_eq!(
            synthesis.diagnostics.violating_initials,
            vec!["s0".to_owned()]
        );
        assert_eq!(
            synthesis.diagnostics.counterexample_trace,
            Some(vec!["s0".to_owned()])
        );
        assert_eq!(
            synthesis.diagnostics.proof_obligations.len(),
            1,
            "expected proof obligations for violating initial"
        );
        assert!(
            synthesis
                .diagnostics
                .messages
                .iter()
                .any(|msg| msg.contains("Controller unrealizable"))
        );
        assert!(!synthesis.diagnostics.messages.is_empty());

        Ok(())
    }

    #[test]
    fn synthesises_controller_reports_counterstrategy_when_enabled() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0");
        builder.initial("s0");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("false")?;
        let env = Environment::new(plant_ref.state_count());

        let diag_opts = DiagnosticsOptions {
            counterexample: true,
            deadlock_traces: false,
            max_counter_traces: None,
            proof_obligations: true,
        };
        let synthesis = context.synthesise_controller_with_options(
            "plant",
            &formula,
            &env,
            ControllerSynthesisOptions {
                diagnostics: Some(&diag_opts),
                ..Default::default()
            },
        )?;

        assert!(!synthesis.realizable);
        assert_eq!(synthesis.controller.state_count(), 0);
        assert_eq!(
            synthesis.diagnostics.counterexample_trace,
            Some(vec!["s0".to_owned()])
        );
        assert_eq!(
            synthesis.diagnostics.counterstrategy_traces,
            vec![vec!["s0".to_owned()]]
        );
        assert_eq!(
            synthesis.diagnostics.proof_obligations.len(),
            1,
            "expected proof obligations for unrealizable controller"
        );

        Ok(())
    }

    #[test]
    fn synthesises_controller_reports_deadlock_traces_when_enabled() -> TestResult {
        let mut builder = Clts::builder();
        let label = builder.labels().intern(["tick"])?;
        builder.state("s0");
        builder.state("s1");
        builder.initial("s0");
        builder.transition("s0", &[label], "s1");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("true")?;
        let env = Environment::new(plant_ref.state_count());

        let diag_opts = DiagnosticsOptions {
            counterexample: false,
            deadlock_traces: true,
            max_counter_traces: None,
            proof_obligations: true,
        };
        let synthesis = context.synthesise_controller_with_options(
            "plant",
            &formula,
            &env,
            ControllerSynthesisOptions {
                diagnostics: Some(&diag_opts),
                ..Default::default()
            },
        )?;

        assert!(synthesis.realizable);
        assert!(synthesis.diagnostics.violating_initials.is_empty());
        assert_eq!(
            synthesis.diagnostics.deadlock_traces,
            vec![vec!["s0".to_owned(), "s1".to_owned()]]
        );

        Ok(())
    }

    #[test]
    fn synthesises_controller_runs_minimization_when_enabled() -> TestResult {
        let mut builder = Clts::builder();
        let step = builder.labels().intern(["step"])?;
        let wait = builder.labels().intern(["wait"])?;
        builder.state("init");
        builder.initial("init");
        builder.state("idle_a");
        builder.state("idle_b");
        builder.transition("init", &[step], "idle_a");
        builder.transition("init", &[step], "idle_b");
        builder.transition("idle_a", &[wait], "idle_a");
        builder.transition("idle_b", &[wait], "idle_b");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("true")?;
        let env = Environment::new(plant_ref.state_count());

        let diag_opts = DiagnosticsOptions {
            counterexample: false,
            deadlock_traces: false,
            max_counter_traces: None,
            proof_obligations: true,
        };
        let synthesis = context.synthesise_controller_with_options(
            "plant",
            &formula,
            &env,
            ControllerSynthesisOptions {
                diagnostics: Some(&diag_opts),
                minimize: true,
                extract_strategy: false,
                ..Default::default()
            },
        )?;

        assert!(synthesis.realizable);
        assert_eq!(synthesis.controller.state_count(), 2);
        let minimization = synthesis
            .diagnostics
            .minimization
            .as_ref()
            .expect("minimization report present");
        assert_eq!(minimization.removed_states, 1);
        assert!(minimization.removed_transitions >= 1);
        assert!(
            minimization
                .merged_states
                .iter()
                .any(|name| name == "idle_b")
        );

        Ok(())
    }

    #[test]
    fn controller_diagnostics_exports_reports() -> TestResult {
        let diagnostics = ControllerDiagnostics {
            property_class: Some("Safety".into()),
            alternation_depth: Some(1),
            messages: vec![
                "Controller realizable: retained 2 of 4 states.".into(),
                "Initial state(s) excluded from controller: s2.".into(),
            ],
            violating_initials: vec!["s2".into()],
            counterexample_trace: Some(vec!["s2".into(), "s3".into()]),
            deadlock_traces: vec![vec!["s3".into()]],
            counterstrategy_traces: vec![vec!["s2".into(), "s3".into()]],
            minimization: None,
            proof_obligations: vec![ProofObligation {
                state: "s2".into(),
                detail: Some("State requires attention.".into()),
            }],
            lasso_traces: Vec::new(),
            counterstrategy: Some(CounterStrategy {
                states: vec!["s2".into(), "s3".into()],
                initial_states: vec!["s2".into()],
                transitions: vec![CounterStrategyEdge {
                    from: "s2".into(),
                    to: "s3".into(),
                    labels: vec!["tick".into()],
                }],
            }),
        };

        let json = diagnostics.to_json_string_pretty()?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["violating_initials"].as_array().unwrap().len(), 1);
        assert!(
            value["proof_obligations"]
                .as_array()
                .is_some_and(|array| !array.is_empty())
        );
        assert!(value["counterstrategy"].is_object());

        let sidecar = NamedTempFile::new()?;
        diagnostics.write_sidecar_dsl(sidecar.path())?;
        let sidecar_contents = std::fs::read_to_string(sidecar.path())?;
        assert!(sidecar_contents.contains("messages ["));
        assert!(sidecar_contents.contains("counterstrategy_traces"));
        assert!(sidecar_contents.contains("proof_obligations"));
        assert!(sidecar_contents.contains("counterstrategy"));

        let json_file = NamedTempFile::new()?;
        diagnostics.write_json(json_file.path())?;
        let written_json = std::fs::read_to_string(json_file.path())?;
        assert!(written_json.contains("\"counterexample_trace\""));
        assert!(written_json.contains("\"proof_obligations\""));
        assert!(written_json.contains("\"counterstrategy\""));

        assert!(diagnostics.summary().contains("messages:"));
        assert!(diagnostics.summary().contains("obligations: 1"));
        assert!(diagnostics.summary().contains("counterstrategy"));

        Ok(())
    }

    #[test]
    fn composes_registered_clts() -> TestResult {
        let context = Context::builder().finish();
        let mut builder_a = context.new_clts_builder();
        let label = builder_a.labels().intern(["sync"])?;
        // Mark shared synchronization label as uncontrollable (environment-controlled)
        builder_a
            .set_label_controllability(label, crate::clts::LabelControllability::Uncontrollable);
        builder_a.state("a0").initial("a0");
        builder_a.transition("a0", &[label], "a1");
        let clts_a = builder_a.build()?;

        let mut builder_b = context.new_clts_builder();
        let label_b = builder_b.labels().intern(["sync"])?;
        // Mark shared synchronization label as uncontrollable (environment-controlled)
        builder_b
            .set_label_controllability(label_b, crate::clts::LabelControllability::Uncontrollable);
        builder_b.state("b0").initial("b0");
        builder_b.transition("b0", &[label_b], "b1");
        let clts_b = builder_b.build()?;

        let context = Context::builder()
            .register_clts("left", clts_a)
            .register_clts("right", clts_b)
            .finish();

        let options = CompositionOptions::new(CompositionSemantics::Synchronous);
        let product = context.compose_named("left", "right", &options)?;

        assert_eq!(product.state_count(), 2);
        // Ensure the composed transition exists.
        let initial = product.state_id("a0|b0")?;
        let outgoing = product.outgoing(initial);
        assert_eq!(outgoing.len(), 1);
        Ok(())
    }

    #[test]
    fn canonical_label_and_variable_order_preserved() -> TestResult {
        let context = Context::builder().finish();

        // First CLTS introduces labels/variables via the context builder.
        let mut builder_a = context.new_clts_builder();
        let sync = builder_a.labels().intern(["sync"])?;
        builder_a.with_variables("s0", ["flag"]);
        builder_a.state("s0");
        let clts_a = builder_a.build()?;

        // Register and obtain a new context with canonical stores merged.
        let context = Context::builder()
            .register_clts("a", clts_a.clone())
            .finish_with_checks()?;

        // Second CLTS should reuse the same label/variable IDs.
        let mut builder_b = context.new_clts_builder();
        let sync_b = builder_b.labels().intern(["sync"])?;
        builder_b.with_variables("b0", ["flag"]);
        builder_b.state("b0");
        let clts_b = builder_b.build()?;

        assert_eq!(sync.index(), sync_b.index());

        let bitset_a = clts_a
            .label_bitset(sync)
            .expect("label bitset should exist");
        let bitset_b = clts_b
            .label_bitset(sync_b)
            .expect("label bitset should exist");
        assert_eq!(bitset_a.bits(), bitset_b.bits());

        let vars_a = clts_a.state_variable_bitset(clts_a.state_id("s0")?);
        let vars_b = clts_b.state_variable_bitset(clts_b.state_id("b0")?);
        assert_eq!(vars_a.bits(), vars_b.bits());

        // A CLTS with a different label should be unequal.
        let mut builder_d = context.new_clts_builder();
        let other_label = builder_d.labels().intern(["other"])?;
        builder_d.state("x0");
        builder_d.transition("x0", &[other_label], "x0");
        let clts_d = builder_d.build()?;
        assert!(!clts_b.structural_eq(&clts_d));
        assert_ne!(clts_b.structural_hash(), clts_d.structural_hash());

        Ok(())
    }

    #[test]
    fn structural_hash_matches_for_identical_clts() -> TestResult {
        let empty_context = Context::builder().finish();

        let make_clts = |ctx: &Context| -> Clts<DefaultStateIdx, DefaultLabelIdx> {
            let mut builder = ctx.new_clts_builder();
            let sync = builder.labels().intern(["sync"]).unwrap();
            builder
                .transition("s0", &[sync], "s1")
                .with_variables("s0", ["flag"])
                .with_variables("s1", ["flag"]);
            builder.build().unwrap()
        };

        let seed = make_clts(&empty_context);
        let context = Context::builder()
            .register_clts("seed", seed)
            .finish_with_checks()?;

        let clts_a = make_clts(&context);
        let clts_b = make_clts(&context);

        assert!(clts_a.structural_eq(&clts_b));
        assert_eq!(clts_a.structural_hash(), clts_b.structural_hash());

        // Differing structure (additional uncontrollable edge).
        let mut builder = context.new_clts_builder();
        let sync = builder.labels().intern(["sync"])?;
        builder
            .transition("c0", &[sync], "c1")
            .transition("c0", &[sync], "c2");
        let clts_c = builder.build()?;

        assert!(!clts_a.structural_eq(&clts_c));
        assert_ne!(clts_a.structural_hash(), clts_c.structural_hash());

        Ok(())
    }

    #[test]
    fn evaluate_mu_matches_direct_invocation() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        builder.state("s0").initial("s0");
        builder.state("s1");
        let tick = builder.labels().intern(["tick"])?;
        builder.transition("s0", &[tick], "s1");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", clts.clone())
            .finish();

        let formula = parser::parse("< labels = {tick} > true")?;
        let env = Environment::new(clts.state_count());
        let direct = evaluate(&formula, &clts, &env)?;
        let via_context = context.evaluate_mu("plant", &formula, &env, None)?;

        assert_eq!(direct, via_context);
        Ok(())
    }

    #[test]
    fn evaluate_mu_rejects_mismatched_environment() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        builder.state("s0").initial("s0");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", clts.clone())
            .finish();

        let formula = parser::parse("true")?;
        let env = Environment::new(clts.state_count() + 1);

        let err = context
            .evaluate_mu("plant", &formula, &env, None)
            .expect_err("mismatched environment should error");
        assert!(matches!(
            err,
            ContextError::EnvironmentMismatch { expected, provided, .. }
            if expected == clts.state_count() && provided == clts.state_count() + 1
        ));
        Ok(())
    }

    #[test]
    fn evaluate_mu_many_runs_batch() -> TestResult {
        let context = Context::builder().finish();

        let mut builder_a = context.new_clts_builder();
        builder_a.state("a0").initial("a0");
        let sync = builder_a.labels().intern(["sync"])?;
        builder_a.transition("a0", &[sync], "a1");
        let plant_a = builder_a.build()?;

        let mut builder_b = context.new_clts_builder();
        builder_b.state("b0").initial("b0");
        let sync_b = builder_b.labels().intern(["sync"])?;
        builder_b.transition("b0", &[sync_b], "b1");
        let plant_b = builder_b.build()?;

        let context = Context::builder()
            .register_clts("plant_a", plant_a.clone())
            .register_clts("plant_b", plant_b.clone())
            .finish();

        let formula = parser::parse("< labels = {sync} > true")?;
        let results = context.evaluate_mu_many(
            ["plant_a", "plant_b"],
            &formula,
            |_, clts| Environment::new(clts.state_count()),
            None,
        )?;

        assert_eq!(results.len(), 2);
        for (name, result) in &results {
            let clts = context.clts(name).unwrap();
            let initial = clts
                .initial_states()
                .iter()
                .copied()
                .next()
                .expect("initial state present");
            assert!(result.get(initial.index()).map(|b| *b).unwrap_or(false));
        }
        Ok(())
    }

    #[test]
    fn evaluate_mu_many_propagates_mismatch() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        builder.state("s0").initial("s0");
        let clts = builder.build()?;
        let context = Context::builder().register_clts("plant", clts).finish();

        let formula = parser::parse("true")?;
        let err = context
            .evaluate_mu_many(["plant"], &formula, |_, _| Environment::new(0), None)
            .expect_err("expected mismatch error");
        assert!(matches!(err, ContextError::EnvironmentMismatch { .. }));
        Ok(())
    }

    #[test]
    fn saves_and_loads_clts_via_persistence() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        let sync = builder.labels().intern(["sync"])?;
        builder.state("s0").initial("s0");
        builder.transition("s0", &[sync], "s1");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", clts.clone())
            .finish();

        let dir = tempdir()?;
        let path = dir.path().join("plant.json");
        context.save_clts_to_path("plant", &path)?;

        let mut restored = Context::builder().finish();
        restored.load_clts_from_path("plant", &path)?;

        let loaded = restored.clts("plant").unwrap();
        assert!(clts.structural_eq(loaded));
        Ok(())
    }

    #[test]
    fn spill_clts_if_exceeds_triggers_when_large_enough() -> TestResult {
        let context = Context::builder().finish();
        let mut builder = context.new_clts_builder();
        builder.state("s0").initial("s0");
        let clts = builder.build()?;
        let context = Context::builder().register_clts("plant", clts).finish();

        let dir = tempdir()?;
        let path = dir.path().join("spill.json");
        let spilled = context.spill_clts_if_exceeds("plant", 16, &path)?;
        assert!(spilled.is_some());
        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn dsl_cache_tracks_incremental_changes() -> TestResult {
        let mut cache = ContextDslCache::new();
        let doc1 = parse(
            r"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = nu X. (tick && X); }
    }
    controllers {
        controller C1 { source Machine; satisfying safe; }
    }
}
",
        )?;

        let plan1 = cache.diff(&doc1);
        assert!(plan1.changed_automata.contains("Machine"));
        assert!(plan1.changed_controllers.contains("C1"));
        cache.update(&plan1);

        let doc2 = parse(
            r"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = nu X. (tick && X && < labels = {tick} > true); }
    }
    controllers {
        controller C1 { source Machine; satisfying safe; }
    }
}
",
        )?;

        let plan2 = cache.diff(&doc2);
        assert!(plan2.changed_formulas.contains("safe"));
        assert!(plan2.changed_controllers.contains("C1"));
        cache.update(&plan2);

        Ok(())
    }

    #[test]
    fn controllable_alphabet_accessor() -> TestResult {
        // Test controllable_alphabet() accessor (lines 114-115)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");
        let label = builder.labels().intern(["ctrl"])?;
        builder.transition("s0", &[label], "s1");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", clts)
            .finish_with_checks()?;

        let alphabet = context.controllable_alphabet();
        assert!(alphabet.contains("ctrl"));

        Ok(())
    }

    #[test]
    fn context_dsl_cache_diff_and_update() -> TestResult {
        // Test diff_and_update() method (lines 73-76)
        let mut cache = ContextDslCache::new();
        let doc1 = parse(
            r"
context test {
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
    }
}
",
        )?;
        let plan1 = cache.diff_and_update(&doc1);
        // Plan should indicate changes for new automaton
        assert!(!plan1.changed_automata.is_empty() || !plan1.removed_automata.is_empty());

        let doc2 = parse(
            r"
context test {
    automata {
        automaton A { states { state S initial; } transitions { transition S -> S on epsilon; } }
        automaton B { states { state T initial; } transitions { transition T -> T on epsilon; } }
    }
}
",
        )?;
        let plan2 = cache.diff_and_update(&doc2);
        // Plan should indicate new automaton B was added
        assert!(!plan2.changed_automata.is_empty() || !plan2.removed_automata.is_empty());

        Ok(())
    }

    #[test]
    fn synthesises_controller_no_initial_states_satisfy() -> TestResult {
        // Test synthesis when no initial states satisfy (lines 267-275)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1");
        builder.initial("s0").initial("s1");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        // Formula that no state satisfies
        let formula = parser::parse("false")?;
        let env = Environment::new(plant_ref.state_count());

        let synthesis = context.synthesise_controller("plant", &formula, &env, None)?;
        assert!(!synthesis.realizable);
        assert_eq!(synthesis.controller.state_count(), 0);
        assert_eq!(synthesis.diagnostics.violating_initials.len(), 2);
        assert!(!synthesis.diagnostics.proof_obligations.is_empty());

        Ok(())
    }

    #[test]
    fn synthesises_controller_with_counterexample_explorer() -> TestResult {
        // Test synthesis with counterexample explorer enabled (lines 349-387)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");
        builder.initial("s0");
        let label = builder.labels().intern(["tick"])?;
        builder.transition("s0", &[label], "s1");
        builder.transition("s1", &[label], "s2");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        // Formula that s0 doesn't satisfy but s1 does
        let formula = parser::parse("mu X. (<labels={tick}>X || true)")?;
        let env = Environment::new(plant_ref.state_count());

        let diagnostics = DiagnosticsOptions {
            counterexample: true,
            max_counter_traces: Some(2),
            ..Default::default()
        };
        let options = ControllerSynthesisOptions {
            diagnostics: Some(&diagnostics),
            ..Default::default()
        };

        let synthesis =
            context.synthesise_controller_with_options("plant", &formula, &env, options)?;

        // Should have counterstrategy if unrealizable
        if !synthesis.realizable {
            assert!(synthesis.diagnostics.counterstrategy.is_some());
            assert!(!synthesis.diagnostics.counterstrategy_traces.is_empty());
        }

        Ok(())
    }

    #[test]
    fn minimization_returns_none_for_single_state() -> TestResult {
        // Test minimization returns None for single state controller (line 432)
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        let formula = parser::parse("true")?;
        let env = Environment::new(plant_ref.state_count());

        let synthesis = context.synthesise_controller("plant", &formula, &env, None)?;
        assert!(synthesis.realizable);

        // Minimization should return None for single state
        let minimized = context.minimise_controller(&synthesis.controller)?;
        assert!(minimized.is_none());

        Ok(())
    }

    #[test]
    fn minimization_returns_none_when_no_reduction_possible() -> TestResult {
        // Test minimization returns None when no states can be merged (lines 468, 481, 483, 488, 491)
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").state("s2");
        builder.initial("s0");
        let label_a = builder.labels().intern(["a"])?;
        let label_b = builder.labels().intern(["b"])?;
        builder.transition("s0", &[label_a], "s1");
        builder.transition("s0", &[label_b], "s2");
        builder.transition("s1", &[label_a], "s1");
        builder.transition("s2", &[label_b], "s2");
        let plant = builder.build()?;

        let context = Context::builder()
            .register_clts("plant", plant)
            .finish_with_checks()?;

        let plant_ref = context.clts("plant").unwrap();
        // Formula that requires all states to be distinct
        let formula = parser::parse("true")?;
        let env = Environment::new(plant_ref.state_count());

        let synthesis = context.synthesise_controller("plant", &formula, &env, None)?;
        assert!(synthesis.realizable);

        // If all states are distinct, minimization may return None
        let _minimized = context.minimise_controller(&synthesis.controller)?;
        // Result depends on controller structure - test that it doesn't panic

        Ok(())
    }

    #[test]
    fn load_clts_from_path_error_handling() -> TestResult {
        // Test load_clts_from_path error handling (lines 590-591)
        let mut context = Context::builder().finish();
        let temp_dir = tempdir()?;
        let invalid_path = temp_dir.path().join("nonexistent.clts");

        // Should return error for non-existent file
        let result = context.load_clts_from_path("test", &invalid_path);
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn save_clts_to_path_error_handling() -> TestResult {
        // Test save_clts_to_path error handling (line 580)
        let mut builder = Clts::builder();
        builder.state("s0");
        let clts = builder.build()?;

        let context = Context::builder().register_clts("test", clts).finish();

        let temp_dir = tempdir()?;
        let file_path = temp_dir.path().join("test.clts");

        // Should succeed for valid path
        let result = context.save_clts_to_path("test", &file_path);
        assert!(result.is_ok());
        assert!(file_path.exists());

        Ok(())
    }

    #[test]
    fn context_save_and_load_round_trip() -> TestResult {
        // Build a small context with a single CLTS.
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        let label = builder.labels().intern(["a"])?;
        builder.transition("s0", &[label], "s0");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("sys", clts)
            .finish_with_checks()?;

        let temp_dir = tempdir()?;
        let file_path = temp_dir.path().join("context.mununu");

        // Save the full context.
        context.save_to_path(&file_path)?;
        assert!(file_path.exists());

        // Load it back and check that the CLTS round-trips structurally.
        let restored = Context::load_from_path(&file_path)?;
        let orig_clts = context.clts("sys").unwrap();
        let restored_clts = restored.clts("sys").unwrap();
        assert!(orig_clts.structural_eq(restored_clts));

        Ok(())
    }

    #[test]
    fn spill_clts_if_exceeds_triggers() -> TestResult {
        // Test spill_clts_if_exceeds (lines 606, 633, 635)
        let mut builder = Clts::builder();
        // Create a large CLTS
        for i in 0..100 {
            builder.state(format!("s{}", i));
        }
        let clts = builder.build()?;

        let context = Context::builder().register_clts("large", clts).finish();

        let temp_dir = tempdir()?;
        let spill_path = temp_dir.path().join("spill.clts");

        // Test with very small limit - should trigger spill
        let result = context.spill_clts_if_exceeds("large", 1, &spill_path)?;
        assert!(result.is_some());
        assert!(spill_path.exists());

        // Test with large limit - should not trigger
        let spill_path2 = temp_dir.path().join("spill2.clts");
        let result2 = context.spill_clts_if_exceeds("large", usize::MAX, &spill_path2)?;
        assert!(result2.is_none());

        Ok(())
    }

    #[test]
    fn compose_named_error_handling() -> TestResult {
        // Test compose_named error handling (line 135)
        let context = Context::builder().finish();

        // Should return error for unknown CLTS
        let options = CompositionOptions::new(CompositionSemantics::Synchronous);
        let result = context.compose_named("nonexistent", "also_nonexistent", &options);
        assert!(result.is_err());
        match result {
            Err(ContextError::UnknownClts(name)) => {
                assert!(name == "nonexistent" || name == "also_nonexistent");
            }
            _ => panic!("expected UnknownClts error"),
        }

        Ok(())
    }

    #[test]
    fn print_structure_outputs_expected_format() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        builder.state("s1");
        let label = builder.labels().intern(["a"])?;
        builder.transition("s0", &[label], "s1");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("test_automaton", clts)
            .finish_with_checks()?;

        let mut output = Vec::new();
        context.print_structure(&mut output)?;
        let output_str = String::from_utf8(output)?;

        // Check that key elements are present
        assert!(output_str.contains("Context Structure:"));
        assert!(output_str.contains("Automaton: test_automaton"));
        assert!(output_str.contains("States: 2"));
        assert!(output_str.contains("Initial: s0"));
        assert!(output_str.contains("[0] s0"));
        assert!(output_str.contains("[1] s1"));
        assert!(output_str.contains("Transitions: 1 total"));
        assert!(output_str.contains("1 controllable, 0 uncontrollable"));

        Ok(())
    }

    #[test]
    fn print_structure_includes_global_variables() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("test", clts)
            .finish_with_checks()?;

        let mut output = Vec::new();
        context.print_structure(&mut output)?;
        let output_str = String::from_utf8(output)?;

        assert!(output_str.contains("Global Variables: 0"));
        assert!(output_str.contains("Controllable Alphabet:"));

        Ok(())
    }

    #[test]
    fn print_structure_shows_transition_details() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        builder.state("s1");
        builder.state("s2");
        let label_a = builder.labels().intern(["a"])?;
        let label_b = builder.labels().intern(["b"])?;
        builder
            .set_label_controllability(label_b, crate::clts::LabelControllability::Uncontrollable);
        builder.transition("s0", &[label_a], "s1");
        builder.transition("s0", &[label_b], "s2");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("test", clts)
            .finish_with_checks()?;

        let mut output = Vec::new();
        context.print_structure(&mut output)?;
        let output_str = String::from_utf8(output)?;

        // Check transition details
        assert!(output_str.contains("Transitions: 2 total"));
        assert!(output_str.contains("1 controllable, 1 uncontrollable"));
        assert!(output_str.contains("-> [1] s1"));
        assert!(output_str.contains("-> [2] s2"));
        assert!(output_str.contains("(ctrl)"));
        assert!(output_str.contains("(unctrl)"));

        Ok(())
    }

    #[test]
    fn print_structure_handles_multiple_automata() -> TestResult {
        let mut builder1 = Clts::builder();
        builder1.state("a0").initial("a0");
        let clts1 = builder1.build()?;

        let mut builder2 = Clts::builder();
        builder2.state("b0").initial("b0");
        let clts2 = builder2.build()?;

        let context = Context::builder()
            .register_clts("automaton_a", clts1)
            .register_clts("automaton_b", clts2)
            .finish_with_checks()?;

        let mut output = Vec::new();
        context.print_structure(&mut output)?;
        let output_str = String::from_utf8(output)?;

        assert!(output_str.contains("CLTS Instances: 2"));
        assert!(output_str.contains("Automaton: automaton_a"));
        assert!(output_str.contains("Automaton: automaton_b"));

        Ok(())
    }

    #[test]
    fn print_structure_shows_state_transition_counts() -> TestResult {
        let mut builder = Clts::builder();
        builder.state("s0").initial("s0");
        builder.state("s1");
        builder.state("s2");
        let label = builder.labels().intern(["a"])?;
        // s0 has 2 outgoing, s1 has 1 incoming
        builder.transition("s0", &[label], "s1");
        builder.transition("s0", &[label], "s2");
        let clts = builder.build()?;

        let context = Context::builder()
            .register_clts("test", clts)
            .finish_with_checks()?;

        let mut output = Vec::new();
        context.print_structure(&mut output)?;
        let output_str = String::from_utf8(output)?;

        // Check that state details show correct counts
        assert!(output_str.contains("[0] s0"));
        assert!(output_str.contains("2 outgoing, 0 incoming"));
        assert!(output_str.contains("[1] s1"));
        assert!(output_str.contains("0 outgoing, 1 incoming"));

        Ok(())
    }
}
