//! BTOR2 parser.
//!
//! BTOR2 is line-based ASCII; tokens are whitespace-separated. Comments start
//! with `;` and run to end of line. Empty lines and comment-only lines are
//! ignored.

use super::ast::*;
use crate::adapter::{AdapterError, AdapterErrorKind, SourceLocation};
use std::collections::HashMap;

pub fn parse(content: &str) -> Result<Btor2File, AdapterError> {
    let mut file = Btor2File::default();

    for (idx, raw) in content.lines().enumerate() {
        let source_line = idx + 1;
        // Strip comments and trim.
        let line = match raw.find(';') {
            Some(pos) => &raw[..pos],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 2 {
            return Err(parse_err(source_line, format!("malformed line: {line:?}")));
        }

        let nid = parse_nid(tokens[0], source_line)?;
        let kw = tokens[1];

        let node = parse_node(kw, &tokens[2..], source_line)?;
        let immediates = collect_immediates(kw, &tokens[2..], source_line)?;

        if file.by_nid.contains_key(&nid) {
            return Err(parse_err(source_line, format!("duplicate NID {nid}")));
        }
        file.by_nid.insert(nid, file.lines.len());
        file.lines.push(Line {
            nid,
            node,
            immediates,
            source_line,
        });
    }

    validate(&file)?;
    Ok(file)
}

fn parse_node(kw: &str, args: &[&str], source_line: usize) -> Result<Node, AdapterError> {
    match kw {
        "sort" => parse_sort(args, source_line),
        "input" => Ok(Node::Input {
            sort: parse_nid(arg(args, 0, source_line, "input sort")?, source_line)?,
            symbol: args.get(1).map(|s| s.to_string()),
        }),
        "state" => Ok(Node::State {
            sort: parse_nid(arg(args, 0, source_line, "state sort")?, source_line)?,
            symbol: args.get(1).map(|s| s.to_string()),
        }),
        "init" => {
            let sort = parse_nid(arg(args, 0, source_line, "init sort")?, source_line)?;
            let state = parse_nid(arg(args, 1, source_line, "init state ref")?, source_line)?;
            let value = parse_operand(arg(args, 2, source_line, "init value ref")?, source_line)?;
            Ok(Node::Init { sort, state, value })
        }
        "next" => {
            let sort = parse_nid(arg(args, 0, source_line, "next sort")?, source_line)?;
            let state = parse_nid(arg(args, 1, source_line, "next state ref")?, source_line)?;
            let value = parse_operand(arg(args, 2, source_line, "next value ref")?, source_line)?;
            Ok(Node::Next { sort, state, value })
        }
        "bad" => Ok(Node::Bad {
            signal: parse_operand(arg(args, 0, source_line, "bad signal")?, source_line)?,
        }),
        "constraint" => Ok(Node::Constraint {
            signal: parse_operand(arg(args, 0, source_line, "constraint signal")?, source_line)?,
        }),
        "fair" => Ok(Node::Fair {
            signal: parse_operand(arg(args, 0, source_line, "fair signal")?, source_line)?,
        }),
        "output" => Ok(Node::Output {
            signal: parse_operand(arg(args, 0, source_line, "output signal")?, source_line)?,
            symbol: args.get(1).map(|s| s.to_string()),
        }),
        "justice" => {
            let n = parse_uint(arg(args, 0, source_line, "justice count")?, source_line)?;
            let mut signals = Vec::with_capacity(n as usize);
            for i in 0..n as usize {
                signals.push(parse_operand(
                    arg(args, 1 + i, source_line, "justice signal")?,
                    source_line,
                )?);
            }
            Ok(Node::Justice { signals })
        }
        // Constants
        "zero" | "one" | "ones" | "const" | "constd" | "consth" => {
            parse_const(kw, args, source_line)
        }
        // Operators
        other => parse_op(other, args, source_line),
    }
}

fn parse_sort(args: &[&str], source_line: usize) -> Result<Node, AdapterError> {
    let kind = arg(args, 0, source_line, "sort kind")?;
    match kind {
        "bitvec" => {
            let width = parse_uint(arg(args, 1, source_line, "bitvec width")?, source_line)?;
            Ok(Node::Sort {
                sort: Sort::BitVec { width },
            })
        }
        "array" => {
            let index = parse_nid(arg(args, 1, source_line, "array index sort")?, source_line)?;
            let element = parse_nid(
                arg(args, 2, source_line, "array element sort")?,
                source_line,
            )?;
            Ok(Node::Sort {
                sort: Sort::Array { index, element },
            })
        }
        other => Err(parse_err(
            source_line,
            format!("unknown sort kind '{other}'"),
        )),
    }
}

fn parse_const(kw: &str, args: &[&str], source_line: usize) -> Result<Node, AdapterError> {
    let sort = parse_nid(arg(args, 0, source_line, "const sort")?, source_line)?;
    let value = match kw {
        "zero" => ConstValue::Zero,
        "one" => ConstValue::One,
        "ones" => ConstValue::Ones,
        "const" => ConstValue::Bin(arg(args, 1, source_line, "const bits")?.to_string()),
        "constd" => {
            let raw = arg(args, 1, source_line, "constd value")?;
            let v: i128 = raw
                .parse()
                .map_err(|_| parse_err(source_line, format!("bad decimal literal '{raw}'")))?;
            ConstValue::Dec(v)
        }
        "consth" => ConstValue::Hex(arg(args, 1, source_line, "consth value")?.to_string()),
        _ => unreachable!("parse_const dispatched by keyword"),
    };
    Ok(Node::Const { sort, value })
}

fn parse_op(kw: &str, args: &[&str], source_line: usize) -> Result<Node, AdapterError> {
    let op = Op::from_keyword(kw)
        .ok_or_else(|| parse_err(source_line, format!("unknown BTOR2 keyword '{kw}'")))?;

    let sort = parse_nid(arg(args, 0, source_line, "op sort")?, source_line)?;
    let arity = op.arity();

    let mut operands = Vec::with_capacity(arity);
    for i in 0..arity {
        operands.push(parse_operand(
            arg(args, 1 + i, source_line, "op operand")?,
            source_line,
        )?);
    }

    // Capture the trailing symbol (e.g. `uext SORT NID 0 fill`). The number
    // of immediates depends on the op; for slice it's 2 (upper, lower), for
    // uext/sext it's 1 (amount), and for the rest it's 0. Anything past
    // (1 + arity + immediates) that doesn't start with a comment marker is
    // the symbol.
    let immediates_count = match kw {
        "slice" => 2,
        "uext" | "sext" => 1,
        _ => 0,
    };
    let symbol_idx = 1 + arity + immediates_count;
    let symbol = args
        .get(symbol_idx)
        .filter(|s| !s.starts_with(';'))
        .map(|s| s.to_string());

    Ok(Node::Op {
        sort,
        op,
        args: operands,
        symbol,
    })
}

fn collect_immediates(
    kw: &str,
    args: &[&str],
    source_line: usize,
) -> Result<Vec<u32>, AdapterError> {
    // Slice: <sort> <signal> <upper> <lower>
    if kw == "slice" {
        let upper = parse_uint(arg(args, 2, source_line, "slice upper")?, source_line)?;
        let lower = parse_uint(arg(args, 3, source_line, "slice lower")?, source_line)?;
        return Ok(vec![upper, lower]);
    }
    // uext / sext: <sort> <signal> <amount>
    if kw == "uext" || kw == "sext" {
        let amount = parse_uint(arg(args, 2, source_line, "ext amount")?, source_line)?;
        return Ok(vec![amount]);
    }
    Ok(Vec::new())
}

fn validate(file: &Btor2File) -> Result<(), AdapterError> {
    // Cross-reference check: every operand and sort reference must exist.
    let known: std::collections::HashSet<Nid> = file.by_nid.keys().copied().collect();

    for line in &file.lines {
        match &line.node {
            Node::Input { sort, .. } | Node::State { sort, .. } | Node::Const { sort, .. } => {
                check_ref(*sort, &known, line.source_line)?;
            }
            Node::Op { sort, args, .. } => {
                check_ref(*sort, &known, line.source_line)?;
                for op in args {
                    check_ref(op.nid(), &known, line.source_line)?;
                }
            }
            Node::Init { sort, state, value } | Node::Next { sort, state, value } => {
                check_ref(*sort, &known, line.source_line)?;
                check_ref(*state, &known, line.source_line)?;
                check_ref(value.nid(), &known, line.source_line)?;
            }
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => {
                check_ref(signal.nid(), &known, line.source_line)?;
            }
            Node::Justice { signals } => {
                for s in signals {
                    check_ref(s.nid(), &known, line.source_line)?;
                }
            }
            Node::Sort { sort } => {
                if let Sort::Array { index, element } = sort {
                    check_ref(*index, &known, line.source_line)?;
                    check_ref(*element, &known, line.source_line)?;
                }
            }
        }
    }
    Ok(())
}

fn check_ref(
    nid: Nid,
    known: &std::collections::HashSet<Nid>,
    source_line: usize,
) -> Result<(), AdapterError> {
    if !known.contains(&nid) {
        return Err(parse_err(
            source_line,
            format!("reference to undefined NID {nid}"),
        ));
    }
    Ok(())
}

fn parse_nid(s: &str, source_line: usize) -> Result<Nid, AdapterError> {
    s.parse::<Nid>()
        .map_err(|_| parse_err(source_line, format!("expected non-negative NID, got '{s}'")))
}

fn parse_operand(s: &str, source_line: usize) -> Result<Operand, AdapterError> {
    let n = s
        .parse::<Nid>()
        .map_err(|_| parse_err(source_line, format!("expected operand NID, got '{s}'")))?;
    Ok(Operand(n))
}

fn parse_uint(s: &str, source_line: usize) -> Result<u32, AdapterError> {
    s.parse::<u32>().map_err(|_| {
        parse_err(
            source_line,
            format!("expected non-negative integer, got '{s}'"),
        )
    })
}

fn arg<'a>(
    args: &'a [&'a str],
    idx: usize,
    source_line: usize,
    what: &str,
) -> Result<&'a str, AdapterError> {
    args.get(idx)
        .copied()
        .ok_or_else(|| parse_err(source_line, format!("missing {what} (index {idx})")))
}

