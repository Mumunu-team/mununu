//! The planner's per-property routing rationale as DATA (mununu#964): what the planner predicts
//! for each row, with its cost estimate, and why a row has none. The planner
//! ([`crate::planner`]) computes these; the engine report (`AutoVerifyReport::routing`) and the
//! plan-only report carry them. Kept in a leaf module so the report types need nothing from the
//! planner — the planner depends on the engines, never the other way round.

/// One property's predicted routing + rationale (P2.2d). A prediction, not a verdict.
#[derive(Debug, Clone)]
pub struct RoutingRationale {
    /// Property name.
    pub property: String,
    /// Fixpoint class (safety / reachability / liveness-νμ / mixed / propositional).
    pub class: crate::mu_calculus::PropertyClass,
    /// The exact engine's cone-of-influence bit width for the property's atoms.
    pub cone_bits: u32,
    /// The exact engine's effective bit cap for that cone.
    pub cap: u32,
    /// The structural diameter proxy — the widest in-cone down-counter (`Some(W)` ⇒ ~2^W
    /// diameter), or `None`. Free/static (`ModelFacts::cone_counter_diameter_log2`).
    pub diameter_log2: Option<u32>,
    /// The engine the planner predicts will decide this property.
    pub predicted_engine: &'static str,
    /// The one-line rationale.
    pub why: String,
    /// Roadmap J2 — the static shape facts the cost estimate is keyed by (all read off the cone,
    /// no solving). Default (all zero / false) where they were not computed.
    pub shape: ShapeFacts,
}

/// Roadmap J2 — what the property's cone and formula look like, statically: the features the
/// measured cost table ([`RoutingRationale::estimate`]) is keyed by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShapeFacts {
    /// Multiplier / divider / remainder cells in the cone — the exponential-BDD marker (Bryant).
    pub arith_ops: u32,
    /// Array-sorted states (memories) in the cone — the exact engine refuses them.
    pub array_states: u32,
    /// Primary inputs in the cone (inputs-in-the-state widens it).
    pub inputs: u32,
    /// The formula's fixpoint alternation depth (2 = a μ under a ν: recoverability).
    pub alternation: u32,
    /// The formula is constant on every model (decided without an engine).
    pub constant: bool,
    /// Register-versus-register comparison atoms (`a == b`, `a == b + k`, `a == a__past`): the
    /// atom shape the predicate cube cannot seed as a dimension, so the exact skip-rescue pass
    /// runs on the row as well (edn: all 16 such rows 13–20 s; 48 of the other 51 under 10 s).
    pub relational_atoms: u32,
}

/// Roadmap J2 — a wall-time band: `seconds` < 10 s, `minutes` < 10 min, `hour` < 1 h, `wall` beyond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBand {
    Seconds,
    Minutes,
    Hour,
    Wall,
}

impl TimeBand {
    pub fn tag(self) -> &'static str {
        match self {
            TimeBand::Seconds => "seconds",
            TimeBand::Minutes => "minutes",
            TimeBand::Hour => "hour",
            TimeBand::Wall => "wall",
        }
    }
}

/// Roadmap J2 — the memory a row needs: `small` = no exact BDD work expected; `medium` = exact
/// BDD within the default arena (the bit-cap probes peak at ≤ 12 M nodes); `large` = the arena
/// may overflow (a multiplier) or the cap ladder runs. A sizing class, not a byte count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryClass {
    Small,
    Medium,
    Large,
}

impl MemoryClass {
    pub fn tag(self) -> &'static str {
        match self {
            MemoryClass::Small => "small",
            MemoryClass::Medium => "medium",
            MemoryClass::Large => "large",
        }
    }
}

/// Roadmap J2 — how much the table cell has been measured: `high` = every measured row in the
/// band; `medium` = most; `low` = thin evidence or a named miss class — size for the band's top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn tag(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// Roadmap J2 — the cost estimate of one property: a band, a memory class, a confidence, and the
/// table row it came from. A scheduler hint, never a verdict input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub time_band: TimeBand,
    pub memory_class: MemoryClass,
    pub confidence: Confidence,
    /// The table row, with its evidence (which measured rows put it there).
    pub basis: &'static str,
}

