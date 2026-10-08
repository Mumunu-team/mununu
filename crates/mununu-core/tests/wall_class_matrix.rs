//! Track R — the **wall-class lever-evaluation matrix** (`.claude/plans/wall-class-lever-evaluation.md`).
//!
//! A FIXED set of class-representative recoverability cases (§ `CASES`) × the mechanisms with a
//! corpus claim — the SINGLETONS (`default` = exact-first+WP · `cube-wp` · `cube-craig`) AND the
//! `combined` COMPOSED plan (config-pin + exact + cube + ranking + guard + Craig, via
//! [`solve_recoverability_combined`]) — run as ONE matrix so a lever is judged on the class it
//! TARGETS, not on whatever design happened to be handy. Crucially, no single mechanism is
//! expected to flip a ⊥ alone: the `combined` column measures whether COMPOSITION decides what
//! every singleton leaves ⊥ (the planner's raison d'être). The run prints a `class × lever →
//! verdict` table and asserts the SOUNDNESS invariant (no lever contradicts the oracle) + that
//! each decidable case is decided by at least one lever. A `*` on a `combined` verdict = the
//! decision is config-SCOPED (pins were applied).
//!
//! `#[ignore]` — a validation HARNESS (some cones are wide + the Craig lever needs MathSAT), run on
//! demand: `cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture`
//! (in `mununu-sva-pono` with `MUNUNU_MATHSAT_PATH` set, the `cube-craig` column comes alive).

use mununu_core::adapter::btor2::cegar::PredicateSource;
use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
use mununu_core::adapter::recoverability::{
    verify_recoverability, verify_recoverability_scalable,
    verify_recoverability_scalable_with_source, verify_recoverability_spcr_only,
};
use mununu_core::planner::solve_recoverability_combined;
use mununu_core::verdict::PropertyVerdict;

/// The sound oracle verdict for a case — what a COMPLETE analysis reaches.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Oracle {
    Holds,
    Violated,
    /// A trap the abstraction can't prove — the sound requirement is only "never fabricate Holds".
    NeverHolds,
    /// No current lever decides it (register-dominated / research-⊥) — documented, not asserted.
    HardUnknown,
}

struct Case {
    name: &'static str,
    class: &'static str,
    btor2: &'static str,
    target: &'static str,
    oracle: Oracle,
    /// Provenance: `"RTL:<design>"` for a lifted design, `"synthetic"` for a hand-written
    /// class fixture. The requirement (2026-07-26) is an RTL representative for every class;
    /// synthetic fixtures are labelled "awaiting RTL" so the gap is explicit + trackable.
    source: &'static str,
    /// Config/reset inputs baked to constants BEFORE the levers run (e.g. `rst_ni=1` to pin an
    /// active-low reset inactive → the operational, no-reset recoverability question). A pinned
    /// verdict is config-SCOPED but sound (the exact engine on the concrete pinned model is
    /// 2-valued sound); used to source the RTL VIOLATED class from a reset-recovering FSM.
    pin: &'static [(&'static str, u64)],
}

fn is_rtl(c: &Case) -> bool {
    c.source.starts_with("RTL")
}

// ---- The fixed class-representative set --------------------------------------------------------
// Synthetic fixtures mirror the recoverability/cegar test corpus (kept in sync by class, not
// copied blindly); the RTL case is a checked-in lifted BTOR2.

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

const STALLER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 ite 1 6 7 4
13 ite 1 11 9 3
14 ite 1 10 12 13
15 next 1 3 14
";

const WIDE_RECOVERABLE: &str = "\
1 sort bitvec 300
2 sort bitvec 2
3 sort bitvec 1
4 state 1 cnt
5 zero 1
6 init 1 4 5
7 inc 1 4
8 next 1 4 7
9 state 2 st
10 zero 2
11 init 2 9 10
12 one 2
13 eq 3 9 10
14 eq 3 4 5
15 ite 2 14 12 10
16 ite 2 13 15 10
17 next 2 9 16
";

const WIDE_TRAP: &str = "\
1 sort bitvec 300
2 sort bitvec 2
3 sort bitvec 1
4 state 1 cnt
5 zero 1
6 init 1 4 5
7 inc 1 4
8 next 1 4 7
9 state 2 st
10 zero 2
11 init 2 9 10
12 constd 2 2
13 eq 3 4 5
14 ite 2 13 12 12
15 next 2 9 14
";

const TRAP_UF_W48: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
13 one 3
30 constd 3 2
9 init 3 7 13
10 input 2 start
11 one 1
12 constd 1 2
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
17 ite 1 10 11 5
25 ite 1 23 12 11
18 ite 1 15 25 5
19 ite 1 14 17 18
20 next 1 4 19
31 add 3 7 30
22 next 3 7 31
";

/// mununu#602 — the crossed-vector field (see the case). `flag_o` is bit 128 of the
/// second copy, i.e. bit 128 of the free input two cycles back: always recoverable.
const CROSSED_VECTOR_FIELD: &str = "\
1 sort bitvec 736
2 input 1 rd_words
3 state 1 words
4 next 1 3 2
5 state 1 words_sync
6 next 1 5 3
7 sort bitvec 32
8 sort bitvec 1
9 slice 7 5 159 128 word4
10 slice 8 9 0 0 flag
11 uext 8 10 0 flag_o
";

const POS_W48: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
8 zero 3
9 init 3 7 8
10 input 2 start
11 one 1
12 constd 1 2
13 one 3
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 8
24 and 2 10 23
17 ite 1 24 11 5
18 ite 1 15 12 5
19 ite 1 14 17 18
20 next 1 4 19
21 add 3 7 13
22 next 3 7 21
";

const CLASS2_DATADEP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort bitvec 48
4 state 1 ctrl
5 zero 1
6 init 1 4 5
7 state 3 data
30 constd 3 7
9 init 3 7 30
10 input 2 start
11 one 1
14 eq 2 4 5
15 eq 2 4 11
23 eq 2 7 30
17 ite 1 10 11 5
25 ite 1 23 5 11
18 ite 1 15 25 5
19 ite 1 14 17 18
20 next 1 4 19
22 next 3 7 7
";

// The ITERATION-BOUND class (A2 of the engine-performance roadmap, 2026-10-07): a 1920×1080
// raster — `hcount` wraps at 1920, `vcount` advances on that wrap and wraps at 1080 — with the
// recovery target "the last line". 22 state bits, so the exact engine takes it within the cap,
// but its `EF` walks the frame one pre-image per iteration (`iters ≈ 1.95·h·v` ≈ 4.0 M).
// Oracle HOLDS: every state reaches the frame end. Measured 2026-10-07, with and without the
// squaring rescue (`MUNUNU_BDD_SQUARING=0`): every column reads HOLDS either way — without the
// rescue the exact member abstains on the 2^20 ITERATION budget after ~3 s and the ladder's
// cube escalation decides; with it the exact member decides in milliseconds (23 relational
// products). So on THIS set the lever's reach is cost and decider, not verdict; its
// unknown → verdict reach is on the reach portfolio (`btor2 verify`, no cube escalation:
// forward 2^32 `unknown` → `violated`). The class is represented here so that stays measured.
// Synthetic; the RTL representative is a video timing generator (a consumer's 640×480
// raster-wrap property is the instance behind the `MUNUNU_BDD_ITER_BUDGET` note in CLAUDE.md).
const RASTER_1080P: &str = "\
1 sort bitvec 1
2 sort bitvec 11
3 sort bitvec 11
4 state 2 hcount
5 state 3 vcount
6 zero 2
7 zero 3
8 init 2 4 6
9 init 3 5 7
10 constd 2 1919
11 constd 3 1079
12 eq 1 4 10
13 eq 1 5 11
14 one 2
15 one 3
16 add 2 4 14
17 add 3 5 15
18 ite 2 12 6 16
19 next 2 4 18
20 ite 3 13 7 17
21 ite 3 12 20 5
22 next 3 5 21
";

