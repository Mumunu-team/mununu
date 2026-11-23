//! mununu#1189 — what an SV lift decided besides the model, as the notes a report owes its reader.
//!
//! The lift returns, with the model, the modules it cut because they have no body, whether it
//! re-ran its yosys script, and the clock domains; the request adds the nets it asked to cut and
//! the parameters it overrode; `sv verify-auto`'s lift, which `sv plan` and `sv mutate` share,
//! may add a behavioural stub for a known flop primitive. Each is a fact about the MODEL a
//! verb's result is read on.
//! `sv verify-auto` reports them in its `notes`; until this module every other verb kept the
//! model and dropped the rest (the clock domains excepted, since mununu#1181).
//!
//! The notes are not a copy of `sv verify-auto`'s. A cut module and a cut point both leave nets
//! free, an over-approximation of the design, and what an over-approximation means depends on
//! what the verb computes: it keeps a HOLDS of a response property, keeps neither verdict of
//! `AG EF`, and for a scan or a lint it changes what was looked at. [`LiftReader`] names the
//! reader and [`LiftReader::on_a_free_net`] is the one place that says what each may conclude.
//!
//! That holds while the clocks of the model are the design's. A lift with a clock note (merged
//! domains, a clock with no driver the model can follow) ticks every clock on every step, which
//! takes runs away from the model: with free nets on top it is neither an over- nor an
//! under-approximation, and [`LiftReader::on_a_free_net_under_a_clock_note`] says what is left.
//!
//! One builder, [`lift_decision_notes`], serves every verb that returns `lift_notes`;
//! [`cdc_lint_lift_lines`] is its peer for `sv cdc-lint`, whose notes are plain lines. The table
//! test `x1189_every_reader_reports_every_lift_decision` enumerates decision × reader.

use crate::adapter::slang::verify_auto::{
    AutoVerifyReport, CLOCK_ROOT_NOTE_KINDS, NoteLevel, VerificationNote, clock_notes_of_report,
    lift_clock_notes,
};
use crate::adapter::yosys::{SvLiftResult, YosysOptions};

/// The note kind of a lift whose yosys script ran twice.
pub(crate) const LIFT_RETRY: &str = "lift-retry";
/// The note kind of a parameter set before elaboration.
pub(crate) const PARAMETER_OVERRIDE: &str = "parameter-override";
/// The note kind of a net the request cut to a free value.
pub(crate) const CONTROL_SLICE: &str = "control-slice";
/// The note kind of a known flop primitive the lift supplied a behavioural stub for.
pub(crate) const FLOP_STUB: &str = "flop-stub";
/// The note kind of a module with no body, cut to free outputs.
pub(crate) const BLACKBOX_CUT: &str = "blackbox-cut";

/// The note kind of the clock domains a lift merged onto the model's one clock (mununu#1181).
pub(crate) const CLOCK_DOMAINS_MERGED: &str = "clock-domains-merged";

/// The sentence a cut note says only when the lift has no clock note: with the design's own
/// clocks, a free net leaves the model an over-approximation. A constant, so a test asks whether
/// a note makes the claim without reading its prose.
pub(crate) const EVERY_RUN_AND_MORE: &str =
    "The model therefore has every run of the design and more.";

/// The five kinds above, in the order a report lists them.
#[cfg(test)]
pub(crate) const LIFT_DECISION_KINDS: [&str; 5] = [
    LIFT_RETRY,
    PARAMETER_OVERRIDE,
    CONTROL_SLICE,
    FLOP_STUB,
    BLACKBOX_CUT,
];

/// What reads the lifted model: the verb, by what it computes on it. A free net means something
/// different to each ([`Self::on_a_free_net`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiftReader {
    /// `sv verify-liveness`: `AG(request → AF grant)`, alone, as a conjunction, or under
    /// fairness. Universal over the runs of the model.
    ResponseLiveness,
    /// `sv verify-recoverability`: `AG EF target`, an alternating fixpoint.
    Recoverability,
    /// `sv check-fsm`: no illegal encoding of an FSM register is reachable.
    FsmEncoding,
    /// `sv check-fsm --liveness`: encodings never reached, encodings never left, the return to
    /// the reset value.
    FsmLiveness,
    /// `sv lint`: structural checks on the lifted registers.
    Lint,
    /// `sv cegar`: a μ-calculus formula of the caller's on the predicate-cube abstraction.
    Cegar,
    /// `sv mutate --list`: the mutation targets of the lifted model.
    MutationTargets,
    /// `sv mutate`: two `sv verify-auto` runs on the lifted model, compared.
    Mutation,
    /// `sv plan`: the routing `sv verify-auto` would follow on the lifted model.
    Plan,
}

