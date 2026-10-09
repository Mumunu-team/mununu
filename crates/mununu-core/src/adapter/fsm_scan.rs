//! P1 — automatic FSM illegal-encoding scan (no user input).
//!
//! Discovers the FSM-like state registers of a BTOR2 design, derives each one's set of
//! **legal encodings** from the design (the constants its own logic compares it
//! against, plus its reset value), and checks — starting from the real reset state —
//! whether any **illegal** encoding (a value outside that set) is reachable. A
//! reachable illegal encoding is an unambiguous defect: some input drives the FSM to a
//! value it never legally holds (an incomplete `case`, a missing `default`, a decoder
//! that can emit an out-of-range code).
//!
//! # Why this is a clean auto bug-finder (unlike recoverability)
//!
//! Recoverability `AG EF idle` is *tautological* on a reset-carrying design: with reset
//! free the environment can always assert reset to reach idle, so it can never see a
//! trap; with reset held it flags every reset-recoverable-only state, including
//! *intended* error states — neither is a zero-touch bug signal. Illegal-encoding
//! reachability asks a question reset cannot paper over and design intent cannot
//! excuse: **can the next-state logic, for some input, corrupt the register past its
//! enum?** If yes it is a real bug, full stop. It is a *safety* property, decided by the
//! word-level reachability portfolio ([`decide_reach_portfolio`], no bit cap), so it
//! scales past the exact engine's cone.
//!
//! # Starting from a legal state
//!
//! Reachability must start from a LEGAL encoding. yosys-lifted RTL carries no `init`
//! line (reset is an explicit signal), so [`inject_reset_init`] derives the post-reset
//! state (one held-reset cycle) and seeds `init` lines, and the recognized reset inputs
//! are pinned inactive ([`pin_inputs_to_constants`]) — the same "verified out of reset"
//! discipline verify-auto uses. Without this the init-less power-on default 0 is itself
//! an illegal sparse encoding and the scan would trivially "find" it. Hand-written
//! BTOR2 that already carries `init` lines is left as-is.
//!
//! # Scope
//!
//! - **Named, FSM-width state.** A register is scanned if a symbol names it — reported
//!   under its [`parser::canonical_register_names`] name: the cell's own symbol, else a
//!   **value-identical** alias (`uext … 0 state_q`, the port that mirrors it), never a
//!   derived op's loose cone name (mununu#633: `beat_cnt_d` was reported as a register,
//!   for the *state* cell its `case (state_q)` reads) — and it is at most `max_width`
//!   bits wide. A wider register is a datapath / counter, skipped.
//! - **A counter is set aside, with no verdict** (mununu#633). A register whose next-state
//!   logic steps it by ±1 (`cnt + 1`, `cnt - 1`, `inc`/`dec`) takes a *range* of values,
//!   not an enumeration: its "legal set" — the constants it is compared against, `{0,
//!   limit}` — misses every value in between, and the scan would report each as a
//!   reachable illegal encoding. It is listed as [`RegisterKind::Counter`] with
//!   `Skipped`. Arithmetic by any other step (`st + 3`) is NOT a counter: a computed
//!   out-of-enum value is exactly the bug class the scan exists for.
//! - **Non-trivial enum only.** A register whose legal set has fewer than 2 values, or
//!   already covers every value of its width (`|L| ≥ 2^width`), has no illegal encoding
//!   to reach and is skipped.
//! - The reach portfolio abstains (`Unknown`) when no member decides (e.g. over every
//!   engine's ceiling); a definite `Holds` / `Violated` is sound.

use crate::adapter::btor2::ast::{Btor2File, Nid, Node, Op};
use crate::adapter::btor2::bad_monitor::emit_ag_state_in_set_monitor_by_nid;
use crate::adapter::btor2::bit_blast::resolve_btor2_constant;
use crate::adapter::btor2::parser;
use crate::adapter::btor2::pin::pin_inputs_to_constants;
use crate::adapter::btor2::reset_init::inject_reset_init;
use crate::adapter::reach_portfolio::decide_reach_portfolio;
use crate::verdict::PropertyVerdict;
use std::collections::{BTreeSet, HashMap, HashSet};

/// The default "FSM-like" width bound: a state register wider than this is treated as
/// a datapath / counter and skipped (256 encodings is a generous enum ceiling).
pub const DEFAULT_FSM_MAX_WIDTH: u32 = 8;

/// What kind of register a scanned cell is (mununu#633).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterKind {
    /// An enumeration-valued state register: its legal set is checked.
    Fsm,
    /// A register its own next-state logic steps by ±1: its values are a range, not an
    /// enumeration, so it carries no legal set and no verdict.
    Counter,
}