// The Craig-DISCRIMINATING case: exact decides it (small), but the CUBE path needs the emergent
// relational invariant `data == target` that only Craig discovers — cube-wp ⊥, cube-craig HOLDS.
const CRAIG_EMERGENT: &str = "\
1 sort bitvec 1
2 sort bitvec 3
3 state 1 busy
4 state 2 data
5 state 2 target
6 one 1
7 zero 1
8 zero 2
9 one 2
10 init 1 3 6
11 init 2 4 8
12 init 2 5 8
13 add 2 4 9
14 next 2 4 13
15 add 2 5 9
16 next 2 5 15
17 eq 1 4 5
18 ite 1 17 7 3
19 next 1 3 18
";

// RTL — lifted i2c (44 registers); the register-dominated cone (86% data-dependent, per the
// register-width probe) that NO current lever decides. Reproduce: lift examples/… i2c with
// `sv verify-auto --engine exact-symbolic` + MUNUNU_KEEP_YOSYS_TMP=1.
const I2C_SCL_PADOEN: &str = include_str!("fixtures/wall_classes/i2c_scl_padoen.btor");

// RTL — lifted OpenTitan aes_cipher_control_fsm (5-state sparse FSM), via the Auto→slang
// fallback (#396; read_verilog rejects its `module … import aes_pkg::*;`). Two classes from one
// design: free-reset recoverability HOLDS (reset clears any state); reset PINNED (`rst_ni=1`,
// the standard operational reset-gating) → VIOLATED (the FSM traps in its absorbing error state,
// only reset recovers). Reproduce: examples/verify/dc_opentitan_aes_cipher_control_fsm/source.
const AES_CIPHER: &str = include_str!("fixtures/wall_classes/aes_cipher_control_fsm.btor");
// More RTL FSMs of the SAME two classes (breadth across designs, via the #396 Auto→slang
// fallback): OpenTitan csrng_main_sm (single 6-bit sparse FSM, idle MainSmIdle=55) + aes_ctr_fsm
// (idle CTR_IDLE=14). Same pattern: free-reset HOLDS, reset-pinned `rst_ni=1` VIOLATED.
const CSRNG: &str = include_str!("fixtures/wall_classes/csrng_main_sm.btor");
const AES_CTR: &str = include_str!("fixtures/wall_classes/aes_ctr_fsm.btor");

// RTL — the OpenTitan EDN main state machine's boot handshake, extracted (verbatim FSM transitions) to
// examples/recoverability/edn_boot_sm.sv and lifted. The corpus's genuine POSITIONAL env-strategy case:
// reaching BootUniAckWait (state_q==44) needs `boot_req_mode_i=1` in Idle (start) AND `=0` in BootDone
// (progress) — no constant hold works. Free-input `AG EF(state_q==44)` HOLDS (∃ the positional path).
const EDN_BOOT: &str = include_str!("fixtures/wall_classes/edn_boot_sm.btor");

// RTL — the OpenTitan CSRNG app-command state machine, extracted (verbatim FSM transitions) to
// examples/recoverability/csrng_main_sm_fsm.sv and lifted (name-preserving, distinct from the anonymous
// CSRNG fixture above). This is edn's MONOTONE A/B CONTROL: reaching MainSmCmdVld (state_q==16) is a
// straight assert-to-progress pipeline (Idle→ParseCmd→CmdPrep→CmdVld), so a SINGLE CONSTANT input vector
// drives it — no input needs a state-dependent (positional) value. The env-strategy lever therefore has
// ZERO marginal reach here (the shipped constant-hold already suffices), the exact opposite of edn_boot.
const CSRNG_MAIN_SM: &str = include_str!("fixtures/wall_classes/csrng_main_sm_fsm.btor");

// RTL — the OpenTitan OTBN (big-number crypto co-processor) start/stop controller FSM, extracted verbatim
// to examples/recoverability/otbn_start_stop_fsm.sv and lifted. The SECOND real POSITIONAL env-strategy
// case, from a DIFFERENT module family than edn (a start/execute/secure-wipe handshake, not a boot flow)
// and a DIFFERENT positional flavor: reaching Running (state_q==119) needs `urnd_reseed_ack_i=0` in Halt
// (a stray ACK there hits the spurious-ACK security trap → Locked) AND `=1` in UrndRefresh (to progress)
// — an "acknowledge only when expected" discipline. No constant ACK hold reaches Running; free HOLDS.
const OTBN_START_STOP: &str = include_str!("fixtures/wall_classes/otbn_start_stop_fsm.btor");

// RTL — the OpenTitan ROM controller check FSM, extracted verbatim to
// examples/recoverability/rom_ctrl_fsm.sv and lifted. The THIRD real POSITIONAL env-strategy case, and a
// SECOND instance of the otbn "ack-when-expected" flavor (confirming it recurs across OT security FSMs):
// reaching the terminal Done (state_q==518) needs `kmac_done_i=0` at ReadingLow (a stray ACK trips the
// SEC_CM consistency check → Invalid) AND `=1` at ReadingHigh/RomAhead (to progress to Checking→Done).
// The FSM is LINEAR (upstream ASSERT_FPV_LINEAR_FSM) so Done is unique-path; no constant ACK reaches it.
// The rom_ctrl_counter / rom_ctrl_compare submodules are black-boxed to free inputs (counter_*/checker_*).
const ROM_CTRL_FSM: &str = include_str!("fixtures/wall_classes/rom_ctrl_fsm.btor");

// ARRAY-CONTENT-GATED νµ recoverability (the SPCR class, PR #410). `AG EF(busy==0)` where recovery
// routes through ARRAY CONTENT read at a latched index: `busy` clears only when `mem[key]==all-ones`.
// exact-symbolic SKIPs the in-cone `$mem`; the plain cube's must-edge is an AUFBV ∀-over-array query
// → Unknown → the νµ abstains (⊥). SPCR registerizes the accessed cell + drops the array → QF_BV →
// exact decides. TWO sibling cases mark the FRAGMENT BOUNDARY (the whole point of the class):
//   (decides) `key` latches `waddr` (the write address) → the index only ever moves TO the written
//             cell → `mem'[key']=mem'[waddr]=wdata` exact → SPCR applies → HOLDS.
//   (abstains) `key` latches a free input `sel` INDEPENDENT of `waddr` → the index jumps to an
//             arbitrary past-written cell no finite prophecy-set tracks → SPCR soundly abstains
//             (`spcr` col = SKIP) → exact SKIP + ∀-array cube ⊥ → HardUnknown.
// synthetic: the class is measured-ABSENT from real RTL corpora (0/135 AssertLLM2, 0/315 HWMCC20,
// full OpenTitan hw/ip sweep — the only near-miss, the OTBN loop stack, recovers via a down-counter
// + PC-match, not a stored-content value-test), so there is no RTL representative to lift.
const ARRAY_CONTENT_SPCR: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 sort array 2 2
4 input 1 start
5 input 2 waddr
6 input 2 wdata
7 state 1 busy
8 state 2 key
9 state 3 mem
10 const 1 0
11 init 1 7 10
12 const 2 00
13 init 2 8 12
14 const 2 11
15 read 2 9 8
16 eq 1 15 14
17 not 1 7
18 and 1 4 17
19 const 1 1
20 and 1 7 16
21 ite 1 20 10 7
22 ite 1 18 19 21
23 next 1 7 22
24 ite 2 18 5 8
25 next 2 8 24
26 write 3 9 5 6
27 next 3 9 26
";

// Sibling of ARRAY_CONTENT_SPCR with ONE change: `key` latches the independent input `sel` (nid 28)
// instead of `waddr` (nid 5) — the outside-the-fragment boundary case (SPCR abstains soundly).
const ARRAY_CONTENT_INDEP: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 sort array 2 2
4 input 1 start
5 input 2 waddr
6 input 2 wdata
7 state 1 busy
8 state 2 key
9 state 3 mem
10 const 1 0
11 init 1 7 10
12 const 2 00
13 init 2 8 12
14 const 2 11
15 read 2 9 8
16 eq 1 15 14
17 not 1 7
18 and 1 4 17
19 const 1 1
20 and 1 7 16
21 ite 1 20 10 7
22 ite 1 18 19 21
23 next 1 7 22
24 ite 2 18 28 8
25 next 2 8 24
26 write 3 9 5 6
27 next 3 9 26
28 input 2 sel
";

