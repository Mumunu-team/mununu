//! LTL with its path semantics: the universal translation of `ltl` bodies.
//!
//! > Source of truth: [`translate_universal`] — surface: (CLI+API+UI), through a CTXDSL
//! > `body = ltl …;` formula.
//!
//! An LTL formula holds on a model when it holds on EVERY path from the initial states. The
//! translator used to map it homomorphically onto CTL-style fixpoints with `F` and `U` read
//! existentially (`F p` ↦ `μX. (p ∨ <>X)`, i.e. `EF p`), and `¬` passed straight through, so a
//! property over every path was checked over SOME path: an imported Promela `ltl` claim, which
//! SPIN reads universally, could come back `holds` on a model where a path never reaches its goal.
//!
//! The formula is brought to negation normal form (`¬G φ = F ¬φ`, `¬(φ U ψ) = ¬ψ W (¬φ ∧ ¬ψ)`, …)
//! and translated exactly over the fragment where an LTL formula and its universal branching
//! reading agree (a syntactic subset of ACTL ∩ LTL, after Maidl 2000):
//!
//! ```text
//! φ ::= b | φ ∧ φ | b ∨ φ | X φ | G φ | F b | b U b | b W b
//! ```
//!
//! where `b` is propositional. Response `G (b → F b′)` and recurrence `G F b` fall inside it. A
//! formula outside it (`F G p`, `G p ∨ G q`, `F (p ∧ X q)`) is refused with a message that names
//! the subformula, rather than translated into something that means a different property. The
//! existential reading stays available as `body = eltl …;` ([`super::translator::translate_existential`]).
//!
//! `F` and `U` also require a successor (`<> true`): a path that ends in a deadlock never reaches
//! its goal, and the plain `μX. (b ∨ [] X)` would accept it vacuously.

use super::ast::LtlFormula;
use super::translator::TranslationError;
use crate::mu_calculus::Formula;

/// Translate `ltl` under its path (universal) semantics; see the module doc for the fragment.
pub fn translate_universal(ltl: &LtlFormula) -> Result<Formula, TranslationError> {
    translate_universal_fair(ltl, &[])
}

/// The universal translation as the mu-calculus TEXT the parser reads — for callers that carry
/// formulas as strings (the SV annotation scanner, whose rows re-parse their `formula` in the
/// per-property loop). The text is parse-checked before it is returned, so a caller never
/// stores a string the loop would refuse.
///
/// > Source of truth: this function — surface: (CLI+API), through `@mununu_guarantee ltl …`.
pub fn translate_universal_text(ltl: &LtlFormula) -> Result<String, TranslationError> {
    let mut fresh = 0usize;
    let text = universal(&nnf(ltl, false), &mut fresh, &[])?;
    crate::mu_calculus::parser::parse(&text).map_err(|e| TranslationError::Translation {
        message: format!("internal: the universal translation `{text}` does not parse: {e:?}"),
    })?;
    Ok(text)
}

/// [`translate_universal`] over the FAIR paths only — those satisfying every `fairness`
/// constraint infinitely often (an asynchronous composition's `fairness weak;`). `F b` becomes the
/// fair `AF b` (`¬ E_C G ¬b`, [`crate::verify::fairness::fair_af`]) and `b U c` becomes
/// `(b W c) ∧ fair AF c`; `G` and `W` are unchanged, because weak fairness is machine-closed —
/// every finite prefix extends to a fair path, so a safety property over every path and over the
/// fair ones agree.
pub fn translate_universal_fair(
    ltl: &LtlFormula,
    fairness: &[crate::verify::fairness::Assumption],
) -> Result<Formula, TranslationError> {
    let mut fresh = 0usize;
    let text = universal(&nnf(ltl, false), &mut fresh, fairness)?;
    crate::mu_calculus::parser::parse(&text).map_err(|e| TranslationError::Translation {
        message: format!("internal: the universal translation `{text}` does not parse: {e:?}"),
    })
}

