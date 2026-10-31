//! The fault-injection posture (PR A1 of the 2026-10-22 plan): a BTOR2 rewrite that lets an
//! adversary overwrite named registers, at most `k` times in total after a warm-up, so a
//! security-countermeasure check (`$rose(err_o) |-> ##[0:30] alert_p`) is asked about the
//! corruptions it was written for instead of holding vacuously on a design whose fault flags
//! never rise.
//!
//! **The rewrite.** For every site `i` (a register `r_i : σ_i` with a `next`), two fresh named
//! inputs `mununu_fault_en_i : bool` and `mununu_fault_val_i : σ_i`, and the register's next value
//! becomes `ite(fire_i, val_i, next_i)`. A global budget counter `mununu_fault_cnt` (saturating,
//! `⌈log₂(k+n+1)⌉` bits) and a warm-up clock `mununu_fault_warm` (true from cycle `W` on) gate
//! the firing STRUCTURALLY: `fire_i = en_i ∧ warm ∧ (cnt + popcount(en) ≤ k)` and
//! `cnt' = cnt + popcount(fire)`. No BTOR2 `constraint` is used — the cube engines never read
//! `Node::Constraint`, and in a game a constraint over an environment input would be conjoined
//! inside the controller's `∃` — so every engine sees the same total transition relation.
//!
//! **SOUNDNESS.** The injected design over-approximates the un-faulted one (`en ≡ 0` is one
//! input sequence). A safety / response HOLDS transfers to the RTL and reads "the countermeasure
//! responds for every corruption at these sites within the budget"; a VIOLATED is a real escape
//! under the declared sites, budget and warm-up — the fault inputs are NAMED, so mununu#751's
//! withholding of a VIOLATED over anonymous undriven nets does not apply. An ALTERNATING (νμ)
//! row whose cone holds a site is skipped by the caller: the fault player can restore any
//! state, so `AG EF Idle` would hold trivially.

#![deny(clippy::unwrap_used)] // docs/policies/failure-paths.md Rule 2: abstain, never panic
use std::collections::{BTreeSet, HashMap};

use super::ast::{Btor2File, ConstValue, Line, Nid, Node, Op, Operand, Sort};
use super::cex_value::CexValue;
use super::native_bmc::BmcTrace;
use super::parser;

/// How many injections the adversary may make in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultBound {
    /// Any number of injections (no counter is emitted).
    Unbounded,
    /// At most this many injections, counted over every site together (a global budget; two
    /// sites fired in one cycle cost two).
    AtMost(u32),
}

impl FaultBound {
    /// The budget as a number, `None` when unbounded.
    pub fn budget(self) -> Option<u32> {
        match self {
            FaultBound::Unbounded => None,
            FaultBound::AtMost(k) => Some(k),
        }
    }
}

/// One register the adversary may overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultSite {
    /// The register, as the model names it (`gen[1].q` is accepted and read as `gen$1.q`).
    pub register: String,
}

/// The whole posture: the sites, the one budget, the warm-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultSpec {
    pub sites: Vec<FaultSite>,
    pub bound: FaultBound,
    /// No injection before this many cycles have elapsed (OpenTitan's own `[*10]` assume).
    pub warmup: u32,
}

impl FaultSpec {
    /// Parse `--fault` operands: `REG`, `REG:k` (k ≥ 0) or `REG:unbounded`. A `:k` on any
    /// operand sets the one budget; two different `:k` values are an error; no `:k` anywhere
    /// means the default budget of one (a single-event upset).
    pub fn parse_many(operands: &[String], warmup: u32) -> Result<FaultSpec, String> {
        if operands.is_empty() {
            return Err("--fault: at least one register is required".into());
        }
        let mut sites = Vec::new();
        let mut bound: Option<FaultBound> = None;
        for raw in operands {
            let raw = raw.trim();
            let (reg, b) = match raw.rsplit_once(':') {
                Some((r, k)) if !r.is_empty() && !k.contains(['.', ']', '$']) => {
                    let b = if k.eq_ignore_ascii_case("unbounded") {
                        FaultBound::Unbounded
                    } else {
                        FaultBound::AtMost(k.parse::<u32>().map_err(|_| {
                            format!("--fault {raw}: the budget `{k}` is not a count or `unbounded`")
                        })?)
                    };
                    (r, Some(b))
                }
                _ => (raw, None),
            };
            if reg.is_empty() {
                return Err(format!("--fault {raw}: empty register name"));
            }
            if let Some(b) = b {
                match bound {
                    None => bound = Some(b),
                    Some(prev) if prev == b => {}
                    Some(prev) => {
                        return Err(format!(
                            "--fault: conflicting budgets {} and {} — the budget is global, give it \
                             once",
                            describe(prev),
                            describe(b)
                        ));
                    }
                }
            }
            let register = parser::flat_name(reg);
            if sites.iter().any(|s: &FaultSite| s.register == register) {
                return Err(format!("--fault: `{reg}` is named twice"));
            }
            sites.push(FaultSite { register });
        }
        Ok(FaultSpec {
            sites,
            bound: bound.unwrap_or(FaultBound::AtMost(1)),
            warmup,
        })
    }

    /// The posture as one label (`fault: a, b (≤1, warm-up 10)`), for notes and dump headers.
    pub fn as_label(&self) -> String {
        format!(
            "fault: {} ({}, warm-up {})",
            self.sites
                .iter()
                .map(|s| s.register.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            describe(self.bound),
            self.warmup
        )
    }

    /// The fault inputs the rewrite introduces, by name (the `en` then the `val` of each site).
    pub fn input_names(&self) -> Vec<String> {
        (0..self.sites.len())
            .flat_map(|i| [format!("{EN_PREFIX}{i}"), format!("{VAL_PREFIX}{i}")])
            .collect()
    }
}

fn describe(b: FaultBound) -> String {
    match b {
        FaultBound::Unbounded => "unbounded".into(),
        FaultBound::AtMost(k) => format!("≤{k}"),
    }
}

/// Which registers the adversary may corrupt: named by the user, or found automatically from the
/// security-countermeasure checks (`--fault-model sec-cm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultSelection {
    /// `--fault REG[:k]…` — the sites as given.
    Explicit(FaultSpec),
    /// `--fault-model sec-cm[:k]` — for every error → alert chain row, the state cells its
    /// fault flag compares (`cells_compared_by`), under one budget. `AtMost(0)` selects the
    /// sites and runs the antecedent-vacuity check without injecting: the "before" instrument.
    SecCm(FaultBound),
}

