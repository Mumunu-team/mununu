//! Incremental loader utilities for the CLTS Context DSL.
//!
//! The loader fingerprints canonicalised documents, tracks dependencies between
//! automata, compositions, formulas, and controllers, and produces change plans
//! that consumers can use to drive minimal rebuilds.
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Write;
use std::hash::{Hash, Hasher};

use super::ast::{FormulaExpr, *};
use super::canonicalize;

#[derive(Debug, Clone, Default)]
/// Stores the latest fingerprints for each top-level section of a context
/// document. The state can be diffed against a new document to compute an
/// incremental [`LoadPlan`].
pub struct IncrementalState {
    automata: HashMap<String, u64>,
    compositions: HashMap<String, u64>,
    controllers: HashMap<String, u64>,
    formulas: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
/// Describes the set of elements that must be rebuilt (or removed) to bring a
/// consumer in sync with a context document.
pub struct LoadPlan {
    /// Automata whose fingerprints changed compared to the previously cached
    /// state.
    pub changed_automata: BTreeSet<String>,
    /// Automata that disappeared from the document.
    pub removed_automata: BTreeSet<String>,
    /// Compositions whose fingerprints changed.
    pub changed_compositions: BTreeSet<String>,
    /// Compositions that were removed.
    pub removed_compositions: BTreeSet<String>,
    /// Controllers whose fingerprints changed, either directly or via
    /// dependency propagation.
    pub changed_controllers: BTreeSet<String>,
    /// Controllers that were removed.
    pub removed_controllers: BTreeSet<String>,
    /// μ-calculus formulas whose body or metadata changed.
    pub changed_formulas: BTreeSet<String>,
    /// μ-calculus formulas that were removed.
    pub removed_formulas: BTreeSet<String>,
    new_state: IncrementalState,
}

impl LoadPlan {
    /// Returns `true` when the plan contains no changes, allowing callers to
    /// skip rebuild work entirely.
    pub fn is_noop(&self) -> bool {
        self.changed_automata.is_empty()
            && self.removed_automata.is_empty()
            && self.changed_compositions.is_empty()
            && self.removed_compositions.is_empty()
            && self.changed_controllers.is_empty()
            && self.removed_controllers.is_empty()
            && self.changed_formulas.is_empty()
            && self.removed_formulas.is_empty()
    }
}

impl IncrementalState {
    /// Constructs an empty state with no cached fingerprints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Diffs the provided context document against the cached fingerprints,
    /// returning a [`LoadPlan`] that captures the required rebuild actions.
    ///
    /// The document is canonicalised prior to hashing so equivalent documents
    /// with reordered sections produce stable fingerprints.
    pub fn diff(&self, doc: &ContextDoc) -> LoadPlan {
        let mut canonical = doc.clone();
        canonicalize::canonicalize(&mut canonical);

        let fingerprints = compute_fingerprints(&canonical);
        let dependencies = compute_dependencies(&canonical);

        let mut changed_automata = detect_changes(&self.automata, &fingerprints.automata);
        let mut removed_automata = detect_removals(&self.automata, &fingerprints.automata);
        let mut changed_compositions =
            detect_changes(&self.compositions, &fingerprints.compositions);
        let mut removed_compositions =
            detect_removals(&self.compositions, &fingerprints.compositions);
        let mut changed_controllers = detect_changes(&self.controllers, &fingerprints.controllers);
        let mut removed_controllers = detect_removals(&self.controllers, &fingerprints.controllers);
        let mut changed_formulas = detect_changes(&self.formulas, &fingerprints.formulas);
        let mut removed_formulas = detect_removals(&self.formulas, &fingerprints.formulas);

        propagate_dependencies(
            &mut changed_automata,
            &mut changed_compositions,
            &mut changed_controllers,
            &mut changed_formulas,
            &dependencies,
        );

        propagate_dependencies(
            &mut removed_automata,
            &mut removed_compositions,
            &mut removed_controllers,
            &mut removed_formulas,
            &dependencies,
        );

        let new_state = fingerprints.into_state();

        LoadPlan {
            changed_automata,
            removed_automata,
            changed_compositions,
            removed_compositions,
            changed_controllers,
            removed_controllers,
            changed_formulas,
            removed_formulas,
            new_state,
        }
    }

