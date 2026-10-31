//! Guarantees relativized to the assumptions (OpenTitan coverage, Phase 4).
//!
//! > Source of truth: [`relativize`] — surface: (CLI+API), through `sv verify-auto --assumes
//! > precondition` (the default).
//!
//! With the assumptions compiled into the monotone signal `ok` (`btor2::assume_monitor`), a
//! guarantee is checked on the traces along which every assumption has held so far by guarding
//! each **boolean block** of its formula by polarity:
//!
//! - where the formula quantifies UNIVERSALLY (an `assert`, a `[]`, a `nu` over boxes) the block
//!   `B` becomes `(!ok || B)` — a state where an assumption has broken owes nothing;
//! - where it quantifies EXISTENTIALLY (a `cover`, a `<>`, the `EF` of recoverability) it becomes
//!   `(ok && B)` — the witness must stay on assumption-respecting states;
//! - an existential fixpoint in a universal position (`AG EF b`'s `EF b`) is guarded as a whole,
//!   `(!ok || EF (ok && b))`, and dually.
//!
//! Because `ok` is monotone along every path, guarding the blocks equals rewriting every modality
//! (`AG (!ok || B)` ≡ `!ok || AG_{ok} B`), and keeps the formula in the shapes the reachability and
//! liveness-to-safety rescue lanes reduce. A shape this cannot guard soundly — a negated modality,
//! a fixpoint mixing `[]` and `<>`, a guarded modality — is refused, and the caller checks that
//! guarantee WITHOUT the assumptions (the stronger obligation).

use crate::mu_calculus::{Formula, Guard, ModalKind, Node, NodeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    Universal,
    Existential,
}

fn is_boolean(f: &Formula, id: NodeId) -> bool {
    match f.node(id) {
        Node::True | Node::False | Node::Predicate(_) => true,
        Node::Not(a) => is_boolean(f, *a),
        Node::And(a, b) | Node::Or(a, b) => is_boolean(f, *a) && is_boolean(f, *b),
        _ => false,
    }
}

/// The quantifier a fixpoint's own modalities use (nested fixpoints excluded): `None` when it has
/// none, `Err` when it mixes `[]` and `<>`.
fn fixpoint_polarity(f: &Formula, body: NodeId) -> Result<Option<Polarity>, String> {
    fn walk(f: &Formula, id: NodeId, seen: &mut (bool, bool)) {
        match f.node(id) {
            Node::Modal { kind, target, .. } => {
                match kind {
                    ModalKind::Box => seen.0 = true,
                    ModalKind::Diamond => seen.1 = true,
                }
                walk(f, *target, seen);
            }
            Node::Not(a) => walk(f, *a, seen),
            Node::And(a, b) | Node::Or(a, b) => {
                walk(f, *a, seen);
                walk(f, *b, seen);
            }
            _ => {}
        }
    }
    let mut seen = (false, false);
    walk(f, body, &mut seen);
    match seen {
        (true, true) => Err("a fixpoint that mixes `[]` and `<>`".to_string()),
        (true, false) => Ok(Some(Polarity::Universal)),
        (false, true) => Ok(Some(Polarity::Existential)),
        (false, false) => Ok(None),
    }
}

/// Print a node back in the syntax `mu_calculus::parser` reads.
pub(super) fn print(f: &Formula, id: NodeId) -> Result<String, String> {
    Ok(match f.node(id) {
        Node::True => "true".to_string(),
        Node::False => "false".to_string(),
        Node::Predicate(p) => {
            if p.contains(' ') {
                format!("({p})")
            } else {
                p.clone()
            }
        }
        Node::Variable(v) => f.var(*v).name.clone(),
        Node::Not(a) => format!("(!({}))", print(f, *a)?),
        Node::And(a, b) => format!("({} && {})", print(f, *a)?, print(f, *b)?),
        Node::Or(a, b) => format!("({} || {})", print(f, *a)?, print(f, *b)?),
        Node::Modal {
            kind,
            guard,
            target,
        } => {
            if *guard != Guard::default() {
                return Err("a guarded modality".to_string());
            }
            let op = if *kind == ModalKind::Box { "[]" } else { "<>" };
            format!("{op} {}", print(f, *target)?)
        }
        // Parenthesised whole: a binder's scope extends as far right as it can, so an unwrapped
        // `mu Y. (A) && B` re-parses as `mu Y. ((A) && B)`.
        Node::Mu { var, body } => format!("(mu {}. ({}))", f.var(*var).name, print(f, *body)?),
        Node::Nu { var, body } => format!("(nu {}. ({}))", f.var(*var).name, print(f, *body)?),
    })
}

