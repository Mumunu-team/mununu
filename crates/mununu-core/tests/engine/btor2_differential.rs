//! N0.6 — the shared random-BTOR2 tier: small transition systems with a `bad` node,
//! decided by two engines that must agree whenever both are definite.
//!
//! - `exact_bad_reachable` (full-state ROBDD, complete on these widths) is the oracle;
//! - `decide_bad_safety` (native BMC + k-induction on z3) must never contradict it: a
//!   `Violated` must be a real counterexample (the exact engine says reachable), a `Safe`
//!   a real proof (unreachable). `Unknown` is allowed per case, but the gate fails unless
//!   native decides at least [`MIN_DECIDED_PERCENT`] of the draw with both verdicts
//!   present, and two negative controls show the gate can fail (mununu#954).
//!
//! The generator is deliberately plain text: every design it writes is a legal BTOR2
//! file a human can paste into `mununu btor2 verify`, and a failing case prints it. It is
//! the generator every later lever reuses (arrays in BMC, the owned IC3, bounded
//! registerization, the SEC miter) — one source of random designs, several
//! differential pairs.
//!
//! Shape: 1–5 state bits (as 1–3 registers of width 1–3), 0–2 inputs, each `next` a
//! random expression over the registers and inputs from a small op menu, `init`
//! constants, one `bad` that is a random comparison or bit of a register.
//!
//! **Gate vs explorer.** Under `make ci` the tier replays one fixed draw ([`TIER_SEED`]), so
//! a red gate names a regression the change made, never a pre-existing engine bug the dice
//! happened to find (mununu#852 turned two unrelated PRs red that way). Fresh draws are
//! `make diff-tier-explore`, off the gate; a disagreement it finds is filed with its printed
//! design and pinned as a deterministic test. `PROPTEST_RNG_SEED=<n>` replays another draw.
use mununu_core::adapter::btor2::ast::Btor2File;
use mununu_core::adapter::btor2::native_bmc::{SafetyVerdict, decide_bad_safety};
use mununu_core::adapter::btor2::parser;
use mununu_core::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;

/// The gate's draw. Any value works; changing it changes which designs the gate checks.
const TIER_SEED: u64 = 0x6d75_6e75_6e75;

/// `Random` only when exploring; otherwise the env's `PROPTEST_RNG_SEED`, else [`TIER_SEED`].
fn tier_seed() -> RngSeed {
    if std::env::var_os("MUNUNU_DIFF_TIER_EXPLORE").is_some() {
        return RngSeed::Random;
    }
    match ProptestConfig::default().rng_seed {
        RngSeed::Random => RngSeed::Fixed(TIER_SEED),
        from_env => from_env,
    }
}

/// A random design, rendered as BTOR2 text.
#[derive(Debug, Clone)]
pub struct Design {
    pub text: String,
}

#[derive(Debug, Clone)]
enum Expr {
    Reg(usize),
    In(usize),
    Const(u64),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Ite(Box<Expr>, Box<Expr>, Box<Expr>),
}

fn expr_strategy(regs: usize, inputs: usize, width: u32, depth: u32) -> BoxedStrategy<Expr> {
    let leaf = prop_oneof![
        (0..regs).prop_map(Expr::Reg),
        if inputs > 0 {
            (0..inputs).prop_map(Expr::In).boxed()
        } else {
            (0..regs).prop_map(Expr::Reg).boxed()
        },
        (0..(1u64 << width)).prop_map(Expr::Const),
    ];
    leaf.prop_recursive(depth, 12, 2, move |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| Expr::Not(Box::new(e))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::Or(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::Xor(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::Add(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone(), inner).prop_map(|(c, a, b)| Expr::Ite(
                Box::new(c),
                Box::new(a),
                Box::new(b)
            )),
        ]
    })
    .boxed()
}

/// Emit `e` into `lines`, returning its nid. Every register, input and intermediate
/// shares one width so the menu needs no slicing; the `ite` condition is the low bit
/// of its first operand (`slice` to width 1).
struct Emitter {
    lines: Vec<String>,
    next_nid: i64,
    sort_w: i64,
    sort_1: i64,
    width: u32,
    reg_nids: Vec<i64>,
    in_nids: Vec<i64>,
}