const NO_PIN: &[(&str, u64)] = &[];
const PIN_RST: &[(&str, u64)] = &[("rst_ni", 1)];

fn cases() -> Vec<Case> {
    vec![
        // ---- synthetic class fixtures (mechanism checks; awaiting RTL representatives) ----
        Case {
            name: "responder",
            class: "decidable-exact/HOLDS",
            btor2: RESPONDER,
            target: "st == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "staller",
            class: "decidable-exact/VIOLATED",
            btor2: STALLER,
            target: "st == 0",
            oracle: Oracle::Violated,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "wide_recoverable",
            class: "wide-cube/HOLDS",
            btor2: WIDE_RECOVERABLE,
            target: "st == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "wide_trap",
            class: "wide-cube/VIOLATED",
            btor2: WIDE_TRAP,
            target: "st == 0",
            oracle: Oracle::Violated,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "trap_uf_w48",
            class: "uf-wrap/soundness",
            btor2: TRAP_UF_W48,
            target: "ctrl == 0",
            oracle: Oracle::NeverHolds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "pos_w48",
            class: "wide-datapath-return/HOLDS",
            btor2: POS_W48,
            target: "ctrl == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "class2_datadep",
            class: "guard-atom/HOLDS",
            btor2: CLASS2_DATADEP,
            target: "ctrl == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "craig_emergent",
            class: "invariant-class/HOLDS",
            btor2: CRAIG_EMERGENT,
            target: "busy == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "raster_1080p",
            class: "iteration-bound/HOLDS",
            btor2: RASTER_1080P,
            target: "vcount == 1079",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        // The SPCR class (PR #410) + its fragment boundary. `array_content_spcr` is decided ONLY via
        // the SPCR pre-pass (exact SKIPs the in-cone array; the ∀-array cube leaves ⊥) — so the
        // completeness assertion below is a live regression guard on SPCR's marginal reach. The
        // independent-index sibling is HardUnknown (SPCR soundly abstains → the class's honest ⊥).
        Case {
            name: "array_content_spcr",
            class: "array-content-gated/HOLDS",
            btor2: ARRAY_CONTENT_SPCR,
            target: "busy == 0",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        Case {
            name: "array_content_indep_idx",
            class: "array-content-indep-idx/⊥",
            btor2: ARRAY_CONTENT_INDEP,
            target: "busy == 0",
            oracle: Oracle::HardUnknown,
            source: "synthetic",
            pin: NO_PIN,
        },
        // mununu#602 — a 32-bit field of a 23-word (736-bit) packed vector crossed twice, read
        // through a constant part-select. The signal-level cone is 3 × 736 = 2,208 bits (exact
        // abstains on the cap); the bit-level cone is 96 and exact decides. The consumer's
        // `bundle_cdc` shape; synthetic until that design lifts here.
        Case {
            name: "crossed_vector_field",
            class: "wide-vector-slice/HOLDS",
            btor2: CROSSED_VECTOR_FIELD,
            target: "flag_o == 1",
            oracle: Oracle::Holds,
            source: "synthetic",
            pin: NO_PIN,
        },
        // ---- RTL class-representatives (lifted designs — the curation target) ----
        Case {
            name: "i2c_scl_padoen",
            class: "register-dominated/⊥",
            btor2: I2C_SCL_PADOEN,
            target: "scl_padoen_o == 1",
            oracle: Oracle::HardUnknown,
            source: "RTL:i2c",
            pin: NO_PIN,
        },
        Case {
            name: "aes_cipher (free)",
            class: "decidable/HOLDS",
            btor2: AES_CIPHER,
            target: "aes_cipher_ctrl_cs == 9",
            oracle: Oracle::Holds,
            source: "RTL:opentitan-aes",
            pin: NO_PIN,
        },
        Case {
            name: "aes_cipher (rst pinned)",
            class: "operational/VIOLATED",
            btor2: AES_CIPHER,
            target: "aes_cipher_ctrl_cs == 9",
            oracle: Oracle::Violated,
            source: "RTL:opentitan-aes",
            pin: PIN_RST,
        },
        Case {
            name: "csrng (free)",
            class: "decidable/HOLDS",
            btor2: CSRNG,
            target: "state_q == 55",
            oracle: Oracle::Holds,
            source: "RTL:opentitan-csrng",
            pin: NO_PIN,
        },
        Case {
            name: "csrng (rst pinned)",
            class: "operational/VIOLATED",
            btor2: CSRNG,
            target: "state_q == 55",
            oracle: Oracle::Violated,
            source: "RTL:opentitan-csrng",
            pin: PIN_RST,
        },
        Case {
            name: "aes_ctr (free)",
            class: "decidable/HOLDS",
            btor2: AES_CTR,
            target: "aes_ctr_cs == 14",
            oracle: Oracle::Holds,
            source: "RTL:opentitan-aes",
            pin: NO_PIN,
        },
        Case {
            name: "aes_ctr (rst pinned)",
            class: "operational/VIOLATED",
            btor2: AES_CTR,
            target: "aes_ctr_cs == 14",
            oracle: Oracle::Violated,
            source: "RTL:opentitan-aes",
            pin: PIN_RST,
        },
    ]
}

fn v(r: &Result<PropertyVerdict, String>) -> &'static str {
    match r {
        Ok(PropertyVerdict::Holds) => "HOLDS",
        Ok(PropertyVerdict::Violated) => "VIOL ",
        Ok(PropertyVerdict::Unknown) => "  ⊥  ",
        Ok(PropertyVerdict::Skipped) => "SKIP ",
        Err(_) => "ERR  ",
    }
}

fn is_holds(r: &Result<PropertyVerdict, String>) -> bool {
    matches!(r, Ok(PropertyVerdict::Holds))
}
fn is_viol(r: &Result<PropertyVerdict, String>) -> bool {
    matches!(r, Ok(PropertyVerdict::Violated))
}
fn decides(r: &Result<PropertyVerdict, String>, want: PropertyVerdict) -> bool {
    matches!(r, Ok(x) if *x == want)
}

#[test]
#[ignore = "validation harness: wide cones + cube-craig needs MathSAT (mununu-sva-pono)"]
fn wall_class_matrix() {
    let mathsat = std::env::var_os("MUNUNU_MATHSAT_PATH").is_some();
    println!(
        "\n{:32} {:26} {:>6} {:>6} {:>6} {:>6} {:>7}",
        "case", "class", "dflt", "wp", "craig", "spcr", "comb"
    );
    println!("{}", "-".repeat(98));

    let mut soundness_violations = Vec::new();
    let mut craig_unique = Vec::new();
    let mut spcr_decided = Vec::new(); // array-content cases the SPCR pre-pass decides (its reach)
    let mut combined_fullspace = Vec::new(); // ⊥-singletons → combined decides with NO pins (transfers)

    let mut rtl_classes = std::collections::BTreeSet::new();
    let mut synthetic_classes = std::collections::BTreeSet::new();

    for c in cases() {
        // Bake the case's reset/config pins (e.g. `rst_ni=1` — the standard operational
        // reset-gating) into the model BEFORE the levers. Sound: the exact engine on the
        // concrete pinned model is 2-valued sound (the operational, out-of-reset question).
        let model = if c.pin.is_empty() {
            c.btor2.to_string()
        } else {
            let pins: Vec<(String, u64)> = c.pin.iter().map(|(n, v)| (n.to_string(), *v)).collect();
            pin_inputs_to_constants(c.btor2, &pins).0
        };
        if is_rtl(&c) {
            rtl_classes.insert(c.class);
        } else {
            synthetic_classes.insert(c.class);
        }
        let dflt = verify_recoverability(&model, c.target);
        let wp = verify_recoverability_scalable(&model, c.target, &[]);
        let craig = verify_recoverability_scalable_with_source(
            &model,
            c.target,
            &[],
            PredicateSource::CraigInterpolation,
        );
        // The SPCR pre-pass in ISOLATION (its own `spcr` column): Holds/Violated when SPCR applies +
        // exact decides the array-free design, Skipped when SPCR does not apply (no array / outside
        // the fragment → sound abstention). Attributes an array-content decision to SPCR — otherwise
        // invisible because SPCR is embedded inside `dflt`/`wp`.
        let spcr = verify_recoverability_spcr_only(&model, c.target);
        // The COMPOSED plan (exact-first + cube + ranking + guard + Craig) — the whole point of a
        // planner: a lever that is 0 alone can contribute in combination. It runs only SOUND,
        // transferable levers (the earlier unsound auto-input-pinning was removed — see
        // `solve_recoverability_combined`), so its verdict is full-space and oracle-comparable.
        let combined = solve_recoverability_combined(&model, c.target);
        println!(
            "{:32} {:26} {:>6} {:>6} {:>6} {:>6} {:>7}",
            c.name,
            c.class,
            v(&dflt),
            v(&wp),
            v(&craig),
            v(&spcr),
            v(&combined)
        );

        // SOUNDNESS invariant — no lever may contradict the oracle. The `combined` plan is included:
        // it now returns only full-space, transferable verdicts, so a contradiction would be a real
        // composition-soundness bug (the gap the removed input-pinning previously hid).
        for (lever, r) in [
            ("default", &dflt),
            ("cube-wp", &wp),
            ("cube-craig", &craig),
            ("spcr", &spcr),
            ("combined", &combined),
        ] {
            let bad = match c.oracle {
                Oracle::Holds => is_viol(r),
                Oracle::Violated => is_holds(r),
                Oracle::NeverHolds => is_holds(r),
                Oracle::HardUnknown => false,
            };
            if bad {
                soundness_violations.push(format!("{}::{} = {}", c.name, lever, v(r)));
            }
        }

        // COMPLETENESS — a decidable case is decided by at least one lever.
        match c.oracle {
            Oracle::Holds => assert!(
                decides(&dflt, PropertyVerdict::Holds)
                    || decides(&wp, PropertyVerdict::Holds)
                    || decides(&craig, PropertyVerdict::Holds)
                    || decides(&spcr, PropertyVerdict::Holds),
                "{} (HOLDS) is decided by no lever",
                c.name
            ),
            Oracle::Violated => assert!(
                decides(&dflt, PropertyVerdict::Violated)
                    || decides(&wp, PropertyVerdict::Violated),
                "{} (VIOLATED) is decided by no lever",
                c.name
            ),
            _ => {}
        }

        // Craig's UNIQUE reach: a case cube-wp leaves ⊥ that cube-craig decides.
        if matches!(wp, Ok(PropertyVerdict::Unknown)) && is_holds(&craig) {
            craig_unique.push(c.name);
        }

        // SPCR's reach: an array-content case the SPCR pre-pass decides (Holds/Violated) in
        // isolation. exact-alone SKIPs it (in-cone `$mem`) and the plain ∀-array cube leaves ⊥, so a
        // decision here is SPCR's marginal contribution.
        if is_holds(&spcr) || is_viol(&spcr) {
            spcr_decided.push(c.name);
        }

        // The COMBINE-MECHANISMS hypothesis: a case EVERY singleton leaves ⊥ that the composed
        // plan DECIDES — a genuine FULL-SPACE combination win (transfers). The composed plan now
        // runs only SOUND, transferable levers (no unsound input-pinning), so any decision it
        // reaches is full-space and oracle-comparable.
        let all_singletons_bottom = [&dflt, &wp, &craig]
            .iter()
            .all(|r| matches!(r, Ok(PropertyVerdict::Unknown)));
        let combined_decides = matches!(
            combined,
            Ok(PropertyVerdict::Holds | PropertyVerdict::Violated)
        );
        if all_singletons_bottom && combined_decides {
            combined_fullspace.push(c.name);
        }
    }

    println!("{}", "-".repeat(92));
    assert!(
        soundness_violations.is_empty(),
        "SOUNDNESS VIOLATIONS (a lever contradicted the oracle): {soundness_violations:?}"
    );

    // The COMBINE-MECHANISMS hypothesis (the planner's raison d'être) — measured, not assumed. The
    // composed plan is the union of its SOUND component levers (exact-first ∪ scalable-cube), so a
    // win here is a case every singleton leaves ⊥ that their sound composition decides + TRANSFERS.
    if combined_fullspace.is_empty() {
        println!(
            "MEASURED: no genuine FULL-SPACE combination win on this set. Among the SOUND, \
             transferable levers the composed plan is the union of exact-first and the scalable-cube \
             path (cube+ranking+guard+Craig): every case is decided by a single sound lever, so \
             composition adds only union COVERAGE, no synergy. (The earlier apparent 'win' — \
             `staller` HOLDS* / `trap_uf_w48` HOLDS* — was the removed UNSOUND auto-input-pinning \
             producing non-transferable scoped verdicts; AG EF is not monotone under \
             input-restriction, so those never transferred.) Combination stays the right DEFAULT for \
             COVERAGE + the soundness cross-check (exact corroborating/overturning cube); it is not a \
             new decidability lever on this set."
        );
    } else {
        println!(
            "COMBINATION WIN (full-space, transfers — every singleton was ⊥, the sound composition \
             decided): {combined_fullspace:?}"
        );
    }

    // Craig's MARGINAL reach is MEASURED, not presupposed. Through the production pipeline the
    // WP path already runs guard-atom extraction (reads a syntactic invariant like `data==target`
    // off the design's own `eq` node), so it subsumes the invariant-class cases whose invariant is
    // syntactically present. Craig's unique value is only a NON-syntactic emergent invariant — the
    // research frontier. An empty `craig_unique` here is a real finding: on this set the shipped
    // guard-atom lever covers what Craig would, so a Craig planner-operator adds no marginal reach.
    if mathsat {
        if craig_unique.is_empty() {
            println!(
                "MEASURED: cube-craig has NO unique reach on this set (WP+guard-atoms subsume it). \
                 A Craig operator adds no marginal decision here; its value needs a NON-syntactic \
                 emergent-invariant case (research frontier — not yet in the set)."
            );
        } else {
            println!("cube-craig UNIQUELY decided (wp ⊥ → craig HOLDS): {craig_unique:?}");
        }
    } else {
        println!(
            "MathSAT absent (MUNUNU_MATHSAT_PATH unset): cube-craig falls back to WP; \
             run in mununu-sva-pono to exercise + measure the Craig column."
        );
    }

    // SPCR's MARGINAL reach (PR #410) — MEASURED, not presupposed. The `spcr` column decides the
    // array-content-gated νµ class (`array_content_spcr`) that exact-alone SKIPs (in-cone `$mem`) and
    // the plain ∀-array cube leaves ⊥; on every non-array case it correctly SKIPs (no marginal reach,
    // no harm), and on the independent-index sibling it SKIPs too (the sound fragment boundary). The
    // class is measured-ABSENT from real RTL corpora (2026-07-29), so this is a MECHANISM guard.
    println!("{}", "-".repeat(98));
    if spcr_decided.is_empty() {
        println!(
            "MEASURED: SPCR decides nothing on this set — a REGRESSION (the array-content case must \
             be SPCR-decided; exact SKIPs it and the ∀-array cube is ⊥)."
        );
    } else {
        println!(
            "SPCR uniquely enables the array-content-gated νµ class (exact SKIP + ∀-array cube ⊥ → \
             SPCR HOLDS): {spcr_decided:?}. Fragment boundary: the independent-index sibling → SKIP \
             (sound abstain). Class measured-ABSENT from real RTL — mechanism guard, not corpus-decide."
        );
    }
    // Guard the reach so a silent SPCR regression fails the matrix, not just the printout.
    assert!(
        spcr_decided.contains(&"array_content_spcr"),
        "SPCR must decide the array-content-gated νµ case (its whole reach); exact SKIPs it and the \
         ∀-array cube leaves ⊥, so a non-decision is an SPCR regression"
    );

    // RTL-COVERAGE tracker (requirement 2026-07-26: an RTL representative for every class). The
    // synthetic fixtures are valid MECHANISM checks; RTL adds realism. This prints the classes
    // that HAVE a lifted-RTL representative vs those still synthetic-only, so the gap is explicit
    // and shrinks visibly as P1.1 lifts more designs (OpenTitan via the #396 Auto→slang fallback).
    println!("{}", "-".repeat(92));
    println!("RTL-backed classes: {rtl_classes:?}");
    let synthetic_only: Vec<_> = synthetic_classes.difference(&rtl_classes).collect();
    println!("synthetic-only (awaiting RTL representative): {synthetic_only:?}");
}

/// Config-partition on REAL OpenTitan RTL — the refined-verdicts capability-A industrial validation
/// (A3). The `AG EF(idle)` recoverability of the aes_cipher / csrng control FSMs DEPENDS on `rst_ni`:
/// held in reset (`rst_ni=0`, active-low) the FSM stays idle (HOLDS); operational (`rst_ni=1`) it
/// traps in its absorbing error state (VIOLATED). `config_partition` turns that branching pair — the
/// SVA-inexpressible differentiator (§3.3) — into ONE `ConfigDependent` verdict. Sound per cell (each
/// pinned `rst_ni` is a concrete model the exact engine decides). Host-runnable on the checked-in
/// lifted fixtures (small FSMs), so NOT `#[ignore]` — it gates `make ci`.
#[test]
fn config_partition_over_reset_partitions_opentitan_fsms() {
    use mununu_core::adapter::recoverability::config_partition;

    let aes = config_partition(
        AES_CIPHER,
        "aes_cipher_ctrl_cs == 9",
        &[("rst_ni".to_string(), vec![0, 1])],
    )
    .expect("aes_cipher AG EF(idle) depends on rst_ni ⇒ a ConfigDependent partition");
    assert!(
        aes.violated.contains(&vec![("rst_ni".to_string(), 1)]),
        "operational (rst_ni=1) traps in the absorbing error state: {aes:?}"
    );
    assert!(
        aes.holds.contains(&vec![("rst_ni".to_string(), 0)]),
        "held in reset (rst_ni=0) stays idle: {aes:?}"
    );

    let csrng = config_partition(
        CSRNG,
        "state_q == 55",
        &[("rst_ni".to_string(), vec![0, 1])],
    )
    .expect("csrng_main_sm AG EF(idle) depends on rst_ni ⇒ a ConfigDependent partition");
    assert!(
        csrng.violated.contains(&vec![("rst_ni".to_string(), 1)]),
        "operational csrng traps: {csrng:?}"
    );
    assert!(
        csrng.holds.contains(&vec![("rst_ni".to_string(), 0)]),
        "held-in-reset csrng stays idle: {csrng:?}"
    );
}

/// Slice 2b (AUTO config-atom identification) on the SAME OpenTitan RTL — a bare refined verify (NO
/// `--config-values`) auto-identifies `rst_ni` (the structurally-detected reset) as the config axis and
/// reproduces the ConfigDependent partition. Proves the user need not name the reset: the reset-mux scan
/// ([`detect_resets`]) does it. This is the industrial validation of the 2b auto path — host-runnable,
/// gates `make ci`.
#[test]
fn auto_config_partition_identifies_reset_on_opentitan_fsms() {
    use mununu_core::adapter::recoverability::verify_recoverability_refined;

    let (_v, aes) =
        verify_recoverability_refined(AES_CIPHER, "aes_cipher_ctrl_cs == 9", &[], &[], false);
    let part = aes
        .config_partition
        .expect("bare --refine auto-identifies rst_ni on aes_cipher");
    assert!(
        part.violated.contains(&vec![("rst_ni".to_string(), 1)])
            && part.holds.contains(&vec![("rst_ni".to_string(), 0)]),
        "auto reset-partition reproduces the aes_cipher branching pair: {part:?}"
    );

    let (_v, csrng) = verify_recoverability_refined(CSRNG, "state_q == 55", &[], &[], false);
    let part = csrng
        .config_partition
        .expect("bare --refine auto-identifies rst_ni on csrng");
    assert!(
        part.violated.contains(&vec![("rst_ni".to_string(), 1)])
            && part.holds.contains(&vec![("rst_ni".to_string(), 0)]),
        "auto reset-partition reproduces the csrng branching pair: {part:?}"
    );
}

/// Capability B (assumption discovery, Phase 2a) — marginal reach on the fixed wall-class set (the
/// lever RULE). On an INPUT-GATED trap the lever turns a flat VIOLATED into a conditional
/// `HoldsUnder(input-hold)` — the enabling assumption that keeps the trap unreachable; on an
/// UNCONDITIONAL trap (no input avoids it, and the only stuck-at-good hold is vacuous) it correctly
/// finds NOTHING. Measures both the lever's reach and its no-false-positive boundary. The canonical
/// verdict is UNCHANGED in both (conditional-only). Host-runnable, gates `make ci`.
#[test]
fn assumption_discovery_marginal_reach_on_input_gated_trap() {
    use mununu_core::adapter::recoverability::verify_recoverability_refined;
    // Input-gated trap: IDLE(0) --go--> WORK(1); WORK --en=0--> FAULT(2, absorbing) / --en=1--> IDLE.
    // Held `en==1` keeps FAULT unreachable ⇒ a non-vacuous HOLDS ⇒ a discovered assumption.
    const EN_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 en
4 input 2 go
5 state 1 st
6 zero 1
7 init 1 5 6
8 one 1
9 constd 1 2
10 eq 2 5 6
11 eq 2 5 8
13 ite 1 4 8 6
14 ite 1 3 6 9
15 ite 1 11 14 9
16 ite 1 10 13 15
17 next 1 5 16
";
    let (v, r) = verify_recoverability_refined(EN_TRAP, "st == 0", &[], &[], true);
    assert_eq!(
        v,
        PropertyVerdict::Violated,
        "free-input the FAULT trap is reachable ⇒ canonical verdict VIOLATED (unchanged)"
    );
    assert!(
        r.holds_under
            .iter()
            .any(|a| a.phi == "en == 1" && a.non_vacuous),
        "assumption discovery REACHES the input-gated-trap class (HoldsUnder(en==1)): {:?}",
        r.holds_under
    );

    // Unconditional trap (STALLER): st=1→3→3 absorbing; the only input hold `go==0` keeps st stuck at
    // IDLE (vacuous — good never left, gate rejects it) and `go==1` leads to the trap ⇒ NO assumption.
    let (v2, r2) = verify_recoverability_refined(STALLER, "st == 0", &[], &[], true);
    assert_eq!(v2, PropertyVerdict::Violated);
    assert!(
        r2.holds_under.is_empty(),
        "an unconditional trap has no enabling input-hold ⇒ no false-positive assumption: {:?}",
        r2.holds_under
    );
}

/// Phase 2b (symbolic env-strategy synthesis, EnvStrategy lever) — marginal reach on the fixed set (the
/// lever RULE). On a POSITIONAL-strategy trap (the safe input DIFFERS per state, so no CONSTANT hold
/// recovers) the symbolic safety game `νX. EF(good) ∧ ◇X` finds a maintaining strategy that slice-1's
/// constant holds CANNOT — the marginal reach over Phase-2a slice 1. STALLER (unconditional trap) is
/// the no-false-positive control (already asserted above: no EnvStrategy either). Canonical verdict
/// stays VIOLATED (conditional-only). Host-runnable, gates `make ci`.
#[test]
fn env_strategy_marginal_reach_over_constant_hold() {
    use mununu_core::adapter::recoverability::verify_recoverability_refined;
    // A(0)→B(1)→C(2)→A cycle; safe input is x=0 at A, x=1 at B, x=0 at C; the wrong input drops to
    // TRAP(3). No constant hold recovers; a positional env strategy stays in {A,B,C}.
    const POSITIONAL_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 x
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 5
11 eq 2 4 7
12 eq 2 4 8
13 ite 1 3 9 7
14 ite 1 3 8 9
15 ite 1 3 9 5
16 ite 1 12 15 9
17 ite 1 11 14 16
18 ite 1 10 13 17
19 next 1 4 18
";
    let (v, r) = verify_recoverability_refined(POSITIONAL_TRAP, "st == 0", &[], &[], true);
    assert_eq!(
        v,
        PropertyVerdict::Violated,
        "free-input the positional trap is VIOLATED (unchanged, conditional-only)"
    );
    assert!(
        r.holds_under.iter().any(|a| matches!(
            a.kind,
            mununu_core::verdict::AssumptionKind::EnvStrategy
        ) && a.non_vacuous),
        "the symbolic env-strategy REACHES the positional-trap class where a constant hold cannot: {:?}",
        r.holds_under
    );
}

/// P2.5-E — the REAL-RTL positional env-strategy case (the §0.7 search payoff). The OpenTitan EDN boot
/// handshake (`edn_boot_sm`, extracted verbatim) needs `boot_req_mode_i=1` in Idle to START the boot flow
/// and `=0` in BootDone to PROGRESS to BootUniAckWait — opposite values in two states, so NO constant
/// input-hold drives a full boot, but a POSITIONAL strategy does. Confirmed signature: free-input
/// `AG EF(state_q==44)` HOLDS (∃ the positional path) yet BOTH constant `boot_req_mode_i` holds are
/// VIOLATED. This is the genuine real-RTL demand for the strategy-witness machinery (T2). Host-runnable
/// (small FSM), gates `make ci`.
#[test]
fn edn_boot_handshake_is_a_positional_env_strategy_case() {
    use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
    let good = "state_q == 44"; // BootUniAckWait
    // Free-input: the environment CAN drive the full boot handshake ⇒ AG EF HOLDS.
    assert_eq!(
        verify_recoverability(EDN_BOOT, good).ok(),
        Some(PropertyVerdict::Holds),
        "free-input the boot handshake is completable (the positional path exists)"
    );
    // But NO constant `boot_req_mode_i` hold works — the recovering discipline is state-dependent.
    for v in [0u64, 1] {
        let (pinned, _) = pin_inputs_to_constants(EDN_BOOT, &[("boot_req_mode_i".to_string(), v)]);
        assert_eq!(
            verify_recoverability(&pinned, good).ok(),
            Some(PropertyVerdict::Violated),
            "constant boot_req_mode_i={v} cannot complete the boot (positional, no constant hold)"
        );
    }
}

/// P2.5-E §0.7 — MEASURE-FIRST scan (gates T2/T3). Question: does a REAL-RTL ⊥/VIOLATED recoverability
/// case have an environment strategy (`exact_env_strategy_exists = Some(true)`) that rescues a NON-Holds
/// base — i.e. env-strategy has marginal reach beyond the shipped memoryless constant-hold? If only the
/// synthetic POSITIONAL_TRAP shows the gap and no real RTL row does, the positional/general strategy is a
/// SYNTHETIC-ONLY capability (the binary-landscape risk) → T2/T3 stay documented-not-headlined.
/// `#[ignore]` — run on demand: `cargo test -p mununu-core --test wall_class_matrix -- --ignored --nocapture env_strategy_positional_marginal_reach_scan`
#[test]
#[ignore]
#[allow(clippy::type_complexity)] // the scan's row tuple is local + self-documenting
fn env_strategy_positional_marginal_reach_scan() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_strategy_exists;
    use mununu_core::mu_calculus::parser;

    // Synthetic positive control: per-state safe input, no constant hold works (T1's marginal case).
    const POSITIONAL_TRAP: &str = "\
1 sort bitvec 2\n2 sort bitvec 1\n3 input 2 x\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 one 1\n8 constd 1 2\n9 constd 1 3\n10 eq 2 4 5\n11 eq 2 4 7\n12 eq 2 4 8\n13 ite 1 3 9 7\n14 ite 1 3 8 9\n15 ite 1 3 9 5\n16 ite 1 12 15 9\n17 ite 1 11 14 16\n18 ite 1 10 13 17\n19 next 1 4 18\n";

    // `exact_env_strategy_exists` applies the `env_enforce` rewrite internally, so the scan passes the
    // plain recoverability formula.
    let recov =
        |good: &str| parser::parse(&format!("nu Y. ((mu X. (({good}) || <> X)) && [] Y)")).unwrap();
    let rows: Vec<(&str, &str, &str, &[(&str, u64)])> = vec![
        (
            "POSITIONAL_TRAP (synthetic control)",
            POSITIONAL_TRAP,
            "st == 0",
            &[],
        ),
        ("STALLER (uncond trap, synthetic)", STALLER, "st == 0", &[]),
        (
            "aes_cipher (free)",
            AES_CIPHER,
            "aes_cipher_ctrl_cs == 9",
            &[],
        ),
        (
            "aes_cipher (operational rst_ni=1)",
            AES_CIPHER,
            "aes_cipher_ctrl_cs == 9",
            &[("rst_ni", 1)],
        ),
        (
            "csrng (operational rst_ni=1)",
            CSRNG,
            "state_q == 55",
            &[("rst_ni", 1)],
        ),
        (
            "aes_ctr (operational rst_ni=1)",
            AES_CTR,
            "aes_ctr_cs == 14",
            &[("rst_ni", 1)],
        ),
    ];
    println!("\n=== P2.5-E §0.7 env-strategy marginal-reach scan ===");
    let mut real_gap = 0;
    for (name, btor2, good, pins) in rows {
        let src = if pins.is_empty() {
            btor2.to_string()
        } else {
            let p: Vec<(String, u64)> = pins.iter().map(|(n, v)| (n.to_string(), *v)).collect();
            pin_inputs_to_constants(btor2, &p).0
        };
        let base = verify_recoverability(&src, good);
        let exists = exact_env_strategy_exists(&src, &recov(good));
        let non_holds = !matches!(base, Ok(PropertyVerdict::Holds));
        let gap = matches!(exists, Ok(Some(true))) && non_holds;
        if gap && !name.contains("synthetic") {
            real_gap += 1;
        }
        println!(
            "{name:38} base={:?}  env_strategy_exists={:?}  {}",
            base.as_ref().map(|v| v.as_str()).unwrap_or("Err"),
            exists,
            if gap {
                "<-- env-strategy rescues a NON-Holds row"
            } else {
                ""
            }
        );
    }
    println!(
        "\nreal-RTL rows where env-strategy rescues a non-Holds base: {real_gap} \
         (0 ⇒ positional/general strategy is SYNTHETIC-ONLY ⇒ T2/T3 documented-not-headlined)\n"
    );
}

