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
//! **Memory tier (mununu#959).** [`memory_design_strategy`] adds one two-cell memory (a 1-bit
//! address), whole-initialised, written from random enable / address / data expressions, read
//! by the register nexts and by `bad`, with an optional `constraint`. Each draw is rendered
//! twice: as a BTOR2 array for native BMC (QF_AUFBV), and with each cell a register for the
//! exact engine, which cannot bit-blast an array sort. A third test checks the two renderings
//! against each other with native alone, so a wrong cell rendering cannot hide behind the
//! oracle.
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
    /// mununu#959 — the same design without arrays (each memory cell a register), for an
    /// oracle that cannot bit-blast an array sort. `None`: the oracle reads `text`.
    pub oracle: Option<String>,
}

impl Design {
    fn oracle_text(&self) -> &str {
        self.oracle.as_deref().unwrap_or(&self.text)
    }
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
    /// mununu#959 — the memory cell at the low bit of the address expression. Only the memory
    /// tier draws it, so the other tiers' draws are unchanged.
    Read(Box<Expr>),
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
    mem: Option<MemRender>,
}

/// mununu#959 — how a two-cell memory is rendered: as a BTOR2 array (the engine under test) or
/// as one register per cell (the oracle). The two renderings are the same transition system.
#[derive(Debug, Clone, Copy)]
enum MemRender {
    Array { sort: i64, state: i64 },
    Cells { c0: i64, c1: i64 },
}

/// mununu#959 — a two-cell memory (1-bit address), initialised whole to `init`, and written at
/// the low bit of `addr` with `data` on every step whose `we` low bit is set.
#[derive(Debug, Clone)]
struct MemSpec {
    init: u64,
    we: Expr,
    addr: Expr,
    data: Expr,
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
            mem: None,
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
            Expr::Read(addr) => {
                let bit = self.low_bit(addr);
                let s = self.sort_w;
                match self.mem.expect("only the memory tier draws a read") {
                    MemRender::Array { state, .. } => self.push(format!("read {s} {state} {bit}")),
                    MemRender::Cells { c0, c1 } => self.push(format!("ite {s} {bit} {c1} {c0}")),
                }
            }
        }
    }

    /// The low bit of `e`, as a 1-bit signal.
    fn low_bit(&mut self, e: &Expr) -> i64 {
        let n = self.emit(e);
        if self.width == 1 {
            n
        } else {
            let s1 = self.sort_1;
            self.push(format!("slice {s1} {n} 0 0"))
        }
    }

    /// mununu#959 — sorts, registers, the memory, inputs, inits and nexts, in either rendering.
    fn memory_body(
        width: u32,
        inputs: usize,
        inits: &[u64],
        nexts: &[Expr],
        mem: &MemSpec,
        as_array: bool,
    ) -> Self {
        let mut em = Emitter {
            lines: Vec::new(),
            next_nid: 1,
            sort_w: 0,
            sort_1: 0,
            width,
            reg_nids: Vec::new(),
            in_nids: Vec::new(),
            mem: None,
        };
        em.sort_w = em.push(format!("sort bitvec {width}"));
        em.sort_1 = if width == 1 {
            em.sort_w
        } else {
            em.push("sort bitvec 1".to_string())
        };
        let s = em.sort_w;
        for i in 0..inits.len() {
            let n = em.push(format!("state {s} r{i}"));
            em.reg_nids.push(n);
        }
        em.mem = Some(if as_array {
            let s1 = em.sort_1;
            let sort = em.push(format!("sort array {s1} {s}"));
            let state = em.push(format!("state {sort} mem"));
            MemRender::Array { sort, state }
        } else {
            let c0 = em.push(format!("state {s} mem0"));
            let c1 = em.push(format!("state {s} mem1"));
            MemRender::Cells { c0, c1 }
        });
        for i in 0..inputs {
            let n = em.push(format!("input {s} i{i}"));
            em.in_nids.push(n);
        }
        for (i, c) in inits.iter().enumerate() {
            let cn = em.push(format!("constd {s} {c}"));
            let r = em.reg_nids[i];
            em.push(format!("init {s} {r} {cn}"));
        }
        let m = em.push(format!("constd {s} {}", mem.init));
        match em.mem.expect("set above") {
            // BTOR2's whole-memory init: a bit-vector value fills every cell.
            MemRender::Array { sort, state } => {
                em.push(format!("init {sort} {state} {m}"));
            }
            MemRender::Cells { c0, c1 } => {
                em.push(format!("init {s} {c0} {m}"));
                em.push(format!("init {s} {c1} {m}"));
            }
        }
        for (i, e) in nexts.iter().enumerate() {
            let n = em.emit(e);
            let r = em.reg_nids[i];
            em.push(format!("next {s} {r} {n}"));
        }
        let we = em.low_bit(&mem.we);
        let addr = em.low_bit(&mem.addr);
        let data = em.emit(&mem.data);
        let s1 = em.sort_1;
        match em.mem.expect("set above") {
            MemRender::Array { sort, state } => {
                let w = em.push(format!("write {sort} {state} {addr} {data}"));
                let n = em.push(format!("ite {sort} {we} {w} {state}"));
                em.push(format!("next {sort} {state} {n}"));
            }
            MemRender::Cells { c0, c1 } => {
                let not_addr = em.push(format!("not {s1} {addr}"));
                for (cell, sel) in [(c0, not_addr), (c1, addr)] {
                    let hit = em.push(format!("and {s1} {we} {sel}"));
                    let n = em.push(format!("ite {s} {hit} {data} {cell}"));
                    em.push(format!("next {s} {cell} {n}"));
                }
            }
        }
        em
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
                oracle: None,
            }
        })
        .boxed()
}

