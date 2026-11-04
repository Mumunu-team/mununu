//! Comprehensive tests for state variable abstraction.

use mununu_core::abstraction::constraint::Constraint;
use mununu_core::abstraction::domains::{BoolDomain, IntDomain};
use mununu_core::abstraction::evaluator::{EvaluationError, evaluate_expr, evaluate_guard};
use mununu_core::abstraction::expression::{Expr, GuardExpr, GuardResult};
use mununu_core::abstraction::state::AbstractState;
use mununu_core::abstraction::value::AbstractValue;
use mununu_core::guard::ComparisonOp;
use std::collections::HashMap;

#[test]
fn test_bool_domain_all_operations() {
    // Test all boolean operations
    assert_eq!(!BoolDomain::True, BoolDomain::False);
    assert_eq!(!BoolDomain::False, BoolDomain::True);
    assert_eq!(!BoolDomain::Unknown, BoolDomain::Unknown);

    // AND
    assert_eq!(BoolDomain::True.and(BoolDomain::True), BoolDomain::True);
    assert_eq!(BoolDomain::True.and(BoolDomain::False), BoolDomain::False);
    assert_eq!(BoolDomain::False.and(BoolDomain::False), BoolDomain::False);
    assert_eq!(
        BoolDomain::True.and(BoolDomain::Unknown),
        BoolDomain::Unknown
    );
    assert_eq!(
        BoolDomain::False.and(BoolDomain::Unknown),
        BoolDomain::False
    );

    // OR
    assert_eq!(BoolDomain::True.or(BoolDomain::True), BoolDomain::True);
    assert_eq!(BoolDomain::True.or(BoolDomain::False), BoolDomain::True);
    assert_eq!(BoolDomain::False.or(BoolDomain::False), BoolDomain::False);
    assert_eq!(BoolDomain::True.or(BoolDomain::Unknown), BoolDomain::True);
    assert_eq!(
        BoolDomain::False.or(BoolDomain::Unknown),
        BoolDomain::Unknown
    );

    // XOR
    assert_eq!(BoolDomain::True.xor(BoolDomain::False), BoolDomain::True);
    assert_eq!(BoolDomain::True.xor(BoolDomain::True), BoolDomain::False);
    assert_eq!(BoolDomain::False.xor(BoolDomain::False), BoolDomain::False);
    assert_eq!(
        BoolDomain::True.xor(BoolDomain::Unknown),
        BoolDomain::Unknown
    );

    // Implies
    assert_eq!(BoolDomain::True.implies(BoolDomain::True), BoolDomain::True);
    assert_eq!(
        BoolDomain::True.implies(BoolDomain::False),
        BoolDomain::False
    );
    assert_eq!(
        BoolDomain::False.implies(BoolDomain::True),
        BoolDomain::True
    );
    assert_eq!(
        BoolDomain::False.implies(BoolDomain::False),
        BoolDomain::True
    );
    assert_eq!(
        BoolDomain::True.implies(BoolDomain::Unknown),
        BoolDomain::Unknown
    );
}

#[test]
fn test_int_domain_arithmetic_comprehensive() {
    // Addition
    let a = IntDomain::interval(Some(0), Some(5));
    let b = IntDomain::interval(Some(10), Some(20));
    let sum = a.add(b);
    assert_eq!(sum.lower(), Some(10));
    assert_eq!(sum.upper(), Some(25));

    // Subtraction
    let diff = b.sub(a);
    assert_eq!(diff.lower(), Some(5));
    assert_eq!(diff.upper(), Some(20));

    // Multiplication - both positive
    let prod = a.mul(b);
    assert_eq!(prod.lower(), Some(0));
    assert_eq!(prod.upper(), Some(100));

    // Multiplication - mixed signs
    let neg = IntDomain::interval(Some(-5), Some(-1));
    let pos = IntDomain::interval(Some(2), Some(4));
    let mixed = neg.mul(pos);
    assert_eq!(mixed.lower(), Some(-20));
    assert_eq!(mixed.upper(), Some(-2));

    // Division
    let dividend = IntDomain::interval(Some(0), Some(10));
    let divisor = IntDomain::interval(Some(2), Some(5));
    let quotient = dividend.div(divisor).unwrap();
    assert_eq!(quotient.lower(), Some(0));
    assert_eq!(quotient.upper(), Some(5));
}

#[test]
fn test_int_domain_comparisons() {
    // Test interval comparisons
    let small = IntDomain::interval(Some(0), Some(5));
    let large = IntDomain::interval(Some(10), Some(20));

    // small < large should be always true
    assert!(small.upper().unwrap() < large.lower().unwrap());

    // small > large should be always false
    assert!(small.upper().unwrap() < large.lower().unwrap());
}

