//! mununu#579 — does a finished report contradict **itself**?
//!
//! [`crate::adapter::slang::verify_auto::BottomReason::EngineContradiction`] already catches two
//! *engines* returning opposite definite verdicts on one property. It cannot catch one engine
//! returning verdicts that are mutually unsatisfiable across *different* properties of the same
//! model — which is how mununu#577 reached a consumer. Nothing looked, and the consumer's report was
//! the detector.
//!
//! This module is the pure half of the check: given two property formulas and their verdicts, decide
//! whether they can both be right. It holds no report plumbing so the decision is testable at the
//! layer it is made (CLAUDE.md §14).
//!
//! # What counts as a contradiction, and why the list is short
//!
//! Only `Violated`-`Violated` pairs over **exactly negated** atoms:
//!
//! | verdict | means |
//! |---|---|
//! | `AG(P)` VIOLATED | some reachable state satisfies `¬P` |
//! | `EF(Q)` VIOLATED | **no** reachable state satisfies `Q` |
//!
//! With `Q ≡ ¬P` those are `∃s. ¬P(s)` and `∀s. ¬¬P(s)` — unsatisfiable together. Nothing weaker is
//! admitted, because the consequence of a hit is forcing two verdicts to `⊥`: a false alarm destroys
//! sound results, which is a worse failure than the one this exists to catch.
//!
//! # ⚠️ The shape mununu#577 actually reported is NOT a contradiction
//!
//! The issue proposed a broader rule — *"for `AG(x <= K)` VIOLATED, at least one `EF(x == v)` with
//! `v > K` must not be VIOLATED"* — and asserted of its own example that the verdicts "cannot all
//! hold". **They can.** Take the reported trio:
//!
//! ```text
//! AG(drop_q <= 1)  VIOLATED   =>  ∃s. drop_q(s) > 1
//! EF(drop_q == 2)  VIOLATED   =>  ∀s. drop_q(s) != 2
//! EF(drop_q == 3)  VIOLATED   =>  ∀s. drop_q(s) != 3
//! ```
//!
//! A model where `drop_q` reaches 7 and never 2 or 3 satisfies all three. The witnesses pin two
//! values out of the 1022 that `> 1` admits on a 10-bit register, so they refute nothing. The rule
//! would only become sound once the witnesses **exhaust** the range — 1022 properties here — which
//! no real report carries.
//!
//! So this module does **not** implement that rule, and the omission is deliberate rather than
//! incomplete: implementing it would force two verdicts to `⊥` on evidence that permits both. That
//! mununu#577's own pair is outside the sound fragment is the finding, not a gap — what was wrong
//! there was a verdict, not a pair of verdicts, and no report-internal check could have seen it.

use crate::mu_calculus::{Formula, ModalKind, Node};

/// The two canonical property shapes mununu's SVA translator emits, carrying their atom text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    /// `nu X. (P && [] X)` — `AG(P)`. VIOLATED means some reachable state satisfies `¬P`.
    Universal(String),
    /// `mu Z. (Q || <> Z)` — `EF(Q)`. VIOLATED means no reachable state satisfies `Q`.
    Existential(String),
}

