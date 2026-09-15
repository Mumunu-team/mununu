//! W-2′ — the **bit-level interaction graph**: which individual BDD variables a design's operators
//! actually relate to one another.
//!
//! # Why this exists
//!
//! The exact engine's variable order was only ever CELL-MAJOR (every bit of one register
//! contiguous). Measured, that is the textbook bad order for relational structure — `a == b` is
//! Θ(2^n) cell-major and O(n) interleaved (84× / 806× / 2608× at n = 12 / 16 / 18), and a barrel
//! shifter is 65 537 nodes cell-major against 1 interleaved. But blind interleaving regressed a
//! consumer block 10.9×, so neither fixed layout is right and we need a way to CHOOSE per cone.
//!
//! W-1 tried the literature's answer — Weighted Event Span over a dependency matrix, minimised by
//! Cuthill–McKee / Sloan. It failed, and the failure was instructive: **the graph was wrong, not
//! the algorithms.** A cell-level incidence structure expands "this event touches `a`" into edges
//! against ALL bits of `a`, so `a == b` becomes a complete bipartite blob whose span is maximal
//! under every order. In the literature a *variable* is a whole state component; ours is a BIT.
//!
//! This module builds the graph at the granularity that matters. The edges are not new theory —
//! they are a reading of what [`super::symbolic_bitblast`]'s `eval_op` already does:
//!
//! | operator | edges emitted |
//! |---|---|
//! | `and` / `or` / `xor` / `eq` / `iff` … | `a_i — b_i`, **aligned and sparse** |
//! | `add` / `sub` / comparisons | `a_i — b_i`, plus the `a_i — a_{i+1}` carry/borrow chain |
//! | `sll` / `srl` / `sra` with a SYMBOLIC amount | every amount bit — every data bit (**dense**) |
//! | `mul` / division / remainder | dense — no order helps (Bryant 1986) |
//! | `concat` / `slice` / `uext` / `sext` | re-map only; no new coupling |
//! | `ite` | condition bits — every output bit |
//!
//! A bandwidth/wavefront reduction over THIS graph places `a_i` beside `b_i`, i.e. it **derives**
//! interleaving where the structure is aligned and grouping where it is not.
//!
//! # What this module does NOT do
//!
//! It computes no order and changes no verdict. It is the input a reordering pass would consume,
//! built and validated separately so the graph can be judged before anything depends on it.

use super::ast::{Btor2File, Nid, Node, Op, Sort};
use std::collections::{HashMap, HashSet};

/// One BDD variable: a bit of a leaf (state/input) cell.
pub type BitRef = (Nid, usize);

/// The bit-level interaction graph of a design.
#[derive(Debug, Default, Clone)]
pub struct BitGraph {
    /// Undirected edges, each stored once with the smaller endpoint first.
    pub edges: HashSet<(BitRef, BitRef)>,
    /// Every leaf bit the design declares, whether or not it has an edge.
    pub nodes: HashSet<BitRef>,
    /// Bits that SELECT rather than combine: a symbolic shift amount, an `ite` condition. They are
    /// dense against the data they control, but unlike a multiplier's operands that density is
    /// ORDERABLE — branch on the selector first and the rest is a wire. Distinguishing them is what
    /// separates a barrel shifter (interleaving wins 65 537×) from a multiplier (order-immune),
    /// which look identical on edge density alone.
    pub selectors: HashSet<BitRef>,
}

impl BitGraph {
    fn add_edge(&mut self, a: BitRef, b: BitRef) {
        if a == b {
            return; // a bit interacting with itself carries no ordering information
        }
        self.edges.insert(if a < b { (a, b) } else { (b, a) });
    }

    /// Edges between every member of `xs` and every member of `ys` — the DENSE case (`mul`, a
    /// symbolic shift amount). A dense neighbourhood is itself the useful signal: it says no order
    /// helps this operator, which is Bryant's 1986 result for multiplication.
    fn add_cross(&mut self, xs: &HashSet<BitRef>, ys: &HashSet<BitRef>) {
        for x in xs {
            for y in ys {
                self.add_edge(*x, *y);
            }
        }
    }