/// The fault-injection posture `sv verify-auto` / `sv mutate` / `btor2 verify` run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultPosture {
    pub selection: FaultSelection,
    /// No injection before this many cycles (OpenTitan's own `[*10]` assume; the CLI default).
    pub warmup: u32,
}

impl FaultPosture {
    /// Parse the CLI / API operands: `--fault` entries, an optional `--fault-model sec-cm[:k]`,
    /// the warm-up. Exactly one of the two selections; both or neither is an error only when
    /// both are given (neither ⇒ `None`, no posture).
    pub fn parse(
        faults: &[String],
        fault_model: Option<&str>,
        warmup: u32,
    ) -> Result<Option<FaultPosture>, String> {
        match (faults.is_empty(), fault_model) {
            (true, None) => Ok(None),
            (false, Some(_)) => Err("--fault and --fault-model are alternatives; give one".into()),
            (false, None) => Ok(Some(FaultPosture {
                selection: FaultSelection::Explicit(FaultSpec::parse_many(faults, warmup)?),
                warmup,
            })),
            (true, Some(model)) => {
                let (name, bound) = match model.split_once(':') {
                    Some((n, k)) => (
                        n,
                        if k.eq_ignore_ascii_case("unbounded") {
                            FaultBound::Unbounded
                        } else {
                            FaultBound::AtMost(k.parse::<u32>().map_err(|_| {
                                format!("--fault-model {model}: the budget `{k}` is not a count or `unbounded`")
                            })?)
                        },
                    ),
                    None => (model, FaultBound::AtMost(1)),
                };
                if !name.eq_ignore_ascii_case("sec-cm") {
                    return Err(format!(
                        "--fault-model {model}: unknown model `{name}` (the one model is `sec-cm`)"
                    ));
                }
                Ok(Some(FaultPosture {
                    selection: FaultSelection::SecCm(bound),
                    warmup,
                }))
            }
        }
    }
}

/// One selected site, for the report: what was corrupted, under which budget, why it was
/// chosen (`explicit`, `instance-prefix`, `cone-fallback`) and which rows it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultSiteReport {
    pub register: String,
    pub width: u32,
    pub bound: String,
    pub selected_by: String,
    pub rows: Vec<String>,
}

/// The posture as applied to one prepared model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultApplied {
    /// `fault: a, b (≤1, warm-up 10)` / `fault-model sec-cm (≤0 — selection only)`.
    pub label: String,
    pub sites: Vec<FaultSiteReport>,
    /// Empty when the budget is zero (selection and vacuity only, nothing injected).
    pub injected: Vec<InjectedFault>,
    /// The `(row name, rose atom)` pairs of the security-countermeasure chains found
    /// (`sec-cm` selection) — the rows the antecedent-vacuity check runs on.
    pub sec_cm_rows: Vec<(String, String)>,
    /// Rows whose fault flag reaches no state cell (`no-site`).
    pub rows_without_site: Vec<String>,
}

/// The state cells a set of named signals depends on, transitively through `next`
/// functions (the cone of influence), as names.
pub fn coi_states(file: &Btor2File, starts: &[String]) -> BTreeSet<String> {
    let symbols = parser::collect_symbols(file);
    let mut name_to_nid: HashMap<&str, Nid> =
        symbols.iter().map(|(n, s)| (s.as_str(), *n)).collect();
    for l in &file.lines {
        match &l.node {
            Node::Output {
                symbol: Some(s),
                signal,
            } => {
                name_to_nid.entry(s.as_str()).or_insert(signal.nid());
            }
            Node::Op {
                symbol: Some(s), ..
            } => {
                name_to_nid.entry(s.as_str()).or_insert(l.nid);
            }
            _ => {}
        }
    }
    let next_of: HashMap<Nid, Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, value, .. } => Some((*state, value.nid())),
            _ => None,
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = BTreeSet::new();
    let mut work: Vec<Nid> = starts
        .iter()
        .filter_map(|s| name_to_nid.get(s.as_str()).copied())
        .collect();
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::State { .. } => {
                if let Some(s) = symbols.get(&nid) {
                    out.insert(s.clone());
                }
                if let Some(v) = next_of.get(&nid) {
                    work.push(*v);
                }
            }
            Node::Op { args, .. } => work.extend(args.iter().map(|a| a.nid())),
            _ => {}
        }
    }
    out
}

/// Symbol prefixes of the inputs and cells the rewrite introduces.
pub const EN_PREFIX: &str = "mununu_fault_en_";
pub const VAL_PREFIX: &str = "mununu_fault_val_";
pub const CNT_SYMBOL: &str = "mununu_fault_cnt";
pub const WARM_SYMBOL: &str = "mununu_fault_warm";

/// One site after the rewrite: where it landed in the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedFault {
    pub register: String,
    pub nid: Nid,
    pub width: u32,
    pub en_input: String,
    pub val_input: String,
}