/// Negation normal form: negations only on atoms. `neg` is the parity of the enclosing `¬`s.
/// `¬X φ = X ¬φ` holds on total models (every state has a successor).
fn nnf(f: &LtlFormula, neg: bool) -> LtlFormula {
    use LtlFormula as L;
    let b = |x: LtlFormula| Box::new(x);
    match f {
        L::True => {
            if neg {
                L::False
            } else {
                L::True
            }
        }
        L::False => {
            if neg {
                L::True
            } else {
                L::False
            }
        }
        L::Predicate(_) => {
            if neg {
                L::Not(b(f.clone()))
            } else {
                f.clone()
            }
        }
        L::Not(a) => nnf(a, !neg),
        L::And(x, y) if !neg => L::And(b(nnf(x, false)), b(nnf(y, false))),
        L::And(x, y) => L::Or(b(nnf(x, true)), b(nnf(y, true))),
        L::Or(x, y) if !neg => L::Or(b(nnf(x, false)), b(nnf(y, false))),
        L::Or(x, y) => L::And(b(nnf(x, true)), b(nnf(y, true))),
        L::Implies(x, y) => nnf(&L::Or(b(L::Not(x.clone())), y.clone()), neg),
        L::Next(a) => L::Next(b(nnf(a, neg))),
        L::Always(a) if !neg => L::Always(b(nnf(a, false))),
        L::Always(a) => L::Eventually(b(nnf(a, true))),
        L::Eventually(a) if !neg => L::Eventually(b(nnf(a, false))),
        L::Eventually(a) => L::Always(b(nnf(a, true))),
        L::Until { left, right } if !neg => L::Until {
            left: b(nnf(left, false)),
            right: b(nnf(right, false)),
        },
        // ¬(φ U ψ) = ¬ψ W (¬φ ∧ ¬ψ)
        L::Until { left, right } => L::WeakUntil {
            left: b(nnf(right, true)),
            right: b(L::And(b(nnf(left, true)), b(nnf(right, true)))),
        },
        L::WeakUntil { left, right } if !neg => L::WeakUntil {
            left: b(nnf(left, false)),
            right: b(nnf(right, false)),
        },
        // ¬(φ W ψ) = ¬ψ U (¬φ ∧ ¬ψ)
        L::WeakUntil { left, right } => L::Until {
            left: b(nnf(right, true)),
            right: b(L::And(b(nnf(left, true)), b(nnf(right, true)))),
        },
        // φ R ψ = ψ W (φ ∧ ψ)
        L::Release { left, right } => nnf(
            &L::WeakUntil {
                left: right.clone(),
                right: b(L::And(left.clone(), right.clone())),
            },
            neg,
        ),
        L::Recurrence(a) => nnf(&L::Always(b(L::Eventually(a.clone()))), neg),
        L::Stabilization(a) => nnf(&L::Eventually(b(L::Always(a.clone()))), neg),
        L::Response { trigger, response } => nnf(
            &L::Always(b(L::Implies(
                trigger.clone(),
                b(L::Eventually(response.clone())),
            ))),
            neg,
        ),
    }
}

fn is_propositional(f: &LtlFormula) -> bool {
    use LtlFormula as L;
    match f {
        L::True | L::False | L::Predicate(_) => true,
        L::Not(a) => is_propositional(a),
        L::And(x, y) | L::Or(x, y) | L::Implies(x, y) => is_propositional(x) && is_propositional(y),
        _ => false,
    }
}

/// Propositional formula, as μ-calculus text.
fn prop(f: &LtlFormula) -> String {
    use LtlFormula as L;
    match f {
        L::True => "true".to_string(),
        L::False => "false".to_string(),
        L::Predicate(p) if p.contains(' ') => format!("({p})"),
        L::Predicate(p) => p.clone(),
        L::Not(a) => format!("(!({}))", prop(a)),
        L::And(x, y) => format!("({} && {})", prop(x), prop(y)),
        L::Or(x, y) => format!("({} || {})", prop(x), prop(y)),
        L::Implies(x, y) => format!("((!({})) || {})", prop(x), prop(y)),
        _ => unreachable!("propositional by construction"),
    }
}

fn outside(f: &LtlFormula, why: &str) -> TranslationError {
    TranslationError::Translation {
        message: format!(
            "`{}` is outside the LTL fragment mununu checks over every path ({why}). Write it \
             in μ-calculus, use `body = eltl …;` for the some-path reading, or state it as a \
             GR(1) objective.",
            show(f)
        ),
    }
}