    /// Replaces the cached fingerprints with the ones embedded in `plan`.
    /// Typically invoked after a successful rebuild.
    pub fn apply(&mut self, plan: &LoadPlan) {
        *self = plan.new_state.clone();
    }
}

#[derive(Debug, Clone, Default)]
struct Fingerprints {
    automata: HashMap<String, u64>,
    compositions: HashMap<String, u64>,
    controllers: HashMap<String, u64>,
    formulas: HashMap<String, u64>,
}

impl Fingerprints {
    /// Converts the collected fingerprints into an [`IncrementalState`].
    fn into_state(self) -> IncrementalState {
        IncrementalState {
            automata: self.automata,
            compositions: self.compositions,
            controllers: self.controllers,
            formulas: self.formulas,
        }
    }
}

#[derive(Debug, Default)]
struct Dependencies {
    composition_members: HashMap<String, Vec<String>>,
    controller_source: HashMap<String, String>,
    controller_formula: HashMap<String, String>,
    formula_targets: HashMap<String, Vec<String>>,
}

/// Returns the set of entries present in `new` whose fingerprint differs from
/// the cached value in `old`.
fn detect_changes(old: &HashMap<String, u64>, new: &HashMap<String, u64>) -> BTreeSet<String> {
    let mut changed = BTreeSet::new();
    for (key, value) in new {
        if old.get(key) != Some(value) {
            changed.insert(key.clone());
        }
    }
    changed
}

/// Returns the set of keys present in `old` that are no longer present in `new`.
fn detect_removals(old: &HashMap<String, u64>, new: &HashMap<String, u64>) -> BTreeSet<String> {
    let mut removed = BTreeSet::new();
    for key in old.keys() {
        if !new.contains_key(key) {
            removed.insert(key.clone());
        }
    }
    removed
}

/// Propagates automaton changes to compositions, formulas, and controllers by
/// traversing the dependency graph captured in `dependencies`.
fn propagate_dependencies(
    changed_automata: &mut BTreeSet<String>,
    changed_compositions: &mut BTreeSet<String>,
    changed_controllers: &mut BTreeSet<String>,
    changed_formulas: &mut BTreeSet<String>,
    dependencies: &Dependencies,
) {
    // Breadth-first propagation: any automaton change can affect formulas that
    // depend on it, which may in turn affect controllers.
    let mut queue: VecDeque<String> = changed_automata.iter().cloned().collect();
    let mut visited = BTreeSet::new();

    while let Some(automaton) = queue.pop_front() {
        if !visited.insert(automaton.clone()) {
            continue;
        }
        for (composition, members) in &dependencies.composition_members {
            if members.contains(&automaton) {
                changed_compositions.insert(composition.clone());
            }
        }
        for (formula, targets) in &dependencies.formula_targets {
            if targets.contains(&automaton) && changed_formulas.insert(formula.clone()) {
                queue.push_back(formula.clone());
            }
        }
        for (controller, source) in &dependencies.controller_source {
            if source == &automaton {
                changed_controllers.insert(controller.clone());
            }
        }
    }

    // Propagate composition-to-composition dependencies: if an inner composition
    // changed, outer compositions that reference it must also be reprocessed.
    let mut comp_queue: VecDeque<String> = changed_compositions.iter().cloned().collect();
    let mut comp_visited = BTreeSet::new();
    while let Some(comp) = comp_queue.pop_front() {
        if !comp_visited.insert(comp.clone()) {
            continue;
        }
        for (outer_comp, members) in &dependencies.composition_members {
            if members.contains(&comp) && changed_compositions.insert(outer_comp.clone()) {
                comp_queue.push_back(outer_comp.clone());
            }
        }
    }

    for formula in changed_formulas.clone() {
        for (controller, target_formula) in &dependencies.controller_formula {
            if target_formula == &formula {
                changed_controllers.insert(controller.clone());
            }
        }
    }
}

/// Computes stable fingerprints for all top-level entities in `doc`.
fn compute_fingerprints(doc: &ContextDoc) -> Fingerprints {
    let mut fingerprints = Fingerprints::default();

    for automaton in &doc.automata {
        let id = stable_id(&automaton.meta, &automaton.name.name);
        fingerprints.automata.insert(id, hash_automaton(automaton));
    }

    for composition in &doc.compositions {
        let id = stable_id(&composition.meta, &composition.name.name);
        fingerprints
            .compositions
            .insert(id, hash_composition(composition));
    }

    for controller in &doc.controllers {
        let id = stable_id(&controller.meta, &controller.name.name);
        fingerprints
            .controllers
            .insert(id, hash_controller(controller));
    }

    for formula in &doc.mu_formulas {
        let id = stable_id(&formula.meta, &formula.name.name);
        fingerprints.formulas.insert(id, hash_formula(formula));
    }

    fingerprints
}

/// Builds the dependency graph used to propagate change notifications across
/// sections.
fn compute_dependencies(doc: &ContextDoc) -> Dependencies {
    let mut deps = Dependencies::default();

    for composition in &doc.compositions {
        let id = stable_id(&composition.meta, &composition.name.name);
        let members = composition
            .members
            .iter()
            .map(|m| m.name.name.clone())
            .collect();
        deps.composition_members.insert(id, members);
    }

    for controller in &doc.controllers {
        let id = stable_id(&controller.meta, &controller.name.name);
        deps.controller_source
            .insert(id.clone(), controller.source.name.clone());
        deps.controller_formula
            .insert(id, controller.formula.name.clone());
    }

    for formula in &doc.mu_formulas {
        let id = stable_id(&formula.meta, &formula.name.name);
        match &formula.targets {
            FormulaTargets::All(_) => {
                deps.formula_targets.insert(id, Vec::new());
            }
            FormulaTargets::Named(list) => {
                let targets = list.iter().map(|ident| ident.name.clone()).collect();
                deps.formula_targets.insert(id, targets);
            }
        }
    }

    deps
}

/// Returns the explicit `meta.id` if present or falls back to the provided
/// name.
fn stable_id(meta: &Meta, fallback: &str) -> String {
    meta.id.clone().unwrap_or_else(|| fallback.to_owned())
}

/// Produces a stable hash for an automaton, capturing parameters, alphabet,
/// variables, states, and transitions.
fn hash_automaton(automaton: &Automaton) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_string(
        &mut hasher,
        &stable_id(&automaton.meta, &automaton.name.name),
    );
    for param in &automaton.parameters {
        hash_string(&mut hasher, &param.name.name);
        match &param.spec {
            RangeSpec::Named(ident) => {
                hash_string(&mut hasher, &ident.name);
            }
            RangeSpec::Bounds { lower, upper } => {
                hash_string(&mut hasher, &expr_to_string(lower));
                hash_string(&mut hasher, &expr_to_string(upper));
            }
        }
    }