/// Apply the posture to `file` (the fully prepared model). Errors: a site that is not a
/// register, a site with no `next` (a cut — the adversary cannot corrupt a free cell), or a
/// model that already names a `mununu_fault_*` symbol.
pub fn inject_faults(
    file: &Btor2File,
    spec: &FaultSpec,
) -> Result<(Btor2File, Vec<InjectedFault>), String> {
    let symbols = parser::collect_symbols(file);
    if symbols.values().any(|s| s.starts_with("mununu_fault_")) {
        return Err("the model already names a `mununu_fault_*` symbol".into());
    }
    // Resolve every site first — all errors before any rewrite.
    let mut resolved: Vec<(Nid, u32, &FaultSite)> = Vec::with_capacity(spec.sites.len());
    for site in &spec.sites {
        let nid = resolve_register(file, &site.register).ok_or_else(|| {
            format!(
                "--fault {}: not a register (no state cell in the model carries that name)",
                site.register
            )
        })?;
        let width = state_width(file, nid).ok_or_else(|| {
            format!(
                "--fault {}: the register's sort is not a bit vector",
                site.register
            )
        })?;
        if !file
            .lines
            .iter()
            .any(|l| matches!(&l.node, Node::Next { state, .. } if *state == nid))
        {
            return Err(format!(
                "--fault {}: the register has no `next` — it is a cut or a free cell, and a \
                 corruption of a free cell is already every value",
                site.register
            ));
        }
        resolved.push((nid, width, site));
    }

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut added: Vec<Line> = Vec::new();
    let mut fresh = |node: Node, immediates: Vec<u32>, added: &mut Vec<Line>| -> Nid {
        let n = next_nid;
        next_nid += 1;
        added.push(Line {
            nid: n,
            node,
            immediates,
            source_line: 0,
        });
        n
    };
    // Sorts: the 1-bit sort (existing or fresh) and, per site, its register's sort.
    let bool_sort = file
        .lines
        .iter()
        .find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width: 1 },
            } => Some(l.nid),
            _ => None,
        })
        .unwrap_or_else(|| {
            fresh(
                Node::Sort {
                    sort: Sort::BitVec { width: 1 },
                },
                vec![],
                &mut added,
            )
        });
    let one = fresh(
        Node::Const {
            sort: bool_sort,
            value: ConstValue::One,
        },
        vec![],
        &mut added,
    );
    let zero = fresh(
        Node::Const {
            sort: bool_sort,
            value: ConstValue::Zero,
        },
        vec![],
        &mut added,
    );

    // Warm-up: `warm` is true from cycle W on (a saturating counter; W = 0 ⇒ constant true).
    let warm = if spec.warmup == 0 {
        one
    } else {
        let w = 32 - spec.warmup.leading_zeros();
        let s = fresh(
            Node::Sort {
                sort: Sort::BitVec { width: w },
            },
            vec![],
            &mut added,
        );
        let c0 = fresh(
            Node::Const {
                sort: s,
                value: ConstValue::Zero,
            },
            vec![],
            &mut added,
        );
        let c1 = fresh(
            Node::Const {
                sort: s,
                value: ConstValue::One,
            },
            vec![],
            &mut added,
        );
        let cw = fresh(
            Node::Const {
                sort: s,
                value: ConstValue::Dec(spec.warmup as i128),
            },
            vec![],
            &mut added,
        );
        let clock = fresh(
            Node::State {
                sort: s,
                symbol: Some(WARM_SYMBOL.into()),
            },
            vec![],
            &mut added,
        );
        fresh(
            Node::Init {
                sort: s,
                state: clock,
                value: Operand(c0),
            },
            vec![],
            &mut added,
        );
        let full = fresh(op(bool_sort, Op::Ugte, vec![clock, cw]), vec![], &mut added);
        let inc = fresh(op(s, Op::Add, vec![clock, c1]), vec![], &mut added);
        let nxt = fresh(op(s, Op::Ite, vec![full, clock, inc]), vec![], &mut added);
        fresh(
            Node::Next {
                sort: s,
                state: clock,
                value: Operand(nxt),
            },
            vec![],
            &mut added,
        );
        full
    };

    // The enables, warmed.
    let mut ens: Vec<Nid> = Vec::with_capacity(resolved.len());
    let mut vals: Vec<Nid> = Vec::with_capacity(resolved.len());
    let mut injected: Vec<InjectedFault> = Vec::with_capacity(resolved.len());
    for (i, (nid, width, site)) in resolved.iter().enumerate() {
        let en_name = format!("{EN_PREFIX}{i}");
        let val_name = format!("{VAL_PREFIX}{i}");
        let en = fresh(
            Node::Input {
                sort: bool_sort,
                symbol: Some(en_name.clone()),
            },
            vec![],
            &mut added,
        );
        let reg_sort = sort_of(file, *nid).expect("resolved above");
        let val = fresh(
            Node::Input {
                sort: reg_sort,
                symbol: Some(val_name.clone()),
            },
            vec![],
            &mut added,
        );
        let en_warm = fresh(op(bool_sort, Op::And, vec![en, warm]), vec![], &mut added);
        ens.push(en_warm);
        vals.push(val);
        injected.push(InjectedFault {
            register: site.register.clone(),
            nid: *nid,
            width: *width,
            en_input: en_name,
            val_input: val_name,
        });
    }

    // The budget: `fire_i = en_i ∧ (cnt + popcount(en) ≤ k)`, `cnt' = cnt + popcount(fire)`.
    let fires: Vec<Nid> = match spec.bound {
        FaultBound::Unbounded => ens.clone(),
        FaultBound::AtMost(k) => {
            // The counter is `bits` wide (it never exceeds `k`), the comparison one bit wider:
            // `cnt + popcount(en)` must not wrap for ANY `cnt` the sort admits, because an
            // inductive engine (k-induction, IC3) considers states the design never reaches —
            // with a wrapping sum, `cnt = 2^bits - 1` and one enable would read as "within
            // budget" in the induction step and the un-faulted design would stop being
            // provable (measured: k=0 read Unknown at k=30 before this).
            let n = ens.len() as u32;
            let bits = (32 - (k + n + 1).leading_zeros()).max(1);
            let s = fresh(
                Node::Sort {
                    sort: Sort::BitVec { width: bits },
                },
                vec![],
                &mut added,
            );
            let s_wide = fresh(
                Node::Sort {
                    sort: Sort::BitVec { width: bits + 1 },
                },
                vec![],
                &mut added,
            );
            let c0 = fresh(
                Node::Const {
                    sort: s,
                    value: ConstValue::Zero,
                },
                vec![],
                &mut added,
            );
            let ck = fresh(
                Node::Const {
                    sort: s_wide,
                    value: ConstValue::Dec(k as i128),
                },
                vec![],
                &mut added,
            );
            let cnt = fresh(
                Node::State {
                    sort: s,
                    symbol: Some(CNT_SYMBOL.into()),
                },
                vec![],
                &mut added,
            );
            fresh(
                Node::Init {
                    sort: s,
                    state: cnt,
                    value: Operand(c0),
                },
                vec![],
                &mut added,
            );
            // popcount(en) as a sum of zero-extended bits
            let mut sum_en = c0;
            for &e in &ens {
                let ext = fresh(op(s, Op::Uext, vec![e]), vec![bits - 1], &mut added);
                sum_en = fresh(op(s, Op::Add, vec![sum_en, ext]), vec![], &mut added);
            }
            let cnt_wide = fresh(op(s_wide, Op::Uext, vec![cnt]), vec![1], &mut added);
            let sum_wide = fresh(op(s_wide, Op::Uext, vec![sum_en]), vec![1], &mut added);
            let total = fresh(
                op(s_wide, Op::Add, vec![cnt_wide, sum_wide]),
                vec![],
                &mut added,
            );
            let within = fresh(op(bool_sort, Op::Ulte, vec![total, ck]), vec![], &mut added);
            let fires: Vec<Nid> = ens
                .iter()
                .map(|&e| fresh(op(bool_sort, Op::And, vec![e, within]), vec![], &mut added))
                .collect();
            let mut sum_fire = c0;
            for &f in &fires {
                let ext = fresh(op(s, Op::Uext, vec![f]), vec![bits - 1], &mut added);
                sum_fire = fresh(op(s, Op::Add, vec![sum_fire, ext]), vec![], &mut added);
            }
            let nxt = fresh(op(s, Op::Add, vec![cnt, sum_fire]), vec![], &mut added);
            fresh(
                Node::Next {
                    sort: s,
                    state: cnt,
                    value: Operand(nxt),
                },
                vec![],
                &mut added,
            );
            fires
        }
    };
    let _ = zero;

    // Rewrite each site's `next` in place: `ite(fire, val, next)`.
    let mut lines: Vec<Line> = file.lines.clone();
    for (i, (nid, _, _)) in resolved.iter().enumerate() {
        let reg_sort = sort_of(file, *nid).expect("resolved above");
        for l in lines.iter_mut() {
            if let Node::Next { state, value, .. } = &mut l.node
                && *state == *nid
            {
                let mux = fresh(
                    op(reg_sort, Op::Ite, vec![fires[i], vals[i], value.0]),
                    vec![],
                    &mut added,
                );
                // the mux reads the old next value with its sign; emit the operand as written
                if let Some(last) = added.last_mut()
                    && let Node::Op { args, .. } = &mut last.node
                {
                    args[2] = *value;
                }
                *value = Operand(mux);
            }
        }
    }
    // The added lines define what the rewritten `next` reads; the emitter for subprocess members
    // reorders constants and sorts, and mununu's parser accepts any acyclic order.
    lines.extend(added);
    Ok((rebuild(lines), injected))
}

