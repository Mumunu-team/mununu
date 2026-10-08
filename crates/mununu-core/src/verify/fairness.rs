//! mununu#595 — environment assumptions (fairness) on the `verify` path: the fair-path
//! mu-calculus encoder.
//!
//! A liveness property on a hand-written composition is almost always violated trivially — an
//! environment that may stay silent, fault, or crash forever violates every `AF`. The BTOR2 verbs
//! have fairness-constrained checking; the CTXDSL / `verify.toml` path had none, and
//! `ltl (GF a) -> (GF b)` is translated state-wise (`!(νμ…) || (νμ…)`), which is not the
//! path-quantified implication. What works is to encode fair paths as nested fixpoints — the
//! classic fair-`EG` characterisation (Emerson–Lei 1986; Clarke–Grumberg–Peled §6.4) — and that is
//! what this module generates from declared [`Assumption`]s:
//!
//! ```text
//! E_C G q   =  νZ. ( q ∧ ⋀_k C_k(Z, q) )
//!   state constraint  GF P      : C_k = ◇ μY. ((Z ∧ P) ∨ (q ∧ ◇Y))
//!   edge constraint   GF ⟨l⟩    : C_k = μY. ( ⟨l⟩Z ∨ ◇(q ∧ Y) )
//!   edges constraint  GF ⟨l₁|…⟩ : C_k = μY. ( ⟨l₁⟩Z ∨ … ∨ ◇(q ∧ Y) )
//!   weak fairness of l          : C_k = μY. ( ⟨l⟩Z ∨ ◇(Z ∧ ¬⟨l⟩true) ∨ ◇(q ∧ Y) )
//! A_C F p   =  ¬E_C G ¬p
//! AG(a → A_C F b)  =  νW. ((¬a ∨ A_C F b) ∧ □W)
//! gate      =  E_C G true        — a fair path exists; an unsatisfiable assumption makes every
//!                                  guarantee vacuous, so the orchestrator evaluates this too and
//!                                  reports the verdict as CONDITIONAL, never as a bare `holds`.
//! ```
//!
//! **Semantics.** `E_C G q` holds at `s` iff some path from `s` stays in `q` forever AND satisfies
//! every constraint infinitely often. With no constraints it degenerates to `EG q`
//! (`νZ. (q ∧ ◇Z)`), so `A_C F p` with no assumptions is the plain `AF p` — the fair templates
//! with an empty assumption list are their unconditional cousins, and the config validator
//! refuses that spelling so the degenerate case cannot pass for a conditional verdict.
//!
//! **Soundness.** 2-valued, on the explicit CLTS the `verify` path realizes: the encoding is the
//! textbook one and is exact on a finite Kripke structure. The twelve known-answer cases the issue
//! shipped with (`sanity_fair.ctxdsl`) are this module's test, including the two vacuity traps —
//! an assumption no path can satisfy makes `A_C F p` true while `E_C G true` is false.

use serde::{Deserialize, Serialize};

/// One declared environment assumption — an unconditional fairness constraint every considered
/// path must satisfy infinitely often.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assumption {
    /// `GF P` — a state predicate holds infinitely often.
    State { atom: String },
    /// `GF ⟨l⟩` — a transition carrying label `l` is taken infinitely often.
    Edge { label: String },
    /// `GF ⟨l₁⟩ ∨ … ∨ ⟨lₙ⟩` — a transition carrying any one of the labels is taken infinitely often.
    Edges { labels: Vec<String> },
    /// Weak fairness of `l`: taken infinitely often, or disabled infinitely often.
    Weak { label: String },
}

/// The k-th conjunct `C_k(Z, q)` of `E_C G q`.
fn constraint(k: usize, a: &Assumption, z: &str, q: &str) -> String {
    let y = format!("Y{k}");
    match a {
        Assumption::State { atom } => {
            format!("< > (mu {y}. ((({z}) && ({atom})) || (({q}) && < > {y})))")
        }
        Assumption::Edge { label } => {
            format!("(mu {y}. (< labels = {{ {label} }} > {z} || < > (({q}) && {y})))")
        }
        Assumption::Edges { labels } => {
            let alts = labels
                .iter()
                .map(|l| format!("< labels = {{ {l} }} > {z}"))
                .collect::<Vec<_>>()
                .join(" || ");
            format!("(mu {y}. ({alts} || < > (({q}) && {y})))")
        }
        Assumption::Weak { label } => format!(
            "(mu {y}. (< labels = {{ {label} }} > {z} || < > ({z} && !(< labels = {{ {label} }} > true)) || < > (({q}) && {y})))"
        ),
    }
}

