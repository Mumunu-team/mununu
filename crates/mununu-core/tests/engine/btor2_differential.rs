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

/// A 1-bit property signal over the design.
#[derive(Debug, Clone)]
enum Literal {
    RegEq(usize, u64),
    LowBit(Expr),
    /// The complement of a literal (the justice tier's alternating sets `{l, ¬l}`).
    Neg(Box<Literal>),
}

impl From<(Option<(usize, u64)>, Option<Expr>)> for Literal {
    fn from(choice: (Option<(usize, u64)>, Option<Expr>)) -> Self {
        match choice {
            (Some((i, c)), _) => Literal::RegEq(i, c),
            (None, Some(e)) => Literal::LowBit(e),
            (None, None) => Literal::RegEq(0, 0),
        }
    }
}

fn literal_strategy(regs: usize, inputs: usize, width: u32) -> BoxedStrategy<Literal> {
    prop_oneof![
        (0..regs, 0..(1u64 << width)).prop_map(|(i, c)| Literal::RegEq(i, c)),
        expr_strategy(regs, inputs, width, 2).prop_map(Literal::LowBit),
    ]
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
    /// Sorts, registers, inputs, inits and nexts: everything but the property lines.
    fn body(width: u32, regs: usize, inputs: usize, inits: &[u64], nexts: &[Expr]) -> Self {
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
        em
    }

    /// A 1-bit signal: `register == constant`, or the low bit of an expression.
    fn literal(&mut self, l: &Literal) -> i64 {
        match l {
            Literal::RegEq(i, c) => {
                let s = self.sort_w;
                let cn = self.push(format!("constd {s} {c}"));
                let r = self.reg_nids[*i];
                let s1 = self.sort_1;
                self.push(format!("eq {s1} {r} {cn}"))
            }
            Literal::LowBit(e) => {
                let n = self.emit(e);
                if self.width == 1 {
                    n
                } else {
                    let s1 = self.sort_1;
                    self.push(format!("slice {s1} {n} 0 0"))
                }
            }
            Literal::Neg(inner) => {
                let n = self.literal(inner);
                let s1 = self.sort_1;
                self.push(format!("not {s1} {n}"))
            }
        }
    }

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
            let mut em = Emitter::body(width, regs, inputs, &inits, &nexts);
            let bad_nid = em.literal(&Literal::from(bad));
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

// ── The justice pair (mununu#985): the k-liveness rung against the exact fair-cycle fixpoint ──
//
// The k-liveness rung (#984) is a new HOLDS-producing path for the justice verbs: a round
// counter over the justice set (and every `fair` literal), proved bounded as plain `bad`
// safety. A wrong monitor proves a design that has a fair infinite path. The oracle is the
// exact rung (`JusticeRung::Exact`, the fair-EG fixpoint over the full state space), complete
// on these widths. The decider here is the monitor plus the in-process native k-induction,
// not the shipped proof-directed portfolio: rIC3 and Pono are subprocesses that may be absent
// (the tier would pass by skipping, #981), and the property under test is the monitor's
// soundness, which every member inherits.

use mununu_core::adapter::btor2::kliveness_monitor::emit_kliveness_monitor;
use mununu_core::adapter::justice::{JusticeRung, verify_justice};
use mununu_core::verdict::PropertyVerdict;

/// A random design with one `justice` set (1–3 literals, or a complementary pair), optionally a `fair` literal and a
/// `constraint`.
pub fn justice_design_strategy() -> BoxedStrategy<Design> {
    (1u32..=3, 1usize..=3, 0usize..=2)
        .prop_flat_map(|(width, regs, inputs)| {
            let nexts = proptest::collection::vec(expr_strategy(regs, inputs, width, 3), regs);
            let inits = proptest::collection::vec(0..(1u64 << width), regs);
            // a third of the sets are `{l, ¬l}`: they hold iff `l` stops toggling, the case a
            // same-step conjunction gets wrong.
            let set = prop_oneof![
                2 => proptest::collection::vec(literal_strategy(regs, inputs, width), 1..=3),
                1 => literal_strategy(regs, inputs, width)
                    .prop_map(|l| vec![l.clone(), Literal::Neg(Box::new(l))]),
            ];
            let fair = proptest::option::weighted(0.3, literal_strategy(regs, inputs, width));
            let constraint =
                proptest::option::weighted(0.15, literal_strategy(regs, inputs, width));
            (
                Just(width),
                Just(regs),
                Just(inputs),
                nexts,
                inits,
                set,
                fair,
                constraint,
            )
        })
        .prop_map(
            |(width, regs, inputs, nexts, inits, set, fair, constraint)| {
                let mut em = Emitter::body(width, regs, inputs, &inits, &nexts);
                let lits: Vec<i64> = set.iter().map(|l| em.literal(l)).collect();
                if let Some(f) = fair {
                    let n = em.literal(&f);
                    em.push(format!("fair {n}"));
                }
                if let Some(c) = constraint {
                    let n = em.literal(&c);
                    em.push(format!("constraint {n}"));
                }
                let list: Vec<String> = lits.iter().map(i64::to_string).collect();
                em.push(format!("justice {} {}", lits.len(), list.join(" ")));
                Design {
                    text: em.lines.join("\n") + "\n",
                }
            },
        )
        .boxed()
}

/// The widths the tier climbs (`k = 2^w − 1` rounds); stops at the first width native leaves
/// undecided, as the shipped ladder does.
const KL_WIDTHS: &[u32] = &[1, 2, 4];

/// What a k-liveness decider says about one justice set: `Some(w)` = proved at counter width
/// `w` (the set holds), `None` = no proof.
type KlDecider = dyn Fn(&str) -> Option<u32>;

/// The rung's monitor on `text`, proved by native k-induction, width by width.
fn kliveness_native(text: &str) -> Option<u32> {
    for &w in KL_WIDTHS {
        let monitor = emit_kliveness_monitor(text, 0, w)
            .unwrap_or_else(|e| panic!("the monitor refused a generated design: {e}\n{text}"));
        let file = parser::parse(&monitor)
            .unwrap_or_else(|e| panic!("the monitor does not parse: {e}\n{monitor}"));
        match native_bmc(&file) {
            SafetyVerdict::Safe { .. } => return Some(w),
            SafetyVerdict::Violated { .. } => continue,
            SafetyVerdict::Unknown { .. } => return None,
        }
    }
    None
}

/// The exact rung's verdict on the design's one justice set.
fn exact_justice(text: &str) -> PropertyVerdict {
    let v = verify_justice(text, JusticeRung::Exact)
        .unwrap_or_else(|e| panic!("the exact rung refused a generated design: {e}\n{text}"));
    assert_eq!(v.len(), 1, "one justice set per design\n{text}");
    v[0].verdict
}

/// One justice-tier run's counts, on a run with no contradiction.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct JusticeTally {
    /// k-liveness proved the set; exact agrees (Holds).
    proved: usize,
    /// No k-liveness proof; exact Holds (a bound the ladder did not reach, or native abstained).
    unproved_holds: usize,
    /// Exact Violated (a fair infinite path); k-liveness correctly never proved it.
    violated: usize,
    /// Exact undecided.
    exact_unknown: usize,
}

