//! The flattened netlist as a circuit of values (2026-10-30): yosys's `write_json` after
//! `flatten; opt_clean` — the same artifact the source map reads (`cell_sources`,
//! `trace_aliases`) — parsed into cells with their `src` spans, evaluated on one cycle's
//! `(state, inputs)`, and walked backwards along a counterexample to the statements that decided
//! it: the **trace-relative slice** (path slicing, Jhala & Majumdar, PLDI 2005: keep what lay on
//! the concrete path and controlled its outcome; drop the branches not taken and the operands that
//! did not matter).
//!
//! Display only — no verdict is read or changed. The evaluation is exact where it evaluates
//! (`simlib.v` semantics for the cell types below) and **widens** where it does not: a cell type
//! this module does not know keeps its whole fan-in in the slice and is named in
//! [`SliceReport::widened`], so the slice is never smaller than the truth. Cross-checked against
//! the engine's own one-step evaluator on the lifted BTOR2 (`bit_blast::simulate_one_step`) in
//! the tests: every flop's `D` at cycle `t` equals the trace's state at `t+1`, and the engine's
//! `next_state`.
//!
//! Why the netlist and not the BTOR2: BTOR2 nodes carry no source location, netlist cells carry a
//! `src` statement span on every front end that stamps them (yosys-slang, `read_verilog`); the two
//! are the same design (`write_btor` runs on this netlist after `async2sync`/`dffunmap`), which is
//! what the cross-check pins.

use crate::trace::SourceLoc;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// One bit of a connection: a net bit by id, or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bit {
    Id(u64),
    Zero,
    One,
    /// `x` / `z`: yosys's "don't care" (the default of a `full_case` mux chain).
    Undef,
}

/// One cell of the top module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub name: String,
    /// yosys's internal type (`$mux`, `$eq`, `$aldff`, …).
    pub ty: String,
    /// Every span of the cell's `src` attribute, in order (yosys joins the spans of merged cells
    /// with `|`; a `proc`-generated mux carries the statement first and the enclosing block
    /// second).
    pub src: Vec<SourceLoc>,
    /// Integer parameters (`WIDTH`, `A_SIGNED`, …).
    pub params: BTreeMap<String, u64>,
    /// Port → bits, LSB first.
    pub conns: BTreeMap<String, Vec<Bit>>,
}

/// yosys's storage cells: `Q` is the register's value, `D` what it takes at the next edge.
pub const FLOPS: [&str; 14] = [
    "$dff",
    "$dffe",
    "$adff",
    "$adffe",
    "$sdff",
    "$sdffe",
    "$sdffce",
    "$aldff",
    "$aldffe",
    "$dffsr",
    "$dffsre",
    "$dlatch",
    "$adlatch",
    "$dlatchsr",
];

/// One cycle of a trace as the slice evaluates it: the registers' values by name, the inputs'
/// (the pinned ones included) by name.
pub type Step = (HashMap<String, u128>, HashMap<String, u128>);

/// The top module of a flattened netlist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Netlist {
    pub cells: Vec<Cell>,
    /// Every public net (not `$`-named, not hidden) → its bits, LSB first.
    pub nets: BTreeMap<String, Vec<Bit>>,
    pub inputs: BTreeSet<String>,
    pub outputs: BTreeSet<String>,
    /// The public net(s) a storage cell drives: register name → its `Q` bits.
    pub registers: BTreeMap<String, Vec<Bit>>,
    /// Bit id → the cell driving it, the output port, the index within that port.
    driver: HashMap<u64, (usize, String, usize)>,
}