/// The two spellings of one guard: `excused` releases a universal block (`(excused || B)`),
/// `owed` keeps an existential one (`(owed && B)`) — `excused ≡ ¬owed`. For the assumptions
/// `owed` is the monitor signal `ok`; for a kept `disable iff (D)`, `excused` is `D`.
struct Guards {
    excused: String,
    owed: String,
}

fn guard(block: String, g: &Guards, pol: Polarity) -> String {
    match pol {
        Polarity::Universal => format!("({} || {block})", g.excused),
        Polarity::Existential => format!("({} && {block})", g.owed),
    }
}

fn rel(f: &Formula, id: NodeId, g: &Guards, pol: Polarity) -> Result<String, String> {
    if is_boolean(f, id) {
        // A negated block under a universal guard prints as `!(owed && X)` — the same boolean as
        // `excused || !X`, in the single-negation shape the liveness rescue reduces as an
        // antecedent.
        if pol == Polarity::Universal
            && let Node::Not(inner) = f.node(id)
        {
            return Ok(format!("(!(({} && {})))", g.owed, print(f, *inner)?));
        }
        return Ok(guard(print(f, id)?, g, pol));
    }
    Ok(match f.node(id) {
        Node::Variable(v) => f.var(*v).name.clone(),
        Node::Not(_) => return Err("a negated modality".to_string()),
        Node::And(a, b) => format!("({} && {})", rel(f, *a, g, pol)?, rel(f, *b, g, pol)?),
        Node::Or(a, b) => format!("({} || {})", rel(f, *a, g, pol)?, rel(f, *b, g, pol)?),
        Node::Modal {
            kind,
            guard: modal_guard,
            target,
        } => {
            if *modal_guard != Guard::default() {
                return Err("a guarded modality".to_string());
            }
            let (op, inner) = match kind {
                ModalKind::Box => ("[]", Polarity::Universal),
                ModalKind::Diamond => ("<>", Polarity::Existential),
            };
            if inner != pol {
                // `<>` under a universal block (or `[]` under an existential one) outside a
                // fixpoint of its own polarity: not a shape the translator emits.
                return Err(
                    "a modality of the opposite polarity outside its own fixpoint".to_string(),
                );
            }
            format!("{op} {}", rel(f, *target, g, inner)?)
        }
        Node::Mu { var, body } | Node::Nu { var, body } => {
            let kw = if matches!(f.node(id), Node::Mu { .. }) {
                "mu"
            } else {
                "nu"
            };
            let own = fixpoint_polarity(f, *body)?.unwrap_or(pol);
            let inner = format!("({kw} {}. ({}))", f.var(*var).name, rel(f, *body, g, own)?);
            if own == pol {
                inner
            } else {
                guard(inner, g, pol)
            }
        }
        Node::True | Node::False | Node::Predicate(_) => unreachable!("boolean, handled above"),
    })
}

