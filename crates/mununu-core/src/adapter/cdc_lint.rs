//! Structural clock-domain-crossing lint over the flattened yosys netlist (`sv cdc-lint`).
//!
//! > Source of truth: [`cdc_lint`] — surface: (CLI+API), `mununu sv cdc-lint` /
//! > `POST /api/v1/sv/cdc-lint`.
//!
//! The lift refuses a design with more than one clock domain, because its BTOR2 model has one
//! implicit clock. This module answers the structural half of the question that refusal leaves
//! open: which flops sample a value launched in another domain, and is every such path a
//! synchroniser? It reads the netlist snapshot the lift already takes ([`SvLiftResult`]'s
//! `flat_netlist_json`: after `flatten` and an `opt_clean` that keeps every named net, before
//! `async2sync`), where every storage cell of the model still carries its own `CLK` pin — also
//! one that no output port reads (mununu#1229).
//!
//! **What is classified.** A *crossing* is a destination flop bit whose data pins (`D`, `EN`,
//! `SRST`) read, through combinational cells, a flop of another clock net. A memory takes part
//! as the cells it stands for (mununu#1230, [`Netlist::add_memory`]): a clocked write port is a
//! storage cell on its write clock whose data pins are the port's enable, address and data; a
//! read port is logic over its address and the stored word, or a register when the port is
//! clocked. Per crossing:
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
//! - `dual-clock-memory` (unknown): a register that reads, on its own clock, a word a memory
//!   stores on another (an asynchronous FIFO's read side), or a memory written on more than one
//!   clock. Safe only if no word is read while it is written, which is a functional property.
//!   The pointers, and every value a write port captures, are checked as ordinary crossings.
//!
//! **Out of scope, said in the report's notes.** Primary inputs carry no clock, so a crossing
//! from an input port is not seen; asynchronous set / reset pins (`ARST`, `SET`, `CLR`, `AD`) and
//! latches are not analysed. A memory cell the lint cannot read is named in a note, never
//! skipped in silence.

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
use crate::adapter::yosys::{ClockDomain, net_names};
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
    /// The destination register (bit) name, or the memory whose write port captures.
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

/// mununu#1148 — one note per domain whose clock the lifted design holds no driver for: the lint
/// classifies its crossings as from an independent clock, which is right for a PLL and wrong for
/// a black-boxed clock gate or divider (then the two "domains" are one clock, gated). A clock
/// tied to a constant (mununu#1180) is no domain of the lint at all — its cells never update on
/// a clock edge and the walk reads their outputs as unclocked — and the note says that instead. Exhaustive over
/// `ClockRoot`, so a new root kind is a compile error here.
pub(crate) fn clock_root_notes(domains: &[ClockDomain]) -> Vec<String> {
    use crate::adapter::yosys::{ClockRoot, quoted_list};
    domains
        .iter()
        .filter_map(|d| {
            let what = match &d.root {
                ClockRoot::Input | ClockRoot::Derived(_) => return None,
                ClockRoot::Constant { value, register } => {
                    return Some(format!(
                        "the clock of `{register}` is tied to constant {value} ({} storage \
                         cell(s) on it): a constant has no edge, so they never update on a clock edge; the lint \
                         reads their outputs as unclocked and reports no crossing from or to \
                         them (connect the clock in the RTL)",
                        d.cells
                    ));
                }
                ClockRoot::BlackBox(t) => format!("driven by the output of black box `{t}`"),
                ClockRoot::BlackBoxPort(types) => format!(
                    "on a port of black box {} (an `inout` port, or a module with no \
                     declaration: it may drive the clock)",
                    quoted_list(types)
                ),
                ClockRoot::MultiDriven(types) => {
                    format!("driven by more than one cell ({})", quoted_list(types))
                }
                ClockRoot::Cut => {
                    "driven by a cut net (the output of a declared or empty black box, or a cut \
                     point)"
                        .to_string()
                }
                ClockRoot::Undriven => "driven by nothing (the net has no driver)".to_string(),
            };
            Some(format!(
                "clock `{}` is {what}: crossings from or to it are classified as from \
                 an independent clock, which holds for a PLL and not for a gated or divided copy \
                 of another domain's clock (provide the cell's source to see which)",
                d.net
            ))
        })
        .collect()
}

/// How the note about a memory cell the lint could not read starts (mununu#1230); the cells
/// follow it.
pub(crate) const UNREAD_MEMORY_NOTE: &str =
    "a crossing into, out of or through a memory the lint could not read is not checked";

/// The note of a design with a register that has an asynchronous set, reset or load pin.
pub(crate) const ASYNC_PIN_NOTE: &str =
    "asynchronous set / reset pins are not analysed (reset synchronisers are out of scope)";

/// The note of a design with storage that is not edge clocked: a latch, or a memory port written
/// without a clock.
pub(crate) const LATCH_NOTE: &str =
    "latches are not analysed; their outputs are treated as unclocked";