impl RegisterKind {
    /// The wire spelling (`"fsm"` / `"counter"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fsm => "fsm",
            Self::Counter => "counter",
        }
    }
}

/// One register's illegal-encoding result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsmFinding {
    /// The state register's symbol ([`parser::canonical_register_names`]).
    pub register: String,
    /// [`RegisterKind::Fsm`] (checked) or [`RegisterKind::Counter`] (set aside).
    pub kind: RegisterKind,
    /// The legal encodings the register's own logic recognizes (sorted); empty for a
    /// counter.
    pub legal_encodings: Vec<u64>,
    /// `Holds` = the register provably stays within its legal encodings; `Violated` =
    /// an illegal encoding is **reachable** (a finding); `Unknown` = the portfolio
    /// could not decide; `Skipped` = a counter, not checked.
    pub verdict: PropertyVerdict,
}

impl FsmFinding {
    /// A `Violated` verdict is a real finding (a reachable illegal encoding).
    pub fn is_finding(&self) -> bool {
        self.verdict == PropertyVerdict::Violated
    }

    /// An enumeration-valued register the scan checked (counts toward
    /// `fsm_registers_checked`).
    pub fn is_checked(&self) -> bool {
        self.kind == RegisterKind::Fsm
    }
}

/// Scan `btor2` for FSM-like state registers and check, from the reset state, that no
/// illegal encoding is reachable for each. Returns one [`FsmFinding`] per checked
/// register (registers without a non-trivial legal set are skipped). See the module
/// docs.
pub fn fsm_encoding_scan(btor2: &str, max_width: u32) -> Result<Vec<FsmFinding>, String> {
    let file = parser::parse(btor2).map_err(|e| format!("parsing BTOR2: {}", e.message))?;
    let symbols = parser::collect_symbols(&file);

    // Establish a legal start: seed `init` at the post-reset state and pin the resets
    // inactive (no-op for a design that already carries `init` lines / has no reset).
    let resets = detect_resets(&file, &symbols);
    let mut seeded = inject_reset_init(btor2, &resets)
        .map_err(|e| format!("seeding reset init: {}", e.message))?;
    let (pinned, _) = pin_inputs_to_constants(&seeded, &resets);
    seeded = pinned;

    // The FSM-like state registers, each under its strict-preferred name (the cell's
    // own symbol or a value-identical alias; the loose cone name only when nothing
    // exact names it — mununu#633). Deterministic order by cell nid.
    let names = parser::canonical_register_names(&file);
    let mut registers: Vec<(Nid, Nid, &String)> = names
        .iter()
        .filter_map(|(&nid, name)| match file.lookup(nid).map(|l| &l.node) {
            Some(Node::State { sort, .. }) => Some((nid, *sort, name)),
            _ => None,
        })
        .collect();
    registers.sort_by_key(|(nid, _, _)| *nid);

    let mut out = Vec::new();
    for (cell_nid, sort, name) in registers {
        let Some(width) = parser::bv_width(&file, sort) else {
            continue;
        };
        if width == 0 || width > max_width {
            continue;
        }

        // A counter's values are a range, not an enumeration: set it aside, unchecked.
        if is_counter(&file, cell_nid) {
            out.push(FsmFinding {
                register: name.clone(),
                kind: RegisterKind::Counter,
                legal_encodings: Vec::new(),
                verdict: PropertyVerdict::Skipped,
            });
            continue;
        }

        // Legal encodings = the constants the register is assigned / compared against +
        // its reset value. Skip a register with no non-trivial enum: fewer than two
        // legal values, or a legal set already covering every value of its width.
        let legal = legal_encodings(&file, cell_nid, &symbols);
        if legal.len() < 2 {
            continue;
        }
        let total: u128 = 1u128 << width;
        if legal.len() as u128 >= total {
            continue;
        }
        let legal_vec: Vec<u64> = legal.into_iter().collect();

        // bad = (reg ∉ legal); reachable from the (reset) start ⇒ an illegal encoding
        // is reachable (a bug). Decided by the word-level safety portfolio.
        let monitored = emit_ag_state_in_set_monitor_by_nid(&seeded, cell_nid, sort, &legal_vec)
            .map_err(|e| {
                format!(
                    "building the illegal-encoding monitor for `{name}`: {}",
                    e.message
                )
            })?;
        let mfile = parser::parse(&monitored)
            .map_err(|e| format!("parsing the monitored BTOR2 for `{name}`: {}", e.message))?;
        let outcome = decide_reach_portfolio(&mfile);

        out.push(FsmFinding {
            register: name.clone(),
            kind: RegisterKind::Fsm,
            legal_encodings: legal_vec,
            verdict: PropertyVerdict::from(outcome.verdict),
        });
    }
    Ok(out)
}