impl RoutingRationale {
    /// Roadmap J2 — the cost estimate, from a table keyed by static shape and measured on the
    /// second host (`mununu-private` `e2e-history/j2-falsifier/`, 2026-11-08/09: edn's 67 rows on
    /// `19295f9`, the 21-test differential corpus's 705 rows, the EXP-0031 bit-cap probes). The
    /// falsifier: 57 of edn's 67 rows in the predicted band, none predicted `seconds` running over
    /// 10 min. Its named miss class: on an in-cap exact row with an in-cone counter the wall follows
    /// the VERDICT's fixpoint depth (`fsm-timeout` 0.3 s vs `fsm-timeout-stuck` 108.6 s at the same
    /// width), which no static feature sees — so that cell says `minutes` with `low` confidence,
    /// and a scheduler sizes for the band's top. The cells are checked in order; the first match wins.
    pub fn estimate(&self) -> Estimate {
        use crate::mu_calculus::PropertyClass as C;
        let e = |time_band, memory_class, confidence, basis| Estimate {
            time_band,
            memory_class,
            confidence,
            basis,
        };
        let s = &self.shape;
        if s.constant {
            return e(
                TimeBand::Seconds,
                MemoryClass::Small,
                Confidence::High,
                "constant formula, decided without an engine (edn: 14 of 14 in seconds)",
            );
        }
        if s.array_states > 0 {
            return e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Low,
                "memory in the cone: the exact engine refuses arrays, the cube or the reach \
                 rescue decides (F1 fixture: under 0.2 s; thin evidence, so the band's top)",
            );
        }
        let fits = self.cone_bits <= self.cap;
        // An alternating fixpoint costs minutes where the recoverability ladder runs — over the
        // cap. In the cap the exact engine decides it directly: the differential corpus's 19
        // in-cap alternating rows (6-38 b) all decided in 1 s or less (the box's out-of-sample
        // check, 2026-11-09), so they fall to the in-cap cells below.
        if s.alternation >= 2 && !fits {
            return e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Medium,
                "over-cap alternating fixpoint (a μ under a ν): the recoverability ladder or a \
                 trap certificate (edn: 2 of 3 in minutes, ~127 s; 1 in seconds)",
            );
        }
        // A multiplier is the BDD's wall (Bryant), so the cell applies where the exact engine
        // runs — in the cap. Over the cap the reach portfolio's SAT-based members decide, and a
        // multiplier is not their wall: edn's 23 over-cap rows with one measured 1.3-7.4 s, and
        // this cell placed them in `minutes` (the box's end-to-end `--plan-only`, 2026-11-09).
        if s.arith_ops > 0 && fits {
            return e(
                TimeBand::Minutes,
                MemoryClass::Large,
                Confidence::Low,
                "in-cap multiplier / divider: the exact BDD walls near 19 b (Bryant) and the \
                 rescue ladder runs about a minute (bit-cap probe `mult`: 58.9 s at 193 b)",
            );
        }
        // Over the cap only: in the cap the exact engine decides a relational atom directly (the
        // bit-cap probe `relational` decides `a == b` at 1 023 b in 0.2 s).
        if s.relational_atoms > 0 && !fits {
            return e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Medium,
                "over-cap register-versus-register atom (`a == b`, a stability check against \
                 `__past`): the predicate cube cannot seed it, so the exact skip-rescue pass \
                 runs too (edn: 16 of 16 in minutes, 13-20 s)",
            );
        }
        match (self.class, fits, self.diameter_log2) {
            (C::Safety | C::Reachability, false, _) => e(
                TimeBand::Seconds,
                MemoryClass::Small,
                Confidence::Medium,
                "over-cap one-step `bad`-reachability, decided by the reach portfolio (edn: 33 \
                 of 33 in seconds; differential corpus, all classes: 643 of 705 rows in \
                 seconds, the slowest 65 s)",
            ),
            (C::Safety | C::Reachability, true, Some(_)) => e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Low,
                "in-cap exact with an in-cone counter: the wall follows the verdict's fixpoint \
                 depth (bit-cap probes: fsm-timeout 0.3 s vs fsm-timeout-stuck 108.6 s at the \
                 same width, twocount 294.9 s at 830 b), so size for the band's top",
            ),
            (C::Safety | C::Reachability, true, None) => e(
                TimeBand::Seconds,
                MemoryClass::Medium,
                Confidence::Medium,
                "in-cap exact, no in-cone counter (bit-cap probes fsm-pair, relational, raster, \
                 i2c, aes: 2 s or less)",
            ),
            (C::Liveness, true, None) => e(
                TimeBand::Seconds,
                MemoryClass::Medium,
                Confidence::Low,
                "in-cap liveness, no counter: exact decides the branching fixpoint (few \
                 measured rows)",
            ),
            (C::Liveness, _, _) => e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Low,
                "liveness with a counter or over the cap: a ranking certificate or the cube \
                 (few measured rows)",
            ),
            (C::Mixed | C::Propositional, true, None) => e(
                TimeBand::Seconds,
                MemoryClass::Medium,
                Confidence::Low,
                "in-cap mixed / propositional, no counter: the exact engine decides it directly \
                 (differential corpus: in-cap alternating rows 1 s or less)",
            ),
            (C::Mixed | C::Propositional, _, _) => e(
                TimeBand::Minutes,
                MemoryClass::Medium,
                Confidence::Low,
                "mixed / propositional with a counter or over the cap: few measured rows",
            ),
        }
    }
}

/// mununu#941 — a property the planner could not estimate, and why. Three causes used to end the
/// same way (no rationale, `routing: null`), and the CLI named only one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingMiss {
    pub property: String,
    pub reason: &'static str,
}

/// The model did not parse as BTOR2 for the planner.
pub const MISS_MODEL_UNPARSED: &str =
    "the lifted model did not parse for the planner, so no property has an estimate";
/// The row's formula did not parse as a μ-calculus formula for the planner.
pub const MISS_FORMULA_UNPARSED: &str = "the formula did not parse for the planner";
/// mununu#577 — the cone's width could not be established.
pub const MISS_CONE_WIDTH_UNKNOWN: &str =
    "the cone width could not be established (a cone that reads a memory, mununu#577)";