/// P2.5-E T2 — the strategy-WITNESS extraction, validated on the real OpenTitan EDN boot handshake.
/// `exact_env_strategy_witness` returns a concrete winning play that EXHIBITS the positional discipline:
/// `boot_req_mode_i = 1` at Idle (state 193, to START the boot flow) and `= 0` at BootDone (state 240,
/// to PROGRESS to BootLoadUni) — the same input at opposite values in two states, the strategy no
/// constant hold can express. This is the deeper machinery (T2) doing real work on real RTL.
#[test]
fn edn_witness_play_exhibits_positional_strategy() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_strategy_witness;
    let play =
        exact_env_strategy_witness(EDN_BOOT, "state_q == 44").expect("a winning play exists");
    // Per-step (state, boot_req) pairs from the play (state cell resolves to `state_d` post-async2sync).
    let step = |want_state: u128| -> Option<u128> {
        play.prefix
            .iter()
            .zip(play.inputs.iter())
            .find_map(|(st, inp)| {
                let s = st
                    .iter()
                    .find(|(k, _)| k.contains("state"))
                    .map(|(_, v)| *v)?;
                (s == want_state)
                    .then(|| inp.get("boot_req_mode_i").copied())
                    .flatten()
            })
    };
    assert_eq!(
        step(193),
        Some(1),
        "positional: the strategy asserts boot_req_mode_i=1 at Idle (193) to start the boot"
    );
    assert_eq!(
        step(240),
        Some(0),
        "positional: the strategy deasserts boot_req_mode_i=0 at BootDone (240) to progress"
    );
    assert_eq!(
        play.cycle
            .first()
            .and_then(|s| s.iter().find(|(k, _)| k.contains("state")).map(|(_, v)| *v)),
        Some(44),
        "the play reaches BootUniAckWait (44)"
    );
}