/// mununu#1141 — the plain draw with a `constraint` on every design (the low bit of a random
/// expression over the registers and inputs). The exact engine refuses a constrained design, so
/// the SAT tiers compare it against native BMC + k-induction instead. A constraint that no run
/// satisfies makes every `bad` unreachable, which both engines must agree on too.
pub fn constrained_design_strategy() -> BoxedStrategy<Design> {
    (1u32..=3, 1usize..=3, 0usize..=2)
        .prop_flat_map(|(width, regs, inputs)| {
            let nexts = proptest::collection::vec(expr_strategy(regs, inputs, width, 3), regs);
            let inits = proptest::collection::vec(0..(1u64 << width), regs);
            let bad = prop_oneof![
                (0..regs, 0..(1u64 << width)).prop_map(|(i, c)| (Some((i, c)), None)),
                expr_strategy(regs, inputs, width, 2).prop_map(|e| (None, Some(e))),
            ];
            let constraint = expr_strategy(regs, inputs, width, 2);
            (
                Just(width),
                Just(regs),
                Just(inputs),
                nexts,
                inits,
                bad,
                constraint,
            )
        })
        .prop_map(|(width, regs, inputs, nexts, inits, bad, constraint)| {
            let mut em = Emitter::body(width, regs, inputs, &inits, &nexts);
            let c = em.low_bit(&constraint);
            em.push(format!("constraint {c}"));
            let bad_nid = em.literal(&Literal::from(bad));
            em.push(format!("bad {bad_nid}"));
            Design {
                text: em.lines.join("\n") + "\n",
                oracle: None,
            }
        })
        .boxed()
}

/// mununu#959 — an expression that may read the memory: a plain expression, a read, or a read
/// combined with one.
fn mem_expr_strategy(regs: usize, inputs: usize, width: u32) -> BoxedStrategy<Expr> {
    let read = expr_strategy(regs, inputs, width, 1).prop_map(|a| Expr::Read(Box::new(a)));
    prop_oneof![
        expr_strategy(regs, inputs, width, 2),
        read.clone(),
        (read, expr_strategy(regs, inputs, width, 1))
            .prop_map(|(r, e)| Expr::Xor(Box::new(r), Box::new(e))),
    ]
    .boxed()
}