impl LiftReader {
    /// Every reader, for the tests that enumerate them.
    #[cfg(test)]
    pub(crate) const ALL: [LiftReader; 9] = [
        LiftReader::ResponseLiveness,
        LiftReader::Recoverability,
        LiftReader::FsmEncoding,
        LiftReader::FsmLiveness,
        LiftReader::Lint,
        LiftReader::Cegar,
        LiftReader::MutationTargets,
        LiftReader::Mutation,
        LiftReader::Plan,
    ];

    /// What this reader's result says about the DESIGN when the model has free nets the design
    /// drives (a cut module's outputs, a cut point) and the clocks of the model are the design's
    /// (the lift has no clock note; with one, [`Self::on_a_free_net_under_a_clock_note`]).
    ///
    /// SOUNDNESS: a free net takes any value on every step, so the model has every run of the
    /// design and more: an over-approximation. A claim about every run of the model (a HOLDS of
    /// a universal property, an encoding no run reaches) is a claim about every run of the
    /// design. A claim that some run exists may rest on a value the logic left out never
    /// produces. `AG EF` needs both, so neither of its verdicts transfers on its own. Exhaustive
    /// over the readers: a new one is a compile error here.
    pub(crate) fn on_a_free_net(self) -> &'static str {
        match self {
            LiftReader::ResponseLiveness => {
                "A response property speaks of every run, so a HOLDS here holds on the design. A \
                 VIOLATED may follow a run that only a free value allows: read its trace against \
                 the nets listed before taking it as a defect of the design."
            }
            LiftReader::Recoverability => {
                "`AG EF target` asks that from every reachable state some run gets back to the \
                 target, so neither verdict transfers to the design on its own: a HOLDS may get \
                 back through a value the logic left out never produces, and a VIOLATED may start \
                 from a state only such a value reaches."
            }
            LiftReader::FsmEncoding => {
                "The scan reads each register's legal encodings off the logic that is in the \
                 model and checks that no other value is reachable. A register it reports as \
                 staying within its encodings does so on the design too. A reachable illegal \
                 encoding may be reached only through a free value, and a register whose own \
                 logic was left out may be scanned against fewer encodings or not at all."
            }
            LiftReader::FsmLiveness => {
                "An encoding listed as never reached is never reached on the design either. \
                 Every other entry may rest on a free value: an encoding the register cannot \
                 leave, the return to its reset value, and the absence of either. A register \
                 whose own logic was left out may be scanned against fewer encodings or not at \
                 all."
            }
            LiftReader::Lint => {
                "The lint reads the registers of the lifted model. A finding on a signal that \
                 reads a free net describes the model with that net free, and a register that is \
                 not in the model is not checked."
            }
            LiftReader::Cegar => {
                "A verdict that speaks of every run transfers to the design: a HOLDS of a formula \
                 whose modalities are all universal (`[…]`, with negation only on the atoms), a \
                 VIOLATED of one whose modalities are all existential (`<…>`). The other two \
                 verdicts need some run to exist and may rest on a free value, and with both \
                 kinds of modality nested (`AG EF`) neither verdict transfers on its own."
            }
            LiftReader::MutationTargets => {
                "The targets listed are those of the lifted model: a register that is not in it \
                 is not among them."
            }
            LiftReader::Mutation => {
                "The two verdicts compared for each property are `sv verify-auto`'s on this \
                 model, and that verb's note of the same kind says what a free net means for \
                 them. A property counts as flipped when its two verdicts differ, and one the \
                 mutant run no longer has reads there as `skipped`."
            }
            LiftReader::Plan => {
                "The plan routes the properties on this model. `sv verify-auto` on the same \
                 request reports this note with what a free net means for each verdict."
            }
        }
    }

    /// What this reader's result says about the DESIGN when the model has free nets AND the lift
    /// has a clock note: merged clock domains, or a clock with no driver the model can follow.
    ///
    /// SOUNDNESS: such a lift puts every clock on the model's one clock, which ticks on every
    /// step. Of the design's runs the model keeps those in which the clocks tick together, an
    /// UNDER-approximation of the interleavings (and for a divided clock or one with no driver
    /// not even that: the clock's own note says so). The free nets add runs the design lacks.
    /// The model is then neither an over- nor an under-approximation: a claim about every run of
    /// the model says nothing of the runs it lacks, and a claim that some run exists may rest on
    /// a free value. So the four readers whose rule in [`Self::on_a_free_net`] lets a result
    /// transfer lose it here. The other five make no transfer claim there and keep their text.
    /// Exhaustive over the readers: a new one is a compile error here.
    pub(crate) fn on_a_free_net_under_a_clock_note(self) -> &'static str {
        match self {
            LiftReader::ResponseLiveness => {
                "Neither verdict transfers to the design on its own: a HOLDS says nothing of the \
                 runs the model lacks, and a VIOLATED may follow a run that only a free value \
                 allows."
            }
            LiftReader::FsmEncoding => {
                "No answer of the scan transfers to the design on its own: a register reported \
                 as staying within its encodings may leave them on a run the model lacks, and a \
                 reachable illegal encoding may be reached only through a free value. A register \
                 whose own logic was left out may be scanned against fewer encodings or not at \
                 all."
            }
            LiftReader::FsmLiveness => {
                "No entry transfers to the design on its own: an encoding listed as never \
                 reached may be reached on a run the model lacks, and every other entry may rest \
                 on a free value. A register whose own logic was left out may be scanned against \
                 fewer encodings or not at all."
            }
            LiftReader::Cegar => {
                "No verdict of any formula transfers to the design on its own: one that speaks \
                 of every run says nothing of the runs the model lacks, and one that needs some \
                 run to exist may rest on a free value."
            }
            LiftReader::Recoverability
            | LiftReader::Lint
            | LiftReader::MutationTargets
            | LiftReader::Mutation
            | LiftReader::Plan => self.on_a_free_net(),
        }
    }
}