/// P2.5-E — the MONOTONE A/B CONTROL for the edn positional case (the second-case survey payoff). The
/// OpenTitan CSRNG app-command FSM (`csrng_main_sm_fsm`, extracted verbatim) reaches MainSmCmdVld
/// (`state_q==16`) through a straight assert-to-progress pipeline. Unlike edn's boot handshake, a SINGLE
/// CONSTANT input vector drives the command to validation and cycles indefinitely — every gating input
/// advances the flow when asserted and never needs the opposite value later. So the environment-strategy
/// lever has ZERO MARGINAL REACH here: the shipped constant-hold (Phase-2a slice 1) already suffices,
/// where on edn BOTH constant `boot_req_mode_i` holds are VIOLATED and only a positional strategy works.
///
/// This is the wall-class-matrix marginal-reach discipline (the RULE) applied to the env-strategy lever:
/// it must decide the edn-class POSITIONAL case AND leave the csrng-class MONOTONE case unchanged (no
/// over-firing). Both are real OpenTitan CSRNG-family FSMs. Host-runnable, gates `make ci`.
#[test]
fn csrng_command_flow_is_monotone_not_positional() {
    use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
    let good = "state_q == 16"; // MainSmCmdVld
    // Free-input: a command can be driven to validation ⇒ AG EF HOLDS.
    assert_eq!(
        verify_recoverability(CSRNG_MAIN_SM, good).ok(),
        Some(PropertyVerdict::Holds),
        "free-input the command flow reaches CmdVld"
    );
    // MONOTONE: a SINGLE CONSTANT input vector already reaches (and re-reaches) CmdVld — safety inputs
    // held safe (local_escalate_i=0, enable_i=1), functional inputs all asserted (acmd_i=GEN=3 takes the
    // no-entropy path). No input needs a state-dependent value ⇒ a constant strategy suffices ⇒ the
    // env-strategy lever has zero marginal reach (contrast edn, where every constant boot_req VIOLATED).
    let monotone_hold: &[(String, u64)] = &[
        ("enable_i".to_string(), 1),
        ("acmd_avail_i".to_string(), 1),
        ("acmd_eop_i".to_string(), 1),
        ("acmd_i".to_string(), 3), // GEN → skips MainSmEntropyReq
        ("flag0_i".to_string(), 1),
        ("cmd_rdy_i".to_string(), 1),
        ("cmd_complete_i".to_string(), 1),
        ("local_escalate_i".to_string(), 0),
    ];
    let (pinned, _) = pin_inputs_to_constants(CSRNG_MAIN_SM, monotone_hold);
    assert_eq!(
        verify_recoverability(&pinned, good).ok(),
        Some(PropertyVerdict::Holds),
        "a CONSTANT strategy reaches CmdVld — csrng's command flow is monotone, not positional"
    );
}

