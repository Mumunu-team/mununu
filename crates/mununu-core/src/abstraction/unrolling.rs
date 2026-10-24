//! State unrolling algorithm.
//!
//! This module implements the state unrolling algorithm that creates abstract states
//! from original states by incorporating variable values into the state space.

use super::evaluator::{EvaluationError, ExpressionEvaluator};
use super::expression::{Expr, GuardExpr, GuardResult};
use super::heuristics::{HeuristicConfig, StateSpaceStats, VariableContext, select_abstract_type};
use super::state::AbstractState;
use super::value::AbstractValue;
use crate::context_dsl::ast::TransitionModalitySpec;
use std::collections::{HashMap, HashSet, VecDeque};

/// Represents a transition in the original model before unrolling.
#[derive(Debug, Clone)]
pub struct OriginalTransition {
    pub from: String,
    pub to: String,
    pub label: String,
    pub guard: Option<String>,
    pub effects: Vec<Effect>,
    /// R.5 Item K sub-item K.2b (2026-06-06) — CTXDSL modality
    /// attribute (`[may]` / `[must]` / `[sharp]`) on the transition.
    /// Threaded through the unrolling pipeline so the unrolled
    /// CLTS edge inherits the source's modality. Defaults to
    /// `Sharp` for pre-K.2b adapters that don't populate this
    /// field (matches the K.1 default).
    pub modality: TransitionModalitySpec,
    /// R.5 Item K sub-item K.1b-unrolled (2026-06-08) — additional
    /// hyper-must target state names from the multi-target CTXDSL
    /// syntax `transition s -> [t1, t2, t3] on a [must];`. Carries
    /// `[t2, t3, ...]`; the primary target is the `to` field. Only
    /// load-bearing when `modality == MustOnly`. Defaults to
    /// `Vec::new()` (singleton hyper-must, matches K.2b behaviour
    /// exactly).
    ///
    /// Note: per-state-variable substitution in the unrolling loop
    /// applies to the primary `to` field via the unroller's
    /// existing state-name resolution; the additional targets here
    /// are used as-is. This works because the CTXDSL parser stores
    /// `additional_targets` as `StateSelector`s, which are pre-
    /// unrolling identifiers; the unroller expands the location
    /// portion of state names but preserves the variable-bindings
    /// suffix. For the K.1b-unrolled MVP, callers populate
    /// `additional_targets` with the LOCATION names (matching
    /// `from` / `to` shape); per-variable expansion of additional
    /// targets is a future follow-up if a real fixture demonstrates
    /// the need.
    pub additional_targets: Vec<String>,
}

/// Represents an effect (variable assignment) in a transition.
#[derive(Debug, Clone)]
pub struct Effect {
    pub target: String,
    pub value_expr: String, // Expression string that will be parsed to Expr
}

/// Represents a state in the original model.
#[derive(Debug, Clone)]
pub struct OriginalState {
    pub name: String,
    pub initial: bool,
}

/// Represents a variable declaration.
#[derive(Debug, Clone)]
pub struct VariableDecl {
    pub name: String,
    pub ty: String,              // "bool" or "i64"
    pub initial: Option<String>, // Expression string for initial value
}

/// Result of state unrolling.
#[derive(Debug, Clone)]
pub struct UnrolledClts {
    pub states: Vec<AbstractState>,
    pub transitions: Vec<UnrolledTransition>,
}

/// Represents a transition in the unrolled CLTS.
#[derive(Debug, Clone)]
pub struct UnrolledTransition {
    pub from: AbstractState,
    pub to: AbstractState,
    pub label: String,
    /// R.5 Item K sub-item K.2b (2026-06-06) — modality inherited
    /// from the source [`OriginalTransition`]. `build_clts_from_unrolled`
    /// reads this to emit the right `TransitionModality` on the
    /// resulting CLTS edge.
    pub modality: TransitionModalitySpec,
    /// R.5 Item K sub-item K.1b-unrolled (2026-06-08) — additional
    /// hyper-must target state NAMES (post-unrolling). The
    /// unrolling loop substitutes the primary `to` per
    /// variable-binding combination; additional targets are
    /// carried verbatim as location-portion names. Only
    /// load-bearing when `modality == MustOnly` and
    /// `additional_targets` is non-empty; otherwise
    /// `build_clts_from_unrolled` emits a singleton hyper-must
    /// or non-hyper edge per K.2b.
    pub additional_targets: Vec<String>,
}

/// Building context for state unrolling with thresholds and limits.
///
/// This context combines configuration, statistics, and limits to prevent
/// state space explosion during unrolling.
#[derive(Debug, Clone)]
pub struct BuildingContext {
    /// Configuration for type selection heuristics.
    pub heuristic_config: HeuristicConfig,
    /// Statistics tracking state space growth.
    pub stats: StateSpaceStats,
    /// Maximum total states allowed (None = use heuristic_config.max_total_states).
    pub max_total_states: Option<usize>,
    /// Maximum states per location (None = unlimited).
    pub max_states_per_location: Option<usize>,
    /// Warning threshold as fraction of max_total_states (default: 0.5).
    /// When reached, aggressive abstraction is applied.
    pub warning_threshold: f64,
    /// Whether to apply widening automatically when limits are approached.
    pub auto_widen: bool,
}

impl BuildingContext {
    /// Creates a new building context with default settings.
    pub fn new() -> Self {
        Self {
            heuristic_config: HeuristicConfig::default(),
            stats: StateSpaceStats::default(),
            max_total_states: None,
            max_states_per_location: None,
            warning_threshold: 0.5,
            auto_widen: true,
        }
    }

    /// Creates a building context from unrolling options.
    pub fn from_options(options: &UnrollingOptions) -> Self {
        let heuristic_config = options.heuristic_config.clone().unwrap_or_default();
        Self {
            heuristic_config: heuristic_config.clone(),
            stats: StateSpaceStats::default(),
            max_total_states: None, // Use heuristic_config.max_total_states
            max_states_per_location: options.max_states_per_location,
            warning_threshold: 0.5,
            auto_widen: true,
        }
    }

    /// Checks if state space is approaching limits.
    pub fn is_approaching_limit(&self) -> bool {
        let max_total = self.effective_max_total_states();
        self.stats.total_states >= (max_total as f64 * self.warning_threshold) as usize
    }

    /// Checks if state space has exceeded limits.
    pub fn has_exceeded_limit(&self) -> bool {
        self.stats.total_states > self.effective_max_total_states()
    }

    /// Checks if a specific location has exceeded its limit.
    pub fn location_exceeded_limit(&self, location: &str) -> bool {
        if let Some(limit) = self.max_states_per_location {
            self.stats
                .states_per_location
                .get(location)
                .map(|&count| count > limit)
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// Gets the effective maximum total states limit.
    pub fn effective_max_total_states(&self) -> usize {
        self.max_total_states
            .unwrap_or(self.heuristic_config.max_total_states)
    }
}

impl Default for BuildingContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for controlling unrolling behavior.
#[derive(Debug, Clone)]
pub struct UnrollingOptions {
    /// Maximum number of abstract states per original state location.
    pub max_states_per_location: Option<usize>,
    /// Use interval abstraction instead of exact values.
    pub use_interval_abstraction: bool,
    /// Widen intervals after this many refinement steps.
    pub widen_after: Option<usize>,
    /// Configuration for type selection heuristics.
    pub heuristic_config: Option<HeuristicConfig>,
}

impl Default for UnrollingOptions {
    fn default() -> Self {
        Self {
            max_states_per_location: None,
            use_interval_abstraction: false,
            widen_after: None,
            heuristic_config: Some(HeuristicConfig::default()),
        }
    }
}

/// Errors that can occur during unrolling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnrollingError {
    Evaluation(EvaluationError),
    StateLimitExceeded {
        location: String,
        count: usize,
        limit: usize,
    },
    InvalidVariableType {
        name: String,
        ty: String,
    },
    InvalidInitialValue {
        variable: String,
        value: String,
    },
    ParseError {
        expression: String,
        error: String,
    },
    Conflict(ConflictError),
    StateSpaceExplosion {
        message: String,
        current_state_count: usize,
        limit: usize,
        location: Option<String>,
    },
    /// A guard the variables cannot decide at a state: it reads a name that is no declared
    /// variable, or a shape refinement cannot split.
    UndecidableGuard {
        location: String,
        label: String,
        guard: String,
    },
}

/// Error indicating a conflict between realized valuations and explicit variable assignments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictError {
    /// Variable name that has the conflict.
    pub variable: String,
    /// Value realized from effects.
    pub realized_value: AbstractValue,
    /// Explicit value assigned in the target CLTS state (if any).
    pub explicit_value: Option<AbstractValue>,
    /// Name of the target CLTS state where the conflict occurred.
    pub state_name: String,
    /// Optional detailed message.
    pub message: Option<String>,
}