/// The floor on the fixed draw: the rung must prove a share of the sets exact says hold, and
/// the draw must contain violated sets, or the "never proves a violated set" check is vacuous.
/// Set 2026-11-12 when the gate draw (48 cases) gave 24 proved / 9 unproved holds / 15
/// violated; the explorer's constraint-free draws prove 158–169 of ~220 holds per 400.
///
/// **Known explorer finding (2026-11-12):** with a `constraint` whose signals are all outside
/// the justice literals' cone, the EXACT rung reports `Violated` on a design with no
/// constraint-satisfying infinite path: the cone slice's selective pullback drops the disjoint
/// constraint. k-liveness is the one that is right there. The gate draw does not contain such
/// a case; the fix (pull back every `constraint` / `fair`) pins the shrunk design.
const MIN_PROVED_PERCENT_OF_HOLDS: usize = 50;

fn check_justice_floor(t: JusticeTally) -> Result<(), String> {
    let holds = t.proved + t.unproved_holds;
    if t.violated == 0 {
        return Err(format!("the draw has no violated justice set: {t:?}"));
    }
    if holds == 0 || t.proved * 100 < holds * MIN_PROVED_PERCENT_OF_HOLDS {
        return Err(format!(
            "k-liveness proved {}/{holds} of the sets exact says hold, under the \
             {MIN_PROVED_PERCENT_OF_HOLDS}% floor: {t:?}",
            t.proved
        ));
    }
    if t.exact_unknown > 0 {
        return Err(format!(
            "the exact oracle left {} sets undecided: {t:?}",
            t.exact_unknown
        ));
    }
    Ok(())
}