fn universal(
    f: &LtlFormula,
    fresh: &mut usize,
    fairness: &[crate::verify::fairness::Assumption],
) -> Result<String, TranslationError> {
    use LtlFormula as L;
    if is_propositional(f) {
        return Ok(prop(f));
    }
    let mut var = |p: &str| {
        *fresh += 1;
        format!("{p}{}", *fresh)
    };
    Ok(match f {
        L::And(x, y) => format!(
            "({} && {})",
            universal(x, fresh, fairness)?,
            universal(y, fresh, fairness)?
        ),
        L::Or(x, y) if is_propositional(x) => {
            format!("({} || {})", prop(x), universal(y, fresh, fairness)?)
        }
        L::Or(x, y) if is_propositional(y) => {
            format!("({} || {})", universal(x, fresh, fairness)?, prop(y))
        }
        L::Or(..) => {
            return Err(outside(
                f,
                "a disjunction of two temporal formulas holds path by path, not state by state",
            ));
        }
        L::Next(a) => format!("([] {})", universal(a, fresh, fairness)?),
        L::Always(a) => {
            let x = var("G");
            format!("(nu {x}. ({} && [] {x}))", universal(a, fresh, fairness)?)
        }
        L::Eventually(a) if is_propositional(a) && !fairness.is_empty() => {
            crate::verify::fairness::fair_af(&prop(a), fairness)
        }
        L::Eventually(a) if is_propositional(a) => {
            let x = var("F");
            format!("(mu {x}. ({} || (([] {x}) && (<> true))))", prop(a))
        }
        L::Eventually(_) => {
            return Err(outside(f, "`F` applies to a propositional formula here"));
        }
        L::Until { left, right }
            if is_propositional(left) && is_propositional(right) && !fairness.is_empty() =>
        {
            let x = var("W");
            format!(
                "((nu {x}. (({q} || {p}) && ({q} || ([] {x})))) && {})",
                crate::verify::fairness::fair_af(&prop(right), fairness),
                q = prop(right),
                p = prop(left),
            )
        }
        L::Until { left, right } if is_propositional(left) && is_propositional(right) => {
            let x = var("U");
            format!(
                "(mu {x}. ({} || ({} && (([] {x}) && (<> true)))))",
                prop(right),
                prop(left)
            )
        }
        // `p W q` in the And-root spelling `nu W. ((q || p) && (q || [] W))` — the same reading
        // as `nu W. (q || (p && [] W))`, but the spelling the one-path safety monitor recognises
        // as a weak-until block (`bad_monitor::weak_until_parts`), so a VIOLATED `G (a -> X (!a W
        // b))` ships its counterexample path like any other safety row.
        L::WeakUntil { left, right } if is_propositional(left) && is_propositional(right) => {
            let x = var("W");
            format!(
                "(nu {x}. (({q} || {p}) && ({q} || ([] {x}))))",
                q = prop(right),
                p = prop(left),
            )
        }
        L::Until { .. } | L::WeakUntil { .. } => {
            return Err(outside(
                f,
                "`U` and `W` apply to propositional formulas here",
            ));
        }
        // NNF leaves no other connective.
        _ => return Err(outside(f, "not in negation normal form")),
    })
}

/// An LTL formula in the surface syntax, for messages.
pub fn show(f: &LtlFormula) -> String {
    use LtlFormula as L;
    match f {
        L::True => "true".to_string(),
        L::False => "false".to_string(),
        L::Predicate(p) => p.clone(),
        L::Not(a) => format!("!{}", show(a)),
        L::And(x, y) => format!("({} && {})", show(x), show(y)),
        L::Or(x, y) => format!("({} || {})", show(x), show(y)),
        L::Implies(x, y) => format!("({} -> {})", show(x), show(y)),
        L::Next(a) => format!("X {}", show(a)),
        L::Always(a) => format!("G {}", show(a)),
        L::Eventually(a) => format!("F {}", show(a)),
        L::Until { left, right } => format!("({} U {})", show(left), show(right)),
        L::WeakUntil { left, right } => format!("({} W {})", show(left), show(right)),
        L::Release { left, right } => format!("({} R {})", show(left), show(right)),
        L::Recurrence(a) => format!("G F {}", show(a)),
        L::Stabilization(a) => format!("F G {}", show(a)),
        L::Response { trigger, response } => {
            format!("G ({} -> F {})", show(trigger), show(response))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ltl::parser::parse;

    fn t(s: &str) -> Result<Formula, TranslationError> {
        translate_universal(&parse(s).expect("LTL parses"))
    }

    #[test]
    fn the_fragment_translates_and_the_rest_is_refused() {
        for ok in [
            "G p",
            "F p",
            "G (p -> F q)",
            "G F p",
            "p U q",
            "p W q",
            "X G p",
            "!G p",
            "!(p U q)",
            "G (p -> X q)",
            "G (p || X q)",
            "p R q",
        ] {
            assert!(t(ok).is_ok(), "{ok}: {:?}", t(ok).err());
        }
        for bad in [
            "F G p",
            "G p || G q",
            "F (p && X q)",
            "(G p) U q",
            "!(G F p)",
        ] {
            let err = t(bad).expect_err(bad).to_string();
            assert!(err.contains("outside the LTL fragment"), "{bad}: {err}");
        }
    }

    #[test]
    fn negation_is_pushed_to_the_atoms() {
        // `!G p` is `F !p`: a least fixpoint over boxes, not a negated greatest fixpoint.
        let f = t("!G p").unwrap();
        assert!(matches!(
            f.node(f.root()),
            crate::mu_calculus::Node::Mu { .. }
        ));
    }
}