    for alphabet in &automaton.alphabet {
        hash_string(&mut hasher, &alphabet.name.name);
        if let Some(expr) = &alphabet.index {
            hash_string(&mut hasher, &expr_to_string(expr));
        }
    }

    for variable in &automaton.variables {
        hash_string(&mut hasher, &variable.name.name);
        if let Some(index) = &variable.index {
            hash_string(&mut hasher, &expr_to_string(index));
        }
        hash_string(&mut hasher, &format!("{:?}", variable.ty));
        hash_string(&mut hasher, &expr_to_string(&variable.init));
    }

    for state in &automaton.states {
        hash_string(&mut hasher, &state.name.name);
        if let Some(index) = &state.index {
            hash_string(&mut hasher, &state_index_to_string(index));
        }
        hash_bool(&mut hasher, state.is_initial);
        for assignment in &state.overrides {
            hash_string(&mut hasher, &assignment.target.name);
            hash_string(&mut hasher, &expr_to_string(&assignment.expr));
        }
    }

    for group in &automaton.state_groups {
        hash_string(&mut hasher, &group.name.name);
        for member in &group.members {
            hash_string(&mut hasher, &state_selector_to_string(member));
        }
    }

    for transition in &automaton.transitions {
        hash_string(&mut hasher, &state_selector_to_string(&transition.source));
        hash_string(&mut hasher, &state_selector_to_string(&transition.target));
        match &transition.label {
            TransitionLabel::Named { name, index } => {
                hash_string(&mut hasher, &name.name);
                if let Some(expr) = index {
                    hash_string(&mut hasher, &expr_to_string(expr));
                }
            }
            TransitionLabel::Epsilon(_) => {
                hash_string(&mut hasher, "epsilon");
            }
        }
        for label in &transition.additional_labels {
            hash_string(&mut hasher, "additional");
            match label {
                TransitionLabel::Named { name, index } => {
                    hash_string(&mut hasher, &name.name);
                    if let Some(expr) = index {
                        hash_string(&mut hasher, &expr_to_string(expr));
                    }
                }
                TransitionLabel::Epsilon(_) => {
                    hash_string(&mut hasher, "epsilon");
                }
            }
        }
        if let Some(guard) = &transition.guard {
            hash_string(&mut hasher, &expr_to_string(guard));
        }
        for assignment in &transition.effects {
            hash_string(&mut hasher, &assignment.target.name);
            hash_string(&mut hasher, &expr_to_string(&assignment.expr));
        }
    }