/// What one lift decided, read off its result and the yosys options it ran with.
#[derive(Debug, Clone, Default)]
pub(crate) struct LiftDecisions<'a> {
    /// The clock notes of the same lift (`clock-domains-merged`, one note per clock with no
    /// driver the model can follow). They lead the report's list, and the cut notes read them.
    pub clock_notes: Vec<VerificationNote>,
    /// Modules instantiated with no body: their outputs are free.
    pub cut_modules: &'a [String],
    /// Nets the request asked the lift to cut to a free value (`--cutpoint`, `lift.cutpoints`).
    /// The lift does not say which names matched a net, so the notes speak of the request.
    pub cut_points: &'a [String],
    /// Parameters the request set before elaboration, `(NAME | MODULE.NAME, VALUE)`.
    pub params: &'a [(String, String)],
    /// Why the yosys script ran a second time, when it did.
    pub retry: Option<&'a str>,
    /// Known flop primitives with no definition in the sources, lifted with a behavioural stub.
    /// Only `sv verify-auto`'s lift supplies one ([`Self::of_report`]).
    pub stubs: &'a [String],
}

impl<'a> LiftDecisions<'a> {
    /// The decisions of a lift made here: `yopts` is what the lift ran with (after the lift
    /// overrides and any adjustment), so a cut or a parameter the verb added itself is reported.
    pub(crate) fn of_lift(lifted: &'a SvLiftResult, yopts: &'a YosysOptions) -> Self {
        LiftDecisions {
            clock_notes: lift_clock_notes(&lifted.clock_domains),
            cut_modules: &lifted.blackboxed_modules,
            cut_points: &yopts.cutpoint_signals,
            params: &yopts.params,
            retry: lifted.backend_retry.as_deref(),
            // This lift supplies no stub: a primitive with no definition is a lift error here.
            stubs: &[],
        }
    }

    /// The decisions of the lift behind a `sv verify-auto` report or its skeleton (`sv plan`,
    /// `sv mutate`): the diagnostics hold what the lift returned, `yopts` what was asked.
    pub(crate) fn of_report(report: &'a AutoVerifyReport, yopts: &'a YosysOptions) -> Self {
        LiftDecisions {
            clock_notes: clock_notes_of_report(report),
            cut_modules: &report.diagnostics.blackboxed_modules,
            cut_points: &yopts.cutpoint_signals,
            params: &yopts.params,
            retry: report.diagnostics.lift_retry.as_deref(),
            stubs: &report.diagnostics.auto_provided_stubs,
        }
    }
}

/// The `lift-retry` note (Info): the lift re-ran its yosys script with `opt_clean` after the
/// BTOR backend reported a topological loop (2026-10-22, OpenTitan csrng through yosys-slang's
/// `$buf` cells). The model is the same design (the retry keeps every named net first,
/// mununu#1229), and a reader comparing two runs wants to know the script moved. One wording for
/// every verb, `sv verify-auto` included.
pub(crate) fn lift_retry_note(reason: &str) -> VerificationNote {
    VerificationNote {
        kind: LIFT_RETRY.into(),
        level: NoteLevel::Info,
        summary: format!("lift: retried — {reason}."),
        detail: "The yosys BTOR backend walks the netlist cell by cell and can report a \
                 topological loop where `check` finds none (wide `$buf` cells from yosys-slang \
                 chained bit-wise through other cells). The retry marks every net that has a \
                 name in the design as kept and runs `opt_clean`, which folds the buffers into \
                 connections and drops only cells that reach no named net and no output port. \
                 The backend writes none of those, so the model has the same registers and is \
                 of the same design. The retry runs only on that error."
            .to_string(),
        items: Vec::new(),
        property: None,
    }
}