/// mununu#633 — does the register's own next-state logic step it by ±1? Walks the
/// value positions of its `next` cone (`ite` branches, `uext`/`sext`/`slice`
/// width-adjusts) and answers yes at an `inc`/`dec` of the cell, or an `add`/`sub` of
/// the cell and the constant `1` or `-1` (`2^w − 1` at the op's width — how a
/// decrement is spelled). The cell is recognised through its value aliases and through
/// any width extension (an `int`-context `cnt_q + 1` is a wide `add` over `uext
/// cnt_q`, sliced back). Any other arithmetic — `st + 3`, a multiply — is NOT a
/// counter step: that computed value is the illegal encoding the scan hunts.
///
/// The walk carries the BIT RANGE of the value that feeds the register, because yosys
/// packs every `always_comb` output into ONE `concat` and `slice`s each register's bits
/// back out (`next cnt = ite(rst, slice(concat(…, cnt_d, …), 4, 3), 0)` on the #633
/// fixture): a `slice` narrows the range, a `concat` descends into the operand that
/// holds it. A `concat` reached as a whole value is a computed word, not a counter.
fn is_counter(file: &Btor2File, state_nid: Nid) -> bool {
    let Some(next_val) = file.lines.iter().find_map(|l| match &l.node {
        Node::Next { state, value, .. } if *state == state_nid => Some(*value),
        _ => None,
    }) else {
        return false;
    };
    let Some(width) = node_width(file, state_nid).filter(|&w| w > 0) else {
        return false;
    };
    let aliases = value_alias_nids(file, state_nid);
    let mut visited = HashSet::new();
    steps_by_one(file, next_val.nid(), 0, width - 1, &aliases, &mut visited)
}

/// The bit width of the value `nid` carries, for the node kinds a next-state cone holds.
fn node_width(file: &Btor2File, nid: Nid) -> Option<u32> {
    match &file.lookup(nid)?.node {
        Node::Op { sort, .. }
        | Node::Const { sort, .. }
        | Node::State { sort, .. }
        | Node::Input { sort, .. } => parser::bv_width(file, *sort),
        _ => None,
    }
}

/// Is bit range `lo..=hi` of `nid`'s value a ±1 step of the register (see [`is_counter`])?
fn steps_by_one(
    file: &Btor2File,
    nid: Nid,
    lo: u32,
    hi: u32,
    aliases: &HashSet<Nid>,
    visited: &mut HashSet<(Nid, u32, u32)>,
) -> bool {
    if lo > hi || !visited.insert((nid, lo, hi)) {
        return false;
    }
    let Some(line) = file.lookup(nid) else {
        return false;
    };
    let Node::Op { op, args, sort, .. } = &line.node else {
        return false;
    };
    let reads_cell = |n: Nid| reaches_cell_through_width_adjust(file, n, aliases);
    match op {
        Op::Ite if args.len() == 3 => {
            steps_by_one(file, args[1].nid(), lo, hi, aliases, visited)
                || steps_by_one(file, args[2].nid(), lo, hi, aliases, visited)
        }
        // The inner value occupies bits `0..w_in`; above it are extension bits.
        Op::Uext | Op::Sext if args.len() == 1 => {
            let w_in = node_width(file, args[0].nid()).unwrap_or(0);
            w_in > 0
                && lo < w_in
                && steps_by_one(file, args[0].nid(), lo, hi.min(w_in - 1), aliases, visited)
        }
        // `slice sig upper lower`: our range sits at `lower + lo ..= lower + hi` of `sig`.
        Op::Slice if args.len() == 1 && line.immediates.len() == 2 => {
            let (upper, lower) = (line.immediates[0], line.immediates[1]);
            steps_by_one(
                file,
                args[0].nid(),
                lower + lo,
                (lower + hi).min(upper),
                aliases,
                visited,
            )
        }
        // `concat high low`: `low` holds bits `0..w_low`, `high` the rest.
        Op::Concat if args.len() == 2 => {
            let w_low = node_width(file, args[1].nid()).unwrap_or(0);
            w_low > 0
                && ((lo < w_low
                    && steps_by_one(file, args[1].nid(), lo, hi.min(w_low - 1), aliases, visited))
                    || (hi >= w_low
                        && steps_by_one(
                            file,
                            args[0].nid(),
                            lo.saturating_sub(w_low),
                            hi - w_low,
                            aliases,
                            visited,
                        )))
        }
        Op::Inc | Op::Dec if args.len() == 1 => reads_cell(args[0].nid()),
        Op::Add | Op::Sub if args.len() == 2 => {
            let width = parser::bv_width(file, *sort).unwrap_or(0);
            let minus_one = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width).wrapping_sub(1)
            };
            let is_unit =
                |n: Nid| resolve_btor2_constant(file, n).is_some_and(|c| c == 1 || c == minus_one);
            let (a, b) = (args[0].nid(), args[1].nid());
            (reads_cell(a) && is_unit(b)) || (*op == Op::Add && reads_cell(b) && is_unit(a))
        }
        _ => false,
    }
}