/// Reorder `lines` so every line follows the lines it references (a stable topological order:
/// a line moves only when one of its operands is defined after it). The rewrites in this module
/// append new nodes that EARLIER lines then read (a re-pointed `next`, a re-minted flag); mununu's
/// own parser accepts that, but the subprocess members (btormc, pono, rIC3) read a BTOR2 file in
/// line order and reject a forward reference — measured 2026-10-23: the edn counter chains they
/// used to decide went ⊥. NIDs are unchanged; `emit_btor2_for_subprocess` renumbers in line order.
fn topo_order(lines: Vec<Line>) -> Vec<Line> {
    fn deps(node: &Node) -> Vec<Nid> {
        match node {
            Node::Sort {
                sort: Sort::Array { index, element },
            } => vec![*index, *element],
            Node::Sort { .. } => vec![],
            Node::Input { sort, .. } | Node::State { sort, .. } | Node::Const { sort, .. } => {
                vec![*sort]
            }
            Node::Op { sort, args, .. } => {
                let mut v = vec![*sort];
                v.extend(args.iter().map(|a| a.nid()));
                v
            }
            Node::Init { sort, state, value } | Node::Next { sort, state, value } => {
                vec![*sort, *state, value.nid()]
            }
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => vec![signal.nid()],
            Node::Justice { signals } => signals.iter().map(|s| s.nid()).collect(),
        }
    }
    let index: HashMap<Nid, usize> = lines.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    let mut placed = vec![false; lines.len()];
    let mut order: Vec<usize> = Vec::with_capacity(lines.len());
    for start in 0..lines.len() {
        if placed[start] {
            continue;
        }
        let mut stack: Vec<(usize, bool)> = vec![(start, false)];
        while let Some((i, expanded)) = stack.pop() {
            if placed[i] {
                continue;
            }
            if expanded {
                placed[i] = true;
                order.push(i);
                continue;
            }
            stack.push((i, true));
            for d in deps(&lines[i].node).into_iter().rev() {
                if let Some(&j) = index.get(&d)
                    && !placed[j]
                    && j != i
                {
                    stack.push((j, false));
                }
            }
        }
    }
    let mut slots: Vec<Option<Line>> = lines.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("placed once"))
        .collect()
}

pub(crate) fn rebuild(lines: Vec<Line>) -> Btor2File {
    let lines = topo_order(lines);
    let by_nid = lines.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    Btor2File { lines, by_nid }
}

fn op(sort: Nid, op: Op, args: Vec<Nid>) -> Node {
    Node::Op {
        sort,
        op,
        args: args.into_iter().map(Operand).collect(),
        symbol: None,
    }
}

pub(crate) fn sort_of(file: &Btor2File, nid: Nid) -> Option<Nid> {
    match &file.lookup(nid)?.node {
        Node::State { sort, .. } | Node::Input { sort, .. } => Some(*sort),
        _ => None,
    }
}

fn state_width(file: &Btor2File, nid: Nid) -> Option<u32> {
    let sort = sort_of(file, nid)?;
    match &file.lookup(sort)?.node {
        Node::Sort {
            sort: Sort::BitVec { width },
        } => Some(*width),
        _ => None,
    }
}

/// A register by any value-identical spelling, else by its canonical `--list` name
/// (`mutate::resolve_state_nid`'s rule).
pub(crate) fn resolve_register(file: &Btor2File, name: &str) -> Option<Nid> {
    if let Some(nid) = parser::resolve_state_alias(file, name, true) {
        return Some(nid);
    }
    parser::canonical_register_names(file)
        .into_iter()
        .find_map(|(nid, sym)| (sym == name).then_some(nid))
}

/// A sparse-FSM fault flag that the lift left as a synthesis constant, re-minted from the model:
/// `flag := ¬⋁_i (state == K_i)` over the LEGAL encodings — every constant the design compares
/// the state register against (its `case` labels, the assertions' and outputs' comparisons).
/// Returns the state cell's name, its width and the legal values. OpenTitan's
/// `prim_sparse_fsm_flop.unused_err_o` is `is_undefined_state(state_o)` (an enum-method loop
/// under `INC_ASSERT` that no frontend lifts — the shim's `else` branch is `1'b0`); this mint
/// restores it from the model when the compared constants are the enum's members, and
/// over-flags a member the design never compares against (such a value takes the FSM's
/// `default:` arm, so the alert still follows on OpenTitan's FSMs). Reported as
/// `selected_by: fsm-mint`.
pub struct MintedFlag {
    pub state: String,
    pub width: u32,
    pub legal: Vec<i128>,
}