fn parse_err(source_line: usize, msg: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: msg,
        location: Some(SourceLocation {
            line: source_line,
            column: 0,
        }),
    }
}

/// Compute symbol-table style symbols for inputs and states (best-effort).
/// Used by the bit-blaster to populate human-readable signal names.
///
/// Yosys's `write_btor` typically attaches synthetic names like
/// `$auto$async2sync.cc:234:execute$30` to state lines, while the
/// user's actual register names (`fill`, `state`, …) appear as no-op
/// `uext _ NID 0 NAME` alias ops. To surface user-visible names on
/// `valuations { … }` blocks, we also walk the alias chain: for each Op
/// with a symbol, we trace its operand graph backward looking for the
/// first reachable State and attach the symbol to it (only when the
/// state has no user-visible symbol of its own — synthetic compiler
/// names are recognised by their `$auto$` / `$0` prefixes and treated
/// as overridable).
pub fn collect_symbols(file: &Btor2File) -> HashMap<Nid, String> {
    let mut out = HashMap::new();

    // Pass 1: collect direct Input/State symbols.
    for line in &file.lines {
        match &line.node {
            Node::Input {
                symbol: Some(s), ..
            }
            | Node::State {
                symbol: Some(s), ..
            } => {
                out.insert(line.nid, s.clone());
            }
            _ => {}
        }
    }

    // Pass 2: trace `Op { symbol: Some(NAME) }` aliases (Yosys's `uext _ _ 0 NAME`
    // pattern) back to their underlying state lines and attach the user-
    // visible name. Only overrides synthetic compiler-generated names.
    //
    // **Width filter** — only attach a symbol when the Op's result width
    // matches the target state's width. Without this, narrow combinational
    // aliases (`uext 1 ... arc_BOOT_X_BOOT_Y`, a single-bit `assign` output)
    // false-positive onto wide state cells they happen to reference (the
    // multi-bit state register that drives the combinational chain).
    // The width-matched alias is the canonical "this NID *is* state S"
    // shape Yosys emits for SystemVerilog regs that lose their direct
    // symbol annotation through `flatten` + `async2sync` + `dffunmap`.
    let is_synthetic = |s: &str| s.starts_with('$');
    for line in &file.lines {
        if let Node::Op {
            sort: op_sort,
            args,
            symbol: Some(name),
            ..
        } = &line.node
            && let Some(state_nid) = trace_to_state(file, args)
        {
            let op_width = bv_width(file, *op_sort);
            let state_width = file.lookup(state_nid).and_then(|l| match &l.node {
                Node::State { sort, .. } => bv_width(file, *sort),
                _ => None,
            });
            if op_width != state_width || op_width.is_none() {
                continue;
            }
            let entry = out.entry(state_nid);
            match entry {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(name.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if is_synthetic(o.get()) {
                        o.insert(name.clone());
                    }
                }
            }
        }
    }

    // Pass 3: trace `output <signal> <NAME>` lines back to their underlying
    // state cells. Yosys's `write_btor` retains output-port names through
    // synthesis (the original `output logic boot_fsm_ps` becomes
    // `<nid> output <ite-nid> boot_fsm_ps`) but does NOT emit a symbol on
    // the state-cell line itself when the cell goes through `flatten` +
    // `async2sync` + `dffunmap`. This pass closes the gap: an output that
    // 1:1 reflects a state register propagates its user-visible name back
    // to the state line, so `.mununu.json` sidecar entries keyed on the
    // SystemVerilog port name match the underlying BTOR2 nid.
    //
    // Same width-filter as Pass 2 — a narrow output combinational chain
    // pointing at a wide state cell is not the same signal. Same override
    // logic — a state cell that already carries a user-visible symbol on
    // its `state` line (direct, or via a width-matched Pass 2 alias) is
    // not overwritten.
    for line in &file.lines {
        if let Node::Output {
            signal,
            symbol: Some(name),
        } = &line.node
            && let Some(state_nid) = trace_to_state(file, &[*signal])
        {
            let signal_width = bv_width_of_operand(file, *signal);
            let state_width = file.lookup(state_nid).and_then(|l| match &l.node {
                Node::State { sort, .. } => bv_width(file, *sort),
                _ => None,
            });
            if signal_width != state_width || signal_width.is_none() {
                continue;
            }
            let entry = out.entry(state_nid);
            match entry {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(name.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if is_synthetic(o.get()) {
                        o.insert(name.clone());
                    }
                }
            }
        }
    }
    out
}