/// mununu#959 — the memory tier's design: 1–2 registers, 0–2 inputs, one two-cell memory
/// written from random expressions, register nexts and `bad` that may read it, and an optional
/// `constraint`. `text` renders the memory as a BTOR2 array, `oracle` as two registers.
pub fn memory_design_strategy() -> BoxedStrategy<Design> {
    (1u32..=3, 1usize..=2, 0usize..=2)
        .prop_flat_map(|(width, regs, inputs)| {
            let nexts = proptest::collection::vec(mem_expr_strategy(regs, inputs, width), regs);
            let inits = proptest::collection::vec(0..(1u64 << width), regs);
            let mem = (
                0..(1u64 << width),
                expr_strategy(regs, inputs, width, 1),
                expr_strategy(regs, inputs, width, 1),
                mem_expr_strategy(regs, inputs, width),
            )
                .prop_map(|(init, we, addr, data)| MemSpec {
                    init,
                    we,
                    addr,
                    data,
                });
            let bad = mem_expr_strategy(regs, inputs, width);
            let constraint =
                proptest::option::weighted(0.3, mem_expr_strategy(regs, inputs, width));
            (
                Just(width),
                Just(inputs),
                nexts,
                inits,
                mem,
                bad,
                constraint,
            )
        })
        .prop_map(|(width, inputs, nexts, inits, mem, bad, constraint)| {
            let render = |as_array: bool| {
                let mut em = Emitter::memory_body(width, inputs, &inits, &nexts, &mem, as_array);
                if let Some(c) = &constraint {
                    let c = em.low_bit(c);
                    em.push(format!("constraint {c}"));
                }
                let b = em.low_bit(&bad);
                em.push(format!("bad {b}"));
                em.lines.join("\n") + "\n"
            };
            Design {
                text: render(true),
                oracle: Some(render(false)),
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
    strategy: &BoxedStrategy<Design>,
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
        .run(strategy, |d| {
            let file = parser::parse(&d.text)
                .unwrap_or_else(|e| panic!("generated BTOR2 does not parse: {e}\n{}", d.text));
            let exact = exact_bad_reachable(d.oracle_text()).unwrap_or_else(|e| {
                panic!(
                    "exact engine refused a generated design: {e}\n{}",
                    d.oracle_text()
                )
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
    let tally = run_tier(&design_strategy(), cases_from_env(), 1024, native_bmc)
        .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("btor2_differential: {tally:?}");
    enforce_floor_on_the_gate_draw(tally);
}

/// The floor is measured on the gate's draw; an explorer draw or another case count only
/// reports it.
fn enforce_floor_on_the_gate_draw(tally: Tally) {
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
    let err = run_tier(&design_strategy(), cases_from_env(), 0, flipped)
        .expect_err("a native that calls a reachable bad Safe must fail the gate");
    assert!(err.contains("exact says bad reachable"), "{err}");
}

/// Negative control: a native that abstains on every case contradicts nothing, so only the
/// floor can catch it — and must.
#[test]
fn the_gate_fails_when_native_abstains_everywhere() {
    let tally = run_tier(&design_strategy(), cases_from_env(), 0, |_| {
        SafetyVerdict::Unknown { k: 0 }
    })
    .expect("an abstaining native contradicts nothing");
    assert_eq!(tally.safe + tally.violated, 0);
    let err = check_floor(tally).expect_err("an all-abstain tally must fail the floor");
    assert!(err.contains("under the"), "{err}");
}

/// mununu#959 — the memory path: native BMC / k-induction decides the array rendering under
/// QF_AUFBV, the exact engine decides the same design with each cell a register, and they must
/// agree whenever native is definite. Same floor as the bit-vector gate: measured 2026-11-18 on
/// the gate's draw at 46/48 decided (12 Safe, 34 Violated); a 300-case explorer draw decided
/// 268/300 (84 Safe, 184 Violated) with no contradiction.
#[test]
fn native_bmc_on_memories_never_contradicts_the_exact_engine() {
    let tally = run_tier(
        &memory_design_strategy(),
        cases_from_env(),
        1024,
        native_bmc,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("btor2_differential memory: {tally:?}");
    enforce_floor_on_the_gate_draw(tally);
}

/// mununu#959 negative control: a native that calls every reachable `bad` Safe turns the memory
/// gate red, so the tier's memory draws do reach reachable and unreachable cases.
#[test]
fn the_memory_gate_fails_when_native_flips_a_verdict() {
    let flipped = |f: &Btor2File| match native_bmc(f) {
        SafetyVerdict::Violated { depth } => SafetyVerdict::Safe { k: depth },
        v => v,
    };
    let err = run_tier(&memory_design_strategy(), cases_from_env(), 0, flipped)
        .expect_err("a native that calls a reachable bad Safe must fail the memory gate");
    assert!(err.contains("exact says bad reachable"), "{err}");
}

/// mununu#959 — the two renderings are one transition system: native BMC run on each must
/// reach the same definite verdict. A wrong cell rendering would make the exact oracle check
/// a different design; this catches it without trusting that oracle.
#[test]
fn the_array_and_cell_renderings_of_a_memory_agree() {
    let config = ProptestConfig {
        cases: cases_from_env(),
        rng_seed: tier_seed(),
        max_shrink_iters: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    runner
        .run(&memory_design_strategy(), |d| {
            let array = parser::parse(&d.text).expect("the array rendering parses");
            let cells = parser::parse(d.oracle_text()).expect("the cell rendering parses");
            let definite = |v: SafetyVerdict| match v {
                SafetyVerdict::Violated { .. } => Some(true),
                SafetyVerdict::Safe { .. } => Some(false),
                SafetyVerdict::Unknown { .. } => None,
            };
            if let (Some(a), Some(c)) = (definite(native_bmc(&array)), definite(native_bmc(&cells)))
            {
                prop_assert_eq!(
                    a,
                    c,
                    "array says reachable={}, cells say reachable={}\n{}\n---\n{}",
                    a,
                    c,
                    d.text,
                    d.oracle_text()
                );
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
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
                    oracle: None,
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

// ---------------------------------------------------------------------------------------------
// Track V (mununu#1145) — the response k-liveness fallback (`emit_response_kliveness_monitor`,
// the HOLDS-only rung behind `sv verify-liveness` / `btor2 verify-liveness`) against the exact
// fair-cycle rung, on random response draws. The oracle is an independent reduction of
// `¬(GF fair → AG(ante → AF cons))` to one justice set: a free `commit` input that may latch
// `alive` on a step where `ante` holds, `alive` cleared — and a `failed` sink entered for good —
// on the first `cons` after it, so `GF alive` is exactly "some commitment is never discharged":
// a fair path with `ante` at some step and `¬cons` from then on. The decider is the shipped
// monitor plus native k-induction, width by width, as the justice tier runs its rung.

// The decider is the shipped monitor plus the owned IC3, width by width — not native
// k-induction as in the justice tier: the response monitor has a free `commit` input, so it is
// not k-inductive (an arbitrary start with the counter one short of overflow and a late commit
// is a spurious bad at every k; the first gate run proved the toggling responder at no width
// that way). The shipped ladder runs it on the proof-directed members for the same reason.
use mununu_core::adapter::btor2::kliveness_monitor::emit_response_kliveness_monitor;
use mununu_core::adapter::btor2::predicate_expr::CmpOp;

/// An atom as the response monitor takes it: the register's name, the comparison, the value.
type NamedAtom = (String, CmpOp, u128);

/// One response case: a property-free design, `ante` / `cons` as `(register, value)` equalities,
/// an optional fairness equality, and the oracle's justice design over the same body.
#[derive(Debug, Clone)]
struct ResponseCase {
    design: String,
    oracle: String,
    ante: (usize, u64),
    cons: (usize, u64),
    fair: Option<(usize, u64)>,
}

impl ResponseCase {
    /// The atoms by the registers' names, the shape the monitor takes.
    fn atoms(&self) -> (NamedAtom, NamedAtom, Vec<NamedAtom>) {
        let atom = |(i, v): (usize, u64)| (format!("r{i}"), CmpOp::Eq, u128::from(v));
        (
            atom(self.ante),
            atom(self.cons),
            self.fair.map(atom).into_iter().collect(),
        )
    }

    /// Build the case over an emitted body: the design is the body as it stands; the oracle
    /// appends the commit / alive / failed machinery and the justice set.
    fn over(
        mut em: Emitter,
        ante: (usize, u64),
        cons: (usize, u64),
        fair: Option<(usize, u64)>,
    ) -> Self {
        let design = em.lines.join("\n") + "\n";
        let a = em.literal(&Literal::RegEq(ante.0, ante.1));
        let c = em.literal(&Literal::RegEq(cons.0, cons.1));
        let f = fair.map(|(i, v)| em.literal(&Literal::RegEq(i, v)));
        let s1 = em.sort_1;
        let commit = em.push(format!("input {s1} commit"));
        let alive = em.push(format!("state {s1} alive"));
        let failed = em.push(format!("state {s1} failed"));
        let zero = em.push(format!("zero {s1}"));
        em.push(format!("init {s1} {alive} {zero}"));
        em.push(format!("init {s1} {failed} {zero}"));
        let committed = em.push(format!("and {s1} {commit} {a}"));
        let alive_or_committed = em.push(format!("or {s1} {alive} {committed}"));
        let not_c = em.push(format!("not {s1} {c}"));
        let candidate = em.push(format!("and {s1} {alive_or_committed} {not_c}"));
        let discharged = em.push(format!("and {s1} {alive} {c}"));
        let failed_next = em.push(format!("or {s1} {failed} {discharged}"));
        let not_failed_next = em.push(format!("not {s1} {failed_next}"));
        let alive_next = em.push(format!("and {s1} {candidate} {not_failed_next}"));
        em.push(format!("next {s1} {alive} {alive_next}"));
        em.push(format!("next {s1} {failed} {failed_next}"));
        match f {
            None => em.push(format!("justice 1 {alive}")),
            Some(f) => em.push(format!("justice 2 {alive} {f}")),
        };
        ResponseCase {
            design,
            oracle: em.lines.join("\n") + "\n",
            ante,
            cons,
            fair,
        }
    }
}

/// A random response case: the justice tier's body shapes, two register equalities for the
/// property and a fairness equality on a third of the draws.
fn response_case_strategy() -> BoxedStrategy<ResponseCase> {
    (1u32..=3, 1usize..=3, 0usize..=2)
        .prop_flat_map(|(width, regs, inputs)| {
            let nexts = proptest::collection::vec(expr_strategy(regs, inputs, width, 3), regs);
            let inits = proptest::collection::vec(0..(1u64 << width), regs);
            let eq = move || (0..regs, 0..(1u64 << width));
            let fair = proptest::option::weighted(0.3, eq());
            (
                Just(width),
                Just(regs),
                Just(inputs),
                nexts,
                inits,
                eq(),
                eq(),
                fair,
            )
        })
        .prop_map(|(width, regs, inputs, nexts, inits, ante, cons, fair)| {
            ResponseCase::over(
                Emitter::body(width, regs, inputs, &inits, &nexts),
                ante,
                cons,
                fair,
            )
        })
        .boxed()
}

/// The exact rung's verdict on the oracle's one justice set: `Violated` = a fair undischarged
/// commitment exists = the response property is violated; `Holds` = it holds.
fn exact_response(case: &ResponseCase) -> PropertyVerdict {
    exact_justice(&case.oracle)
}

/// What a response decider says: `Some(w)` = the monitor proved the property at counter width
/// `w`, `None` = no proof.
type ResponseDecider = dyn Fn(&ResponseCase) -> Option<u32>;

/// The shipped monitor over `case.design`, proved by the owned IC3, width by width (a
/// `Violated` monitor at a width is the counter overflowing: the next width is tried; a stop
/// is no proof).
fn response_kliveness_ic3(case: &ResponseCase) -> Option<u32> {
    let (ante, cons, fairs) = case.atoms();
    let fairs: Vec<(&str, CmpOp, u128)> =
        fairs.iter().map(|(n, o, v)| (n.as_str(), *o, *v)).collect();
    for &w in KL_WIDTHS {
        let monitor = emit_response_kliveness_monitor(
            &case.design,
            (&ante.0, ante.1, ante.2),
            (&cons.0, cons.1, cons.2),
            &fairs,
            false,
            w,
        )
        .unwrap_or_else(|e| {
            panic!(
                "the response monitor refused a generated design: {e}\n{}",
                case.design
            )
        });
        let file = parser::parse(&monitor)
            .unwrap_or_else(|e| panic!("the response monitor does not parse: {e}\n{monitor}"));
        let opts = Ic3Options {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
            cancel: None,
            max_frames: None,
        };
        match ic3(&file, &opts) {
            Ok(Ic3Outcome::Safe { .. }) => return Some(w),
            Ok(Ic3Outcome::Violated { .. }) => continue,
            Ok(Ic3Outcome::Stopped { .. }) => return None,
            Err(e) => panic!("the owned IC3 refused a response monitor: {e}\n{monitor}"),
        }
    }
    None
}

/// The floor on the fixed draw, as the justice tier's: the fallback must prove a share of the
/// properties exact says hold, and the draw must contain violated ones. First measurement on
/// the gate draw (2026-11-20, 48 cases, the owned IC3 as decider): 30 proved / 0 unproved holds
/// / 18 violated; the floor is raised from the second draw.
const MIN_RESPONSE_PROVED_PERCENT_OF_HOLDS: usize = 30;

fn check_response_floor(t: JusticeTally) -> Result<(), String> {
    let holds = t.proved + t.unproved_holds;
    if t.violated == 0 {
        return Err(format!("the draw has no violated response property: {t:?}"));
    }
    if holds == 0 || t.proved * 100 < holds * MIN_RESPONSE_PROVED_PERCENT_OF_HOLDS {
        return Err(format!(
            "the response fallback proved {}/{holds} of the properties exact says hold, under \
             the {MIN_RESPONSE_PROVED_PERCENT_OF_HOLDS}% floor: {t:?}",
            t.proved
        ));
    }
    if t.exact_unknown > 0 {
        return Err(format!(
            "the exact oracle left {} properties undecided: {t:?}",
            t.exact_unknown
        ));
    }
    Ok(())
}

/// Draw `cases` response cases from the tier's seed and fail on the first property the decider
/// proves while exact finds a fair undischarged commitment.
fn run_response_tier(
    cases: u32,
    max_shrink_iters: u32,
    decider: &ResponseDecider,
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
        .run(&response_case_strategy(), |c| {
            let exact = exact_response(&c);
            let proved = decider(&c);
            let mut t = tally.get();
            match (exact, proved) {
                (PropertyVerdict::Violated, Some(w)) => {
                    prop_assert!(
                        false,
                        "the response fallback proved AG(r{}=={} → AF r{}=={}){} at width {w}, \
                         exact finds a fair undischarged commitment\n{}",
                        c.ante.0,
                        c.ante.1,
                        c.cons.0,
                        c.cons.1,
                        c.fair
                            .map(|(i, v)| format!(" under GF r{i}=={v}"))
                            .unwrap_or_default(),
                        c.design
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

/// The response fallback never proves a property the exact fair-cycle rung violates, and it
/// proves enough of the draw for that to mean something.
#[test]
fn response_kliveness_never_proves_a_response_exact_violates() {
    let tally = run_response_tier(cases_from_env(), 1024, &response_kliveness_ic3)
        .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("btor2_differential (response): {tally:?}");
    let explorer = std::env::var_os("MUNUNU_DIFF_TIER_EXPLORE").is_some()
        || std::env::var_os("PROPTEST_RNG_SEED").is_some()
        || std::env::var_os("PROPTEST_CASES").is_some();
    match check_response_floor(tally) {
        Ok(()) => {}
        Err(e) if explorer => eprintln!("btor2_differential (response): floor not met: {e}"),
        Err(e) => panic!("{e}"),
    }
}

/// Negative control: a monitor that discharges on the antecedent itself (`cons := ante`, the
/// property `AG(a → AF a)`) proves everything, so the gate must fail on the draw's violated
/// cases — the gate can see a wrong monitor.
#[test]
fn the_response_gate_fails_on_a_monitor_that_discharges_on_its_antecedent() {
    let wrong = |case: &ResponseCase| {
        let discharging = ResponseCase {
            cons: case.ante,
            ..case.clone()
        };
        response_kliveness_ic3(&discharging)
    };
    let err = run_response_tier(cases_from_env(), 0, &wrong)
        .expect_err("the discharging monitor proves a violated property");
    assert!(
        err.contains("exact finds a fair undischarged commitment"),
        "{err}"
    );
}

/// The oracle on the three hand shapes the monitor's own tests use: a responder (holds), a
/// staller (violated), and a fairness-gated design (violated without the fairness, holds with
/// it) — so a wrong reduction would show here before the draw.
#[test]
fn the_response_oracle_decides_the_three_hand_shapes() {
    // r0 toggles: every r0==1 is followed by r0==0.
    let responder = ResponseCase::over(
        Emitter::body(1, 1, 0, &[0], &[Expr::Not(Box::new(Expr::Reg(0)))]),
        (0, 1),
        (0, 0),
        None,
    );
    assert_eq!(
        exact_response(&responder),
        PropertyVerdict::Holds,
        "{}",
        responder.oracle
    );
    // r0 latches on i0 and never clears: r0==1 is never followed by r0==0 on that path.
    let staller = ResponseCase::over(
        Emitter::body(
            1,
            1,
            1,
            &[0],
            &[Expr::Or(Box::new(Expr::Reg(0)), Box::new(Expr::In(0)))],
        ),
        (0, 1),
        (0, 0),
        None,
    );
    assert_eq!(
        exact_response(&staller),
        PropertyVerdict::Violated,
        "{}",
        staller.oracle
    );
    // r0 copies i0; r1 copies i0 too. Without fairness i0 can stay 1 (violated); under
    // GF r1==0 the input is 0 infinitely often, so r0==0 recurs (holds).
    let gated = |fair| {
        ResponseCase::over(
            Emitter::body(1, 2, 1, &[0, 0], &[Expr::In(0), Expr::In(0)]),
            (0, 1),
            (0, 0),
            fair,
        )
    };
    assert_eq!(exact_response(&gated(None)), PropertyVerdict::Violated);
    assert_eq!(exact_response(&gated(Some((1, 0)))), PropertyVerdict::Holds);
    // And the shipped monitor agrees on all three (proves the two that hold, never the staller).
    assert!(response_kliveness_ic3(&responder).is_some());
    assert!(response_kliveness_ic3(&staller).is_none());
    assert!(response_kliveness_ic3(&gated(Some((1, 0)))).is_some());
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

// ---------------------------------------------------------------------------------------------
// N5.0 — the owned bit-level BMC (AIG → CNF → CaDiCaL) against the exact engine, both ways.
//
// A generated design has at most 9 state bits, so a search to depth 2^bits covers every
// reachable state: on this tier the bounded engine is complete, and its answer must EQUAL the
// exact engine's, not merely not contradict it. Every counterexample is replayed on the concrete
// simulator (a probe register latches `bad`), so a wrong trace fails too, not only a wrong verdict.
// ---------------------------------------------------------------------------------------------

use mununu_core::adapter::btor2::ast::Node;
use mununu_core::adapter::btor2::bit_blast::simulate_one_step_observe;
use mununu_core::adapter::btor2::sat_bmc::{DynamicStep, SatBmcOptions, SatBmcOutcome, sat_bmc};

/// mununu#1141 — the oracle for a constrained draw: native BMC + k-induction to 16 (the exact
/// engine refuses a `constraint`). `None` when it abstained.
fn native_oracle(text: &str) -> Option<bool> {
    let file = parser::parse(text).expect("parses");
    match decide_bad_safety(&file, 16, Some(5_000))
        .unwrap_or_else(|e| panic!("native engine refused a generated design: {e:?}\n{text}"))
    {
        SafetyVerdict::Violated { .. } => Some(true),
        SafetyVerdict::Safe { .. } => Some(false),
        _ => None,
    }
}

/// mununu#1141 — the shipped dynamic step against native BMC + k-induction on constrained
/// draws: a `Violated` must be a real constrained path (the replay checks every frame's
/// constraint), and "no counterexample through 16" must not contradict a native counterexample.
/// On the parent the step asserted every window frame's constraint as a unit clause, so a bad
/// whose continuations later break the constraint was refuted — the kind of row only a
/// constrained draw can catch.
#[test]
fn sat_bmc_dynamic_step_agrees_with_native_bmc_on_constrained_draws() {
    let config = ProptestConfig {
        cases: cases_from_env().max(96),
        rng_seed: tier_seed(),
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = std::cell::Cell::new((0usize, 0usize, 0usize));
    runner
        .run(&constrained_design_strategy(), |d| {
            let file = parser::parse(&d.text).expect("parses");
            let oracle = native_oracle(&d.text);
            let out = sat_bmc(
                &file,
                &SatBmcOptions {
                    max_depth: 16,
                    dynamic_step: Some(DynamicStep::default()),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("sat-bmc refused a generated design: {e}\n{}", d.text));
            let (mut v, mut n, mut u) = tally.get();
            match out {
                SatBmcOutcome::Violated {
                    depth,
                    initial,
                    inputs,
                    ..
                } => {
                    prop_assert!(
                        oracle != Some(false),
                        "sat-bmc: violated at {depth}, native: safe\n{}",
                        d.text
                    );
                    if let Err(e) = replay(&d.text, depth, &initial, &inputs) {
                        prop_assert!(false, "the trace does not replay: {e}\n{}", d.text);
                    }
                    v += 1;
                }
                SatBmcOutcome::NoCounterexample { depth } => {
                    prop_assert!(
                        oracle != Some(true),
                        "sat-bmc: none to depth {depth}, native: reachable within 16\n{}",
                        d.text
                    );
                    if oracle.is_some() {
                        n += 1;
                    } else {
                        u += 1;
                    }
                }
                SatBmcOutcome::Stopped { depth, reason } => {
                    prop_assert!(
                        false,
                        "sat-bmc stopped at {depth} with no budget set: {reason}"
                    );
                }
            }
            tally.set((v, n, u));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let (v, n, u) = tally.get();
    eprintln!(
        "sat_bmc constrained tier: {v} violated (replayed), {n} unreachable (native agrees), {u} native-undecided"
    );
    assert!(
        v > 0 && n > 0,
        "the draw must exercise both answers: {v} violated, {n} unreachable"
    );
}

fn state_bits(file: &Btor2File) -> u32 {
    file.lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { sort, .. } => parser::bv_width(file, *sort),
            _ => None,
        })
        .sum()
}

/// `text` with a 1-bit probe register `mununu_bad_probe` whose `next` is the (single) `bad`
/// signal: after stepping frame k, the probe holds `bad` at frame k.
fn with_bad_probe(text: &str) -> String {
    let max = text
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.parse::<i64>().ok())
        .max()
        .unwrap_or(0);
    let sig = text
        .lines()
        .find_map(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            (t.get(1) == Some(&"bad")).then(|| t[2].to_string())
        })
        .expect("a bad line");
    format!(
        "{text}{} sort bitvec 1\n{} state {} mununu_bad_probe\n{} next {} {} {sig}\n",
        max + 1,
        max + 2,
        max + 1,
        max + 3,
        max + 1,
        max + 2
    )
}

/// Replay a SAT-BMC counterexample on the concrete simulator; `Err` says where it breaks.
fn replay(
    text: &str,
    depth: u32,
    initial: &[(String, u128)],
    inputs: &[Vec<(String, u128)>],
) -> Result<(), String> {
    let probed = parser::parse(&with_bad_probe(text)).map_err(|e| e.to_string())?;
    let mut state: std::collections::HashMap<String, u128> = initial.iter().cloned().collect();
    state.insert("mununu_bad_probe".into(), 0);
    for (f, frame) in inputs.iter().enumerate().take(depth as usize + 1) {
        let ins: std::collections::HashMap<String, u128> = frame.iter().cloned().collect();
        let out =
            simulate_one_step_observe(&probed, &state, &ins, &[]).map_err(|e| e.to_string())?;
        if !out.admissible {
            return Err(format!("a constraint fails at frame {f}"));
        }
        state = out.next_state;
    }
    match state.get("mununu_bad_probe") {
        Some(1) => Ok(()),
        other => Err(format!("bad is {other:?} at depth {depth} on replay")),
    }
}

#[test]
fn sat_bmc_equals_the_exact_engine_and_its_traces_replay() {
    let config = ProptestConfig {
        cases: cases_from_env().max(96),
        rng_seed: tier_seed(),
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = std::cell::Cell::new((0usize, 0usize));
    runner
        .run(&design_strategy(), |d| {
            let file = parser::parse(&d.text).expect("parses");
            let exact = exact_bad_reachable(&d.text).expect("exact decides");
            let bound = 1u32 << state_bits(&file);
            // mununu#1141 — the member runs the default dynamic step; the tier used to run
            // only the fixed step, so the shipped search had no cheap-tier oracle.
            for dynamic_step in [None, Some(DynamicStep::default())] {
                let out = sat_bmc(
                    &file,
                    &SatBmcOptions {
                        max_depth: bound,
                        dynamic_step,
                        ..Default::default()
                    },
                )
                .unwrap_or_else(|e| panic!("sat-bmc refused a generated design: {e}\n{}", d.text));
                let (mut v, mut n) = tally.get();
                match out {
                    SatBmcOutcome::Violated {
                        depth,
                        initial,
                        inputs,
                        ..
                    } => {
                        prop_assert!(
                            exact,
                            "sat-bmc: violated at {depth}, exact: unreachable\n{}",
                            d.text
                        );
                        if let Err(e) = replay(&d.text, depth, &initial, &inputs) {
                            prop_assert!(false, "the trace does not replay: {e}\n{}", d.text);
                        }
                        v += 1;
                    }
                    SatBmcOutcome::NoCounterexample { .. } => {
                        prop_assert!(
                            !exact,
                            "sat-bmc: none to depth {bound}, exact: reachable\n{}",
                            d.text
                        );
                        n += 1;
                    }
                    SatBmcOutcome::Stopped { depth, reason } => {
                        prop_assert!(
                            false,
                            "sat-bmc stopped at {depth} with no budget set: {reason}"
                        );
                    }
                }
                tally.set((v, n));
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let (v, n) = tally.get();
    eprintln!("sat_bmc tier (fixed + dynamic step): {v} violated (replayed), {n} unreachable");
    assert!(
        v > 0 && n > 0,
        "the draw must exercise both answers: {v} violated, {n} unreachable"
    );
}

/// Negative control: the replay must reject a trace that does not reach `bad`.
#[test]
fn the_replay_rejects_a_trace_that_misses_bad() {
    let text = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n\
                7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";
    let initial = vec![("c".to_string(), 0)];
    let inputs = vec![Vec::new(); 8];
    assert!(replay(text, 7, &initial, &inputs).is_ok());
    assert!(replay(text, 6, &initial, &inputs).is_err());
}

// ---- N5.1: the owned IC3 against the exact engine ------------------------------------------------
use mununu_core::adapter::btor2::ic3::{Ic3Options, Ic3Outcome, Replay, ic3};

/// Every verdict the owned IC3 gives on the shared draw agrees with the exact engine; every
/// `Violated` replays on the concrete simulator (the engine's own replay AND this file's, two
/// independent readings of the trace); every `Safe` carries a certified invariant. With no budget
/// set the engine may not stop.
#[test]
fn ic3_equals_the_exact_engine_and_its_traces_replay() {
    let config = ProptestConfig {
        cases: cases_from_env().max(96),
        rng_seed: tier_seed(),
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = std::cell::Cell::new((0usize, 0usize));
    runner
        .run(&design_strategy(), |d| {
            let file = parser::parse(&d.text).expect("parses");
            let exact = exact_bad_reachable(&d.text).expect("exact decides");
            let out = ic3(&file, &Ic3Options::default())
                .unwrap_or_else(|e| panic!("ic3 refused a generated design: {e}\n{}", d.text));
            let (mut v, mut n) = tally.get();
            match out {
                Ic3Outcome::Violated {
                    depth,
                    initial,
                    inputs,
                    replay: status,
                    ..
                } => {
                    prop_assert!(
                        exact,
                        "ic3: violated at {depth}, exact: unreachable\n{}",
                        d.text
                    );
                    prop_assert!(
                        matches!(status, Replay::Confirmed(_)),
                        "{:?}\n{}",
                        status,
                        d.text
                    );
                    if let Err(e) = replay(&d.text, depth, &initial, &inputs) {
                        prop_assert!(false, "the trace does not replay: {e}\n{}", d.text);
                    }
                    v += 1;
                }
                Ic3Outcome::Safe { frames, clauses } => {
                    prop_assert!(
                        !exact,
                        "ic3: safe at frame {frames} ({clauses} clauses), exact: reachable\n{}",
                        d.text
                    );
                    n += 1;
                }
                Ic3Outcome::Stopped { frames, reason } => {
                    prop_assert!(
                        false,
                        "ic3 stopped at frame {frames} with no budget set: {reason}\n{}",
                        d.text
                    );
                }
            }
            tally.set((v, n));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let (v, n) = tally.get();
    eprintln!("ic3 tier: {v} violated (replayed), {n} safe (certified)");
    assert!(
        v > 0 && n > 0,
        "the draw must exercise both answers: {v} violated, {n} safe"
    );
}

/// mununu#1141 — IC3 against native BMC + k-induction on constrained draws (the #1056 / #1072
/// class was found on HWMCC, never by a tier): a `Safe` must not contradict a native
/// counterexample, a `Violated` must not contradict a native proof and must replay under the
/// constraints.
#[test]
fn ic3_agrees_with_native_bmc_on_constrained_draws() {
    let config = ProptestConfig {
        cases: cases_from_env().max(96),
        rng_seed: tier_seed(),
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = proptest::test_runner::TestRunner::new(config);
    let tally = std::cell::Cell::new((0usize, 0usize));
    runner
        .run(&constrained_design_strategy(), |d| {
            let file = parser::parse(&d.text).expect("parses");
            let oracle = native_oracle(&d.text);
            let out = ic3(&file, &Ic3Options::default())
                .unwrap_or_else(|e| panic!("ic3 refused a generated design: {e}\n{}", d.text));
            let (mut v, mut n) = tally.get();
            match out {
                Ic3Outcome::Violated {
                    depth,
                    initial,
                    inputs,
                    ..
                } => {
                    prop_assert!(
                        oracle != Some(false),
                        "ic3: violated at {depth}, native: safe\n{}",
                        d.text
                    );
                    if let Err(e) = replay(&d.text, depth, &initial, &inputs) {
                        prop_assert!(false, "the trace does not replay: {e}\n{}", d.text);
                    }
                    v += 1;
                }
                Ic3Outcome::Safe { .. } => {
                    prop_assert!(
                        oracle != Some(true),
                        "ic3: safe, native: reachable within 16\n{}",
                        d.text
                    );
                    n += 1;
                }
                _ => {}
            }
            tally.set((v, n));
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let (v, n) = tally.get();
    eprintln!("ic3 constrained tier: {v} violated (replayed), {n} safe (certified)");
    assert!(
        v > 0 && n > 0,
        "the draw must exercise both answers: {v} violated, {n} safe"
    );
}