/// Recognise `AG(atom)` / `EF(atom)` over a **single** atom, or `None`.
///
/// Deliberately strict. A compound body (`P && R`), a nested modality, or anything but these two
/// shapes returns `None` — the negation test below reasons about one comparison, and handing it a
/// conjunction would make a pair look exactly-negated when it is not.
pub(crate) fn classify_shape(f: &Formula) -> Option<Shape> {
    let atom_of = |id| match f.node(id) {
        Node::Predicate(p) => Some(p.clone()),
        _ => None,
    };
    let is_self_modal = |id, want: ModalKind| {
        matches!(
            f.node(id),
            Node::Modal { kind, target, .. }
                if *kind == want && matches!(f.node(*target), Node::Variable(_))
        )
    };
    match f.node(f.root()) {
        Node::Nu { body, .. } => match f.node(*body) {
            // `P && [] X`, either association.
            Node::And(l, r) => {
                if is_self_modal(*r, ModalKind::Box) {
                    atom_of(*l).map(Shape::Universal)
                } else if is_self_modal(*l, ModalKind::Box) {
                    atom_of(*r).map(Shape::Universal)
                } else {
                    None
                }
            }
            _ => None,
        },
        Node::Mu { body, .. } => match f.node(*body) {
            // `Q || <> Z`, either association.
            Node::Or(l, r) => {
                if is_self_modal(*r, ModalKind::Diamond) {
                    atom_of(*l).map(Shape::Existential)
                } else if is_self_modal(*l, ModalKind::Diamond) {
                    atom_of(*r).map(Shape::Existential)
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// One comparison atom: `<register> <op> <value>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cmp<'a> {
    reg: &'a str,
    op: &'a str,
    value: &'a str,
}

/// Split a normalised atom. mununu spells these with spaces around the operator (`cnt == 3`), which
/// is what `resolve_predicate_registers` and the seeding paths already rely on.
fn parse_cmp(atom: &str) -> Option<Cmp<'_>> {
    // Longest operators first: `<=` must not be read as `<`.
    for op in ["<=", ">=", "==", "!=", "<", ">"] {
        let pat = format!(" {op} ");
        if let Some(i) = atom.find(&pat) {
            let reg = atom[..i].trim();
            let value = atom[i + pat.len()..].trim();
            // One comparison only: a second operator means a compound this module must not judge.
            if reg.is_empty()
                || value.is_empty()
                || reg.contains(' ')
                || value.contains(' ')
                || reg.contains("&&")
                || reg.contains("||")
            {
                return None;
            }
            return Some(Cmp { reg, op, value });
        }
    }
    None
}

/// Is `b` exactly `¬a`? Same register, same value, complementary operator.
///
/// The complement table is total over the six operators mununu admits, so there is no "unknown
/// operator" case that could be mistaken for a negation. Values are compared as **text**: two
/// spellings of the same number (`2` vs `0x2`) are conservatively treated as different atoms, which
/// can only ever miss a contradiction, never invent one.
pub(crate) fn is_negation_pair(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (parse_cmp(a), parse_cmp(b)) else {
        return false;
    };
    if x.reg != y.reg || x.value != y.value {
        return false;
    }
    let complement = |op: &str| match op {
        "<=" => ">",
        ">" => "<=",
        ">=" => "<",
        "<" => ">=",
        "==" => "!=",
        "!=" => "==",
        _ => unreachable!("parse_cmp only yields the six operators above"),
    };
    complement(x.op) == y.op
}

/// Do these two shapes-plus-`Violated` verdicts contradict each other?
///
/// Both properties must have come back `Violated`; the caller checks that. Order-insensitive.
pub(crate) fn violated_pair_contradicts(a: &Shape, b: &Shape) -> bool {
    match (a, b) {
        (Shape::Universal(p), Shape::Existential(q))
        | (Shape::Existential(q), Shape::Universal(p)) => is_negation_pair(p, q),
        // Two universals, or two existentials, cannot contradict on their own: `AG(P)` and `AG(¬P)`
        // both VIOLATED just says the model reaches both `¬P` and `P`, and two unreachable targets
        // are jointly satisfiable in an empty-ish model.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> Formula {
        crate::mu_calculus::parser::parse(s).expect("formula parses")
    }

    #[test]
    fn recognises_the_two_shapes_the_translator_emits() {
        assert_eq!(
            classify_shape(&f("nu X. ((drop_q <= 1) && [] X)")),
            Some(Shape::Universal("drop_q <= 1".into()))
        );
        assert_eq!(
            classify_shape(&f("mu Z. ((drop_q == 2) || <> Z)")),
            Some(Shape::Existential("drop_q == 2".into()))
        );
    }

    /// A compound body must NOT be reduced to one atom — the negation test reasons about a single
    /// comparison, and `P && R` handed to it as `P` would make a non-negation look like one.
    #[test]
    fn refuses_a_compound_body_rather_than_taking_its_first_atom() {
        assert_eq!(
            classify_shape(&f("nu X. (((drop_q <= 1) && (count_q >= 256)) && [] X)")),
            None
        );
        assert_eq!(
            classify_shape(&f("nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)")),
            None
        );
    }

    #[test]
    fn the_operator_complement_table_is_exact() {
        for (a, b) in [
            ("cnt <= 1", "cnt > 1"),
            ("cnt > 1", "cnt <= 1"),
            ("cnt >= 4", "cnt < 4"),
            ("cnt == 2", "cnt != 2"),
        ] {
            assert!(is_negation_pair(a, b), "{a} vs {b} are exact negations");
        }
        // Same register, DIFFERENT value — not a negation.
        assert!(!is_negation_pair("cnt <= 1", "cnt > 2"));
        // Different register — never a negation.
        assert!(!is_negation_pair("cnt <= 1", "other > 1"));
        // `<=` must not be parsed as `<`.
        assert!(!is_negation_pair("cnt <= 1", "cnt >= 1"));
    }

    /// The pair that would be flagged: `AG(P)` violated beside `EF(¬P)` violated.
    #[test]
    fn a_universal_beside_the_refutation_of_its_own_negation_contradicts() {
        let ag = classify_shape(&f("nu X. ((cnt <= 1) && [] X)")).unwrap();
        let ef = classify_shape(&f("mu Z. ((cnt > 1) || <> Z)")).unwrap();
        assert!(violated_pair_contradicts(&ag, &ef));
        assert!(violated_pair_contradicts(&ef, &ag), "order-insensitive");
    }

    /// ⚠️ mununu#579's OWN example is not a contradiction, and this test is the record of that.
    ///
    /// `AG(drop_q <= 1)` violated says some reachable state has `drop_q > 1`; `EF(drop_q == 2)`
    /// violated says 2 is never reached. A model reaching 7 and never 2 satisfies both. The issue
    /// claimed they "cannot all hold"; they can, and flagging them would force two verdicts to ⊥ on
    /// evidence that permits both.
    #[test]
    fn the_577_shape_is_deliberately_not_flagged() {
        let ag = classify_shape(&f("nu X. ((drop_q <= 1) && [] X)")).unwrap();
        for witness in [
            "mu Z. ((drop_q == 2) || <> Z)",
            "mu Z. ((drop_q == 3) || <> Z)",
        ] {
            let ef = classify_shape(&f(witness)).unwrap();
            assert!(
                !violated_pair_contradicts(&ag, &ef),
                "a single value > K refutes nothing about a 10-bit register: {witness}"
            );
        }
    }

    #[test]
    fn two_universals_never_contradict_each_other() {
        let a = classify_shape(&f("nu X. ((cnt <= 1) && [] X)")).unwrap();
        let b = classify_shape(&f("nu X. ((cnt > 1) && [] X)")).unwrap();
        assert!(
            !violated_pair_contradicts(&a, &b),
            "both violated just means the model reaches both sides"
        );
    }
}