impl Netlist {
    /// Parse yosys's `write_json` output: the module flagged `top` (or the only one).
    pub fn parse(flat_netlist_json: &str) -> Result<Netlist, String> {
        let json: serde_json::Value =
            serde_json::from_str(flat_netlist_json).map_err(|e| format!("not JSON: {e}"))?;
        let modules = json
            .get("modules")
            .and_then(|m| m.as_object())
            .ok_or("no `modules`")?;
        let top = modules
            .values()
            .find(|m| m.get("attributes").and_then(|a| a.get("top")).is_some())
            .or_else(|| {
                (modules.len() == 1)
                    .then(|| modules.values().next())
                    .flatten()
            })
            .ok_or("no top module")?;
        let mut out = Netlist::default();
        let bits_of = |v: &serde_json::Value| -> Vec<Bit> {
            v.as_array()
                .into_iter()
                .flatten()
                .map(|b| match b {
                    serde_json::Value::Number(n) => n.as_u64().map_or(Bit::Undef, Bit::Id),
                    serde_json::Value::String(s) => match s.as_str() {
                        "0" => Bit::Zero,
                        "1" => Bit::One,
                        _ => Bit::Undef,
                    },
                    _ => Bit::Undef,
                })
                .collect()
        };
        if let Some(ports) = top.get("ports").and_then(|p| p.as_object()) {
            for (name, port) in ports {
                match port.get("direction").and_then(|d| d.as_str()) {
                    Some("input") => {
                        out.inputs.insert(name.clone());
                    }
                    Some("output") => {
                        out.outputs.insert(name.clone());
                    }
                    _ => {}
                }
            }
        }
        if let Some(nets) = top.get("netnames").and_then(|n| n.as_object()) {
            for (name, net) in nets {
                let hidden = net.get("hide_name").and_then(|h| h.as_i64()) == Some(1);
                if name.starts_with('$') || hidden {
                    continue;
                }
                out.nets.insert(
                    name.clone(),
                    bits_of(net.get("bits").unwrap_or(&serde_json::Value::Null)),
                );
            }
        }
        if let Some(cells) = top.get("cells").and_then(|c| c.as_object()) {
            for (name, cell) in cells {
                let ty = cell
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let src: Vec<SourceLoc> = cell
                    .get("attributes")
                    .and_then(|a| a.get("src"))
                    .and_then(|s| s.as_str())
                    .map(|s| {
                        s.split('|')
                            .filter_map(SourceLoc::parse_yosys_src)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut params = BTreeMap::new();
                for (k, v) in cell
                    .get("parameters")
                    .and_then(|p| p.as_object())
                    .into_iter()
                    .flatten()
                {
                    let n = match v {
                        serde_json::Value::Number(n) => n.as_u64(),
                        serde_json::Value::String(s) if s.chars().all(|c| c == '0' || c == '1') => {
                            let tail = &s[s.len().saturating_sub(64)..];
                            u64::from_str_radix(tail, 2).ok()
                        }
                        _ => None,
                    };
                    if let Some(n) = n {
                        params.insert(k.clone(), n);
                    }
                }
                let mut conns = BTreeMap::new();
                for (port, v) in cell
                    .get("connections")
                    .and_then(|c| c.as_object())
                    .into_iter()
                    .flatten()
                {
                    conns.insert(port.clone(), bits_of(v));
                }
                let dirs = cell.get("port_directions").and_then(|d| d.as_object());
                let idx = out.cells.len();
                for (port, bits) in &conns {
                    let is_output = dirs
                        .and_then(|d| d.get(port))
                        .and_then(|d| d.as_str())
                        .map(|d| d == "output")
                        .unwrap_or(port == "Y" || port == "Q");
                    if !is_output {
                        continue;
                    }
                    for (i, b) in bits.iter().enumerate() {
                        if let Bit::Id(id) = b {
                            out.driver.insert(*id, (idx, port.clone(), i));
                        }
                    }
                }
                out.cells.push(Cell {
                    name: name.clone(),
                    ty,
                    src,
                    params,
                    conns,
                });
            }
        }
        // The registers: a public net whose bits are a storage cell's `Q`.
        let q_bits: HashSet<u64> = out
            .cells
            .iter()
            .filter(|c| FLOPS.contains(&c.ty.as_str()))
            .flat_map(|c| c.conns.get("Q").into_iter().flatten())
            .filter_map(|b| match b {
                Bit::Id(id) => Some(*id),
                _ => None,
            })
            .collect();
        for (name, bits) in &out.nets {
            if !bits.is_empty()
                && bits
                    .iter()
                    .all(|b| matches!(b, Bit::Id(id) if q_bits.contains(id)))
            {
                out.registers.insert(name.clone(), bits.clone());
            }
        }
        Ok(out)
    }

    /// Rename a source file in every span (the lift stages the primary as `work.sv`; the report
    /// names it as the user did — `yosys::name_staged_primary` does the same for the legend).
    pub fn rename_file(&mut self, from: &str, to: &str) {
        for c in &mut self.cells {
            for l in &mut c.src {
                if l.file == from {
                    l.file = to.to_string();
                }
            }
        }
    }

    /// Evaluate every cell of the netlist on one cycle's values: the registers' `Q` from `state`
    /// (by register net name), the input ports from `inputs` (by port name). A value the maps do
    /// not give, or that depends on a cell type this module does not evaluate, stays unknown.
    pub fn evaluate(
        &self,
        state: &HashMap<String, u128>,
        inputs: &HashMap<String, u128>,
    ) -> Evaluation {
        let mut ev = Evaluation::default();
        let mut seed = |bits: &[Bit], value: u128| {
            for (i, b) in bits.iter().enumerate() {
                if let Bit::Id(id) = b {
                    ev.bits.insert(*id, (value >> i) & 1 == 1);
                }
            }
        };
        for (name, bits) in &self.registers {
            if let Some(v) = state.get(name) {
                seed(bits, *v);
            }
        }
        for name in &self.inputs {
            if let (Some(v), Some(bits)) = (inputs.get(name), self.nets.get(name)) {
                seed(bits, *v);
            }
        }
        let mut visiting = HashSet::new();
        for idx in 0..self.cells.len() {
            self.evaluate_cell(idx, &mut ev, &mut visiting);
        }
        ev
    }

    fn bit(&self, b: Bit, ev: &mut Evaluation, visiting: &mut HashSet<usize>) -> Option<bool> {
        match b {
            Bit::Zero => Some(false),
            Bit::One => Some(true),
            Bit::Undef => None,
            Bit::Id(id) => {
                if let Some(v) = ev.bits.get(&id) {
                    return Some(*v);
                }
                let (idx, _, _) = *self.driver.get(&id)?;
                self.evaluate_cell(idx, ev, visiting);
                ev.bits.get(&id).copied()
            }
        }
    }

    fn word(
        &self,
        bits: &[Bit],
        signed: bool,
        ev: &mut Evaluation,
        visiting: &mut HashSet<usize>,
    ) -> Option<i128> {
        if bits.is_empty() || bits.len() > 127 {
            return None;
        }
        let mut v: i128 = 0;
        for (i, b) in bits.iter().enumerate() {
            if self.bit(*b, ev, visiting)? {
                v |= 1i128 << i;
            }
        }
        if signed && bits.len() < 128 && (v >> (bits.len() - 1)) & 1 == 1 {
            v -= 1i128 << bits.len();
        }
        Some(v)
    }

    fn evaluate_cell(&self, idx: usize, ev: &mut Evaluation, visiting: &mut HashSet<usize>) {
        if ev.done.contains(&idx) || !visiting.insert(idx) {
            return;
        }
        let c = &self.cells[idx];
        let p = |k: &str| c.params.get(k).copied();
        let port = |k: &str| c.conns.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
        let ty = c.ty.as_str();
        let set_bits = |ev: &mut Evaluation, out: &[Bit], values: &[Option<bool>]| {
            for (b, v) in out.iter().zip(values) {
                if let (Bit::Id(id), Some(v)) = (b, v) {
                    ev.bits.insert(*id, *v);
                }
            }
        };
        let y = port("Y").to_vec();
        let yw = y.len();
        // A word result as `Y_WIDTH` bits (two's complement truncation / extension).
        let word_out =
            |v: i128| -> Vec<Option<bool>> { (0..yw).map(|i| Some((v >> i) & 1 == 1)).collect() };
        let a_signed = p("A_SIGNED") == Some(1);
        let b_signed = p("B_SIGNED") == Some(1);
        if FLOPS.contains(&ty) {
            // `Q` is the state the caller seeded; nothing to compute.
        } else {
            match ty {
                "$mux" => {
                    let s = port("S")
                        .first()
                        .copied()
                        .and_then(|s| self.bit(s, ev, visiting));
                    if let Some(s) = s {
                        let src = if s {
                            port("B").to_vec()
                        } else {
                            port("A").to_vec()
                        };
                        let vals: Vec<Option<bool>> =
                            src.iter().map(|b| self.bit(*b, ev, visiting)).collect();
                        set_bits(ev, &y, &vals);
                    }
                }
                "$pmux" => {
                    let w = p("WIDTH").unwrap_or(yw as u64) as usize;
                    let s: Vec<Option<bool>> = port("S")
                        .iter()
                        .map(|b| self.bit(*b, ev, visiting))
                        .collect();
                    if s.iter().all(Option::is_some) {
                        let on: Vec<usize> = s
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| **v == Some(true))
                            .map(|(i, _)| i)
                            .collect();
                        let src: Option<Vec<Bit>> = match on.as_slice() {
                            [] => Some(port("A").to_vec()),
                            [i] => port("B").get(i * w..(i + 1) * w).map(|b| b.to_vec()),
                            _ => None, // not one-hot: undefined
                        };
                        if let Some(src) = src {
                            let vals: Vec<Option<bool>> =
                                src.iter().map(|b| self.bit(*b, ev, visiting)).collect();
                            set_bits(ev, &y, &vals);
                        }
                    }
                }
                "$not" | "$pos" | "$neg" => {
                    if let Some(a) = self.word(port("A"), a_signed, ev, visiting) {
                        let v = match ty {
                            "$not" => !a,
                            "$neg" => a.wrapping_neg(),
                            _ => a,
                        };
                        set_bits(ev, &y, &word_out(v));
                    }
                }
                "$and" | "$or" | "$xor" | "$xnor" => {
                    // Bitwise over sign/zero-extended operands, three-valued where a side is
                    // unknown (`0 & x = 0`, `1 | x = 1`).
                    let ext = |bits: &[Bit],
                               signed: bool,
                               ev: &mut Evaluation,
                               visiting: &mut HashSet<usize>|
                     -> Vec<Option<bool>> {
                        let vals: Vec<Option<bool>> =
                            bits.iter().map(|b| self.bit(*b, ev, visiting)).collect();
                        let fill = if signed {
                            vals.last().copied().flatten()
                        } else {
                            Some(false)
                        };
                        (0..yw)
                            .map(|i| vals.get(i).copied().unwrap_or(fill))
                            .collect()
                    };
                    let a = ext(port("A"), a_signed, ev, visiting);
                    let b = ext(port("B"), b_signed, ev, visiting);
                    let vals: Vec<Option<bool>> = a
                        .iter()
                        .zip(&b)
                        .map(|(x, y)| match ty {
                            "$and" => match (x, y) {
                                (Some(false), _) | (_, Some(false)) => Some(false),
                                (Some(true), Some(true)) => Some(true),
                                _ => None,
                            },
                            "$or" => match (x, y) {
                                (Some(true), _) | (_, Some(true)) => Some(true),
                                (Some(false), Some(false)) => Some(false),
                                _ => None,
                            },
                            "$xor" => Some(x.zip(*y)?.0 ^ x.zip(*y)?.1),
                            _ => Some(!(x.zip(*y)?.0 ^ x.zip(*y)?.1)),
                        })
                        .collect();
                    set_bits(ev, &y, &vals);
                }
                "$logic_not" | "$logic_and" | "$logic_or" | "$reduce_or" | "$reduce_and"
                | "$reduce_bool" | "$reduce_xor" | "$reduce_xnor" => {
                    let reduce_or = |bits: &[Bit],
                                     ev: &mut Evaluation,
                                     visiting: &mut HashSet<usize>|
                     -> Option<bool> {
                        let vals: Vec<Option<bool>> =
                            bits.iter().map(|b| self.bit(*b, ev, visiting)).collect();
                        if vals.contains(&Some(true)) {
                            Some(true)
                        } else if vals.iter().all(|v| *v == Some(false)) {
                            Some(false)
                        } else {
                            None
                        }
                    };
                    let reduce_and = |bits: &[Bit],
                                      ev: &mut Evaluation,
                                      visiting: &mut HashSet<usize>|
                     -> Option<bool> {
                        let vals: Vec<Option<bool>> =
                            bits.iter().map(|b| self.bit(*b, ev, visiting)).collect();
                        if vals.contains(&Some(false)) {
                            Some(false)
                        } else if vals.iter().all(|v| *v == Some(true)) {
                            Some(true)
                        } else {
                            None
                        }
                    };
                    let v = match ty {
                        "$logic_not" => reduce_or(port("A"), ev, visiting).map(|a| !a),
                        "$logic_and" => {
                            let a = reduce_or(port("A"), ev, visiting);
                            let b = reduce_or(port("B"), ev, visiting);
                            match (a, b) {
                                (Some(false), _) | (_, Some(false)) => Some(false),
                                (Some(true), Some(true)) => Some(true),
                                _ => None,
                            }
                        }
                        "$logic_or" => {
                            let a = reduce_or(port("A"), ev, visiting);
                            let b = reduce_or(port("B"), ev, visiting);
                            match (a, b) {
                                (Some(true), _) | (_, Some(true)) => Some(true),
                                (Some(false), Some(false)) => Some(false),
                                _ => None,
                            }
                        }
                        "$reduce_or" | "$reduce_bool" => reduce_or(port("A"), ev, visiting),
                        "$reduce_and" => reduce_and(port("A"), ev, visiting),
                        _ => {
                            let vals: Vec<Option<bool>> = port("A")
                                .iter()
                                .map(|b| self.bit(*b, ev, visiting))
                                .collect();
                            let x = vals.iter().try_fold(false, |acc, v| v.map(|v| acc ^ v));
                            if ty == "$reduce_xor" {
                                x
                            } else {
                                x.map(|v| !v)
                            }
                        }
                    };
                    let mut vals = vec![Some(false); yw];
                    if let Some(first) = vals.first_mut() {
                        *first = v;
                    }
                    if v.is_none() {
                        vals.iter_mut().for_each(|x| *x = None);
                    }
                    set_bits(ev, &y, &vals);
                }
                "$eq" | "$ne" | "$lt" | "$le" | "$gt" | "$ge" | "$eqx" | "$nex" => {
                    let signed = a_signed && b_signed;
                    let a = self.word(port("A"), signed, ev, visiting);
                    let b = self.word(port("B"), signed, ev, visiting);
                    if let (Some(a), Some(b)) = (a, b) {
                        let v = match ty {
                            "$eq" | "$eqx" => a == b,
                            "$ne" | "$nex" => a != b,
                            "$lt" => a < b,
                            "$le" => a <= b,
                            "$gt" => a > b,
                            _ => a >= b,
                        };
                        set_bits(ev, &y, &word_out(v as i128));
                    }
                }
                "$add" | "$sub" | "$mul" => {
                    let signed = a_signed && b_signed;
                    let a = self.word(port("A"), signed, ev, visiting);
                    let b = self.word(port("B"), signed, ev, visiting);
                    if let (Some(a), Some(b)) = (a, b) {
                        let v = match ty {
                            "$add" => a.wrapping_add(b),
                            "$sub" => a.wrapping_sub(b),
                            _ => a.wrapping_mul(b),
                        };
                        set_bits(ev, &y, &word_out(v));
                    }
                }
                "$shl" | "$sshl" | "$shr" | "$sshr" => {
                    let a = self.word(port("A"), a_signed, ev, visiting);
                    let b = self.word(port("B"), false, ev, visiting);
                    if let (Some(a), Some(b)) = (a, b) {
                        let aw = port("A").len() as u32;
                        let mask = if aw >= 127 {
                            i128::MAX
                        } else {
                            (1i128 << aw) - 1
                        };
                        let v = match ty {
                            "$shl" | "$sshl" => a.checked_shl(b as u32).unwrap_or(0),
                            "$shr" => ((a & mask) as u128 >> (b as u32).min(127)) as i128,
                            _ => a
                                .checked_shr(b as u32)
                                .unwrap_or(if a < 0 { -1 } else { 0 }),
                        };
                        set_bits(ev, &y, &word_out(v));
                    }
                }
                _ => {
                    ev.unevaluated.insert(c.ty.clone());
                }
            }
        }
        ev.done.insert(idx);
        visiting.remove(&idx);
    }

    /// The trace-relative slice of `steps` (one `(state, inputs)` per cycle, in order) for the
    /// `seeds` (public net names the property reads): per cycle, the cells that lay on the path
    /// to those nets and controlled their values — a `$mux` / `$pmux` contributes its selector
    /// and the selected input only, an `$and` whose output is 0 the operands that are 0, an `$or`
    /// whose output is 1 the operands that are 1 (the controlling-value rule); a register sends
    /// the walk to its `D` at the previous cycle; an input or a constant ends it. A cell this
    /// module cannot evaluate keeps every input and widens the slice (named in the report).
    pub fn slice(&self, steps: &[Step], seeds: &[String]) -> SliceReport {
        let evs: Vec<Evaluation> = steps.iter().map(|(s, i)| self.evaluate(s, i)).collect();
        let mut report = SliceReport::default();
        for ev in &evs {
            report.widened.extend(ev.unevaluated.iter().cloned());
        }
        let mut relevant: Vec<Vec<u64>> = vec![Vec::new(); steps.len()];
        let mut known_seed = false;
        for seed in seeds {
            match self.nets.get(seed) {
                Some(bits) => {
                    known_seed = true;
                    for r in &mut relevant {
                        r.extend(bits.iter().filter_map(|b| match b {
                            Bit::Id(id) => Some(*id),
                            _ => None,
                        }));
                    }
                }
                None => report.unknown_seeds.push(seed.clone()),
            }
        }
        if !known_seed {
            return report;
        }
        let mut cells_per_cycle: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); steps.len()];
        for t in (0..steps.len()).rev() {
            let ev = &evs[t];
            let mut seen: HashSet<u64> = HashSet::new();
            let mut work: Vec<u64> = std::mem::take(&mut relevant[t]);
            while let Some(bit) = work.pop() {
                if !seen.insert(bit) {
                    continue;
                }
                let Some((idx, port, k)) = self.driver.get(&bit) else {
                    continue; // an input port bit or an undriven net: the walk ends
                };
                let c = &self.cells[*idx];
                cells_per_cycle[t].insert(*idx);
                let val = |b: &Bit| -> Option<bool> {
                    match b {
                        Bit::Zero => Some(false),
                        Bit::One => Some(true),
                        Bit::Undef => None,
                        Bit::Id(id) => ev.bits.get(id).copied(),
                    }
                };
                let push_all = |ports: &[&str], work: &mut Vec<u64>| {
                    for p in ports {
                        for b in c.conns.get(*p).into_iter().flatten() {
                            if let Bit::Id(id) = b {
                                work.push(*id);
                            }
                        }
                    }
                };
                let push = |b: Option<&Bit>, work: &mut Vec<u64>| {
                    if let Some(Bit::Id(id)) = b {
                        work.push(*id);
                    }
                };
                let a = c.conns.get("A").map(|v| v.as_slice()).unwrap_or(&[]);
                let b = c.conns.get("B").map(|v| v.as_slice()).unwrap_or(&[]);
                let s = c.conns.get("S").map(|v| v.as_slice()).unwrap_or(&[]);
                let ty = c.ty.as_str();
                if FLOPS.contains(&ty) {
                    if port == "Q" && t > 0 {
                        push(
                            c.conns.get("D").and_then(|d| d.get(*k)),
                            &mut relevant[t - 1],
                        );
                    }
                    continue;
                }
                match ty {
                    "$mux" => {
                        push(s.first(), &mut work);
                        match s.first().and_then(val) {
                            Some(true) => push(b.get(*k), &mut work),
                            Some(false) => push(a.get(*k), &mut work),
                            None => push_all(&["A", "B"], &mut work),
                        }
                    }
                    "$pmux" => {
                        let w = c.params.get("WIDTH").copied().unwrap_or(1) as usize;
                        let sv: Vec<Option<bool>> = s.iter().map(val).collect();
                        let on: Vec<usize> = sv
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| **v == Some(true))
                            .map(|(i, _)| i)
                            .collect();
                        if sv.iter().any(Option::is_none) || on.len() > 1 {
                            push_all(&["A", "B", "S"], &mut work);
                        } else if let [i] = on.as_slice() {
                            push(s.get(*i), &mut work);
                            push(b.get(i * w + k), &mut work);
                        } else {
                            // No arm selected: the default input, and every selector (each
                            // being 0 is what chose it).
                            push_all(&["S"], &mut work);
                            push(a.get(*k), &mut work);
                        }
                    }
                    "$and" | "$or" => {
                        let want = ty == "$and"; // the controlling value: 0 for and, 1 for or
                        let y = c.conns.get("Y").and_then(|y| y.get(*k)).and_then(val);
                        let ak = a.get(*k).and_then(val);
                        let bk = b.get(*k).and_then(val);
                        if y == Some(!want) && (ak == Some(!want) || bk == Some(!want)) {
                            if ak == Some(!want) {
                                push(a.get(*k), &mut work);
                            }
                            if bk == Some(!want) {
                                push(b.get(*k), &mut work);
                            }
                        } else {
                            push(a.get(*k), &mut work);
                            push(b.get(*k), &mut work);
                        }
                    }
                    "$logic_and" | "$logic_or" => {
                        let want = ty == "$logic_and";
                        let reduce = |bits: &[Bit]| -> Option<bool> {
                            let vals: Vec<Option<bool>> = bits.iter().map(val).collect();
                            if vals.contains(&Some(true)) {
                                Some(true)
                            } else if vals.iter().all(|v| *v == Some(false)) {
                                Some(false)
                            } else {
                                None
                            }
                        };
                        let (ra, rb) = (reduce(a), reduce(b));
                        let controlling = |r: Option<bool>| r == Some(!want);
                        if controlling(ra) || controlling(rb) {
                            if controlling(ra) {
                                push_all(&["A"], &mut work);
                            }
                            if controlling(rb) {
                                push_all(&["B"], &mut work);
                            }
                        } else {
                            push_all(&["A", "B"], &mut work);
                        }
                    }
                    "$reduce_or" | "$reduce_bool" | "$reduce_and" => {
                        let want = ty != "$reduce_and";
                        let hits: Vec<&Bit> = a.iter().filter(|b| val(b) == Some(want)).collect();
                        if hits.is_empty() {
                            push_all(&["A"], &mut work);
                        } else {
                            for b in hits {
                                push(Some(b), &mut work);
                            }
                        }
                    }
                    _ => {
                        if ev.unevaluated.contains(ty) {
                            report.widened.insert(ty.to_string());
                        }
                        let inputs: Vec<&str> = c
                            .conns
                            .keys()
                            .filter(|p| *p != "Y" && *p != "Q")
                            .map(String::as_str)
                            .collect();
                        push_all(&inputs, &mut work);
                    }
                }
            }
        }
        report.cycles = cells_per_cycle
            .into_iter()
            .enumerate()
            .map(|(t, cells)| {
                let mut lines: BTreeSet<SourceLoc> = BTreeSet::new();
                for idx in cells {
                    let c = &self.cells[idx];
                    // A `$pmux` is the `case` as a whole: its first span is whichever arm yosys
                    // merged last, its last the block; the taken arm's own cells carry its lines.
                    let loc = if c.ty == "$pmux" {
                        c.src.last()
                    } else {
                        c.src.first()
                    };
                    if let Some(l) = loc {
                        lines.insert(l.clone());
                    }
                }
                SliceCycle {
                    cycle: t,
                    lines: lines.into_iter().collect(),
                }
            })
            .filter(|c| !c.lines.is_empty())
            .collect();
        report
    }
}

