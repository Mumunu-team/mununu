//! mununu#878: a cancelled native k-induction query never yields a definite verdict.
//!
//! On the 3-state responder, `AG(st == 1 → AF st == 2)` holds; the exact member decides it first
//! and the portfolio cancels native mid-query. Before the fix the interrupted query sometimes came
//! back definite, and native reported `reachable` against exact's `unreachable`, a contradiction
//! (10 of 240 runs under load). The test supplies its own load: many rescues at once.

use mununu_core::adapter::liveness_rescue::{LivenessVerdict, response_liveness_rescue};
use mununu_core::mu_calculus::parser::parse;

// 3-state responder: st 0 = idle, 1 = req, 2 = grant. idle -go-> req, req -> grant, grant -> idle.
const RESPONDER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 eq 2 3 4
10 eq 2 3 7
11 ite 1 6 7 4
12 ite 1 10 8 4
13 ite 1 9 11 12
14 next 1 3 13
";

#[test]
fn a_cancelled_native_member_never_contradicts_the_exact_verdict() {
    const THREADS: usize = 24;
    const ROUNDS: usize = 10;
    let bad: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    let f = parse("nu Z. ((!(st == 1) || mu Y. ((st == 2) || [] Y)) && [] Z)")
                        .expect("the response formula parses");
                    (0..ROUNDS)
                        .filter_map(|_| {
                            let (v, out) = response_liveness_rescue(RESPONDER, &f, false)
                                .expect("the response shape reduces to l2s");
                            (v != LivenessVerdict::Holds).then(|| format!("{v:?}: {out:?}"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a rescue thread panicked"))
            .collect()
    });
    assert!(
        bad.is_empty(),
        "{} of {} rescues did not hold; first: {}",
        bad.len(),
        THREADS * ROUNDS,
        bad.first().map(String::as_str).unwrap_or("")
    );
}
