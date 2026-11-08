//! mununu#893 — REPRODUCER (fails on the parent commit, passes on the fix).
//!
//! The native k-induction member's per-check budget was a z3 wall clock: a check finishing in
//! 4.9 s on one run and 5.1 s on the next gave a definite answer once and a silent `Unknown` once,
//! so the same row flipped between ⊥ and `holds` across runs and hosts (HWMCC'20
//! `zipcpu_dcache-p588`). This test asks the two things the parent could not deliver on a query
//! that cannot finish within the allowance: that a budget stop is an **error naming its cause**
//! rather than a silent `Ok(Unknown)`, and that two runs stop with the **same words** — a resource
//! limit counts work, not time. It uses only API the parent already had (`decide_bad_safety`,
//! `BmcError`'s `Display`), so `make verify-repro TEST=the_native_query_budget_stop_is_named_and_identical_twice`
//! compiles against the parent and fails there.
//!
//! It lives outside `native_bmc.rs` on purpose: `verify-repro` copies every file holding the test
//! name into the parent tree, and a test in the fixed file carries the fix with it (mununu#853's
//! co-location trap, met again in #913).

use mununu_core::adapter::btor2::native_bmc::decide_bad_safety;
use mununu_core::adapter::btor2::parser;

/// The pigeonhole principle as a BTOR2 `bad` over 1-bit inputs — unsatisfiable, trivial to
/// bit-blast, exponential for resolution: z3 needs tens of seconds at 11 pigeons / 10 holes, so a
/// 1 ms allowance can never decide it on any host.
fn pigeonhole_btor2(pigeons: usize, holes: usize) -> String {
    let mut src = String::from("1 sort bitvec 1\n");
    let mut nid = 2i64;
    let mut x = vec![vec![0i64; holes]; pigeons];
    for (p, row) in x.iter_mut().enumerate() {
        for (h, cell) in row.iter_mut().enumerate() {
            src.push_str(&format!("{nid} input 1 x_{p}_{h}\n"));
            *cell = nid;
            nid += 1;
        }
    }
    let mut conj: Vec<i64> = Vec::new();
    for row in &x {
        let mut acc = row[0];
        for &cell in &row[1..] {
            src.push_str(&format!("{nid} or 1 {acc} {cell}\n"));
            acc = nid;
            nid += 1;
        }
        conj.push(acc);
    }
    for h in 0..holes {
        let col: Vec<i64> = x.iter().map(|row| row[h]).collect();
        for p in 0..pigeons {
            for q in (p + 1)..pigeons {
                src.push_str(&format!("{nid} and 1 {} {}\n", col[p], col[q]));
                let both = nid;
                nid += 1;
                src.push_str(&format!("{nid} not 1 {both}\n"));
                conj.push(nid);
                nid += 1;
            }
        }
    }
    let mut acc = conj[0];
    for &c in &conj[1..] {
        src.push_str(&format!("{nid} and 1 {acc} {c}\n"));
        acc = nid;
        nid += 1;
    }
    src.push_str(&format!("{nid} bad {acc}\n"));
    src
}

#[test]
fn the_native_query_budget_stop_is_named_and_identical_twice() {
    let file = parser::parse(&pigeonhole_btor2(11, 10)).expect("parses");
    let run = || decide_bad_safety(&file, 0, Some(1));
    let (a, b) = (run(), run());
    let words = |r: &Result<_, mununu_core::adapter::btor2::native_bmc::BmcError>| match r {
        Ok(v) => panic!(
            "a check that cannot finish within its allowance must stop with a NAMED cause, not a \
             silent verdict: {v:?}"
        ),
        Err(e) => e.to_string(),
    };
    let (a, b) = (words(&a), words(&b));
    assert_eq!(
        a, b,
        "a budget stop is the same stop twice — it counts work, not time"
    );
    assert!(
        a.contains("budget") || a.contains("limit"),
        "the stop names the budget it hit: {a}"
    );
}