impl std::fmt::Display for UnrollingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evaluation(e) => write!(f, "evaluation error: {}", e),
            Self::StateLimitExceeded {
                location,
                count,
                limit,
            } => {
                write!(
                    f,
                    "state limit exceeded for location '{}': {} states (limit: {})",
                    location, count, limit
                )
            }
            Self::InvalidVariableType { name, ty } => {
                write!(f, "invalid variable type for '{}': {}", name, ty)
            }
            Self::InvalidInitialValue { variable, value } => {
                write!(
                    f,
                    "invalid initial value for variable '{}': {}",
                    variable, value
                )
            }
            Self::ParseError { expression, error } => {
                write!(f, "parse error in expression '{}': {}", expression, error)
            }
            Self::UndecidableGuard {
                location,
                label,
                guard,
            } => write!(
                f,
                "the guard `{guard}` on `{location}` --{label}--> cannot be decided from the \
                 automaton's variables (a name that is no declared `i64` variable, or a shape \
                 refinement cannot split); the transition would never fire"
            ),
            Self::Conflict(conflict) => {
                if let Some(msg) = &conflict.message {
                    write!(f, "{}", msg)
                } else {
                    match &conflict.explicit_value {
                        Some(explicit) => {
                            write!(
                                f,
                                "variable '{}' in state '{}' has conflict: realized value '{}' incompatible with explicit assignment '{}'",
                                conflict.variable,
                                conflict.state_name,
                                conflict.realized_value,
                                explicit
                            )
                        }
                        None => {
                            write!(
                                f,
                                "variable '{}' has realized value '{}' but was not declared in CLTS state '{}'",
                                conflict.variable, conflict.realized_value, conflict.state_name
                            )
                        }
                    }
                }
            }
            Self::StateSpaceExplosion {
                message,
                current_state_count,
                limit,
                location,
            } => {
                if let Some(loc) = location {
                    write!(
                        f,
                        "state space explosion in location '{}': {} states (limit: {}) - {}",
                        loc, current_state_count, limit, message
                    )
                } else {
                    write!(
                        f,
                        "state space explosion: {} states (limit: {}) - {}",
                        current_state_count, limit, message
                    )
                }
            }
        }
    }
}

impl std::fmt::Display for ConflictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(msg) = &self.message {
            write!(f, "{}", msg)
        } else {
            match &self.explicit_value {
                Some(explicit) => {
                    write!(
                        f,
                        "variable '{}' in state '{}' has conflict: realized value '{}' incompatible with explicit assignment '{}'",
                        self.variable, self.state_name, self.realized_value, explicit
                    )
                }
                None => {
                    write!(
                        f,
                        "variable '{}' has realized value '{}' but was not declared in CLTS state '{}'",
                        self.variable, self.realized_value, self.state_name
                    )
                }
            }
        }
    }
}

// ConflictError / UnrollingError keep hand-rolled Display: their formatting is
// conditional (message override + Option-field branches), which thiserror's
// per-variant `#[error("...")]` attribute cannot express.
impl std::error::Error for ConflictError {}

impl std::error::Error for UnrollingError {}

impl From<EvaluationError> for UnrollingError {
    fn from(e: EvaluationError) -> Self {
        Self::Evaluation(e)
    }
}

impl From<ConflictError> for UnrollingError {
    fn from(e: ConflictError) -> Self {
        Self::Conflict(e)
    }
}

/// Internal pipeline that orchestrates the state unrolling algorithm.
#[derive(Debug)]
struct UnrollingPipeline {
    original_states: Vec<OriginalState>,
    transitions: Vec<OriginalTransition>,
    variables: Vec<VariableDecl>,
    options: UnrollingOptions,
}

