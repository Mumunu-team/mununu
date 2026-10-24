//! Structural clock-domain-crossing lint over the flattened yosys netlist (`sv cdc-lint`).
//!
//! > Source of truth: [`cdc_lint`] — surface: (CLI+API), `mununu sv cdc-lint` /
//! > `POST /api/v1/sv/cdc-lint`.
//!
//! The lift refuses a design with more than one clock domain, because its BTOR2 model has one
//! implicit clock. This module answers the structural half of the question that refusal leaves
//! open: which flops sample a value launched in another domain, and is every such path a
//! synchroniser? It reads the netlist snapshot the lift already takes ([`SvLiftResult`]'s
//! `flat_netlist_json`: after `flatten` and `opt_clean`, before `async2sync`), where every
//! storage cell still carries its own `CLK` pin.
//!
//! **What is classified.** A *crossing* is a destination flop bit whose data pins (`D`, `EN`,
//! `SRST`) read, through combinational cells, a flop of another clock net. Per crossing:
//!
//! - `combinational-before-sync` (violated): logic sits between the source flop and the first
//!   destination flop, so a glitch or a multi-input race can be captured;
//! - `unsynchronized` (violated): the capturing flop's output, which can be metastable, is read
//!   by logic instead of a second flop of its domain;
//! - `synchronized` (holds): a chain of at least two destination flops, the first one read only
//!   by the second. A 2-FF synchroniser passes levels; whether a source *pulse* survives depends
//!   on the clock ratio, which no structural check sees (the functional recipe,
//!   `scripts/cdc_formal_sby.sh`, answers it);
//! - `multi-bit-bus`: several bits of one source register cross through parallel synchronisers.
//!   Their samples are coherent only when the source changes at most one bit per source cycle; a
//!   reach query decides that gray-step invariant on the source register ([`gray_step_verdict`]),
//!   sound only when the register's cone stays inside its own domain;
//! - `qualified-data` (unknown): data captured directly, enabled by a synchronised qualifier from
//!   the source domain. Safe if the data holds while the qualifier is asserted, which is a
//!   functional property;
//! - `reconvergence` (unknown): one destination flop reads two separately synchronised signals
//!   from the same source domain, which can arrive one cycle apart;
//! - `dual-clock-memory` (unknown): a memory written in one domain and read in another (an
//!   asynchronous FIFO); its pointers are checked as ordinary crossings.
//!
//! **Out of scope, said in the report's notes.** Primary inputs carry no clock, so a crossing
//! from an input port is not seen; asynchronous set / reset pins (`ARST`, `SET`, `CLR`, `AD`) and
//! latches are not analysed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use serde_json::Value;

use crate::adapter::btor2::ast::Node;
use crate::adapter::btor2::bad_monitor::emit_ag_gray_step_monitor_by_nid;
use crate::adapter::btor2::parser;
use crate::adapter::btor2::pin::pin_inputs_to_constants;
use crate::adapter::btor2::reset_init::inject_reset_init;
use crate::adapter::fsm_scan::detect_resets;
use crate::adapter::reach_portfolio::decide_reach_portfolio;
use crate::adapter::yosys::{ClockDomain, clock_domains, net_names};
use crate::verdict::PropertyVerdict;

/// What a crossing is, structurally (see the module docs for the table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrossingKind {
    CombinationalBeforeSync,
    Unsynchronized,
    MultiBitBus,
    QualifiedData,
    Reconvergence,
    DualClockMemory,
    Synchronized,
}

impl CrossingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CrossingKind::CombinationalBeforeSync => "combinational-before-sync",
            CrossingKind::Unsynchronized => "unsynchronized",
            CrossingKind::MultiBitBus => "multi-bit-bus",
            CrossingKind::QualifiedData => "qualified-data",
            CrossingKind::Reconvergence => "reconvergence",
            CrossingKind::DualClockMemory => "dual-clock-memory",
            CrossingKind::Synchronized => "synchronized",
        }
    }
}

/// One finding: a crossing (or a memory, or a reconvergence point) and its verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Crossing {
    pub kind: CrossingKind,
    /// `holds` (a safe crossing), `violated` (an unsafe one), `unknown` (safe only under a
    /// functional condition the lint names in `detail`).
    pub outcome: PropertyVerdict,
    /// The source register (or memory) name.
    pub source: String,
    pub source_clock: String,
    /// The destination register (bit) name.
    pub destination: String,
    pub destination_clock: String,
    /// Synchroniser depth, for `synchronized` / `multi-bit-bus` rows.
    pub stages: Option<usize>,
    /// The combinational cells between source and destination, with their source locations.
    pub path: Vec<String>,
    pub detail: String,
}

/// The lint's report: the clock domains, every crossing, and the scope notes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CdcReport {
    pub domains: Vec<ClockDomain>,
    pub crossings: Vec<Crossing>,
    pub notes: Vec<String>,
}

impl CdcReport {
    /// The worst outcome over the crossings (`holds` when there are none).
    pub fn worst(&self) -> PropertyVerdict {
        let rank = |v: PropertyVerdict| match v {
            PropertyVerdict::Violated => 3,
            PropertyVerdict::Unknown => 2,
            PropertyVerdict::Skipped => 1,
            PropertyVerdict::Holds => 0,
        };
        self.crossings
            .iter()
            .map(|c| c.outcome)
            .max_by_key(|v| rank(*v))
            .unwrap_or(PropertyVerdict::Holds)
    }
}