    /// Mean degree — a cheap density summary. High mean degree means the graph is close to
    /// complete, so no linear order can do much and a reordering pass should decline rather than
    /// pay for one.
    #[must_use]
    pub fn mean_degree(&self) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        let mut deg: HashMap<BitRef, usize> = HashMap::new();
        for (a, b) in &self.edges {
            *deg.entry(*a).or_default() += 1;
            *deg.entry(*b).or_default() += 1;
        }
        deg.values().sum::<usize>() as f64 / self.nodes.len() as f64
    }
}

/// Per-bit support: for one node of the DAG, which LEAF bits each of its output bits depends on.
type Support = Vec<HashSet<BitRef>>;

/// Build the bit-level interaction graph for `file`.
///
/// Walks the design once in declaration order (BTOR2 is topologically sorted by construction: an
/// operand's NID always precedes its use), carrying a per-bit support for every node and emitting
/// edges at each operator according to the table in the module docs.
#[must_use]
pub fn build(file: &Btor2File) -> BitGraph {
    let mut g = BitGraph::default();
    let mut support: HashMap<Nid, Support> = HashMap::new();
    // Which constants are POWERS OF TWO. `x * 2^k` and `x << k` lift to pure wiring — no bit of
    // `x` meets any other — whereas `x * 3` is `x + (x << 1)` and does carry. Distinguishing them
    // matters: a consumer's block carries eight 32-bit multiplies that are ALL by constant powers
    // of two, and treating those as dense would have mispredicted the whole block.
    let mut pow2: HashSet<Nid> = HashSet::new();

    let widths = sort_widths(file);
    let width_of = |nid: Nid, support: &HashMap<Nid, Support>| -> usize {
        support.get(&nid).map_or(0, Vec::len)
    };

    for line in &file.lines {
        match &line.node {
            Node::State { sort, .. } | Node::Input { sort, .. } => {
                let w = widths.get(sort).copied().unwrap_or(0);
                let bits: Support = (0..w)
                    .map(|i| {
                        g.nodes.insert((line.nid, i));
                        HashSet::from([(line.nid, i)])
                    })
                    .collect();
                support.insert(line.nid, bits);
            }
            Node::Const { sort, value } => {
                let w = widths.get(sort).copied().unwrap_or(0);
                if const_is_pow2(value) {
                    pow2.insert(line.nid);
                }
                // A constant depends on nothing: empty support, so it contributes no edges. This
                // is why `x * 32` is free while `x * y` is not.
                support.insert(line.nid, vec![HashSet::new(); w]);
            }
            Node::Op { op, sort, args, .. } => {
                let w = widths.get(sort).copied().unwrap_or(0);
                let a = args.first().map(|o| o.nid());
                let b = args.get(1).map(|o| o.nid());
                let out = eval(&mut g, &support, *op, w, a, b, args, &width_of, &pow2);
                support.insert(line.nid, out);
            }
            _ => {}
        }
    }
    g
}

/// Is this constant a power of two? `x * 2^k` and `x << k` are pure wiring; `x * 3` carries.
fn const_is_pow2(v: &super::ast::ConstValue) -> bool {
    use super::ast::ConstValue as C;
    match v {
        C::Zero | C::One => true,
        C::Dec(d) => *d > 0 && (*d as u128).is_power_of_two(),
        C::Bin(b) => b.chars().filter(|c| *c == '1').count() == 1,
        // A hex literal is a power of two iff exactly one bit is set.
        C::Hex(h) => {
            u128::from_str_radix(h.trim_start_matches("0x"), 16).is_ok_and(u128::is_power_of_two)
        }
        C::Ones => false,
    }
}

