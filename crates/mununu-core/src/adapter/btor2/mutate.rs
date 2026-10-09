//! BTOR2 structural mutation (#468) — apply a **named** fault to a lifted design
//! and re-emit, so a verify pass can assert its property verdicts **flip**.
//!
//! This is the engine behind `sv mutate`: a mutation that flips a `holds` to
//! `violated` confirms the property actually **constrains** the mutated behaviour;
//! a mutation that does NOT flip it is the finding — a **vacuous** property or a
//! dead line the spec never pins down. Per [claims-integrity §2], a mutation
//! result is a statement about **property adequacy**, never a bug finding about the
//! design.
//!
//! Every mutation is a pure `parse → mutate [`Node`] → `emit_btor2` round-trip
//! (the emitter is a fixed point of the parser, so an untouched model re-emits
//! byte-for-byte; only the mutated node changes). No lift is needed, so the core
//! is unit-tested on a BTOR2 fixture in make-ci.

use super::ast::{Btor2File, ConstValue, Nid, Node, Op, Operand, Sort};
use super::bit_blast::resolve_btor2_constant;
use super::parser;

/// A named structural fault applied to a lifted BTOR2 design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// **Freeze** a register: replace its `next` value with the register's own
    /// current value (identity), so it never updates. Universal — every register
    /// with a `next` can be stuck.
    Stick(String),
    /// **Remove the reset arm**: a register whose `next` is a reset mux
    /// `ite(rst, RESET, d)` is rewritten to the data branch `d`, so it no longer
    /// returns to its reset value. Flips reset-dependent properties (recoverability).
    DropReset(String),
    /// **Perturb a comparison bound** in a register's fanout by `delta` (±1): the
    /// `Const` a register is compared against (`cnt < 8` → `cnt < 9`). `const_nid`
    /// disambiguates when the register is compared against more than one constant;
    /// `None` requires a unique bound. The classic off-by-one fault — flips a
    /// threshold/boundary property.
    OffByOne {
        reg: String,
        const_nid: Option<Nid>,
        delta: i64,
    },
    /// **Invert a named 1-bit condition** at every use site (flip the sign of every
    /// operand referencing it, reusing BTOR2's `-N` bit-not shorthand). Flips a
    /// property whose truth depends on the guard's polarity.
    InvertCond(String),
}

impl Mutation {
    /// The stable CLI/label spelling (`stick:<reg>` / `drop-reset:<reg>` /
    /// `off-by-one:<reg>[@<nid>][:±delta]` / `invert-cond:<sig>`).
    pub fn as_label(&self) -> String {
        match self {
            Mutation::Stick(r) => format!("stick:{r}"),
            Mutation::DropReset(r) => format!("drop-reset:{r}"),
            Mutation::OffByOne {
                reg,
                const_nid,
                delta,
            } => {
                let at = const_nid.map(|n| format!("@{n}")).unwrap_or_default();
                let sign = if *delta >= 0 { "+" } else { "" };
                format!("off-by-one:{reg}{at}:{sign}{delta}")
            }
            Mutation::InvertCond(s) => format!("invert-cond:{s}"),
        }
    }

    /// Parse a `stick:<reg>` / `drop-reset:<reg>` /
    /// `off-by-one:<reg>[@<const_nid>][:±1]` / `invert-cond:<sig>` selector.
    pub fn parse(spec: &str) -> Result<Mutation, String> {
        // mununu#635 — `--list` spells the kinds as JSON keys (`drop_reset`, `off_by_one`,
        // `invert_cond`) while the selector uses hyphens; a user copying a key from `--list`
        // got "unknown mutation" and concluded the kind was not applicable. Both spellings
        // are accepted; the canonical one (and the one `--help` shows) is hyphenated.
        let spec: std::borrow::Cow<'_, str> = match spec.split_once(':') {
            Some((kind, rest)) if kind.contains('_') => {
                format!("{}:{rest}", kind.replace('_', "-")).into()
            }
            _ => spec.into(),
        };
        let spec = spec.as_ref();
        if let Some(reg) = spec.strip_prefix("stick:") {
            reg_nonempty(reg).map(|r| Mutation::Stick(r.to_string()))
        } else if let Some(reg) = spec.strip_prefix("drop-reset:") {
            reg_nonempty(reg).map(|r| Mutation::DropReset(r.to_string()))
        } else if let Some(rest) = spec.strip_prefix("off-by-one:") {
            parse_off_by_one(rest)
        } else if let Some(sig) = spec.strip_prefix("invert-cond:") {
            reg_nonempty(sig).map(|s| Mutation::InvertCond(s.to_string()))
        } else {
            Err(format!(
                "unknown mutation `{spec}` — expected `stick:<reg>`, `drop-reset:<reg>`, \
                 `off-by-one:<reg>[@<const_nid>][:±1]`, or `invert-cond:<sig>` \
                 (see `sv mutate --list` for the available targets)"
            ))
        }
    }
}