/// `nid` is the register's value (one of its aliases), possibly widened / narrowed by
/// `uext` / `sext` / `slice` on the way into an arithmetic op.
fn reaches_cell_through_width_adjust(file: &Btor2File, nid: Nid, aliases: &HashSet<Nid>) -> bool {
    let mut cur = nid;
    for _ in 0..16 {
        if aliases.contains(&cur) {
            return true;
        }
        match file.lookup(cur).map(|l| &l.node) {
            Some(Node::Op {
                op: Op::Uext | Op::Sext | Op::Slice,
                args,
                ..
            }) if !args.is_empty() => cur = args[0].nid(),
            _ => return false,
        }
    }
    false
}

/// The legal encodings of a state register — an over-approximation, so that only a
/// genuinely *computed* out-of-enum value (arithmetic / concat / decode) is ever
/// flagged illegal (biasing to zero false positives). It is the union of:
///
/// - the register's reset/init value;
/// - constants it is **compared** against (`eq` over a value-alias of the cell) — the
///   enum values the case/if logic recognizes;
/// - constants it is **assigned** (constant `ite`-branch leaves in its next-state cone)
///   — the enum values transitions write, including states a `case` folds into
///   `default` and so never compares against.
fn legal_encodings(
    file: &Btor2File,
    state_nid: Nid,
    symbols: &HashMap<Nid, String>,
) -> BTreeSet<u64> {
    let aliases = value_alias_nids(file, state_nid);
    let cell_sort = match file.lookup(state_nid).map(|l| &l.node) {
        Some(Node::State { sort, .. }) => *sort,
        _ => return BTreeSet::new(),
    };
    let mut legal = BTreeSet::new();

    if let Some(v) = derive_idle(file, state_nid, symbols) {
        legal.insert(v);
    }
    // Compared constants (eq over a value-alias of the cell).
    for line in &file.lines {
        let Node::Op {
            op: Op::Eq, args, ..
        } = &line.node
        else {
            continue;
        };
        if args.len() != 2 {
            continue;
        }
        let (a, b) = (args[0].nid(), args[1].nid());
        let konst = if aliases.contains(&a) {
            resolve_btor2_constant(file, b)
        } else if aliases.contains(&b) {
            resolve_btor2_constant(file, a)
        } else {
            None
        };
        if let Some(c) = konst {
            legal.insert(c);
        }
    }
    // Assigned constants (constant `ite`-branch leaves in the next-state cone).
    if let Some(next_val) = file.lines.iter().find_map(|l| match &l.node {
        Node::Next { state, value, .. } if *state == state_nid => Some(*value),
        _ => None,
    }) {
        collect_assigned_consts(
            file,
            next_val.nid(),
            cell_sort,
            &mut legal,
            &mut HashSet::new(),
        );
    }
    legal
}

