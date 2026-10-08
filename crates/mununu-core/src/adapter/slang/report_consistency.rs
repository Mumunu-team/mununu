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
//!
//! # mununu#599 — the HOLDS side, and a guarantee judged against its own witness
//!
//! The `Violated`-`Violated` pair above has a mirror on the `Holds` side, and the recoverability
//! shape `AG EF(P)` (`nu Y. ((mu X. (P || <> X)) && [] Y)`) joins the table. With `P'` the same
//! atom and `Q ≡ ¬P`, every row is unsatisfiable over one model with a non-empty initial set:
//!
//! | a | b | why they cannot both be right |
//! |---|---|---|
//! | `EF(P)` HOLDS | `AG(Q)` HOLDS | `∃s. P(s)` against `∀s. ¬P(s)` |
//! | `AG EF(P)` HOLDS | `EF(P')` VIOLATED | the initial state is reachable, so `EF P` holds there |
//! | `AG EF(P)` HOLDS | `AG(Q)` HOLDS | `EF P` at the initial state against `P` nowhere |
//! | `AG EF(P)` VIOLATED | `AG(P')` HOLDS | `P` invariant makes `EF P` true everywhere |
//! | `AG EF(P)` VIOLATED | `EF(Q)` VIOLATED | `¬P` unreachable is `AG P`, the row above |
//!
//! A definite verdict transfers to the concrete model whatever engine produced it (CLAUDE.md
//! §Soundness Guarantees), so two definite verdicts that cannot share a model expose a broken one
//! — and, as before, not which. Both are withheld.
//!
//! **Vacuity is a different relation and does not touch the verdict.** `AG EF(P)` HOLDS beside
//! `EF(¬P)` VIOLATED (or `AG(P)` HOLDS) is *consistent*: the design never leaves `P`, so "always
//! recoverable to `P`" is true and says nothing about recovery. That is the vacuous pass the
//! two-sided gate exists to prevent, and mununu#599's ask is that it be said **at the guarantee**
//! rather than left for a reader who checks witnesses by habit. [`vacuity_of_guarantee`] is that
//! relation; it also names the weaker case the issue actually met — a same-register reachability
//! (`EF(st_q == S_WAIT)` beside `AG EF(st_q == S_IDLE)`) refuted in the same report — which proves
//! nothing about the guarantee's truth but does mean this report has not shown it non-vacuous.

use crate::mu_calculus::{Formula, ModalKind, Node};

/// The two canonical property shapes mununu's SVA translator emits, carrying their atom text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    /// `nu X. (P && [] X)` — `AG(P)`. VIOLATED means some reachable state satisfies `¬P`.
    Universal(String),
    /// `mu Z. (Q || <> Z)` — `EF(Q)`. VIOLATED means no reachable state satisfies `Q`.
    Existential(String),
    /// `nu Y. ((mu X. (P || <> X)) && [] Y)` — `AG EF(P)`, recoverability. HOLDS means every
    /// reachable state can reach `P`; VIOLATED means some reachable state cannot.
    Recoverability(String),
}

impl Shape {
    /// The single comparison atom the shape is over.
    pub(crate) fn atom(&self) -> &str {
        match self {
            Shape::Universal(a) | Shape::Existential(a) | Shape::Recoverability(a) => a,
        }
    }
}