impl UnrollingPipeline {
    #[allow(clippy::result_large_err)]
    fn run(self) -> Result<UnrolledClts, UnrollingError> {
        let UnrollingPipeline {
            original_states,
            transitions,
            variables,
            options,
        } = self;

        // 1. Build a set of initial location names
        let initial_locations: HashSet<String> = original_states
            .iter()
            .filter(|s| s.initial)
            .map(|s| s.name.clone())
            .collect();

        // 2. Initialize abstract states from original states
        let abstract_states = initialize_abstract_states(
            original_states,
            &variables,
            options.heuristic_config.as_ref(),
        )?;

        // 3. Process transitions, applying effects and evaluating guards
        let mut unrolled_transitions = Vec::new();
        let mut worklist = VecDeque::new();
        let mut visited = HashSet::new();
        let mut state_map: HashMap<String, AbstractState> = HashMap::new();

        // Initialize state map with abstract states
        for state in abstract_states {
            let key = state.state_name();
            state_map.insert(key.clone(), state);
        }

        // Add initial states to worklist (only states at initial locations)
        for (key, state) in &state_map {
            if initial_locations.contains(&state.location) {
                worklist.push_back(key.clone());
            }
        }

        // Create building context for explosion prevention
        let mut building_context = BuildingContext::from_options(&options);

        // Track previous variable values for widening (per variable, per location)
        let mut previous_values: HashMap<(String, String), AbstractValue> = HashMap::new();

        while let Some(state_key) = worklist.pop_front() {
            if visited.contains(&state_key) {
                continue;
            }
            visited.insert(state_key.clone());

            let state = state_map.get(&state_key).cloned().unwrap_or_else(|| {
                // Should not happen, but defensive
                AbstractState::new(state_key.clone())
            });

            // Update building context statistics
            building_context
                .stats
                .add_state(&state.location, &state.variables);

            // Check for state space explosion
            if building_context.has_exceeded_limit() {
                return Err(UnrollingError::StateSpaceExplosion {
                    message: format!(
                        "Total state count {} exceeds limit {}",
                        building_context.stats.total_states,
                        building_context.effective_max_total_states()
                    ),
                    current_state_count: building_context.stats.total_states,
                    limit: building_context.effective_max_total_states(),
                    location: None,
                });
            }

            // Check location-specific limit
            if building_context.location_exceeded_limit(&state.location)
                && let Some(limit) = building_context.max_states_per_location
            {
                return Err(UnrollingError::StateSpaceExplosion {
                    message: format!("Location '{}' has exceeded state limit", state.location),
                    current_state_count: building_context
                        .stats
                        .states_per_location
                        .get(&state.location)
                        .copied()
                        .unwrap_or(0),
                    limit,
                    location: Some(state.location.clone()),
                });
            }

            // Find transitions from this location
            for transition in transitions_from_location(&transitions, &state.location) {
                // Parse guard expression
                let guard_expr = bind_variable_predicates(
                    parse_guard_expr(transition.guard.as_deref())?,
                    &state,
                );

                // Evaluate guard
                let predicates = HashMap::new(); // No predicates for now
                // evaluate_guard handles UnknownVariable errors by returning GuardResult::Maybe
                // Other evaluation errors (type mismatch, etc.) should be converted to Maybe for robustness
                let evaluator = ExpressionEvaluator::new(&state, &predicates);
                let guard_result = match evaluator.evaluate_guard(&guard_expr) {
                    Ok(result) => result,
                    Err(EvaluationError::UnknownVariable(_)) => {
                        // Unknown variables are handled by evaluate_guard, but if it still errors,
                        // treat as Maybe (conservative)
                        GuardResult::Maybe
                    }
                    Err(_) => {
                        // For other evaluation errors, treat as Maybe to allow unrolling to continue
                        // This handles cases where expressions reference variables that don't exist yet
                        // or have type mismatches - unrolling will refine states to resolve these
                        GuardResult::Maybe
                    }
                };

                match guard_result {
                    GuardResult::AlwaysTrue => {
                        // Apply effects to compute target state
                        let mut target_state = apply_effects(
                            &state,
                            &transition.effects,
                            &variables,
                            Some(&building_context.heuristic_config),
                            &mut building_context.stats,
                        )?;

                        // Check for conflicts with target state
                        let declared_vars: HashSet<String> =
                            variables.iter().map(|v| v.name.clone()).collect();
                        check_conflicts(
                            &target_state.variables,
                            &transition.to,
                            &declared_vars,
                            None, // No explicit variables in current implementation
                        )?;

                        // Apply explosion prevention: widening and threshold-based abstraction
                        // Use stats.total_states for consistency with limit checking
                        // Trigger much earlier (at 5% for counters, 10% for others) to prevent state explosion
                        let limit_ratio = building_context.stats.total_states as f64
                            / building_context.effective_max_total_states() as f64;
                        let should_apply_widening =
                            limit_ratio >= 0.05 || building_context.is_approaching_limit();

                        if should_apply_widening && building_context.auto_widen {
                            // Apply widening to variables that are growing
                            for (var_name, value) in &mut target_state.variables {
                                let key = (var_name.clone(), transition.to.clone());
                                let context = infer_variable_context(var_name);

                                // For counters or any integer variable, be more aggressive: convert to IntTop earlier
                                // Convert counters to IntTop at 5% of limit, other growing integers at 10%
                                if let AbstractValue::IntConstant(_)
                                | AbstractValue::IntInterval(_, _) = value
                                {
                                    // If it's a known counter, convert to IntTop immediately when we're at 5% of limit
                                    if matches!(context, VariableContext::Counter)
                                        && limit_ratio >= 0.05
                                    {
                                        *value = AbstractValue::IntTop;
                                        previous_values.insert(key, value.clone());
                                        continue;
                                    }
                                    // For other integer variables, check if they're growing
                                    if let Some(prev_value) = previous_values.get(&key) {
                                        let is_growing = match (prev_value, &value) {
                                            (
                                                AbstractValue::IntConstant(p),
                                                AbstractValue::IntConstant(c),
                                            ) => c > p,
                                            (
                                                AbstractValue::IntInterval(_, pmax),
                                                AbstractValue::IntConstant(c),
                                            ) => c > pmax,
                                            _ => false,
                                        };
                                        // If growing and we're past 10% of limit, convert to IntTop
                                        if is_growing && limit_ratio >= 0.1 {
                                            *value = AbstractValue::IntTop;
                                            previous_values.insert(key, value.clone());
                                            continue;
                                        }
                                    }
                                }

                                if let Some(prev_value) = previous_values.get(&key) {
                                    // Convert growing concrete values to intervals when approaching limits
                                    let abstracted_value = match (prev_value, &value) {
                                        // If we have a sequence of growing concrete integers, convert to interval
                                        (
                                            AbstractValue::IntConstant(prev_int),
                                            AbstractValue::IntConstant(curr_int),
                                        ) if *curr_int > *prev_int => {
                                            // Create an interval from previous to current
                                            // This bounds the counter and prevents infinite state creation
                                            AbstractValue::IntInterval(*prev_int, *curr_int)
                                        }
                                        // If we already have an interval and it's growing, apply widening
                                        (
                                            AbstractValue::IntInterval(prev_min, prev_max),
                                            AbstractValue::IntConstant(curr_int),
                                        ) if *curr_int > *prev_max => {
                                            // Extend interval to include new value, then widen if needed
                                            let extended =
                                                AbstractValue::IntInterval(*prev_min, *curr_int);
                                            super::heuristics::apply_widening_if_needed(
                                                extended,
                                                Some(prev_value),
                                                &building_context.heuristic_config,
                                            )
                                        }
                                        // For other cases, apply standard widening
                                        _ => super::heuristics::apply_widening_if_needed(
                                            value.clone(),
                                            Some(prev_value),
                                            &building_context.heuristic_config,
                                        ),
                                    };
                                    *value = abstracted_value;
                                } else {
                                    // Even without previous value, if we're approaching limits and this is an integer,
                                    // convert concrete integers to intervals to prevent unbounded growth
                                    // Do this early (at 5% for counters, 10% for others)
                                    let threshold = if matches!(context, VariableContext::Counter) {
                                        0.05
                                    } else {
                                        0.1
                                    };
                                    if limit_ratio >= threshold
                                        && let AbstractValue::IntConstant(int_val) = value
                                    {
                                        // Convert integer to interval starting from current value
                                        // This bounds future growth
                                        *value = AbstractValue::IntInterval(*int_val, *int_val);
                                    }
                                }
                                // Update previous value for next iteration
                                previous_values.insert(key, value.clone());
                            }
                        }

                        // Apply threshold-based abstraction (sets → intervals) if approaching limits
                        // Use stats.total_states for consistency with limit checking
                        let limit_ratio = building_context.stats.total_states as f64
                            / building_context.effective_max_total_states() as f64;
                        // Trigger earlier: at 5% for counters, 10% for others
                        if limit_ratio >= 0.05 || building_context.is_approaching_limit() {
                            for (var_name, value) in &mut target_state.variables {
                                let context = infer_variable_context(var_name);
                                let mut abstracted = select_abstract_type(
                                    value.clone(),
                                    context,
                                    &building_context.heuristic_config,
                                    &building_context.stats,
                                );

                                // Convert counters to IntTop much earlier (at 5% of limit) to prevent state explosion
                                // For other integers, convert at 10% if they're concrete values
                                let should_convert_to_top = match context {
                                    VariableContext::Counter => limit_ratio >= 0.05,
                                    _ => limit_ratio >= 0.1,
                                };
                                if should_convert_to_top
                                    && let AbstractValue::IntConstant(_)
                                    | AbstractValue::IntInterval(_, _) = abstracted
                                {
                                    abstracted = AbstractValue::IntTop;
                                }
                                *value = abstracted;
                            }
                        }

                        // Update location to target location
                        target_state.location = transition.to.clone();
                        let target_key = target_state.state_name();

                        // Check if adding this state would exceed the limit BEFORE adding it
                        // This prevents state explosion by catching it early
                        // Apply aggressive abstraction when we're at 15% of limit to prevent hitting the limit
                        // Use stats.total_states for consistency
                        let limit_ratio = building_context.stats.total_states as f64
                            / building_context.effective_max_total_states() as f64;
                        if limit_ratio >= 0.15 {
                            // We're approaching the limit - apply aggressive abstraction to ALL integer variables
                            // This prevents unbounded growth from any integer variable, not just counters
                            for value in target_state.variables.values_mut() {
                                if let AbstractValue::IntConstant(_)
                                | AbstractValue::IntInterval(_, _) = value
                                {
                                    // Convert any integer variable to IntTop to prevent further state explosion
                                    *value = AbstractValue::IntTop;
                                }
                            }
                        }

                        // Store target state in map
                        state_map.insert(target_key.clone(), target_state.clone());

                        // Update building context statistics (will be updated again when state is processed)
                        building_context.stats.total_states = state_map.len();

                        unrolled_transitions.push(UnrolledTransition {
                            from: state.clone(),
                            to: target_state,
                            label: transition.label.clone(),
                            // R.5 Item K sub-item K.2b (2026-06-06)
                            // — inherit modality from the source
                            // OriginalTransition.
                            modality: transition.modality,
                            // R.5 Item K sub-item K.1b-unrolled
                            // (2026-06-08) — propagate additional
                            // hyper-must target names verbatim.
                            additional_targets: transition.additional_targets.clone(),
                        });

                        // Add target to worklist if new
                        if !visited.contains(&target_key) {
                            worklist.push_back(target_key.clone());
                        }
                    }
                    GuardResult::AlwaysFalse => {
                        // Skip transition
                    }
                    GuardResult::Maybe => {
                        // Refine state and retry
                        let refined = refine_state_with_guard(
                            &state,
                            &guard_expr,
                            &variables,
                            options.heuristic_config.as_ref(),
                        )?;
                        // A refinement that returns the state unchanged cannot decide the guard
                        // (it reads a name no variable carries, or a shape refinement does not
                        // split): retrying would revisit the same state and the transition would
                        // silently never fire. Refuse instead.
                        if refined.len() == 1 && refined[0].state_name() == state.state_name() {
                            return Err(UnrollingError::UndecidableGuard {
                                location: transition.from.clone(),
                                label: transition.label.clone(),
                                guard: transition.guard.clone().unwrap_or_default(),
                            });
                        }
                        for refined_state in refined {
                            let refined_key = refined_state.state_name();
                            state_map.insert(refined_key.clone(), refined_state);
                            if !visited.contains(&refined_key) {
                                worklist.push_back(refined_key);
                            }
                        }
                        // Update building context statistics after refinement
                        building_context.stats.total_states = state_map.len();
                    }
                }
            }
        }

        // Collect only reachable states (those that were visited or are targets of transitions)
        let mut reachable_states: HashSet<String> = visited;
        for transition in &unrolled_transitions {
            reachable_states.insert(transition.to.state_name());
        }

        let final_states: Vec<AbstractState> = state_map
            .into_iter()
            .filter(|(key, _)| reachable_states.contains(key))
            .map(|(_, state)| state)
            .collect();

        Ok(UnrolledClts {
            states: final_states,
            transitions: unrolled_transitions,
        })
    }
}

