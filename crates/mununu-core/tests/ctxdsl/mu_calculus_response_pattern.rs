//! Diagnostic tests for μ-calculus response pattern formula evaluation.
//!
//! This test suite diagnoses issues with the response pattern formula:
//! `nu X. ((!trigger || mu Y. (response || <> Y)) && [] X)`
//!
//! **Key Finding**: The formula must use `<>` (diamond) not `[]` (box) for "eventually".
//! - `[] Y` means "ALL next states satisfy Y" (too strong)
//! - `<> Y` means "SOME next state satisfies Y" (correct for "eventually")
//!
//! The formula should be unsatisfiable when:
//! - We're in a state where `trigger` is true
//! - `response` is unreachable from that state (considering only enabled transitions)

use bitvec::prelude::*;
use mununu_core::clts::Clts;
use mununu_core::mu_calculus::{Environment, evaluator::evaluate, parser};
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Test case: Executing state where Completed is unreachable
///
/// CLTS structure:
/// - States: Executing (initial), Completed
/// - Transitions: Executing -> Executing (loop), Completed is unreachable
/// - Formula: nu X. ((!Executing || mu Y. (Completed || [] Y)) && [] X)
/// - Expected: Executing should NOT satisfy the formula
#[test]
fn test_response_pattern_executing_unreachable_completed() -> TestResult {
    // Build CLTS
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Completed");

    let loop_label = builder.labels().intern(["loop"])?;
    builder.transition("Executing", &[loop_label], "Executing");

    // No transition to Completed - it's unreachable
    let clts = builder.build()?;

    // Set up environment with state predicates
    let mut env = Environment::new(clts.state_count());

    let mut executing_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    executing_bits.set(clts.state_id("Executing")?.index(), true);
    env = env.with_predicate("Executing", executing_bits);

    let mut completed_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    completed_bits.set(clts.state_id("Completed")?.index(), true);
    env = env.with_predicate("Completed", completed_bits);

    // Formula: nu X. ((!Executing || mu Y. (Completed || <> Y)) && [] X)
    // Note: We use <> (diamond) for "eventually", not [] (box)
    let formula = parser::parse("nu X. ((!Executing || mu Y. (Completed || <> Y)) && [] X)")?;

    // Evaluate
    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();
    let completed_idx = clts.state_id("Completed")?.index();

    println!("Formula evaluation result:");
    println!(
        "  Executing satisfies: {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );
    println!(
        "  Completed satisfies: {}",
        result.get(completed_idx).map(|b| *b).unwrap_or(false)
    );

    // Executing should NOT satisfy because Completed is unreachable
    assert!(
        !result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "Executing should NOT satisfy the formula when Completed is unreachable"
    );

    // Completed should satisfy (vacuously, since !Executing is true there)
    assert!(
        result.get(completed_idx).map(|b| *b).unwrap_or(false),
        "Completed should satisfy the formula (vacuously)"
    );

    Ok(())
}

/// Test case: Executing state where Completed IS reachable
///
/// CLTS structure:
/// - States: Executing (initial), Completed
/// - Transitions: Executing -> Completed, Executing -> Executing (loop)
/// - Formula: nu X. ((!Executing || mu Y. (Completed || [] Y)) && [] X)
/// - Expected: Executing SHOULD satisfy the formula (can reach Completed)
#[test]
fn test_response_pattern_executing_reachable_completed() -> TestResult {
    // Build CLTS
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Completed");

    let loop_label = builder.labels().intern(["loop"])?;
    let complete_label = builder.labels().intern(["complete"])?;

    builder.transition("Executing", &[loop_label], "Executing");
    builder.transition("Executing", &[complete_label], "Completed");

    let clts = builder.build()?;

    // Set up environment with state predicates
    let mut env = Environment::new(clts.state_count());

    let mut executing_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    executing_bits.set(clts.state_id("Executing")?.index(), true);
    env = env.with_predicate("Executing", executing_bits);

    let mut completed_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    completed_bits.set(clts.state_id("Completed")?.index(), true);
    env = env.with_predicate("Completed", completed_bits);

    // Formula: nu X. ((!Executing || mu Y. (Completed || <> Y)) && [] X)
    // Note: We use <> (diamond) for "eventually", not [] (box)
    let formula = parser::parse("nu X. ((!Executing || mu Y. (Completed || <> Y)) && [] X)")?;

    // Evaluate
    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();

    println!("Formula evaluation result:");
    println!(
        "  Executing satisfies: {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );

    // Executing SHOULD satisfy because Completed is reachable
    assert!(
        result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "Executing SHOULD satisfy the formula when Completed is reachable"
    );

    Ok(())
}

/// Test the inner fixpoint: mu Y. (Completed || [] Y)
///
/// This tests whether the least fixpoint computation is correct.
/// From Executing, if Completed is unreachable, this should be false.
#[test]
fn test_inner_fixpoint_unreachable() -> TestResult {
    // Build CLTS
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Completed");

    let loop_label = builder.labels().intern(["loop"])?;
    builder.transition("Executing", &[loop_label], "Executing");

    let clts = builder.build()?;

    // Set up environment
    let mut env = Environment::new(clts.state_count());

    let mut completed_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    completed_bits.set(clts.state_id("Completed")?.index(), true);
    env = env.with_predicate("Completed", completed_bits);

    // Formula: mu Y. (Completed || <> Y)
    // This should be false in Executing (Completed is unreachable)
    // Note: We use <> (diamond) for "eventually", not [] (box)
    let formula = parser::parse("mu Y. (Completed || <> Y)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();

    println!("Inner fixpoint evaluation:");
    println!(
        "  Executing satisfies mu Y. (Completed || [] Y): {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );

    // Should be false in Executing
    assert!(
        !result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "mu Y. (Completed || [] Y) should be false in Executing when Completed is unreachable"
    );

    Ok(())
}

/// Test the inner fixpoint: mu Y. (Completed || <> Y) when Completed IS reachable
///
/// NOTE: The correct formula for "eventually Completed" should use diamond (<>), not box ([]).
/// The formula mu Y. (Completed || [] Y) is too strong - it requires ALL paths to eventually reach Completed.
#[test]
fn test_inner_fixpoint_reachable_diamond() -> TestResult {
    // Build CLTS
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Completed");

    let loop_label = builder.labels().intern(["loop"])?;
    let complete_label = builder.labels().intern(["complete"])?;

    builder.transition("Executing", &[loop_label], "Executing");
    builder.transition("Executing", &[complete_label], "Completed");

    let clts = builder.build()?;

    // Set up environment
    let mut env = Environment::new(clts.state_count());

    let mut completed_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    completed_bits.set(clts.state_id("Completed")?.index(), true);
    env = env.with_predicate("Completed", completed_bits);

    // Formula: mu Y. (Completed || <> Y)
    // This should be true in Executing (can reach Completed)
    let formula = parser::parse("mu Y. (Completed || <> Y)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();

    println!("Inner fixpoint evaluation (diamond):");
    println!(
        "  Executing satisfies mu Y. (Completed || <> Y): {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );

    // Should be true in Executing (can reach Completed)
    assert!(
        result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "mu Y. (Completed || <> Y) should be true in Executing when Completed is reachable"
    );

    Ok(())
}

/// Test the inner fixpoint: mu Y. (Completed || [] Y) when Completed IS reachable
///
/// This test demonstrates why [] (box) is incorrect: it requires ALL paths, which is too strong.
/// The correct formula uses <> (diamond) for "eventually".
#[test]
fn test_inner_fixpoint_reachable_box_incorrect() -> TestResult {
    // Build CLTS
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Completed");

    let loop_label = builder.labels().intern(["loop"])?;
    let complete_label = builder.labels().intern(["complete"])?;

    builder.transition("Executing", &[loop_label], "Executing");
    builder.transition("Executing", &[complete_label], "Completed");

    let clts = builder.build()?;

    // Set up environment
    let mut env = Environment::new(clts.state_count());

    let mut completed_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    completed_bits.set(clts.state_id("Completed")?.index(), true);
    env = env.with_predicate("Completed", completed_bits);

    // Formula: mu Y. (Completed || [] Y)
    // This is INCORRECT - [] requires ALL paths, which is too strong
    let formula = parser::parse("mu Y. (Completed || [] Y)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();

    println!("Inner fixpoint evaluation (box - incorrect):");
    println!(
        "  Executing satisfies mu Y. (Completed || [] Y): {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );

    // This will be false because [] requires ALL paths to satisfy Y
    // From Executing, we have a loop back to Executing, so not all paths reach Completed
    // This demonstrates why [] is incorrect for "eventually"
    assert!(
        !result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "mu Y. (Completed || [] Y) is INCORRECT - it requires ALL paths, which is too strong. Use <> instead."
    );

    Ok(())
}

/// Test the box modality [] Y behavior
///
/// Tests whether [] Y correctly requires Y to hold in all next states.
#[test]
fn test_box_modality_all_next_states() -> TestResult {
    // Build CLTS with two paths from Executing
    let mut builder = Clts::builder();
    builder.state("Executing").initial("Executing");
    builder.state("Path1");
    builder.state("Path2");
    builder.state("Target");

    let a_label = builder.labels().intern(["a"])?;
    let b_label = builder.labels().intern(["b"])?;

    builder.transition("Executing", &[a_label], "Path1");
    builder.transition("Executing", &[b_label], "Path2");
    builder.transition("Path1", &[a_label], "Target");
    builder.transition("Path2", &[b_label], "Target");

    let clts = builder.build()?;

    // Set up environment
    let mut env = Environment::new(clts.state_count());

    let mut target_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    target_bits.set(clts.state_id("Target")?.index(), true);
    env = env.with_predicate("Target", target_bits);

    // Formula: [] Target
    // This should be false in Executing (not all next states are Target)
    let formula = parser::parse("[] Target")?;

    let result = evaluate(&formula, &clts, &env)?;

    let executing_idx = clts.state_id("Executing")?.index();

    println!("Box modality evaluation:");
    println!(
        "  Executing satisfies [] Target: {}",
        result.get(executing_idx).map(|b| *b).unwrap_or(false)
    );

    // Should be false (Path1 and Path2 are not Target)
    assert!(
        !result.get(executing_idx).map(|b| *b).unwrap_or(false),
        "[] Target should be false in Executing (not all next states are Target)"
    );

    Ok(())
}

/// Test fixpoint issue: mu Y. (Target || <> Y) with self-loop when Target is unreachable
///
/// This test reproduces the issue where a least fixpoint incorrectly evaluates to true
/// when the target state is unreachable but there's a self-loop.
///
/// CLTS structure:
/// - States: Start (initial), Target
/// - Transitions: Start -> Start (self-loop only)
/// - Target is unreachable (no transition to it)
/// - Formula: mu Y. (Target || <> Y)
/// - Expected: false in Start (Target is unreachable)
///
/// This test should FAIL to demonstrate the fixpoint computation bug.
#[test]
fn test_fixpoint_self_loop_unreachable_target() -> TestResult {
    // Build CLTS with self-loop but no path to Target
    let mut builder = Clts::builder();
    builder.state("Start").initial("Start");
    builder.state("Target");

    let loop_label = builder.labels().intern(["loop"])?;

    // Only self-loop, no transition to Target
    builder.transition("Start", &[loop_label], "Start");

    let clts = builder.build()?;

    // Set up environment
    let mut env = Environment::new(clts.state_count());

    let mut target_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    target_bits.set(clts.state_id("Target")?.index(), true);
    env = env.with_predicate("Target", target_bits);

    // Formula: mu Y. (Target || <> Y)
    // This should be false in Start because Target is unreachable
    let formula = parser::parse("mu Y. (Target || <> Y)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let start_idx = clts.state_id("Start")?.index();
    let target_idx = clts.state_id("Target")?.index();

    println!("Fixpoint evaluation with self-loop and unreachable target:");
    println!(
        "  Start satisfies mu Y. (Target || <> Y): {}",
        result.get(start_idx).map(|b| *b).unwrap_or(false)
    );
    println!(
        "  Target satisfies mu Y. (Target || <> Y): {}",
        result.get(target_idx).map(|b| *b).unwrap_or(false)
    );

    // This should be FALSE in Start (Target is unreachable)
    // If this assertion fails, it demonstrates the fixpoint bug
    assert!(
        !result.get(start_idx).map(|b| *b).unwrap_or(false),
        "BUG: mu Y. (Target || <> Y) should be FALSE in Start when Target is unreachable, but it's TRUE. \
         This indicates a bug in the fixpoint computation with self-loops."
    );

    // Target itself should satisfy (vacuously, since Target is true there)
    assert!(
        result.get(target_idx).map(|b| *b).unwrap_or(false),
        "Target should satisfy mu Y. (Target || <> Y) (vacuously, since Target is true)"
    );

    Ok(())
}

/// Test fixpoint issue: Full response pattern with self-loop when target is unreachable
///
/// This test reproduces the issue seen in the BPM integration test where the full
/// response pattern formula incorrectly evaluates to true when the target is unreachable.
///
/// CLTS structure:
/// - States: Trigger (initial), Response
/// - Transitions: Trigger -> Trigger (self-loop only)
/// - Response is unreachable (no transition to it)
/// - Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
/// - Expected: false in Trigger (Response is unreachable)
///
/// This test should FAIL to demonstrate the response pattern evaluation bug.
#[test]
fn test_response_pattern_self_loop_unreachable_response() -> TestResult {
    // Build CLTS with self-loop but no path to Response
    let mut builder = Clts::builder();
    builder.state("Trigger").initial("Trigger");
    builder.state("Response");

    let loop_label = builder.labels().intern(["loop"])?;

    // Only self-loop, no transition to Response
    builder.transition("Trigger", &[loop_label], "Trigger");

    let clts = builder.build()?;

    // Set up environment with state predicates
    let mut env = Environment::new(clts.state_count());

    let mut trigger_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    trigger_bits.set(clts.state_id("Trigger")?.index(), true);
    env = env.with_predicate("Trigger", trigger_bits);

    let mut response_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    response_bits.set(clts.state_id("Response")?.index(), true);
    env = env.with_predicate("Response", response_bits);

    // Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
    // This should be false in Trigger because Response is unreachable
    let formula = parser::parse("nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let trigger_idx = clts.state_id("Trigger")?.index();
    let response_idx = clts.state_id("Response")?.index();

    println!("Full response pattern evaluation with self-loop:");
    println!(
        "  Trigger satisfies: {}",
        result.get(trigger_idx).map(|b| *b).unwrap_or(false)
    );
    println!(
        "  Response satisfies: {}",
        result.get(response_idx).map(|b| *b).unwrap_or(false)
    );

    // This should be FALSE in Trigger (Response is unreachable)
    // If this assertion fails, it demonstrates the response pattern bug
    assert!(
        !result.get(trigger_idx).map(|b| *b).unwrap_or(false),
        "BUG: Response pattern should be FALSE in Trigger when Response is unreachable, but it's TRUE. \
         This indicates a bug in the greatest fixpoint computation with self-loops and unreachable targets."
    );

    // Response should satisfy (vacuously, since !Trigger is true there)
    assert!(
        result.get(response_idx).map(|b| *b).unwrap_or(false),
        "Response should satisfy the formula (vacuously, since !Trigger is true there)"
    );

    Ok(())
}

/// Test fixpoint issue: Response pattern with uncontrollable self-loop when target is unreachable
///
/// This test mimics the BPM scenario more closely by using uncontrollable transitions,
/// which might affect how the Skolem paradigm groups transitions and evaluates the formula.
///
/// CLTS structure:
/// - States: Trigger (initial), Response
/// - Transitions: Trigger -> Trigger (uncontrollable self-loop)
/// - Response is unreachable (no transition to it)
/// - Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
/// - Expected: false in Trigger (Response is unreachable)
///
/// This test should FAIL if the issue is related to uncontrollable transitions.
#[test]
fn test_response_pattern_uncontrollable_self_loop_unreachable() -> TestResult {
    // Build CLTS with uncontrollable self-loop but no path to Response
    let mut builder = Clts::builder();
    builder.state("Trigger").initial("Trigger");
    builder.state("Response");

    let loop_label = builder.labels().intern(["loop"])?;

    // Uncontrollable self-loop, no transition to Response
    builder.transition("Trigger", &[loop_label], "Trigger");

    let clts = builder.build()?;

    // Set up environment with state predicates
    let mut env = Environment::new(clts.state_count());

    let mut trigger_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    trigger_bits.set(clts.state_id("Trigger")?.index(), true);
    env = env.with_predicate("Trigger", trigger_bits);

    let mut response_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    response_bits.set(clts.state_id("Response")?.index(), true);
    env = env.with_predicate("Response", response_bits);

    // Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
    let formula = parser::parse("nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let trigger_idx = clts.state_id("Trigger")?.index();
    let response_idx = clts.state_id("Response")?.index();

    println!("Response pattern with uncontrollable self-loop:");
    println!(
        "  Trigger satisfies: {}",
        result.get(trigger_idx).map(|b| *b).unwrap_or(false)
    );
    println!(
        "  Response satisfies: {}",
        result.get(response_idx).map(|b| *b).unwrap_or(false)
    );

    // This should be FALSE in Trigger (Response is unreachable)
    assert!(
        !result.get(trigger_idx).map(|b| *b).unwrap_or(false),
        "BUG: Response pattern should be FALSE in Trigger when Response is unreachable (uncontrollable self-loop case)"
    );

    Ok(())
}

/// Test fixpoint issue: Response pattern with mixed controllable/uncontrollable transitions
///
/// This test mimics a scenario where there are both controllable and uncontrollable transitions,
/// which might affect how the Skolem paradigm groups transitions.
///
/// CLTS structure:
/// - States: Trigger (initial), Response
/// - Transitions:
///   - Trigger -> Trigger (controllable self-loop)
///   - Trigger -> Trigger (uncontrollable self-loop, different label)
/// - Response is unreachable (no transition to it)
/// - Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
/// - Expected: false in Trigger (Response is unreachable)
#[test]
fn test_response_pattern_mixed_transitions_unreachable() -> TestResult {
    // Build CLTS with both controllable and uncontrollable self-loops
    let mut builder = Clts::builder();
    builder.state("Trigger").initial("Trigger");
    builder.state("Response");

    let ctrl_label = builder.labels().intern(["ctrl_loop"])?;
    let unctrl_label = builder.labels().intern(["unctrl_loop"])?;

    // Both controllable and uncontrollable self-loops, no transition to Response
    builder.transition("Trigger", &[ctrl_label], "Trigger");
    builder.transition("Trigger", &[unctrl_label], "Trigger");
    builder.transition("Trigger", &[unctrl_label], "Trigger");

    let clts = builder.build()?;

    // Set up environment with state predicates
    let mut env = Environment::new(clts.state_count());

    let mut trigger_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    trigger_bits.set(clts.state_id("Trigger")?.index(), true);
    env = env.with_predicate("Trigger", trigger_bits);

    let mut response_bits = BitVec::<usize, Lsb0>::repeat(false, clts.state_count());
    response_bits.set(clts.state_id("Response")?.index(), true);
    env = env.with_predicate("Response", response_bits);

    // Formula: nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)
    let formula = parser::parse("nu X. ((!Trigger || mu Y. (Response || <> Y)) && [] X)")?;

    let result = evaluate(&formula, &clts, &env)?;

    let trigger_idx = clts.state_id("Trigger")?.index();

    println!("Response pattern with mixed transitions:");
    println!(
        "  Trigger satisfies: {}",
        result.get(trigger_idx).map(|b| *b).unwrap_or(false)
    );

    // This should be FALSE in Trigger (Response is unreachable)
    assert!(
        !result.get(trigger_idx).map(|b| *b).unwrap_or(false),
        "BUG: Response pattern should be FALSE in Trigger when Response is unreachable (mixed transitions case)"
    );

    Ok(())
}