impl Emitter {
    fn push(&mut self, s: String) -> i64 {
        let nid = self.next_nid;
        self.lines.push(format!("{nid} {s}"));
        self.next_nid += 1;
        nid
    }
    fn emit(&mut self, e: &Expr) -> i64 {
        match e {
            Expr::Reg(i) => self.reg_nids[*i],
            Expr::In(i) => self.in_nids[*i],
            Expr::Const(c) => {
                let v = c & ((1u64 << self.width) - 1);
                let s = self.sort_w;
                self.push(format!("constd {s} {v}"))
            }
            Expr::Not(a) => {
                let a = self.emit(a);
                let s = self.sort_w;
                self.push(format!("not {s} {a}"))
            }
            Expr::And(a, b) => {
                let (a, b) = (self.emit(a), self.emit(b));
                let s = self.sort_w;
                self.push(format!("and {s} {a} {b}"))
            }
            Expr::Or(a, b) => {
                let (a, b) = (self.emit(a), self.emit(b));
                let s = self.sort_w;
                self.push(format!("or {s} {a} {b}"))
            }
            Expr::Xor(a, b) => {
                let (a, b) = (self.emit(a), self.emit(b));
                let s = self.sort_w;
                self.push(format!("xor {s} {a} {b}"))
            }
            Expr::Add(a, b) => {
                let (a, b) = (self.emit(a), self.emit(b));
                let s = self.sort_w;
                self.push(format!("add {s} {a} {b}"))
            }
            Expr::Ite(c, a, b) => {
                let c = self.emit(c);
                let (s1, s) = (self.sort_1, self.sort_w);
                let bit = if self.width == 1 {
                    c
                } else {
                    self.push(format!("slice {s1} {c} 0 0"))
                };
                let (a, b) = (self.emit(a), self.emit(b));
                self.push(format!("ite {s} {bit} {a} {b}"))
            }
        }
    }
}

/// The whole design: registers, inputs, nexts, inits, one bad.
pub fn design_strategy() -> BoxedStrategy<Design> {
    (1u32..=3, 1usize..=3, 0usize..=2)
        .prop_flat_map(|(width, regs, inputs)| {
            let nexts = proptest::collection::vec(expr_strategy(regs, inputs, width, 3), regs);
            let inits = proptest::collection::vec(0..(1u64 << width), regs);
            // bad: register i == constant c, or the low bit of an expression
            let bad = prop_oneof![
                (0..regs, 0..(1u64 << width)).prop_map(|(i, c)| (Some((i, c)), None)),
                expr_strategy(regs, inputs, width, 2).prop_map(|e| (None, Some(e))),
            ];
            (Just(width), Just(regs), Just(inputs), nexts, inits, bad)
        })
        .prop_map(|(width, regs, inputs, nexts, inits, bad)| {
            let mut em = Emitter {
                lines: Vec::new(),
                next_nid: 1,
                sort_w: 0,
                sort_1: 0,
                width,
                reg_nids: Vec::new(),
                in_nids: Vec::new(),
            };
            em.sort_w = em.push(format!("sort bitvec {width}"));
            em.sort_1 = if width == 1 {
                em.sort_w
            } else {
                em.push("sort bitvec 1".to_string())
            };
            for i in 0..regs {
                let s = em.sort_w;
                let n = em.push(format!("state {s} r{i}"));
                em.reg_nids.push(n);
            }
            for i in 0..inputs {
                let s = em.sort_w;
                let n = em.push(format!("input {s} i{i}"));
                em.in_nids.push(n);
            }
            for (i, c) in inits.iter().enumerate() {
                let s = em.sort_w;
                let cn = em.push(format!("constd {s} {c}"));
                let r = em.reg_nids[i];
                em.push(format!("init {s} {r} {cn}"));
            }
            for (i, e) in nexts.iter().enumerate() {
                let n = em.emit(e);
                let s = em.sort_w;
                let r = em.reg_nids[i];
                em.push(format!("next {s} {r} {n}"));
            }
            let bad_nid = match bad {
                (Some((i, c)), _) => {
                    let s = em.sort_w;
                    let cn = em.push(format!("constd {s} {c}"));
                    let r = em.reg_nids[i];
                    let s1 = em.sort_1;
                    em.push(format!("eq {s1} {r} {cn}"))
                }
                (_, Some(e)) => {
                    let n = em.emit(&e);
                    if width == 1 {
                        n
                    } else {
                        let s1 = em.sort_1;
                        em.push(format!("slice {s1} {n} 0 0"))
                    }
                }
                _ => unreachable!(),
            };
            em.push(format!("bad {bad_nid}"));
            Design {
                text: em.lines.join("\n") + "\n",
            }
        })
        .boxed()
}

/// What one tier run decided. Counted only on a run that found no contradiction, so a case
/// proptest re-ran while shrinking is never counted twice.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    safe: usize,
    violated: usize,
    unknown: usize,
}

/// The gate's floor on the fixed draw ([`TIER_SEED`], 48 cases), so a native that abstains
/// everywhere — or one that only ever answers one way — fails the gate instead of passing it
/// vacuously (mununu#954). Set 2026-11-10 when the draw decided 47/48 (12 Safe, 35 Violated);
/// the verdicts are deterministic (rlimit, #893), so the headroom is a regression budget, not noise.
const MIN_DECIDED_PERCENT: usize = 80;