/// Parse the `off-by-one:` tail: `<reg>[@<const_nid>][:±delta]` (default delta +1).
fn parse_off_by_one(rest: &str) -> Result<Mutation, String> {
    // Split off an optional `:±delta` suffix (delta is a signed integer).
    let (head, delta) = match rest.rsplit_once(':') {
        Some((h, d)) => {
            let delta = d.parse::<i64>().map_err(|_| {
                format!("off-by-one: delta `{d}` must be a signed integer (e.g. `:+1` or `:-1`)")
            })?;
            if delta == 0 {
                return Err("off-by-one: delta must be non-zero".to_string());
            }
            (h, delta)
        }
        None => (rest, 1),
    };
    // Split off an optional `@<const_nid>` disambiguator.
    let (reg, const_nid) = match head.split_once('@') {
        Some((r, n)) => {
            let nid = n
                .parse::<Nid>()
                .map_err(|_| format!("off-by-one: `@{n}` — const_nid must be an integer"))?;
            (r, Some(nid))
        }
        None => (head, None),
    };
    let reg = reg_nonempty(reg)?;
    Ok(Mutation::OffByOne {
        reg: reg.to_string(),
        const_nid,
        delta,
    })
}

fn reg_nonempty(reg: &str) -> Result<&str, String> {
    if reg.is_empty() {
        Err("mutation target register name is empty".to_string())
    } else {
        Ok(reg)
    }
}

/// The mutation targets available in a design — the `sv mutate --list` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationTargets {
    /// Every **named** register — a `stick:<reg>` target (sorted, deduped).
    pub stick: Vec<String>,
    /// Every named register whose `next` is a one-constant reset mux — a
    /// `drop-reset:<reg>` target (sorted, deduped; a subset of `stick`).
    pub drop_reset: Vec<String>,
    /// Every named register compared against ≥1 constant — an `off-by-one:<reg>`
    /// target (sorted, deduped).
    pub off_by_one: Vec<String>,
    /// Every named 1-bit signal (register alias / combinational output) — an
    /// `invert-cond:<sig>` target (sorted, deduped).
    pub invert_cond: Vec<String>,
}

/// Apply `m` to the BTOR2 text and return the mutated BTOR2 (round-trip via the
/// emitter). Errors if the named register does not exist or the mutation does not
/// apply to it (e.g. `drop-reset` on a register with no reset mux).
pub fn apply_mutation(btor2: &str, m: &Mutation) -> Result<String, String> {
    let mut file = parser::parse(btor2).map_err(|e| format!("BTOR2 parse: {}", e.message))?;
    match m {
        Mutation::Stick(reg) => apply_stick(&mut file, reg)?,
        Mutation::DropReset(reg) => apply_drop_reset(&mut file, reg)?,
        Mutation::OffByOne {
            reg,
            const_nid,
            delta,
        } => apply_off_by_one(&mut file, reg, *const_nid, *delta)?,
        Mutation::InvertCond(sig) => apply_invert_cond(&mut file, sig)?,
    }
    Ok(super::emit::emit_btor2(&file))
}