/// `E_C G q` — some path from here stays in `q` forever and satisfies every assumption
/// infinitely often. With no assumptions, `EG q`.
pub fn fair_eg(q: &str, assumptions: &[Assumption]) -> String {
    let z = "Z";
    if assumptions.is_empty() {
        return format!("(nu {z}. (({q}) && < > {z}))");
    }
    let body = assumptions
        .iter()
        .enumerate()
        .map(|(k, a)| constraint(k, a, z, q))
        .collect::<Vec<_>>()
        .join(" && ");
    format!("(nu {z}. (({q}) && {body}))")
}

/// `A_C F p` — every fair path eventually reaches `p`.
pub fn fair_af(p: &str, assumptions: &[Assumption]) -> String {
    format!("!{}", fair_eg(&format!("!({p})"), assumptions))
}

/// `AG(a → A_C F b)` — the fair response pattern.
pub fn fair_response(a: &str, b: &str, assumptions: &[Assumption]) -> String {
    format!("(nu W. ((!({a}) || {}) && [] W))", fair_af(b, assumptions))
}

/// `AG(A_C F p)` — the fair always-eventually (recurrence) pattern.
pub fn fair_always_eventually(p: &str, assumptions: &[Assumption]) -> String {
    format!("(nu W. ({} && [] W))", fair_af(p, assumptions))
}