/// `Err` names what the tally is missing: too few decided cases, or no Safe, or no Violated.
fn check_floor(t: Tally) -> Result<(), String> {
    let total = t.safe + t.violated + t.unknown;
    let decided = t.safe + t.violated;
    if total == 0 {
        return Err("the tier ran no cases".into());
    }
    if decided * 100 < total * MIN_DECIDED_PERCENT {
        return Err(format!(
            "native decided {decided}/{total} cases, under the {MIN_DECIDED_PERCENT}% floor: {t:?}"
        ));
    }
    if t.safe == 0 || t.violated == 0 {
        return Err(format!(
            "the draw must exercise both verdicts, got {t:?} — a one-sided tier cannot catch a \
             flipped verdict on the other side"
        ));
    }
    Ok(())
}

/// Draw `cases` designs from the tier's seed, decide each with the exact oracle and `native`,
/// and fail (with the proptest-shrunk design) on the first contradiction; otherwise return
/// what `native` decided.
fn run_tier(
    cases: u32,
    max_shrink_iters: u32,
    native: impl Fn(&Btor2File) -> SafetyVerdict,
) -> Result<Tally, String> {
    use std::cell::Cell;
    let config = ProptestConfig {
        cases,
        rng_seed: tier_seed(),
        max_shrink_iters,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = Cell::new(Tally::default());
    runner
        .run(&design_strategy(), |d| {
            let file = parser::parse(&d.text)
                .unwrap_or_else(|e| panic!("generated BTOR2 does not parse: {e}\n{}", d.text));
            let exact = exact_bad_reachable(&d.text).unwrap_or_else(|e| {
                panic!("exact engine refused a generated design: {e}\n{}", d.text)
            });
            let mut t = tally.get();
            match native(&file) {
                SafetyVerdict::Violated { depth } => {
                    prop_assert!(
                        exact,
                        "native says bad reachable at depth {depth}, exact says unreachable\n{}",
                        d.text
                    );
                    t.violated += 1;
                }
                SafetyVerdict::Safe { k } => {
                    prop_assert!(
                        !exact,
                        "native says {k}-inductive safe, exact says bad reachable\n{}",
                        d.text
                    );
                    t.safe += 1;
                }
                SafetyVerdict::Unknown { .. } => t.unknown += 1,
            }
            tally.set(t);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    // A failing case is shrunk by re-running it, so the tally is exact only when nothing failed.
    Ok(tally.get())
}

fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48)
}

fn native_bmc(file: &Btor2File) -> SafetyVerdict {
    decide_bad_safety(file, 16, Some(2_000))
        .unwrap_or_else(|e| panic!("native engine refused a generated design: {e:?}"))
}

/// Every generated design parses, the two engines agree whenever both decide, and native
/// decides enough of the draw, both ways, for the agreement to mean something.
#[test]
fn native_bmc_never_contradicts_the_exact_engine() {
    let tally = run_tier(cases_from_env(), 1024, native_bmc).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("btor2_differential: {tally:?}");
    // The floor is measured on the gate's draw; an explorer draw or another case count only
    // reports it.
    let explorer = std::env::var_os("MUNUNU_DIFF_TIER_EXPLORE").is_some()
        || std::env::var_os("PROPTEST_RNG_SEED").is_some()
        || std::env::var_os("PROPTEST_CASES").is_some();
    match check_floor(tally) {
        Ok(()) => {}
        Err(e) if explorer => eprintln!("btor2_differential: floor not met on this draw: {e}"),
        Err(e) => panic!("{e}"),
    }
}

/// Negative control: a native that flips every Violated into Safe must turn the gate red
/// through the same runner — the contradiction path is live, not just written.
#[test]
fn the_gate_fails_when_native_flips_a_verdict() {
    let flipped = |f: &Btor2File| match native_bmc(f) {
        SafetyVerdict::Violated { depth } => SafetyVerdict::Safe { k: depth },
        v => v,
    };
    let err = run_tier(cases_from_env(), 0, flipped)
        .expect_err("a native that calls a reachable bad Safe must fail the gate");
    assert!(err.contains("exact says bad reachable"), "{err}");
}

/// Negative control: a native that abstains on every case contradicts nothing, so only the
/// floor can catch it — and must.
#[test]
fn the_gate_fails_when_native_abstains_everywhere() {
    let tally = run_tier(cases_from_env(), 0, |_| SafetyVerdict::Unknown { k: 0 })
        .expect("an abstaining native contradicts nothing");
    assert_eq!(tally.safe + tally.violated, 0);
    let err = check_floor(tally).expect_err("an all-abstain tally must fail the floor");
    assert!(err.contains("under the"), "{err}");
}

/// The floor's other arms, without engines.
#[test]
fn the_floor_rejects_a_one_sided_or_empty_tally() {
    assert!(check_floor(Tally::default()).is_err());
    let one_sided = Tally {
        safe: 40,
        violated: 0,
        unknown: 0,
    };
    assert!(
        check_floor(one_sided)
            .unwrap_err()
            .contains("both verdicts")
    );
    let ok = Tally {
        safe: 20,
        violated: 20,
        unknown: 8,
    };
    assert_eq!(check_floor(ok), Ok(()));
}