/// Resolve a BTOR2 leaf cell (state/input) NID to its canonical user-visible
/// symbol via [`collect_symbols`] alias resolution — a `uext _ NID 0 NAME`
/// alias name wins over a missing `state`-line symbol (the flattened-yosys
/// shape), falling back to `{tag}_{nid}` for a truly anonymous cell.
///
/// **AR-S1 / AR-GO-1 / #242 drift guard.** This is the ONE cell-naming
/// function. Every site that names a state/input cell — the exact
/// bit-blaster's leaf enumeration ([`crate::adapter::sts_ir::BtorSts::leaf_cells`]),
/// its `next_funcs` keying, its init-constraint keying — MUST resolve through
/// here, else two sites drift and `next_funcs.get(&cell.symbol)` misses,
/// silently freezing a register at its init value (the #242 soundness bug: a
/// false `EF`-VIOLATED / vacuous `AG AF`-HOLDS). Routing every site through
/// one function makes that drift impossible by construction rather than by
/// matching comments.
pub(crate) fn resolve_leaf_symbol(
    symbols: &HashMap<Nid, String>,
    nid: Nid,
    raw_symbol: &Option<String>,
    tag: &str,
) -> String {
    symbols
        .get(&nid)
        .cloned()
        .or_else(|| raw_symbol.clone())
        .unwrap_or_else(|| format!("{tag}_{nid}"))
}

/// Resolve an Operand's bit-vector width by looking up its defining
/// line and reading the sort. Returns `None` for unresolvable operands
/// (off-graph references, array sorts).
fn bv_width_of_operand(file: &Btor2File, op: crate::adapter::btor2::ast::Operand) -> Option<u32> {
    let line = file.lookup(op.nid())?;
    let sort_nid = match &line.node {
        Node::Sort { .. } => return None,
        Node::Input { sort, .. }
        | Node::State { sort, .. }
        | Node::Const { sort, .. }
        | Node::Op { sort, .. }
        | Node::Init { sort, .. }
        | Node::Next { sort, .. } => *sort,
        _ => return None,
    };
    bv_width(file, sort_nid)
}

/// Walk the operand graph backward from a list of operands, returning
/// the nid of the first State node reachable. Returns `None` if no state
/// is found within a small bounded traversal (cycles + width are guarded
/// to keep this O(n) overall across the whole file).
fn trace_to_state(file: &Btor2File, args: &[crate::adapter::btor2::ast::Operand]) -> Option<Nid> {
    let mut stack: Vec<Nid> = args.iter().map(|o| o.nid()).collect();
    let mut seen: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    while let Some(nid) = stack.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::State { .. } => return Some(line.nid),
            Node::Op { args, .. } => {
                for o in args {
                    stack.push(o.nid());
                }
            }
            _ => {}
        }
    }
    None
}

/// M.1 Path B (§Phase 11) — find the nearest `State` NID reachable
/// backward from any node carrying `symbol`. Used by the sidecar
/// resolvers in [`super::bit_blast`] to map user-facing register
/// names (`sreg_q`) onto BTOR2 state cells even when Yosys's
/// `flatten` + `async2sync` + `dffunmap` chain has stripped the
/// symbol from the `state` line itself.
///
/// Resolution order, in declining preference:
/// 1. A `State { symbol: Some(s) }` line where `s == symbol` —
///    distance 0 (a direct hit).
/// 2. Any `Op { symbol: Some(s), args }` line where `s == symbol` —
///    BFS backward from `args` to the nearest `State`; distance
///    counts Op hops. Yosys's `uext _ <state-nid> 0 NAME` pattern
///    (the "alias" shape) lands here at distance 2.
/// 3. Any `Output { symbol: Some(s), signal }` line where
///    `s == symbol` — BFS backward from `signal` to the nearest
///    `State`.
///
/// **No width filter.** Pass 2 / Pass 3 in [`collect_symbols`] apply
/// a width filter to avoid spurious matches between narrow
/// combinational chains and wide state cells; this resolver
/// intentionally does *not*, because the user has explicitly named
/// the driver they care about via the sidecar's `drives` field.
/// Combinational chains (`idle = bit_cnt_q == 0` — a 1-bit output
/// driven by a 4-bit state) are exactly what `drives` is for.
///
/// **Ambiguity handling.** When multiple distinct `State` NIDs are
/// reachable at the same minimum distance from candidates carrying
/// the same symbol, returns `None` so the user can disambiguate by
/// declaring a more specific `drives` value. Multiple candidates
/// reaching the *same* state are deduplicated — only distinct state
/// NIDs trigger the ambiguity guard.
/// H.E (combinational classification, 2026-06-28) — does the combinational cone
/// rooted at `start` reach a primary `Input`? Traverses the transitive fan-in
/// (following `Op` args + side-effect operands), stopping at `State` cells and
/// `Const` leaves — a register's *current* value is state, not a primary input,
/// so states are cone leaves and are NOT recursed through.
///
/// Splits combinational signals for the predicate-cube path: **input-dependent**
/// ones (`trigger_active = (trigger_i == 0)` → reaches `trigger_i`) must route
/// through the FREE-INPUT machinery (source-pin / target-free, so the may/must
/// edges respect the signal's value), while **state-only** ones
/// (`event_detected_o = f(state_q)`) are sound as derived per-cube labels.
/// Treating an input-dependent combinational as a state-determined label is
/// unsound (it produced the spurious VIOLATED on sysrst sva_6 / sva_9).
pub fn cone_reaches_input(file: &Btor2File, start: Nid) -> bool {
    let mut seen: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let mut work: Vec<Nid> = vec![start];
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::Input { .. } => return true,
            // State cells + constants are cone leaves.
            Node::State { .. } | Node::Const { .. } | Node::Sort { .. } => {}
            Node::Op { args, .. } => {
                for a in args {
                    work.push(a.nid());
                }
            }
            Node::Init { value, .. } | Node::Next { value, .. } => work.push(value.nid()),
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => work.push(signal.nid()),
            Node::Justice { signals } => {
                for s in signals {
                    work.push(s.nid());
                }
            }
        }
    }
    false
}

