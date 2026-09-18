//! SV-direct property verbs — lift a SystemVerilog module to BTOR2 (sv2v + Yosys)
//! and decide a property in **one** call.
//!
//! An external tool (e.g. an agent writing RTL) can hand raw SV plus a target atom
//! and get a verdict, with no manual `sv emit-btor2-per-module` → `btor2 verify-*`
//! round-trip. Each function is a thin bridge: [`sv_to_btor2`] then the matching
//! BTOR2 verb core ([`decide_reach_portfolio_parallel`],
//! [`response_liveness_rescue_atoms`], [`verify_recoverability`]). The SV → BTOR2
//! lift requires sv2v + Yosys on the host (see [`docs/verifying-rtl.md`]); a missing
//! tool returns a structured error.
//!
//! Atoms are validated **before** the (toolchain-gated) lift, so a malformed
//! request fails fast and identically to the BTOR2-direct verbs.
//!
//! **Reset**: these verbs lift the design with the reset as a *free input* (no
//! reset-gating). Sound, and the honest default — recoverability in particular is
//! reset-dependent (see `docs/design/recoverability-vs-sva.md` §3.2). Reset-gating
//! (as `sv verify-auto` offers) is a future option.

use crate::adapter::btor2::kmts_lift::PredicateSpec;
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::parse_predicate_expr;
use crate::adapter::liveness_rescue::{
    Atom, LivenessVerdict, parse_response_atom, parse_response_pairs,
    response_liveness_rescue_atoms, response_liveness_rescue_conjunction,
};
use crate::adapter::reach_portfolio::{ReachOutcome, decide_reach_portfolio_parallel};
use crate::adapter::recoverability::verify_recoverability_with_predicates;
use crate::adapter::yosys::{SvFrontend, YosysOptions, sv_to_btor2};
use crate::verdict::PropertyVerdict;

/// The SV → BTOR2 lift inputs shared by every SV-direct verb.
pub struct SvLift {
    /// Primary SystemVerilog source.
    pub source: String,
    /// Additional SV sources (packages / includes) as `(name, content)`.
    pub additional_sources: Vec<(String, String)>,
    /// Top module (Yosys auto-detects when `None`).
    pub top: Option<String>,
    /// Run sv2v before Yosys (modern SV module-header imports, etc.).
    pub use_sv2v: bool,
    /// Extra on-disk include-search directories (`-I<dir>`), so
    /// `` `include "frag.vh" `` resolves against the original source tree
    /// without the fragment being read as a standalone compilation unit.
    /// Feeds [`YosysOptions::extra_include_dirs`]; empty by default.
    pub include_dirs: Vec<std::path::PathBuf>,
    /// RTL front-end selection. `Slang` forces the yosys-slang plugin, which
    /// lifts modern-SV constructs `read_verilog`/sv2v reject. Feeds
    /// [`YosysOptions::frontend`]; `Auto` by default.
    pub frontend: SvFrontend,
}

impl SvLift {
    /// Lift to a single flattened BTOR2 string (sv2v optional + Yosys).
    fn lift(&self) -> Result<String, String> {
        let yopts = YosysOptions {
            top: self.top.clone(),
            additional_sources: self.additional_sources.clone(),
            use_sv2v: self.use_sv2v,
            extra_include_dirs: self.include_dirs.clone(),
            frontend: self.frontend,
            ..Default::default()
        };
        sv_to_btor2(&self.source, &yopts)
            .map_err(|e| format!("SV → BTOR2 (sv2v + Yosys): {}", e.message))
    }
}

/// `sv verify` — lift SV and decide `bad`-reachability of its assertions with the
/// multi-engine safety portfolio.
pub fn sv_verify_safety(lift: &SvLift) -> Result<ReachOutcome, String> {
    let btor2 = lift.lift()?;
    let file =
        parser::parse(&btor2).map_err(|e| format!("parsing the lifted BTOR2: {}", e.message))?;
    Ok(decide_reach_portfolio_parallel(&file))
}

/// `sv verify-liveness` — lift SV and decide `AG(request → AF grant)` via the l2s
/// reduction + the portfolio. The atoms are parsed first (a malformed atom errors
/// before the lift).
pub fn sv_verify_liveness(
    lift: &SvLift,
    request: &str,
    grant: &str,
) -> Result<(LivenessVerdict, ReachOutcome), String> {
    let ante: Atom = parse_response_atom(request)?;
    let cons: Atom = parse_response_atom(grant)?;
    let btor2 = lift.lift()?;
    response_liveness_rescue_atoms(&btor2, &ante, &cons, false).ok_or_else(|| {
        "could not build the liveness monitor — an atom likely binds no signal in the design"
            .to_string()
    })
}

/// `sv verify-liveness-all` — lift SV and decide the **conjunction** of
/// response-liveness properties `⋀ᵢ AG(aᵢ → AF bᵢ)` via the l2s reduction + the
/// portfolio (one lift, one `bad`-reachability query per response). Each `responses`
/// entry is a `"ANTE => CONS"` pair; every pair is parsed first, so a malformed
/// response errors before the (toolchain-gated) lift — matching [`sv_verify_liveness`].
///
/// Returns the combined verdict + the per-response [`ReachOutcome`] (same order as
/// `responses`), via [`response_liveness_rescue_conjunction`].
pub fn sv_verify_liveness_all(
    lift: &SvLift,
    responses: &[String],
) -> Result<(LivenessVerdict, Vec<ReachOutcome>), String> {
    let pairs = parse_response_pairs(responses)?;
    let btor2 = lift.lift()?;
    response_liveness_rescue_conjunction(&btor2, &pairs, false).ok_or_else(|| {
        "could not build a liveness monitor — an atom likely binds no signal in the design, \
         or no responses were given"
            .to_string()
    })
}

/// `sv verify-recoverability` — lift SV and decide `AG EF target`. The target atom is
/// validated before the lift.
pub fn sv_verify_recoverability(lift: &SvLift, target: &str) -> Result<PropertyVerdict, String> {
    sv_verify_recoverability_with_predicates(lift, target, &[])
}

/// [`sv_verify_recoverability`] with optional extra abstraction predicates for the
/// cube-path escalation (P2 Slice 1). The target atom is validated before the
/// (toolchain-gated) lift; the extras only refine the cube if the exact engine abstains.
pub fn sv_verify_recoverability_with_predicates(
    lift: &SvLift,
    target: &str,
    extra_predicates: &[PredicateSpec],
) -> Result<PropertyVerdict, String> {
    parse_predicate_expr(target).map_err(|e| {
        format!("recoverability target `{target}` is not a register-comparison atom: {e:?}")
    })?;
    let btor2 = lift.lift()?;
    verify_recoverability_with_predicates(&btor2, target, extra_predicates)
}

/// `sv verify-recoverability --refine` — lift SV and decide `AG EF target`, returning the canonical
/// verdict PLUS a structured [`VerdictRefinement`]: a `Vacuous` witness, an auto `config_partition`
/// over the detected reset, a `bot_diagnosis` hint, and — when `discover_assumptions` is set and the
/// property does not hold — discovered `holds_under` assumptions. The refinement is diagnostic-only — it
/// never changes the canonical verdict. Sidecar-free (operates on the same lift as the plain verb).
pub fn sv_verify_recoverability_refined(
    lift: &SvLift,
    target: &str,
    extra_predicates: &[PredicateSpec],
    config_specs: &[(String, Vec<u64>)],
    discover_assumptions: bool,
) -> Result<(PropertyVerdict, crate::verdict::VerdictRefinement), String> {
    parse_predicate_expr(target).map_err(|e| {
        format!("recoverability target `{target}` is not a register-comparison atom: {e:?}")
    })?;
    let btor2 = lift.lift()?;
    Ok(
        crate::adapter::recoverability::verify_recoverability_refined(
            &btor2,
            target,
            extra_predicates,
            config_specs,
            discover_assumptions,
        ),
    )
}

/// `sv check-fsm` — lift SV and auto-scan every FSM-like state register for a reachable
/// illegal encoding (no user input). The SV-direct peer of `btor2 check-fsm`.
pub fn sv_check_fsm(
    lift: &SvLift,
    max_width: u32,
) -> Result<Vec<crate::adapter::fsm_scan::FsmFinding>, String> {
    let btor2 = lift.lift()?;
    crate::adapter::fsm_scan::fsm_encoding_scan(&btor2, max_width)
}