    hasher.finish()
}

/// Hashes a composition definition (its ID, kind, and ordered member list).
fn hash_composition(composition: &Composition) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_string(
        &mut hasher,
        &stable_id(&composition.meta, &composition.name.name),
    );
    hash_string(&mut hasher, &format!("{:?}", composition.kind));
    for member in &composition.members {
        hash_string(&mut hasher, &member.name.name);
        if let Some(ref index) = member.index {
            hash_string(&mut hasher, &format!("{:?}", index.kind));
        }
    }
    hasher.finish()
}

/// Hashes a controller definition, including the optional export path.
fn hash_controller(controller: &Controller) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_string(
        &mut hasher,
        &stable_id(&controller.meta, &controller.name.name),
    );
    hash_string(&mut hasher, &controller.source.name);
    hash_string(&mut hasher, &controller.formula.name);
    if let Some(path) = &controller.export {
        hash_string(&mut hasher, path);
    }
    if let Some(minimize) = controller.options.minimize {
        hash_string(&mut hasher, &format!("minimize:{minimize}"));
    }
    if let Some(diag) = &controller.options.diagnostics {
        if let Some(counterexample) = diag.counterexample {
            hash_string(
                &mut hasher,
                &format!("diagnostics:counterexample:{counterexample}"),
            );
        }
        if let Some(deadlock_traces) = diag.deadlock_traces {
            hash_string(
                &mut hasher,
                &format!("diagnostics:deadlock:{deadlock_traces}"),
            );
        }
        if let Some(max_traces) = diag.max_counter_traces {
            hash_string(
                &mut hasher,
                &format!("diagnostics:max_counter_traces:{max_traces}"),
            );
        }
        if let Some(proof_obligations) = diag.proof_obligations {
            hash_string(
                &mut hasher,
                &format!("diagnostics:proof_obligations:{proof_obligations}"),
            );
        }
    }
    hasher.finish()
}

/// Hashes a μ-calculus formula body and its metadata.
fn hash_formula(formula: &MuFormula) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_string(&mut hasher, &stable_id(&formula.meta, &formula.name.name));
    match &formula.targets {
        FormulaTargets::All(_) => hash_string(&mut hasher, "all"),
        FormulaTargets::Named(list) => {
            for ident in list {
                hash_string(&mut hasher, &ident.name);
            }
        }
    }
    match &formula.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            hash_string(&mut hasher, &mu_expr.raw);
        }
        FormulaExpr::Ltl(ltl_expr) => {
            // Hash the LTL formula representation
            hash_string(&mut hasher, &format!("{:?}", ltl_expr.formula));
        }
        FormulaExpr::Template(spec) => {
            hash_string(&mut hasher, "template");
            hash_string(&mut hasher, &spec.template.name);
            for arg in &spec.args {
                hash_string(&mut hasher, &arg.name.name);
                hash_string(&mut hasher, &arg.value);
            }
        }
    }
    hasher.finish()
}

/// Renders a state index specification into a canonical string for hashing.
fn state_index_to_string(index: &StateIndexSpec) -> String {
    match index {
        StateIndexSpec::Range { symbol, range } => format!("{} in {}", symbol.name, range.name),
        StateIndexSpec::Expr(expr) => expr_to_string(expr),
    }
}