/// From a **value position** in the register's next-state expression, insert into
/// `legal` every constant of `cell_sort` assigned to the register. Recurses only
/// through value-carrying structure — `ite` branches (not the boolean condition) and
/// `uext`/`sext` renames — and treats any *computed* op (arithmetic, concat, decode) as
/// opaque: its result is the potential illegal value, and its operands are inputs, not
/// assigned enum constants. This keeps a stray addend/comparison constant out of the
/// legal set.
fn collect_assigned_consts(
    file: &Btor2File,
    nid: Nid,
    cell_sort: Nid,
    legal: &mut BTreeSet<u64>,
    visited: &mut HashSet<Nid>,
) {
    if !visited.insert(nid) {
        return;
    }
    let Some(line) = file.lookup(nid) else {
        return;
    };
    match &line.node {
        // A constant of the register's sort reached as a value is a legal encoding.
        Node::Const { sort, .. } if *sort == cell_sort => {
            if let Some(v) = resolve_btor2_constant(file, nid) {
                legal.insert(v);
            }
        }
        // `ite(cond, then, else)`: the branches are assigned values; the condition is a
        // boolean, not a value — skip it.
        Node::Op {
            op: Op::Ite, args, ..
        } if args.len() == 3 => {
            collect_assigned_consts(file, args[1].nid(), cell_sort, legal, visited);
            collect_assigned_consts(file, args[2].nid(), cell_sort, legal, visited);
        }
        // `uext`/`sext`: a pure width-adjust rename — the value passes through.
        Node::Op {
            op: Op::Uext | Op::Sext,
            args,
            ..
        } if !args.is_empty() => {
            collect_assigned_consts(file, args[0].nid(), cell_sort, legal, visited);
        }
        // The register itself, or a computed op: opaque (its result is not a fresh enum
        // constant, and may be the illegal value we are hunting).
        _ => {}
    }
}

/// The set of NIDs value-identical to `state_nid` — the cell itself plus every
/// `uext`/`sext`-0 rename and async-reset-mux passthrough of it (how yosys surfaces the
/// register's current value, e.g. `state_q`). The design's `eq` comparisons target
/// these nids, not the raw cell.
fn value_alias_nids(file: &Btor2File, state_nid: Nid) -> HashSet<Nid> {
    let mut set: HashSet<Nid> = HashSet::from([state_nid]);
    let mut changed = true;
    while changed {
        changed = false;
        for line in &file.lines {
            if set.contains(&line.nid) {
                continue;
            }
            let is_alias = match &line.node {
                // `uext`/`sext` by 0 = a pure rename (same value).
                Node::Op {
                    op: Op::Uext | Op::Sext,
                    args,
                    ..
                } if args.len() == 1
                    && line.immediates.first() == Some(&0)
                    && set.contains(&args[0].nid()) =>
                {
                    true
                }
                // Async-reset mux `ite(rst, then, else)`: one branch passes the state
                // through, the other is the reset constant.
                Node::Op {
                    op: Op::Ite, args, ..
                } if args.len() == 3 => {
                    let (then_nid, else_nid) = (args[1].nid(), args[2].nid());
                    (set.contains(&then_nid) && resolve_btor2_constant(file, else_nid).is_some())
                        || (set.contains(&else_nid)
                            && resolve_btor2_constant(file, then_nid).is_some())
                }
                _ => false,
            };
            if is_alias {
                set.insert(line.nid);
                changed = true;
            }
        }
    }
    set
}

/// Detect the design's reset inputs as `(name, inactive_value)` from each state cell's
/// reset mux — the set [`inject_reset_init`] / [`pin_inputs_to_constants`] consume.
///
/// `pub(crate)` so `verify_auto` can reuse it to structurally auto-pin the reset on a
/// plain-RTL `@mununu_guarantee` design that carries no SVA `disable iff` (otherwise the
/// free reset input leaves a reset-edge from every state to the reset state, which
/// soundly-but-spuriously VIOLATES box/`AF`/box-universal properties).
pub(crate) fn detect_resets(
    file: &Btor2File,
    symbols: &HashMap<Nid, String>,
) -> Vec<(String, u64)> {
    let mut resets: BTreeSet<(String, u64)> = BTreeSet::new();
    for line in &file.lines {
        if !matches!(&line.node, Node::State { .. }) {
            continue;
        }
        if let Some(mux) = analyze_reset_mux(file, line.nid, symbols) {
            resets.insert((mux.reset_name, mux.inactive_value));
        }
    }
    resets.into_iter().collect()
}

/// The reset mux of a state cell: its `next` value is a top-level
/// `ite(reset_input, normal_logic, RESET_CONST)` (how yosys `async2sync` lowers an RTL
/// reset). Returns the reset input name, its inactive level, and the reset constant.
struct ResetMux {
    reset_name: String,
    inactive_value: u64,
    reset_value: u64,
}