/// Whether a lint finding names a state register (or its alias) or a
/// combinational output derived from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SvLintSignalKind {
    /// A state register — the root of the finding: the RTL front-end left some of
    /// its bits undriven (modelled as free inputs).
    Register,
    /// A combinational output that reads such a register — flagged downstream of a
    /// `Register` finding (a property over it would be refused for the same reason).
    Output,
}

impl SvLintSignalKind {
    /// The stable lowercase tag used across the CLI JSON / API / UI surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            SvLintSignalKind::Register => "register",
            SvLintSignalKind::Output => "output",
        }
    }
}

/// Which structural check produced a finding. Serialised as a stable
/// lowercase-kebab tag on the CLI JSON / API / UI surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SvLintRule {
    /// monono#partsel — the register's lift reaches an anonymous free input, so
    /// a state predicate over it is *refused* by the verifier.
    UndrivenPartialWrite,
    /// mununu#496 — a registered array read (`q <= mem[a_q];`) whose address
    /// register can change in the same cycle `q` is consumed, with no register
    /// recording which address the current `q` corresponds to.
    RegisteredArrayReadMovingAddress,
}

impl SvLintRule {
    /// The stable tag used across the CLI JSON / API / UI surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            SvLintRule::UndrivenPartialWrite => "undriven-partial-write",
            SvLintRule::RegisteredArrayReadMovingAddress => "registered-array-read-moving-address",
        }
    }
}

/// One `sv lint` finding — a named signal a structural check flagged. See
/// [`SvLintRule`] for what each check means and [`sv_lint_registers`] for the
/// entry point.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SvLintFinding {
    /// The offending signal's symbol (register name or the combinational output).
    pub signal: String,
    /// Whether `signal` is the register itself or a downstream output.
    pub kind: SvLintSignalKind,
    /// Which check fired. Defaults to the partial-write rule so an older
    /// consumer deserialising a finding without the field keeps working.
    #[serde(default = "SvLintFinding::default_rule")]
    pub rule: SvLintRule,
    /// Human-readable specifics — for the array-read rule, the address register
    /// and why it is unsafe. Empty for rules that need no elaboration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl SvLintFinding {
    fn default_rule() -> SvLintRule {
        SvLintRule::UndrivenPartialWrite
    }
}

/// `sv lint` — lift SV and report, at CI time (~lift cost, no bit-blast / no
/// model checking), every register whose partial-write lift the verifier can NOT
/// keep faithfully.
///
/// This is the read-only, cap-immune preflight for the monono#partsel soundness
/// refusal shipped in #464/#465: the yosys-slang lift of a plain-vector partial
/// register assignment (`q[hi:lo] <= d`) models `q`'s *unwritten* bits as free
/// `input`s (havoc) and aliases the register name to the `concat` mixing them, so
/// a state predicate over `q` reads those free inputs and would be unsound — the
/// verifier *refuses* (skips) such a property up front
/// ([`parser::signal_reaches_anonymous_input`]). `sv lint` surfaces exactly those
/// registers *before* the ~minutes-long formal gate runs, so a design whose lift
/// is unfaithful is caught in ~0.1 s. It changes **no verdict**: a faithful state
/// cell (read_verilog / sv2v) and a packed-2-D `q[idx] <= d` split (anonymous
/// *sub-registers*, no free inputs — the NID-indexed cone keeps them) are both
/// NOT flagged; the latter decides.
///
/// Findings are deterministic (sorted by name, deduplicated); a name that is both
/// a register alias and an output is reported once, as a `Register`.
pub fn sv_lint_registers(lift: &SvLift) -> Result<Vec<SvLintFinding>, String> {
    let btor2 = lift.lift()?;
    let mut findings = lint_undriven_partial_writes(&btor2)?;
    findings.extend(lint_registered_array_read_moving_address(&btor2)?);
    findings.sort_by(|a, b| (a.rule.as_str(), &a.signal).cmp(&(b.rule.as_str(), &b.signal)));
    Ok(findings)
}

/// mununu#506/#507 — the set of dotted prefixes that name a real INSTANCE scope,
/// evidenced by a `state` cell living under them.
///
/// `lint_undriven_partial_writes` skips Op symbols containing `.` because Yosys
/// mangles a function argument as `<function>.<arg>` (`ones8.v`, `ctrl_code.c`),
/// and those false-positived as partial-write aliases (mununu#475 item 1). But on
/// a FLATTENED integrator every hierarchical signal is also dotted
/// (`u_ld_bank.addr`), so the blanket filter suppressed the entire design — which
/// is why `sv lint` reported zero findings on an integrator while `verify-auto`
/// refused a property on the same file. They were not disagreeing; the lint was
/// not looking.
///
/// The discriminator is self-contained: **a function has no registers.** A module
/// instance almost always does, so a prefix that appears on a `state` symbol names
/// an instance and its Op aliases must NOT be skipped. Every `.`-boundary prefix
/// counts, so nested hierarchy (`u_a.u_b.sig`) resolves too.
///
/// Residual, and documented: an instance containing no state at all is still
/// skipped. Its Op aliases can only be `Output`-kind findings downstream of a
/// register root, and the root itself lives in a state-bearing scope — strictly
/// better than skipping every dotted symbol.
fn instance_scopes(
    file: &crate::adapter::btor2::ast::Btor2File,
) -> std::collections::HashSet<String> {
    use crate::adapter::btor2::ast::Node;
    let mut out = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::State {
            symbol: Some(s), ..
        } = &line.node
        {
            let mut idx = 0usize;
            while let Some(pos) = s[idx..].find('.') {
                let end = idx + pos;
                out.insert(s[..end].to_string());
                idx = end + 1;
            }
        }
    }
    out
}

/// The pure BTOR2 core of [`sv_lint_registers`] — parse the flattened design and
/// return every named register/output whose cone reaches an anonymous free input.
/// Split out so the regression suite can exercise it on a captured BTOR2 fixture
/// with no sv2v / Yosys on the host.
fn lint_undriven_partial_writes(btor2: &str) -> Result<Vec<SvLintFinding>, String> {
    use crate::adapter::btor2::ast::Node;
    use crate::adapter::btor2::parser::signal_reaches_anonymous_input;

    let file = parser::parse(btor2).map_err(|e| format!("BTOR2 parse: {e}"))?;

    // Candidate name universe = exactly the names `signal_reaches_anonymous_input`
    // can match on: State + Op(symbol) → a register (the partsel alias `a_q` is a
    // `uext`/`concat` Op carrying the register's name), Output(symbol) → a
    // combinational output. `Register` wins when a name appears as both.
    //
    // mununu#475 item 1 — SKIP Op-node symbols containing `.`. Yosys/slang
    // mangle **function-argument** names as `<function>.<arg>` (e.g.
    // `ctrl_code.c`, `ones8.v` in monono's `tmds_encoder.sv`). These emit as
    // Op nodes with a dotted symbol whose cone happens to reach an anonymous
    // input (the function-scope input placeholder), so the raw filter below
    // false-positives them as partial-write registers. Register aliases the
    // lint IS supposed to catch (`a_q` from `q[hi:lo] <= d`) never carry
    // dotted names, so the heuristic is targeted. State-node names may
    // contain dots (hierarchical `top.sub.reg_q`) and are not filtered here —
    // real registers should still be reported.
    let scopes = instance_scopes(&file);
    let mut kinds: std::collections::BTreeMap<String, SvLintSignalKind> =
        std::collections::BTreeMap::new();
    for line in &file.lines {
        let (name, kind) = match &line.node {
            Node::State {
                symbol: Some(s), ..
            } => (s, SvLintSignalKind::Register),
            Node::Op {
                symbol: Some(s), ..
            } => {
                // Skip a dotted symbol ONLY when no prefix of it names an instance
                // scope — i.e. it is a `<function>.<arg>` mangling (mununu#475) and
                // not flattened hierarchy (mununu#506). See `instance_scopes`.
                if s.contains('.') && !s.match_indices('.').any(|(i, _)| scopes.contains(&s[..i])) {
                    continue;
                }
                (s, SvLintSignalKind::Register)
            }
            Node::Output {
                symbol: Some(s), ..
            } => (s, SvLintSignalKind::Output),
            _ => continue,
        };
        kinds
            .entry(name.clone())
            .and_modify(|k| {
                if kind == SvLintSignalKind::Register {
                    *k = SvLintSignalKind::Register;
                }
            })
            .or_insert(kind);
    }

    // BTreeMap keeps the output sorted-by-name (deterministic).
    Ok(kinds
        .into_iter()
        .filter(|(name, _)| signal_reaches_anonymous_input(&file, name))
        .map(|(signal, kind)| SvLintFinding {
            signal,
            kind,
            rule: SvLintRule::UndrivenPartialWrite,
            detail: String::new(),
        })
        .collect())
}