/// `sort nid -> width`, for the bit-vector sorts.
fn sort_widths(file: &Btor2File) -> HashMap<Nid, usize> {
    file.lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width },
            } => Some((l.nid, *width as usize)),
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn eval(
    g: &mut BitGraph,
    support: &HashMap<Nid, Support>,
    op: Op,
    w: usize,
    a: Option<Nid>,
    b: Option<Nid>,
    args: &[super::ast::Operand],
    width_of: &impl Fn(Nid, &HashMap<Nid, Support>) -> usize,
    pow2: &HashSet<Nid>,
) -> Support {
    let sa = a.and_then(|n| support.get(&n)).cloned().unwrap_or_default();
    let sb = b.and_then(|n| support.get(&n)).cloned().unwrap_or_default();
    let bit = |s: &Support, i: usize| s.get(i).cloned().unwrap_or_default();
    let all = |s: &Support| -> HashSet<BitRef> { s.iter().flatten().copied().collect() };

    match op {
        // ALIGNED + SPARSE. Bit i of one operand meets bit i of the other and nothing else — the
        // structure that makes interleaving worth 84× and cell-major Θ(2^n).
        Op::And | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor | Op::Iff | Op::Implies => (0
            ..w)
            .map(|i| {
                let (x, y) = (bit(&sa, i), bit(&sb, i));
                g.add_cross(&x, &y);
                x.union(&y).copied().collect()
            })
            .collect(),
        // Aligned like the bitwise ops, but the RESULT is one bit, so every pair still meets
        // pairwise while the output depends on all of them.
        Op::Eq | Op::Neq => {
            let n = sa.len().max(sb.len());
            for i in 0..n {
                g.add_cross(&bit(&sa, i), &bit(&sb, i));
            }
            let mut u = all(&sa);
            u.extend(all(&sb));
            vec![u; w.max(1)]
        }
        // Aligned PLUS a carry/borrow chain: bit i also meets bit i+1 of the same operand.
        Op::Add | Op::Sub | Op::Inc | Op::Dec => {
            let n = sa.len().max(sb.len()).max(w);
            for i in 0..n {
                g.add_cross(&bit(&sa, i), &bit(&sb, i));
                if i + 1 < n {
                    g.add_cross(&bit(&sa, i), &bit(&sa, i + 1));
                    g.add_cross(&bit(&sb, i), &bit(&sb, i + 1));
                }
            }
            // Carry propagates upward: bit i of the sum depends on every bit at or below i.
            (0..w)
                .map(|i| {
                    let mut u = HashSet::new();
                    for j in 0..=i {
                        u.extend(bit(&sa, j));
                        u.extend(bit(&sb, j));
                    }
                    u
                })
                .collect()
        }
        // Comparisons: aligned pairwise, result one bit depending on everything.
        Op::Sgt | Op::Ugt | Op::Sgte | Op::Ugte | Op::Slt | Op::Ult | Op::Slte | Op::Ulte => {
            let n = sa.len().max(sb.len());
            for i in 0..n {
                g.add_cross(&bit(&sa, i), &bit(&sb, i));
                if i + 1 < n {
                    g.add_cross(&bit(&sa, i), &bit(&sa, i + 1));
                }
            }
            let mut u = all(&sa);
            u.extend(all(&sb));
            vec![u; w.max(1)]
        }
        // DENSE. A SYMBOLIC shift amount makes a barrel shifter: every output bit is a mux tree
        // over every input bit, selected by the amount. A CONSTANT amount is pure wiring and
        // contributes nothing — which is why `x * 32` (lifted to a shift) is free.
        Op::Sll | Op::Srl | Op::Sra | Op::Rol | Op::Ror => {
            let amount = all(&sb);
            if amount.is_empty() {
                // Constant amount — a re-map, no coupling. Approximated as a shift of the support.
                return (0..w).map(|i| bit(&sa, i)).collect();
            }
            let data = all(&sa);
            g.add_cross(&data, &amount);
            // NO data×data clique: a barrel shifter's inputs do not combine pairwise, they are
            // SELECTED among. Adding that clique made the graph call this "near-complete, no order
            // helps" when interleaving in fact wins 65 537× — the measured counter-example.
            g.selectors.extend(amount.iter().copied());
            let mut u = data;
            u.extend(amount);
            vec![u; w]
        }
        // DENSE by Bryant 1986: exponential under EVERY order, so the graph should say "no order
        // helps" rather than suggest one.
        Op::Mul | Op::Sdiv | Op::Udiv | Op::Smod | Op::Srem | Op::Urem => {
            let (x, y) = (all(&sa), all(&sb));
            // One operand CONSTANT ⇒ not a general multiplier. By a power of two it is a pure
            // shift (no coupling at all); by any other constant it is a sum of shifted copies,
            // which carries but is not dense. Only symbolic × symbolic is the exponential case.
            let konst_pow2 = (y.is_empty() && b.is_some_and(|n| pow2.contains(&n)))
                || (x.is_empty() && a.is_some_and(|n| pow2.contains(&n)));
            if konst_pow2 {
                return (0..w)
                    .map(|i| bit(&sa, i).union(&bit(&sb, i)).copied().collect())
                    .collect();
            }
            if x.is_empty() || y.is_empty() {
                let live = if x.is_empty() { &y } else { &x };
                let mut ordered: Vec<BitRef> = live.iter().copied().collect();
                ordered.sort_unstable();
                for pair in ordered.windows(2) {
                    g.add_edge(pair[0], pair[1]); // carry chain of the shift-and-add
                }
                let mut u = x;
                u.extend(y);
                return vec![u; w];
            }
            g.add_cross(&x, &y);
            g.add_cross(&x, &x);
            let mut u = x;
            u.extend(y);
            vec![u; w]
        }
        // Reductions: one output bit over all input bits, and the bits meet each other.
        Op::Redand | Op::Redor | Op::Redxor => {
            let x = all(&sa);
            g.add_cross(&x, &x);
            vec![x; w.max(1)]
        }
        // Pure re-maps: no new coupling.
        Op::Not | Op::Neg => (0..w).map(|i| bit(&sa, i)).collect(),
        Op::Uext | Op::Sext => (0..w)
            .map(|i| {
                if i < sa.len() {
                    bit(&sa, i)
                } else {
                    HashSet::new()
                }
            })
            .collect(),
        Op::Concat => {
            let lo_w = b.map_or(0, |n| width_of(n, support));
            (0..w)
                .map(|i| {
                    if i < lo_w {
                        bit(&sb, i)
                    } else {
                        bit(&sa, i - lo_w)
                    }
                })
                .collect()
        }
        Op::Slice => {
            // `slice sort signal upper lower` — args carry the bounds as literals.
            let lower = args.get(2).map_or(0, |o| o.nid().max(0) as usize);
            (0..w).map(|i| bit(&sa, i + lower)).collect()
        }
        // The condition selects, so it meets every output bit.
        Op::Ite => {
            let cond = all(&sa);
            let st = args
                .get(1)
                .and_then(|o| support.get(&o.nid()))
                .cloned()
                .unwrap_or_default();
            let sf = args
                .get(2)
                .and_then(|o| support.get(&o.nid()))
                .cloned()
                .unwrap_or_default();
            (0..w)
                .map(|i| {
                    let (t, f) = (bit(&st, i), bit(&sf, i));
                    g.add_cross(&cond, &t);
                    g.add_cross(&cond, &f);
                    g.selectors.extend(cond.iter().copied());
                    g.add_cross(&t, &f);
                    let mut u = cond.clone();
                    u.extend(t);
                    u.extend(f);
                    u
                })
                .collect()
        }
        // Overflow predicates and memory ops: treated as dense, which is conservative — it can
        // only make the graph claim LESS about a good order, never more.
        _ => {
            let (x, y) = (all(&sa), all(&sb));
            g.add_cross(&x, &y);
            g.add_cross(&x, &x);
            let mut u = x;
            u.extend(y);
            vec![u; w.max(1)]
        }
    }
}