#[test]
fn test_abstract_state_operations() {
    let mut state = AbstractState::new("Executing");
    state.set_variable("x", AbstractValue::int_constant(5));
    state.set_variable("y", AbstractValue::bool_constant(true));
    state.set_variable("count", AbstractValue::int_interval(0, 10));

    assert_eq!(
        state.get_variable("x"),
        Some(&AbstractValue::int_constant(5))
    );
    assert_eq!(
        state.get_variable("y"),
        Some(&AbstractValue::bool_constant(true))
    );
    assert!(state.get_variable("count").is_some());

    let name = state.state_name();
    assert!(name.contains("Executing"));
    assert!(name.contains("x_5"));
    assert!(name.contains("y_true"));
}

#[test]
fn test_evaluate_complex_expressions() {
    let mut state = AbstractState::new("Test");
    state.set_variable("x", AbstractValue::int_constant(5));
    state.set_variable("y", AbstractValue::int_constant(10));

    // x + y
    let expr = Expr::Add(Box::new(Expr::var("x")), Box::new(Expr::var("y")));
    let result = evaluate_expr(&expr, &state).unwrap();
    assert_eq!(result.as_int_constant(), Some(15));

    // x * y
    let expr = Expr::Mul(Box::new(Expr::var("x")), Box::new(Expr::var("y")));
    let result = evaluate_expr(&expr, &state).unwrap();
    assert_eq!(result.as_int_constant(), Some(50));

    // (x + y) - x
    let expr = Expr::Sub(
        Box::new(Expr::Add(
            Box::new(Expr::var("x")),
            Box::new(Expr::var("y")),
        )),
        Box::new(Expr::var("x")),
    );
    let result = evaluate_expr(&expr, &state).unwrap();
    assert_eq!(result.as_int_constant(), Some(10));
}

#[test]
fn test_evaluate_guard_combinations() {
    let mut state = AbstractState::new("Test");
    state.set_variable("x", AbstractValue::int_constant(5));
    state.set_variable("y", AbstractValue::int_constant(10));

    let predicates = HashMap::new();

    // x > 3 && y < 15
    let guard = GuardExpr::And(
        Box::new(GuardExpr::comparison(
            Expr::var("x"),
            ComparisonOp::Gt,
            Expr::constant(3),
        )),
        Box::new(GuardExpr::comparison(
            Expr::var("y"),
            ComparisonOp::Lt,
            Expr::constant(15),
        )),
    );
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::AlwaysTrue);

    // x > 10 || y < 5
    let guard = GuardExpr::Or(
        Box::new(GuardExpr::comparison(
            Expr::var("x"),
            ComparisonOp::Gt,
            Expr::constant(10),
        )),
        Box::new(GuardExpr::comparison(
            Expr::var("y"),
            ComparisonOp::Lt,
            Expr::constant(5),
        )),
    );
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::AlwaysFalse);

    // !(x > 10)
    let guard = GuardExpr::Not(Box::new(GuardExpr::comparison(
        Expr::var("x"),
        ComparisonOp::Gt,
        Expr::constant(10),
    )));
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::AlwaysTrue);
}

#[test]
fn test_evaluate_guard_with_intervals() {
    let mut state = AbstractState::new("Test");
    state.set_variable("x", AbstractValue::int_interval(0, 10));

    let predicates = HashMap::new();

    // x > 15 (always false)
    let guard = GuardExpr::comparison(Expr::var("x"), ComparisonOp::Gt, Expr::constant(15));
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::AlwaysFalse);

    // x > -5 (always true)
    let guard = GuardExpr::comparison(Expr::var("x"), ComparisonOp::Gt, Expr::constant(-5));
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::AlwaysTrue);

    // x > 5 (maybe)
    let guard = GuardExpr::comparison(Expr::var("x"), ComparisonOp::Gt, Expr::constant(5));
    let result = evaluate_guard(&guard, &state, &predicates).unwrap();
    assert_eq!(result, GuardResult::Maybe);
}

#[test]
fn test_constraint_creation() {
    let constraint = Constraint::var_const("x".to_string(), ComparisonOp::Gt, 5);
    assert!(matches!(
        constraint.kind,
        mununu_core::abstraction::constraint::ConstraintKind::VarConst { .. }
    ));

    let constraint = Constraint::var_var("x".to_string(), ComparisonOp::Lt, "y".to_string());
    assert!(matches!(
        constraint.kind,
        mununu_core::abstraction::constraint::ConstraintKind::VarVar { .. }
    ));
}

#[test]
fn test_division_by_zero() {
    let mut state = AbstractState::new("Test");
    state.set_variable("x", AbstractValue::int_constant(10));
    state.set_variable("y", AbstractValue::int_constant(0));

    let expr = Expr::Div(Box::new(Expr::var("x")), Box::new(Expr::var("y")));
    let result = evaluate_expr(&expr, &state);
    assert!(matches!(result, Err(EvaluationError::DivisionByZero)));
}

#[test]
fn test_unknown_variable() {
    let state = AbstractState::new("Test");

    let expr = Expr::var("nonexistent");
    let result = evaluate_expr(&expr, &state);
    assert!(matches!(result, Err(EvaluationError::UnknownVariable(_))));
}