/// Run the lint over a flattened netlist (`flat_json`) and, for the gray-step query, the BTOR2 the
/// same lift produced (`btor2`, lifted with merged clock domains; `None` skips the query and
/// leaves multi-bit buses `unknown`).
pub fn cdc_lint(flat_json: &str, btor2: Option<&str>) -> Result<CdcReport, String> {
    let root: Value =
        serde_json::from_str(flat_json).map_err(|e| format!("cdc-lint: netlist JSON: {e}"))?;
    let module = top_module(&root).ok_or("cdc-lint: the netlist has no top module")?;
    let net = Netlist::new(module);
    let mut report = CdcReport {
        domains: clock_domains(flat_json),
        ..Default::default()
    };
    report.notes.push(
        "primary inputs carry no clock: a crossing that starts at an input port is not checked"
            .to_string(),
    );
    if net.has_async_pins {
        report.notes.push(
            "asynchronous set / reset pins are not analysed (reset synchronisers are out of scope)"
                .to_string(),
        );
    }
    if net.has_latches {
        report
            .notes
            .push("latches are not analysed; their outputs are treated as unclocked".to_string());
    }
    if report.domains.iter().any(|d| !d.posedge) {
        report.notes.push(
            "a negedge domain exists: the lift refuses it, and the lint treats both edges of one \
             net as one clock"
                .to_string(),
        );
    }
    let clocks: BTreeSet<i64> = net.flops.iter().map(|f| f.clk).collect();
    if clocks.len() < 2 {
        report
            .notes
            .push("one clock net: the design has no clock-domain crossing".to_string());
        return Ok(report);
    }
    lint_crossings(&net, &mut report, btor2);
    collapse_bits(&mut report.crossings);
    report.crossings.sort_by(|a, b| {
        (a.kind, &a.destination, &a.source).cmp(&(b.kind, &b.destination, &b.source))
    });
    Ok(report)
}

/// One row per (kind, source, destination register): the bits of a register that cross the
/// same way are one finding, named by the register (`cap`, not `cap[0]` … `cap[7]`).
fn collapse_bits(rows: &mut Vec<Crossing>) {
    let base = |n: &str| match n.rfind('[') {
        Some(i) if n.ends_with(']') => n[..i].to_string(),
        _ => n.to_string(),
    };
    let mut seen: HashMap<(CrossingKind, String, String), usize> = HashMap::new();
    let mut out: Vec<Crossing> = Vec::new();
    for row in rows.drain(..) {
        let key = (row.kind, row.source.clone(), base(&row.destination));
        match seen.get(&key) {
            Some(&i) => out[i].destination = key.2.clone(),
            None => {
                seen.insert(key, out.len());
                out.push(row);
            }
        }
    }
    *rows = out;
}

/// The top module: the one flagged `top`, else the only one.
fn top_module(root: &Value) -> Option<&Value> {
    let modules = root.get("modules")?.as_object()?;
    let is_top = |m: &Value| {
        m.get("attributes")
            .and_then(|a| a.get("top"))
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.contains('1'))
    };
    modules
        .values()
        .find(|m| is_top(m))
        .or_else(|| (modules.len() == 1).then(|| modules.values().next())?)
}

/// A storage cell: its clock net bit and its pins, bit by bit.
struct Flop {
    name: String,
    clk: i64,
    q: Vec<i64>,
    /// The synchronous data pins: `D`, `EN`, `SRST`, `CE` (pin, bits).
    data: Vec<(String, Vec<i64>)>,
}