/// Checks if two abstract values are compatible (not conflicting).
///
/// Two values are compatible if their intersection (meet) is not empty.
/// This is used to determine if a realized value conflicts with an explicit assignment.
///
/// # Examples
/// ```
/// use mununu_core::abstraction::value::AbstractValue;
/// use mununu_core::abstraction::unrolling::compatible;
///
/// // Compatible: concrete value within interval
/// assert!(compatible(
///     &AbstractValue::int_constant(5),
///     &AbstractValue::int_interval(0, 10)
/// ));
///
/// // Incompatible: concrete values differ
/// assert!(!compatible(
///     &AbstractValue::int_constant(5),
///     &AbstractValue::int_constant(10)
/// ));
/// ```
pub fn compatible(realized: &AbstractValue, explicit: &AbstractValue) -> bool {
    let intersection = realized.meet(explicit);
    !matches!(intersection, AbstractValue::Undefined)
}

/// Checks for conflicts between a realized valuation and a target CLTS state.
///
/// This function implements conflict detection as specified in the variable type system
/// documentation. It checks:
/// 1. Conflicts between realized valuations and explicit variable assignments
/// 2. Undeclared variables in realized valuations
///
/// # Arguments
/// * `valuation` - The realized valuation (variable bindings from effects)
/// * `target_state_name` - Name of the target CLTS state
/// * `declared_variables` - Set of variables declared in the CLTS
/// * `explicit_variables` - Optional map of explicit variable assignments in the target state
///
/// # Returns
/// * `Ok(())` if no conflicts are found
/// * `Err(ConflictError)` if a conflict is detected
///
/// # Examples
/// ```
/// use std::collections::{HashMap, HashSet};
/// use mununu_core::abstraction::value::AbstractValue;
/// use mununu_core::abstraction::unrolling::check_conflicts;
///
/// let valuation = {
///     let mut map = HashMap::new();
///     map.insert("count".to_string(), AbstractValue::int_constant(5));
///     map
/// };
///
/// let declared = {
///     let mut set = HashSet::new();
///     set.insert("count".to_string());
///     set
/// };
///
/// // No conflicts
/// assert!(check_conflicts(&valuation, "Done", &declared, None).is_ok());
///
/// // Undeclared variable
/// let valuation_undeclared = {
///     let mut map = HashMap::new();
///     map.insert("unknown".to_string(), AbstractValue::int_constant(5));
///     map
/// };
/// assert!(check_conflicts(&valuation_undeclared, "Done", &declared, None).is_err());
/// ```
#[allow(clippy::result_large_err)]
pub fn check_conflicts(
    valuation: &HashMap<String, AbstractValue>,
    target_state_name: &str,
    declared_variables: &HashSet<String>,
    explicit_variables: Option<&HashMap<String, AbstractValue>>,
) -> Result<(), ConflictError> {
    // Check for undeclared variables in realized valuation
    for (var, realized_value) in valuation {
        if !declared_variables.contains(var) {
            return Err(ConflictError {
                variable: var.clone(),
                realized_value: realized_value.clone(),
                explicit_value: None,
                state_name: target_state_name.to_string(),
                message: Some(format!(
                    "Variable '{}' has realized value '{}' but was not declared in CLTS state '{}'",
                    var, realized_value, target_state_name
                )),
            });
        }
    }

    // Check conflicts with explicit variable assignments (if provided)
    if let Some(explicit_vars) = explicit_variables {
        for (var, explicit_value) in explicit_vars {
            if let Some(realized_value) = valuation.get(var) {
                // Check if values are compatible
                if !compatible(realized_value, explicit_value) {
                    return Err(ConflictError {
                        variable: var.clone(),
                        realized_value: realized_value.clone(),
                        explicit_value: Some(explicit_value.clone()),
                        state_name: target_state_name.to_string(),
                        message: Some(format!(
                            "Variable '{}' in state '{}' has conflict: realized value '{}' incompatible with explicit assignment '{}'",
                            var, target_state_name, realized_value, explicit_value
                        )),
                    });
                }

                // If compatible, check if we need to refine (use intersection)
                let intersection = realized_value.meet(explicit_value);
                if matches!(intersection, AbstractValue::Undefined) {
                    return Err(ConflictError {
                        variable: var.clone(),
                        realized_value: realized_value.clone(),
                        explicit_value: Some(explicit_value.clone()),
                        state_name: target_state_name.to_string(),
                        message: Some(format!(
                            "Variable '{}' in state '{}' has conflict: intersection of realized value '{}' and explicit assignment '{}' is empty",
                            var, target_state_name, realized_value, explicit_value
                        )),
                    });
                }
            }
        }
    }

    Ok(())
}