/// Enumerate the mutation targets in a lifted design (`sv mutate --list`).
pub fn list_targets(btor2: &str) -> Result<MutationTargets, String> {
    let file = parser::parse(btor2).map_err(|e| format!("BTOR2 parse: {}", e.message))?;
    // Each register under its strict-preferred name (its own symbol or a value-identical
    // alias) — the loose cone name advertised a next-value wire as a register (#633).
    let names = parser::canonical_register_names(&file);
    let mut stick: Vec<String> = Vec::new();
    let mut drop_reset: Vec<String> = Vec::new();
    let mut off_by_one: Vec<String> = Vec::new();
    for line in &file.lines {
        if !matches!(line.node, Node::State { .. }) {
            continue;
        }
        // Only NAMED registers are addressable targets (an anonymous split
        // sub-cell has no stable handle).
        let Some(name) = names.get(&line.nid) else {
            continue;
        };
        stick.push(name.clone());
        if reset_data_branch(&file, line.nid).is_some() {
            drop_reset.push(name.clone());
        }
        if !compare_bounds_over_state(&file, line.nid).is_empty() {
            off_by_one.push(name.clone());
        }
    }

    // invert-cond targets — every named 1-bit signal (a `state`/`Op` alias or an
    // `Output`), the same name universe `sv lint` scans, filtered to width 1.
    let mut invert_cond: Vec<String> = Vec::new();
    for line in &file.lines {
        let name = match &line.node {
            Node::State {
                symbol: Some(s), ..
            }
            | Node::Op {
                symbol: Some(s), ..
            }
            | Node::Output {
                symbol: Some(s), ..
            } => s,
            _ => continue,
        };
        if let Some((_, 1)) = resolve_signal(&file, name) {
            invert_cond.push(name.clone());
        }
    }

    for v in [
        &mut stick,
        &mut drop_reset,
        &mut off_by_one,
        &mut invert_cond,
    ] {
        v.sort();
        v.dedup();
    }
    Ok(MutationTargets {
        stick,
        drop_reset,
        off_by_one,
        invert_cond,
    })
}

/// Resolve a user register NAME to its `state` line nid: any **value-identical**
/// spelling (the symbol on the `state` line, a `uext … 0 NAME` alias, the port that
/// mirrors it — [`parser::resolve_state_alias`]), else the name `--list` advertises it
/// under ([`parser::canonical_register_names`], the loose fallback for a cell nothing
/// names exactly). `None` if no state cell carries it — a next-value wire such as
/// `cnt_d` is not a register (#633).
fn resolve_state_nid(file: &Btor2File, name: &str) -> Option<Nid> {
    if let Some(nid) = parser::resolve_state_alias(file, name, true) {
        return Some(nid);
    }
    parser::canonical_register_names(file)
        .into_iter()
        .find_map(|(nid, sym)| (sym == name).then_some(nid))
}

/// Freeze a register: point its `next` at its own state nid (identity → frozen).
fn apply_stick(file: &mut Btor2File, reg: &str) -> Result<(), String> {
    let state_nid = resolve_state_nid(file, reg).ok_or_else(|| {
        format!("stick: `{reg}` is not a register (no state cell in the lift carries that name)")
    })?;
    let mut found = false;
    for l in file.lines.iter_mut() {
        if let Node::Next { state, value, .. } = &mut l.node
            && *state == state_nid
        {
            *value = Operand(state_nid);
            found = true;
        }
    }
    if found {
        Ok(())
    } else {
        Err(format!(
            "stick: register `{reg}` (nid {state_nid}) has no `next` line to freeze \
             (it is already an undriven/free register)"
        ))
    }
}

/// Drop a register's reset arm: rewrite its reset-mux `next` to the data branch.
fn apply_drop_reset(file: &mut Btor2File, reg: &str) -> Result<(), String> {
    let state_nid = resolve_state_nid(file, reg).ok_or_else(|| {
        format!("drop-reset: `{reg}` is not a register (no state cell carries that name)")
    })?;
    let data = reset_data_branch(file, state_nid).ok_or_else(|| {
        format!(
            "drop-reset: register `{reg}`'s next is not a one-constant reset mux \
             (`ite(rst, RESET, d)`) — there is no reset arm to drop"
        )
    })?;
    for l in file.lines.iter_mut() {
        if let Node::Next { state, value, .. } = &mut l.node
            && *state == state_nid
        {
            *value = data;
        }
    }
    Ok(())
}