/// A combinational cell: its input and output bits.
struct Comb {
    ty: String,
    src: String,
    inputs: Vec<(String, Vec<i64>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Driver {
    Flop(usize, usize),
    Comb(usize),
    Memory(usize),
}

struct Memory {
    name: String,
    write_clocks: BTreeSet<i64>,
    read_clocks: BTreeSet<i64>,
}

/// The netlist, indexed for the backward cone walk.
struct Netlist {
    names: HashMap<i64, String>,
    flops: Vec<Flop>,
    combs: Vec<Comb>,
    memories: Vec<Memory>,
    driver: HashMap<i64, Driver>,
    /// Bit → every (cell kind, cell index, pin) that reads it; `None` for a module output port.
    loads: HashMap<i64, Vec<Option<(Driver, String)>>>,
    has_async_pins: bool,
    has_latches: bool,
}

const ASYNC_PINS: [&str; 5] = ["ARST", "SET", "CLR", "AD", "ALOAD"];
const DATA_PINS: [&str; 4] = ["D", "EN", "SRST", "CE"];

fn bits(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().map(|b| b.as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default()
}

/// A cell's source location as `file:line` (yosys `src` is `path:l1.c1-l2.c2`, `|`-joined when
/// merged; the first one is kept, with the directory dropped).
fn src_of(cell: &Value) -> String {
    let Some(src) = cell
        .get("attributes")
        .and_then(|a| a.get("src"))
        .and_then(|s| s.as_str())
        .and_then(|s| s.split('|').next())
    else {
        return String::new();
    };
    let Some((file, pos)) = src.rsplit_once(':') else {
        return String::new();
    };
    let file = file.rsplit('/').next().unwrap_or(file);
    let line = pos.split(['.', '-']).next().unwrap_or(pos);
    format!("{file}:{line}")
}

fn param_bit(cell: &Value, name: &str, i: usize) -> Option<bool> {
    let bits = cell.get("parameters")?.get(name)?.as_str()?;
    let b = bits.as_bytes();
    (i < b.len()).then(|| b[b.len() - 1 - i] == b'1')
}

impl Netlist {
    fn new(module: &Value) -> Self {
        let mut n = Netlist {
            names: net_names(module),
            flops: Vec::new(),
            combs: Vec::new(),
            memories: Vec::new(),
            driver: HashMap::new(),
            loads: HashMap::new(),
            has_async_pins: false,
            has_latches: false,
        };
        let empty = serde_json::Map::new();
        let cells = module
            .get("cells")
            .and_then(|c| c.as_object())
            .unwrap_or(&empty);
        for (name, cell) in cells {
            let ty = cell.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let conns = cell
                .get("connections")
                .and_then(|c| c.as_object())
                .unwrap_or(&empty);
            let dirs = cell.get("port_directions").and_then(|d| d.as_object());
            let dir = |pin: &str| {
                dirs.and_then(|d| d.get(pin))
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
            };
            if ty.starts_with("$mem") {
                let idx = n.memories.len();
                let mut m = Memory {
                    name: name.clone(),
                    write_clocks: BTreeSet::new(),
                    read_clocks: BTreeSet::new(),
                };
                for (clk, en, set) in [
                    ("WR_CLK", "WR_CLK_ENABLE", 0),
                    ("RD_CLK", "RD_CLK_ENABLE", 1),
                ] {
                    for (i, b) in conns
                        .get(clk)
                        .map(bits)
                        .unwrap_or_default()
                        .iter()
                        .enumerate()
                    {
                        if *b >= 0 && param_bit(cell, en, i) == Some(true) {
                            if set == 0 {
                                m.write_clocks.insert(*b);
                            } else {
                                m.read_clocks.insert(*b);
                            }
                        }
                    }
                }
                for b in conns.get("RD_DATA").map(bits).unwrap_or_default() {
                    n.driver.insert(b, Driver::Memory(idx));
                }
                n.memories.push(m);
                continue;
            }
            let has = |pin: &str| conns.contains_key(pin);
            if has("Q") && !has("CLK") {
                n.has_latches = true;
                continue;
            }
            if has("CLK") && has("Q") {
                let clk = conns.get("CLK").map(bits).unwrap_or_default();
                let Some(&clk) = clk.first().filter(|b| **b >= 0) else {
                    continue;
                };
                if ASYNC_PINS.iter().any(|p| has(p)) {
                    n.has_async_pins = true;
                }
                let idx = n.flops.len();
                let q = conns.get("Q").map(bits).unwrap_or_default();
                for (j, b) in q.iter().enumerate() {
                    n.driver.insert(*b, Driver::Flop(idx, j));
                }
                let data: Vec<(String, Vec<i64>)> = DATA_PINS
                    .iter()
                    .filter_map(|p| conns.get(*p).map(|v| (p.to_string(), bits(v))))
                    .collect();
                for (pin, bs) in &data {
                    for b in bs {
                        n.loads
                            .entry(*b)
                            .or_default()
                            .push(Some((Driver::Flop(idx, 0), pin.clone())));
                    }
                }
                n.flops.push(Flop {
                    name: name.clone(),
                    clk,
                    q,
                    data,
                });
                continue;
            }
            // A combinational cell, or an instance of an undefined module (no directions: its
            // outputs stay undriven, which the walk reads as unclocked leaves).
            let idx = n.combs.len();
            let mut inputs = Vec::new();
            for (pin, v) in conns {
                match dir(pin) {
                    "output" => {
                        for b in bits(v) {
                            n.driver.insert(b, Driver::Comb(idx));
                        }
                    }
                    "input" => {
                        for b in bits(v) {
                            n.loads
                                .entry(b)
                                .or_default()
                                .push(Some((Driver::Comb(idx), pin.clone())));
                        }
                        inputs.push((pin.clone(), bits(v)));
                    }
                    _ => {}
                }
            }
            n.combs.push(Comb {
                ty: ty.to_string(),
                src: src_of(cell),
                inputs,
            });
        }
        if let Some(ports) = module.get("ports").and_then(|p| p.as_object()) {
            for port in ports.values() {
                if port.get("direction").and_then(|d| d.as_str()) == Some("output") {
                    for b in port.get("bits").map(bits).unwrap_or_default() {
                        n.loads.entry(b).or_default().push(None);
                    }
                }
            }
        }
        n
    }

    fn bit_name(&self, b: i64) -> String {
        self.names
            .get(&b)
            .cloned()
            .unwrap_or_else(|| format!("net#{b}"))
    }

    /// A register's name: its `Q` net without the bit index.
    fn register_name(&self, f: usize) -> String {
        let flop = &self.flops[f];
        let first = flop
            .q
            .first()
            .map(|b| self.bit_name(*b))
            .unwrap_or_default();
        match first.rfind('[') {
            Some(i) if flop.q.len() > 1 => first[..i].to_string(),
            _ if first.is_empty() => flop.name.clone(),
            _ => first,
        }
    }

    fn clock_name(&self, clk: i64) -> String {
        self.bit_name(clk)
    }

    fn is_transparent(&self, c: usize) -> bool {
        matches!(self.combs[c].ty.as_str(), "$buf" | "$pos")
    }
}

/// A flop (cell index) a bit's combinational cone reads, and whether logic sits on the way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Source {
    flop: usize,
    via_logic: bool,
}

/// The flops each bit's cone reads, memoised; iterative, so a deep cone cannot overflow a stack.
struct Cones<'a> {
    net: &'a Netlist,
    memo: HashMap<i64, Rc<BTreeSet<Source>>>,
    memories: HashMap<i64, Rc<BTreeSet<usize>>>,
}

impl<'a> Cones<'a> {
    fn new(net: &'a Netlist) -> Self {
        Cones {
            net,
            memo: HashMap::new(),
            memories: HashMap::new(),
        }
    }