/// Unrolls states by incorporating variable values into the state space.
///
/// This function takes a set of original states (with locations) and transitions
/// (with guards and effects), and creates a new CLTS where variable values are
/// encoded directly into state names. This enables static evaluation of guards
/// and effects, making the resulting CLTS suitable for μ-calculus evaluation.
///
/// # Process
///
/// 1. **Initialize abstract states**: Create initial abstract states from original
///    states, assigning initial variable values.
/// 2. **Process transitions**: For each transition, evaluate guards over abstract
///    states and apply effects to compute target states.
/// 3. **Refine states**: When a guard evaluates to `Maybe`, split the state
///    into refined states that satisfy or don't satisfy the guard.
/// 4. **Apply heuristics**: Use type selection heuristics to prevent state space
///    explosion by choosing appropriate abstract value representations.
///
/// # Examples
///
/// ## Basic unrolling with integer counter
/// ```
/// use mununu_core::abstraction::unrolling::*;
/// use mununu_core::abstraction::AbstractValue;
///
/// let states = vec![
///     OriginalState { name: "Start".to_string(), initial: true },
///     OriginalState { name: "Done".to_string(), initial: false },
/// ];
///
/// let transitions = vec![
///     OriginalTransition {
///         from: "Start".to_string(),
///         to: "Done".to_string(),
///         label: "finish".to_string(),
///         guard: Some("count >= 3".to_string()),
///         effects: vec![],
///         modality: Default::default(),
///         additional_targets: vec![],
///     },
/// ];
///
/// let variables = vec![
///     VariableDecl {
///         name: "count".to_string(),
///         ty: "i64".to_string(),
///         initial: Some("0".to_string()),
///     },
/// ];
///
/// let options = UnrollingOptions::default();
/// let result = unroll_states(states, transitions, variables, options);
/// // Result contains unrolled states with variable values encoded in state names
/// ```
///
/// ## Unrolling with effects
/// ```
/// use mununu_core::abstraction::unrolling::*;
///
/// let states = vec![
///     OriginalState { name: "Counting".to_string(), initial: true },
/// ];
///
/// let transitions = vec![
///     OriginalTransition {
///         from: "Counting".to_string(),
///         to: "Counting".to_string(),
///         label: "increment".to_string(),
///         guard: Some("count < 5".to_string()),
///         effects: vec![
///             Effect {
///                 target: "count".to_string(),
///                 value_expr: "count + 1".to_string(),
///             },
///         ],
///         modality: Default::default(),
///         additional_targets: vec![],
///     },
/// ];
///
/// let variables = vec![
///     VariableDecl {
///         name: "count".to_string(),
///         ty: "i64".to_string(),
///         initial: Some("0".to_string()),
///     },
/// ];
///
/// let options = UnrollingOptions::default();
/// let result = unroll_states(states, transitions, variables, options);
/// // Result contains states: Counting_count_0, Counting_count_1, ..., Counting_count_4
/// ```
#[allow(clippy::result_large_err)]
pub fn unroll_states(
    original_states: Vec<OriginalState>,
    transitions: Vec<OriginalTransition>,
    variables: Vec<VariableDecl>,
    options: UnrollingOptions,
) -> Result<UnrolledClts, UnrollingError> {
    UnrollingPipeline {
        original_states,
        transitions,
        variables,
        options,
    }
    .run()
}

/// Initializes abstract states from original states with variable initializations.
///
/// Creates abstract states with normalized abstract values. Optionally applies
/// type selection heuristics if a configuration is provided.
#[allow(clippy::result_large_err)]
fn initialize_abstract_states(
    original_states: Vec<OriginalState>,
    variables: &[VariableDecl],
    heuristic_config: Option<&HeuristicConfig>,
) -> Result<Vec<AbstractState>, UnrollingError> {
    let mut abstract_states = Vec::new();

    for state in original_states {
        let mut abstract_state = AbstractState::new(state.name);

        // Initialize variables from declarations
        for var in variables {
            let mut initial_value = evaluate_initial_value(var)?;
            // Normalize the initial value
            initial_value = initial_value.normalized();

            // Apply type selection heuristics if configured
            if let Some(config) = heuristic_config {
                // Use Unknown context for initial values
                let context = VariableContext::Unknown;
                // Initial state: no states yet, so use default stats
                let stats = StateSpaceStats::default();
                initial_value = select_abstract_type(initial_value, context, config, &stats);
            }

            abstract_state.set_variable(var.name.clone(), initial_value);
        }

        abstract_states.push(abstract_state);
    }

    Ok(abstract_states)
}

/// Evaluates the initial value of a variable.
#[allow(clippy::result_large_err)]
fn evaluate_initial_value(var: &VariableDecl) -> Result<AbstractValue, UnrollingError> {
    match var.ty.as_str() {
        "bool" => {
            if let Some(init_str) = &var.initial {
                match init_str.as_str() {
                    "true" => Ok(AbstractValue::bool_constant(true)),
                    "false" => Ok(AbstractValue::bool_constant(false)),
                    _ => Err(UnrollingError::InvalidInitialValue {
                        variable: var.name.clone(),
                        value: init_str.clone(),
                    }),
                }
            } else {
                Ok(AbstractValue::bool_constant(false)) // Default for bool
            }
        }
        "i64" => {
            if let Some(init_str) = &var.initial {
                match init_str.parse::<i64>() {
                    Ok(val) => Ok(AbstractValue::int_constant(val)),
                    Err(_) => Err(UnrollingError::InvalidInitialValue {
                        variable: var.name.clone(),
                        value: init_str.clone(),
                    }),
                }
            } else {
                Ok(AbstractValue::int_constant(0)) // Default for i64
            }
        }
        ty => Err(UnrollingError::InvalidVariableType {
            name: var.name.clone(),
            ty: ty.to_string(),
        }),
    }
}

/// Finds transitions from a given location.
fn transitions_from_location<'a>(
    transitions: &'a [OriginalTransition],
    location: &str,
) -> Vec<&'a OriginalTransition> {
    transitions.iter().filter(|t| t.from == location).collect()
}

/// Parses a guard expression string into a GuardExpr.
#[allow(clippy::result_large_err)]
fn parse_guard_expr(guard_str: Option<&str>) -> Result<GuardExpr, UnrollingError> {
    match guard_str {
        None | Some("") | Some("true") => Ok(GuardExpr::true_guard()),
        Some("false") => Ok(GuardExpr::false_guard()),
        Some(s) => {
            // Parse the guard using the existing parser
            // parse_guard returns (normalized_string, GuardExpr) - it doesn't fail on parse errors
            // but may produce invalid expressions. We'll handle evaluation errors during guard evaluation.
            convert_guard_tree(&crate::guard::parse_guard_tree(s))
        }
    }
}

/// A bare name in a guard (`guard ready`) that is one of the state's variables reads that
/// variable (`ready == true`). Left as a predicate it looked up an environment the unroller passes
/// empty, so the guard was undecidable and the transition never fired, whatever the variable held.
fn bind_variable_predicates(guard: GuardExpr, state: &AbstractState) -> GuardExpr {
    match guard {
        GuardExpr::Predicate(name) if state.get_variable(&name).is_some() => GuardExpr::comparison(
            Expr::var(name),
            crate::guard::ComparisonOp::Eq,
            Expr::Bool(true),
        ),
        GuardExpr::And(a, b) => GuardExpr::And(
            Box::new(bind_variable_predicates(*a, state)),
            Box::new(bind_variable_predicates(*b, state)),
        ),
        GuardExpr::Or(a, b) => GuardExpr::Or(
            Box::new(bind_variable_predicates(*a, state)),
            Box::new(bind_variable_predicates(*b, state)),
        ),
        GuardExpr::Not(a) => GuardExpr::Not(Box::new(bind_variable_predicates(*a, state))),
        other => other,
    }
}

/// Converts a guard's boolean structure ([`crate::guard::GuardTree`]) to our GuardExpr, leaf by
/// leaf through [`convert_guard_expr`].
#[allow(clippy::result_large_err)]
fn convert_guard_tree(tree: &crate::guard::GuardTree) -> Result<GuardExpr, UnrollingError> {
    use crate::guard::GuardTree;
    Ok(match tree {
        GuardTree::Leaf(e) => convert_guard_expr(e)?,
        GuardTree::And(a, b) => GuardExpr::And(
            Box::new(convert_guard_tree(a)?),
            Box::new(convert_guard_tree(b)?),
        ),
        GuardTree::Or(a, b) => GuardExpr::Or(
            Box::new(convert_guard_tree(a)?),
            Box::new(convert_guard_tree(b)?),
        ),
        GuardTree::Not(a) => GuardExpr::Not(Box::new(convert_guard_tree(a)?)),
    })
}

/// Converts a parsed GuardExpr from the guard module to our abstraction GuardExpr.
#[allow(clippy::result_large_err)]
fn convert_guard_expr(expr: &crate::guard::GuardExpr) -> Result<GuardExpr, UnrollingError> {
    match expr {
        crate::guard::GuardExpr::True => Ok(GuardExpr::true_guard()),
        crate::guard::GuardExpr::False => Ok(GuardExpr::false_guard()),
        crate::guard::GuardExpr::Predicate(name) => Ok(GuardExpr::Predicate(name.clone())),
        crate::guard::GuardExpr::Comparison { left, op, right } => {
            // Try to parse expressions, but if parsing fails (e.g., malformed expressions),
            // treat as variables to allow evaluation to handle UnknownVariable gracefully
            let left_expr = parse_expr_string(left).unwrap_or_else(|_| Expr::var(left.trim()));
            let right_expr = parse_expr_string(right).unwrap_or_else(|_| Expr::var(right.trim()));
            Ok(GuardExpr::comparison(left_expr, *op, right_expr))
        }
    }
}