/// A kept `disable iff (D)` applied to a property BODY (before its `nu X. (… && [] X)` wrapper):
/// every boolean block is guarded by `D` at the cycle it is evaluated — `(D || B)` where the body
/// quantifies universally, `(!D && B)` where existentially. SVA disables an attempt when `D` holds
/// at ANY cycle of it; the former lowering `(D || body)` checked only the first, so an attempt
/// disabled mid-way still had to complete (a spurious VIOLATED, mununu#705). Guarding the blocks
/// is exact for one-step attempts (`a |=> c`) and never weaker than SVA for longer ones (a block
/// at `t+k` sees only `D_t` and `D_{t+k}`, not the disables at `t+1 … t+k-1`).
///
/// The whole guarded body is ALSO guarded by `D` at the attempt's start: a body whose first
/// evaluated block lies later (`##1 a |-> c`) would otherwise never look at `D_t`, and an attempt
/// disabled where it starts still had to complete (a spurious VIOLATED, mununu#708 — OpenTitan
/// `prim_esc_rxtx` `EscRespCheck_A`).
pub fn guard_with_disable(body: &str, disable: &str, existential: bool) -> Result<String, String> {
    let guarded = guard_blocks(
        body,
        &Guards {
            excused: disable.to_string(),
            owed: format!("(!({disable}))"),
        },
        existential,
    )?;
    let f = crate::mu_calculus::parser::parse(body)
        .map_err(|e| format!("formula does not parse: {e:?}"))?;
    if guards_the_start(&f, f.root(), existential) {
        return Ok(guarded);
    }
    Ok(if existential {
        format!("((!({disable})) && {guarded})")
    } else {
        format!("({disable} || {guarded})")
    })
}

/// Whether guarding the boolean blocks of `id` already settles it at the attempt's start when the
/// guard fires: a guarded block there is true (universal) or false (existential), so a disjunction
/// needs one such side and a conjunction both — dually for the existential reading.
fn guards_the_start(f: &Formula, id: NodeId, existential: bool) -> bool {
    if is_boolean(f, id) {
        return true;
    }
    let side = |n: &NodeId| guards_the_start(f, *n, existential);
    match f.node(id) {
        Node::Or(a, b) if !existential => side(a) || side(b),
        Node::And(a, b) if existential => side(a) || side(b),
        Node::Or(a, b) | Node::And(a, b) => side(a) && side(b),
        Node::Mu { body, .. } | Node::Nu { body, .. } => side(body),
        _ => false,
    }
}

/// Relativize `formula` to the monotone assumption signal `ok`. `existential` is true for a
/// `cover` (its root quantifies existentially). `Err(reason)` for a shape this does not guard.
pub fn relativize(formula: &str, ok: &str, existential: bool) -> Result<String, String> {
    guard_blocks(
        formula,
        &Guards {
            excused: format!("(!({ok}))"),
            owed: ok.to_string(),
        },
        existential,
    )
}

/// Whether `formula`'s root quantifies existentially (2026-10-30): a `mu` / `nu` whose body
/// reaches only `<>` modalities — `EF t` is `mu X. (t || <> X)` — so that [`relativize`] guards
/// it as it guards a cover. A guarantee row is an `assert` whatever its shape, and guarding an EF
/// root universally yields `(!ok) || EF (ok && t)`, which an initial state that already breaks
/// the assumption satisfies at once: a vacuous HOLDS on the one row that exists to rule vacuity
/// out (measured 2026-10-30: a 2-bit counter under `assume property (!inc)`, `rtl_reachable
/// TARGET="q == 3"` HOLDS although no assumption-keeping trace reaches it). A root that is not a
/// fixpoint, or whose body mixes `[]` and `<>`, is read as universal, as before.
pub fn root_is_existential(formula: &str) -> bool {
    let Ok(f) = crate::mu_calculus::parser::parse(formula) else {
        return false;
    };
    match f.node(f.root()) {
        Node::Mu { body, .. } | Node::Nu { body, .. } => {
            matches!(
                fixpoint_polarity(&f, *body),
                Ok(Some(Polarity::Existential))
            )
        }
        _ => false,
    }
}