    fn of(&mut self, root: i64) -> Rc<BTreeSet<Source>> {
        if let Some(s) = self.memo.get(&root) {
            return s.clone();
        }
        let mut stack = vec![(root, false)];
        let mut on_stack: HashSet<i64> = HashSet::new();
        while let Some((b, expanded)) = stack.pop() {
            if self.memo.contains_key(&b) {
                continue;
            }
            match self.net.driver.get(&b).copied() {
                Some(Driver::Flop(f, _)) => {
                    self.memo.insert(
                        b,
                        Rc::new(BTreeSet::from([Source {
                            flop: f,
                            via_logic: false,
                        }])),
                    );
                }
                Some(Driver::Memory(m)) => {
                    self.memo.insert(b, Rc::new(BTreeSet::new()));
                    self.memories.insert(b, Rc::new(BTreeSet::from([m])));
                }
                Some(Driver::Comb(c)) => {
                    let inputs: Vec<i64> = self.net.combs[c]
                        .inputs
                        .iter()
                        .flat_map(|(_, bs)| bs.iter().copied())
                        .filter(|x| *x >= 0)
                        .collect();
                    if !expanded {
                        if !on_stack.insert(b) {
                            // A combinational loop: cut it here.
                            self.memo.insert(b, Rc::new(BTreeSet::new()));
                            continue;
                        }
                        stack.push((b, true));
                        for i in inputs {
                            if !self.memo.contains_key(&i) {
                                stack.push((i, false));
                            }
                        }
                        continue;
                    }
                    let transparent = self.net.is_transparent(c);
                    let mut out = BTreeSet::new();
                    let mut mems = BTreeSet::new();
                    for i in inputs {
                        for s in self
                            .memo
                            .get(&i)
                            .map(|s| s.as_ref().clone())
                            .unwrap_or_default()
                        {
                            out.insert(Source {
                                flop: s.flop,
                                via_logic: s.via_logic || !transparent,
                            });
                        }
                        if let Some(m) = self.memories.get(&i) {
                            mems.extend(m.iter().copied());
                        }
                    }
                    on_stack.remove(&b);
                    self.memo.insert(b, Rc::new(out));
                    if !mems.is_empty() {
                        self.memories.insert(b, Rc::new(mems));
                    }
                }
                None => {
                    self.memo.insert(b, Rc::new(BTreeSet::new()));
                }
            }
        }
        self.memo.get(&root).cloned().unwrap_or_default()
    }