/// The `parameter-override` note (ScopeCaveat): the design was elaborated with these module
/// parameters, so everything the report says is about the design they size. One wording for
/// every verb, `sv verify-auto` included.
pub(crate) fn parameter_override_note(params: &[(String, String)]) -> VerificationNote {
    VerificationNote {
        kind: PARAMETER_OVERRIDE.into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "{} module parameter(s) overridden before the lift (yosys chparam / slang -G) — \
             every result here is for THESE parameter values.",
            params.len()
        ),
        detail: "A module parameter was set before elaboration (e.g. to shrink a timing interval \
                 so its counters fit the exact engine), so the design lifted here is the one \
                 sized by these parameters. A different parameter value would size a different \
                 design; re-run to cover other values."
            .into(),
        items: params.iter().map(|(lhs, v)| format!("{lhs}={v}")).collect(),
        property: None,
    }
}

/// The `flop-stub` note (Info): a flop primitive the sources do not define was lifted with a
/// behavioural model of the same primitive, so the register it holds is in the model. One wording
/// for every verb, `sv verify-auto` included.
pub(crate) fn flop_stub_note(stubs: &[String]) -> VerificationNote {
    VerificationNote {
        kind: FLOP_STUB.into(),
        level: NoteLevel::Info,
        summary: format!("{} flop primitive(s) behaviorally stubbed.", stubs.len()),
        detail: "A cut flop primitive (no body in the source set) was auto-replaced with an \
                 EXACT behavioral model of its datapath, so the register it drives survives \
                 the lift (no soundness loss — the stub is functionally identical)."
            .into(),
        items: stubs.to_vec(),
        property: None,
    }
}

/// Whether `kind` is one of the clock notes of a lift: the merged-domains caveat, or a clock with
/// no driver the model can follow.
pub(crate) fn is_clock_note(kind: &str) -> bool {
    kind == CLOCK_DOMAINS_MERGED || CLOCK_ROOT_NOTE_KINDS.contains(&kind)
}