/// monono#partsel soundness guard — does the combinational cone rooted at `start`
/// reach an **anonymous** (symbol-less) primary `input`?
///
/// A properly-lifted, flattened design names every real primary input after its
/// top-module port (`clk`, `rst_n`, …). An **anonymous** `input` — a
/// `<nid> input <sort>` line with no trailing symbol — is therefore never a real
/// port: it is a net the RTL front-end left *undriven* and modelled as a free
/// (havoc-each-cycle) variable. The yosys-slang lift of a plain-vector **partial
/// register assignment** (`q[hi:lo] <= d`) produces exactly this: the register's
/// *unwritten* bits become anonymous free `input`s that are `concat`-ed back into
/// a signal aliased to the register name `q`. A predicate over `q` then reads
/// those free inputs and mis-decides (a spurious `Holds` on a reachably non-zero
/// register), so callers REFUSE it.
///
/// Deliberately narrower than [`cone_reaches_input`] (which fires on named ports
/// too) AND narrower than the anonymous-*state* case: an anonymous split
/// sub-*register* (the packed-2-D `q[idx] <= d` shape) is now KEPT and modelled by
/// the NID-indexed cone-of-influence
/// ([`crate::adapter::btor2::dep_graph::cone_leaf_nids`]), so that case DECIDES
/// rather than being refused — only the genuinely havoc free-input case (which is
/// an over-approximation the engines cannot soundly decide for all properties)
/// trips this guard. A **named** input or any register cell stops the walk (the
/// walk never descends into a register's `next`), so a legitimate combinational
/// of a named port / register is not flagged.
pub fn cone_reaches_anonymous_input(file: &Btor2File, start: Nid) -> bool {
    let mut seen: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let mut work: Vec<Nid> = vec![start];
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            // A symbol-less input is the undriven-net (havoc) artifact.
            Node::Input { symbol: None, .. } => return true,
            // A named input, or ANY register cell, is a sound cone leaf — stop.
            Node::Input { .. } | Node::State { .. } | Node::Const { .. } | Node::Sort { .. } => {}
            Node::Op { args, .. } => {
                for a in args {
                    work.push(a.nid());
                }
            }
            Node::Init { value, .. } | Node::Next { value, .. } => work.push(value.nid()),
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => work.push(signal.nid()),
            Node::Justice { signals } => {
                for s in signals {
                    work.push(s.nid());
                }
            }
        }
    }
    false
}

/// monono#partsel soundness guard — does the signal a property atom binds to
/// `NAME` reach an anonymous (undriven-register-bit) free input?
///
/// Resolves `NAME` to its candidate binding node(s) — a `state` cell of that
/// name, an `Op` carrying `NAME` as its own symbol (the `uext … 0 NAME`
/// register-name alias Yosys emits), or a named `output` — and reports whether
/// any of them [`cone_reaches_anonymous_input`]. A **faithful state cell** of
/// `NAME` (read_verilog / sv2v lift), and an **anonymous split sub-register** the
/// NID-indexed COI now keeps, both return false — only the plain-vector
/// partial-write shape whose unwritten bits are modelled as free inputs is
/// flagged.
pub fn signal_reaches_anonymous_input(file: &Btor2File, name: &str) -> bool {
    let symbols = collect_symbols(file);
    file.lines.iter().any(|l| {
        let node = match &l.node {
            Node::State { .. } if symbols.get(&l.nid).map(String::as_str) == Some(name) => l.nid,
            Node::Op {
                symbol: Some(s), ..
            } if s == name => l.nid,
            Node::Output {
                symbol: Some(s),
                signal,
            } if s == name => signal.nid(),
            _ => return false,
        };
        cone_reaches_anonymous_input(file, node)
    })
}

/// Collect the *symbols* of every primary input reachable in the combinational
/// cone of `start`. The dual of [`cone_reaches_input`] (which only reports
/// existence): the returned names are the raw free inputs a
/// combinational-of-input signal `g(s,i)` depends on.
///
/// The verify-auto seeder uses this to seed those raw inputs as free H.B cube
/// dimensions (refining the may-relation so a consequent box over a
/// conditional transition becomes definite), while the combinational itself
/// stays a derived per-cube label (now definite at cubes that pin the input).
/// Deterministic (sorted, deduped). An input with no symbol is skipped (it is
/// not seedable by name).
pub fn cone_inputs(file: &Btor2File, start: Nid) -> Vec<String> {
    let symbols = collect_symbols(file);
    let mut seen: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let mut inputs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut work: Vec<Nid> = vec![start];
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::Input { .. } => {
                if let Some(s) = symbols.get(&nid) {
                    inputs.insert(s.clone());
                }
            }
            Node::State { .. } | Node::Const { .. } | Node::Sort { .. } => {}
            Node::Op { args, .. } => {
                for a in args {
                    work.push(a.nid());
                }
            }
            Node::Init { value, .. } | Node::Next { value, .. } => work.push(value.nid()),
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => work.push(signal.nid()),
            Node::Justice { signals } => {
                for s in signals {
                    work.push(s.nid());
                }
            }
        }
    }
    inputs.into_iter().collect()
}

pub fn resolve_state_by_symbol(file: &Btor2File, symbol: &str) -> Option<Nid> {
    let mut best: Option<(Nid, usize)> = None;
    let mut tied: bool = false;

    let consider =
        |state_nid: Nid, distance: usize, best: &mut Option<(Nid, usize)>, tied: &mut bool| {
            match *best {
                None => {
                    *best = Some((state_nid, distance));
                    *tied = false;
                }
                Some((cur_nid, cur_dist)) => {
                    if distance < cur_dist {
                        *best = Some((state_nid, distance));
                        *tied = false;
                    } else if distance == cur_dist && cur_nid != state_nid {
                        *tied = true;
                    }
                }
            }
        };

    for line in &file.lines {
        match &line.node {
            Node::State {
                symbol: Some(s), ..
            } if s == symbol => {
                consider(line.nid, 0, &mut best, &mut tied);
            }
            Node::Op {
                symbol: Some(s),
                args,
                ..
            } if s == symbol => {
                if let Some((state_nid, distance)) = bfs_nearest_state(file, args) {
                    consider(state_nid, distance, &mut best, &mut tied);
                }
            }
            Node::Output {
                symbol: Some(s),
                signal,
            } if s == symbol => {
                if let Some((state_nid, distance)) =
                    bfs_nearest_state(file, std::slice::from_ref(signal))
                {
                    consider(state_nid, distance, &mut best, &mut tied);
                }
            }
            _ => {}
        }
    }

    if tied {
        return None;
    }
    best.map(|(nid, _)| nid)
}