/// If `state_nid`'s `next` is a one-constant reset mux `ite(cond, then, else)`
/// (exactly one branch a constant — the reset value), return the OTHER (data)
/// branch operand; else `None`. Mirrors `fsm_scan::analyze_reset_mux`'s shape test
/// (the constant branch is the reset value, whatever the polarity).
fn reset_data_branch(file: &Btor2File, state_nid: Nid) -> Option<Operand> {
    let next_val_nid = file.lines.iter().find_map(|l| match &l.node {
        Node::Next { state, value, .. } if *state == state_nid => Some(value.nid()),
        _ => None,
    })?;
    // Copy the two data operands out (Operand: Copy), releasing the file borrow
    // before the constant lookups below.
    let (then_op, else_op) = match file.lookup(next_val_nid).map(|l| &l.node) {
        Some(Node::Op {
            op: Op::Ite, args, ..
        }) if args.len() == 3 => (args[1], args[2]),
        _ => return None,
    };
    match (
        resolve_btor2_constant(file, then_op.nid()),
        resolve_btor2_constant(file, else_op.nid()),
    ) {
        (None, Some(_)) => Some(then_op), // reset in the `else` branch ⇒ data is `then`
        (Some(_), None) => Some(else_op), // reset in the `then` branch ⇒ data is `else`
        _ => None, // both-const / neither-const ⇒ not an unambiguous reset mux
    }
}

// ── off-by-one (comparison-bound perturbation) ─────────────────────────────────

/// Is `op` a comparison producing a 1-bit result (the ops an off-by-one bound
/// lives on)? There is no exposed predicate for this set, so it is spelled here.
fn is_comparison(op: Op) -> bool {
    matches!(
        op,
        Op::Ult
            | Op::Ulte
            | Op::Slt
            | Op::Slte
            | Op::Ugt
            | Op::Ugte
            | Op::Sgt
            | Op::Sgte
            | Op::Eq
            | Op::Neq
    )
}

/// True if `state_nid` is in the COMBINATIONAL cone of `start` (stops at state
/// cells and inputs — does not cross a state's `next`, which is sequential depth).
fn cone_reaches_state(file: &Btor2File, start: Nid, state_nid: Nid) -> bool {
    let mut seen: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let mut work = vec![start];
    while let Some(nid) = work.pop() {
        if nid == state_nid {
            return true;
        }
        if !seen.insert(nid) {
            continue;
        }
        if let Some(line) = file.lookup(nid)
            && let Node::Op { args, .. } = &line.node
        {
            for a in args {
                work.push(a.nid());
            }
        }
    }
    false
}

/// Every `<reg> <cmp> <const>` comparison in the fanout of `state_nid`: the
/// `(compare_nid, const_nid, const_value)` of each comparison op with exactly one
/// constant operand whose OTHER operand's cone reaches the register.
fn compare_bounds_over_state(file: &Btor2File, state_nid: Nid) -> Vec<(Nid, Nid, u64)> {
    let mut out = Vec::new();
    for line in &file.lines {
        let Node::Op { op, args, .. } = &line.node else {
            continue;
        };
        if !is_comparison(*op) || args.len() != 2 {
            continue;
        }
        let (a, b) = (args[0], args[1]);
        let (const_nid, const_val, other) = match (
            resolve_btor2_constant(file, a.nid()),
            resolve_btor2_constant(file, b.nid()),
        ) {
            (Some(v), None) => (a.nid(), v, b.nid()),
            (None, Some(v)) => (b.nid(), v, a.nid()),
            _ => continue, // both / neither constant ⇒ not a register-vs-const bound
        };
        if cone_reaches_state(file, other, state_nid) {
            out.push((line.nid, const_nid, const_val));
        }
    }
    out
}