/// W-3′ — what the graph SAYS about a design's variable order, as one line.
///
/// Three readings, and the third is the one that matters for a decision:
///
/// - **`mean_degree`** — how close to complete the graph is. High means no linear order can do
///   much, so a reordering pass should DECLINE rather than pay for one.
/// - **`aligned_fraction`** — of the edges, how many join bits at the SAME index (`a_i — b_i`).
///   That is the structure interleaving exploits; a high fraction argues for interleaving.
/// - **`cross_cell_fraction`** — how many edges leave their own cell. Near zero means the cells do
///   not interact, so cell-major is right and interleaving would spread them for nothing.
#[derive(Debug, Clone, Copy)]
pub struct OrderVerdict {
    pub bits: usize,
    pub edges: usize,
    pub mean_degree: f64,
    pub aligned_fraction: f64,
    pub cross_cell_fraction: f64,
    /// Fraction of cross-cell edges with a SELECTOR endpoint — dense but orderable structure.
    pub selector_fraction: f64,
}

impl OrderVerdict {
    /// The recommendation this graph supports, in words. Deliberately conservative: it says
    /// "no order helps" for a dense graph rather than picking one, because that is the case
    /// where paying to reorder is waste — and it is the shape of the one block where blind
    /// interleaving regressed 10.9×.
    #[must_use]
    pub fn recommends(&self) -> &'static str {
        if self.edges == 0 || self.cross_cell_fraction < 0.05 {
            "cell-major (cells barely interact)"
        } else if self.selector_fraction > 0.5 {
            // Checked BEFORE density: selector coupling is dense by nature but orderable, so a
            // density test alone would wrongly decline the case interleaving helps most.
            "interleaved (selector bits must sit among the data they control)"
        } else if self.mean_degree > (self.bits as f64) * 0.5 {
            "neither — graph is near-complete, no linear order helps"
        } else if self.aligned_fraction > 0.5 {
            "interleaved (aligned cross-cell structure dominates)"
        } else {
            "neither — cross-cell but not aligned"
        }
    }
}