/// Draw `cases` justice designs from the tier's seed and fail on the first set the decider
/// proves while exact finds a fair infinite path.
fn run_justice_tier(
    cases: u32,
    max_shrink_iters: u32,
    kl: &KlDecider,
) -> Result<JusticeTally, String> {
    use std::cell::Cell;
    let config = ProptestConfig {
        cases,
        rng_seed: tier_seed(),
        max_shrink_iters,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = Cell::new(JusticeTally::default());
    runner
        .run(&justice_design_strategy(), |d| {
            let exact = exact_justice(&d.text);
            let proved = kl(&d.text);
            let mut t = tally.get();
            match (exact, proved) {
                (PropertyVerdict::Violated, Some(w)) => {
                    prop_assert!(
                        false,
                        "k-liveness proved the set at width {w}, exact finds a fair infinite \
                         path\n{}",
                        d.text
                    );
                }
                (PropertyVerdict::Violated, None) => t.violated += 1,
                (PropertyVerdict::Holds, Some(_)) => t.proved += 1,
                (PropertyVerdict::Holds, None) => t.unproved_holds += 1,
                _ => t.exact_unknown += 1,
            }
            tally.set(t);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    Ok(tally.get())
}

/// A k-liveness proof never contradicts the exact fair-cycle fixpoint, and the rung proves
/// enough of the draw for that to mean something.
#[test]
fn kliveness_never_proves_a_justice_set_exact_violates() {
    let tally = run_justice_tier(cases_from_env(), 1024, &kliveness_native)
        .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("btor2_differential (justice): {tally:?}");
    let explorer = std::env::var_os("MUNUNU_DIFF_TIER_EXPLORE").is_some()
        || std::env::var_os("PROPTEST_RNG_SEED").is_some()
        || std::env::var_os("PROPTEST_CASES").is_some();
    match check_justice_floor(tally) {
        Ok(()) => {}
        Err(e) if explorer => eprintln!("btor2_differential (justice): floor not met: {e}"),
        Err(e) => panic!("{e}"),
    }
}

/// The design with its justice SET collapsed to one literal, the conjunction of the set's
/// literals at the same step. Its k-liveness monitor is the wrong construction #984 pinned:
/// it counts steps where every literal holds at once, not rounds in which each held at some
/// step, so it under-counts and proves sets whose literals alternate.
fn conjoin_justice_set(text: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let pos = lines
        .iter()
        .position(|l| l.split_whitespace().nth(1) == Some("justice"))
        .expect("one justice line");
    let toks: Vec<String> = lines[pos].split_whitespace().map(str::to_string).collect();
    let lits = &toks[3..];
    let bool_sort = lines
        .iter()
        .find_map(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            (t.len() == 4 && t[1] == "sort" && t[2] == "bitvec" && t[3] == "1")
                .then(|| t[0].to_string())
        })
        .expect("a 1-bit sort");
    let mut next: i64 = lines
        .iter()
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
        + 1;
    lines.remove(pos);
    let mut acc = lits[0].clone();
    for l in &lits[1..] {
        lines.push(format!("{next} and {bool_sort} {acc} {l}"));
        acc = next.to_string();
        next += 1;
    }
    lines.push(format!("{next} justice 1 {acc}"));
    lines.join("\n") + "\n"
}

/// Negative control: the same-step-conjunction monitor must turn the gate red through the same
/// runner, on a set whose literals alternate.
#[test]
fn the_justice_gate_fails_on_a_same_step_conjunction_monitor() {
    let wrong = |text: &str| kliveness_native(&conjoin_justice_set(text));
    let err = run_justice_tier(cases_from_env(), 0, &wrong)
        .expect_err("a monitor that counts same-step conjunctions must fail the gate");
    assert!(err.contains("exact finds a fair infinite path"), "{err}");
}

/// Negative control: a decider that never proves anything contradicts nothing; the floor
/// must catch it.
#[test]
fn the_justice_gate_fails_when_kliveness_never_proves() {
    let tally = run_justice_tier(cases_from_env(), 0, &|_| None)
        .expect("a silent decider contradicts nothing");
    assert_eq!(tally.proved, 0);
    let err = check_justice_floor(tally).expect_err("a never-proving rung must fail the floor");
    assert!(err.contains("under the"), "{err}");
}

/// The conjunction rewrite itself, on a hand design: a toggle whose justice set {r0, ¬r0}
/// has a fair path (exact Violated) and whose conjunction r0 ∧ ¬r0 never holds.
#[test]
fn the_conjunction_rewrite_collapses_an_alternating_set() {
    let toggle = "1 sort bitvec 1\n2 state 1 r0\n3 zero 1\n4 init 1 2 3\n5 not 1 2\n6 next 1 2 5\n7 justice 2 2 5\n";
    assert_eq!(exact_justice(toggle), PropertyVerdict::Violated);
    assert_eq!(
        kliveness_native(toggle),
        None,
        "the rung must not prove the toggle"
    );
    let conj = conjoin_justice_set(toggle);
    assert!(
        conj.contains("and 1 2 5") && conj.contains("justice 1 8"),
        "{conj}"
    );
    assert_eq!(
        kliveness_native(&conj),
        Some(1),
        "the wrong monitor proves it at k = 1"
    );
}
