//! AIGER → BTOR2 (HWMCC LIVE study, 2026-10-11).
//!
//! Lowers a parsed AIGER 1.9 [`Circuit`] — binary or ASCII — to BTOR2 text the `btor2` verbs
//! and the exact engine consume. Every AIGER construct has a one-line BTOR2 image, so the
//! translation is structural and exact:
//!
//! | AIGER                      | BTOR2                                               |
//! |----------------------------|-----------------------------------------------------|
//! | input `2v`                 | `input 1 <name>` (`i<k>` when the file names none)   |
//! | latch `2v` next `n` init   | `state 1 <name>` + `init` (0 → `zero`, 1 → `one`, undefined → no `init` = free) + `next` |
//! | AND `l = a ∧ b`            | `and 1 <a> <b>`; a negated literal is `not 1 <v>`     |
//! | `bad l`                    | `bad <l>`                                            |
//! | `constraint l`             | `constraint <l>`                                     |
//! | `justice {l₁…lₙ}`          | `justice n <l₁> … <lₙ>`                              |
//! | `fairness l`               | `fair <l>`                                           |
//! | output `l`                 | `output <l> <name>` (`o<k>` when unnamed)             |
//!
//! Literal 0 / 1 are the constants `zero` / `one`. The AIGER variable order already puts every
//! AND's operands before it (lhs > rhs by the format's invariant), so the gates are emitted in
//! file order after the inputs and latches. Why not the toolchain: the AIGER toolkit's
//! `aigtobtor` "can not handle latches" and yosys `read_aiger` rejects the binary 1.9 justice
//! sections — measured 2026-10-11 on the HWMCC'17 LIVE set.
//!
//! Nothing here names the justice / fairness literals: [`crate::adapter::justice`] does that on
//! any BTOR2 (also one yosys produced), so the converter stays a pure format map.

use super::ast::{Circuit, Literal};

struct Emit {
    out: String,
    next: u64,
    sort: u64,
    zero: u64,
    one: u64,
    /// AIGER variable → the BTOR2 node carrying its positive literal.
    var_node: Vec<Option<u64>>,
    /// Memoised `not` node per variable.
    neg_node: Vec<Option<u64>>,
}

impl Emit {
    fn push(&mut self, line: &str) -> u64 {
        let nid = self.next;
        self.next += 1;
        self.out.push_str(&format!("{nid} {line}\n"));
        nid
    }

    /// The node for an AIGER literal, emitting a `not` for a negated one (once per variable).
    fn lit(&mut self, lit: Literal) -> u64 {
        if lit == 0 {
            return self.zero;
        }
        if lit == 1 {
            return self.one;
        }
        let v = lit >> 1;
        let pos = self.var_node[v].expect("AIGER literal refers to a variable defined earlier");
        if lit & 1 == 0 {
            return pos;
        }
        if let Some(n) = self.neg_node[v] {
            return n;
        }
        let n = self.push(&format!("not {} {pos}", self.sort));
        self.neg_node[v] = Some(n);
        n
    }
}