/// P2.5-E — the SECOND real POSITIONAL env-strategy case, on the OpenTitan OTBN start/stop controller
/// (`otbn_start_stop_fsm`, extracted verbatim). A different module family from edn (a compute-engine
/// start → execute → secure-wipe handshake) and a different positional FLAVOR: reaching Running
/// (`state_q==119`) needs `urnd_reseed_ack_i` at OPPOSITE values in two states — `=0` at Halt (a stray
/// ACK outside {Initial, UrndRefresh} hits the spurious-ACK security trap → Locked) and `=1` at
/// UrndRefresh (to progress to Running). This is an "acknowledge only when expected" discipline: no
/// CONSTANT ACK hold reaches Running (const-0 never leaves Initial/UrndRefresh; const-1 traps at Halt),
/// yet a POSITIONAL strategy does. Escalation + RMA held inactive (`escalate_i=0, rma_req_i=0` — normal
/// operation) to isolate the ACK handshake; the free reset provides the AG-escape from traps.
/// Host-runnable, gates `make ci`.
#[test]
fn otbn_start_stop_ack_is_a_second_positional_env_strategy_case() {
    use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
    let good = "state_q == 119"; // OtbnStartStopStateRunning
    // Escalation + RMA inactive (normal operation); optionally pin one more input to a constant.
    let inactive = |extra: &[(&str, u64)]| -> String {
        let mut pins: Vec<(String, u64)> =
            vec![("escalate_i".to_string(), 0), ("rma_req_i".to_string(), 0)];
        for (n, v) in extra {
            pins.push((n.to_string(), *v));
        }
        pin_inputs_to_constants(OTBN_START_STOP, &pins).0
    };
    // Free (ACK, start, secure_wipe, reset all free): the env CAN drive to Running ⇒ AG EF HOLDS.
    assert_eq!(
        verify_recoverability(&inactive(&[]), good).ok(),
        Some(PropertyVerdict::Holds),
        "free-input the start/refresh handshake reaches Running (the positional path exists)"
    );
    // But NO constant `urnd_reseed_ack_i` hold reaches Running — the ACK discipline is state-dependent.
    for v in [0u64, 1] {
        assert_eq!(
            verify_recoverability(&inactive(&[("urnd_reseed_ack_i", v)]), good).ok(),
            Some(PropertyVerdict::Violated),
            "constant urnd_reseed_ack_i={v} cannot reach Running (positional, no constant ACK hold)"
        );
    }
}