/// Renders a state reference (with optional indices) into a string.
fn state_ref_to_string(state: &StateRef) -> String {
    match state {
        StateRef::Simple(ident) => ident.name.clone(),
        StateRef::Indexed { name, indices } => {
            let mut buffer = String::new();
            write!(&mut buffer, "{}[", name.name).unwrap();
            for (idx, expr) in indices.iter().enumerate() {
                if idx > 0 {
                    buffer.push(',');
                }
                buffer.push_str(&expr_to_string(expr));
            }
            buffer.push(']');
            buffer
        }
    }
}

fn state_selector_to_string(selector: &StateSelector) -> String {
    match selector {
        StateSelector::Named(state) => state_ref_to_string(state),
        StateSelector::Group(name) => format!("group:{}", name.name),
        StateSelector::Wildcard(pattern) => format!("wildcard:{}", pattern.pattern),
    }
}

/// Converts an arithmetic or logical expression into a canonical string.
fn expr_to_string(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Integer(value) => value.to_string(),
        ExprKind::Ident(ident) => ident.name.clone(),
        ExprKind::Index { target, expr } => {
            format!("{}[{}]", target.name, expr_to_string(expr))
        }
        ExprKind::Unary { op, expr } => {
            let op_str = match op {
                UnaryOp::Not => "!",
                UnaryOp::Neg => "-",
            };
            format!("({}{})", op_str, expr_to_string(expr))
        }
        ExprKind::Binary { left, op, right } => {
            let op_str = match op {
                BinaryOp::Add => "+",
                BinaryOp::Sub => "-",
                BinaryOp::Mul => "*",
                BinaryOp::Div => "/",
                BinaryOp::Mod => "%",
                BinaryOp::And => "&&",
                BinaryOp::Or => "||",
                BinaryOp::Eq => "==",
                BinaryOp::Ne => "!=",
                BinaryOp::Lt => "<",
                BinaryOp::Le => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::Ge => ">=",
            };
            format!(
                "({}{}{})",
                expr_to_string(left),
                op_str,
                expr_to_string(right)
            )
        }
        ExprKind::Group(expr) => format!("({})", expr_to_string(expr)),
    }
}

/// Helper that feeds a string into the hasher.
fn hash_string(hasher: &mut DefaultHasher, value: &str) {
    value.hash(hasher);
}