fn fmt_bounds(bounds: &[(Nid, Nid, u64)]) -> String {
    bounds
        .iter()
        .map(|(_, cn, v)| format!("@{cn}(={v})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Perturb a `Const` register bound by `delta` (±1), re-encoding it as `constd`
/// (the sort — and so the width — is preserved). A perturbation below 0 is an error.
fn apply_off_by_one(
    file: &mut Btor2File,
    reg: &str,
    const_nid: Option<Nid>,
    delta: i64,
) -> Result<(), String> {
    let state_nid = resolve_state_nid(file, reg)
        .ok_or_else(|| format!("off-by-one: `{reg}` is not a register (no state cell)"))?;
    let bounds = compare_bounds_over_state(file, state_nid);
    let target = match const_nid {
        Some(n) => {
            if !bounds.iter().any(|(_, cn, _)| *cn == n) {
                return Err(format!(
                    "off-by-one: @{n} is not a comparison-bound constant in the fanout of `{reg}` \
                     — candidates: {}",
                    fmt_bounds(&bounds)
                ));
            }
            n
        }
        None => match bounds.as_slice() {
            [] => {
                return Err(format!(
                    "off-by-one: found no `{reg} <cmp> <const>` comparison to perturb"
                ));
            }
            [(_, cn, _)] => *cn,
            many => {
                return Err(format!(
                    "off-by-one: `{reg}` is compared against {} constants — ambiguous; \
                     disambiguate with `off-by-one:{reg}@<const_nid>`. Candidates: {}",
                    many.len(),
                    fmt_bounds(many)
                ));
            }
        },
    };

    let cur = resolve_btor2_constant(file, target)
        .ok_or_else(|| format!("off-by-one: nid {target} is not a resolvable constant"))?;
    let new = cur as i128 + delta as i128;
    if new < 0 {
        return Err(format!(
            "off-by-one: perturbing the bound {cur} by {delta} would go below 0"
        ));
    }
    for l in file.lines.iter_mut() {
        if l.nid == target
            && let Node::Const { value, .. } = &mut l.node
        {
            *value = ConstValue::Dec(new);
            return Ok(());
        }
    }
    Err(format!("off-by-one: nid {target} is not a `const` node"))
}

// ── invert-cond (named 1-bit condition polarity flip) ──────────────────────────

/// Resolve a named signal to its value nid + bit width — a `state`/`Op` alias
/// (value at the line's nid) or an `Output` (value at the observed signal's nid).
fn resolve_signal(file: &Btor2File, name: &str) -> Option<(Nid, u32)> {
    for line in &file.lines {
        let value_nid = match &line.node {
            Node::State {
                symbol: Some(s), ..
            } if s == name => line.nid,
            Node::Op {
                symbol: Some(s), ..
            } if s == name => line.nid,
            Node::Output {
                symbol: Some(s),
                signal,
            } if s == name => signal.nid(),
            _ => continue,
        };
        if let Some(w) = nid_width(file, value_nid) {
            return Some((value_nid, w));
        }
    }
    None
}

/// The bit width of the value produced at `nid` (via its sort line). `None` for a
/// node that produces no bit-vector value (or an array sort).
fn nid_width(file: &Btor2File, nid: Nid) -> Option<u32> {
    let sort_nid = match &file.lookup(nid)?.node {
        Node::Input { sort, .. }
        | Node::State { sort, .. }
        | Node::Const { sort, .. }
        | Node::Op { sort, .. } => *sort,
        _ => return None,
    };
    match &file.lookup(sort_nid)?.node {
        Node::Sort {
            sort: Sort::BitVec { width },
        } => Some(*width),
        _ => None,
    }
}

/// Invert a named 1-bit condition at EVERY use site — flip the sign of every
/// operand referencing its value nid (BTOR2's `-N` bit-not shorthand), skipping the
/// value's own defining line. Errors if the signal is missing, not 1-bit, or unused.
fn apply_invert_cond(file: &mut Btor2File, sig: &str) -> Result<(), String> {
    let (sig_nid, width) = resolve_signal(file, sig)
        .ok_or_else(|| format!("invert-cond: `{sig}` is not a named signal in the lift"))?;
    if width != 1 {
        return Err(format!(
            "invert-cond: `{sig}` is {width} bits wide — only a 1-bit condition can be inverted \
             (a wider `-N` is a bitwise-not, not a boolean flip)"
        ));
    }
    // mununu#635 — the NAME usually sits on a `uext … 0` alias yosys mints for a wire
    // (`uext 1 <and> 0 violation`), while the logic reads the underlying node directly; the
    // alias itself has no readers. Invert every reader of the whole value-identical set — the
    // node the alias chain bottoms out at and every alias of it — so `invert-cond:violation`
    // flips the condition where the design consumes it, not only where it is labelled.
    // (Measured on `link_ctrl`: "no use sites to invert" on a target `--list` had advertised.)
    let mut set: std::collections::HashSet<Nid> = std::collections::HashSet::from([sig_nid]);
    let mut cur = sig_nid;
    while let Some(Node::Op {
        op: Op::Uext | Op::Sext,
        args,
        ..
    }) = file.lookup(cur).map(|l| &l.node)
        && args.len() == 1
        && file.lookup(cur).map(|l| l.immediates.first() == Some(&0)) == Some(true)
    {
        cur = args[0].nid();
        set.insert(cur);
    }
    let mut grew = true;
    while grew {
        grew = false;
        for l in &file.lines {
            if set.contains(&l.nid) {
                continue;
            }
            if let Node::Op {
                op: Op::Uext | Op::Sext,
                args,
                ..
            } = &l.node
                && args.len() == 1
                && l.immediates.first() == Some(&0)
                && set.contains(&args[0].nid())
            {
                set.insert(l.nid);
                grew = true;
            }
        }
    }
    let mut flipped = 0usize;
    for l in file.lines.iter_mut() {
        if set.contains(&l.nid) {
            continue; // never flip the signal's own inputs or its aliases — only its consumers
        }
        for target in &set {
            flip_operands_to(&mut l.node, *target, &mut flipped);
        }
    }
    if flipped == 0 {
        return Err(format!(
            "invert-cond: `{sig}` (nid {sig_nid}) has no use sites to invert"
        ));
    }
    Ok(())
}

/// Flip the sign of every operand of `node` whose absolute nid is `target`
/// (`+target` ↔ `-target`), counting each flip.
fn flip_operands_to(node: &mut Node, target: Nid, counter: &mut usize) {
    let mut flip = |op: &mut Operand| {
        if op.nid() == target {
            op.0 = -op.0;
            *counter += 1;
        }
    };
    match node {
        Node::Op { args, .. } => {
            for a in args {
                flip(a);
            }
        }
        Node::Init { value, .. } | Node::Next { value, .. } => flip(value),
        Node::Bad { signal }
        | Node::Constraint { signal }
        | Node::Fair { signal }
        | Node::Output { signal, .. } => flip(signal),
        Node::Justice { signals } => {
            for s in signals {
                flip(s);
            }
        }
        Node::Sort { .. } | Node::Input { .. } | Node::State { .. } | Node::Const { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 2-register design: `q` toggles (no reset mux); `r` is a reset mux
    // `ite(rst, 0, d)` — reset in the THEN branch (active-high, reset value 0),
    // data in the ELSE branch (`d`). Both named.
    const FIXTURE: &str = "1 sort bitvec 1
2 input 1 rst
3 input 1 d
4 state 1 q
5 not 1 4
6 next 1 4 5
7 zero 1
8 init 1 4 7
9 state 1 r
10 ite 1 2 7 3
11 next 1 9 10
12 init 1 9 7
";

    fn next_value_of(btor2: &str, state_nid: Nid) -> Nid {
        let file = parser::parse(btor2).expect("parse mutated");
        file.lines
            .iter()
            .find_map(|l| match &l.node {
                Node::Next { state, value, .. } if *state == state_nid => Some(value.nid()),
                _ => None,
            })
            .expect("a next for the state")
    }

    #[test]
    fn stick_freezes_the_register_to_its_own_state() {
        let out = apply_mutation(FIXTURE, &Mutation::Stick("q".into())).expect("stick q");
        // next(q=4) now points at 4 (identity → frozen), was 5 (not q).
        assert_eq!(next_value_of(&out, 4), 4);
        // The other register is untouched.
        assert_eq!(next_value_of(&out, 9), 10);
    }

    #[test]
    fn drop_reset_rewrites_next_to_the_data_branch() {
        let out = apply_mutation(FIXTURE, &Mutation::DropReset("r".into())).expect("drop-reset r");
        // next(r=9) now points at the data branch `d` (nid 3), dropping the ite (10).
        assert_eq!(next_value_of(&out, 9), 3);
        // `q` untouched.
        assert_eq!(next_value_of(&out, 4), 5);
    }

    /// mununu#635 — a condition named through a `uext 0` alias (how yosys labels a wire) is
    /// inverted where the logic READS it (the underlying node), not only at the alias, which has
    /// no readers of its own. Measured on `link_ctrl`: `invert-cond:violation` reported "no use
    /// sites" on a target `--list` had advertised.
    #[test]
    fn x635_invert_cond_follows_the_alias_to_the_readers() {
        let btor2 = "1 sort bitvec 1\n2 input 1 ack\n3 input 1 nack\n4 and 1 2 3\n\
                     5 uext 1 4 0 violation\n6 state 1 fatal\n7 ite 1 4 2 6\n8 next 1 6 7\n";
        let out = apply_mutation(btor2, &Mutation::parse("invert-cond:violation").unwrap())
            .expect("the alias resolves to the and-node's readers");
        assert!(
            out.lines().any(|l| l.starts_with("7 ite 1 -4 ")),
            "the ite's condition is the negated and-node: {out}"
        );
        assert!(
            out.lines().any(|l| l.starts_with("5 uext 1 4 0")),
            "the alias itself is not flipped: {out}"
        );
    }

    /// mununu#635 — the `_` spelling `--list` prints as JSON keys parses to the same mutation as
    /// the hyphenated selector; the hyphenated form is canonical and the error names it.
    #[test]
    fn x635_underscore_kind_spellings_are_accepted_aliases() {
        for (a, b) in [
            ("drop_reset:r", "drop-reset:r"),
            ("off_by_one:cnt", "off-by-one:cnt"),
            ("off_by_one:cnt@7:-1", "off-by-one:cnt@7:-1"),
            ("invert_cond:sig", "invert-cond:sig"),
        ] {
            assert_eq!(
                Mutation::parse(a).unwrap(),
                Mutation::parse(b).unwrap(),
                "{a}"
            );
        }
        let err = Mutation::parse("flip_bit:r").unwrap_err();
        assert!(
            err.contains("drop-reset") && err.contains("off-by-one") && err.contains("invert-cond")
        );
    }

    #[test]
    fn list_targets_reports_stick_for_all_named_and_drop_reset_for_reset_muxes() {
        let t = list_targets(FIXTURE).expect("list");
        assert_eq!(t.stick, vec!["q".to_string(), "r".to_string()]);
        assert_eq!(
            t.drop_reset,
            vec!["r".to_string()],
            "only the reset-mux register `r` is a drop-reset target; `q` toggles"
        );
    }

    // #633 — the cell (4) carries no symbol; the next-value op `cnt_d` (9) comes first
    // and reaches the cell in the loose cone trace, the `uext 0` alias `cnt_q` (11) is
    // value-identical. `--list` used to advertise `cnt_d` as the register.
    const ALIASED_COUNTER: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n\
4 state 2\n5 zero 2\n6 init 2 4 5\n7 one 2\n8 add 2 4 7\n\
9 ite 2 3 8 4 cnt_d\n10 next 2 4 9\n11 uext 2 4 0 cnt_q\n";

    #[test]
    fn x633_list_targets_names_a_register_by_its_value_identical_alias() {
        let t = list_targets(ALIASED_COUNTER).expect("list");
        assert_eq!(
            t.stick,
            vec!["cnt_q".to_string()],
            "not the next-value wire `cnt_d`"
        );
        let file = parser::parse(ALIASED_COUNTER).expect("parse");
        assert_eq!(resolve_state_nid(&file, "cnt_q"), Some(4));
        assert_eq!(
            resolve_state_nid(&file, "cnt_d"),
            None,
            "a next-value wire is not a register"
        );
        // The listed name applies.
        let out =
            apply_mutation(ALIASED_COUNTER, &Mutation::Stick("cnt_q".into())).expect("stick cnt_q");
        assert_eq!(next_value_of(&out, 4), 4);
    }

    #[test]
    fn stick_unknown_register_errors() {
        assert!(apply_mutation(FIXTURE, &Mutation::Stick("nope".into())).is_err());
    }

    #[test]
    fn drop_reset_on_a_non_reset_register_errors() {
        // `q` has no reset mux (its next is `not q`), so there is nothing to drop.
        assert!(apply_mutation(FIXTURE, &Mutation::DropReset("q".into())).is_err());
    }

    #[test]
    fn mutation_parse_roundtrips_the_labels() {
        assert_eq!(
            Mutation::parse("stick:foo").unwrap(),
            Mutation::Stick("foo".into())
        );
        assert_eq!(
            Mutation::parse("drop-reset:bar").unwrap(),
            Mutation::DropReset("bar".into())
        );
        assert_eq!(
            Mutation::parse("stick:foo").unwrap().as_label(),
            "stick:foo"
        );
        assert!(Mutation::parse("frobnicate:x").is_err());
        assert!(Mutation::parse("stick:").is_err());
    }

    // ── #468b targeting mutations ──────────────────────────────────────────────

    // A 4-bit counter `cnt` that increments while `cnt < 8` and freezes at 8. The
    // bound `8` (nid 7) is the off-by-one target; the guard `below` = `cnt < 8`
    // (nid 8, a named 1-bit op) drives the `ite` at nid 9 and is the invert-cond
    // target.
    const BOUNDED_COUNTER: &str = "1 sort bitvec 1
2 sort bitvec 4
3 input 1 en
4 state 2 cnt
5 one 2
6 add 2 4 5
7 constd 2 8
8 ult 1 4 7 below
9 ite 2 8 6 4
10 next 2 4 9
11 zero 2
12 init 2 4 11
";

    #[test]
    fn off_by_one_perturbs_the_unique_compare_bound() {
        let out = apply_mutation(BOUNDED_COUNTER, &Mutation::parse("off-by-one:cnt").unwrap())
            .expect("off-by-one cnt");
        let file = parser::parse(&out).expect("parse mutated");
        // The bound const (nid 7) moved 8 → 9 (+1 default); width preserved.
        assert_eq!(resolve_btor2_constant(&file, 7), Some(9));
    }

    #[test]
    fn off_by_one_honors_a_negative_delta() {
        let out = apply_mutation(
            BOUNDED_COUNTER,
            &Mutation::parse("off-by-one:cnt:-1").unwrap(),
        )
        .expect("off-by-one cnt -1");
        let file = parser::parse(&out).expect("parse");
        assert_eq!(resolve_btor2_constant(&file, 7), Some(7));
    }

    #[test]
    fn off_by_one_on_a_register_with_no_bound_errors() {
        // `q` (from FIXTURE) is never compared against a constant.
        assert!(apply_mutation(FIXTURE, &Mutation::parse("off-by-one:q").unwrap()).is_err());
    }

    #[test]
    fn invert_cond_flips_the_named_guard_at_its_use_site() {
        let out = apply_mutation(
            BOUNDED_COUNTER,
            &Mutation::parse("invert-cond:below").unwrap(),
        )
        .expect("invert-cond below");
        let file = parser::parse(&out).expect("parse mutated");
        // The `ite` at nid 9 selected on `below` (nid 8); after inversion its
        // selector operand is negated (`-8`, the BTOR2 bit-not shorthand).
        let sel = file
            .lines
            .iter()
            .find_map(|l| match &l.node {
                Node::Op {
                    op: Op::Ite, args, ..
                } if l.nid == 9 => Some(args[0]),
                _ => None,
            })
            .expect("the ite at nid 9");
        assert_eq!(sel.nid(), 8, "still the same signal");
        assert!(sel.is_negated(), "the guard operand must be inverted (-8)");
    }

    #[test]
    fn invert_cond_rejects_a_multibit_signal() {
        // `cnt` is 4 bits — not a boolean condition.
        assert!(
            apply_mutation(
                BOUNDED_COUNTER,
                &Mutation::parse("invert-cond:cnt").unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn invert_cond_unknown_signal_errors() {
        assert!(
            apply_mutation(
                BOUNDED_COUNTER,
                &Mutation::parse("invert-cond:nope").unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn list_targets_includes_off_by_one_and_invert_cond() {
        let t = list_targets(BOUNDED_COUNTER).expect("list");
        assert!(t.stick.contains(&"cnt".to_string()));
        assert_eq!(
            t.off_by_one,
            vec!["cnt".to_string()],
            "cnt is compared against a constant bound"
        );
        assert!(
            t.invert_cond.contains(&"below".to_string()),
            "the 1-bit guard `below` is an invert-cond target; got {:?}",
            t.invert_cond
        );
        assert!(
            !t.invert_cond.contains(&"cnt".to_string()),
            "the 4-bit `cnt` is not a 1-bit condition"
        );
    }

    #[test]
    fn targeting_mutation_labels_round_trip() {
        assert_eq!(
            Mutation::parse("off-by-one:cnt").unwrap(),
            Mutation::OffByOne {
                reg: "cnt".into(),
                const_nid: None,
                delta: 1
            }
        );
        assert_eq!(
            Mutation::parse("off-by-one:cnt@7:-1").unwrap(),
            Mutation::OffByOne {
                reg: "cnt".into(),
                const_nid: Some(7),
                delta: -1
            }
        );
        assert_eq!(
            Mutation::parse("invert-cond:below").unwrap(),
            Mutation::InvertCond("below".into())
        );
        assert_eq!(
            Mutation::parse("off-by-one:cnt@7:-1").unwrap().as_label(),
            "off-by-one:cnt@7:-1"
        );
        assert!(Mutation::parse("off-by-one:cnt:0").is_err());
        assert!(Mutation::parse("off-by-one:cnt:bad").is_err());
    }
}