/// P2.5-E T2 — the strategy-WITNESS extraction, validated on the SECOND real positional case (OTBN).
/// `exact_env_strategy_witness` returns a concrete winning play that EXHIBITS the ACK discipline:
/// `urnd_reseed_ack_i = 0` at Halt (state 1 — a stray ACK there would trap to Locked) and `= 1` at
/// UrndRefresh (state 242 — to progress), reaching Running (state 119). Escalation + RMA held inactive
/// (the property's scope). This shows T2 generalizes beyond edn to a second module family + a second
/// positional flavor (acknowledge-when-expected vs edn's request-then-drop).
#[test]
fn otbn_witness_play_exhibits_positional_ack_strategy() {
    use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_strategy_witness;
    let inactive = pin_inputs_to_constants(
        OTBN_START_STOP,
        &[("escalate_i".to_string(), 0), ("rma_req_i".to_string(), 0)],
    )
    .0;
    let play =
        exact_env_strategy_witness(&inactive, "state_q == 119").expect("a winning play exists");
    let step = |want_state: u128| -> Option<u128> {
        play.prefix
            .iter()
            .zip(play.inputs.iter())
            .find_map(|(st, inp)| {
                let s = st
                    .iter()
                    .find(|(k, _)| k.contains("state"))
                    .map(|(_, v)| *v)?;
                (s == want_state)
                    .then(|| inp.get("urnd_reseed_ack_i").copied())
                    .flatten()
            })
    };
    assert_eq!(
        step(1),
        Some(0),
        "positional: the strategy keeps urnd_reseed_ack_i=0 at Halt (a stray ACK would trap to Locked)"
    );
    assert_eq!(
        step(242),
        Some(1),
        "positional: the strategy asserts urnd_reseed_ack_i=1 at UrndRefresh (to progress to Running)"
    );
    assert_eq!(
        play.cycle
            .first()
            .and_then(|s| s.iter().find(|(k, _)| k.contains("state")).map(|(_, v)| *v)),
        Some(119),
        "the play reaches Running (119)"
    );
}