/// Helper that feeds a boolean into the hasher.
fn hash_bool(hasher: &mut DefaultHasher, value: bool) {
    value.hash(hasher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_dsl::parser::parse;

    fn load_doc(source: &str) -> ContextDoc {
        let mut doc = parse(source).expect("context parses");
        canonicalize::canonicalize(&mut doc);
        doc
    }

    #[test]
    fn initial_plan_marks_all_changed() {
        let source = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let doc = load_doc(source);
        let state = IncrementalState::new();
        let plan = state.diff(&doc);
        assert!(plan.changed_automata.contains("A"));
        assert!(plan.changed_compositions.is_empty());
        assert!(plan.changed_formulas.is_empty());
        assert!(plan.changed_controllers.is_empty());
    }

    #[test]
    fn automaton_change_propagates_to_composition() {
        let source = r#"
context example {
    automata {
        automaton Gate {
            states { state Open initial; state Closed; }
            transitions { transition Open -> Closed on label shut; transition Closed -> Open on label open; }
        }
        automaton Panel {
            states { state Idle initial; }
            transitions { transition Idle -> Idle on label idle; }
        }
    }
    composition {
        synchronous Workcell { members [Gate, Panel]; }
    }
}
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);
        state.apply(&plan1);
        assert!(plan1.changed_automata.contains("Gate"));
        assert!(plan1.changed_compositions.contains("Workcell"));

        let modified = r#"
context example {
    automata {
        automaton Gate {
            states { state Open initial; state Closed; }
            transitions { transition Open -> Closed on label shut; transition Closed -> Open on label reopen; }
        }
        automaton Panel {
            states { state Idle initial; }
            transitions { transition Idle -> Idle on label idle; }
        }
    }
    composition {
        synchronous Workcell { members [Gate, Panel]; }
    }
}
"#;
        let doc2 = load_doc(modified);
        let plan2 = state.diff(&doc2);
        assert!(plan2.changed_automata.contains("Gate"));
        assert!(plan2.changed_compositions.contains("Workcell"));
        assert!(plan2.changed_controllers.is_empty());
    }

    #[test]
    fn controller_change_detects_dependencies() {
        let source = r#"
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
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);
        state.apply(&plan1);
        assert!(plan1.changed_controllers.contains("C1"));

        let modified = r#"
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
        controller C1 { source Machine; satisfying safe; export "ctrl.ctxdsl"; }
    }
}
"#;
        let doc2 = load_doc(modified);
        let plan2 = state.diff(&doc2);
        assert!(plan2.changed_controllers.contains("C1"));
    }

    #[test]
    fn load_plan_is_noop() {
        // Test LoadPlan::is_noop() method (lines 53-62)
        let plan = LoadPlan {
            changed_automata: BTreeSet::new(),
            removed_automata: BTreeSet::new(),
            changed_compositions: BTreeSet::new(),
            removed_compositions: BTreeSet::new(),
            changed_controllers: BTreeSet::new(),
            removed_controllers: BTreeSet::new(),
            changed_formulas: BTreeSet::new(),
            removed_formulas: BTreeSet::new(),
            new_state: IncrementalState::new(),
        };
        assert!(plan.is_noop());

        // Test with changes
        let mut plan_with_changes = plan.clone();
        plan_with_changes.changed_automata.insert("A".to_string());
        assert!(!plan_with_changes.is_noop());

        // Test with removals
        let mut plan_with_removals = plan.clone();
        plan_with_removals.removed_automata.insert("B".to_string());
        assert!(!plan_with_removals.is_noop());
    }

    #[test]
    fn incremental_state_apply() {
        // Test IncrementalState::apply() method (lines 127-129)
        let source = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);

        // Verify initial state is empty
        assert!(state.automata.is_empty());

        // Apply the plan
        state.apply(&plan1);

        // Verify state now contains the automaton fingerprint
        assert!(state.automata.contains_key("A"));

        // Verify subsequent diff shows no changes
        let plan2 = state.diff(&doc);
        assert!(plan2.is_noop());
    }

    #[test]
    fn detect_removals() {
        // Test detect_removals function (lines 172-180)
        let source1 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
        automaton B {
            states { state T initial; }
            transitions { transition T -> T on label beta; }
        }
    }
}
"#;
        let doc1 = load_doc(source1);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        // Remove automaton B
        let source2 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let doc2 = load_doc(source2);
        let plan2 = state.diff(&doc2);

        // Verify B is marked as removed
        assert!(plan2.removed_automata.contains("B"));
        assert!(!plan2.removed_automata.contains("A"));
    }

    #[test]
    fn formula_change_propagates_to_controller() {
        // Test that formula changes propagate to controllers (lines 218-224)
        let source = r#"
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
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);
        state.apply(&plan1);

        // Change formula body
        let modified = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = mu X. (tick || X); }
    }
    controllers {
        controller C1 { source Machine; satisfying safe; }
    }
}
"#;
        let doc2 = load_doc(modified);
        let plan2 = state.diff(&doc2);

        // Verify formula change propagates to controller
        assert!(plan2.changed_formulas.contains("safe"));
        assert!(plan2.changed_controllers.contains("C1"));
    }

    #[test]
    fn automaton_change_propagates_to_formula_and_controller() {
        // Test that automaton changes propagate through formulas to controllers (lines 206-210)
        let source = r#"
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
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);
        state.apply(&plan1);

        // Change automaton (add a new transition)
        let modified = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; state T; }
            transitions { 
                transition S -> S on label tick;
                transition S -> T on label reset;
            }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = nu X. (tick && X); }
    }
    controllers {
        controller C1 { source Machine; satisfying safe; }
    }
}
"#;
        let doc2 = load_doc(modified);
        let plan2 = state.diff(&doc2);

        // Verify automaton change propagates to formula and then to controller
        assert!(plan2.changed_automata.contains("Machine"));
        assert!(plan2.changed_formulas.contains("safe"));
        assert!(plan2.changed_controllers.contains("C1"));
    }

    #[test]
    fn fingerprint_stability() {
        // Test that fingerprints are stable for equivalent documents (hash_automaton, etc.)
        let source1 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let source2 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let doc1 = load_doc(source1);
        let doc2 = load_doc(source2);

        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        let plan2 = state.diff(&doc2);
        // Same document should produce noop plan
        assert!(plan2.is_noop());
    }

    #[test]
    fn fingerprint_detects_changes() {
        // Test that fingerprints detect actual changes
        let source1 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
    }
}
"#;
        let source2 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; state T; }
            transitions { 
                transition S -> S on label alpha;
                transition S -> T on label beta;
            }
        }
    }
}
"#;
        let doc1 = load_doc(source1);
        let doc2 = load_doc(source2);

        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        let plan2 = state.diff(&doc2);
        // Different document should detect changes
        assert!(!plan2.is_noop());
        assert!(plan2.changed_automata.contains("A"));
    }

    #[test]
    fn composition_removal_detection() {
        // Test removal detection for compositions
        let source1 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
        automaton B {
            states { state T initial; }
            transitions { transition T -> T on label beta; }
        }
    }
    composition {
        synchronous Comp { members [A, B]; }
    }
}
"#;
        let source2 = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
        automaton B {
            states { state T initial; }
            transitions { transition T -> T on label beta; }
        }
    }
}
"#;
        let doc1 = load_doc(source1);
        let doc2 = load_doc(source2);

        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        let plan2 = state.diff(&doc2);
        assert!(plan2.removed_compositions.contains("Comp"));
    }

    #[test]
    fn controller_removal_detection() {
        // Test removal detection for controllers
        let source1 = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = true; }
    }
    controllers {
        controller C1 { source Machine; satisfying safe; }
    }
}
"#;
        let source2 = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = true; }
    }
}
"#;
        let doc1 = load_doc(source1);
        let doc2 = load_doc(source2);

        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        let plan2 = state.diff(&doc2);
        assert!(plan2.removed_controllers.contains("C1"));
    }

    #[test]
    fn formula_removal_detection() {
        // Test removal detection for formulas
        let source1 = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = true; }
        formula liveness { over Machine; body = <> tick; }
    }
}
"#;
        let source2 = r#"