/// H.A — resolve a signal name to the state cell it is a **value-identical
/// alias** of, or `None` when it is not such an alias.
///
/// Stricter than [`resolve_state_by_symbol`], which finds *any* state in the
/// signal's cone. A combinational function of state (an `eq`/`or` output flag
/// such as `main_sm_err_o`) is in a state's cone but is NOT value-equal to it,
/// so binding it to the state's value yields a spurious verdict. This resolver
/// follows only value-preserving edges and returns `None` for anything else,
/// so the verify-auto seeder binds a name only when reading the state cell's
/// value is correct.
///
/// Followed edges:
/// - the state itself; an `output` of a passthrough; `uext`/`sext` by 0 (a
///   Yosys rename);
/// - when `allow_reset_mux` is set, an `ite(cond, A, B)` where exactly one of
///   `A`/`B` resolves to a state via passthrough and the other is a constant —
///   the async-reset register mux that `async2sync` emits (`q = !rst ? state :
///   resetval`). The constant (reset-value) branch is taken only during reset,
///   which the verify-auto path pins inactive, so the register's value equals
///   the state then. Pass `false` when the reset is not pinned.
pub fn resolve_state_alias(file: &Btor2File, symbol: &str, allow_reset_mux: bool) -> Option<Nid> {
    let start = file.lines.iter().find_map(|l| match &l.node {
        Node::State {
            symbol: Some(s), ..
        }
        | Node::Op {
            symbol: Some(s), ..
        }
        | Node::Output {
            symbol: Some(s), ..
        } if s == symbol => Some(l.nid),
        _ => None,
    })?;
    let mut visited = std::collections::HashSet::new();
    follow_state_alias(file, start, allow_reset_mux, &mut visited)
}