/// One cycle's evaluation: the value of every bit the evaluator could settle.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    pub bits: HashMap<u64, bool>,
    /// Cell types this module does not evaluate that the netlist contains.
    pub unevaluated: BTreeSet<String>,
    done: HashSet<usize>,
}

impl Evaluation {
    /// A public net's value, when every bit is known.
    pub fn net(&self, netlist: &Netlist, name: &str) -> Option<u128> {
        let bits = netlist.nets.get(name)?;
        let mut v = 0u128;
        for (i, b) in bits.iter().enumerate() {
            let bit = match b {
                Bit::Zero => false,
                Bit::One => true,
                Bit::Undef => return None,
                Bit::Id(id) => *self.bits.get(id)?,
            };
            if bit {
                v |= 1 << i;
            }
        }
        Some(v)
    }

    /// A register's `D` — the value it takes at the next edge — when every bit is known.
    pub fn next_of(&self, netlist: &Netlist, register: &str) -> Option<u128> {
        let q = netlist.registers.get(register)?;
        let mut v = 0u128;
        for (i, qb) in q.iter().enumerate() {
            let Bit::Id(qid) = qb else { return None };
            let (idx, _, k) = netlist.driver.get(qid)?;
            let d = netlist.cells[*idx].conns.get("D")?.get(*k)?;
            let bit = match d {
                Bit::Zero => false,
                Bit::One => true,
                Bit::Undef => return None,
                Bit::Id(id) => *self.bits.get(id)?,
            };
            if bit {
                v |= 1 << i;
            }
        }
        Some(v)
    }
}