/// Parses an expression string into an Expr.
#[allow(clippy::result_large_err)]
fn parse_expr_string(expr_str: &str) -> Result<Expr, UnrollingError> {
    // Simple parser for basic expressions
    let mut trimmed = expr_str.trim();

    // Strip outer parentheses if the entire expression is wrapped in balanced parentheses
    // This handles cases where expr_to_string wraps binary expressions in parentheses
    // e.g., "(approvals >= 1)" -> "approvals >= 1"
    let trimmed = loop {
        if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
            break trimmed;
        }
        // Check if the outer parentheses are balanced (the closing paren matches the opening)
        let mut depth = 0;
        let mut found_outer_match = false;
        for (i, ch) in trimmed.chars().enumerate() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 && i == trimmed.len() - 1 {
                        // The closing paren at the end matches the opening paren
                        found_outer_match = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if found_outer_match {
            trimmed = trimmed[1..trimmed.len() - 1].trim();
        } else {
            break trimmed;
        }
    };

    // Try parsing as constant
    if let Ok(val) = trimmed.parse::<i64>() {
        return Ok(Expr::constant(val));
    }

    // Try parsing as boolean
    match trimmed {
        "true" => return Ok(Expr::bool(true)),
        "false" => return Ok(Expr::bool(false)),
        _ => {}
    }

    // Try parsing as variable
    if is_valid_identifier(trimmed) {
        return Ok(Expr::var(trimmed));
    }

    // Try parsing as arithmetic expression
    if let Some((left, op, right)) = parse_arithmetic(trimmed) {
        let left_expr = parse_expr_string(left)?;
        let right_expr = parse_expr_string(right)?;
        return match op {
            "+" => Ok(Expr::Add(Box::new(left_expr), Box::new(right_expr))),
            "-" => Ok(Expr::Sub(Box::new(left_expr), Box::new(right_expr))),
            "*" => Ok(Expr::Mul(Box::new(left_expr), Box::new(right_expr))),
            "/" => Ok(Expr::Div(Box::new(left_expr), Box::new(right_expr))),
            _ => Err(UnrollingError::ParseError {
                expression: trimmed.to_string(),
                error: format!("unknown operator: {}", op),
            }),
        };
    }

    // If we can't parse it as an expression, try treating it as a variable name
    // This handles cases where expressions have parentheses or other formatting issues
    // The evaluation will handle UnknownVariable errors gracefully
    if !trimmed.is_empty() {
        // Extract a potential variable name by removing parentheses and whitespace
        let cleaned = trimmed.trim_matches(|c: char| c.is_whitespace() || c == '(' || c == ')');
        if is_valid_identifier(cleaned) {
            return Ok(Expr::var(cleaned));
        }
    }

    Err(UnrollingError::ParseError {
        expression: trimmed.to_string(),
        error: "could not parse expression".to_string(),
    })
}

/// Checks if a string is a valid identifier.
fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Parses an arithmetic expression into (left, operator, right).
/// Handles subtraction by finding the rightmost operator to avoid conflicts with negative numbers.
fn parse_arithmetic(expr: &str) -> Option<(&str, &str, &str)> {
    // Simple parser - looks for operators in order of precedence
    // For subtraction, we need to be careful: find from right to avoid matching negative numbers
    let operators = ["+", "*", "/", "-"]; // Process - last to handle negative numbers correctly
    for op in &operators {
        // For subtraction, search from right to left to avoid matching negative numbers
        if *op == "-" {
            // Find the rightmost occurrence that's not at the start (to avoid negative numbers)
            if let Some(pos) = expr.rfind('-') {
                // Make sure it's not at the start and has content on both sides
                if pos > 0 && pos < expr.len() - 1 {
                    let left = &expr[..pos].trim();
                    let right = &expr[pos + 1..].trim();
                    // Ensure left side is not empty and doesn't end with an operator
                    if !left.is_empty()
                        && !right.is_empty()
                        && !left.ends_with('+')
                        && !left.ends_with('-')
                        && !left.ends_with('*')
                        && !left.ends_with('/')
                    {
                        return Some((left, op, right));
                    }
                }
            }
        } else if let Some(pos) = expr.find(op) {
            let left = &expr[..pos].trim();
            let right = &expr[pos + op.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, op, right));
            }
        }
    }
    None
}

/// Applies effects to a state, computing the target state.
///
/// Evaluates expressions, normalizes results, and optionally applies type selection
/// heuristics to prevent state space explosion.
#[allow(clippy::result_large_err)]
fn apply_effects(
    state: &AbstractState,
    effects: &[Effect],
    _variables: &[VariableDecl],
    heuristic_config: Option<&HeuristicConfig>,
    stats: &mut StateSpaceStats,
) -> Result<AbstractState, UnrollingError> {
    let mut new_state = state.clone();

    for effect in effects {
        // Parse effect value expression
        let value_expr = parse_expr_string(&effect.value_expr)?;

        // Evaluate expression in current state
        let empty_predicates: HashMap<String, bool> = HashMap::new();
        let evaluator = ExpressionEvaluator::new(state, &empty_predicates);
        let mut rhs_value = evaluator.evaluate(&value_expr)?;
        // Normalize the result
        rhs_value = rhs_value.normalized();

        // Apply type selection heuristics if configured
        if let Some(config) = heuristic_config {
            // Determine variable context (heuristics can infer from variable name patterns)
            let context = infer_variable_context(&effect.target);
            rhs_value = select_abstract_type(rhs_value, context, config, stats);

            // Note: Widening would be applied here if we had previous values to compare.
            // For now, widening is handled by select_abstract_type when appropriate.
        }

        // Update variable
        new_state.set_variable(effect.target.clone(), rhs_value);
    }

    Ok(new_state)
}

/// Infers variable context from variable name patterns.
///
/// This is a simple heuristic that can be extended with more sophisticated
/// pattern matching or user annotations.
fn infer_variable_context(var_name: &str) -> VariableContext {
    let lower = var_name.to_lowercase();
    if lower.contains("counter") || lower.contains("count") {
        VariableContext::Counter
    } else if lower.contains("state") || lower.contains("status") {
        VariableContext::State
    } else if lower.contains("accum") || lower.contains("sum") || lower.contains("total") {
        VariableContext::Accumulator
    } else {
        VariableContext::Unknown
    }
}