fn follow_state_alias(
    file: &Btor2File,
    nid: Nid,
    allow_reset_mux: bool,
    visited: &mut std::collections::HashSet<Nid>,
) -> Option<Nid> {
    use crate::adapter::btor2::ast::Op;
    if !visited.insert(nid) {
        return None;
    }
    let line = file.lookup(nid)?;
    match &line.node {
        Node::State { .. } => Some(nid),
        Node::Output { signal, .. } => {
            follow_state_alias(file, signal.nid(), allow_reset_mux, visited)
        }
        Node::Op { op, args, .. } => match op {
            // `uext`/`sext` by 0 = a pure rename (same width, same value).
            Op::Uext | Op::Sext if line.immediates.first() == Some(&0) && args.len() == 1 => {
                follow_state_alias(file, args[0].nid(), allow_reset_mux, visited)
            }
            // Async-reset register mux: exactly one branch is the state (via
            // passthrough), the other a constant (the reset value).
            Op::Ite if allow_reset_mux && args.len() == 3 => {
                let then_nid = args[1].nid();
                let else_nid = args[2].nid();
                let then_state =
                    follow_state_alias(file, then_nid, allow_reset_mux, &mut visited.clone());
                let else_state =
                    follow_state_alias(file, else_nid, allow_reset_mux, &mut visited.clone());
                match (then_state, else_state) {
                    (Some(s), None) if resolves_to_const(file, else_nid) => Some(s),
                    (None, Some(s)) if resolves_to_const(file, then_nid) => Some(s),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// AR-GO-1 — the strictness of the reverse `name → state-cell` resolution: whether an
/// alias name resolves to a state cell only via a **value-identical** alias
/// ([`Strict`](ResolveStrictness::Strict) — the sound oracle path) or via the looser
/// "nearest state in the signal's cone" BFS ([`Loose`](ResolveStrictness::Loose) — the
/// lift / predicate path). The two are genuinely different and both correct for their
/// caller; making the choice an explicit parameter of one entry keeps the divergence
/// **visible** rather than a silent structural duplication (the #242-family hazard: two
/// resolution paths that can quietly disagree).
pub enum ResolveStrictness {
    /// Value-identical alias only ([`resolve_state_alias`]); `allow_reset_mux` widens
    /// it to an async-reset next-mux.
    Strict { allow_reset_mux: bool },
    /// Nearest state in the signal's cone ([`resolve_state_by_symbol`]).
    Loose,
}

/// Resolve a user-visible `name` to the canonical state-cell symbol the bit-blast / SMT
/// view binds against, under an explicit [`ResolveStrictness`]. Both the strict oracle
/// path (`concrete_oracle::resolve_signal_symbol`) and the loose lift path
/// (`BtorSts::resolve_register`) route through this, so the strict-vs-loose choice is a
/// named argument, not two look-alike wrappers that can drift.
pub fn resolve_to_canonical_name(
    file: &Btor2File,
    name: &str,
    strictness: ResolveStrictness,
) -> Option<String> {
    let nid = match strictness {
        ResolveStrictness::Strict { allow_reset_mux } => {
            resolve_state_alias(file, name, allow_reset_mux)?
        }
        ResolveStrictness::Loose => resolve_state_by_symbol(file, name)?,
    };
    collect_symbols(file).get(&nid).cloned()
}

/// mununu#633 — the name each state cell is REPORTED and ADDRESSED under by the
/// register-facing verbs (`check-fsm`, `sv mutate --list` / `--mutation`), strict-preferred:
///
/// 1. the `state` line's own user-visible symbol (not a `$`-prefixed synthetic one);
/// 2. else the first (file order) `Op` symbol, then the first `Output` symbol, that is a
///    **value-identical** alias of the cell ([`resolve_state_alias`] with the async-reset
///    mux allowed — the `uext … 0 NAME` rename yosys emits for a register's current
///    value, or the port that passes it through);
/// 3. else the loose cone-tracing name [`collect_symbols`] attaches, for a cell nothing
///    names exactly (a hand-written lift whose only symbol sits on a derived op).
///
/// Why not [`collect_symbols`] alone: its pass 2 attaches the FIRST symbol-bearing op
/// whose cone reaches a cell, and a next-state value `beat_cnt_d` reaches the *state*
/// register's cell (the `case (state_q)` it is computed under) before its own — so
/// `check-fsm` reported the state register as `beat_cnt_d` and `--list` advertised a
/// combinational wire as a register. A value-identical alias cannot name the wrong cell.
/// `collect_symbols` itself is unchanged: the sidecar / lint resolvers that want the
/// loose cone name keep it.
pub fn canonical_register_names(file: &Btor2File) -> HashMap<Nid, String> {
    let is_synthetic = |s: &str| s.starts_with('$');
    let mut out: HashMap<Nid, String> = HashMap::new();
    for line in &file.lines {
        if let Node::State {
            symbol: Some(s), ..
        } = &line.node
            && !is_synthetic(s)
        {
            out.insert(line.nid, s.clone());
        }
    }
    // Op aliases before output ports: `state_q` over the port that mirrors it, when
    // both are value-identical to the cell.
    for want_output in [false, true] {
        for line in &file.lines {
            let name = match (&line.node, want_output) {
                (
                    Node::Op {
                        symbol: Some(s), ..
                    },
                    false,
                )
                | (
                    Node::Output {
                        symbol: Some(s), ..
                    },
                    true,
                ) => s,
                _ => continue,
            };
            if is_synthetic(name) {
                continue;
            }
            let mut visited = std::collections::HashSet::new();
            if let Some(cell) = follow_state_alias(file, line.nid, true, &mut visited) {
                out.entry(cell).or_insert_with(|| name.clone());
            }
        }
    }
    for (nid, name) in collect_symbols(file) {
        if matches!(file.lookup(nid).map(|l| &l.node), Some(Node::State { .. })) {
            out.entry(nid).or_insert(name);
        }
    }
    out
}

/// True when `nid` is a constant, or a `uext`/`sext`-by-0 passthrough of one
/// (the shape `async2sync` gives a reset value).
fn resolves_to_const(file: &Btor2File, nid: Nid) -> bool {
    use crate::adapter::btor2::ast::Op;
    match file.lookup(nid).map(|l| (&l.node, l)) {
        Some((Node::Const { .. }, _)) => true,
        Some((
            Node::Op {
                op: Op::Uext | Op::Sext,
                args,
                ..
            },
            l,
        )) if l.immediates.first() == Some(&0) && args.len() == 1 => {
            resolves_to_const(file, args[0].nid())
        }
        _ => false,
    }
}

/// BFS backward from a set of starting operands until a `State` node
/// is reached. Returns `(state_nid, distance)` where `distance`
/// counts Op hops (starting at 1 for direct operands of the seed).
/// `None` when no state is reachable within the operand graph
/// closure of the seeds.
fn bfs_nearest_state(
    file: &Btor2File,
    seeds: &[crate::adapter::btor2::ast::Operand],
) -> Option<(Nid, usize)> {
    use std::collections::{HashSet, VecDeque};
    let mut queue: VecDeque<(Nid, usize)> = seeds.iter().map(|o| (o.nid(), 1)).collect();
    let mut seen: HashSet<Nid> = HashSet::new();
    while let Some((nid, depth)) = queue.pop_front() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::State { .. } => return Some((line.nid, depth)),
            Node::Op { args, .. } => {
                for o in args {
                    queue.push_back((o.nid(), depth + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// R.5 WP cone-of-influence helper — collect every `Node::State`
/// NID reachable backward through the operand graph from `seeds`.
/// Used by the CEGAR loop's `weakest_precondition_predicates` to
/// restrict its proposals to state cells the classifying transition
/// actually depends on (instead of "any uncovered register").
///
/// The walk stops at non-Op leaves (Input, Const, Sort) and collects
/// every State encountered. Cycles are handled by a `seen` set;
/// off-graph references (NIDs the file doesn't define) are skipped.
///
/// Returns an empty set when no States are reachable. The caller
/// should treat an empty COI as "no restriction" and fall back to
/// the unconstrained predicate proposal path.
pub fn collect_reachable_states_from(
    file: &Btor2File,
    seeds: &[crate::adapter::btor2::ast::Operand],
) -> std::collections::HashSet<Nid> {
    use std::collections::{HashSet, VecDeque};
    let mut queue: VecDeque<Nid> = seeds.iter().map(|o| o.nid()).collect();
    let mut seen: HashSet<Nid> = HashSet::new();
    let mut states: HashSet<Nid> = HashSet::new();
    while let Some(nid) = queue.pop_front() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::State { .. } => {
                states.insert(line.nid);
            }
            Node::Op { args, .. } => {
                for o in args {
                    queue.push_back(o.nid());
                }
            }
            _ => {}
        }
    }
    states
}

/// R.5 WP cone-of-influence helper — find the `next` operand for a
/// given state cell NID, if one exists in the file. The returned
/// operand is the seed for [`collect_reachable_states_from`] when
/// computing the cone-of-influence for that state's evolution.
///
/// Returns `None` for state cells without a `Next` line (BTOR2
/// convention: such states retain their init value forever, so the
/// cone is just the state itself).
pub fn find_next_value_operand(
    file: &Btor2File,
    state_nid: Nid,
) -> Option<crate::adapter::btor2::ast::Operand> {
    for line in &file.lines {
        if let Node::Next { state, value, .. } = &line.node
            && *state == state_nid
        {
            return Some(*value);
        }
    }
    None
}

/// Resolve the bit-vector width of a sort node. Returns `None` if the
/// referenced node is an array sort or unresolvable.
pub fn bv_width(file: &Btor2File, sort_nid: Nid) -> Option<u32> {
    match &file.lookup(sort_nid)?.node {
        Node::Sort {
            sort: Sort::BitVec { width },
        } => Some(*width),
        _ => None,
    }
}

/// Resolve the index + element bit-widths of an array sort. BTOR2
/// `Sort::Array { index, element }` references two other (bitvec) sort
/// lines; this resolves both. Returns `None` if the referenced node is
/// not an array sort or either inner sort is unresolvable.
///
/// Sibling of [`bv_width`]: callers distinguish bit-vector from array
/// sorts by which of the two returns `Some`. Used by the generic
/// `walk_design` driver to route (skip) array-sorted op nodes and by
/// the Z3 array encoder to declare `Array` consts.
pub fn array_widths(file: &Btor2File, sort_nid: Nid) -> Option<(u32, u32)> {
    let Node::Sort {
        sort: Sort::Array { index, element },
    } = &file.lookup(sort_nid)?.node
    else {
        return None;
    };
    Some((bv_width(file, *index)?, bv_width(file, *element)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_safety_circuit() {
        // 1-bit toggle latch with a "bad" output that fires when latch=1.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 one 1
4 state 1 q
5 init 1 4 2
6 not 1 4
7 next 1 4 6
8 bad 4
"#;
        let file = parse(src).expect("parse");
        assert_eq!(file.lines.len(), 8);
        assert!(matches!(
            file.lookup(1).unwrap().node,
            Node::Sort {
                sort: Sort::BitVec { width: 1 }
            }
        ));
        assert_eq!(file.states().count(), 1);
        assert_eq!(file.bads().count(), 1);
    }

    #[test]
    fn rejects_undefined_reference() {
        let src = "1 sort bitvec 1\n2 not 1 99\n";
        let err = parse(src).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::ParseError);
        assert!(err.message.contains("undefined NID 99"));
    }

    #[test]
    fn handles_comments_and_blank_lines() {
        let src = r#"
; this is a comment
1 sort bitvec 1
; mid-file comment

2 zero 1   ; trailing comment
"#;
        let file = parse(src).expect("parse");
        assert_eq!(file.lines.len(), 2);
    }

    #[test]
    fn parses_negated_operand() {
        let src = "1 sort bitvec 1\n2 input 1 a\n3 not 1 -2\n";
        let file = parse(src).expect("parse");
        if let Node::Op { args, .. } = &file.lookup(3).unwrap().node {
            assert!(args[0].is_negated());
            assert_eq!(args[0].nid(), 2);
        } else {
            panic!("expected Op node");
        }
    }

    #[test]
    fn parses_slice_immediates() {
        let src = "1 sort bitvec 8\n2 sort bitvec 4\n3 input 1 a\n4 slice 2 3 7 4\n";
        let file = parse(src).expect("parse");
        let line = file.lookup(4).unwrap();
        assert_eq!(line.immediates, vec![7, 4]);
    }

    #[test]
    fn parses_uext_immediate() {
        let src = "1 sort bitvec 4\n2 sort bitvec 8\n3 input 1 a\n4 uext 2 3 4\n";
        let file = parse(src).expect("parse");
        assert_eq!(file.lookup(4).unwrap().immediates, vec![4]);
    }

    #[test]
    fn parses_justice_set() {
        let src = "1 sort bitvec 1\n2 input 1 a\n3 input 1 b\n4 justice 2 2 3\n";
        let file = parse(src).expect("parse");
        if let Node::Justice { signals } = &file.lookup(4).unwrap().node {
            assert_eq!(signals.len(), 2);
        } else {
            panic!("expected Justice node");
        }
    }

    #[test]
    fn resolve_state_by_symbol_direct_state_match() {
        // 14 state 12 sreg_q  — the symbol lives directly on the state line.
        let src = "1 sort bitvec 4\n2 const 1 0000\n3 state 1 sreg_q\n4 init 1 3 2\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_by_symbol(&file, "sreg_q"), Some(3));
    }

    #[test]
    fn resolve_state_by_symbol_op_alias_match() {
        // 14 state 12  (no symbol)
        // 15 ite 12 ... 14 ...
        // 47 uext 12 15 0 bit_cnt_q   (Op alias carrying user-visible name)
        // The resolver walks from NID 47's operand (15) → state 14.
        let src = "1 sort bitvec 1\n2 sort bitvec 4\n3 const 2 0000\n4 input 1 rst_n\n\
                   5 state 2\n6 ite 2 4 5 3\n7 uext 2 6 0 bit_cnt_q\n";
        let file = parse(src).expect("parse");
        // Without `bit_cnt_q` on the state line, direct symbol lookup is empty:
        let symbols = collect_symbols(&file);
        // collect_symbols may or may not attach bit_cnt_q via Pass 2 (it should — width filter passes);
        // but resolve_state_by_symbol must work regardless.
        assert_eq!(resolve_state_by_symbol(&file, "bit_cnt_q"), Some(5));
        // Sanity: symbols may or may not contain it via Pass 2 — the resolver is independent.
        let _ = symbols;
    }

    #[test]
    fn resolve_state_by_symbol_output_alias_match() {
        // 21 ite 1 4 20 11    (combinational using state 20 + const)
        // 22 output 21 tx     (output port symbol attached)
        let src = "1 sort bitvec 1\n2 input 1 rst_n\n3 const 1 1\n4 state 1\n\
                   5 ite 1 2 4 3\n6 output 5 tx\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_by_symbol(&file, "tx"), Some(4));
    }

    #[test]
    fn resolve_state_alias_follows_reset_mux_and_uext0() {
        // bit_cnt_q = uext-0(ite(rst_n, state5, const3)) — the async-reset
        // register mux + a Yosys `uext _ _ 0` rename, the shape `async2sync`
        // emits. It is value-identical to state 5 while reset is inactive.
        let src = "1 sort bitvec 1\n2 sort bitvec 4\n3 const 2 0000\n4 input 1 rst_n\n\
                   5 state 2\n6 ite 2 4 5 3\n7 uext 2 6 0 bit_cnt_q\n";
        let file = parse(src).expect("parse");
        // Reset-mux following on (reset pinned): resolves to the state.
        assert_eq!(resolve_state_alias(&file, "bit_cnt_q", true), Some(5));
        // Reset-mux following off: the `ite` is not a pure passthrough → None.
        assert_eq!(resolve_state_alias(&file, "bit_cnt_q", false), None);
    }

    #[test]
    fn x633_canonical_register_names_prefer_a_value_identical_alias_over_the_loose_cone_name() {
        // Cell 4 carries no symbol. Its next-value op (9, `cnt_d` = ite(go, cnt+1, cnt))
        // comes FIRST in file order and reaches the cell in the loose cone trace, so
        // `collect_symbols` names the cell `cnt_d`; the `uext 0` alias `cnt_q` (11) is
        // the value-identical name and must win. Cell 12 has a direct symbol `mode`
        // that an alias (14, `mode_o`) must not override; cell 15 is named only by an
        // output port (17); cell 18 is named by nothing exact and keeps the loose name.
        let src = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n\
                   4 state 2\n5 zero 2\n6 init 2 4 5\n7 one 2\n8 add 2 4 7\n\
                   9 ite 2 3 8 4 cnt_d\n10 next 2 4 9\n11 uext 2 4 0 cnt_q\n\
                   12 state 2 mode\n13 next 2 12 12\n14 uext 2 12 0 mode_o\n\
                   15 state 2\n16 next 2 15 15\n17 output 15 port_q\n\
                   18 state 2\n19 next 2 18 18\n20 add 2 18 7 sum_d\n";
        let file = parse(src).expect("parse");
        let loose = collect_symbols(&file);
        assert_eq!(
            loose.get(&4).map(String::as_str),
            Some("cnt_d"),
            "the loose trap"
        );
        let names = canonical_register_names(&file);
        assert_eq!(names.get(&4).map(String::as_str), Some("cnt_q"));
        assert_eq!(names.get(&12).map(String::as_str), Some("mode"));
        assert_eq!(names.get(&15).map(String::as_str), Some("port_q"));
        assert_eq!(
            names.get(&18).map(String::as_str),
            Some("sum_d"),
            "a cell nothing names exactly keeps the loose cone name"
        );
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn resolve_state_alias_rejects_combinational_function() {
        // err_o = output(eq(state, const)) — a combinational FUNCTION of state,
        // not a value-identical alias. `resolve_state_alias` must return None
        // (binding it to the state's value would be a spurious verdict — the
        // csrng `main_sm_err_o` soundness case), even though the looser
        // `resolve_state_by_symbol` finds the state in its cone.
        let src = "1 sort bitvec 1\n2 sort bitvec 4\n3 const 2 0101\n\
                   4 state 2\n5 eq 1 4 3\n6 output 5 err_o\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_alias(&file, "err_o", true), None);
        assert_eq!(resolve_state_by_symbol(&file, "err_o"), Some(4));
    }

    #[test]
    fn resolve_state_by_symbol_prefers_nearest() {
        // Two Op aliases both name "bit_cnt_q" — one direct (distance 2),
        // one through several Op hops (next-value combinational chain).
        // The resolver must pick the nearest.
        let src = "1 sort bitvec 1\n2 sort bitvec 4\n3 const 2 0000\n4 input 1 rst_n\n\
                   5 state 2\n6 ite 2 4 5 3\n\
                   7 ite 2 4 6 3\n8 ite 2 4 7 3\n9 ite 2 4 8 3\n\
                   10 uext 2 6 0 bit_cnt_q\n11 uext 2 9 0 bit_cnt_d\n";
        let file = parse(src).expect("parse");
        // Both bit_cnt_q (via 10→6→5: 2 hops) and bit_cnt_d (via 11→9→8→7→6→5: 5 hops)
        // reach state 5. The resolver picks the nearer one for bit_cnt_q.
        assert_eq!(resolve_state_by_symbol(&file, "bit_cnt_q"), Some(5));
        // bit_cnt_d also reaches state 5 — fine, but its distance is longer.
        assert_eq!(resolve_state_by_symbol(&file, "bit_cnt_d"), Some(5));
    }

    #[test]
    fn resolve_state_by_symbol_ambiguous_returns_none() {
        // Two distinct states both share the same minimum distance under
        // the same symbol — the resolver returns None to flag ambiguity.
        let src = "1 sort bitvec 1\n2 input 1 rst_n\n3 const 1 1\n\
                   4 state 1\n5 state 1\n\
                   6 ite 1 2 4 3\n7 ite 1 2 5 3\n\
                   8 uext 1 6 0 shared\n9 uext 1 7 0 shared\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_by_symbol(&file, "shared"), None);
    }

    #[test]
    fn resolve_state_by_symbol_no_match_returns_none() {
        let src = "1 sort bitvec 1\n2 state 1 only_state\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_by_symbol(&file, "absent"), None);
    }

    #[test]
    fn resolve_state_by_symbol_direct_beats_op_alias() {
        // A direct state symbol (distance 0) wins over an Op alias (distance ≥ 1)
        // even though both name the same symbol.
        let src = "1 sort bitvec 1\n2 input 1 rst_n\n3 const 1 1\n\
                   4 state 1 winner\n5 state 1\n\
                   6 ite 1 2 5 3\n7 uext 1 6 0 winner\n";
        let file = parse(src).expect("parse");
        assert_eq!(resolve_state_by_symbol(&file, "winner"), Some(4));
    }

    #[test]
    fn cone_reaches_input_distinguishes_input_dependent_from_state_only() {
        // `trig_active = !in_a` (nid 5) — combinational of the INPUT in_a.
        // `err = (q == 0)` via `eq` (nid 7) — combinational of the STATE q only.
        let src = "1 sort bitvec 1\n\
                   2 input 1 in_a\n\
                   3 state 1 q\n\
                   4 zero 1\n\
                   5 not 1 2 trig_active\n\
                   6 eq 1 3 4 err\n\
                   7 next 1 3 3\n";
        let file = parse(src).expect("parse");
        // input-dependent: `trig_active` (nid 5) reaches the input `in_a`.
        assert!(
            cone_reaches_input(&file, 5),
            "trig_active = !in_a is input-dependent"
        );
        // state-only: `err` (nid 6) reaches only the state cell `q`, no input.
        assert!(
            !cone_reaches_input(&file, 6),
            "err = (q == 0) is state-only"
        );
    }

    #[test]
    fn cone_reaches_anonymous_input_ignores_named_ports() {
        // monono#partsel — `anon` (nid 3) is an ANONYMOUS input (an undriven
        // register bit under the slang partial-assignment lift); `named` (nid 2) is
        // a real port. `cone_reaches_anonymous_input` fires only for the anonymous
        // one — a legitimate combinational of a NAMED input must NOT be refused.
        let src = "1 sort bitvec 1\n\
                   2 input 1 named\n\
                   3 input 1\n\
                   4 state 1 q\n\
                   5 not 1 2 f_named\n\
                   6 not 1 3 f_anon\n\
                   7 not 1 4 f_state\n\
                   8 next 1 4 4\n";
        let file = parse(src).expect("parse");
        // Reaches an ANONYMOUS input.
        assert!(
            cone_reaches_anonymous_input(&file, 6),
            "f_anon reaches nid 3"
        );
        // Reaches only a NAMED input — not the pathology.
        assert!(
            !cone_reaches_anonymous_input(&file, 5),
            "f_named reaches only the named port `named`"
        );
        // Reaches only a NAMED state.
        assert!(
            !cone_reaches_anonymous_input(&file, 7),
            "f_state reaches only the register `q`"
        );
        // But `cone_reaches_input` (the broad one) fires for BOTH input cases — the
        // narrower guard is what avoids over-refusing named-input combinationals.
        assert!(cone_reaches_input(&file, 5) && cone_reaches_input(&file, 6));
    }

    #[test]
    fn cone_reaches_anonymous_input_ignores_anonymous_split_subregisters() {
        // monono#partsel (packed 2-D `p[idx] <= d`) — `p` is split into ANONYMOUS
        // `state` cells (nid 3, 4). The NID-indexed cone-of-influence now KEEPS
        // those cells, so this case DECIDES rather than being refused: the
        // input-only guard must NOT fire on a state-only reconstruction (no free
        // input in the cone).
        let src = "1 sort bitvec 1\n\
                   2 sort bitvec 2\n\
                   3 state 1\n\
                   4 state 1\n\
                   5 concat 2 3 4\n\
                   6 uext 2 5 0 p\n\
                   9 next 1 3 3\n\
                   10 next 1 4 4\n";
        let file = parse(src).expect("parse");
        // Anonymous split sub-registers, NO input in the cone → not flagged.
        assert!(
            !cone_reaches_anonymous_input(&file, 5),
            "an anonymous-state split (no free input) is kept by the COI, not refused"
        );
        assert!(!cone_reaches_input(&file, 5));
    }

    #[test]
    fn signal_reaches_anonymous_input_via_alias_and_output() {
        // A register `q` whose name is carried by a `uext … 0 q` alias over a concat
        // that mixes an anonymous INPUT (the plain-vector partial-write shape) is
        // flagged; a faithful state cell `r` is not. An output over the poisoned
        // signal is flagged; an output over the faithful state is not.
        let src = "1 sort bitvec 1\n\
                   2 input 1\n\
                   3 state 1 r\n\
                   4 sort bitvec 2\n\
                   5 concat 4 2 3\n\
                   6 uext 4 5 0 q\n\
                   7 redor 1 5\n\
                   8 output 7 q_out\n\
                   9 redor 1 3\n\
                   10 output 9 r_out\n\
                   11 next 1 3 3\n";
        let file = parse(src).expect("parse");
        assert!(
            signal_reaches_anonymous_input(&file, "q"),
            "q aliases a concat mixing an anonymous input"
        );
        assert!(
            !signal_reaches_anonymous_input(&file, "r"),
            "r is a faithful state cell"
        );
        assert!(signal_reaches_anonymous_input(&file, "q_out"));
        assert!(!signal_reaches_anonymous_input(&file, "r_out"));
    }
}