fn analyze_reset_mux(
    file: &Btor2File,
    state_nid: Nid,
    symbols: &HashMap<Nid, String>,
) -> Option<ResetMux> {
    let next_val = file.lines.iter().find_map(|l| match &l.node {
        Node::Next { state, value, .. } if *state == state_nid => Some(*value),
        _ => None,
    })?;
    let Node::Op {
        op: Op::Ite, args, ..
    } = &file.lookup(next_val.nid())?.node
    else {
        return None;
    };
    let [cond, then_op, else_op] = args[..] else {
        return None;
    };
    let reset_name = symbols
        .get(&cond.nid())
        .filter(|s| is_reset_name(s))?
        .clone();

    // The reset value is the constant branch; polarity follows which branch it is.
    // `ite(cond, then, else)`: reset in the `else` branch ⇒ applied when cond is 0
    // (active-low, inactive = 1); reset in `then` ⇒ applied when cond is 1
    // (active-high, inactive = 0).
    match (
        resolve_btor2_constant(file, then_op.nid()),
        resolve_btor2_constant(file, else_op.nid()),
    ) {
        (None, Some(reset_value)) => Some(ResetMux {
            reset_name,
            inactive_value: 1,
            reset_value,
        }),
        (Some(reset_value), None) => Some(ResetMux {
            reset_name,
            inactive_value: 0,
            reset_value,
        }),
        _ => None, // both-const / neither-const is ambiguous
    }
}

/// Derive a state register's reset/init value: its BTOR2 `init` value if present, else
/// the constant branch of its reset mux (init-less yosys lifts). `None` if neither.
fn derive_idle(file: &Btor2File, state_nid: Nid, symbols: &HashMap<Nid, String>) -> Option<u64> {
    let init = file.lines.iter().find_map(|l| match &l.node {
        Node::Init { state, value, .. } if *state == state_nid => Some(*value),
        _ => None,
    });
    if let Some(v) = init.and_then(|op| resolve_btor2_constant(file, op.nid())) {
        return Some(v);
    }
    analyze_reset_mux(file, state_nid, symbols).map(|m| m.reset_value)
}