context example {
    automata {
        automaton Machine {
            states { state S initial; }
            transitions { transition S -> S on label tick; }
        }
    }
    mu_formulas {
        formula safe { over Machine; body = true; }
    }
}
"#;
        let doc1 = load_doc(source1);
        let doc2 = load_doc(source2);

        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc1);
        state.apply(&plan1);

        let plan2 = state.diff(&doc2);
        assert!(plan2.removed_formulas.contains("liveness"));
        assert!(!plan2.removed_formulas.contains("safe"));
    }

    #[test]
    fn formula_targets_all_behavior() {
        // Test that formulas with "over all" have empty target list (lines 284-285)
        // Note: Formulas with "over all" don't track specific automaton dependencies,
        // so they won't be automatically marked as changed when automata change.
        // This is the current behavior - controllers depending on "over all" formulas
        // will be marked as changed when their source automaton changes.
        let source = r#"
context example {
    automata {
        automaton A {
            states { state S initial; }
            transitions { transition S -> S on label alpha; }
        }
        automaton B {
            states { state T initial; }
            transitions { transition T -> T on label beta; }
        }
    }
    mu_formulas {
        formula global { over all; body = true; }
    }
    controllers {
        controller C1 { source A; satisfying global; }
        controller C2 { source B; satisfying global; }
    }
}
"#;
        let doc = load_doc(source);
        let mut state = IncrementalState::new();
        let plan1 = state.diff(&doc);
        state.apply(&plan1);

        // Change automaton A
        let modified = r#"
context example {
    automata {
        automaton A {
            states { state S initial; state U; }
            transitions { 
                transition S -> S on label alpha;
                transition S -> U on label gamma;
            }
        }
        automaton B {
            states { state T initial; }
            transitions { transition T -> T on label beta; }
        }
    }
    mu_formulas {
        formula global { over all; body = true; }
    }
    controllers {
        controller C1 { source A; satisfying global; }
        controller C2 { source B; satisfying global; }
    }
}
"#;
        let doc2 = load_doc(modified);
        let plan2 = state.diff(&doc2);

        // Automaton A changed
        assert!(plan2.changed_automata.contains("A"));
        // Formula with "over all" has empty target list, so it's not automatically marked as changed
        // However, controller C1 depends on automaton A, so it should be marked as changed
        assert!(plan2.changed_controllers.contains("C1"));
        // Controller C2 depends on automaton B (unchanged), so it should not be marked as changed
        assert!(!plan2.changed_controllers.contains("C2"));
    }
}