/// mununu#506 — resolve `nid` back to the state register it names, through the
/// IDENTITY-PRESERVING wrappers Yosys leaves after `flatten` / `async2sync` /
/// `dffunmap`.
///
/// A register's identity rarely survives on its raw `state` line. `async2sync`
/// lifts an async reset into a mux, so the value everything downstream reads is
/// `ite(rst_n, state, RESET_VALUE)`, and the register's NAME lands on a `uext`
/// alias of that mux:
///
/// ```text
/// 10 state 8                 <- the register (unnamed)
/// 11 ite 8 4 10 9            <- rst_n ? state : 0     (the reset mux)
/// 12 uext 8 11 0 ld_q        <- the NAME is here
/// 17 read 5 16 11            <- and the read indexes the MUX, not the state
/// ```
///
/// Requiring a bare `state` therefore made the rule fire only on registers with
/// NO reset — close to none in real RTL, which is why it reported nothing on the
/// design that motivated it (mununu#506). Same class as the 2026-07-05
/// `next_funcs` alias-keying fix.
///
/// Walks ONLY through wrappers that preserve identity:
/// * `uext` / `sext` — width padding.
/// * `slice` — a part-select still names the same register (`.addr(ld_q[10:0])`).
/// * `ite` where one arm is a constant — the reset-mux shape; follow the other arm.
///
/// It deliberately does NOT walk arithmetic or multi-register logic, so
/// `mem[a_q + 1]` and `mem[a ^ b]` stay out of scope (documented as such).
fn resolve_to_state_register(
    file: &crate::adapter::btor2::ast::Btor2File,
    nid: i64,
) -> Option<i64> {
    use crate::adapter::btor2::ast::{Node, Op};
    let mut nid = nid;
    for _ in 0..64 {
        match node_of(file, nid)? {
            Node::State { .. } => return Some(nid),
            Node::Op {
                op: Op::Uext | Op::Sext | Op::Slice,
                args,
                ..
            } => {
                nid = args.first()?.nid();
            }
            Node::Op {
                op: Op::Ite, args, ..
            } => {
                // Reset-mux shape: exactly one arm is a constant.
                let (t, e) = (args.get(1)?.nid(), args.get(2)?.nid());
                let is_const = |n| matches!(node_of(file, n), Some(Node::Const { .. }));
                match (is_const(t), is_const(e)) {
                    (false, true) => nid = t,
                    (true, false) => nid = e,
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    None
}

/// mununu#506 Tier 1 — strip the wrappers that do not change an expression's VALUE,
/// for structural comparison of two address expressions.
///
/// Distinct from [`resolve_to_state_register`], which answers "*which register does
/// this name*" and therefore walks `slice` too. Here the question is "*is this the
/// same value*", so a `slice` is meaningful and is NOT stripped — only zero-width
/// extension and the `async2sync` reset mux are.
fn canonical_value_nid(file: &crate::adapter::btor2::ast::Btor2File, nid: i64) -> i64 {
    use crate::adapter::btor2::ast::{Node, Op};
    let mut nid = nid;
    for _ in 0..64 {
        let Some(line) = file.lookup(nid) else {
            return nid;
        };
        match &line.node {
            Node::Op {
                op: Op::Uext | Op::Sext,
                args,
                ..
            } if line.immediates.first().copied() == Some(0) => match args.first() {
                Some(a) => nid = a.nid(),
                None => return nid,
            },
            Node::Op {
                op: Op::Ite, args, ..
            } => {
                let (Some(t), Some(e)) = (args.get(1), args.get(2)) else {
                    return nid;
                };
                let is_const = |n| matches!(node_of(file, n), Some(Node::Const { .. }));
                match (is_const(t.nid()), is_const(e.nid())) {
                    (false, true) => nid = t.nid(),
                    (true, false) => nid = e.nid(),
                    _ => return nid,
                }
            }
            _ => return nid,
        }
    }
    nid
}

/// mununu#506 Tier 1 — the state + input leaves an expression depends on.
///
/// Generalises the address analysis from "*is this a bare register*" to "*what can
/// make this value change*", which is what the rule actually needs. Covers
/// `mem[base + 1]`, `mem[{tag, a_q}]`, `mem[a_q[hi:lo]]`, `mem[a_q ^ mask]` — the
/// shape class, not a list of cases. Mirrors `dep_graph::cone_leaf_nids`, but
/// rooted at a NID rather than at a symbol.
fn expr_leaves(
    file: &crate::adapter::btor2::ast::Btor2File,
    root: i64,
) -> (std::collections::HashSet<i64>, bool) {
    use crate::adapter::btor2::ast::Node;
    let mut states = std::collections::HashSet::new();
    let mut has_input = false;
    let mut seen = std::collections::HashSet::new();
    let mut work = vec![root];
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        match node_of(file, nid) {
            Some(Node::State { .. }) => {
                states.insert(nid);
            }
            Some(Node::Input { .. }) => has_input = true,
            Some(Node::Op { args, .. }) => {
                for a in args {
                    work.push(a.nid());
                }
            }
            Some(Node::Output { signal, .. }) => work.push(signal.nid()),
            _ => {}
        }
    }
    (states, has_input)
}

/// Tier 2 (mununu#506) — the SEMANTIC tracking check.
///
/// Tier 1 recognises a tracking register by STRUCTURAL equality of the address
/// expression. That is right whenever yosys `opt`'s CSE has merged the design's two
/// occurrences of `a_q + 1` into one node — usually, but not guaranteed. When it
/// misses, the rule reports a design that DOES record the correspondence: a **false
/// positive on correct RTL**, the direction that matters most here (strictly worse
/// than the under-firing the #506 fix removed).
///
/// So before emitting, ask the solver whether ANY register's next value is
/// *provably* equal to the address expression: `next(t) != addr` UNSAT ⇒ equal.
///
/// **Runs only on the reporting path.** A clean design never reaches it, so
/// `sv lint`'s "no model checking, ~0.1 s" profile is unchanged on the common path.
/// And it can only WITHDRAW a finding, never create one: an encode failure, an
/// unsupported operator, a width mismatch or a solver `Unknown` all leave the
/// structural verdict standing — the solver stays on the sound side of the decision.
fn smt_confirms_tracking(
    file: &crate::adapter::btor2::ast::Btor2File,
    addr: i64,
    data_state: i64,
) -> bool {
    use crate::adapter::btor2::ast::Node;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || -> bool {
        let Ok(view) = crate::adapter::btor2::kmts_lift::encode_design_for_lift(file) else {
            return false; // cannot encode ⇒ cannot withdraw
        };
        let Some(addr_bv) = view.curr_signal(addr) else {
            return false;
        };
        // Width from the BV itself: `width_of` only covers DECLARED signals, and an
        // address expression is usually a combinational node with no entry — using it
        // here silently skipped every candidate.
        let addr_w = addr_bv.get_size();
        for line in &file.lines {
            if !matches!(line.node, Node::State { .. }) || line.nid == data_state {
                continue;
            }
            // Compare the EXPRESSION the register will latch against the address
            // expression, both evaluated in the SAME step. `next_state()` is the
            // next-step VARIABLE — unconstrained unless the transition relation is
            // asserted, so `t' != addr` is trivially Sat and nothing is ever
            // withdrawn.
            let Some(next_val) =
                crate::adapter::btor2::parser::find_next_value_operand(file, line.nid)
            else {
                continue;
            };
            let Some(next_bv) = view.curr_signal(next_val.nid()) else {
                continue;
            };
            if next_bv.get_size() != addr_w {
                continue; // different widths cannot hold the same value (and z3 would
                // reject the equality as a sort error)
            }
            let solver = z3::Solver::new();
            solver.assert(next_bv.eq(addr_bv).not());
            if matches!(solver.check(), z3::SatResult::Unsat) {
                return true; // `next(t) == addr` is valid — the correspondence IS recorded
            }
        }
        false
    })
}

/// mununu#496 — flag a **registered array read whose address register can change
/// in the same cycle its data is consumed**, with nothing recording which address
/// the current data corresponds to.
///
/// The shape, in RTL:
///
/// ```systemverilog
/// always_ff @(posedge clk) begin
///     if (advance) a_q <= a_q + 1;   // the address register moves
///     q <= mem[a_q];                 // registered read against it
/// end
/// ```
///
/// `q` holds the word at the address `a_q` held *last* cycle, but a consumer
/// reading `q` alongside the live `a_q` sees a pair that never coexisted. monono
/// hit this twice in the same block — the second time with a prose rule against
/// it in force — shipping a 2 KB sprite bank shifted by one halfword. Prose did
/// not prevent the recurrence; this check is the structural refusal.
///
/// In BTOR2 the fault is three facts about one `next`:
///
/// ```text
/// 8  state 7 a_q          the address register
/// 11 read 4 10 8          read(mem, a_q)
/// 12 next 4 5 11          q <= read(...)        <- registered array read
/// 17 next 7 8 16          a_q <= (moving)       <- and the address moves
/// ```
///
/// **The satisfying form** registers the address alongside the data, so some
/// register captures `a_q` in the very cycle the read is issued:
///
/// ```text
/// 11 next 4 5 10          a_d <= a_q            <- the tracking signal
/// ```
///
/// So the check is: a registered read whose address is a *mutable* register and
/// for which **no** register's `next` is that address register. Like the existing
/// lift-form checks, it is satisfiable by writing the supported form — here, by
/// naming the tracking signal. Purely structural: no bit-blast, no properties, no
/// environment.
fn lint_registered_array_read_moving_address(btor2: &str) -> Result<Vec<SvLintFinding>, String> {
    use crate::adapter::btor2::ast::{Node, Op};

    let file = parser::parse(btor2).map_err(|e| format!("BTOR2 parse: {e}"))?;

    // `next` edges, indexed by the state they drive, plus the set of state nids
    // that some OTHER register captures verbatim (`t <= a`) — the tracking
    // signals that satisfy the rule.
    let mut next_of: std::collections::HashMap<i64, Option<i64>> = std::collections::HashMap::new();
    let mut tracked: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut tracked_values: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::Next { state, value, .. } = &line.node {
            // mununu#506: resolve the next-value through the same identity-preserving
            // wrappers as the address. BOTH sides must resolve, or the rule breaks in
            // the WORSE direction — `a_d <= a_q` lifts to
            // `next(a_d) = ite(rst, <alias of a_q>, 0)`, so a resolver applied only to
            // the address would stop recognising the SATISFYING form and start firing
            // on exactly the designs that already do the right thing.
            let resolved = resolve_to_state_register(&file, value.nid());
            next_of.insert(*state, resolved);
            // Tier 1 (mununu#506): also index each register's next value by its
            // CANONICAL VALUE, so a register that captures a whole address
            // EXPRESSION (`a_d <= a_q + 1`) counts as tracking it — not only one
            // that captures a bare register. Post-`opt` CSE means the design's two
            // occurrences of `a_q + 1` share a node, so nid equality is the right
            // structural test; Tier 2 replaces it with an SMT validity query for
            // the cases CSE misses.
            tracked_values.insert(canonical_value_nid(&file, value.nid()));
            // `t <= a` where `a` is a DIFFERENT register: `t` records which `a` the
            // current cycle used. A register's own hold (`a <= a`) is not tracking.
            if let Some(src) = resolved
                && src != *state
            {
                tracked.insert(src);
            }
        }
    }

    // A state's display name: its own symbol, else a `uext … 0 NAME` alias, else
    // an `output` that names it. Mirrors how the partial-write lint recovers names.
    let name_of = |nid: i64| -> Option<String> {
        if let Some(Node::State {
            symbol: Some(s), ..
        }) = node_of(&file, nid)
        {
            return Some(s.clone());
        }
        for line in &file.lines {
            match &line.node {
                Node::Output {
                    signal,
                    symbol: Some(s),
                } if signal.nid() == nid => {
                    return Some(s.clone());
                }
                // mununu#506: the alias carrying the name usually sits over the RESET
                // MUX, not the raw state (`12 uext 8 11 0 a_q`, where nid 11 is
                // `ite(rst_n, state, 0)`). Match on the alias operand RESOLVED to its
                // register, else the finding degrades to `<nid 10>` and loses the name
                // that makes it actionable.
                Node::Op {
                    symbol: Some(s),
                    args,
                    ..
                } if args.first().map(|a| a.nid()).is_some_and(|n| {
                    n == nid || resolve_to_state_register(&file, n) == Some(nid)
                }) =>
                {
                    return Some(s.clone());
                }
                _ => {}
            }
        }
        None
    };

    let mut findings = Vec::new();
    for line in &file.lines {
        let Node::Next { state, value, .. } = &line.node else {
            continue;
        };
        // Is this register's next value an array read?
        let Some(Node::Op {
            op: Op::Read, args, ..
        }) = node_of(&file, value.nid())
        else {
            continue;
        };
        // `read <sort> <array> <index>` — the address is the second operand.
        let Some(addr) = args.get(1).map(|a| a.nid()) else {
            continue;
        };
        // Tier 1 (mununu#506): analyse the address's CONE rather than demanding a
        // bare register. The question the rule needs answered is "what can make this
        // value change", which generalises over the whole shape class —
        // `mem[base + 1]`, `mem[{tag, a_q}]`, `mem[a_q[hi:lo]]`, `mem[a_q ^ mask]` —
        // instead of a list of special cases.
        let (cone_states, cone_has_input) = expr_leaves(&file, addr);

        // A register in the cone MOVES when its next value does not resolve back to
        // itself. `a <= a` lifts to `next(a) = ite(rst, <alias of a>, RESET)`, so
        // resolving is what distinguishes a genuine hold from the reset mux.
        let moving: Vec<i64> = cone_states
            .iter()
            .copied()
            .filter(|st| match next_of.get(st) {
                // SOUNDNESS: a BTOR2 `state` with NO `next` is HAVOC — the format
                // leaves it unconstrained at every step (the same reading as the
                // mununu#498 fix, and as `array_prophecy`'s registerized-index
                // guard). It therefore moves MAXIMALLY, not "not at all".
                None => true,
                Some(Some(src)) => *src != *st, // resolves to itself ⇒ genuinely held
                Some(None) => true,             // real logic ⇒ it moves
            })
            .collect();

        // SCOPE: the fault needs a moving REGISTER to mis-pair with. An address
        // driven only by inputs (`sprite_bank`'s `addr` port — how every block-RAM
        // wrapper is written) is the CALLER's obligation, not this module's, and
        // flagging it would fire on every memory wrapper in a design. Inputs alone
        // are therefore out of scope; inputs ALONGSIDE a moving register still flag,
        // because the register half can mis-pair.
        if moving.is_empty() {
            let _ = cone_has_input;
            continue;
        }

        // Satisfied when some register captures the address in the same cycle —
        // either the whole EXPRESSION (Tier 1: `a_d <= a_q + 1`) or, for a bare
        // register address, the register itself.
        let addr_value = canonical_value_nid(&file, addr);
        if tracked_values.contains(&addr_value) || moving.iter().all(|st| tracked.contains(st)) {
            continue;
        }
        // Tier 2 — the structural check says "report". Ask the solver whether some
        // register nevertheless records this exact address, and withdraw if it does.
        // Only reached when a finding is about to be emitted, so a clean design never
        // pays for it (see `smt_confirms_tracking`).
        if smt_confirms_tracking(&file, addr, *state) {
            continue;
        }
        // Report the moving register deterministically (lowest nid) — for a bare
        // address this is the register itself, unchanged from before.
        let addr = *moving.iter().min().expect("non-empty");

        let data = name_of(*state).unwrap_or_else(|| format!("<nid {state}>"));
        let address = name_of(addr).unwrap_or_else(|| format!("<nid {addr}>"));
        findings.push(SvLintFinding {
            signal: data.clone(),
            kind: SvLintSignalKind::Register,
            rule: SvLintRule::RegisteredArrayReadMovingAddress,
            detail: format!(
                "`{data}` is a registered array read addressed by `{address}`, which can change \
                 in the same cycle `{data}` is consumed, and no register captures `{address}` \
                 alongside it — so `{data}` and the live `{address}` are a pair that never \
                 coexisted. Register the address alongside the data — add a register that \
                 captures `{address}` in the same cycle as the read — and consume that instead \
                 of the live `{address}`."
            ),
        });
    }

    findings.sort_by(|a, b| a.signal.cmp(&b.signal));
    findings.dedup_by(|a, b| a.signal == b.signal);
    Ok(findings)
}

/// Borrow the `Node` at `nid`, if the file declares it.
fn node_of(
    file: &crate::adapter::btor2::ast::Btor2File,
    nid: i64,
) -> Option<&crate::adapter::btor2::ast::Node> {
    file.lookup(nid).map(|l| &l.node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lift_of(source: &str) -> SvLift {
        SvLift {
            source: source.to_string(),
            additional_sources: Vec::new(),
            top: None,
            use_sv2v: false,
            include_dirs: Vec::new(),
            frontend: SvFrontend::Auto,
        }
    }

    // The atom guards fire BEFORE the toolchain-gated lift, so they are testable in
    // make-ci (no sv2v / Yosys needed).
    #[test]
    fn liveness_rejects_malformed_atom_before_lift() {
        let e = sv_verify_liveness(&lift_of("module m; endmodule"), "x == y", "st == 1");
        assert!(
            e.is_err(),
            "a relational request atom must be rejected pre-lift"
        );
    }

    #[test]
    fn recoverability_rejects_malformed_target_before_lift() {
        let e = sv_verify_recoverability(&lift_of("module m; endmodule"), "not an atom !!");
        assert!(e.is_err(), "a malformed target must be rejected pre-lift");
    }

    // mununu#496 lint core — the yosys-slang lift (Yosys 0.60+70, slang plugin, in
    // the pinned `mununu-sva` image) of a registered array read, captured verbatim
    // so the regression runs in make-ci without slang.
    //
    //     always_ff @(posedge clk) begin
    //         if (advance) a_q <= a_q + 8'd1;   // the address register moves
    //         q <= mem[a_q];                    // registered read against it
    //     end
    //
    // `12 next 4 5 11` is the registered read (`11 read 4 10 8` = mem[a_q]) and
    // `17 next 7 8 16` is `a_q` moving. Nothing captures `a_q` alongside `q`, so
    // `q` and the live `a_q` are a pair that never coexisted.
    const SLANG_REGISTERED_READ_MOVING_ADDR: &str = r#"1 sort bitvec 1
2 input 1 advance
3 input 1 clk
4 sort bitvec 16
5 state 4
6 output 5 q
7 sort bitvec 8
8 state 7 a_q
9 sort array 7 4
10 state 9 mem
11 read 4 10 8
12 next 4 5 11
13 const 1 1
14 uext 7 13 7
15 add 7 8 14
16 ite 7 2 15 8
17 next 7 8 16
18 next 9 10 10 mem
"#;

    // The SATISFYING form, same lift path: the address is registered ALONGSIDE the
    // data (`a_d <= a_q`, nid `11 next 4 5 10`), so a consumer can tell which
    // address the current `q` corresponds to. Same read, same moving address — the
    // only difference is the tracking register, which is exactly what the rule asks
    // for. This is the contrast twin: a check that flags this too is useless.
    const SLANG_REGISTERED_READ_TRACKED: &str = r#"1 sort bitvec 1
2 input 1 advance
3 input 1 clk
4 sort bitvec 8
5 state 4
6 output 5 a_d
7 sort bitvec 16
8 state 7
9 output 8 q
10 state 4 a_q
11 next 4 5 10
12 sort array 4 7
13 state 12 mem
14 read 7 13 10
15 next 7 8 14
16 const 1 1
17 uext 4 16 7
18 add 4 10 17
19 ite 4 2 18 10
20 next 4 10 19
21 next 12 13 13 mem
"#;

    // mununu#506 — the SAME design as `SLANG_REGISTERED_READ_MOVING_ADDR`, except
    // `a_q` has a reset. That one change was enough to make the shipped rule blind:
    // `async2sync` lifts the async reset into a mux, so the read indexes the MUX
    // (`11 ite 8 4 10 9`) rather than the bare state (`10 state 8`), and requiring a
    // bare `state` meant the rule fired only on registers with NO reset — close to
    // none in real RTL. Captured from the real slang lift.
    const SLANG_RESET_GATED_MOVING_ADDR: &str = r#"1 sort bitvec 1
2 input 1 advance
3 input 1 clk
4 input 1 rst_n
5 sort bitvec 16
6 state 5
7 output 6 q
8 sort bitvec 8
9 const 8 00000000
10 state 8
11 ite 8 4 10 9
12 uext 8 11 0 a_q
13 sort array 8 5
14 state 13 mem
15 read 5 14 11
16 next 5 6 15
17 const 1 1
18 uext 8 17 7
19 add 8 11 18
20 ite 8 2 19 11
21 ite 8 4 20 9
22 next 8 10 21
23 next 13 14 14 mem
"#;

    // The reset-bearing SATISFYING twin: `a_d <= a_q` alongside the read. This is
    // the false-positive guard for the #506 fix — the tracking `next` is itself
    // wrapped in a reset mux (`16 ite 5 4 14 6` → `17 next 5 7 16`), so a resolver
    // applied only to the ADDRESS side would stop recognising this form and start
    // firing on exactly the designs that already do the right thing. Worse than the
    // original silence, hence a first-class regression.
    const SLANG_RESET_GATED_TRACKED: &str = r#"1 sort bitvec 1
2 input 1 advance
3 input 1 clk
4 input 1 rst_n
5 sort bitvec 8
6 const 5 00000000
7 state 5
8 ite 5 4 7 6
9 output 8 a_d
10 sort bitvec 16
11 state 10
12 output 11 q
13 state 5
14 ite 5 4 13 6
15 uext 5 14 0 a_q
16 ite 5 4 14 6
17 next 5 7 16
18 sort array 5 10
19 state 18 mem
20 read 10 19 14
21 next 10 11 20
22 const 1 1
23 uext 5 22 7
24 add 5 14 23
25 ite 5 2 24 14
26 ite 5 4 25 6
27 next 5 13 26
28 next 18 19 19 mem
"#;

    #[test]
    fn flags_a_reset_gated_address_register_through_the_mux_alias() {
        let f =
            lint_registered_array_read_moving_address(SLANG_RESET_GATED_MOVING_ADDR).expect("lint");
        assert_eq!(
            f.len(),
            1,
            "a reset on the address register must not hide the fault (mununu#506): {f:?}"
        );
        assert_eq!(f[0].signal, "q");
        assert!(
            f[0].detail.contains("a_q"),
            "the address register's NAME lives on the uext alias of the reset mux and \
             must still be recovered: {}",
            f[0].detail
        );
    }

    #[test]
    fn a_reset_gated_tracking_register_still_satisfies_the_rule() {
        let f = lint_registered_array_read_moving_address(SLANG_RESET_GATED_TRACKED).expect("lint");
        assert!(
            f.is_empty(),
            "the tracking register is recognised THROUGH its own reset mux; flagging this \
             would be a false positive on correctly-written RTL — strictly worse than the \
             under-firing the #506 fix removes. Got {f:?}"
        );
    }

    /// Tier 1 (mununu#506) — an ARITHMETIC address is now in scope. `mem[a_q + 1]`
    /// can mis-pair exactly as `mem[a_q]` can; what matters is whether anything in
    /// the address's cone MOVES, not whether the address is a bare register.
    /// (This test previously asserted the opposite — it encoded the limitation the
    /// cone analysis removes.)
    #[test]
    fn an_arithmetic_address_is_in_scope_when_its_cone_moves() {
        // nid 19 is `add 8 11 18` — `a_q + 1`. Read at that instead of at `a_q`.
        let arith = SLANG_RESET_GATED_MOVING_ADDR.replace("15 read 5 14 11", "15 read 5 14 19");
        let f = lint_registered_array_read_moving_address(&arith).expect("lint");
        assert_eq!(
            f.len(),
            1,
            "`mem[a_q + 1]` mis-pairs exactly as `mem[a_q]` does; the cone contains a \
             moving register: {f:?}"
        );
        assert!(
            f[0].detail.contains("a_q"),
            "the moving register in the cone must still be named: {}",
            f[0].detail
        );
    }

    /// Tier 2 (mununu#506) — the tracking register holds a STRUCTURALLY DIFFERENT
    /// but SEMANTICALLY EQUAL address, so Tier 1's nid comparison misses it and only
    /// the solver can withdraw the finding.
    ///
    /// The read indexes `add(a_q, 1)`; the tracking register captures `add(1, a_q)`
    /// — a distinct node that CSE did not merge (operand order differs), computing
    /// the same value. Flagging this would be a false positive on correct RTL, which
    /// is exactly what Tier 2 exists to prevent.
    #[test]
    fn smt_withdraws_a_finding_when_tracking_is_semantically_equal_but_not_structural() {
        // 19 = add(11, 18) = a_q + 1  (the read index)
        // 24 = add(18, 11) = 1 + a_q  (same value, different node)
        // 25/26: `a_d` whose next value is node 24.
        let tracked = SLANG_RESET_GATED_MOVING_ADDR
            .replace("15 read 5 14 11", "15 read 5 14 19")
            .replace(
                "23 next 13 14 14 mem\n",
                "23 next 13 14 14 mem\n24 add 8 18 11\n25 state 8 a_d\n26 next 8 25 24\n",
            );

        // Sanity: Tier 1 alone cannot see it — the two nodes are not the same nid.
        let a = canonical_value_nid(&parser::parse(&tracked).expect("parse"), 19);
        let b = canonical_value_nid(&parser::parse(&tracked).expect("parse"), 24);
        assert_ne!(
            a, b,
            "the fixture must be structurally distinct, else it tests nothing"
        );

        let f = lint_registered_array_read_moving_address(&tracked).expect("lint");
        assert!(
            f.is_empty(),
            "`a_d <= 1 + a_q` records the same address the read uses; the solver must \
             withdraw the finding even though the expressions are structurally \
             different: {f:?}"
        );
    }

    /// The false-positive guard for Tier 1, and the one that governs the design: a
    /// register capturing the whole address EXPRESSION satisfies the rule just as a
    /// register capturing a bare address does. Widening what counts as a moving
    /// address without widening what counts as tracking it would fire on correct
    /// code — strictly worse than the under-firing it replaces.
    #[test]
    fn a_register_capturing_the_whole_address_expression_satisfies_the_rule() {
        // Add `a_d` whose next value IS `a_q + 1` (nid 19) — the same node the read
        // indexes, which is what post-`opt` CSE produces for a design that registers
        // the address it reads at.
        let tracked = SLANG_RESET_GATED_MOVING_ADDR
            .replace("15 read 5 14 11", "15 read 5 14 19")
            .replace(
                "23 next 13 14 14 mem\n",
                "23 next 13 14 14 mem\n24 state 8 a_d\n25 next 8 24 19\n",
            );
        let f = lint_registered_array_read_moving_address(&tracked).expect("lint");
        assert!(
            f.is_empty(),
            "`a_d <= a_q + 1` alongside `q <= mem[a_q + 1]` records the correspondence; \
             flagging it would be a false positive on correct RTL: {f:?}"
        );
    }

    #[test]
    fn flags_registered_array_read_against_a_moving_address() {
        let f = lint_registered_array_read_moving_address(SLANG_REGISTERED_READ_MOVING_ADDR)
            .expect("lint");
        assert_eq!(f.len(), 1, "expected exactly one finding, got {f:?}");
        assert_eq!(f[0].signal, "q");
        assert_eq!(f[0].rule, SvLintRule::RegisteredArrayReadMovingAddress);
        assert!(
            f[0].detail.contains("a_q"),
            "the finding must name the address register: {}",
            f[0].detail
        );
    }

    #[test]
    fn registering_the_address_alongside_the_data_satisfies_the_rule() {
        let f =
            lint_registered_array_read_moving_address(SLANG_REGISTERED_READ_TRACKED).expect("lint");
        assert!(
            f.is_empty(),
            "the tracked form must NOT be flagged — the rule is satisfiable by naming \
             the tracking signal; got {f:?}"
        );
    }

    /// SOUNDNESS — a BTOR2 `state` with NO `next` is HAVOC, not held. The format
    /// leaves it unconstrained at every step (the mununu#498 reading, and the same
    /// rule `array_prophecy` applies to a registerized index), so such an address
    /// moves MAXIMALLY and must be flagged.
    ///
    /// This is the direction that is easy to get backwards: "no next-state function"
    /// reads like "never changes", and treating it that way silently drops the
    /// worst case.
    #[test]
    fn a_havoc_address_register_with_no_next_is_flagged() {
        // Drop `a_q`'s `next` line entirely — it becomes a free variable.
        let havoc = SLANG_REGISTERED_READ_MOVING_ADDR.replace("17 next 7 8 16\n", "");
        let f = lint_registered_array_read_moving_address(&havoc).expect("lint");
        assert_eq!(
            f.len(),
            1,
            "a state with no `next` is havoc every cycle, so the read can never be \
             paired with it; treating no-next as 'held' would drop the worst case: {f:?}"
        );
    }

    #[test]
    fn a_held_address_register_is_not_flagged() {
        // Same registered read, but `a_q` never changes (`next` is itself), so `q`
        // and the live `a_q` can never disagree. No finding.
        let held = SLANG_REGISTERED_READ_MOVING_ADDR.replace("17 next 7 8 16", "17 next 7 8 8");
        let f = lint_registered_array_read_moving_address(&held).expect("lint");
        assert!(
            f.is_empty(),
            "a register whose address is held constant cannot be mis-paired; got {f:?}"
        );
    }

    #[test]
    fn the_partial_write_rule_is_unaffected_by_the_array_read_rule() {
        // The two checks are independent: neither fixture trips the other rule.
        let a = lint_undriven_partial_writes(SLANG_REGISTERED_READ_MOVING_ADDR).expect("lint");
        assert!(
            a.is_empty(),
            "array-read fixture must not trip partsel: {a:?}"
        );
        let b = lint_registered_array_read_moving_address(SLANG_PARTSEL_LIFT).expect("lint");
        assert!(
            b.is_empty(),
            "partsel fixture must not trip array-read: {b:?}"
        );
    }

    // monono#partsel lint core — the yosys-slang lift of a plain-vector partial
    // register assignment (`a_q[11:8] <= val`), captured verbatim so the
    // regression runs in make-ci without slang. `a_q`'s unwritten bits are free
    // `input`s (nid 17, 21) mixed into the `concat` its name aliases; `o_partsel`
    // reads them. `b_q`/`c_q`/`d_q` (and their outputs) reach only the *named*
    // `val` input — faithful, never flagged. `sv lint` reports exactly the pair
    // the verifier would refuse: `a_q` (register) + `o_partsel` (output).
    const SLANG_PARTSEL_LIFT: &str = r#"1 sort bitvec 1
2 input 1 clk
3 one 1 rst_n
4 sort bitvec 4
5 input 4 val
6 sort bitvec 16
7 const 6 0000000000000000
8 state 6
9 ite 6 3 8 7
10 redor 1 9
11 output 10 o_concat
12 state 6
13 ite 6 3 12 7
14 redor 1 13
15 output 14 o_lowconcat
16 sort bitvec 8
17 input 16
18 const 4 0000
19 state 4
20 ite 4 3 19 18
21 input 4
22 sort bitvec 12
23 concat 22 20 17
24 concat 6 21 23
25 redor 1 24
26 output 25 o_partsel
27 state 6
28 ite 6 3 27 7
29 redor 1 28
30 output 29 o_plain
31 uext 6 24 0 a_q
32 uext 6 9 0 b_q
33 uext 6 13 0 c_q
34 uext 6 28 0 d_q
35 slice 16 9 7 0
36 concat 22 5 35
37 slice 4 9 15 12
38 concat 6 37 36
39 ite 6 3 38 7
40 next 6 8 39
41 const 22 000000000000
42 concat 6 41 5
43 ite 6 3 42 7
44 next 6 12 43
45 ite 4 3 5 18
46 next 4 19 45
47 ite 6 3 42 7
48 next 6 27 47
49 constd 6 0
50 init 6 8 49
51 constd 6 0
52 init 6 12 51
53 constd 4 0
54 init 4 19 53
55 constd 6 0
56 init 6 27 55
"#;

    #[test]
    fn lint_reports_only_the_undriven_partial_write_signals() {
        let findings =
            lint_undriven_partial_writes(SLANG_PARTSEL_LIFT).expect("lint parses the BTOR2");
        let names: Vec<&str> = findings.iter().map(|f| f.signal.as_str()).collect();
        // Exactly the register whose unwritten bits are free inputs, plus the one
        // output that reads it — deterministic, sorted, deduplicated.
        assert_eq!(
            names,
            vec!["a_q", "o_partsel"],
            "lint must flag the undriven partial-write register + its output, and \
             NOTHING faithful (b_q/c_q/d_q reach only the named `val`)"
        );
        assert_eq!(
            findings[0].kind,
            SvLintSignalKind::Register,
            "a_q is a register"
        );
        assert_eq!(
            findings[1].kind,
            SvLintSignalKind::Output,
            "o_partsel is an output"
        );
    }

    #[test]
    fn lint_is_clean_when_every_register_is_faithful() {
        // A fully-driven single register: no free inputs, no findings.
        const FAITHFUL: &str = "1 sort bitvec 1
2 input 1 d
3 state 1 q
4 next 1 3 2
5 zero 1
6 init 1 3 5
";
        let findings = lint_undriven_partial_writes(FAITHFUL).expect("lint parses");
        assert!(
            findings.is_empty(),
            "a faithful register driven by a named input must not be flagged; got {findings:?}"
        );
    }

    #[test]
    fn lint_rejects_malformed_btor2() {
        assert!(
            lint_undriven_partial_writes("not btor2 at all").is_err(),
            "a malformed BTOR2 body must surface a parse error, not an empty pass"
        );
    }

    /// mununu#475 item 1 — a function argument's `<function>.<arg>` mangled
    /// name (e.g. monono's `ctrl_code.c`, `ones8.v` in `tmds_encoder.sv`)
    /// emits as an `Op` node whose cone reaches a function-scope anonymous
    /// input; the raw `signal_reaches_anonymous_input` check would
    /// false-positive it as a partial-write register. The `.` heuristic
    /// filters these out. Fixture: two Op-symbol nodes — one dotted (a
    /// function-arg alias, must NOT be flagged) and one plain (a legitimate
    /// partsel alias, MUST be flagged).
    /// mununu#506/#507 — a FLATTENED HIERARCHY symbol is dotted too, and must NOT
    /// be swept up by the function-argument filter.
    ///
    /// This is why `sv lint` reported zero findings on monono's integrator while
    /// `verify-auto` refused a property on the same file: every hierarchical alias
    /// is `u_inst.sig`, so the blanket dotted-skip suppressed the whole design. They
    /// were not disagreeing — the lint was not looking.
    ///
    /// The discriminator is that a function has no registers: `u_inst` prefixes a
    /// `state`, so it is an instance scope; `ones8` does not, so it is a function.
    #[test]
    fn lint_does_not_skip_flattened_hierarchy_symbols() {
        const HIER_LIFT: &str = r#"1 sort bitvec 1
2 input 1
3 state 1 u_inst.r
4 and 1 2 3 u_inst.a_q
5 output 4 u_inst.a_q_out
6 and 1 2 2 ones8.v
7 output 6 ones8_out
"#;
        let findings = lint_undriven_partial_writes(HIER_LIFT).expect("lint parses the BTOR2");
        let names: Vec<&str> = findings.iter().map(|f| f.signal.as_str()).collect();
        assert!(
            names.contains(&"u_inst.a_q"),
            "a hierarchical alias must be visible — `u_inst` prefixes a `state`, so it \
             names an INSTANCE, not a function scope: {findings:?}"
        );
        assert!(
            !names.contains(&"ones8.v"),
            "the function-arg case stays suppressed — nothing under `ones8.` is a \
             `state` (mununu#475 item 1): {findings:?}"
        );
    }

    #[test]
    fn lint_skips_function_arg_dotted_op_symbols() {
        // - Anonymous input (nid 2) — the "havoc" that both Op signals read.
        // - `a_q` (nid 3, Op with plain symbol, reads the anonymous input via
        //   nid 4) — a legitimate partsel-alias shape → MUST be flagged.
        // - `ones8.v` (nid 5, Op with dotted symbol, same shape) — the
        //   function-arg case that used to false-positive → MUST NOT be flagged.
        const FN_ARG_LIFT: &str = r#"1 sort bitvec 1
2 input 1
3 and 1 2 2 a_q
4 output 3 a_q_out
5 and 1 2 2 ones8.v
6 output 5 ones8_out
"#;
        let findings = lint_undriven_partial_writes(FN_ARG_LIFT).expect("lint parses the BTOR2");
        let names: Vec<&str> = findings.iter().map(|f| f.signal.as_str()).collect();
        assert!(
            names.contains(&"a_q"),
            "the plain partsel-alias `a_q` must still be flagged: {findings:?}"
        );
        assert!(
            !names.contains(&"ones8.v"),
            "the function-arg `ones8.v` must NOT be flagged (mununu#475 item 1): {findings:?}"
        );
    }

    // P2 industrial anchor — recoverability on REAL OpenTitan RTL. From every reachable
    // state the AES cipher-control FSM can return to CIPHER_CTRL_IDLE (= 6'b001001 = 9):
    // `AG EF (aes_cipher_ctrl_cs == 9)`, the branching νμ property SVA cannot express,
    // decides HOLDS end-to-end through the SV lift + the recoverability verb. NOTE: the
    // vendored design is the EXTRACTED control FSM (18 state bits — within the exact
    // engine's ~40-bit cap, so the exact engine decides it); the OVER-CAP cube +
    // smt-hyper-must scale path is proven separately by the wide-fixture differential
    // tests in `crate::adapter::recoverability` (an 80-bit design where the exact engine
    // abstains and the cube path decides both polarities).
    #[test]
    #[ignore = "requires sv2v + Yosys + z3 (mununu-sva docker image); run with --ignored"]
    fn e2e_opentitan_aes_cipher_ctrl_recoverability_holds() {
        use std::path::PathBuf;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/dc_opentitan_aes_cipher_control_fsm/source");
        let read = |n: &str| {
            std::fs::read_to_string(dir.join(n)).unwrap_or_else(|e| panic!("read {n}: {e}"))
        };
        let lift = SvLift {
            source: read("aes_cipher_control_fsm.sv"),
            additional_sources: vec![
                ("aes_pkg.sv".into(), read("aes_pkg.sv")),
                ("aes_reg_pkg.sv".into(), read("aes_reg_pkg.sv")),
                ("prim_util_pkg.sv".into(), read("prim_util_pkg.sv")),
                ("prim_assert.sv".into(), read("prim_assert.sv")),
            ],
            top: Some("aes_cipher_control_fsm".into()),
            use_sv2v: true,
            include_dirs: Vec::new(),
            frontend: SvFrontend::Auto,
        };
        let verdict = sv_verify_recoverability(&lift, "aes_cipher_ctrl_cs == 9")
            .expect("recoverability decides on the AES cipher-control FSM");
        assert_eq!(
            verdict,
            PropertyVerdict::Holds,
            "AG EF idle must HOLD on the AES cipher-control FSM (every reachable state can \
             return to CIPHER_CTRL_IDLE); got {verdict:?}"
        );
    }

    // monono#partsel — the real slang lift of a plain-vector partial register
    // assignment, pinned as an AGREEMENT between the lint and the lift rather than
    // as a fixed lift shape, because the shape is the PLUGIN's, not ours:
    //   * yosys-slang in oss-cad-suite 2025-12-31 (yosys 0.60+70) split `a_q`'s
    //     unwritten bits into anonymous free `input`s → the lint must flag `a_q`;
    //   * yosys-slang in oss-cad-suite 2026-08-24 (yosys 0.68+120) lifts
    //     `a_q[11:8] <= val` faithfully — no anonymous inputs, `a_q` a plain state
    //     cell — so there is nothing to flag (measured 2026-09-18 when the image
    //     moved to that release).
    // The invariant: `a_q` is flagged EXACTLY WHEN the lift carries an anonymous
    // free input, and the fully-written `b_q` is never flagged. The structural
    // query itself is pinned against the captured old-plugin BTOR2
    // (`SLANG_PARTSEL_LIFT`, in make-ci without slang), so a plugin that stops
    // producing the shape does not silently retire the rule. Runs only in the
    // mununu-sva image.
    #[test]
    #[ignore = "requires yosys-slang (mununu-sva docker image); run with --ignored"]
    fn e2e_sv_lint_flags_slang_partial_write_iff_the_lift_splits_it() {
        const SRC: &str = r#"module partsel_lint (
  input  logic       clk,
  input  logic       rst_n,
  input  logic [3:0] val
);
  logic [15:0] a_q;  // only [11:8] written -> unwritten bits are free inputs (slang)
  logic [15:0] b_q;  // written in full -> faithful state cell
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      a_q <= '0;
      b_q <= '0;
    end else begin
      a_q[11:8] <= val;
      b_q       <= {12'b0, val};
    end
  end
endmodule
"#;
        let lift = SvLift {
            source: SRC.to_string(),
            additional_sources: Vec::new(),
            top: Some("partsel_lint".into()),
            use_sv2v: false,
            include_dirs: Vec::new(),
            frontend: SvFrontend::Slang,
        };
        let btor2 = lift.lift().expect("the slang lift runs");
        // An anonymous free input is an `input` line with no symbol — the design's
        // own ports (`clk`, `rst_n`, `val`) carry theirs.
        let anonymous_inputs = btor2
            .lines()
            .filter(|l| {
                let toks: Vec<&str> = l
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                toks.len() == 3 && toks[1] == "input"
            })
            .count();
        let findings = lint_undriven_partial_writes(&btor2).expect("lint scans the lift");
        let names: Vec<&str> = findings.iter().map(|f| f.signal.as_str()).collect();
        eprintln!("slang lift: {anonymous_inputs} anonymous free input(s); lint flagged {names:?}");
        assert_eq!(
            names.contains(&"a_q"),
            anonymous_inputs > 0,
            "a_q must be flagged exactly when the lift split it into free inputs \
             ({anonymous_inputs} anonymous input(s)); got {names:?}"
        );
        assert!(
            !names.contains(&"b_q"),
            "the fully-written register b_q is faithful on every plugin and must NOT be \
             flagged; got {names:?}"
        );
    }

    /// mununu#496 end-to-end, through the REAL slang lift: the faulty form is
    /// flagged and its satisfying twin is not. The unit tests above pin the
    /// structural query against captured BTOR2; this pins the *lift* — that
    /// yosys-slang really does emit `next(q) = read(mem, a_q)` with `a_q` carrying
    /// its own moving `next`, which is the shape the query depends on.
    ///
    /// Run in the pinned image (host has no slang):
    ///   docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work \
    ///     mununu-sva cargo test -p mununu-core --lib e2e_sv_lint_registered_array -- --ignored
    #[test]
    #[ignore = "requires yosys-slang (mununu-sva docker image); run with --ignored"]
    fn e2e_sv_lint_registered_array_read_moving_address() {
        const BAD: &str = r#"module rom_bad (
  input  logic        clk,
  input  logic        advance,
  output logic [15:0] q
);
  logic [15:0] mem [0:255];
  logic [7:0]  a_q;
  always_ff @(posedge clk) begin
    if (advance) a_q <= a_q + 8'd1;
    q <= mem[a_q];
  end
endmodule
"#;
        // Identical, plus the tracking register the rule asks for.
        const OK: &str = r#"module rom_ok (
  input  logic        clk,
  input  logic        advance,
  output logic [15:0] q,
  output logic [7:0]  a_d
);
  logic [15:0] mem [0:255];
  logic [7:0]  a_q;
  always_ff @(posedge clk) begin
    if (advance) a_q <= a_q + 8'd1;
    q   <= mem[a_q];
    a_d <= a_q;
  end
endmodule
"#;
        let lift_of = |src: &str, top: &str| SvLift {
            source: src.to_string(),
            additional_sources: Vec::new(),
            top: Some(top.into()),
            use_sv2v: false,
            include_dirs: Vec::new(),
            frontend: SvFrontend::Slang,
        };

        let bad = sv_lint_registers(&lift_of(BAD, "rom_bad")).expect("lint lifts rom_bad");
        let hit: Vec<_> = bad
            .iter()
            .filter(|f| f.rule == SvLintRule::RegisteredArrayReadMovingAddress)
            .collect();
        assert_eq!(
            hit.len(),
            1,
            "the registered read against a moving address must be flagged; got {bad:?}"
        );
        assert!(
            hit[0].detail.contains("a_q"),
            "the finding must name the address register; got {}",
            hit[0].detail
        );

        let ok = sv_lint_registers(&lift_of(OK, "rom_ok")).expect("lint lifts rom_ok");
        assert!(
            ok.iter()
                .all(|f| f.rule != SvLintRule::RegisteredArrayReadMovingAddress),
            "registering the address alongside the data satisfies the rule; got {ok:?}"
        );

        // mununu#506 — the same pair, but with a RESET on the address register, which
        // is what real RTL looks like. `async2sync` lifts the reset into a mux so the
        // read indexes the mux rather than the bare state; requiring a bare state made
        // the rule fire only on reset-less registers, i.e. almost never. Through the
        // REAL lift, both directions must still be right.
        const BAD_RST: &str = r#"module rom_bad_rst (
  input  logic        clk, rst_n,
  input  logic        advance,
  output logic [15:0] q
);
  logic [15:0] mem [0:255];
  logic [7:0]  a_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) a_q <= 8'd0;
    else if (advance) a_q <= a_q + 8'd1;
  end
  always_ff @(posedge clk) q <= mem[a_q];