/// Summarise `file`'s bit-level graph into the three readings above.
#[must_use]
pub fn order_verdict(file: &Btor2File) -> OrderVerdict {
    let g = build(file);
    let edges = g.edges.len();
    let (mut aligned, mut cross, mut sel) = (0usize, 0usize, 0usize);
    for (a, b) in &g.edges {
        if a.0 != b.0 {
            cross += 1;
            if a.1 == b.1 {
                aligned += 1;
            }
            if g.selectors.contains(a) || g.selectors.contains(b) {
                sel += 1;
            }
        }
    }
    OrderVerdict {
        bits: g.nodes.len(),
        edges,
        mean_degree: g.mean_degree(),
        aligned_fraction: if cross == 0 {
            0.0
        } else {
            aligned as f64 / cross as f64
        },
        cross_cell_fraction: if edges == 0 {
            0.0
        } else {
            cross as f64 / edges as f64
        },
        selector_fraction: if cross == 0 {
            0.0
        } else {
            sel as f64 / cross as f64
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    /// Leaf nid by symbol, so a test can name `a` rather than a number.
    fn cell(file: &Btor2File, want: &str) -> Nid {
        file.lines
            .iter()
            .find(|l| match &l.node {
                Node::State {
                    symbol: Some(s), ..
                }
                | Node::Input {
                    symbol: Some(s), ..
                } => s == want,
                _ => false,
            })
            .map(|l| l.nid)
            .unwrap_or_else(|| panic!("no cell named {want}"))
    }

    fn g_of(src: &str) -> (Btor2File, BitGraph) {
        let file = parser::parse(src).expect("parse");
        let g = build(&file);
        (file, g)
    }

    /// W-3′ — what the graph RECOMMENDS, against outcomes we have already measured.
    ///
    /// This is the half of the validation that can be done with designs we hold. The decisive half
    /// is a consumer's `sdram_burst` — the only known case where CELL-MAJOR wins (10.9× in wall
    /// time) — and the falsifier for the whole track is that the graph must NOT say "interleaved"
    /// there. Its 52 symbolic-amount shifts at 33 bits should read as near-complete.
    #[test]
    #[ignore = "probe: W-3' order verdicts; run with --ignored --nocapture"]
    fn probe_w3_order_verdict_against_measured_outcomes() {
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "relational a==b",
                "1 sort bitvec 1\n2 sort bitvec 12\n3 state 2 a\n4 state 2 b\n5 eq 1 3 4\n6 bad 5\n".into(),
                "interleaved WINS 84x",
            ),
            (
                "barrel shift (symbolic amount)",
                "1 sort bitvec 1\n2 sort bitvec 12\n3 state 2 x\n4 state 2 k\n5 zero 2\n\
                 6 init 2 3 5\n7 srl 2 3 4\n8 next 2 3 7\n".into(),
                "interleaved WINS 65537->1",
            ),
            (
                "shift by a CONSTANT",
                "1 sort bitvec 1\n2 sort bitvec 12\n3 state 2 x\n4 constd 2 5\n5 zero 2\n\
                 6 init 2 3 5\n7 srl 2 3 4\n8 next 2 3 7\n".into(),
                "TIE (1 node both)",
            ),
            (
                "multiply (symbolic x symbolic)",
                "1 sort bitvec 1\n2 sort bitvec 12\n3 state 2 a\n4 state 2 b\n5 mul 2 3 4\n\
                 6 zero 2\n7 init 2 3 6\n8 next 2 3 5\n".into(),
                "order-IMMUNE (0.97-1.06x)",
            ),
            (
                "independent cells",
                "1 sort bitvec 1\n2 sort bitvec 12\n3 state 2 a\n4 state 2 b\n5 one 2\n\
                 6 add 2 3 5\n7 next 2 3 6\n8 add 2 4 5\n9 next 2 4 8\n".into(),
                "no cross-cell structure",
            ),
        ];
        eprintln!("\n===== W-3′: order verdict vs MEASURED outcome =====");
        eprintln!(
            "{:<32} {:>5} {:>7} {:>8} {:>7} {:>7} {:>6}  {:<52} measured",
            "design", "bits", "edges", "mean-deg", "align", "cross", "sel", "graph recommends"
        );
        for (name, src, measured) in &cases {
            let file = parser::parse(src).expect("parse");
            let v = order_verdict(&file);
            eprintln!(
                "{:<32} {:>5} {:>7} {:>8.2} {:>8.2} {:>8.2}  {:<44} {}",
                name,
                v.bits,
                v.edges,
                v.mean_degree,
                v.aligned_fraction,
                v.cross_cell_fraction,
                v.recommends(),
                measured
            );
        }
        for (name, rel) in [
            (
                "uart_msg_handler",
                "scratchpad/uart_lift/uart_msg_handler.btor2",
            ),
            ("spiCtrl", "scratchpad/spictrl_lift/spiCtrl.btor2"),
            (
                "sd_data_master",
                "scratchpad/sddm_lift/sd_data_master.btor2",
            ),
        ] {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let Ok(src) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            let Ok(file) = parser::parse(&src) else {
                continue;
            };
            let v = order_verdict(&file);
            eprintln!(
                "{:<32} {:>5} {:>7} {:>8.2} {:>7.2} {:>7.2} {:>6.2}  {:<52} TIE (real lift)",
                name,
                v.bits,
                v.edges,
                v.mean_degree,
                v.aligned_fraction,
                v.cross_cell_fraction,
                v.selector_fraction,
                v.recommends(),
            );
        }
        eprintln!("===== end W-3′ =====\n");
    }

    /// W-2′ — an equality is ALIGNED and SPARSE: `a_i` meets `b_i` and NOTHING else.
    ///
    /// This is the property the whole idea rests on. A cell-level graph cannot express it — it
    /// yields the complete bipartite `{a_0..a_n} × {b_0..b_n}`, which is why W-1's span metric was
    /// degenerate. If this test does not hold, the bit-level graph buys nothing over W-1.
    #[test]
    fn equality_couples_only_bits_at_the_same_index() {
        let (f, g) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 a\n4 state 2 b\n5 eq 1 3 4\n6 bad 5\n",
        );
        let (a, b) = (cell(&f, "a"), cell(&f, "b"));

        for i in 0..4 {
            let e = if (a, i) < (b, i) {
                ((a, i), (b, i))
            } else {
                ((b, i), (a, i))
            };
            assert!(g.edges.contains(&e), "a_{i} must meet b_{i}");
        }
        // And crucially: no MISALIGNED pair.
        for i in 0..4 {
            for j in 0..4 {
                if i == j {
                    continue;
                }
                let e = if (a, i) < (b, j) {
                    ((a, i), (b, j))
                } else {
                    ((b, j), (a, i))
                };
                assert!(
                    !g.edges.contains(&e),
                    "a_{i} must NOT meet b_{j}: an equality compares aligned bits only"
                );
            }
        }
        assert_eq!(
            g.edges.len(),
            4,
            "exactly the four aligned pairs, nothing more"
        );
    }

    /// W-2′ — multiplication is DENSE, which is the graph saying "no order helps here"
    /// (Bryant 1986). The measured control: interleaving moved a symbolic multiply by 0.97–1.06×.
    #[test]
    fn symbolic_multiply_is_dense_and_constant_multiply_is_free() {
        let (_, dense) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 a\n4 state 2 b\n5 mul 2 3 4\n\
             6 zero 2\n7 init 2 3 6\n8 next 2 3 5\n",
        );
        // 8 bits, complete-ish: every a_i meets every b_j.
        assert!(
            dense.mean_degree() > 3.0,
            "a symbolic multiply must look dense; mean degree {}",
            dense.mean_degree()
        );

        let (_, cheap) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 a\n4 constd 2 8\n5 mul 2 3 4\n\
             6 zero 2\n7 init 2 3 6\n8 next 2 3 5\n",
        );
        assert_eq!(
            cheap.edges.len(),
            0,
            "multiply by a CONSTANT couples nothing — the constant has empty support, which is \
             why a lifted `x * 32` costs nothing while `x * y` is exponential"
        );
    }

    /// W-2′ — a SYMBOLIC shift amount couples every amount bit to every data bit (a barrel
    /// shifter); a CONSTANT amount is pure wiring. Measured: 65 537 nodes cell-major vs 1
    /// interleaved for the symbolic case, and 1 vs 1 for the constant one.
    #[test]
    fn symbolic_shift_amount_couples_to_the_data_and_a_constant_one_does_not() {
        let (f, sym) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 x\n4 state 2 k\n5 zero 2\n\
             6 init 2 3 5\n7 srl 2 3 4\n8 next 2 3 7\n",
        );
        let (x, k) = (cell(&f, "x"), cell(&f, "k"));
        for i in 0..4 {
            for j in 0..4 {
                let e = if (x, i) < (k, j) {
                    ((x, i), (k, j))
                } else {
                    ((k, j), (x, i))
                };
                assert!(
                    sym.edges.contains(&e),
                    "x_{i} must meet amount bit k_{j}: every output bit is a mux over every input \
                     bit, selected by the amount"
                );
            }
        }

        let (_, konst) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 x\n4 constd 2 2\n5 zero 2\n\
             6 init 2 3 5\n7 srl 2 3 4\n8 next 2 3 7\n",
        );
        assert_eq!(
            konst.edges.len(),
            0,
            "a constant shift amount is wiring, not a barrel shifter"
        );
    }

    /// W-2′ — cells that never meet get NO edges, so a reordering pass has no reason to interleave
    /// them. This is the case blind interleaving spreads apart for nothing.
    #[test]
    fn independent_cells_share_no_edges() {
        let (f, g) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 a\n4 state 2 b\n5 one 2\n\
             6 add 2 3 5\n7 next 2 3 6\n8 add 2 4 5\n9 next 2 4 8\n",
        );
        let (a, b) = (cell(&f, "a"), cell(&f, "b"));
        for i in 0..4 {
            for j in 0..4 {
                let e = if (a, i) < (b, j) {
                    ((a, i), (b, j))
                } else {
                    ((b, j), (a, i))
                };
                assert!(
                    !g.edges.contains(&e),
                    "a_{i} and b_{j} never meet in any operator, so they must not be coupled"
                );
            }
        }
    }

    /// W-2′ — an adder couples aligned bits AND the carry chain, so `a_i` meets `a_{i+1}`.
    /// That is why an adder's operands want interleaving with the carry direction preserved.
    #[test]
    fn addition_couples_aligned_bits_and_the_carry_chain() {
        let (f, g) = g_of(
            "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 a\n4 state 2 b\n5 add 2 3 4\n\
             6 zero 2\n7 init 2 3 6\n8 next 2 3 5\n",
        );
        let (a, b) = (cell(&f, "a"), cell(&f, "b"));
        for i in 0..4 {
            let al = if (a, i) < (b, i) {
                ((a, i), (b, i))
            } else {
                ((b, i), (a, i))
            };
            assert!(g.edges.contains(&al), "aligned pair a_{i}—b_{i}");
        }
        for i in 0..3 {
            assert!(
                g.edges.contains(&((a, i), (a, i + 1))),
                "carry chain a_{i}—a_{}",
                i + 1
            );
        }
    }
}