/// The non-vacuity gate: a fair path exists from here (`E_C G true`).
pub fn fair_path_exists(assumptions: &[Assumption]) -> String {
    fair_eg("true", assumptions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::evaluate;

    /// The issue's sanity model, verbatim: `M` may idle forever, cycle `S0-b-S1-c-S0` forever, or
    /// go to `Goal`; `M2` may idle forever or go, with `go2` continuously enabled at `T0`.
    const SANITY: &str = r#"
context FairSanity {
    automata {
        automaton M {
            controllable { }
            states { state S0 initial; state S1; state Goal; }
            transitions {
                transition S0 -> S0 on label wait;
                transition S0 -> Goal on label go;
                transition S0 -> S1 on label b;
                transition S1 -> S0 on label c;
                transition Goal -> Goal on label g;
            }
        }
        automaton M2 {
            controllable { }
            states { state T0 initial; state Goal2; }
            transitions {
                transition T0 -> T0 on label wait2;
                transition T0 -> Goal2 on label go2;
                transition Goal2 -> Goal2 on label g2;
            }
        }
    }
}
"#;

    /// Evaluate `formula` over automaton `over` of the sanity model at its initial states — as a
    /// `mu_formulas` entry of the document, the way `context eval` and the verify assembler
    /// present a property, so its state-name atoms are bound at realize time.
    fn holds_at_initials(over: &str, formula: &str) -> bool {
        let src = SANITY.replacen(
            "\n}\n",
            &format!(
                "\n    mu_formulas {{ formula probe {{ over {over}; body = {formula}; }} }}\n}}\n"
            ),
            1,
        );
        let doc = crate::context_dsl::parse(&src)
            .unwrap_or_else(|e| panic!("sanity model + probe parses: {e:?}\n{formula}"));
        let realized = crate::context_dsl::realize_context(&doc, &[]).expect("realizes");
        let clts = realized.context.clts(over).expect("automaton");
        let env = realized.environment_for(over);
        let f = &realized.formulas["probe"].formula;
        let sat = evaluate(f, clts, &env).expect("evaluates");
        !clts.initial_states().is_empty()
            && clts
                .initial_states()
                .iter()
                .all(|s| sat.get(s.index()).map(|b| *b).unwrap_or(false))
    }

    fn edge(l: &str) -> Assumption {
        Assumption::Edge { label: l.into() }
    }
    fn weak(l: &str) -> Assumption {
        Assumption::Weak { label: l.into() }
    }

    /// Every generated formula is balanced — the one syntactic check the reference encoder made.
    #[test]
    fn generated_formulas_are_balanced_and_parse() {
        for f in [
            fair_eg("true", &[]),
            fair_af("Goal", &[edge("go")]),
            fair_response("S0", "Goal", &[edge("go"), weak("b")]),
            fair_always_eventually("Goal", &[Assumption::State { atom: "S1".into() }]),
            fair_path_exists(&[Assumption::Edges {
                labels: vec!["go".into(), "b".into()],
            }]),
        ] {
            assert_eq!(f.matches('(').count(), f.matches(')').count(), "{f}");
            crate::mu_calculus::parser::parse(&f).unwrap_or_else(|e| panic!("{f}\n{e:?}"));
        }
    }

    /// mununu#595 — the twelve known-answer cases, as measured by the issue on `context eval`.
    #[test]
    fn x595_the_twelve_known_answers() {
        // Unconditional: the environment may idle forever ⇒ `AF Goal` is false on both.
        assert!(!holds_at_initials("M", "mu X. (Goal || [] X)"));
        assert!(!holds_at_initials("M2", "mu X. (Goal2 || [] X)"));

        // GF⟨go⟩ on M: `go` leaves S0 for Goal, so every path taking `go` infinitely often …
        // cannot exist (Goal is a `g`-only sink) — the assumption is UNSATISFIABLE. `A_C F Goal`
        // is then vacuously true and the gate is false: the first vacuity trap.
        assert!(holds_at_initials("M", &fair_af("Goal", &[edge("go")])));
        assert!(!holds_at_initials("M", &fair_path_exists(&[edge("go")])));

        // GF⟨b⟩ on M: the `S0-b-S1-c-S0` cycle is fair and avoids Goal — a GENUINE violation,
        // with the gate true.
        assert!(!holds_at_initials("M", &fair_af("Goal", &[edge("b")])));
        assert!(holds_at_initials("M", &fair_path_exists(&[edge("b")])));

        // GF S1 (state): the same cycle visits S1 infinitely often and avoids Goal.
        assert!(!holds_at_initials(
            "M",
            &fair_af("Goal", &[Assumption::State { atom: "S1".into() }])
        ));

        // Weak fairness of `go` on M: the cycle DISABLES `go` at S1 infinitely often, so the
        // cycle is weakly fair and avoids Goal.
        assert!(!holds_at_initials("M", &fair_af("Goal", &[weak("go")])));

        // M2: `go2` is continuously enabled at T0, so weak fairness forces it — `A_C F Goal2`
        // holds and the gate is true (Goal2's `g2` self-loop keeps `go2` disabled forever).
        assert!(holds_at_initials("M2", &fair_af("Goal2", &[weak("go2")])));
        assert!(holds_at_initials("M2", &fair_path_exists(&[weak("go2")])));

        // M2 under GF⟨go2⟩: `go2` can be taken at most once ⇒ unsatisfiable; the second trap.
        assert!(holds_at_initials("M2", &fair_af("Goal2", &[edge("go2")])));
        assert!(!holds_at_initials("M2", &fair_path_exists(&[edge("go2")])));
    }

    /// The response and recurrence wrappers compose the same core.
    #[test]
    fn x595_response_and_recurrence_wrap_the_fair_af() {
        // Under weak fairness of `go2`, from every reachable state of M2 Goal2 is eventually
        // reached on every fair path (T0 must take go2; Goal2 is already there).
        assert!(holds_at_initials(
            "M2",
            &fair_always_eventually("Goal2", &[weak("go2")])
        ));
        // `T0 → A_wf F Goal2` holds; `S0 → A_{GF b} F Goal` on M does not.
        assert!(holds_at_initials(
            "M2",
            &fair_response("T0", "Goal2", &[weak("go2")])
        ));
        assert!(!holds_at_initials(
            "M",
            &fair_response("S0", "Goal", &[edge("b")])
        ));
        // `edges` — any of several labels: `go` or `b` infinitely often is satisfiable (via b).
        let any = Assumption::Edges {
            labels: vec!["go".into(), "b".into()],
        };
        assert!(holds_at_initials(
            "M",
            &fair_path_exists(std::slice::from_ref(&any))
        ));
        assert!(!holds_at_initials(
            "M",
            &fair_af("Goal", std::slice::from_ref(&any))
        ));
    }
}