fn guard_blocks(formula: &str, g: &Guards, existential: bool) -> Result<String, String> {
    let f = crate::mu_calculus::parser::parse(formula)
        .map_err(|e| format!("formula does not parse: {e:?}"))?;
    let pol = if existential {
        Polarity::Existential
    } else {
        Polarity::Universal
    };
    let out = rel(&f, f.root(), g, pol)?;
    crate::mu_calculus::parser::parse(&out)
        .map_err(|e| format!("relativized formula does not parse ({e:?}): {out}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printed(s: &str) -> String {
        let f = crate::mu_calculus::parser::parse(s).expect("parses");
        print(&f, f.root()).expect("prints")
    }

    #[test]
    fn the_printer_round_trips_every_translator_shape() {
        for s in [
            "nu X. ((x == 3) && [] X)",
            "nu X. (((!((a && b))) || [] (c != 0)) && [] X)",
            "nu X. ((mu Y. ((cnt == 0) || [] Y)) && [] X)",
            "mu X. ((st == 2) || <> X)",
            "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)",
            "nu X. ((mununu_x_00ff00ff00ff00ff || (!(e__bits1_1))) && [] X)",
            "nu X. (true && [] X)",
        ] {
            let once = printed(s);
            assert_eq!(printed(&once), once, "{s}");
        }
    }

    #[test]
    fn blocks_are_guarded_by_polarity() {
        // Compared after a parse + print, so only the structure matters.
        let r = |s: &str, ex: bool| printed(&relativize(s, "ok", ex).expect("relativizes"));
        let same = |a: &str| printed(a);
        assert_eq!(
            r("nu X. ((x == 3) && [] X)", false),
            same("nu X. (((!(ok)) || (x == 3)) && [] X)")
        );
        // `a |=> c`: both the antecedent block (folded to `!(ok && a)`) and the consequent block.
        assert_eq!(
            r("nu X. (((!(a)) || [] c) && [] X)", false),
            same("nu X. (((!((ok && a))) || [] ((!(ok)) || c)) && [] X)")
        );
        // A cover: existential.
        assert_eq!(
            r("mu X. ((st == 2) || <> X)", true),
            same("mu X. ((ok && (st == 2)) || <> X)")
        );
        // Recoverability: the EF block in a universal position is guarded as a whole.
        assert_eq!(
            r("nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)", false),
            same("nu Y. (((!(ok)) || (mu X. ((ok && (st == 0)) || <> X))) && [] Y)")
        );
        // Liveness: AF under AG — universal throughout.
        assert_eq!(
            r("nu X. ((mu Y. ((cnt == 0) || [] Y)) && [] X)", false),
            same("nu X. ((mu Y. (((!(ok)) || (cnt == 0)) || [] Y)) && [] X)")
        );
    }

    /// 2026-10-30 — the root's polarity decides how an `assert` row is guarded: an EF root is
    /// existential (guarded like a cover), every safety / AG EF / AG AF root is universal.
    #[test]
    fn an_ef_root_is_existential_and_a_universal_root_is_not() {
        assert!(root_is_existential("mu X. ((q == 3) || <> X)"));
        assert!(root_is_existential("nu X. ((q != 3) && <> X)"));
        assert!(!root_is_existential("nu X. ((q != 3) && [] X)"));
        assert!(!root_is_existential(
            "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)"
        ));
        assert!(!root_is_existential("nu X. ((mu Y. (p || [] Y)) && [] X)"));
        assert!(!root_is_existential("(!(a)) || b"));
        assert!(!root_is_existential("not a formula ("));
    }

    #[test]
    fn unguardable_shapes_are_refused() {
        assert!(relativize("nu X. ((!([] a)) && [] X)", "ok", false).is_err());
        assert!(relativize("nu X. (([] a || <> b) && [] X)", "ok", false).is_err());
    }
}

#[cfg(test)]
mod liveness_seam {
    /// The seam with the liveness-to-safety rescue: a shape it reduces before relativization it
    /// still reduces after (`AG AF b`, and `AG (a → AF b)` with the antecedent folded to
    /// `!(ok && a)`).
    #[test]
    fn relativized_liveness_shapes_stay_reducible() {
        for s in [
            "nu X. ((mu Y. ((cnt == 0) || [] Y)) && [] X)",
            "nu X. (((!(req)) || (mu Y. (gnt || [] Y))) && [] X)",
        ] {
            let orig = crate::mu_calculus::parser::parse(s).unwrap();
            let r = super::relativize(s, "ok", false).unwrap();
            let rel = crate::mu_calculus::parser::parse(&r).unwrap();
            assert!(crate::adapter::liveness_rescue::reduce_response_shape(&orig).is_some());
            assert!(
                crate::adapter::liveness_rescue::reduce_response_shape(&rel).is_some(),
                "{s} -> {r}"
            );
        }
    }
}