/// The lines of one cycle of a slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceCycle {
    pub cycle: usize,
    pub lines: Vec<SourceLoc>,
}

/// A trace-relative slice and what limited it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliceReport {
    /// Cycles with at least one line, in order.
    pub cycles: Vec<SliceCycle>,
    /// Cell types the evaluator does not know: their whole fan-in was kept.
    pub widened: BTreeSet<String>,
    /// Seeds that are not public nets of the netlist (a monitor cell, a name the lift mangled).
    pub unknown_seeds: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slice fixture: `lane_arbiter.sv` through the lift's own yosys script in `mununu-sva`
    /// (2026-10-30; `read_slang … ; hierarchy; proc; cutpoint -blackbox; memory_collect;
    /// flatten; opt_clean; write_json` — the netlist the source map reads), its BTOR2 as the
    /// engine ran it (`MUNUNU_SHADOW_BTOR2_DUMP`, the 0.5.8 release binary) and the `alternates`
    /// counterexample that run reported.
    const FLAT_JSON: &str = include_str!("testdata/lane_arbiter.flat.json");
    const SHADOW_BTOR2: &str = include_str!("testdata/lane_arbiter.shadow.btor");

    fn m(pairs: &[(&str, u128)]) -> HashMap<String, u128> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// The measured trace: `[t] stall_q, start_a, start_b, state_q; in: ack, rel, req`.
    fn trace() -> Vec<(HashMap<String, u128>, HashMap<String, u128>)> {
        let st = |stall, sa, sb, state| {
            m(&[
                ("stall_q", stall),
                ("start_a", sa),
                ("start_b", sb),
                ("state_q", state),
            ])
        };
        let inp = |ack, rel, req| {
            m(&[
                ("ack", ack),
                ("rel", rel),
                ("req", req),
                ("rst_n", 1),
                ("clk", 0),
            ])
        };
        vec![
            (st(0, 0, 0, 0), inp(0, 1, 1)),
            (st(0, 1, 0, 1), inp(0, 1, 1)),
            (st(1, 0, 0, 0), inp(0, 0, 1)),
            (st(0, 1, 0, 1), inp(1, 1, 1)),
        ]
    }

    #[test]
    fn the_fixture_netlist_parses_with_its_cells_nets_and_registers() {
        let n = Netlist::parse(FLAT_JSON).expect("parses");
        assert_eq!(
            n.cells.len(),
            47,
            "{:?}",
            n.cells.iter().map(|c| &c.ty).collect::<Vec<_>>()
        );
        assert!(
            n.cells.iter().all(|c| !c.src.is_empty()),
            "every cell carries a src"
        );
        assert_eq!(
            n.registers.keys().collect::<Vec<_>>(),
            vec!["stall_q", "start_a", "start_b", "state_q"]
        );
        assert_eq!(
            n.inputs.iter().collect::<Vec<_>>(),
            vec!["ack", "clk", "rel", "req", "rst_n"]
        );
        assert_eq!(n.nets["state_q"].len(), 2);
        assert_eq!(n.nets["stall_q"].len(), 3);
    }

    /// Self-consistency with the engine's trace: every register's `D` at cycle `t` is the trace's
    /// state at `t+1`; the outputs follow the state.
    #[test]
    fn evaluation_reproduces_the_measured_trace_step_by_step() {
        let n = Netlist::parse(FLAT_JSON).expect("parses");
        let tr = trace();
        for t in 0..tr.len() - 1 {
            let ev = n.evaluate(&tr[t].0, &tr[t].1);
            assert!(ev.unevaluated.is_empty(), "{:?}", ev.unevaluated);
            for r in ["state_q", "stall_q", "start_a", "start_b"] {
                assert_eq!(
                    ev.next_of(&n, r),
                    Some(tr[t + 1].0[r]),
                    "cycle {t}, register {r}"
                );
            }
            let state = tr[t].0["state_q"];
            assert_eq!(
                ev.net(&n, "grant_a"),
                Some((state == 1) as u128),
                "cycle {t}"
            );
            assert_eq!(
                ev.net(&n, "busy"),
                Some((state == 1 || state == 2) as u128),
                "cycle {t}"
            );
            assert_eq!(ev.net(&n, "fault"), Some(0), "cycle {t}");
        }
    }

    /// The differential against the engine's own evaluator on the BTOR2 it ran: the same
    /// `(state, inputs)` give the same next state for every design register.
    #[test]
    fn evaluation_agrees_with_the_engines_one_step_semantics_on_the_lifted_model() {
        let n = Netlist::parse(FLAT_JSON).expect("parses");
        let file = crate::adapter::btor2::parser::parse(SHADOW_BTOR2).expect("btor2 parses");
        let tr = trace();
        for (t, (state, inputs)) in tr.iter().enumerate() {
            let ev = n.evaluate(state, inputs);
            let next = crate::adapter::btor2::bit_blast::simulate_one_step(&file, state, inputs)
                .expect("the engine steps the model");
            for r in ["state_q", "stall_q", "start_a", "start_b"] {
                assert_eq!(
                    ev.next_of(&n, r),
                    next.get(r).copied(),
                    "cycle {t}, register {r}: netlist vs engine ({:?})",
                    next.keys().collect::<Vec<_>>()
                );
            }
        }
    }

    /// The slice of the `alternates` counterexample: the arm that granted A twice and the pulse
    /// that reported it, cycle by cycle — and NOT the FAULT / default arms, the `start_b` pulse's
    /// neighbours' or the `assign`s, which the cone of influence (`involves:`) lists.
    #[test]
    fn the_slice_of_the_alternation_counterexample_names_the_arm_that_decided_it() {
        let n = Netlist::parse(FLAT_JSON).expect("parses");
        let s = n.slice(&trace(), &["start_a".to_string(), "start_b".to_string()]);
        assert!(s.widened.is_empty(), "{:?}", s.widened);
        assert!(s.unknown_seeds.is_empty());
        let lines = |t: usize| -> Vec<u32> {
            s.cycles
                .iter()
                .find(|c| c.cycle == t)
                .map(|c| c.lines.iter().map(|l| l.line).collect())
                .unwrap_or_default()
        };
        // The second grant: IDLE arm (`if (req) next = LANE_A`, line 35, under the `case` at
        // 34), the flops (45) and the two pulses (53, 54).
        assert_eq!(lines(2), vec![34, 35, 45, 53, 54]);
        // The release from LANE_A: the LANE arm (37–38) — its fault condition evaluated false,
        // which makes `stall_q` relevant — the flops and the pulses.
        assert_eq!(lines(1), vec![34, 37, 38, 45, 53, 54]);
        // Reset state: the first grant (IDLE arm) and `stall_q`'s own path (55).
        assert_eq!(lines(0), vec![34, 35, 45, 53, 54, 55]);
        let all: BTreeSet<u32> = s
            .cycles
            .iter()
            .flat_map(|c| c.lines.iter().map(|l| l.line))
            .collect();
        for absent in [40u32, 41, 60, 61, 62, 63] {
            assert!(
                !all.contains(&absent),
                "line {absent} is not on the path: {all:?}"
            );
        }
        assert!(
            s.cycles
                .iter()
                .all(|c| c.lines.iter().all(|l| l.file == "lane_arbiter.sv"))
        );
    }

    /// The controlling-value rule on the smallest netlists that exercise each case.
    #[test]
    fn controlling_values_keep_only_the_operand_that_decided_the_output() {
        // y = a & b with a = 0, b = 1: only `a` decided y = 0.
        let and = r#"{"modules":{"m":{"attributes":{"top":1},"ports":{"pa":{"direction":"input","bits":[5]},"pb":{"direction":"input","bits":[6]},"y":{"direction":"output","bits":[4]}},
          "cells":{"g":{"type":"$and","parameters":{"A_SIGNED":0,"B_SIGNED":0,"A_WIDTH":1,"B_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:7.3-7.12"},"port_directions":{"A":"input","B":"input","Y":"output"},"connections":{"A":[2],"B":[3],"Y":[4]}},
                   "ia":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:5.3-5.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[5],"Y":[2]}},
                   "ib":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:6.3-6.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[6],"Y":[3]}}},
          "netnames":{"a":{"bits":[2]},"b":{"bits":[3]},"y":{"bits":[4]},"pa":{"bits":[5]},"pb":{"bits":[6]}}}}}"#;
        let n = Netlist::parse(and).expect("parses");
        let s = n.slice(&[(m(&[]), m(&[("pa", 1), ("pb", 0)]))], &["y".to_string()]);
        let lines: Vec<u32> = s.cycles[0].lines.iter().map(|l| l.line).collect();
        assert_eq!(
            lines,
            vec![5, 7],
            "the inverter feeding the 0 operand and the gate; not line 6"
        );
        // With both operands 0, both decided it.
        let s = n.slice(&[(m(&[]), m(&[("pa", 1), ("pb", 1)]))], &["y".to_string()]);
        let lines: Vec<u32> = s.cycles[0].lines.iter().map(|l| l.line).collect();
        assert_eq!(lines, vec![5, 6, 7]);
        // A mux follows its selector and the selected input only.
        let mux = r#"{"modules":{"m":{"attributes":{"top":1},"ports":{"s":{"direction":"input","bits":[2]},"a":{"direction":"input","bits":[3]},"b":{"direction":"input","bits":[4]},"y":{"direction":"output","bits":[5]}},
          "cells":{"g":{"type":"$mux","parameters":{"WIDTH":1},"attributes":{"src":"m.sv:9.3-9.20"},"port_directions":{"A":"input","B":"input","S":"input","Y":"output"},"connections":{"A":[6],"B":[7],"S":[2],"Y":[5]}},
                   "ia":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:5.3-5.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[3],"Y":[6]}},
                   "ib":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:6.3-6.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[4],"Y":[7]}}},
          "netnames":{"s":{"bits":[2]},"a":{"bits":[3]},"b":{"bits":[4]},"y":{"bits":[5]}}}}}"#;
        let n = Netlist::parse(mux).expect("parses");
        let s = n.slice(
            &[(m(&[]), m(&[("s", 1), ("a", 0), ("b", 0)]))],
            &["y".to_string()],
        );
        let lines: Vec<u32> = s.cycles[0].lines.iter().map(|l| l.line).collect();
        assert_eq!(
            lines,
            vec![6, 9],
            "selector 1: the B side (line 6), not the A side"
        );
        let s = n.slice(
            &[(m(&[]), m(&[("s", 0), ("a", 0), ("b", 0)]))],
            &["y".to_string()],
        );
        let lines: Vec<u32> = s.cycles[0].lines.iter().map(|l| l.line).collect();
        assert_eq!(lines, vec![5, 9]);
    }

    /// A cell type the evaluator does not know widens the slice (its whole fan-in stays) and is
    /// named, rather than guessed.
    #[test]
    fn an_unknown_cell_widens_the_slice_and_is_named() {
        let j = r#"{"modules":{"m":{"attributes":{"top":1},"ports":{"a":{"direction":"input","bits":[2]},"b":{"direction":"input","bits":[3]},"y":{"direction":"output","bits":[4]}},
          "cells":{"g":{"type":"$frobnicate","parameters":{},"attributes":{"src":"m.sv:7.3-7.12"},"port_directions":{"A":"input","B":"input","Y":"output"},"connections":{"A":[5],"B":[6],"Y":[4]}},
                   "ia":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:5.3-5.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[2],"Y":[5]}},
                   "ib":{"type":"$not","parameters":{"A_SIGNED":0,"A_WIDTH":1,"Y_WIDTH":1},"attributes":{"src":"m.sv:6.3-6.9"},"port_directions":{"A":"input","Y":"output"},"connections":{"A":[3],"Y":[6]}}},
          "netnames":{"a":{"bits":[2]},"b":{"bits":[3]},"y":{"bits":[4]}}}}}"#;
        let n = Netlist::parse(j).expect("parses");
        let s = n.slice(&[(m(&[]), m(&[("a", 1), ("b", 0)]))], &["y".to_string()]);
        assert_eq!(s.widened.iter().collect::<Vec<_>>(), vec!["$frobnicate"]);
        let lines: Vec<u32> = s.cycles[0].lines.iter().map(|l| l.line).collect();
        assert_eq!(lines, vec![5, 6, 7]);
        let ev = n.evaluate(&m(&[]), &m(&[("a", 1), ("b", 0)]));
        assert_eq!(
            ev.net(&n, "y"),
            None,
            "an unknown cell's output is unknown, never guessed"
        );
    }

    #[test]
    fn a_seed_that_is_not_a_public_net_is_reported() {
        let n = Netlist::parse(FLAT_JSON).expect("parses");
        let s = n.slice(&trace(), &["start_a__past".to_string()]);
        assert!(s.cycles.is_empty());
        assert_eq!(s.unknown_seeds, vec!["start_a__past".to_string()]);
    }
}