endmodule
"#;
        const OK_RST: &str = r#"module rom_ok_rst (
  input  logic        clk, rst_n,
  input  logic        advance,
  output logic [15:0] q,
  output logic [7:0]  a_d
);
  logic [15:0] mem [0:255];
  logic [7:0]  a_q;
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin a_q <= 8'd0; a_d <= 8'd0; end
    else begin
      if (advance) a_q <= a_q + 8'd1;
      a_d <= a_q;
    end
  end
  always_ff @(posedge clk) q <= mem[a_q];
endmodule
"#;
        let bad_rst =
            sv_lint_registers(&lift_of(BAD_RST, "rom_bad_rst")).expect("lint lifts rom_bad_rst");
        let hit_rst: Vec<_> = bad_rst
            .iter()
            .filter(|f| f.rule == SvLintRule::RegisteredArrayReadMovingAddress)
            .collect();
        assert_eq!(
            hit_rst.len(),
            1,
            "a reset on the address register must not hide the fault; got {bad_rst:?}"
        );
        assert!(
            hit_rst[0].detail.contains("a_q"),
            "the address name must survive the reset-mux alias: {}",
            hit_rst[0].detail
        );

        let ok_rst =
            sv_lint_registers(&lift_of(OK_RST, "rom_ok_rst")).expect("lint lifts rom_ok_rst");
        assert!(
            ok_rst
                .iter()
                .all(|f| f.rule != SvLintRule::RegisteredArrayReadMovingAddress),
            "the reset-gated TRACKING register still satisfies the rule — flagging it \
             would be a false positive on correct RTL; got {ok_rst:?}"
        );
    }

    // The full lift → verdict path needs sv2v + Yosys; covered by the e2e suite in
    // the mununu-sva image, not make-ci.
}