/// P2.5-E — the THIRD real POSITIONAL env-strategy case, on the OpenTitan ROM controller check FSM
/// (`rom_ctrl_fsm`, extracted verbatim). A different security-critical function (secure-boot ROM
/// integrity) and a SECOND instance of the otbn "ack-when-expected" flavor — confirming that discipline
/// recurs across OpenTitan security FSMs rather than being a one-off. `kmac_done_i` (the KMAC-digest-ready
/// ACK) must take opposite values in two states to reach the terminal Done (`state_q==518`): `=0` at
/// ReadingLow (a stray ACK trips the SEC_CM consistency check → Invalid) and `=1` at ReadingHigh/RomAhead
/// (to hand off the digest and progress to Checking→Done). The FSM is LINEAR (upstream
/// `ASSERT_FPV_LINEAR_FSM`) so Done is unique-path; NO constant ACK reaches it (const-1 → Invalid at
/// ReadingLow; const-0 → stalls at RomAhead), yet a POSITIONAL strategy does. The rom_ctrl_counter /
/// rom_ctrl_compare submodules are black-boxed to free inputs (the standard chaotic-stub over-approx);
/// the free reset provides the AG-escape from Invalid. Host-runnable, gates `make ci`.
#[test]
fn rom_ctrl_kmac_ack_is_a_third_positional_env_strategy_case() {
    use mununu_core::adapter::btor2::pin::pin_inputs_to_constants;
    let good = "state_q == 518"; // Done
    // Free-input: the env CAN drive the ROM check to completion ⇒ AG EF HOLDS.
    assert_eq!(
        verify_recoverability(ROM_CTRL_FSM, good).ok(),
        Some(PropertyVerdict::Holds),
        "free-input the ROM check reaches Done (the positional path exists)"
    );
    // But NO constant `kmac_done_i` hold reaches Done — the ACK discipline is state-dependent.
    for v in [0u64, 1] {
        let (pinned, _) = pin_inputs_to_constants(ROM_CTRL_FSM, &[("kmac_done_i".to_string(), v)]);
        assert_eq!(
            verify_recoverability(&pinned, good).ok(),
            Some(PropertyVerdict::Violated),
            "constant kmac_done_i={v} cannot reach Done (positional, no constant ACK hold)"
        );
    }
}

/// P2.5-E T2 — the strategy-WITNESS extraction on the THIRD case (ROM controller). The play exhibits the
/// ack-when-expected discipline: `kmac_done_i = 0` at ReadingLow (state 201 — a stray ACK would trap to
/// Invalid) and `= 1` at ReadingHigh (state 185) or RomAhead (state 921), reaching Done (518). Shows T2
/// generalizes to all three positional cases.
#[test]
fn rom_ctrl_witness_play_exhibits_positional_ack_strategy() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_strategy_witness;
    let play =
        exact_env_strategy_witness(ROM_CTRL_FSM, "state_q == 518").expect("a winning play exists");
    let ack_at = |want_state: u128| -> Option<u128> {
        play.prefix
            .iter()
            .zip(play.inputs.iter())
            .find_map(|(st, inp)| {
                let s = st
                    .iter()
                    .find(|(k, _)| k.contains("state"))
                    .map(|(_, v)| *v)?;
                (s == want_state)
                    .then(|| inp.get("kmac_done_i").copied())
                    .flatten()
            })
    };
    assert_eq!(
        ack_at(201),
        Some(0),
        "positional: the strategy keeps kmac_done_i=0 at ReadingLow (a stray ACK would trap to Invalid)"
    );
    // Progress to Checking needs kmac_done_i=1 at ReadingHigh (185) or RomAhead (921) — accept either,
    // since the {kmac_done,counter_done} race picks the path.
    let progressed = ack_at(185) == Some(1) || ack_at(921) == Some(1);
    assert!(
        progressed,
        "positional: the strategy asserts kmac_done_i=1 at ReadingHigh/RomAhead to progress"
    );
    assert_eq!(
        play.cycle
            .first()
            .and_then(|s| s.iter().find(|(k, _)| k.contains("state")).map(|(_, v)| *v)),
        Some(518),
        "the play reaches Done (518)"
    );
}

/// P2.5-E — the FULL positional strategy (positional strategy over reachable states), the generalization of the
/// single witness play. `exact_env_positional_strategy` returns a state-indexed map; for edn the map prescribes
/// `boot_req_mode_i` at OPPOSITE values in two control states — the positional obligation as a strategy
/// rather than one trace: `=1` at Idle (193, request the boot) and `=0` at BootDone (240, release to
/// progress). The map also covers the whole reachable region (every control state gets a discipline).
#[test]
fn edn_full_positional_strategy_maps_boot_req_per_state() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_positional_strategy;
    let strategy =
        exact_env_positional_strategy(EDN_BOOT, "state_q == 44").expect("a strategy exists");
    let forced = |v: u128, sig: &str| -> Option<u128> {
        strategy
            .entries
            .iter()
            .find(|e| e.state_value == v)
            .and_then(|e| e.forced_inputs.get(sig).copied())
    };
    // The SAME input forced to OPPOSITE values in two states — the positional obligation, as a map.
    assert_eq!(
        forced(193, "boot_req_mode_i"),
        Some(1),
        "Idle: request the boot"
    );
    assert_eq!(
        forced(240, "boot_req_mode_i"),
        Some(0),
        "BootDone: release boot_req to progress"
    );
    // The target is already good (rank 0), and the strategy covers the reachable control states.
    assert!(
        strategy
            .entries
            .iter()
            .any(|e| e.state_value == 44 && e.rank == 0)
    );
    assert!(
        strategy.entries.len() >= 5,
        "strategy covers the reachable region, not one path: {} entries",
        strategy.entries.len()
    );
}

/// P2.5-E — the full positional strategy on OTBN. The map captures the "acknowledge only when expected"
/// discipline across ALL states: `urnd_reseed_ack_i = 0` at Halt (1) — a stray ACK would trap — and `= 1`
/// at UrndRefresh (242) to progress to Running (119). The per-control-state forced inputs come from the
/// MIN-RANK (fastest-progress) moves, so the UrndRefresh row is not muddied by the wipe-round register.
#[test]
fn otbn_full_positional_strategy_maps_ack_per_state() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_positional_strategy;
    let strategy = exact_env_positional_strategy(OTBN_START_STOP, "state_q == 119")
        .expect("a strategy exists");
    let forced = |v: u128, sig: &str| -> Option<u128> {
        strategy
            .entries
            .iter()
            .find(|e| e.state_value == v)
            .and_then(|e| e.forced_inputs.get(sig).copied())
    };
    assert_eq!(
        forced(1, "urnd_reseed_ack_i"),
        Some(0),
        "Halt: keep ACK silent (a stray ACK traps to Locked)"
    );
    assert_eq!(
        forced(242, "urnd_reseed_ack_i"),
        Some(1),
        "UrndRefresh: assert ACK to progress to Running"
    );
    assert!(
        strategy
            .entries
            .iter()
            .any(|e| e.state_value == 119 && e.rank == 0)
    );
}

/// P2.5-E — the full positional strategy on the ROM controller. The map prescribes `kmac_done_i = 0` at
/// ReadingLow (201) — a stray ACK trips the consistency check → Invalid — and `= 1` at ReadingHigh (185)
/// to hand off the digest toward Checking → Done (518). The same ack-when-expected discipline as OTBN,
/// on a second security FSM.
#[test]
fn rom_ctrl_full_positional_strategy_maps_ack_per_state() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_env_positional_strategy;
    let strategy =
        exact_env_positional_strategy(ROM_CTRL_FSM, "state_q == 518").expect("a strategy exists");
    let forced = |v: u128, sig: &str| -> Option<u128> {
        strategy
            .entries
            .iter()
            .find(|e| e.state_value == v)
            .and_then(|e| e.forced_inputs.get(sig).copied())
    };
    assert_eq!(
        forced(201, "kmac_done_i"),
        Some(0),
        "ReadingLow: keep the KMAC ACK silent (a stray ACK trips the consistency check)"
    );
    assert_eq!(
        forced(185, "kmac_done_i"),
        Some(1),
        "ReadingHigh: assert the KMAC ACK to progress toward Done"
    );
    assert!(
        strategy
            .entries
            .iter()
            .any(|e| e.state_value == 518 && e.rank == 0)
    );
}