    /// The source flops of a flop's synchronous data pins.
    fn of_flop(&mut self, f: usize) -> BTreeSet<Source> {
        let bits: Vec<i64> = self.net.flops[f]
            .data
            .iter()
            .flat_map(|(_, bs)| bs.iter().copied())
            .filter(|b| *b >= 0)
            .collect();
        let mut out = BTreeSet::new();
        for b in bits {
            out.extend(self.of(b).iter().copied());
        }
        out
    }
}

/// The combinational cells on one path from `src` back from bit `b` (breadth-first, shortest).
fn path_to(net: &Netlist, b: i64, src: usize) -> Vec<String> {
    let mut prev: HashMap<i64, Option<(i64, usize)>> = HashMap::from([(b, None)]);
    let mut queue = std::collections::VecDeque::from([b]);
    while let Some(x) = queue.pop_front() {
        match net.driver.get(&x) {
            Some(Driver::Flop(f, _)) if *f == src => {
                let mut cells = Vec::new();
                let mut cur = x;
                while let Some(Some((up, c))) = prev.get(&cur) {
                    let comb = &net.combs[*c];
                    cells.push(if comb.src.is_empty() {
                        comb.ty.clone()
                    } else {
                        format!("{} ({})", comb.ty, comb.src)
                    });
                    cur = *up;
                }
                return cells;
            }
            Some(Driver::Comb(c)) => {
                for (_, bs) in &net.combs[*c].inputs {
                    for i in bs {
                        if *i >= 0 && !prev.contains_key(i) {
                            prev.insert(*i, Some((x, *c)));
                            queue.push_back(*i);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// The synchroniser chain a captured bit starts: `Some((stages, last flop, last bit))` when the
/// capturing flop's output is read by exactly one flop of its own domain, through its `D` pin.
fn chain_from(net: &Netlist, first: usize, bit: usize) -> Option<(usize, usize, usize)> {
    let clk = net.flops[first].clk;
    let (mut cur, mut cur_bit, mut stages) = (first, bit, 1usize);
    while stages < 8 {
        let q = *net.flops[cur].q.get(cur_bit)?;
        let loads = net.loads.get(&q).map(Vec::as_slice).unwrap_or(&[]);
        let [Some((Driver::Flop(g, _), pin))] = loads else {
            break;
        };
        let g = *g;
        if pin != "D" || net.flops[g].clk != clk || g == cur {
            break;
        }
        let d = &net.flops[g].data.iter().find(|(p, _)| p == "D")?.1;
        cur_bit = d.iter().position(|b| *b == q)?;
        cur = g;
        stages += 1;
    }
    (stages >= 2).then_some((stages, cur, cur_bit))
}

fn lint_crossings(net: &Netlist, report: &mut CdcReport, btor2: Option<&str>) {
    let mut cones = Cones::new(net);

    // Every capture: a destination flop bit whose data pin reads, directly or through logic, a
    // flop of another clock.
    struct Capture {
        dst: usize,
        dst_bit_name: String,
        src: usize,
        direct_bit: Option<usize>,
        d_bit: i64,
        via_logic: bool,
        pin: String,
    }
    let mut captures: Vec<Capture> = Vec::new();
    for (fi, f) in net.flops.iter().enumerate() {
        for (pin, bs) in &f.data {
            for (j, &b) in bs.iter().enumerate() {
                if b < 0 {
                    continue;
                }
                for s in cones.of(b).iter() {
                    if net.flops[s.flop].clk == f.clk {
                        continue;
                    }
                    let direct_bit = match net.driver.get(&b) {
                        Some(Driver::Flop(..)) if pin == "D" => Some(j),
                        _ => None,
                    };
                    let q_name =
                        f.q.get(if pin == "D" { j } else { 0 })
                            .map(|q| net.bit_name(*q))
                            .unwrap_or_else(|| f.name.clone());
                    captures.push(Capture {
                        dst: fi,
                        dst_bit_name: q_name,
                        src: s.flop,
                        direct_bit,
                        d_bit: b,
                        via_logic: s.via_logic,
                        pin: pin.clone(),
                    });
                }
            }
        }
    }

    // Synchroniser chains first: a qualifier and a reconvergence are defined over them.
    // sync_out[last flop] = (source flop, first flop).
    let mut sync_out: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut chains: BTreeMap<(usize, i64), Vec<(usize, String)>> = BTreeMap::new();
    for c in &captures {
        if c.via_logic || c.pin != "D" {
            continue;
        }
        if let Some(j) = c.direct_bit
            && let Some((stages, last, _)) = chain_from(net, c.dst, j)
        {
            sync_out.insert(last, (c.src, c.dst));
            chains
                .entry((c.src, net.flops[c.dst].clk))
                .or_default()
                .push((stages, c.dst_bit_name.clone()));
        }
    }
    let synced_from = |cones: &mut Cones, bits: &[i64], src_clk: i64| -> bool {
        bits.iter().filter(|b| **b >= 0).any(|b| {
            cones.of(*b).iter().any(|s| {
                sync_out
                    .get(&s.flop)
                    .is_some_and(|(src, _)| net.flops[*src].clk == src_clk)
            })
        })
    };

    let mut seen: HashSet<(usize, String, usize)> = HashSet::new();
    for c in &captures {
        if !seen.insert((c.dst, c.dst_bit_name.clone(), c.src)) {
            continue;
        }
        let dst = &net.flops[c.dst];
        let src_clk = net.flops[c.src].clk;
        let row = |kind, outcome, stages, path, detail: String| Crossing {
            kind,
            outcome,
            source: net.register_name(c.src),
            source_clock: net.clock_name(src_clk),
            destination: c.dst_bit_name.clone(),
            destination_clock: net.clock_name(dst.clk),
            stages,
            path,
            detail,
        };
        // A qualifier: the flop's enable, or a mux select on the data path, read from a
        // synchroniser of the source domain.
        let en_bits: Vec<i64> = dst
            .data
            .iter()
            .filter(|(p, _)| p == "EN")
            .flat_map(|(_, b)| b.clone())
            .collect();
        let mux_selects: Vec<i64> = qualifier_selects(net, c.d_bit);
        let qualified = synced_from(&mut cones, &en_bits, src_clk)
            || (!mux_selects.is_empty() && synced_from(&mut cones, &mux_selects, src_clk));
        if c.via_logic {
            if qualified && mux_selects_only(net, c.d_bit, c.src) {
                report.crossings.push(row(
                    CrossingKind::QualifiedData,
                    PropertyVerdict::Unknown,
                    None,
                    path_to(net, c.d_bit, c.src),
                    "captured through a mux whose select is synchronised from the source domain; \
                     safe if the source holds while the qualifier is asserted (a functional \
                     property: scripts/cdc_formal_sby.sh)"
                        .to_string(),
                ));
            } else {
                report.crossings.push(row(
                    CrossingKind::CombinationalBeforeSync,
                    PropertyVerdict::Violated,
                    None,
                    path_to(net, c.d_bit, c.src),
                    "combinational logic between the source flop and the first destination flop \
                     can glitch or race; register the value in the source domain, then \
                     synchronise"
                        .to_string(),
                ));
            }
            continue;
        }
        if c.pin != "D" {
            report.crossings.push(row(
                CrossingKind::Unsynchronized,
                PropertyVerdict::Violated,
                None,
                Vec::new(),
                format!(
                    "the {} pin samples another domain's flop directly",
                    c.pin.as_str()
                ),
            ));
            continue;
        }
        match c.direct_bit.and_then(|j| chain_from(net, c.dst, j)) {
            Some((stages, _, _)) => {
                let bus = chains.get(&(c.src, dst.clk)).is_some_and(|v| v.len() > 1);
                if !bus {
                    report.crossings.push(row(
                        CrossingKind::Synchronized,
                        PropertyVerdict::Holds,
                        Some(stages),
                        Vec::new(),
                        format!(
                            "{stages}-flop synchroniser; it passes levels, and whether a pulse \
                             survives depends on the clock ratio (scripts/cdc_formal_sby.sh)"
                        ),
                    ));
                }
            }
            None if qualified => report.crossings.push(row(
                CrossingKind::QualifiedData,
                PropertyVerdict::Unknown,
                None,
                Vec::new(),
                "captured directly under an enable synchronised from the source domain; safe if \
                 the source holds while the enable is asserted (a functional property: \
                 scripts/cdc_formal_sby.sh)"
                    .to_string(),
            )),
            None => report.crossings.push(row(
                CrossingKind::Unsynchronized,
                PropertyVerdict::Violated,
                None,
                Vec::new(),
                "the capturing flop's output, which can be metastable, is read by logic instead \
                 of a second flop of its domain"
                    .to_string(),
            )),
        }
    }

    // Multi-bit buses: one row per (source register, destination clock).
    for ((src, dst_clk), rows) in &chains {
        if rows.len() < 2 {
            continue;
        }
        let stages = rows.iter().map(|(s, _)| *s).min();
        let (outcome, detail) = gray_step_verdict(net, &mut cones, *src, btor2);
        let mut dsts: Vec<&str> = rows.iter().map(|(_, n)| n.as_str()).collect();
        dsts.sort();
        report.crossings.push(Crossing {
            kind: CrossingKind::MultiBitBus,
            outcome,
            source: net.register_name(*src),
            source_clock: net.clock_name(net.flops[*src].clk),
            destination: dsts.join(", "),
            destination_clock: net.clock_name(*dst_clk),
            stages,
            path: Vec::new(),
            detail,
        });
    }

    // Reconvergence: a flop reading, through logic, two synchronisers from the same source
    // domain that start at different source registers.
    for (hi, h) in net.flops.iter().enumerate() {
        if sync_out.contains_key(&hi) {
            continue;
        }
        let mut by_src_clk: BTreeMap<i64, BTreeSet<usize>> = BTreeMap::new();
        for s in cones.of_flop(hi) {
            if let Some((src, _)) = sync_out.get(&s.flop)
                && s.via_logic
                && net.flops[s.flop].clk == h.clk
            {
                by_src_clk
                    .entry(net.flops[*src].clk)
                    .or_default()
                    .insert(*src);
            }
        }
        for (src_clk, srcs) in by_src_clk {
            if srcs.len() < 2 {
                continue;
            }
            let names: Vec<String> = srcs.iter().map(|s| net.register_name(*s)).collect();
            report.crossings.push(Crossing {
                kind: CrossingKind::Reconvergence,
                outcome: PropertyVerdict::Unknown,
                source: names.join(", "),
                source_clock: net.clock_name(src_clk),
                destination: net.register_name(hi),
                destination_clock: net.clock_name(h.clk),
                stages: None,
                path: Vec::new(),
                detail: "separately synchronised signals can arrive one destination cycle apart; \
                         the logic that combines them must tolerate the skew"
                    .to_string(),
            });
        }
    }

    for m in &net.memories {
        if m.read_clocks.is_empty() || m.write_clocks.is_empty() || m.read_clocks == m.write_clocks
        {
            continue;
        }
        let names = |s: &BTreeSet<i64>| {
            s.iter()
                .map(|c| net.clock_name(*c))
                .collect::<Vec<_>>()
                .join(", ")
        };
        report.crossings.push(Crossing {
            kind: CrossingKind::DualClockMemory,
            outcome: PropertyVerdict::Unknown,
            source: m.name.clone(),
            source_clock: names(&m.write_clocks),
            destination: m.name.clone(),
            destination_clock: names(&m.read_clocks),
            stages: None,
            path: Vec::new(),
            detail: "written and read on different clocks; correct only if no address is read \
                     while it is written (the pointer crossings are checked as ordinary rows)"
                .to_string(),
        });
    }
}

/// Mux select bits on the mux tree that drives bit `b`.
fn qualifier_selects(net: &Netlist, b: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let mut stack = vec![b];
    let mut seen = HashSet::new();
    while let Some(x) = stack.pop() {
        if !seen.insert(x) {
            continue;
        }
        let Some(Driver::Comb(c)) = net.driver.get(&x) else {
            continue;
        };
        let comb = &net.combs[*c];
        if !matches!(comb.ty.as_str(), "$mux" | "$pmux") {
            continue;
        }
        for (pin, bs) in &comb.inputs {
            if pin == "S" {
                out.extend(bs.iter().copied().filter(|b| *b >= 0));
            } else {
                stack.extend(bs.iter().copied().filter(|b| *b >= 0));
            }
        }
    }
    out
}

/// Whether every cell between `b` and flop `src` is a mux entered through a data port.
fn mux_selects_only(net: &Netlist, b: i64, src: usize) -> bool {
    let mut stack = vec![b];
    let mut seen = HashSet::new();
    let mut reached = false;
    while let Some(x) = stack.pop() {
        if !seen.insert(x) {
            continue;
        }
        match net.driver.get(&x) {
            Some(Driver::Flop(f, _)) => reached |= *f == src,
            Some(Driver::Comb(c)) => {
                let comb = &net.combs[*c];
                let is_mux = matches!(comb.ty.as_str(), "$mux" | "$pmux");
                for (pin, bs) in &comb.inputs {
                    if is_mux && pin == "S" {
                        continue;
                    }
                    for i in bs.iter().copied().filter(|b| *b >= 0) {
                        let leads_to_src = matches!(
                            net.driver.get(&i),
                            Some(Driver::Flop(f, _)) if *f == src
                        );
                        if !is_mux && (leads_to_src || reaches(net, i, src)) {
                            return false;
                        }
                        stack.push(i);
                    }
                }
            }
            _ => {}
        }
    }
    reached
}

/// Whether bit `b`'s combinational cone reads flop `src`.
fn reaches(net: &Netlist, b: i64, src: usize) -> bool {
    let mut stack = vec![b];
    let mut seen = HashSet::new();
    while let Some(x) = stack.pop() {
        if !seen.insert(x) {
            continue;
        }
        match net.driver.get(&x) {
            Some(Driver::Flop(f, _)) if *f == src => return true,
            Some(Driver::Comb(c)) => {
                for (_, bs) in &net.combs[*c].inputs {
                    stack.extend(bs.iter().copied().filter(|b| *b >= 0));
                }
            }
            _ => {}
        }
    }
    false
}

/// The gray-step invariant on a bus's source register: `holds` when no step flips two bits.
///
/// SOUNDNESS: the query runs on the lift with merged clock domains, an under-approximation of the
/// interleavings. It is exact for the source register when the register's sequential cone (every
/// flop that can influence it) lies in its own domain: the other domains then cannot affect it,
/// so how they tick is irrelevant. When the cone reaches another domain the row stays `unknown`.
/// The reset is pinned inactive after the post-reset state, as `check-fsm` does: a bus is not
/// expected to be gray across its own reset.
fn gray_step_verdict(
    net: &Netlist,
    cones: &mut Cones,
    src: usize,
    btor2: Option<&str>,
) -> (PropertyVerdict, String) {
    let clk = net.flops[src].clk;
    let mut seen = BTreeSet::from([src]);
    let mut stack = vec![src];
    while let Some(f) = stack.pop() {
        for s in cones.of_flop(f) {
            if net.flops[s.flop].clk != clk {
                return (
                    PropertyVerdict::Unknown,
                    format!(
                        "parallel synchronisers on a multi-bit value: coherent only if it is gray \
                         coded; not checked, because its cone reads `{}` from another domain",
                        net.register_name(s.flop)
                    ),
                );
            }
            if seen.insert(s.flop) {
                stack.push(s.flop);
            }
        }
    }
    let Some(btor2) = btor2 else {
        return (
            PropertyVerdict::Unknown,
            "parallel synchronisers on a multi-bit value: coherent only if it is gray coded (not \
             checked)"
                .to_string(),
        );
    };
    let name = net.register_name(src);
    let unchecked = |why: String| {
        (
            PropertyVerdict::Unknown,
            format!("parallel synchronisers on a multi-bit value; the gray-step check {why}"),
        )
    };
    let Ok(file) = parser::parse(btor2) else {
        return unchecked("could not parse the lifted model".to_string());
    };
    let names = parser::canonical_register_names(&file);
    let Some((nid, sort)) = names.iter().find_map(|(nid, n)| {
        (n == &name)
            .then(|| match file.lookup(*nid).map(|l| &l.node) {
                Some(Node::State { sort, .. }) => Some((*nid, *sort)),
                _ => None,
            })
            .flatten()
    }) else {
        return unchecked(format!("found no state named `{name}` in the lifted model"));
    };
    let symbols = parser::collect_symbols(&file);
    let resets = detect_resets(&file, &symbols);
    let Ok(seeded) = inject_reset_init(btor2, &resets) else {
        return unchecked("could not seed the post-reset state".to_string());
    };
    let (pinned, _) = pin_inputs_to_constants(&seeded, &resets);
    let Ok(monitored) = emit_ag_gray_step_monitor_by_nid(&pinned, nid, sort) else {
        return unchecked("could not build its monitor".to_string());
    };
    let Ok(mfile) = parser::parse(&monitored) else {
        return unchecked("could not parse its monitor".to_string());
    };
    match PropertyVerdict::from(decide_reach_portfolio(&mfile).verdict) {
        PropertyVerdict::Holds => (
            PropertyVerdict::Holds,
            format!("`{name}` changes at most one bit per source cycle (gray step decided)"),
        ),
        PropertyVerdict::Violated => (
            PropertyVerdict::Violated,
            format!(
                "`{name}` can change two or more bits in one source cycle, so the destination \
                 can sample a value the source never held; gray-code it or use a handshake"
            ),
        ),
        v => unchecked(format!("was not decided ({})", v.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written netlist in yosys `write_json` shape: `clk_a` = bit 2, `clk_b` = bit 3.
    fn netlist(cells: &str, nets: &str) -> String {
        format!(
            r#"{{"modules": {{"x": {{"attributes": {{"top": "00000000000000000000000000000001"}},
              "ports": {{}}, "cells": {{ {cells} }},
              "netnames": {{"clk_a": {{"bits": [2]}}, "clk_b": {{"bits": [3]}}, {nets} }} }} }} }}"#
        )
    }

    fn dff(name: &str, clk: i64, d: &str, q: &str) -> String {
        format!(
            r#""{name}": {{"type": "$dff", "parameters": {{"CLK_POLARITY": "1"}},
                "port_directions": {{"CLK": "input", "D": "input", "Q": "output"}},
                "connections": {{"CLK": [{clk}], "D": [{d}], "Q": [{q}]}} }}"#
        )
    }

    fn and(name: &str, a: i64, b: i64, y: i64) -> String {
        format!(
            r#""{name}": {{"type": "$and", "port_directions": {{"A": "input", "B": "input", "Y": "output"}},
                "connections": {{"A": [{a}], "B": [{b}], "Y": [{y}]}} }}"#
        )
    }

    fn kinds(r: &CdcReport) -> Vec<(&'static str, &'static str)> {
        r.crossings
            .iter()
            .map(|c| (c.kind.as_str(), c.outcome.as_str()))
            .collect()
    }

    #[test]
    fn a_two_flop_synchroniser_holds_and_logic_before_it_is_violated() {
        // ra, rb in clk_a; m -> s in clk_b (a 2-FF chain); bad <= ra & rb in clk_b.
        let cells = [
            dff("ra", 2, "4", "8"),
            dff("rb", 2, "5", "9"),
            dff("m", 3, "8", "11"),
            dff("s", 3, "11", "12"),
            and("g", 8, 9, 10),
            dff("bad", 3, "10", "7"),
        ]
        .join(",");
        let nets = r#""ra": {"bits": [8]}, "rb": {"bits": [9]}, "m": {"bits": [11]},
                      "s": {"bits": [12]}, "bad": {"bits": [7]}"#;
        let r = cdc_lint(&netlist(&cells, nets), None).unwrap();
        assert_eq!(
            kinds(&r),
            vec![
                ("combinational-before-sync", "violated"),
                ("combinational-before-sync", "violated"),
                ("synchronized", "holds"),
            ],
            "{r:#?}"
        );
        assert_eq!(r.crossings[2].stages, Some(2));
        assert_eq!(r.crossings[2].source, "ra");
        assert_eq!(r.crossings[2].destination, "m");
        assert_eq!(r.worst(), PropertyVerdict::Violated);
    }

    #[test]
    fn a_capture_read_by_logic_is_unsynchronized() {
        // ra in clk_a; m in clk_b captures it, and m feeds logic directly.
        let cells = [
            dff("ra", 2, "4", "8"),
            dff("m", 3, "8", "11"),
            and("g", 11, 5, 10),
            dff("y", 3, "10", "12"),
        ]
        .join(",");
        let nets = r#""ra": {"bits": [8]}, "m": {"bits": [11]}, "y": {"bits": [12]}"#;
        let r = cdc_lint(&netlist(&cells, nets), None).unwrap();
        assert_eq!(kinds(&r), vec![("unsynchronized", "violated")], "{r:#?}");
    }

    #[test]
    fn parallel_synchronisers_on_one_register_are_a_bus() {
        // A 2-bit register st[1:0] in clk_a, each bit through its own 2-FF chain into clk_b.
        let st = r#""st": {"type": "$dff", "parameters": {"CLK_POLARITY": "1"},
            "port_directions": {"CLK": "input", "D": "input", "Q": "output"},
            "connections": {"CLK": [2], "D": [4, 5], "Q": [8, 9]}}"#;
        let cells = [
            st.to_string(),
            dff("m0", 3, "8", "20"),
            dff("s0", 3, "20", "21"),
            dff("m1", 3, "9", "22"),
            dff("s1", 3, "22", "23"),
        ]
        .join(",");
        let nets = r#""st": {"bits": [8, 9]}, "m0": {"bits": [20]}, "s0": {"bits": [21]},
                      "m1": {"bits": [22]}, "s1": {"bits": [23]}"#;
        let r = cdc_lint(&netlist(&cells, nets), None).unwrap();
        assert_eq!(kinds(&r), vec![("multi-bit-bus", "unknown")], "{r:#?}");
        assert_eq!(r.crossings[0].source, "st");
        assert_eq!(r.crossings[0].destination, "m0, m1");
    }