/// Lower `circuit` to BTOR2 text (1-bit sorts only; AIGER is bit-level).
pub fn circuit_to_btor2(circuit: &Circuit) -> String {
    let mut e = Emit {
        out: String::new(),
        next: 1,
        sort: 0,
        zero: 0,
        one: 0,
        var_node: vec![None; circuit.max_var + 1],
        neg_node: vec![None; circuit.max_var + 1],
    };
    e.sort = e.push("sort bitvec 1");
    e.zero = e.push(&format!("zero {}", e.sort));
    e.one = e.push(&format!("one {}", e.sort));
    let sort = e.sort;
    for (k, &lit) in circuit.inputs.iter().enumerate() {
        let name = circuit
            .symbols
            .input_names
            .get(k)
            .cloned()
            .flatten()
            .unwrap_or_else(|| format!("i{k}"));
        let nid = e.push(&format!("input {sort} {}", sanitize(&name)));
        e.var_node[lit >> 1] = Some(nid);
    }
    let mut latch_nids: Vec<u64> = Vec::with_capacity(circuit.latches.len());
    for (k, latch) in circuit.latches.iter().enumerate() {
        let name = circuit
            .symbols
            .latch_names
            .get(k)
            .cloned()
            .flatten()
            .unwrap_or_else(|| format!("l{k}"));
        let nid = e.push(&format!("state {sort} {}", sanitize(&name)));
        e.var_node[latch.current >> 1] = Some(nid);
        latch_nids.push(nid);
        match latch.init {
            0 => {
                let z = e.zero;
                e.push(&format!("init {sort} {nid} {z}"));
            }
            1 => {
                let o = e.one;
                e.push(&format!("init {sort} {nid} {o}"));
            }
            _ => {} // undefined initial value: a free (uninitialised) state cell
        }
    }
    for gate in &circuit.gates {
        let a = e.lit(gate.rhs0);
        let b = e.lit(gate.rhs1);
        let nid = e.push(&format!("and {sort} {a} {b}"));
        e.var_node[gate.lhs >> 1] = Some(nid);
    }
    // `next` lines after every gate exists (a latch's next may be any literal).
    for (k, latch) in circuit.latches.iter().enumerate() {
        let n = e.lit(latch.next);
        e.push(&format!("next {sort} {} {n}", latch_nids[k]));
    }
    for (k, &lit) in circuit.outputs.iter().enumerate() {
        let n = e.lit(lit);
        let name = circuit
            .symbols
            .output_names
            .get(k)
            .cloned()
            .flatten()
            .unwrap_or_else(|| format!("o{k}"));
        e.push(&format!("output {n} {}", sanitize(&name)));
    }
    for &lit in &circuit.bad_outputs {
        let n = e.lit(lit);
        e.push(&format!("bad {n}"));
    }
    for &lit in &circuit.constraints {
        let n = e.lit(lit);
        e.push(&format!("constraint {n}"));
    }
    for set in &circuit.justice_sets {
        let nids: Vec<String> = set.iter().map(|&l| e.lit(l).to_string()).collect();
        e.push(&format!("justice {} {}", set.len(), nids.join(" ")));
    }
    for &lit in &circuit.fairness {
        let n = e.lit(lit);
        e.push(&format!("fair {n}"));
    }
    e.out
}

/// BTOR2 symbols are whitespace-delimited; AIGER names may carry anything.
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == ';' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.is_empty() { "_".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::aiger::parser;

    /// A 2-bit counter (`l0` toggles, `l1` toggles when `l0`), one output on the AND of both,
    /// a justice set {l0 ∧ l1} and a fairness constraint on `l0` — every AIGER 1.9 section once.
    const COUNTER_AAG: &str = "aag 5 0 2 1 3 0 0 1 1\n\
                               2 3\n\
                               4 10\n\
                               8\n\
                               1\n\
                               8\n\
                               2\n\
                               6 2 4\n\
                               8 2 4\n\
                               10 6 9\n\
                               l0 lo\n\
                               l1 hi\n";

    #[test]
    fn every_aiger_section_has_its_btor2_line_and_the_result_parses() {
        let c = parser::parse(COUNTER_AAG).expect("aag parses");
        let b = circuit_to_btor2(&c);
        let f = crate::adapter::btor2::parser::parse(&b).expect("the BTOR2 parses");
        let kinds = |k: &str| {
            b.lines()
                .filter(|l| l.split_whitespace().nth(1) == Some(k))
                .count()
        };
        assert_eq!(kinds("state"), 2, "{b}");
        assert_eq!(kinds("init"), 2);
        assert_eq!(kinds("next"), 2);
        assert_eq!(kinds("and"), 3);
        assert_eq!(kinds("output"), 1);
        assert_eq!(kinds("justice"), 1);
        assert_eq!(kinds("fair"), 1);
        assert!(
            b.contains(" state 1 lo\n") && b.contains(" state 1 hi\n"),
            "latch names kept"
        );
        assert!(
            f.lines
                .iter()
                .any(|l| matches!(l.node, crate::adapter::btor2::ast::Node::Justice { .. }))
        );
    }

    #[test]
    fn an_undefined_initial_value_is_a_free_state_cell() {
        // AIGER 1.9: a latch whose init field is its own literal is uninitialised.
        let aag = "aag 1 0 1 0 0\n2 2 2\n";
        let c = parser::parse(aag).expect("aag parses");
        assert_eq!(c.latches[0].init, 2, "undefined init is recorded as 2");
        let b = circuit_to_btor2(&c);
        assert!(
            !b.contains(" init "),
            "no init line for an uninitialised latch: {b}"
        );
    }

    #[test]
    fn negated_literals_become_one_not_each() {
        let c = parser::parse(COUNTER_AAG).expect("aag parses");
        let b = circuit_to_btor2(&c);
        // `2 3`: next(l0) = ¬l0, and `10 6 9`: ¬(l0∧l1) — two distinct negations.
        assert_eq!(
            b.lines()
                .filter(|l| l.split_whitespace().nth(1) == Some("not"))
                .count(),
            2,
            "{b}"
        );
    }
}