/// Heuristic: does this signal name a reset input? Matches `rst` / `reset` roots with
/// the usual polarity suffixes (`rst_ni`, `rst_n`, `reset`, `arst_n`, `resetn`, …).
fn is_reset_name(sym: &str) -> bool {
    let s = sym.to_ascii_lowercase();
    s.contains("rst") || s.contains("reset")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3-state responder FSM (2-bit `st`, values 0..3). Case arms compare st against
    // 0/1/2 (the recognized enum); transitions cycle 0 →go 1 → 2 → 0. The 4th encoding
    // 3 is never reached ⇒ AG (st ∈ {0,1,2}) HOLDS ⇒ no finding.
    const LEGAL_FSM: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
10 eq 2 3 4
11 eq 2 3 7
12 eq 2 3 8
14 ite 1 6 7 4
15 ite 1 12 4 4
16 ite 1 11 8 15
17 ite 1 10 14 16
18 next 1 3 17
";

    // A 3-bit sparse FSM (enum Idle=1, Busy=2, Done=4) with a COMPUTED illegal-encoding
    // bug: from Busy, `go` assigns `st + 3` (= 2 + 3 = 5 = 3'b101), a value outside the
    // enum. 5 is reachable Idle →go Busy →go 5 ⇒ AG (st ∈ {1,2,4}) VIOLATED. (A *computed*
    // out-of-enum value is what the scan detects; a constant-assigned wrong value is
    // indistinguishable from a legal encoding at the BTOR2 level — see legal_encodings.)
    const ILLEGAL_FSM: &str = "\
1 sort bitvec 3
2 sort bitvec 1
3 state 1 st
4 constd 1 1
5 init 1 3 4
6 input 2 go
7 constd 1 2
8 constd 1 4
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 eq 2 3 8
13 add 1 3 9
14 ite 1 6 13 8
15 ite 1 6 7 4
16 ite 1 12 4 4
17 ite 1 11 14 16
18 ite 1 10 15 17
19 next 1 3 18
";

    #[test]
    fn legal_fsm_finds_no_illegal_encoding() {
        let findings = fsm_encoding_scan(LEGAL_FSM, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        let st = findings
            .iter()
            .find(|f| f.register == "st")
            .expect("st scanned");
        // Legal set = {0,1,2} (compared + assigned + init 0); 3 is illegal but unreachable.
        assert_eq!(st.legal_encodings, vec![0, 1, 2]);
        assert_eq!(
            st.verdict,
            PropertyVerdict::Holds,
            "st stays within its enum"
        );
        assert!(!st.is_finding());
    }

    #[test]
    fn illegal_fsm_reaches_the_illegal_encoding() {
        let findings = fsm_encoding_scan(ILLEGAL_FSM, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        let st = findings
            .iter()
            .find(|f| f.register == "st")
            .expect("st scanned");
        // Legal = {1,2,4}: Idle/Busy/Done. The computed `st + 3` (= 5) is not among them.
        assert_eq!(st.legal_encodings, vec![1, 2, 4]);
        assert_eq!(
            st.verdict,
            PropertyVerdict::Violated,
            "the bug computes st + 3 = 5, an illegal encoding"
        );
        assert!(st.is_finding(), "a reachable illegal encoding is a finding");
        assert_eq!(
            st.kind,
            RegisterKind::Fsm,
            "arithmetic by a step other than ±1 is a computed value, not a counter"
        );
    }

    // mununu#633 — a 2-bit counter `cnt` (reset 0) that increments on `go` and is
    // compared against its limit 3: the scan's legal set would be {0, 3}, and 1 and 2
    // are reachable, so it used to be reported VIOLATED. Its cell carries no symbol;
    // the next-value op `cnt_d` (file-first, the loose cone name) and the `uext 0`
    // alias `cnt_q` (value-identical) both name it.
    const COUNTER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 3
9 eq 2 3 8
10 add 1 3 7
11 ite 1 6 10 3 cnt_d
12 next 1 3 11
13 uext 1 3 0 cnt_q
";

    #[test]
    fn x633_a_register_stepped_by_one_is_a_counter_set_aside_under_its_alias_name() {
        let findings = fsm_encoding_scan(COUNTER, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        assert_eq!(findings.len(), 1, "{findings:?}");
        let cnt = &findings[0];
        assert_eq!(
            cnt.register, "cnt_q",
            "named by its value-identical alias, not `cnt_d`"
        );
        assert_eq!(cnt.kind, RegisterKind::Counter);
        assert_eq!(cnt.verdict, PropertyVerdict::Skipped);
        assert!(cnt.legal_encodings.is_empty());
        assert!(!cnt.is_finding() && !cnt.is_checked());
    }

    #[test]
    fn x633_a_decrement_spelled_as_add_minus_one_or_sub_one_is_a_counter_too() {
        // `cnt - 1` lowered as `add cnt 3` (2^2 − 1), and as `sub cnt 1`; an `int`-context
        // `cnt + 1` lowered as a 32-bit add over `uext 30 cnt`, sliced back.
        for (label, tail) in [
            (
                "add -1",
                "10 add 1 3 8\n11 ite 1 6 10 3\n12 next 1 3 11\n13 uext 1 3 0 cnt_q\n",
            ),
            (
                "sub 1",
                "10 sub 1 3 7\n11 ite 1 6 10 3\n12 next 1 3 11\n13 uext 1 3 0 cnt_q\n",
            ),
            (
                "wide add",
                "10 sort bitvec 32\n11 uext 10 3 30\n12 one 10\n13 add 10 11 12\n\
                 14 slice 1 13 1 0\n15 ite 1 6 14 3\n16 next 1 3 15\n17 uext 1 3 0 cnt_q\n",
            ),
        ] {
            let src = format!(
                "1 sort bitvec 2\n2 sort bitvec 1\n3 state 1\n4 zero 1\n5 init 1 3 4\n\
                 6 input 2 go\n7 one 1\n8 constd 1 3\n9 eq 2 3 8\n{tail}"
            );
            let findings = fsm_encoding_scan(&src, DEFAULT_FSM_MAX_WIDTH).expect("scan");
            assert_eq!(findings.len(), 1, "{label}: {findings:?}");
            assert_eq!(findings[0].kind, RegisterKind::Counter, "{label}");
            assert_eq!(findings[0].register, "cnt_q", "{label}");
        }
    }

    // mununu#633 — the shape yosys actually emits for the fixture: every `always_comb`
    // output packed into ONE `concat` (`[cnt_d : go]`, 3 bits) and each register's bits
    // `slice`d back out, under the async-reset mux. `cnt_d = go ? cnt + 1 : cnt` sits in
    // bits 2..1 of the pack; the register's `next` reads `slice(pack, 2, 1)`.
    const PACKED_COUNTER: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 rst_n
4 input 1 go
5 state 2
6 const 2 00
7 ite 2 3 5 6
8 const 2 01
9 add 2 7 8
10 ite 2 4 9 7
11 sort bitvec 3
12 concat 11 10 4
13 slice 2 12 2 1
14 uext 2 13 0 cnt_d
15 ite 2 3 13 6
16 next 2 5 15
17 uext 2 7 0 cnt_q
18 const 2 11
19 eq 1 7 18
";

    #[test]
    fn x633_a_counter_packed_through_yosys_concat_and_slice_is_still_a_counter() {
        let findings = fsm_encoding_scan(PACKED_COUNTER, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].register, "cnt_q");
        assert_eq!(
            findings[0].kind,
            RegisterKind::Counter,
            "the ±1 step is found through slice(concat(…)): {findings:?}"
        );
        // The slice that does NOT cover the counter's bits (bit 0 = `go`) is not a step.
        let other = PACKED_COUNTER.replace("13 slice 2 12 2 1\n", "13 slice 1 12 0 0\n");
        let file = parser::parse(&other).expect("parse");
        assert!(
            !is_counter(&file, 5),
            "bit 0 of the pack is `go`, not `cnt + 1`"
        );
    }

    #[test]
    fn x633_the_state_register_is_reported_under_its_alias_not_the_next_value_name() {
        // LEGAL_FSM with the cell's symbol moved off the `state` line: the next-value op
        // (17) carries `st_d` and comes first — the loose cone trace names the cell by
        // it — and a `uext 0` alias (19) carries `st_q`. The scan must say `st_q`, with
        // the same legal set and verdict as the directly-named fixture.
        let src = LEGAL_FSM
            .replace("3 state 1 st\n", "3 state 1\n")
            .replace("17 ite 1 10 14 16\n", "17 ite 1 10 14 16 st_d\n")
            + "19 uext 1 3 0 st_q\n";
        let loose = {
            let file = parser::parse(&src).expect("parse");
            parser::collect_symbols(&file).get(&3).cloned()
        };
        assert_eq!(
            loose.as_deref(),
            Some("st_d"),
            "the loose trap the fix bypasses"
        );
        let findings = fsm_encoding_scan(&src, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        assert!(
            findings.iter().all(|f| f.register != "st_d"),
            "a next-value wire is not a register: {findings:?}"
        );
        let st = findings
            .iter()
            .find(|f| f.register == "st_q")
            .unwrap_or_else(|| panic!("st_q scanned: {findings:?}"));
        assert_eq!(st.kind, RegisterKind::Fsm);
        assert_eq!(st.legal_encodings, vec![0, 1, 2]);
        assert_eq!(st.verdict, PropertyVerdict::Holds);
    }

    #[test]
    fn full_coverage_register_is_skipped() {
        // A 1-bit register that legally takes both 0 and 1 (|L| = 2^1): no illegal value.
        const FULL: &str = "1 sort bitvec 1\n2 state 1 flag\n3 zero 1\n4 one 1\n5 init 1 2 3\n6 eq 1 2 3\n7 eq 1 2 4\n8 next 1 2 4\n";
        let findings = fsm_encoding_scan(FULL, DEFAULT_FSM_MAX_WIDTH).expect("scan");
        assert!(
            findings.iter().all(|f| f.register != "flag"),
            "a fully-covered 1-bit register has no illegal encoding to check"
        );
    }

    fn detect(src: &str) -> Vec<(String, u64)> {
        let file = crate::adapter::btor2::parser::parse(src).expect("parse");
        let syms = crate::adapter::btor2::parser::collect_symbols(&file);
        detect_resets(&file, &syms)
    }

    #[test]
    fn detect_resets_active_high_pins_to_zero() {
        // next(st) = ite(rst, 0 /*reset*/, st+1 /*normal*/): const in THEN ⇒ active-high, inactive = 0.
        const HI: &str = "\
1 sort bitvec 1
2 input 1 rst
3 sort bitvec 2
4 state 3 st
5 zero 3
6 one 3
7 add 3 4 6
8 ite 3 2 5 7
9 next 3 4 8
";
        assert_eq!(detect(HI), vec![("rst".to_string(), 0)]);
    }

    #[test]
    fn detect_resets_active_low_pins_to_one() {
        // next(st) = ite(rst_n, st+1 /*normal*/, 0 /*reset*/): const in ELSE ⇒ active-low, inactive = 1.
        const LO: &str = "\
1 sort bitvec 1
2 input 1 rst_n
3 sort bitvec 2
4 state 3 st
5 zero 3
6 one 3
7 add 3 4 6
8 ite 3 2 7 5
9 next 3 4 8
";
        assert_eq!(detect(LO), vec![("rst_n".to_string(), 1)]);
    }

    #[test]
    fn detect_resets_none_without_reset_mux() {
        // A free-running counter (no reset mux) yields no reset to pin.
        const NONE: &str = "1 sort bitvec 2\n2 state 1 st\n3 one 1\n4 add 1 2 3\n5 next 1 2 4\n";
        assert!(detect(NONE).is_empty());
    }
}