/// A definite verdict, as the pair relations below read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Definite {
    Holds,
    Violated,
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
    // `mu X. (P || <> X)` with a single atom `P` → that atom. The inner shape of `AG EF`.
    let reach_atom = |id| match f.node(id) {
        Node::Mu { body, .. } => match f.node(*body) {
            Node::Or(l, r) => {
                if is_self_modal(*r, ModalKind::Diamond) {
                    atom_of(*l)
                } else if is_self_modal(*l, ModalKind::Diamond) {
                    atom_of(*r)
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    };
    match f.node(f.root()) {
        Node::Nu { body, .. } => match f.node(*body) {
            // `P && [] X` (universal) or `(mu X. (P || <> X)) && [] Y` (recoverability), either
            // association.
            Node::And(l, r) => {
                let (inner, modal) = if is_self_modal(*r, ModalKind::Box) {
                    (*l, true)
                } else if is_self_modal(*l, ModalKind::Box) {
                    (*r, true)
                } else {
                    (*l, false)
                };
                if !modal {
                    return None;
                }
                atom_of(inner)
                    .map(Shape::Universal)
                    .or_else(|| reach_atom(inner).map(Shape::Recoverability))
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

/// Are `a` and `b` single comparisons over the SAME register (whatever the operator or value)?
pub(crate) fn same_register(a: &str, b: &str) -> bool {
    matches!((parse_cmp(a), parse_cmp(b)), (Some(x), Some(y)) if x.reg == y.reg)
}

/// Do these two shapes, with these definite verdicts, contradict each other? Order-insensitive.
///
/// The admitted rows are the table in the module doc; everything else is `false`, including every
/// same-direction pair (`AG(P)` and `AG(¬P)` both VIOLATED just says the model reaches both sides;
/// two unreachable targets are jointly satisfiable) and every pair whose atoms are not exactly the
/// same or exactly negated.
pub(crate) fn definite_pair_contradicts(a: &Shape, va: Definite, b: &Shape, vb: Definite) -> bool {
    use Definite::{Holds, Violated};
    use Shape::{Existential, Recoverability, Universal};
    let row = |a: &Shape, va: Definite, b: &Shape, vb: Definite| -> bool {
        match (a, va, b, vb) {
            // ∃s.¬P against ∀s.¬¬P (mununu#579).
            (Universal(p), Violated, Existential(q), Violated) => is_negation_pair(p, q),
            // ∃s.P against ∀s.¬P.
            (Existential(p), Holds, Universal(q), Holds) => is_negation_pair(p, q),
            // `EF P` holds at the (reachable) initial state against `EF P` false there.
            (Recoverability(p), Holds, Existential(q), Violated) => p == q,
            // `EF P` at the initial state against `P` nowhere.
            (Recoverability(p), Holds, Universal(q), Holds) => is_negation_pair(p, q),
            // Some reachable state cannot reach `P` against `P` invariant.
            (Recoverability(p), Violated, Universal(q), Holds) => p == q,
            // … and the same invariant spelled as `¬P` unreachable.
            (Recoverability(p), Violated, Existential(q), Violated) => is_negation_pair(p, q),
            _ => false,
        }
    };
    row(a, va, b, vb) || row(b, vb, a, va)
}

/// mununu#599 — how a recoverability guarantee that HOLDS stands against another property in the
/// same report. `None` when the other property says nothing about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vacuity {
    /// The target is INVARIANT in this report — `EF(¬P)` VIOLATED or `AG(P)` HOLDS beside
    /// `AG EF(P)` HOLDS. Consistent, and vacuous: the design never leaves `P`, so the guarantee
    /// holds without any recovery ever being exercised.
    InvariantTarget,
    /// A reachability over the guarantee's REGISTER — a different value, not the negation — is
    /// VIOLATED in this report. It proves nothing about the guarantee's truth (`P` may be left and
    /// re-entered without ever visiting that value), but the property an author lists next to a
    /// recoverability guarantee as its non-vacuity witness has this shape, and a refuted witness
    /// means this report has not shown the guarantee non-vacuous.
    WitnessRefuted,
}

/// mununu#599 — judge a `guarantee` of shape [`Shape::Recoverability`] that HOLDS against one
/// `other` property with its definite verdict. The caller supplies only guarantees that HOLD and
/// only partners with a definite verdict.
pub(crate) fn vacuity_of_guarantee(
    guarantee: &Shape,
    other: &Shape,
    vo: Definite,
) -> Option<Vacuity> {
    let Shape::Recoverability(p) = guarantee else {
        return None;
    };
    match (other, vo) {
        (Shape::Existential(q), Definite::Violated) if is_negation_pair(p, q) => {
            Some(Vacuity::InvariantTarget)
        }
        (Shape::Universal(q), Definite::Holds) if p == q => Some(Vacuity::InvariantTarget),
        (Shape::Existential(q), Definite::Violated) if same_register(p, q) => {
            Some(Vacuity::WitnessRefuted)
        }
        _ => None,
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
        // A nested fixpoint is not a universal over its inner atom — since mununu#599 it is the
        // recoverability shape, judged by its own rows; a universal over a compound stays `None`.
        assert_eq!(
            classify_shape(&f("nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)")),
            Some(Shape::Recoverability("st_q == 0".into()))
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
        use Definite::Violated;
        let ag = classify_shape(&f("nu X. ((cnt <= 1) && [] X)")).unwrap();
        let ef = classify_shape(&f("mu Z. ((cnt > 1) || <> Z)")).unwrap();
        assert!(definite_pair_contradicts(&ag, Violated, &ef, Violated));
        assert!(
            definite_pair_contradicts(&ef, Violated, &ag, Violated),
            "order-insensitive"
        );
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
                !definite_pair_contradicts(&ag, Definite::Violated, &ef, Definite::Violated),
                "a single value > K refutes nothing about a 10-bit register: {witness}"
            );
        }
    }

    /// mununu#599 — the recoverability shape, either association of both connectives.
    #[test]
    fn recognises_the_recoverability_shape() {
        assert_eq!(
            classify_shape(&f("nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)")),
            Some(Shape::Recoverability("st_q == 0".into()))
        );
        assert_eq!(
            classify_shape(&f("nu Y.([] Y && (mu X.(<> X || (st_q == 0))))")),
            Some(Shape::Recoverability("st_q == 0".into()))
        );
        // `AG AF` is not recoverability, and a compound target is refused like everywhere else.
        assert_eq!(
            classify_shape(&f("nu Y.((mu X.((st_q == 0) || [] X)) && [] Y)")),
            None
        );
        assert_eq!(
            classify_shape(&f(
                "nu Y.((mu X.(((st_q == 0) && (en == 1)) || <> X)) && [] Y)"
            )),
            None
        );
    }

    /// mununu#599 — every row of the module-doc table, both orders; and the rows that look like
    /// contradictions but are not.
    #[test]
    fn the_definite_pair_table_is_exact() {
        use Definite::{Holds, Violated};
        let sh = |s: &str| classify_shape(&f(s)).unwrap();
        let ef_p = sh("mu Z. ((cnt == 2) || <> Z)");
        let ef_not_p = sh("mu Z. ((cnt != 2) || <> Z)");
        let ag_p = sh("nu X. ((cnt == 2) && [] X)");
        let ag_not_p = sh("nu X. ((cnt != 2) && [] X)");
        let agef_p = sh("nu Y.((mu X.((cnt == 2) || <> X)) && [] Y)");
        let rows: [(&Shape, Definite, &Shape, Definite); 6] = [
            (&ag_p, Violated, &ef_not_p, Violated),
            (&ef_p, Holds, &ag_not_p, Holds),
            (&agef_p, Holds, &ef_p, Violated),
            (&agef_p, Holds, &ag_not_p, Holds),
            (&agef_p, Violated, &ag_p, Holds),
            (&agef_p, Violated, &ef_not_p, Violated),
        ];
        for (a, va, b, vb) in rows {
            assert!(
                definite_pair_contradicts(a, va, b, vb),
                "{a:?} {va:?} vs {b:?} {vb:?}"
            );
            assert!(definite_pair_contradicts(b, vb, a, va), "order-insensitive");
        }
        // Consistent pairs that a looser rule would flag.
        let fine: [(&Shape, Definite, &Shape, Definite); 5] = [
            // the vacuity relation, not a contradiction
            (&agef_p, Holds, &ef_not_p, Violated),
            (&agef_p, Holds, &ag_p, Holds),
            // a DIFFERENT value of the register says nothing
            (&agef_p, Holds, &sh("mu Z. ((cnt == 3) || <> Z)"), Violated),
            // same direction
            (&ag_p, Violated, &ag_not_p, Violated),
            // `EF P` HOLDS beside `AG EF P` VIOLATED: reachable from the start, lost later — fine
            (&ef_p, Holds, &agef_p, Violated),
        ];
        for (a, va, b, vb) in fine {
            assert!(
                !definite_pair_contradicts(a, va, b, vb),
                "{a:?} {va:?} vs {b:?} {vb:?}"
            );
        }
    }

    /// mununu#599 — vacuity: the invariant target (both spellings), the refuted same-register
    /// witness, and the pairs that say nothing.
    #[test]
    fn a_guarantee_is_judged_against_its_witness() {
        use Definite::{Holds, Violated};
        let sh = |s: &str| classify_shape(&f(s)).unwrap();
        let g = sh("nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)");
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("mu Z. ((st_q != 0) || <> Z)"), Violated),
            Some(Vacuity::InvariantTarget)
        );
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("nu X. ((st_q == 0) && [] X)"), Holds),
            Some(Vacuity::InvariantTarget)
        );
        // mununu#599's own pair: `EF(st_q == S_WAIT)` VIOLATED beside `AG EF(st_q == S_IDLE)`.
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("mu Z. ((st_q == 3) || <> Z)"), Violated),
            Some(Vacuity::WitnessRefuted)
        );
        // Says nothing: a witness that HOLDS, another register, a universal that is not the
        // target, a guarantee that is not recoverability.
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("mu Z. ((st_q == 3) || <> Z)"), Holds),
            None
        );
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("mu Z. ((other == 3) || <> Z)"), Violated),
            None
        );
        assert_eq!(
            vacuity_of_guarantee(&g, &sh("nu X. ((st_q != 7) && [] X)"), Holds),
            None
        );
        assert_eq!(
            vacuity_of_guarantee(
                &sh("nu X. ((st_q == 0) && [] X)"),
                &sh("mu Z. ((st_q != 0) || <> Z)"),
                Violated
            ),
            None
        );
    }

    #[test]
    fn two_universals_never_contradict_each_other() {
        let a = classify_shape(&f("nu X. ((cnt <= 1) && [] X)")).unwrap();
        let b = classify_shape(&f("nu X. ((cnt > 1) && [] X)")).unwrap();
        assert!(
            !definite_pair_contradicts(&a, Definite::Violated, &b, Definite::Violated),
            "both violated just means the model reaches both sides"
        );
    }
}