    #[test]
    fn one_clock_has_no_crossing() {
        let cells = [dff("ra", 2, "4", "8"), dff("m", 2, "8", "11")].join(",");
        let r = cdc_lint(&netlist(&cells, r#""ra": {"bits": [8]}"#), None).unwrap();
        assert!(r.crossings.is_empty());
        assert_eq!(r.worst(), PropertyVerdict::Holds);
    }
    /// The lint over a real yosys netlist: a binary counter through parallel synchronisers is an
    /// incoherent bus (the gray-step query finds the 1 → 2 step), the same counter gray-coded
    /// holds, and a single-bit flag through a 2-FF chain is `synchronized`. Needs yosys.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_cdc_lint_decides_the_gray_step_of_a_bus() {
        use crate::adapter::sv_verify::{SvLift, sv_cdc_lint};
        let design = |encode: &str| {
            format!(
                "module cdc (input logic clk_a, input logic clk_b, input logic rst_n,
                    input logic inc, input logic flag, output logic [1:0] o, output logic f);
  logic [1:0] cnt, bus, m, s;
  logic fa, fm, fs;
  always_ff @(posedge clk_a) begin
    if (!rst_n) begin cnt <= 2'd0; bus <= 2'd0; fa <= 1'b0; end
    else begin
      fa <= flag;
      if (inc) begin cnt <= cnt + 2'd1; bus <= {encode}; end
    end
  end
  always_ff @(posedge clk_b) begin m <= bus; s <= m; fm <= fa; fs <= fm; end
  assign o = s;
  assign f = fs;
endmodule
"
            )
        };
        let run = |src: String| {
            sv_cdc_lint(&SvLift {
                primary_name: "cdc.sv".to_string(),
                source: src,
                additional_sources: Vec::new(),
                top: Some("cdc".to_string()),
                use_sv2v: false,
                include_dirs: Vec::new(),
                frontend: crate::adapter::yosys::SvFrontend::Verilog,
                merge_clock_domains: false,
                ..Default::default()
            })
            .expect("the lint runs")
        };
        let row = |r: &CdcReport, kind: CrossingKind| {
            r.crossings
                .iter()
                .find(|c| c.kind == kind)
                .cloned()
                .unwrap_or_else(|| panic!("no {} row: {r:#?}", kind.as_str()))
        };

        let binary = run(design("cnt + 2'd1"));
        let bus = row(&binary, CrossingKind::MultiBitBus);
        assert_eq!(bus.outcome, PropertyVerdict::Violated, "{bus:#?}");
        assert_eq!(bus.source, "bus");
        let flag = row(&binary, CrossingKind::Synchronized);
        assert_eq!(
            (flag.source.as_str(), flag.stages),
            ("fa", Some(2)),
            "{flag:#?}"
        );

        let gray = run(design("(cnt + 2'd1) ^ ((cnt + 2'd1) >> 1)"));
        let bus = row(&gray, CrossingKind::MultiBitBus);
        assert_eq!(bus.outcome, PropertyVerdict::Holds, "{bus:#?}");
        assert_eq!(gray.worst(), PropertyVerdict::Holds, "{gray:#?}");
    }
}
