//! N0.6 — the shared random-BTOR2 tier: small transition systems with a `bad` node,
//! decided by two engines that must agree whenever both are definite.
//!
//! - `exact_bad_reachable` (full-state ROBDD, complete on these widths) is the oracle;
//! - `decide_bad_safety` (native BMC + k-induction on z3) must never contradict it: a
//!   `Violated` must be a real counterexample (the exact engine says reachable), a `Safe`
//!   a real proof (unreachable). `Unknown` is allowed.
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
use mununu_core::adapter::btor2::native_bmc::{SafetyVerdict, decide_bad_safety};
use mununu_core::adapter::btor2::parser;
use mununu_core::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
use proptest::prelude::*;

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

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(48),
        ..ProptestConfig::default()
    })]

    /// Every generated design parses, and the two engines agree whenever both decide.
    #[test]
    fn native_bmc_never_contradicts_the_exact_engine(d in design_strategy()) {
        let file = parser::parse(&d.text).unwrap_or_else(|e| panic!("generated BTOR2 does not parse: {e}\n{}", d.text));
        let exact = exact_bad_reachable(&d.text)
            .unwrap_or_else(|e| panic!("exact engine refused a generated design: {e}\n{}", d.text));
        let native = decide_bad_safety(&file, 16, Some(2_000))
            .unwrap_or_else(|e| panic!("native engine refused a generated design: {e:?}\n{}", d.text));
        match native {
            SafetyVerdict::Violated { depth } => prop_assert!(
                exact, "native says bad reachable at depth {depth}, exact says unreachable\n{}", d.text),
            SafetyVerdict::Safe { k } => prop_assert!(
                !exact, "native says {k}-inductive safe, exact says bad reachable\n{}", d.text),
            SafetyVerdict::Unknown { .. } => {}
        }
    }
}