/// Run the lint over a flattened netlist (`flat_json`) with the clock domains the lift read from
/// it (`domains`: [`SvLiftResult::clock_domains`](crate::adapter::yosys::SvLiftResult) — the
/// lift's scan has the hierarchy snapshot, where a stub's port directions are, so the roots the
/// notes name are the ones the lift decided on) and, for the gray-step query, the BTOR2 the same
/// lift produced (`btor2`, lifted with merged clock domains; `None` skips the query and leaves
/// multi-bit buses `unknown`).
pub fn cdc_lint(
    flat_json: &str,
    domains: &[ClockDomain],
    btor2: Option<&str>,
) -> Result<CdcReport, String> {
    let root: Value =
        serde_json::from_str(flat_json).map_err(|e| format!("cdc-lint: netlist JSON: {e}"))?;
    let module = top_module(&root).ok_or("cdc-lint: the netlist has no top module")?;
    let net = Netlist::new(module);
    let mut report = CdcReport {
        domains: domains.to_vec(),
        ..Default::default()
    };
    report.notes.push(
        "primary inputs carry no clock: a crossing that starts at an input port is not checked"
            .to_string(),
    );
    if net.has_async_pins {
        report.notes.push(ASYNC_PIN_NOTE.to_string());
    }
    if net.has_latches {
        report.notes.push(LATCH_NOTE.to_string());
    }
    if !net.unread_memories.is_empty() {
        report.notes.push(format!(
            "{UNREAD_MEMORY_NOTE}: {}. This is a mununu defect: please report it with the design",
            net.unread_memories.join(", ")
        ));
    }
    report.notes.extend(clock_root_notes(&report.domains));
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

/// A storage cell: its clock net bit and its pins, bit by bit. A flop, or one clocked port of a
/// memory (see [`Netlist::add_memory`]).
struct Flop {
    name: String,
    clk: i64,
    /// The output bits. A memory write port has one, which no net of the design carries: it
    /// stands for the stored word, and is named after the memory.
    q: Vec<i64>,
    /// The synchronous data pins (pin, bits): `D`, `EN`, `SRST`, `CE` of a flop;
    /// [`WRITE_PORT_PINS`] of a memory write port; [`READ_PORT_PINS`] and [`STORED_WORD`] of a
    /// clocked memory read port.
    data: Vec<(String, Vec<i64>)>,
    /// `true` for a memory write port: what it holds is a stored word, not a register.
    stores_memory_word: bool,
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
}

/// A memory, for the one row that is about the memory as a whole: more than one write clock.
struct Memory {
    name: String,
    write_clocks: BTreeSet<i64>,
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
    /// Memory cells the lint could not read, as `` `name` (why) ``: one note names them all.
    unread_memories: Vec<String>,
}

const ASYNC_PINS: [&str; 5] = ["ARST", "SET", "CLR", "AD", "ALOAD"];
const DATA_PINS: [&str; 4] = ["D", "EN", "SRST", "CE"];
/// The pins of a memory write port, read as the data pins of a storage cell.
const WRITE_PORT_PINS: [&str; 3] = ["WR_EN", "WR_ADDR", "WR_DATA"];
/// The input pins of a memory read port.
const READ_PORT_PINS: [&str; 3] = ["RD_ADDR", "RD_EN", "RD_SRST"];
/// The pin under which a read port reads the stored word. Not a yosys pin: the word is inside
/// the memory cell.
const STORED_WORD: &str = "MEM";
/// The pins whose captured value is what the cell holds. A capture on any other pin (an enable,
/// a reset, an address) is a control input that samples the other domain directly.
const PAYLOAD_PINS: [&str; 2] = ["D", "WR_DATA"];
/// The pins that qualify a capture: the cell takes its payload only while one is asserted.
const ENABLE_PINS: [&str; 3] = ["EN", "WR_EN", "RD_EN"];

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

/// Port `i`'s share of a memory pin that concatenates `ports` equal shares ([`Netlist::add_memory`]
/// checks that it does before asking).
fn port_slice(pin: &[i64], ports: usize, i: usize) -> Vec<i64> {
    if pin.is_empty() || ports == 0 {
        return Vec::new();
    }
    pin.chunks(pin.len() / ports)
        .nth(i)
        .map(<[i64]>::to_vec)
        .unwrap_or_default()
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
            unread_memories: Vec::new(),
        };
        let empty = serde_json::Map::new();
        let cells = module
            .get("cells")
            .and_then(|c| c.as_object())
            .unwrap_or(&empty);
        // The first bit no net of the design uses: a memory's stored word takes its bits from
        // here on.
        let mut free_bit = cells
            .values()
            .filter_map(|c| c.get("connections")?.as_object())
            .flat_map(|conns| conns.values().flat_map(bits))
            .chain(n.names.keys().copied())
            .max()
            .unwrap_or(0)
            + 1;
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
            if matches!(ty, "$mem" | "$mem_v2") {
                n.add_memory(name, ty, cell, conns, &mut free_bit);
                continue;
            }
            if ty.starts_with("$mem") {
                // `$memrd` / `$memwr` / `$meminit` and their `_v2` forms: the lift's script
                // collects every memory into one cell before this snapshot, so none is expected.
                // One that is here is said in a note; it is not read as logic, whose pins it
                // does not have.
                n.unread_memories
                    .push(format!("`{name}` (an uncollected `{ty}` cell)"));
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
                n.add_loads(Driver::Flop(idx, 0), &data);
                n.flops.push(Flop {
                    name: name.clone(),
                    clk,
                    q,
                    data,
                    stores_memory_word: false,
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

    /// Record `reader` as a load of every bit its pins read.
    fn add_loads(&mut self, reader: Driver, pins: &[(String, Vec<i64>)]) {
        for (pin, bs) in pins {
            for b in bs {
                self.loads
                    .entry(*b)
                    .or_default()
                    .push(Some((reader, pin.clone())));
            }
        }
    }

    /// mununu#1230 — a collected memory (`$mem`, `$mem_v2`), written as the cells the walkers
    /// already know, so that no walker needs a case for it and none can forget one:
    ///
    /// - a clocked write port is a storage cell on its write clock. Its data pins are the port's
    ///   share of `WR_EN`, `WR_ADDR` and `WR_DATA`; its output is one new bit, named after the
    ///   memory, that stands for the stored word;
    /// - an unclocked read port (the only kind the lift's snapshot holds: it runs no
    ///   `memory_dff`) is logic from its `RD_ADDR` and the stored word of every write port to its
    ///   share of `RD_DATA`;
    /// - a clocked read port is a register on its read clock with `RD_DATA` as its output.
    ///
    /// SOUNDNESS: the stored word is one bit per write port, whatever the address: every read
    /// port reads every write port. That over-approximates the paths through the memory (two
    /// ports that never share an address are still connected), so a crossing through it is never
    /// missed; the row it gives is `unknown`, not `violated`.
    ///
    /// A port's clock edge is not read: both edges of one net are one clock here, as for a flop.
    /// A clocked port whose clock is a constant never fires and is left out, as a flop with a
    /// constant clock is; the lift's clock scan counts memory ports, so its note names it.
    fn add_memory(
        &mut self,
        name: &str,
        ty: &str,
        cell: &Value,
        conns: &serde_json::Map<String, Value>,
        free_bit: &mut i64,
    ) {
        let pin = |p: &str| conns.get(p).map(bits).unwrap_or_default();
        let (wr_clk, rd_clk) = (pin("WR_CLK"), pin("RD_CLK"));
        // Every pin holds one equal share per port. A cell where one does not is not split by
        // guessing: it is named in a note and left unread.
        let splits = |pins: &[&str], ports: usize| {
            pins.iter().all(|p| {
                let len = pin(p).len();
                len == 0 || (ports > 0 && len % ports == 0)
            })
        };
        if !splits(&WRITE_PORT_PINS, wr_clk.len())
            || !splits(&READ_PORT_PINS, rd_clk.len())
            || !splits(&["RD_DATA", "RD_ARST"], rd_clk.len())
        {
            self.unread_memories
                .push(format!("`{name}` (its pins do not split into its ports)"));
            return;
        }
        let mut write_clocks = BTreeSet::new();
        let mut stored: Vec<i64> = Vec::new();
        for (i, &clk) in wr_clk.iter().enumerate() {
            if param_bit(cell, "WR_CLK_ENABLE", i) != Some(true) {
                // An unclocked write port stores like a latch. slang + yosys write such a memory
                // as `$dlatch` cells, so this is not a shape the lift gave; if one appears, the
                // latch note says what the lint does with it.
                self.has_latches = true;
                continue;
            }
            if clk < 0 {
                continue;
            }
            let word = *free_bit;
            *free_bit += 1;
            let idx = self.flops.len();
            self.names.insert(word, name.to_string());
            self.driver.insert(word, Driver::Flop(idx, 0));
            let data: Vec<(String, Vec<i64>)> = WRITE_PORT_PINS
                .iter()
                .map(|p| (p.to_string(), port_slice(&pin(p), wr_clk.len(), i)))
                .collect();
            self.add_loads(Driver::Flop(idx, 0), &data);
            self.flops.push(Flop {
                name: name.to_string(),
                clk,
                q: vec![word],
                data,
                stores_memory_word: true,
            });
            write_clocks.insert(clk);
            stored.push(word);
        }
        for (j, &clk) in rd_clk.iter().enumerate() {
            let out = port_slice(&pin("RD_DATA"), rd_clk.len(), j);
            let mut inputs: Vec<(String, Vec<i64>)> = READ_PORT_PINS
                .iter()
                .map(|p| (p.to_string(), port_slice(&pin(p), rd_clk.len(), j)))
                .collect();
            inputs.push((STORED_WORD.to_string(), stored.clone()));
            if param_bit(cell, "RD_CLK_ENABLE", j) == Some(true) {
                if clk < 0 {
                    continue;
                }
                if port_slice(&pin("RD_ARST"), rd_clk.len(), j)
                    .iter()
                    .any(|b| *b >= 0)
                {
                    self.has_async_pins = true;
                }
                let idx = self.flops.len();
                for (k, b) in out.iter().enumerate() {
                    self.driver.insert(*b, Driver::Flop(idx, k));
                }
                self.add_loads(Driver::Flop(idx, 0), &inputs);
                self.flops.push(Flop {
                    name: name.to_string(),
                    clk,
                    q: out,
                    data: inputs,
                    stores_memory_word: false,
                });
            } else {
                let idx = self.combs.len();
                for b in &out {
                    self.driver.insert(*b, Driver::Comb(idx));
                }
                self.add_loads(Driver::Comb(idx), &inputs);
                self.combs.push(Comb {
                    ty: format!("{ty} read port"),
                    src: src_of(cell),
                    inputs,
                });
            }
        }
        self.memories.push(Memory {
            name: name.to_string(),
            write_clocks,
        });
    }

    fn bit_name(&self, b: i64) -> String {
        self.names
            .get(&b)
            .cloned()
            .unwrap_or_else(|| format!("net#{b}"))
    }

    /// A register's name: its `Q` net without the bit index. A memory write port's is the
    /// memory's, which [`Netlist::add_memory`] gave to the bit that stands for the stored word.
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
}

impl<'a> Cones<'a> {
    fn new(net: &'a Netlist) -> Self {
        Cones {
            net,
            memo: HashMap::new(),
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
                    }
                    on_stack.remove(&b);
                    self.memo.insert(b, Rc::new(out));
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
        // mununu#1230 — the source is a word a memory stores on another clock. No synchroniser
        // can sit on that path, and none is asked for: what makes it safe is that the word is
        // not read while it is written, which the pointers decide.
        if net.flops[c.src].stores_memory_word {
            report.crossings.push(row(
                CrossingKind::DualClockMemory,
                PropertyVerdict::Unknown,
                None,
                path_to(net, c.d_bit, c.src),
                "reads, on its own clock, a word the memory stores on another; correct only if \
                 no word is read while it is written (the pointer crossings, and what the write \
                 port captures, are checked as ordinary rows)"
                    .to_string(),
            ));
            continue;
        }
        // A qualifier: the cell's enable, or a mux select on the data path, read from a
        // synchroniser of the source domain.
        let en_bits: Vec<i64> = dst
            .data
            .iter()
            .filter(|(p, _)| ENABLE_PINS.contains(&p.as_str()))
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
        if !PAYLOAD_PINS.contains(&c.pin.as_str()) {
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
            None if dst.stores_memory_word => report.crossings.push(row(
                CrossingKind::Unsynchronized,
                PropertyVerdict::Violated,
                None,
                Vec::new(),
                "the memory stores a value sampled from another domain's flop: the stored word \
                 can be metastable, and a memory word is no synchroniser stage"
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

    // A memory written on more than one clock: the write ports race on a word both address,
    // whatever reads it. The row names the first write clock as the source and the others as
    // the destination.
    for m in &net.memories {
        let mut clocks = m.write_clocks.iter().map(|c| net.clock_name(*c));
        let (Some(first), rest) = (clocks.next(), clocks.collect::<Vec<_>>()) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        report.crossings.push(Crossing {
            kind: CrossingKind::DualClockMemory,
            outcome: PropertyVerdict::Unknown,
            source: m.name.clone(),
            source_clock: first,
            destination: m.name.clone(),
            destination_clock: rest.join(", "),
            stages: None,
            path: Vec::new(),
            detail: "written on more than one clock; correct only if the write ports never \
                     write one word at the same time"
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

/// The first flop of another clock in the sequential cone of `src`: the cone of its data pins and
/// of every flop of its own domain that can influence it. `None` when the cone lies in `src`'s own
/// domain. A memory's write port is a flop on its write clock (mununu#1230), so a register that
/// reads a word stored on another clock is found here like one that reads another domain's
/// register.
fn foreign_cone_source(net: &Netlist, cones: &mut Cones, src: usize) -> Option<usize> {
    let clk = net.flops[src].clk;
    let mut seen = BTreeSet::from([src]);
    let mut stack = vec![src];
    while let Some(f) = stack.pop() {
        for s in cones.of_flop(f) {
            if net.flops[s.flop].clk != clk {
                return Some(s.flop);
            }
            if seen.insert(s.flop) {
                stack.push(s.flop);
            }
        }
    }
    None
}

/// The gray-step invariant on a bus's source register: `holds` when no step flips two bits.
///
/// SOUNDNESS: the query runs on the lift with merged clock domains: every domain steps on the
/// model's one clock. It is exact for the source register when the register's sequential cone (every
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
    if let Some(foreign) = foreign_cone_source(net, cones, src) {
        return (
            PropertyVerdict::Unknown,
            format!(
                "parallel synchronisers on a multi-bit value: coherent only if it is gray coded; \
                 not checked, because its cone reads `{}` from another domain",
                net.register_name(foreign)
            ),
        );
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
    let outcome = decide_reach_portfolio(&mfile);
    match PropertyVerdict::from(outcome.verdict) {
        PropertyVerdict::Holds => (
            PropertyVerdict::Holds,
            format!("`{name}` changes at most one bit per source cycle (gray step decided)"),
        ),
        PropertyVerdict::Violated => (
            PropertyVerdict::Violated,
            gray_step_violated_detail(&name, &outcome),
        ),
        v => unchecked(format!("was not decided ({})", v.as_str())),
    }
}

/// The detail of a `violated` gray-step row. mununu#1175 — a two-bit step found on a path the
/// concrete simulator did not confirm says so after the finding, member by member.
fn gray_step_violated_detail(
    name: &str,
    outcome: &crate::adapter::reach_portfolio::ReachOutcome,
) -> String {
    let mut detail = format!(
        "`{name}` can change two or more bits in one source cycle, so the destination can sample \
         a value the source never held; gray-code it or use a handshake"
    );
    for line in outcome.unconfirmed_notes() {
        detail.push_str(" — ");
        detail.push_str(&line);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mununu#1175 — a two-bit step found on a path the simulator did not confirm says so in
    /// the row's detail; a confirmed one reads as before.
    #[test]
    fn x1175_a_gray_step_found_on_an_unconfirmed_path_says_so() {
        use crate::adapter::reach_portfolio::{ReachOutcome, ReachVerdict};
        let unconfirmed = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["ic3"],
            trace_replay_failed: vec![("ic3", "a 130-bit cell".to_string())],
            ..Default::default()
        };
        let detail = gray_step_violated_detail("ptr", &unconfirmed);
        assert!(
            detail.starts_with("`ptr` can change two or more bits")
                && detail.contains(" — ic3: the path it found was not confirmed")
                && detail.contains("a 130-bit cell"),
            "{detail}"
        );
        let confirmed = ReachOutcome {
            verdict: ReachVerdict::Reachable,
            reachable_by: vec!["native"],
            ..Default::default()
        };
        let detail = gray_step_violated_detail("ptr", &confirmed);
        assert!(!detail.contains("not confirmed"), "{detail}");
    }

    /// A hand-written netlist in yosys `write_json` shape: `clk_a` = bit 2, `clk_b` = bit 3.
    fn netlist(cells: &str, nets: &str) -> String {
        format!(
            r#"{{"modules": {{"x": {{"attributes": {{"top": "00000000000000000000000000000001"}},
              "ports": {{}}, "cells": {{ {cells} }},
              "netnames": {{"clk_a": {{"bits": [2]}}, "clk_b": {{"bits": [3]}}, {nets} }} }} }} }}"#
        )
    }

    /// The lint over a hand netlist, with the domains read from the same netlist.
    fn lint(json: &str) -> CdcReport {
        cdc_lint(json, &crate::adapter::yosys::clock_domains(json), None).unwrap()
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

    /// mununu#1148 — a free-source clock is named in the lint's notes, one line per domain, and an
    /// input or derived clock adds none.
    #[test]
    fn x1148_a_clock_with_no_driver_is_named_in_the_lint_notes() {
        use crate::adapter::yosys::ClockRoot;
        let d = |net: &str, root: ClockRoot| ClockDomain {
            net: net.into(),
            posedge: true,
            cells: 1,
            root,
            input: None,
        };
        let notes = clock_root_notes(&[
            d("clk", ClockRoot::Input),
            d("gated", ClockRoot::Derived("$and".into())),
            d("pll", ClockRoot::Cut),
            d("gclk", ClockRoot::BlackBox("prim_clock_gating".into())),
            d("lost", ClockRoot::Undriven),
            d("pclk", ClockRoot::BlackBoxPort(vec!["pad".into()])),
            d(
                "mclk",
                ClockRoot::MultiDriven(vec!["$and".into(), "osc".into()]),
            ),
            d(
                "1'b0",
                ClockRoot::Constant {
                    value: "0".into(),
                    register: "q".into(),
                },
            ),
        ]);
        assert_eq!(notes.len(), 6, "{notes:#?}");
        assert!(
            notes[3].starts_with("clock `pclk` is on a port of black box `pad`"),
            "{}",
            notes[3]
        );
        assert!(
            notes[4].starts_with("clock `mclk` is driven by more than one cell (`$and`, `osc`)"),
            "{}",
            notes[4]
        );
        // mununu#1180 — a constant clock is no domain of the lint: its note says what the lint
        // does with the cells, not that their crossings are from an independent clock.
        assert!(
            notes[5]
                .starts_with("the clock of `q` is tied to constant 0 (1 storage cell(s) on it)")
                && notes[5].contains("reports no crossing from or to them"),
            "{}",
            notes[5]
        );
        assert!(
            notes[0].starts_with("clock `pll` is driven by a cut net"),
            "{}",
            notes[0]
        );
        assert!(
            notes[1].contains("black box `prim_clock_gating`"),
            "{}",
            notes[1]
        );
        assert!(
            notes[2].starts_with("clock `lost` is driven by nothing"),
            "{}",
            notes[2]
        );
        assert!(notes[..5].iter().all(|n| n.contains("independent clock")));
        assert!(!notes[5].contains("independent clock"), "{}", notes[5]);
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
        let r = lint(&netlist(&cells, nets));
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
        let r = lint(&netlist(&cells, nets));
        assert_eq!(kinds(&r), vec![("unsynchronized", "violated")], "{r:#?}");
    }

    /// mununu#1189 — what the lint's cut-point line says, on the netlist a cut leaves: the crossing
    /// of `a_capture_read_by_logic_is_unsynchronized` with the crossing net cut. Yosys `cutpoint`
    /// drives the net from an `$anyseq` cell and leaves the register's output dangling, so the
    /// capture reads a net with no clocked source and the lint finds no crossing.
    #[test]
    fn x1189_a_crossing_through_a_cut_net_is_not_seen() {
        let free = r#""cut": {"type": "$anyseq", "port_directions": {"Y": "output"},
            "connections": {"Y": [8]}}"#;
        let cells = [
            dff("ra", 2, "4", "30"),
            free.to_string(),
            dff("m", 3, "8", "11"),
            and("g", 11, 5, 10),
            dff("y", 3, "10", "12"),
        ]
        .join(",");
        let nets = r#""ra": {"bits": [8]}, "m": {"bits": [11]}, "y": {"bits": [12]}"#;
        let r = lint(&netlist(&cells, nets));
        assert!(r.crossings.is_empty(), "{r:#?}");
        assert_eq!(
            r.domains.len(),
            2,
            "both clocks still clock a register: {r:#?}"
        );
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
        let r = lint(&netlist(&cells, nets));
        assert_eq!(kinds(&r), vec![("multi-bit-bus", "unknown")], "{r:#?}");
        assert_eq!(r.crossings[0].source, "st");
        assert_eq!(r.crossings[0].destination, "m0, m1");
    }

    #[test]
    fn one_clock_has_no_crossing() {
        let cells = [dff("ra", 2, "4", "8"), dff("m", 2, "8", "11")].join(",");
        let r = lint(&netlist(&cells, r#""ra": {"bits": [8]}"#));
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
                include_dirs: Vec::new(),
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

    /// mununu#1148 — the lint through the real lift on a clock the design holds no driver for,
    /// in the two shapes yosys leaves (a declared empty black box becomes a cut net, a
    /// missing-module stub stays a cell of its own type): the lint runs — it was refused as "a
    /// verb that drops the roots" — finds the crossing into that domain, and names the root in
    /// its notes. Needs yosys.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_cdc_lint_lints_a_clock_rooted_at_a_black_box_and_names_the_root() {
        use crate::adapter::sv_verify::{SvLift, sv_cdc_lint};
        use crate::adapter::yosys::MISSING_MODULE_ATTR;
        const TOP: &str = "\
module top(input clk, input en, input d, output reg q, output reg r);
  wire gclk;
  prim_clock_gating u_cg(.clk_i(clk), .en_i(en), .clk_o(gclk));
  always @(posedge clk) r <= d;
  always @(posedge gclk) q <= r;
endmodule
";
        let declared = format!(
            "module prim_clock_gating(input clk_i, input en_i, output clk_o);\nendmodule\n{TOP}"
        );
        let stub = format!(
            "(* {MISSING_MODULE_ATTR} *)\nmodule prim_clock_gating(input clk_i, input en_i, \
             output clk_o);\nendmodule\n{TOP}"
        );
        for (name, src, root) in [
            ("declared", declared, "a cut net"),
            ("stub", stub, "black box `prim_clock_gating`"),
        ] {
            let report = sv_cdc_lint(&SvLift {
                primary_name: "top.sv".to_string(),
                source: src,
                additional_sources: Vec::new(),
                top: Some("top".to_string()),
                include_dirs: Vec::new(),
                merge_clock_domains: false,
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{name}: the lint lifts a black-box clock: {e}"));
            assert!(
                report.domains.iter().any(|d| d.net == "gclk"),
                "{name}: {report:#?}"
            );
            assert!(
                report.crossings.iter().any(|c| c.source == "r"),
                "{name}: the crossing from `r` into the black-box domain: {report:#?}"
            );
            assert!(
                report
                    .notes
                    .iter()
                    .any(|n| n.starts_with("clock `gclk` is driven by") && n.contains(root)),
                "{name}: {:#?}",
                report.notes
            );
        }
    }

    /// mununu#1229 — the lint reads the scan's snapshot, so it missed what the scan missed: a
    /// second clock domain whose storage no output port reads. With the flop of the second
    /// domain read by assertions alone, the lint answered `holds` on one domain ("one clock
    /// net"); the crossing from `seen` into `clk_b` has no synchroniser. With a memory there
    /// instead, the lint lists both domains, and the write port that takes its enable from the
    /// first domain is a crossing (mununu#1230, whose own cases are `memory_cases`). Needs yosys.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_x1229_cdc_lint_sees_a_domain_whose_storage_no_output_port_reads() {
        use crate::adapter::sv_verify::{SvLift, sv_cdc_lint};
        const FLOP: &str = "\
module top(input logic clk_a, input logic clk_b, input logic rst, input logic req,
           output logic gnt);
  logic seen, mon;
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (rst) mon <= 1'b0;
    else mon <= seen;
  assign gnt = seen;
  a_mon: assert property (@(posedge clk_b) !(mon && rst));
endmodule
";
        const MEMORY: &str = "\
module top(input logic clk_a, input logic clk_b, input logic rst, input logic req,
           input logic [1:0] wa, input logic wd, output logic gnt);
  logic seen;
  logic m [4];
  logic x;
  always_ff @(posedge clk_a)
    if (rst) seen <= 1'b0;
    else seen <= req;
  always_ff @(posedge clk_b)
    if (seen) m[wa] <= wd;
  assign x = m[wa] & ~rst;
  assign gnt = seen;
  a_x: assert property (@(posedge clk_a) !(x && rst));
endmodule
";
        let lint = |name: &str, src: &str| {
            sv_cdc_lint(&SvLift {
                primary_name: "top.sv".to_string(),
                source: src.to_string(),
                additional_sources: Vec::new(),
                top: Some("top".to_string()),
                include_dirs: Vec::new(),
                merge_clock_domains: false,
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{name}: the lint lifts a two-clock design: {e}"))
        };
        let nets = |report: &CdcReport| -> Vec<String> {
            report.domains.iter().map(|d| d.net.clone()).collect()
        };
        let flop = lint("flop", FLOP);
        assert_eq!(nets(&flop), ["clk_a", "clk_b"], "{flop:#?}");
        assert!(
            flop.crossings
                .iter()
                .any(|c| c.destination == "mon" && c.destination_clock == "clk_b"),
            "the crossing into `mon`, in the second domain: {flop:#?}"
        );
        assert_eq!(flop.worst(), PropertyVerdict::Violated, "{flop:#?}");
        let memory = lint("memory", MEMORY);
        assert_eq!(nets(&memory), ["clk_a", "clk_b"], "{memory:#?}");
        assert!(
            memory
                .crossings
                .iter()
                .any(|c| c.destination == "m" && c.destination_clock == "clk_b"),
            "the crossing into the write port of `m`, in the second domain: {memory:#?}"
        );
        assert_eq!(memory.worst(), PropertyVerdict::Violated, "{memory:#?}");
    }

    /// What a test compares of a row: kind, outcome, source, destination.
    type Row = (CrossingKind, PropertyVerdict, &'static str, &'static str);

    /// mununu#1230 — one way a memory takes part in a path between two storage elements: the
    /// design, the snapshot the lift's clock-scan script gives for it, and the rows the lint
    /// reports, in report order.
    ///
    /// The snapshots under `testdata/cdc_lint/` were captured on 2026-11-23 with yosys 0.68+120
    /// in the `mununu-sva` image, by the script of the lift's clock scan on the source beside
    /// each, and reduced to the top module's cells and named nets. The `e2e_x1230_` test runs
    /// the same sources through the real lift, so a snapshot that stops being what the lift
    /// gives is found there.
    struct MemoryCase {
        name: &'static str,
        sv: &'static str,
        snapshot: &'static str,
        rows: &'static [Row],
        /// A scope note the report must carry.
        note: Option<&'static str>,
    }

    macro_rules! memory_case {
        ($name:literal, $rows:expr, $note:expr) => {
            MemoryCase {
                name: $name,
                sv: include_str!(concat!("testdata/cdc_lint/", $name, ".sv")),
                snapshot: include_str!(concat!("testdata/cdc_lint/", $name, ".json")),
                rows: $rows,
                note: $note,
            }
        };
    }

    fn memory_cases() -> Vec<MemoryCase> {
        use CrossingKind::{CombinationalBeforeSync, DualClockMemory, MultiBitBus, Unsynchronized};
        use PropertyVerdict::{Unknown, Violated};
        vec![
            // A clocked write port captures what its pins read. yosys builds the enable of
            // `if (c) m[a] <= d` out of muxes, so an enable is read through logic.
            memory_case!(
                "write_enable_from_another_domain",
                &[(CombinationalBeforeSync, Violated, "seen", "m")],
                None
            ),
            memory_case!(
                "write_data_from_another_domain",
                &[(Unsynchronized, Violated, "seen", "m")],
                None
            ),
            memory_case!(
                "write_address_from_another_domain",
                &[(Unsynchronized, Violated, "seen", "m")],
                None
            ),
            // The stored word is a source in its write clock's domain.
            memory_case!(
                "word_registered_in_another_domain",
                &[(DualClockMemory, Unknown, "m", "q")],
                None
            ),
            memory_case!(
                "word_registered_under_an_enable",
                &[(DualClockMemory, Unknown, "m", "q")],
                None
            ),
            memory_case!(
                "memory_in_a_sub_instance",
                &[(DualClockMemory, Unknown, "u_ram.m", "q")],
                None
            ),
            // The pointers of this FIFO do not cross: each side keeps its own.
            memory_case!(
                "two_clock_fifo",
                &[(DualClockMemory, Unknown, "m", "q")],
                None
            ),
            memory_case!(
                "two_write_clocks",
                &[(DualClockMemory, Unknown, "m", "m")],
                None
            ),
            // The read address is on the path to the read data. The memory is written and read
            // in one domain, so the stored word is no crossing.
            memory_case!(
                "read_address_from_another_domain",
                &[(CombinationalBeforeSync, Violated, "ra", "q")],
                None
            ),
            // A synchroniser's first stage that a write port reads as well is not a clean chain;
            // the second case is the same design with a register in place of the memory.
            memory_case!(
                "synchroniser_stage_also_stored",
                &[(Unsynchronized, Violated, "a", "s1")],
                None
            ),
            memory_case!(
                "synchroniser_stage_also_registered",
                &[(Unsynchronized, Violated, "a", "s1")],
                None
            ),
            // A bus whose source register reads another domain: the gray-step check is not run.
            // The second case is the same design with a register in place of the memory.
            memory_case!(
                "bus_source_reads_a_memory",
                &[
                    (MultiBitBus, Unknown, "ptr", "s1[0], s1[1]"),
                    (DualClockMemory, Unknown, "m", "ptr"),
                ],
                None
            ),
            memory_case!(
                "bus_source_reads_a_register",
                &[
                    (CombinationalBeforeSync, Violated, "h", "ptr"),
                    (MultiBitBus, Unknown, "ptr", "s1[0], s1[1]"),
                ],
                None
            ),
            // yosys writes a latch-written memory as latches: there is no memory cell.
            memory_case!("latch_written_memory", &[], Some(LATCH_NOTE)),
            memory_case!("one_clock_memory", &[], None),
        ]
    }

    fn memory_case(name: &str) -> MemoryCase {
        memory_cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no memory case `{name}`"))
    }

    fn rows(r: &CdcReport) -> Vec<(CrossingKind, PropertyVerdict, &str, &str)> {
        r.crossings
            .iter()
            .map(|c| (c.kind, c.outcome, c.source.as_str(), c.destination.as_str()))
            .collect()
    }

    /// What every memory case's report must be: its rows, its note, and no memory left unread.
    fn assert_memory_case(case: &MemoryCase, report: &CdcReport) {
        assert_eq!(rows(report), case.rows, "{}: {report:#?}", case.name);
        if let Some(note) = case.note {
            assert!(
                report.notes.iter().any(|n| n == note),
                "{}: {:#?}",
                case.name,
                report.notes
            );
        }
        assert!(
            !report
                .notes
                .iter()
                .any(|n| n.starts_with(UNREAD_MEMORY_NOTE)),
            "{}: {:#?}",
            case.name,
            report.notes
        );
    }

    /// mununu#1230 — the lint read a memory in one place, a row that compared a memory's read
    /// clocks with its write clocks, which the lift's snapshot never triggers (its read ports
    /// are unclocked). Every other walker stopped at the cell: ten of these designs were `holds`
    /// with a crossing in them, and one had a `synchronized` row for a stage that is stored.
    /// The rows do not depend on the scan's domains, which feed the notes, so none is passed.
    #[test]
    fn x1230_every_way_a_memory_takes_part_in_a_crossing_has_its_row() {
        for case in memory_cases() {
            let report =
                cdc_lint(case.snapshot, &[], None).unwrap_or_else(|e| panic!("{}: {e}", case.name));
            assert_memory_case(&case, &report);
        }
    }

    /// mununu#1230 — the gray-step check of a bus row is exact only when the source register's
    /// cone lies in its own domain ([`gray_step_verdict`]). The cone walk ended at a memory, so
    /// a source that read a word stored on another clock passed, and the check ran.
    #[test]
    fn x1230_a_word_stored_on_another_clock_is_in_a_register_s_cone() {
        let foreign = |case: &str, register: &str| {
            let case = memory_case(case);
            let root: Value = serde_json::from_str(case.snapshot).unwrap();
            let net = Netlist::new(top_module(&root).unwrap());
            let src = (0..net.flops.len())
                .find(|f| net.register_name(*f) == register)
                .unwrap_or_else(|| panic!("{}: no register `{register}`", case.name));
            let mut cones = Cones::new(&net);
            foreign_cone_source(&net, &mut cones, src)
                .map(|f| (net.register_name(f), net.flops[f].stores_memory_word))
        };
        assert_eq!(
            foreign("bus_source_reads_a_memory", "ptr"),
            Some(("m".to_string(), true))
        );
        assert_eq!(
            foreign("bus_source_reads_a_register", "ptr"),
            Some(("h".to_string(), false))
        );
        // A register that reads input ports only: its cone is its own domain.
        assert_eq!(foreign("write_data_from_another_domain", "seen"), None);
    }

    /// mununu#1230 — a clocked read port is not a shape the lift's script gives (it runs no
    /// `memory_dff`), and is read all the same: a register on its read clock that takes the
    /// stored word. The snapshot is `word_registered_in_another_domain` with `memory_dff` run by
    /// hand before the memory is collected.
    #[test]
    fn x1230_a_clocked_read_port_is_a_register_on_its_read_clock() {
        let report = cdc_lint(
            include_str!("testdata/cdc_lint/clocked_read_port.json"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(
            rows(&report),
            [(
                CrossingKind::DualClockMemory,
                PropertyVerdict::Unknown,
                "m",
                "q"
            )],
            "{report:#?}"
        );
    }

    /// A hand `$mem_v2` cell: `WR_CLK_ENABLE` / `RD_CLK_ENABLE` as yosys writes them (one
    /// character per port) and each pin as the list of its bits, the ports one after another.
    fn mem(name: &str, wr_clk_enable: &str, rd_clk_enable: &str, pins: &[(&str, &str)]) -> String {
        let conns: Vec<String> = pins
            .iter()
            .map(|(pin, bits)| format!(r#""{pin}": [{bits}]"#))
            .collect();
        format!(
            r#""{name}": {{"type": "$mem_v2",
                "parameters": {{"WR_CLK_ENABLE": "{wr_clk_enable}", "RD_CLK_ENABLE": "{rd_clk_enable}"}},
                "connections": {{ {} }} }}"#,
            conns.join(", ")
        )
    }

    /// `ra` (bit 8) is a `clk_a` register and `q` (bit 11) a `clk_b` register that reads bit 10.
    fn read_side() -> (String, &'static str) {
        (
            [dff("ra", 2, "4", "8"), dff("q", 3, "10", "11")].join(","),
            r#""ra": {"bits": [8]}, "q": {"bits": [11]}"#,
        )
    }

    /// mununu#1230 — a memory with no write port holds nothing a clock stores: its read port is
    /// logic over the address alone.
    #[test]
    fn x1230_a_memory_with_no_write_port_is_logic_over_its_read_address() {
        let (flops, nets) = read_side();
        let rom = mem(
            "rom",
            "",
            "0",
            &[("RD_CLK", r#""x""#), ("RD_ADDR", "8"), ("RD_DATA", "10")],
        );
        let r = lint(&netlist(&format!("{flops},{rom}"), nets));
        assert_eq!(
            rows(&r),
            [(
                CrossingKind::CombinationalBeforeSync,
                PropertyVerdict::Violated,
                "ra",
                "q"
            )],
            "{r:#?}"
        );
        assert!(!r.notes.iter().any(|n| n.starts_with(UNREAD_MEMORY_NOTE)));
    }

    /// mununu#1230 — an unclocked write port stores like a latch, and the latch note says what
    /// the lint does with it: the word it stores is no source.
    #[test]
    fn x1230_an_unclocked_write_port_is_under_the_latch_note() {
        let (flops, nets) = read_side();
        let ram = mem(
            "ram",
            "0",
            "0",
            &[
                ("WR_CLK", r#""x""#),
                ("WR_EN", "5"),
                ("WR_ADDR", "6"),
                ("WR_DATA", "8"),
                ("RD_CLK", r#""x""#),
                ("RD_ADDR", "7"),
                ("RD_DATA", "10"),
            ],
        );
        let r = lint(&netlist(&format!("{flops},{ram}"), nets));
        assert!(r.crossings.is_empty(), "{r:#?}");
        assert!(r.notes.iter().any(|n| n == LATCH_NOTE), "{:#?}", r.notes);
    }

    /// mununu#1230 — a memory cell the lint cannot read is named in a note, each with its cell,
    /// and is never read as something else: a memory whose pins do not split into its ports,
    /// and a memory port the lift's script left uncollected.
    #[test]
    fn x1230_a_memory_cell_the_lint_cannot_read_is_named_in_a_note() {
        let (flops, nets) = read_side();
        // Two write ports and a three-bit address pin.
        let odd = mem(
            "odd_ram",
            "11",
            "0",
            &[
                ("WR_CLK", "2, 3"),
                ("WR_EN", "5, 5"),
                ("WR_ADDR", "6, 6, 6"),
                ("WR_DATA", "8, 8"),
                ("RD_CLK", r#""x""#),
                ("RD_ADDR", "7"),
                ("RD_DATA", "10"),
            ],
        );
        let loose = r#""loose_port": {"type": "$memrd_v2",
            "connections": {"CLK": ["x"], "EN": ["1"], "ADDR": [8], "DATA": [12]}}"#;
        let r = lint(&netlist(&format!("{flops},{odd},{loose}"), nets));
        let note = r
            .notes
            .iter()
            .find(|n| n.starts_with(UNREAD_MEMORY_NOTE))
            .unwrap_or_else(|| panic!("{:#?}", r.notes));
        assert!(
            note.contains("`odd_ram`") && note.contains("`loose_port`"),
            "{note}"
        );
        assert!(r.crossings.is_empty(), "{r:#?}");
    }

    /// mununu#1230 — the storage the lint does not follow, cell kind by cell kind: each is under
    /// its note, or is no storage. A register's asynchronous pin and a latch have their notes; a
    /// register whose clock pin is a constant never clocks, so it captures nothing and its output
    /// is no source. (A black box's note needs the lift: the `e2e_x1230_` test below.)
    #[test]
    fn x1230_storage_the_lint_does_not_follow_is_under_a_note_or_stores_nothing() {
        let nets = r#""ra": {"bits": [8]}, "q": {"bits": [11]}"#;
        let with = |cell: &str, q_reads: i64| {
            let cells = [
                dff("ra", 2, "4", "8"),
                dff("q", 3, &q_reads.to_string(), "11"),
                cell.to_string(),
            ]
            .join(",");
            lint(&netlist(&cells, nets))
        };
        let async_reset = with(
            r#""r": {"type": "$adff",
                "port_directions": {"CLK": "input", "ARST": "input", "D": "input", "Q": "output"},
                "connections": {"CLK": [3], "ARST": [5], "D": [6], "Q": [9]}}"#,
            9,
        );
        assert!(
            async_reset.notes.iter().any(|n| n == ASYNC_PIN_NOTE),
            "{:#?}",
            async_reset.notes
        );
        let latch = with(
            r#""l": {"type": "$dlatch",
                "port_directions": {"EN": "input", "D": "input", "Q": "output"},
                "connections": {"EN": [5], "D": [8], "Q": [9]}}"#,
            9,
        );
        assert!(
            latch.notes.iter().any(|n| n == LATCH_NOTE),
            "{:#?}",
            latch.notes
        );
        assert!(latch.crossings.is_empty(), "{latch:#?}");
        let never_clocked = with(
            r#""k": {"type": "$dff",
                "port_directions": {"CLK": "input", "D": "input", "Q": "output"},
                "connections": {"CLK": ["0"], "D": [8], "Q": [9]}}"#,
            9,
        );
        assert!(never_clocked.crossings.is_empty(), "{never_clocked:#?}");
    }

    /// mununu#1230 — the memory cases through the real lift: the snapshots are what the lift
    /// gives, and the rows hold with the lifted model at hand (a bus row whose source reads
    /// another domain stays `unknown`, because the gray-step query is not run). Needs yosys.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_x1230_every_memory_case_has_its_row_through_the_real_lift() {
        use crate::adapter::sv_verify::{SvLift, sv_cdc_lint};
        for case in memory_cases() {
            let report = sv_cdc_lint(&SvLift {
                primary_name: "work.sv".to_string(),
                source: case.sv.to_string(),
                additional_sources: Vec::new(),
                top: Some("top".to_string()),
                include_dirs: Vec::new(),
                merge_clock_domains: false,
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
            assert_memory_case(&case, &report);
        }
    }

    /// mununu#1230 — the cell kind whose note the lift writes: a value that leaves one domain
    /// through a black box and is registered in another gets no row, and the report names the
    /// black box. Needs yosys.
    #[test]
    #[ignore = "requires yosys (mununu-sva image)"]
    fn e2e_x1230_a_value_through_a_black_box_is_under_the_black_box_note() {
        use crate::adapter::sv_verify::{SvLift, sv_cdc_lint};
        const DESIGN: &str = r#"
module opaque_cell(input logic i, output logic o);
endmodule
module top(input logic clk_a, input logic clk_b, input logic d, output logic y);
  logic a, x, q;
  always_ff @(posedge clk_a) a <= d;
  opaque_cell u(.i(a), .o(x));
  always_ff @(posedge clk_b) q <= x;
  assign y = q;
endmodule
"#;
        let report = sv_cdc_lint(&SvLift {
            primary_name: "top.sv".to_string(),
            source: DESIGN.to_string(),
            additional_sources: Vec::new(),
            top: Some("top".to_string()),
            include_dirs: Vec::new(),
            merge_clock_domains: false,
            ..Default::default()
        })
        .unwrap();
        assert!(report.crossings.is_empty(), "{report:#?}");
        assert!(
            report.notes.iter().any(|n| n.contains("opaque_cell")),
            "{:#?}",
            report.notes
        );
    }
}