/// Re-mint `flag` (a constant-0 `…unused_err_o` wire) from the state register of its own
/// instance: the flag `P.unused_err_o` names the sparse flop `P`, whose `P.state_o` aliases the
/// state cell. `None` when the flag is not a constant, the instance's state is not found, or the
/// state is compared against no constant.
pub fn mint_undefined_state_flag(file: &mut Btor2File, flag: &str) -> Option<MintedFlag> {
    use super::bit_blast::resolve_btor2_constant;
    let symbols = parser::collect_symbols(file);
    // the flag line: an Op aliasing a constant
    let flag_idx = file
        .lines
        .iter()
        .position(|l| matches!(&l.node, Node::Op { symbol: Some(s), .. } if s == flag))?;
    let flag_sort = match &file.lines[flag_idx].node {
        Node::Op { sort, args, .. } => {
            let src = args.first()?.nid();
            if !matches!(file.lookup(src)?.node, Node::Const { .. }) {
                return None;
            }
            *sort
        }
        _ => return None,
    };
    // the instance's state cell through `P.state_o`
    let prefix = flag.strip_suffix("unused_err_o")?;
    let state_o = format!("{prefix}state_o");
    let state_o_nid = file.lines.iter().find_map(|l| match &l.node {
        Node::Op {
            symbol: Some(s),
            args,
            ..
        } if *s == state_o => args.first().map(|a| a.nid()),
        Node::State {
            symbol: Some(s), ..
        } if *s == state_o => Some(l.nid),
        _ => None,
    })?;
    // follow value-identical aliases down to the state cell: a zero `uext`/`sext`, and the
    // async-reset mux `ite(rst, RESET, q)` with a constant arm (the reset is pinned inactive, so
    // the mux reads `q` — `parser::follow_state_alias`'s rule). Every node on the way is a
    // spelling of the state the design may compare against.
    let mut state_nid = state_o_nid;
    let mut spellings: Vec<Nid> = vec![state_o_nid];
    for _ in 0..8 {
        match &file.lookup(state_nid)?.node {
            Node::State { .. } => break,
            Node::Op {
                op: Op::Uext | Op::Sext,
                args,
                ..
            } => state_nid = args.first()?.nid(),
            Node::Op {
                op: Op::Ite, args, ..
            } if args.len() == 3 => {
                let is_const =
                    |n: Nid| matches!(file.lookup(n).map(|l| &l.node), Some(Node::Const { .. }));
                let is_state =
                    |n: Nid| matches!(file.lookup(n).map(|l| &l.node), Some(Node::State { .. }));
                let (t1, e1) = (args[1].nid(), args[2].nid());
                state_nid = if is_const(t1) && is_state(e1) && !args[2].is_negated() {
                    e1
                } else if is_const(e1) && is_state(t1) && !args[1].is_negated() {
                    t1
                } else {
                    return None;
                };
            }
            _ => return None,
        }
        spellings.push(state_nid);
    }
    if !matches!(file.lookup(state_nid)?.node, Node::State { .. }) {
        return None;
    }
    let state_sort = sort_of(file, state_nid)?;
    let width = state_width(file, state_nid)?;
    // every constant the design compares the state against (eq / neq), by value
    let mut legal: BTreeSet<i128> = BTreeSet::new();
    let mut const_nids: Vec<Nid> = Vec::new();
    for l in &file.lines {
        if let Node::Op {
            op: Op::Eq | Op::Neq,
            args,
            ..
        } = &l.node
            && args.len() == 2
        {
            let (a, b) = (args[0].nid(), args[1].nid());
            let other = if spellings.contains(&a) {
                b
            } else if spellings.contains(&b) {
                a
            } else {
                continue;
            };
            if let Some(line) = file.lookup(other)
                && matches!(line.node, Node::Const { .. })
                && let Some(v) = resolve_btor2_constant(file, other)
                && legal.insert(v as i128)
            {
                const_nids.push(other);
            }
        }
    }
    if legal.is_empty() {
        return None;
    }
    // `flag := not(or(eq(state, K_1), …))`, appended, and the flag line re-pointed at it
    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut added: Vec<Line> = Vec::new();
    let mut fresh = |node: Node, added: &mut Vec<Line>| -> Nid {
        let n = next_nid;
        next_nid += 1;
        added.push(Line {
            nid: n,
            node,
            immediates: Vec::new(),
            source_line: 0,
        });
        n
    };
    let mut any: Option<Nid> = None;
    for k in &const_nids {
        let eq = fresh(op(flag_sort, Op::Eq, vec![state_nid, *k]), &mut added);
        any = Some(match any {
            None => eq,
            Some(prev) => fresh(op(flag_sort, Op::Or, vec![prev, eq]), &mut added),
        });
    }
    let undefined = fresh(op(flag_sort, Op::Not, vec![any?]), &mut added);
    let _ = state_sort;
    // the flag keeps its NID and symbol; its one argument now reads the minted node
    if let Node::Op { args, .. } = &mut file.lines[flag_idx].node {
        args[0] = Operand(undefined);
    }
    file.lines.extend(added);
    *file = rebuild(std::mem::take(&mut file.lines));
    Some(MintedFlag {
        state: symbols
            .get(&state_nid)
            .cloned()
            .unwrap_or_else(|| state_o.clone()),
        width,
        legal: legal.into_iter().collect(),
    })
}

/// One injection a trace shows: the frame, the site and the value written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultEvent {
    pub cycle: usize,
    pub site: String,
    pub value: CexValue,
}

/// The injections in a BMC trace — every frame whose `mununu_fault_en_i` input is 1, with the
/// `mununu_fault_val_i` of that frame. (Whether the budget let it fire is in the design; a frame
/// whose `en` is 1 past the budget injected nothing — the reader checks the register.)
pub fn faults_in_trace(trace: &BmcTrace, injected: &[InjectedFault]) -> Vec<FaultEvent> {
    let mut out = Vec::new();
    for (cycle, frame) in trace.inputs.iter().enumerate() {
        let by_name: HashMap<&str, &CexValue> =
            frame.iter().map(|(k, v)| (k.as_str(), v)).collect();
        for site in injected {
            if by_name
                .get(site.en_input.as_str())
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                != 0
                && let Some(v) = by_name.get(site.val_input.as_str())
            {
                out.push(FaultEvent {
                    cycle,
                    site: site.register.clone(),
                    value: (*v).clone(),
                });
            }
        }
    }
    out
}