/// Refines a state by splitting it based on a guard that evaluates to Maybe.
///
/// Uses meet operations for refinement and optionally applies type selection
/// heuristics to the refined states.
#[allow(clippy::result_large_err)]
fn refine_state_with_guard(
    state: &AbstractState,
    guard: &GuardExpr,
    _variables: &[VariableDecl],
    heuristic_config: Option<&HeuristicConfig>,
) -> Result<Vec<AbstractState>, UnrollingError> {
    // Use the refinement module to get refined states
    let mut refined = super::refinement::refine_state_with_guard(state, guard);

    // Apply normalization and heuristics to refined states
    if let Some(config) = heuristic_config {
        let stats = StateSpaceStats::default(); // Use default stats for refinement
        for refined_state in &mut refined {
            // Normalize all variable values in the refined state
            let var_names: Vec<String> = refined_state.variables.keys().cloned().collect();
            for var_name in var_names {
                if let Some(value) = refined_state.get_variable(&var_name).cloned() {
                    let normalized = value.normalized();
                    let context = infer_variable_context(&var_name);
                    let optimized = select_abstract_type(normalized, context, config, &stats);
                    refined_state.set_variable(var_name.clone(), optimized);
                }
            }
        }
    } else {
        // Just normalize without heuristics
        for refined_state in &mut refined {
            let var_names: Vec<String> = refined_state.variables.keys().cloned().collect();
            for var_name in var_names {
                if let Some(value) = refined_state.get_variable(&var_name).cloned() {
                    refined_state.set_variable(var_name.clone(), value.normalized());
                }
            }
        }
    }

    Ok(refined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn test_initialize_abstract_states() {
        let states = vec![OriginalState {
            name: "Test".to_string(),
            initial: true,
        }];
        let variables = vec![
            VariableDecl {
                name: "x".to_string(),
                ty: "i64".to_string(),
                initial: Some("5".to_string()),
            },
            VariableDecl {
                name: "flag".to_string(),
                ty: "bool".to_string(),
                initial: Some("true".to_string()),
            },
        ];

        let abstract_states = initialize_abstract_states(states, &variables, None).unwrap();
        assert_eq!(abstract_states.len(), 1);
        assert_eq!(
            abstract_states[0].get_variable("x"),
            Some(&AbstractValue::int_constant(5))
        );
        assert_eq!(
            abstract_states[0].get_variable("flag"),
            Some(&AbstractValue::bool_constant(true))
        );
    }

    #[test]
    fn test_parse_expr_string() {
        assert_eq!(parse_expr_string("5").unwrap(), Expr::constant(5));
        assert_eq!(parse_expr_string("x").unwrap(), Expr::var("x"));
        assert!(matches!(
            parse_expr_string("x + 5").unwrap(),
            Expr::Add(_, _)
        ));
    }

    #[test]
    fn test_unroll_with_abstract_values() {
        // Test unrolling with intervals and sets
        let states = vec![OriginalState {
            name: "Start".to_string(),
            initial: true,
        }];
        let transitions = vec![OriginalTransition {
            from: "Start".to_string(),
            to: "End".to_string(),
            label: "step".to_string(),
            guard: Some("x > 5".to_string()),
            effects: vec![Effect {
                target: "x".to_string(),
                value_expr: "x + 1".to_string(),
            }],
            modality: TransitionModalitySpec::Sharp,
            additional_targets: Vec::new(),
        }];
        let variables = vec![VariableDecl {
            name: "x".to_string(),
            ty: "i64".to_string(),
            initial: Some("0".to_string()),
        }];

        let options = UnrollingOptions {
            max_states_per_location: Some(100),
            ..UnrollingOptions::default()
        };

        let result = unroll_states(states, transitions, variables, options);
        // Should succeed or fail gracefully
        assert!(result.is_ok() || matches!(result, Err(UnrollingError::StateLimitExceeded { .. })));
    }

    #[test]
    fn test_apply_effects_with_normalization() {
        // Test that effects normalize results
        let mut state = AbstractState::new("Test");
        state.set_variable("x", AbstractValue::int_interval(5, 5)); // Should normalize to constant

        let effects = vec![Effect {
            target: "x".to_string(),
            value_expr: "x + 1".to_string(),
        }];

        let variables = vec![];
        let mut stats = StateSpaceStats::default();
        let new_state = apply_effects(&state, &effects, &variables, None, &mut stats).unwrap();

        // Result should be normalized
        let value = new_state.get_variable("x").unwrap();
        assert_eq!(value, &AbstractValue::int_constant(6));
    }

    #[test]
    fn test_refine_state_with_heuristics() {
        let mut state = AbstractState::new("Test");
        state.set_variable("counter", AbstractValue::int_interval(0, 10));

        let guard = GuardExpr::comparison(
            Expr::var("counter"),
            crate::guard::ComparisonOp::Gt,
            Expr::constant(5),
        );

        let config = HeuristicConfig::default();
        let refined = refine_state_with_guard(&state, &guard, &[], Some(&config)).unwrap();

        // Should produce refined states
        assert!(!refined.is_empty());
        // Refined states should be normalized (check that normalization doesn't change values)
        for refined_state in &refined {
            for var_value in refined_state.variables.values() {
                let cloned = var_value.clone();
                let normalized = cloned.normalized();
                // Normalized value should be equivalent
                assert_eq!(var_value, &normalized);
            }
        }
    }

    #[test]
    fn test_apply_effects() {
        let mut state = AbstractState::new("Test");
        state.set_variable("x", AbstractValue::int_constant(5));

        let effects = vec![Effect {
            target: "x".to_string(),
            value_expr: "x + 1".to_string(),
        }];

        let variables = vec![];
        let mut stats = StateSpaceStats::default();
        let new_state = apply_effects(&state, &effects, &variables, None, &mut stats).unwrap();
        assert_eq!(
            new_state.get_variable("x"),
            Some(&AbstractValue::int_constant(6))
        );
    }

    // ============================================================================
    // Conflict Detection Tests (Issue 9)
    // ============================================================================

    #[test]
    fn test_compatible_concrete_values() {
        // Compatible: same concrete values
        assert!(compatible(
            &AbstractValue::int_constant(5),
            &AbstractValue::int_constant(5)
        ));

        // Incompatible: different concrete values
        assert!(!compatible(
            &AbstractValue::int_constant(5),
            &AbstractValue::int_constant(10)
        ));
    }

    #[test]
    fn test_compatible_concrete_with_interval() {
        // Compatible: concrete value within interval
        assert!(compatible(
            &AbstractValue::int_constant(5),
            &AbstractValue::int_interval(0, 10)
        ));

        // Incompatible: concrete value outside interval
        assert!(!compatible(
            &AbstractValue::int_constant(15),
            &AbstractValue::int_interval(0, 10)
        ));
    }

    #[test]
    fn test_compatible_concrete_with_set() {
        // Compatible: concrete value in set
        let mut set = HashSet::new();
        set.insert(5);
        set.insert(10);
        assert!(compatible(
            &AbstractValue::int_constant(5),
            &AbstractValue::IntSet(set.clone())
        ));

        // Incompatible: concrete value not in set
        assert!(!compatible(
            &AbstractValue::int_constant(15),
            &AbstractValue::IntSet(set)
        ));
    }

    #[test]
    fn test_compatible_intervals() {
        // Compatible: overlapping intervals
        assert!(compatible(
            &AbstractValue::int_interval(0, 10),
            &AbstractValue::int_interval(5, 15)
        ));

        // Incompatible: non-overlapping intervals
        assert!(!compatible(
            &AbstractValue::int_interval(0, 5),
            &AbstractValue::int_interval(10, 15)
        ));
    }

    #[test]
    fn test_compatible_boolean_values() {
        // Compatible: same boolean values
        assert!(compatible(
            &AbstractValue::bool_constant(true),
            &AbstractValue::bool_constant(true)
        ));

        // Incompatible: different boolean values
        assert!(!compatible(
            &AbstractValue::bool_constant(true),
            &AbstractValue::bool_constant(false)
        ));
    }

    #[test]
    fn test_compatible_symbol_values() {
        // Compatible: same symbol values
        assert!(compatible(
            &AbstractValue::symbol_constant("pending".to_string()),
            &AbstractValue::symbol_constant("pending".to_string())
        ));

        // Incompatible: different symbol values
        assert!(!compatible(
            &AbstractValue::symbol_constant("pending".to_string()),
            &AbstractValue::symbol_constant("completed".to_string())
        ));
    }

    #[test]
    fn test_check_conflicts_no_conflicts() {
        let mut valuation = HashMap::new();
        valuation.insert("count".to_string(), AbstractValue::int_constant(5));

        let mut declared = HashSet::new();
        declared.insert("count".to_string());

        // No conflicts: variable is declared
        assert!(check_conflicts(&valuation, "Done", &declared, None).is_ok());
    }

    #[test]
    fn test_check_conflicts_undeclared_variable() {
        let mut valuation = HashMap::new();
        valuation.insert("unknown".to_string(), AbstractValue::int_constant(5));

        let declared = HashSet::new();

        // Conflict: undeclared variable
        let result = check_conflicts(&valuation, "Done", &declared, None);
        assert!(result.is_err());
        if let Err(conflict) = result {
            assert_eq!(conflict.variable, "unknown");
            assert_eq!(conflict.state_name, "Done");
            assert_eq!(conflict.explicit_value, None);
        } else {
            panic!("Expected ConflictError");
        }
    }

    #[test]
    fn test_check_conflicts_explicit_assignment_compatible() {
        let mut valuation = HashMap::new();
        valuation.insert("count".to_string(), AbstractValue::int_constant(5));

        let mut declared = HashSet::new();
        declared.insert("count".to_string());

        let mut explicit = HashMap::new();
        explicit.insert("count".to_string(), AbstractValue::int_interval(0, 10));

        // Compatible: realized value within explicit interval
        assert!(check_conflicts(&valuation, "Done", &declared, Some(&explicit)).is_ok());
    }

    #[test]
    fn test_check_conflicts_explicit_assignment_incompatible() {
        let mut valuation = HashMap::new();
        valuation.insert("count".to_string(), AbstractValue::int_constant(5));

        let mut declared = HashSet::new();
        declared.insert("count".to_string());

        let mut explicit = HashMap::new();
        explicit.insert("count".to_string(), AbstractValue::int_constant(10));

        // Incompatible: realized value conflicts with explicit assignment
        let result = check_conflicts(&valuation, "Done", &declared, Some(&explicit));
        assert!(result.is_err());
        if let Err(conflict) = result {
            assert_eq!(conflict.variable, "count");
            assert_eq!(conflict.state_name, "Done");
            assert_eq!(conflict.realized_value, AbstractValue::int_constant(5));
            assert_eq!(
                conflict.explicit_value,
                Some(AbstractValue::int_constant(10))
            );
        } else {
            panic!("Expected ConflictError");
        }
    }

    #[test]
    fn test_check_conflicts_explicit_assignment_overlapping() {
        let mut valuation = HashMap::new();
        valuation.insert("count".to_string(), AbstractValue::int_interval(0, 10));

        let mut declared = HashSet::new();
        declared.insert("count".to_string());

        let mut explicit = HashMap::new();
        explicit.insert("count".to_string(), AbstractValue::int_interval(5, 15));

        // Overlapping: should be compatible (intersection is [5, 10])
        assert!(check_conflicts(&valuation, "Done", &declared, Some(&explicit)).is_ok());
    }

    #[test]
    fn test_check_conflicts_explicit_assignment_non_overlapping() {
        let mut valuation = HashMap::new();
        valuation.insert("count".to_string(), AbstractValue::int_interval(0, 5));

        let mut declared = HashSet::new();
        declared.insert("count".to_string());

        let mut explicit = HashMap::new();
        explicit.insert("count".to_string(), AbstractValue::int_interval(10, 15));

        // Non-overlapping: should conflict
        let result = check_conflicts(&valuation, "Done", &declared, Some(&explicit));
        assert!(result.is_err());
        if let Err(conflict) = result {
            assert_eq!(conflict.variable, "count");
            assert_eq!(conflict.state_name, "Done");
        } else {
            panic!("Expected ConflictError");
        }
    }

    #[test]
    fn test_check_conflicts_multiple_variables() {
        let mut valuation = HashMap::new();
        valuation.insert("x".to_string(), AbstractValue::int_constant(5));
        valuation.insert("y".to_string(), AbstractValue::bool_constant(true));

        let mut declared = HashSet::new();
        declared.insert("x".to_string());
        declared.insert("y".to_string());

        // No conflicts: all variables declared
        assert!(check_conflicts(&valuation, "State", &declared, None).is_ok());
    }

    #[test]
    fn test_check_conflicts_one_undeclared() {
        let mut valuation = HashMap::new();
        valuation.insert("x".to_string(), AbstractValue::int_constant(5));
        valuation.insert("unknown".to_string(), AbstractValue::int_constant(10));

        let mut declared = HashSet::new();
        declared.insert("x".to_string());

        // Conflict: one variable undeclared
        let result = check_conflicts(&valuation, "State", &declared, None);
        assert!(result.is_err());
        if let Err(conflict) = result {
            assert_eq!(conflict.variable, "unknown");
        } else {
            panic!("Expected ConflictError");
        }
    }

    // ============================================================================
    // State Space Explosion Prevention Tests (Issue 10)
    // ============================================================================

    #[test]
    fn test_building_context_new() {
        let ctx = BuildingContext::new();
        assert_eq!(ctx.warning_threshold, 0.5);
        assert!(ctx.auto_widen);
        assert_eq!(ctx.stats.total_states, 0);
    }

    #[test]
    fn test_building_context_from_options() {
        let options = UnrollingOptions {
            max_states_per_location: Some(100),
            heuristic_config: Some(HeuristicConfig {
                max_total_states: 500,
                ..Default::default()
            }),
            ..Default::default()
        };
        let ctx = BuildingContext::from_options(&options);
        assert_eq!(ctx.max_states_per_location, Some(100));
        assert_eq!(ctx.heuristic_config.max_total_states, 500);
    }

    #[test]
    fn test_building_context_is_approaching_limit() {
        let mut ctx = BuildingContext::new();
        ctx.heuristic_config.max_total_states = 100;
        ctx.warning_threshold = 0.5;

        // Add 40 states (below threshold)
        for _ in 0..40 {
            let mut vars = HashMap::new();
            vars.insert("x".to_string(), AbstractValue::int_constant(0));
            ctx.stats.add_state("Location1", &vars);
        }
        assert!(!ctx.is_approaching_limit());

        // Add 20 more states (at threshold)
        for _ in 0..20 {
            let mut vars = HashMap::new();
            vars.insert("x".to_string(), AbstractValue::int_constant(0));
            ctx.stats.add_state("Location1", &vars);
        }
        assert!(ctx.is_approaching_limit());
    }

    #[test]
    fn test_building_context_has_exceeded_limit() {
        let mut ctx = BuildingContext::new();
        ctx.heuristic_config.max_total_states = 100;

        // Add 100 states (at limit)
        for _ in 0..100 {
            let mut vars = HashMap::new();
            vars.insert("x".to_string(), AbstractValue::int_constant(0));
            ctx.stats.add_state("Location1", &vars);
        }
        assert!(!ctx.has_exceeded_limit());

        // Add one more state (exceeds limit)
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), AbstractValue::int_constant(0));
        ctx.stats.add_state("Location1", &vars);
        assert!(ctx.has_exceeded_limit());
    }

    #[test]
    fn test_building_context_location_exceeded_limit() {
        let mut ctx = BuildingContext::new();
        ctx.max_states_per_location = Some(10);

        // Add 10 states to Location1 (at limit)
        for _ in 0..10 {
            let mut vars = HashMap::new();
            vars.insert("x".to_string(), AbstractValue::int_constant(0));
            ctx.stats.add_state("Location1", &vars);
        }
        assert!(!ctx.location_exceeded_limit("Location1"));

        // Add one more state (exceeds limit)
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), AbstractValue::int_constant(0));
        ctx.stats.add_state("Location1", &vars);
        assert!(ctx.location_exceeded_limit("Location1"));
    }

    #[test]
    fn test_building_context_effective_max_total_states() {
        let mut ctx = BuildingContext::new();
        ctx.heuristic_config.max_total_states = 100;
        assert_eq!(ctx.effective_max_total_states(), 100);

        ctx.max_total_states = Some(200);
        assert_eq!(ctx.effective_max_total_states(), 200);
    }

    #[test]
    fn test_state_space_explosion_error() {
        let error = UnrollingError::StateSpaceExplosion {
            message: "Test explosion".to_string(),
            current_state_count: 150,
            limit: 100,
            location: Some("Location1".to_string()),
        };

        let display = format!("{}", error);
        assert!(display.contains("state space explosion"));
        assert!(display.contains("Location1"));
        assert!(display.contains("150"));
        assert!(display.contains("100"));
    }
}