/// The clock notes among `notes`, as a cut note names them: a clock with no driver by its net
/// and its kind, the merged-domains caveat by its kind. Each once, in the order given. Empty when
/// the lift has no clock note. The one place the two cut-note builders (this module's and
/// `sv verify-auto`'s) get the names from.
pub(crate) fn clock_note_names(notes: &[VerificationNote]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for note in notes.iter().filter(|n| is_clock_note(&n.kind)) {
        let net = note
            .items
            .first()
            .filter(|net| note.kind != CLOCK_DOMAINS_MERGED && !net.is_empty());
        let name = match net {
            Some(net) => format!("`{net}` (note `{}`)", note.kind),
            // The merged caveat lists every domain, and a root note with no net has no name to
            // give: the kind is what the reader looks up.
            None => format!("note `{}`", note.kind),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// What a cut note says after "the net is free": what the model is with respect to the design,
/// what `reader` may conclude, and what undoing the cut (`undo`) gives.
///
/// SOUNDNESS: free nets alone leave an over-approximation ([`LiftReader::on_a_free_net`]). With a
/// clock note the model also lacks runs of the design, so the over-approximation claim is not
/// made and the reader's rule is the one of
/// [`LiftReader::on_a_free_net_under_a_clock_note`]. Any clock note withdraws the claim, a kind
/// added later included: the test is "the lift has a clock note", not a list of kinds that limit.
fn cut_conclusion(reader: LiftReader, clock_notes: &[VerificationNote], undo: &str) -> String {
    let clocks = clock_note_names(clock_notes);
    if clocks.is_empty() {
        return format!(
            "{EVERY_RUN_AND_MORE} {} {undo} for a result on the design itself.",
            reader.on_a_free_net()
        );
    }
    format!(
        "On its own that would leave the model with every run of the design and more. But the \
         clocks of this model are not the design's ({}): it ticks every clock on every step, so \
         it lacks the runs of the design in which they do not tick together. The model has runs \
         the design lacks and lacks runs the design has. {} {undo} to take this note out of the \
         report; the clock notes say what the clocks alone allow.",
        clocks.join(", "),
        reader.on_a_free_net_under_a_clock_note()
    )
}

/// Every note the report of `reader` owes about the lift: the clock notes first, then one note
/// per decision the lift made, in `sv verify-auto`'s order. Empty when the lift made none.
pub(crate) fn lift_decision_notes(
    reader: LiftReader,
    decisions: LiftDecisions<'_>,
) -> Vec<VerificationNote> {
    let cut_points = cut_conclusion(
        reader,
        &decisions.clock_notes,
        "Lift without the cut points",
    );
    let cut_modules = cut_conclusion(
        reader,
        &decisions.clock_notes,
        "Give the lift the modules' sources",
    );
    let clocked = decisions.clock_notes.iter().any(|n| is_clock_note(&n.kind));
    let mut notes = decisions.clock_notes;
    notes.extend(decisions.retry.map(lift_retry_note));
    if !decisions.params.is_empty() {
        notes.push(parameter_override_note(decisions.params));
    }
    if !decisions.cut_points.is_empty() {
        notes.push(VerificationNote {
            kind: CONTROL_SLICE.into(),
            level: NoteLevel::ScopeCaveat,
            summary: if clocked {
                format!(
                    "{} cut point(s) requested — a net cut there is a free value in the model, \
                     and the clocks of the model are not the design's (the clock notes): not an \
                     over-approximation of the design.",
                    decisions.cut_points.len()
                )
            } else {
                format!(
                    "{} cut point(s) requested — a net cut there is a free value in the model, \
                     an over-approximation of the design.",
                    decisions.cut_points.len()
                )
            },
            detail: format!(
                "The lift was asked to cut the listed nets. A net it cut takes any value on \
                 every step of the model, and the logic that drove it is left out. {cut_points}"
            ),
            items: decisions.cut_points.to_vec(),
            property: None,
        });
    }
    if !decisions.stubs.is_empty() {
        notes.push(flop_stub_note(decisions.stubs));
    }
    if !decisions.cut_modules.is_empty() {
        notes.push(VerificationNote {
            kind: BLACKBOX_CUT.into(),
            level: NoteLevel::SoundnessCaveat,
            summary: format!(
                "{} module(s) cut (no body) — their outputs are free values and the state \
                 inside them is not modelled.",
                decisions.cut_modules.len()
            ),
            detail: format!(
                "The sources hold no body for the listed modules, so the lift left every output \
                 of their instances free: it takes any value on every step of the model, and a \
                 register inside such a module is not in the model. {cut_modules}"
            ),
            items: decisions.cut_modules.to_vec(),
            property: None,
        });
    }
    notes
}

/// The lines `sv cdc-lint` adds to its `notes` for the same decisions. The lint reads the
/// flattened netlist, where a free net has no storage cell behind it: the walk from a storage
/// cell's data input ends there, so a crossing that runs through one is not seen. Its clock notes
/// are its own (`cdc_lint::clock_root_notes`), so `clock_notes` is not read here, and its lift
/// supplies no flop stub.
pub(crate) fn cdc_lint_lift_lines(decisions: &LiftDecisions<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(reason) = decisions.retry {
        lines.push(format!(
            "lift: retried — {reason}. The netlist the lint walks is written before the retried \
             step, by the same commands in both runs; the retry changes only the model of the \
             gray-step check"
        ));
    }
    if !decisions.params.is_empty() {
        let params: Vec<String> = decisions
            .params
            .iter()
            .map(|(lhs, v)| format!("{lhs}={v}"))
            .collect();
        lines.push(format!(
            "parameters overridden before the lift ({}): the crossings listed are those of the \
             design these values size",
            params.join(", ")
        ));
    }
    if !decisions.cut_points.is_empty() {
        lines.push(format!(
            "cut points requested ({}): a net the lift cut is unclocked, so crossings through \
             it are not seen",
            decisions.cut_points.join(", ")
        ));
    }
    if !decisions.cut_modules.is_empty() {
        lines.push(format!(
            "black-boxed modules ({}): their outputs are unclocked, so crossings inside or through \
             them are not seen",
            decisions.cut_modules.join(", ")
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|s| (*s).to_string()).collect()
    }

    /// One lift decision, as the only one its lift made.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Decision {
        Retry,
        Parameter,
        CutPoint,
        FlopStub,
        CutModule,
    }

    const DECISIONS: [Decision; 5] = [
        Decision::Retry,
        Decision::Parameter,
        Decision::CutPoint,
        Decision::FlopStub,
        Decision::CutModule,
    ];

    impl Decision {
        fn kind(self) -> &'static str {
            match self {
                Decision::Retry => LIFT_RETRY,
                Decision::Parameter => PARAMETER_OVERRIDE,
                Decision::CutPoint => CONTROL_SLICE,
                Decision::FlopStub => FLOP_STUB,
                Decision::CutModule => BLACKBOX_CUT,
            }
        }

        /// A module nobody asked to cut is a soundness caveat; what the request asked for
        /// narrows the scope; the retry and the stub change nothing the design reads.
        fn level(self) -> NoteLevel {
            match self {
                Decision::Retry | Decision::FlopStub => NoteLevel::Info,
                Decision::Parameter | Decision::CutPoint => NoteLevel::ScopeCaveat,
                Decision::CutModule => NoteLevel::SoundnessCaveat,
            }
        }

        /// The operands the note lists (the retry has none: its reason is the summary).
        fn items(self) -> Vec<String> {
            match self {
                Decision::Retry => Vec::new(),
                Decision::Parameter => names(&["top.DEPTH=4"]),
                Decision::CutPoint => names(&["u_dp.sum"]),
                Decision::FlopStub => names(&["prim_flop"]),
                Decision::CutModule => names(&["vendor_pll"]),
            }
        }

        /// Whether `sv cdc-lint`'s lift can make this decision: it supplies no flop stub.
        fn in_the_lint_lift(self) -> bool {
            self != Decision::FlopStub
        }
    }

    struct Fixture {
        modules: Vec<String>,
        points: Vec<String>,
        params: Vec<(String, String)>,
        retry: Option<String>,
        stubs: Vec<String>,
    }

    impl Fixture {
        fn none() -> Self {
            Fixture {
                modules: Vec::new(),
                points: Vec::new(),
                params: Vec::new(),
                retry: None,
                stubs: Vec::new(),
            }
        }

        fn only(decision: Decision) -> Self {
            let mut f = Fixture::none();
            match decision {
                Decision::Retry => f.retry = Some("the backend reported a loop".into()),
                Decision::Parameter => f.params = vec![("top.DEPTH".into(), "4".into())],
                Decision::CutPoint => f.points = names(&["u_dp.sum"]),
                Decision::FlopStub => f.stubs = names(&["prim_flop"]),
                Decision::CutModule => f.modules = names(&["vendor_pll"]),
            }
            f
        }

        fn all() -> Self {
            let mut f = Fixture::none();
            for d in DECISIONS {
                let one = Fixture::only(d);
                f.modules.extend(one.modules);
                f.points.extend(one.points);
                f.params.extend(one.params);
                f.stubs.extend(one.stubs);
                f.retry = f.retry.or(one.retry);
            }
            f
        }

        fn decisions(&self, clock_notes: Vec<VerificationNote>) -> LiftDecisions<'_> {
            LiftDecisions {
                clock_notes,
                cut_modules: &self.modules,
                cut_points: &self.points,
                params: &self.params,
                retry: self.retry.as_deref(),
                stubs: &self.stubs,
            }
        }
    }

    /// The class's enumerating test (mununu#1189): every reader × every lift decision. A lift
    /// that made one decision yields exactly one note, of that decision's kind and level, with
    /// its operands in `items`; a lift that made none yields none. The rows of `sv verify-auto`
    /// (`x1189_verify_auto_reports_the_lift_decisions_under_the_shared_kinds`) and of
    /// `sv cdc-lint` (below) complete the verb dimension.
    #[test]
    fn x1189_every_reader_reports_every_lift_decision() {
        for reader in LiftReader::ALL {
            assert!(
                lift_decision_notes(reader, Fixture::none().decisions(Vec::new())).is_empty(),
                "{reader:?}: a lift that decided nothing has nothing to say"
            );
            for decision in DECISIONS {
                let fixture = Fixture::only(decision);
                let notes = lift_decision_notes(reader, fixture.decisions(Vec::new()));
                assert_eq!(notes.len(), 1, "{reader:?} × {decision:?}: {notes:#?}");
                let note = &notes[0];
                assert_eq!(note.kind, decision.kind(), "{reader:?} × {decision:?}");
                assert_eq!(note.level, decision.level(), "{reader:?} × {decision:?}");
                assert_eq!(note.items, decision.items(), "{reader:?} × {decision:?}");
                assert_eq!(
                    note.property, None,
                    "a note about the lift is about no property"
                );
                assert!(
                    !note.summary.is_empty() && !note.detail.is_empty(),
                    "{reader:?} × {decision:?}: {note:#?}"
                );
            }
            let every = Fixture::all();
            let kinds: Vec<String> = lift_decision_notes(reader, every.decisions(Vec::new()))
                .into_iter()
                .map(|n| n.kind)
                .collect();
            assert_eq!(
                kinds, LIFT_DECISION_KINDS,
                "{reader:?}: one note per decision"
            );
        }
    }

    /// The cut notes say what the reader may conclude, and that differs by reader: the two
    /// property verbs (a HOLDS of a response property transfers, no verdict of `AG EF` does)
    /// must not share a detail, and no two readers do. The parameter, retry and stub notes are
    /// about the lift alone and are the same for all.
    #[test]
    fn x1189_a_free_net_is_worded_for_what_the_reader_computes() {
        for decision in [Decision::CutPoint, Decision::CutModule] {
            let fixture = Fixture::only(decision);
            let details: std::collections::BTreeSet<String> = LiftReader::ALL
                .into_iter()
                .map(|reader| {
                    let mut notes = lift_decision_notes(reader, fixture.decisions(Vec::new()));
                    let note = notes.pop().expect("one note");
                    assert!(
                        note.detail.contains(reader.on_a_free_net()),
                        "{reader:?} × {decision:?}: the note carries the reader's own rule"
                    );
                    note.detail
                })
                .collect();
            assert_eq!(
                details.len(),
                LiftReader::ALL.len(),
                "{decision:?}: every reader has its own wording"
            );
        }
        for decision in [Decision::Parameter, Decision::Retry, Decision::FlopStub] {
            let fixture = Fixture::only(decision);
            let distinct: std::collections::BTreeSet<String> = LiftReader::ALL
                .into_iter()
                .flat_map(|reader| lift_decision_notes(reader, fixture.decisions(Vec::new())))
                .map(|n| format!("{}\n{}", n.summary, n.detail))
                .collect();
            assert_eq!(
                distinct.len(),
                1,
                "{decision:?}: one wording for every reader"
            );
        }
    }

    /// The readers whose rule lets a result transfer from a model with free nets. Under a clock
    /// note each of them has another rule, and the other five keep theirs.
    const TRANSFERRING: [LiftReader; 4] = [
        LiftReader::ResponseLiveness,
        LiftReader::FsmEncoding,
        LiftReader::FsmLiveness,
        LiftReader::Cegar,
    ];

    /// The clock notes of the lift lead the list unchanged, and under any of them a cut note
    /// withdraws the over-approximation claim (mununu#1189 review): merged domains take runs
    /// away from the model, and so does a clock the model cannot follow (mununu#1180). The rows
    /// are the clock notes a lift can return: the merge alone, the merge with each kind of
    /// driverless clock (what a real lift returns for one), and each kind alone. The cut note
    /// names every clock note it rests on, and the reader's rule is the one for that model.
    #[test]
    fn x1189_a_clock_note_withdraws_the_cut_notes_claim() {
        let clock = |kind: &str, net: &str| VerificationNote {
            kind: kind.into(),
            level: NoteLevel::SoundnessCaveat,
            summary: "a clock note".into(),
            detail: String::new(),
            items: vec![net.to_string()],
            property: None,
        };
        let merged = || clock(CLOCK_DOMAINS_MERGED, "clk_a (posedge, 2 cells)");
        let mut rows: Vec<Vec<VerificationNote>> = vec![vec![merged()]];
        for root_kind in CLOCK_ROOT_NOTE_KINDS {
            rows.push(vec![merged(), clock(root_kind, "gated_clk")]);
            rows.push(vec![clock(root_kind, "gated_clk")]);
        }
        for clock_notes in rows {
            let kinds: Vec<&str> = clock_notes.iter().map(|n| n.kind.as_str()).collect();
            for reader in LiftReader::ALL {
                let fixture = Fixture::all();
                let notes = lift_decision_notes(reader, fixture.decisions(clock_notes.clone()));
                assert_eq!(
                    &notes[..clock_notes.len()],
                    &clock_notes[..],
                    "{reader:?} × {kinds:?}: clock notes first"
                );
                for cut in [CONTROL_SLICE, BLACKBOX_CUT] {
                    let note = notes.iter().find(|n| n.kind == cut).expect("the cut note");
                    let at = format!("{reader:?} × {cut} × {kinds:?}: {}", note.detail);
                    assert!(!note.detail.contains(EVERY_RUN_AND_MORE), "{at}");
                    assert!(
                        note.detail
                            .contains(reader.on_a_free_net_under_a_clock_note()),
                        "{at}"
                    );
                    if TRANSFERRING.contains(&reader) {
                        assert!(!note.detail.contains(reader.on_a_free_net()), "{at}");
                    }
                    for kind in &kinds {
                        assert!(note.detail.contains(&format!("note `{kind}`")), "{at}");
                    }
                    assert_eq!(
                        note.detail.contains("`gated_clk`"),
                        kinds.iter().any(|k| CLOCK_ROOT_NOTE_KINDS.contains(k)),
                        "a clock with no driver is named by its net: {at}"
                    );
                }
            }
        }
        // The control: with no clock note the claim is made, under the reader's first rule.
        for reader in LiftReader::ALL {
            let fixture = Fixture::all();
            for note in lift_decision_notes(reader, fixture.decisions(Vec::new())) {
                if [CONTROL_SLICE, BLACKBOX_CUT].contains(&note.kind.as_str()) {
                    assert!(note.detail.contains(EVERY_RUN_AND_MORE), "{reader:?}");
                    assert!(note.detail.contains(reader.on_a_free_net()), "{reader:?}");
                }
            }
        }
    }

    /// Which readers a clock note changes the rule of: exactly those whose first rule lets a
    /// result transfer. A reader moved from one side to the other is a soundness decision, and
    /// this is where it shows.
    #[test]
    fn x1189_a_clock_note_changes_the_rule_of_the_readers_that_transfer() {
        let changed: Vec<LiftReader> = LiftReader::ALL
            .into_iter()
            .filter(|r| r.on_a_free_net() != r.on_a_free_net_under_a_clock_note())
            .collect();
        assert_eq!(changed, TRANSFERRING);
    }

    /// A clock note is named once, by its net when it has one: the merged caveat lists every
    /// domain and is named by its kind, and a root note with no net gives its kind, not an empty
    /// name. A note that is not a clock note is not named.
    #[test]
    fn x1189_a_cut_note_names_each_clock_note_once() {
        let note = |kind: &str, items: &[&str]| VerificationNote {
            kind: kind.into(),
            level: NoteLevel::SoundnessCaveat,
            summary: String::new(),
            detail: String::new(),
            items: names(items),
            property: None,
        };
        let root = CLOCK_ROOT_NOTE_KINDS[0];
        let notes = [
            note(CLOCK_DOMAINS_MERGED, &["clk_a (posedge, 2 cells)"]),
            note(root, &["gclk", "u_pll", "3"]),
            note(root, &["gclk", "u_pll", "3"]),
            note(root, &[]),
            note(root, &[""]),
            note(PARAMETER_OVERRIDE, &["top.DEPTH=4"]),
        ];
        assert_eq!(
            clock_note_names(&notes),
            vec![
                format!("note `{CLOCK_DOMAINS_MERGED}`"),
                format!("`gclk` (note `{root}`)"),
                format!("note `{root}`"),
            ]
        );
        assert!(clock_note_names(&notes[5..]).is_empty());
    }

    /// Where the decisions are read from: the diagnostics of a `sv verify-auto` report (the
    /// lift's own answers) and the yosys options the lift ran with (what the request asked).
    #[test]
    fn x1189_the_decisions_of_a_report_are_its_diagnostics_and_the_request() {
        let mut report = AutoVerifyReport::default();
        report.diagnostics.blackboxed_modules = names(&["vendor_pll"]);
        report.diagnostics.auto_provided_stubs = names(&["prim_flop"]);
        report.diagnostics.lift_retry = Some("the backend reported a loop".into());
        let yopts = YosysOptions {
            cutpoint_signals: names(&["u_dp.sum"]),
            params: vec![("top.DEPTH".into(), "4".into())],
            ..Default::default()
        };
        let kinds: Vec<String> =
            lift_decision_notes(LiftReader::Plan, LiftDecisions::of_report(&report, &yopts))
                .into_iter()
                .map(|n| n.kind)
                .collect();
        assert_eq!(kinds, LIFT_DECISION_KINDS);
        let nothing = AutoVerifyReport::default();
        let plain = YosysOptions::default();
        assert!(
            lift_decision_notes(LiftReader::Plan, LiftDecisions::of_report(&nothing, &plain))
                .is_empty()
        );
    }

    /// `sv cdc-lint`'s row of the table: one line per decision its lift can make, naming its
    /// operands.
    #[test]
    fn x1189_cdc_lint_has_a_line_for_every_lift_decision() {
        assert!(cdc_lint_lift_lines(&Fixture::none().decisions(Vec::new())).is_empty());
        let in_the_lint_lift = DECISIONS.into_iter().filter(|d| d.in_the_lint_lift());
        for decision in in_the_lint_lift.clone() {
            let fixture = Fixture::only(decision);
            let lines = cdc_lint_lift_lines(&fixture.decisions(Vec::new()));
            assert_eq!(lines.len(), 1, "{decision:?}: {lines:#?}");
            let operands = match decision {
                Decision::Retry => names(&["the backend reported a loop"]),
                other => other.items(),
            };
            for operand in operands {
                assert!(lines[0].contains(&operand), "{decision:?}: {}", lines[0]);
            }
        }
        assert_eq!(
            cdc_lint_lift_lines(&Fixture::all().decisions(Vec::new())).len(),
            in_the_lint_lift.count()
        );
    }
}