/// The state cells a boolean atom's value depends on (`--fault-model sec-cm` site selection):
/// resolve the atom's name to a node, walk its combinational cone (Op arguments; stop at a State;
/// ignore Inputs), return the state cells met, `(nid, name, width)`, sorted by name. `__past`
/// shadow cells are left out (they mirror a real cell). When the atom aliases a state cell `E`
/// and the cone holds nothing but `E` and shadows, expand once from `next(E)`'s operand — the
/// cells that DRIVE the flag are the ones to corrupt.
pub fn cells_compared_by(file: &Btor2File, atom: &str) -> Vec<(Nid, String, u32)> {
    let symbols = parser::collect_symbols(file);
    let mut name_to_nid: HashMap<&str, Nid> =
        symbols.iter().map(|(n, s)| (s.as_str(), *n)).collect();
    for l in &file.lines {
        match &l.node {
            Node::Output {
                symbol: Some(s),
                signal,
            } => {
                name_to_nid.entry(s.as_str()).or_insert(signal.nid());
            }
            Node::Op {
                symbol: Some(s), ..
            } => {
                name_to_nid.entry(s.as_str()).or_insert(l.nid);
            }
            _ => {}
        }
    }
    let Some(&start) = name_to_nid.get(atom) else {
        return Vec::new();
    };
    let cone = |start: Nid, skip_state: Option<Nid>| -> BTreeSet<Nid> {
        let mut seen = std::collections::HashSet::new();
        let mut cells = BTreeSet::new();
        let mut work = vec![start];
        while let Some(nid) = work.pop() {
            if !seen.insert(nid) {
                continue;
            }
            let Some(line) = file.lookup(nid) else {
                continue;
            };
            match &line.node {
                Node::State { .. } if Some(nid) != skip_state => {
                    cells.insert(nid);
                }
                Node::Op { args, .. } => work.extend(args.iter().map(|a| a.nid())),
                _ => {}
            }
        }
        cells
    };
    let is_shadow = |nid: Nid| symbols.get(&nid).is_some_and(|s| s.contains("__past"));
    let mut cells: BTreeSet<Nid> = cone(start, None)
        .into_iter()
        .filter(|n| !is_shadow(*n))
        .collect();
    // The atom IS a state cell (or aliases one) and nothing else drives it: look one step back.
    if cells.len() == 1 {
        let e = *cells.iter().next().expect("one");
        let drivers: BTreeSet<Nid> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Next { state, value, .. } if *state == e => Some(value.nid()),
                _ => None,
            })
            .flat_map(|v| cone(v, Some(e)))
            .filter(|n| !is_shadow(*n))
            .collect();
        if !drivers.is_empty() {
            cells = drivers;
        }
    }
    let mut out: Vec<(Nid, String, u32)> = cells
        .into_iter()
        .filter_map(|nid| {
            let name = symbols.get(&nid)?.clone();
            let width = state_width(file, nid)?;
            Some((nid, name, width))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::adapter::btor2::native_bmc::{
        BmcOutcome, SafetyVerdict, bmc_bad_reachable, bmc_bad_reachable_witness, decide_bad_safety,
    };

    /// Two 2-bit copies of a counter that must agree (`a + b == 3` after the complement
    /// arrangement is simplified to `a == b` here), an error flag latched when they differ, its
    /// `$past` shadow, and an alert register that follows the flag (`WIRED`) or never rises
    /// (`UNWIRED`). The sec_cm chain `$rose(err) |-> ##[0:2] alert` cannot fire without a fault.
    const WIRED: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 zero 2\n4 one 2\n5 zero 1\n\
                         6 input 1 inc\n7 state 2 a\n8 init 2 7 3\n9 state 2 b\n10 init 2 9 3\n\
                         11 add 2 7 4\n12 ite 2 6 11 7\n13 next 2 7 12\n14 add 2 9 4\n\
                         15 ite 2 6 14 9\n16 next 2 9 15\n17 neq 1 7 9\n18 state 1 err_q\n\
                         19 init 1 18 5\n20 next 1 18 17\n21 state 1 err_q__past\n22 init 1 21 5\n\
                         23 next 1 21 18\n24 state 1 alert_q\n25 init 1 24 5\n26 next 1 24 18\n";
    const UNWIRED: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 zero 2\n4 one 2\n5 zero 1\n\
                           6 input 1 inc\n7 state 2 a\n8 init 2 7 3\n9 state 2 b\n10 init 2 9 3\n\
                           11 add 2 7 4\n12 ite 2 6 11 7\n13 next 2 7 12\n14 add 2 9 4\n\
                           15 ite 2 6 14 9\n16 next 2 9 15\n17 neq 1 7 9\n18 state 1 err_q\n\
                           19 init 1 18 5\n20 next 1 18 17\n21 state 1 err_q__past\n22 init 1 21 5\n\
                           23 next 1 21 18\n24 state 1 alert_q\n25 init 1 24 5\n26 next 1 24 5\n";

    fn spec(ops: &[&str], warmup: u32) -> FaultSpec {
        FaultSpec::parse_many(
            &ops.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            warmup,
        )
        .expect("parses")
    }

    fn injected(content: &str, s: &FaultSpec) -> (String, Vec<InjectedFault>) {
        let file = parser::parse(content).expect("parses");
        let (out, inj) = inject_faults(&file, s).expect("injects");
        (super::super::emit::emit_btor2(&out), inj)
    }

    /// `bad := expr` appended as a named condition, so the reachability questions below are
    /// asked of the injected model with mununu's own native BMC.
    fn with_bad(btor2: &str, bad_expr: &str) -> String {
        let n = btor2
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse::<i64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        format!("{btor2}{n} {bad_expr}\n{} bad {n}\n", n + 1)
    }

    /// `bad` reachable within `k` steps (native BMC).
    fn reachable(btor2: &str, k: u32) -> bool {
        let file = parser::parse(btor2).expect("parses");
        matches!(bmc_bad_reachable(&file, k), Ok(BmcOutcome::Violated { .. }))
    }

    /// `bad` unreachable at every depth (native k-induction; `Unknown` fails the assertion so a
    /// weak proof never reads as safety).
    fn safe(btor2: &str) -> bool {
        let file = parser::parse(btor2).expect("parses");
        matches!(
            decide_bad_safety(&file, 30, Some(20_000)),
            Ok(SafetyVerdict::Safe { .. })
        )
    }

    #[test]
    fn parse_many_reads_the_one_global_budget_and_flattens_generate_names() {
        let s = spec(&["a", "gen[1].q:2"], 10);
        assert_eq!(s.bound, FaultBound::AtMost(2));
        assert_eq!(s.sites[1].register, "gen$1.q");
        assert_eq!(s.warmup, 10);
        assert_eq!(
            spec(&["a"], 0).bound,
            FaultBound::AtMost(1),
            "default: one upset"
        );
        assert_eq!(spec(&["a:unbounded"], 0).bound, FaultBound::Unbounded);
        let e = FaultSpec::parse_many(&["a:1".into(), "b:2".into()], 0).unwrap_err();
        assert!(e.contains("conflicting"), "{e}");
        let e = FaultSpec::parse_many(&["a".into(), "a".into()], 0).unwrap_err();
        assert!(e.contains("twice"), "{e}");
        assert_eq!(
            s.input_names(),
            vec![
                "mununu_fault_en_0",
                "mununu_fault_val_0",
                "mununu_fault_en_1",
                "mununu_fault_val_1"
            ]
        );
    }

    #[test]
    fn without_a_fault_the_error_flag_never_rises() {
        // the un-faulted design: `err_q` is unreachable (both copies always agree)
        assert!(safe(&with_bad(WIRED, "uext 1 18 0")));
    }

    #[test]
    fn one_fault_on_a_copy_raises_the_flag_and_the_wired_alert_follows() {
        let (model, inj) = injected(WIRED, &spec(&["a"], 2));
        assert_eq!(inj.len(), 1);
        assert_eq!((inj[0].register.as_str(), inj[0].width), ("a", 2));
        // the flag is now reachable …
        assert!(reachable(&with_bad(&model, "uext 1 18 0"), 12));
        // … and never before the warm-up: `err_q` at cycle ≤ 2 is unreachable (the fault
        // cannot fire before `warm`, and the flag follows it one cycle later)
        assert!(!reachable(&with_bad(&model, "uext 1 18 0"), 2));
        // the chain `$rose(err) |-> alert within 2` holds: err rose and alert stayed low two
        // cycles later is unreachable — expressed as `bad := err_q__past && !alert_q` at a frame
        // after the rise; the wired alert follows the flag in one cycle
        assert!(safe(&with_bad(&model, "and 1 21 -24")));
    }

    #[test]
    fn the_unwired_alert_is_a_real_escape_under_one_fault() {
        let (model, _) = injected(UNWIRED, &spec(&["a"], 2));
        assert!(
            reachable(&with_bad(&model, "and 1 21 -24"), 12),
            "err rose, alert never came"
        );
    }

    #[test]
    fn the_budget_is_global_and_never_overspent() {
        // k = 1: a second injection is impossible; the counter never reads 2
        let (model, _) = injected(WIRED, &spec(&["a", "b:1"], 0));
        let cnt_nid = model
            .lines()
            .find(|l| l.contains(CNT_SYMBOL))
            .and_then(|l| l.split_whitespace().next())
            .unwrap()
            .to_string();
        // the counter sort is the one declared for `mununu_fault_cnt`; read it off the line
        let sort = model
            .lines()
            .find(|l| l.contains(CNT_SYMBOL))
            .and_then(|l| l.split_whitespace().nth(2))
            .unwrap()
            .to_string();
        let n = model
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse::<i64>().ok())
            .max()
            .unwrap()
            + 1;
        let probe = format!(
            "{model}{n} constd {sort} 2\n{} eq 1 {cnt_nid} {n}\n{} bad {}\n",
            n + 1,
            n + 2,
            n + 1
        );
        assert!(safe(&probe), "k=1: the counter never reaches 2");
        // k = 2: both copies can be hit in the same cycle (two injections, cost two) and the
        // flag need not rise (both corrupted to the same value); the counter reaches 2
        let (model2, _) = injected(WIRED, &spec(&["a", "b:2"], 0));
        let cnt2 = model2
            .lines()
            .find(|l| l.contains(CNT_SYMBOL))
            .and_then(|l| l.split_whitespace().next())
            .unwrap()
            .to_string();
        let sort2 = model2
            .lines()
            .find(|l| l.contains(CNT_SYMBOL))
            .and_then(|l| l.split_whitespace().nth(2))
            .unwrap()
            .to_string();
        let n2 = model2
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse::<i64>().ok())
            .max()
            .unwrap()
            + 1;
        let probe2 = format!(
            "{model2}{n2} constd {sort2} 2\n{} eq 1 {cnt2} {n2}\n{} bad {}\n",
            n2 + 1,
            n2 + 2,
            n2 + 1
        );
        assert!(reachable(&probe2, 12), "k=2: two injections are spent");
    }

    #[test]
    fn a_zero_budget_reproduces_the_fault_free_design() {
        let (model, _) = injected(WIRED, &spec(&["a:0"], 0));
        assert!(safe(&with_bad(&model, "uext 1 18 0")));
    }

    #[test]
    fn errors_name_the_site() {
        let file = parser::parse(WIRED).expect("parses");
        let e = inject_faults(&file, &spec(&["nope"], 0)).unwrap_err();
        assert!(e.contains("nope") && e.contains("not a register"), "{e}");
        // a free cell (no next): declare one
        let free = format!("{WIRED}27 state 1 free_q\n");
        let file = parser::parse(&free).expect("parses");
        let e = inject_faults(&file, &spec(&["free_q"], 0)).unwrap_err();
        assert!(e.contains("no `next`"), "{e}");
        // a collision
        let taken = format!("{WIRED}27 state 1 mununu_fault_cnt\n");
        let file = parser::parse(&taken).expect("parses");
        let e = inject_faults(&file, &spec(&["a"], 0)).unwrap_err();
        assert!(e.contains("mununu_fault_"), "{e}");
    }

    #[test]
    fn faults_in_trace_lists_each_frame_whose_enable_is_set() {
        let (model, inj) = injected(UNWIRED, &spec(&["a"], 1));
        let file = parser::parse(&with_bad(&model, "and 1 21 -24")).expect("parses");
        let (outcome, trace) = bmc_bad_reachable_witness(&file, 12).expect("runs");
        assert!(matches!(outcome, BmcOutcome::Violated { .. }));
        let trace = trace.expect("a violated BMC carries its trace");
        let events = faults_in_trace(&trace, &inj);
        assert!(
            !events.is_empty(),
            "the escape needs an injection: {events:?}"
        );
        assert!(
            events.iter().all(|e| e.site == "a" && e.cycle >= 1),
            "{events:?}"
        );
    }

    #[test]
    fn the_posture_is_one_of_explicit_sites_or_the_sec_cm_model() {
        assert_eq!(FaultPosture::parse(&[], None, 10).expect("none"), None);
        let p = FaultPosture::parse(&["a:2".into()], None, 10)
            .expect("explicit")
            .expect("some");
        assert!(
            matches!(p.selection, FaultSelection::Explicit(ref s) if s.bound == FaultBound::AtMost(2))
        );
        let p = FaultPosture::parse(&[], Some("sec-cm"), 10)
            .expect("model")
            .expect("some");
        assert_eq!(p.selection, FaultSelection::SecCm(FaultBound::AtMost(1)));
        let p = FaultPosture::parse(&[], Some("sec-cm:0"), 10)
            .expect("model")
            .expect("some");
        assert_eq!(p.selection, FaultSelection::SecCm(FaultBound::AtMost(0)));
        assert!(FaultPosture::parse(&["a".into()], Some("sec-cm"), 10).is_err());
        assert!(
            FaultPosture::parse(&[], Some("random"), 10)
                .unwrap_err()
                .contains("unknown model")
        );
    }

    /// A sparse FSM (3-bit state, legal encodings 1/3/6 as the `case` labels of its `next`
    /// function) whose `u_sm.u_state_regs.unused_err_o` the lift left as `const 0`: the mint
    /// rewrites the flag to `state ∉ {1,3,6}` in place (same NID and symbol), the compared
    /// cells become the state, and under one fault on the state the flag is reachable.
    #[test]
    fn the_undefined_state_flag_is_minted_from_the_states_compared_constants() {
        const SPARSE: &str = "1 sort bitvec 1\n2 sort bitvec 3\n3 zero 1\n4 input 1 go\n\
                              5 state 2 u_sm.state_q\n6 constd 2 1\n7 init 2 5 6\n8 constd 2 3\n\
                              9 constd 2 6\n10 eq 1 5 6\n11 eq 1 5 8\n12 eq 1 5 9\n\
                              13 ite 2 4 8 5\n14 ite 2 10 13 5\n15 ite 2 11 9 14\n16 ite 2 12 6 15\n\
                              17 next 2 5 16\n18 uext 2 5 0 u_sm.u_state_regs.state_o\n\
                              19 uext 1 3 0 u_sm.u_state_regs.unused_err_o\n\
                              20 state 1 u_sm.u_state_regs.unused_err_o__past\n21 init 1 20 3\n22 next 1 20 19\n";
        let mut file = parser::parse(SPARSE).expect("parses");
        let minted =
            mint_undefined_state_flag(&mut file, "u_sm.u_state_regs.unused_err_o").expect("mints");
        assert_eq!((minted.state.as_str(), minted.width), ("u_sm.state_q", 3));
        assert_eq!(minted.legal, vec![1, 3, 6]);
        let text = super::super::emit::emit_btor2(&file);
        assert!(text.contains("u_sm.u_state_regs.unused_err_o"), "{text}");
        // the flag now compares the state
        let cells: Vec<String> = cells_compared_by(&file, "u_sm.u_state_regs.unused_err_o")
            .into_iter()
            .map(|c| c.1)
            .collect();
        assert_eq!(cells, vec!["u_sm.state_q"]);
        // without a fault the flag never rises (the FSM stays in its encodings) …
        assert!(safe(&with_bad(&text, "uext 1 19 0")));
        // … under one fault on the state it does
        let (faulted, _) = inject_faults(&file, &spec(&["u_sm.state_q"], 1)).expect("injects");
        let ftext = super::super::emit::emit_btor2(&faulted);
        assert!(reachable(&with_bad(&ftext, "uext 1 19 0"), 8));
        // a flag that is not a constant is left alone
        let mut file2 = parser::parse(SPARSE.replace("19 uext 1 3 0", "19 uext 1 10 0").as_str())
            .expect("parses");
        assert!(mint_undefined_state_flag(&mut file2, "u_sm.u_state_regs.unused_err_o").is_none());
    }

    /// Every line of a rewritten file follows its operands — the subprocess members read in line
    /// order and reject a forward reference.
    #[test]
    fn rewritten_files_have_no_forward_reference() {
        let no_forward = |text: &str| {
            let mut seen = std::collections::HashSet::new();
            for l in text
                .lines()
                .filter(|l| !l.starts_with(';') && !l.trim().is_empty())
            {
                let f: Vec<&str> = l.split_whitespace().collect();
                let nid: i64 = f[0].parse().expect("nid");
                let nums: Vec<i64> = f[2..]
                    .iter()
                    .filter_map(|x| x.parse::<i64>().ok())
                    .collect();
                let ops: Vec<i64> = match f[1] {
                    "slice" => nums[..nums.len().saturating_sub(2)].to_vec(),
                    "uext" | "sext" => nums[..nums.len().saturating_sub(1)].to_vec(),
                    "const" | "constd" | "consth" => nums[..1.min(nums.len())].to_vec(),
                    "sort" => Vec::new(),
                    _ => nums,
                };
                for o in ops {
                    assert!(
                        seen.contains(&o.abs()),
                        "line `{l}` reads {o} before it is defined"
                    );
                }
                seen.insert(nid);
            }
        };
        let (model, _) = injected(WIRED, &spec(&["a", "b:2"], 3));
        no_forward(&model);
        let mut file = parser::parse(
            "1 sort bitvec 1\n2 sort bitvec 3\n3 zero 1\n4 input 1 go\n5 state 2 s\n6 constd 2 1\n\
             7 init 2 5 6\n8 constd 2 3\n9 eq 1 5 6\n10 eq 1 5 8\n11 ite 2 4 8 5\n12 next 2 5 11\n\
             13 uext 2 5 0 u.state_o\n14 uext 1 3 0 u.unused_err_o\n15 state 1 u.unused_err_o__past\n\
             16 next 1 15 14\n",
        )
        .expect("parses");
        assert!(mint_undefined_state_flag(&mut file, "u.unused_err_o").is_some());
        no_forward(&super::super::emit::emit_btor2(&file));
    }

    #[test]
    fn cells_compared_by_walks_the_flag_to_the_copies_it_compares() {
        let file = parser::parse(WIRED).expect("parses");
        // `err_q` is a state cell whose next reads `a != b`: one step back finds both copies
        let cells: Vec<String> = cells_compared_by(&file, "err_q")
            .into_iter()
            .map(|c| c.1)
            .collect();
        assert_eq!(cells, vec!["a", "b"]);
        // a combinational flag names them directly
        let flag = format!("{WIRED}27 uext 1 17 0 mismatch\n");
        let file = parser::parse(&flag).expect("parses");
        let cells: Vec<String> = cells_compared_by(&file, "mismatch")
            .into_iter()
            .map(|c| c.1)
            .collect();
        assert_eq!(cells, vec!["a", "b"]);
        assert!(cells_compared_by(&file, "unknown").is_empty());
    }
}
