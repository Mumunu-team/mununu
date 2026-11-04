//! R-MM (KMTS multi-module composition) — the netlist-driven driver that
//! composes per-module KMTSes into one product for verification.
//!
//! Each submodule's BTOR2 is lifted independently (one `Clts` per module
//! *type*); the top module's netlist (parsed by
//! [`super::parse_instance_connections`]) says which instance port connects
//! to which net. To make two instances that share a net rendezvous under
//! [`crate::composition::compose`] — whose synchronisation is name-equality
//! on label payloads — each instance's labels are rewritten so the shared
//! net carries a single value-encoded name `<net>_<v>` on *both* sides:
//!
//! - A **reader** input port `P` connected to net `N` has labels
//!   `P_<v>` (the lift value-encodes inputs); they are renamed to `N_<v>`.
//! - A **driver** output port `P` connected to net `N` has *no* label (the
//!   lift drops outputs — see R-MM-4b); its value is surfaced as a per-state
//!   valuation `P = T/F` (R-MM-4b's `surface_output_ports`), from which we
//!   synthesise a `N_<v>` label on every transition leaving that state.
//!   This is the `annotate_driving_output_labels` analog the native path
//!   performs.
//! - Every other label (a free top-level input like `enable`/`pop`) is
//!   instance-namespaced (`<instance>__<label>`) so two instances of the
//!   *same* module type don't accidentally rendezvous on a same-named local
//!   input. The clock-step fallback `step` is kept verbatim — modules with
//!   no free inputs all step together on the clock.
//!
//! Modality (may/must), structured valuations, and 3-valued predicates are
//! preserved (the composed property reads them).

use super::YosysOptions;
use crate::adapter::{AdapterError, AdapterErrorKind, AdapterOptions};
use crate::clts::{Clts, CltsResult, DefaultLabelIdx, DefaultStateIdx, LabelId, StateId};
use crate::composition::{CompositionOptions, CompositionSemantics, compose};
use crate::controllability::BoundaryDirection;
use smallvec::SmallVec;
use std::collections::HashMap;

/// The clock-step fallback label the bit-blaster emits for a module with no
/// free (non-clock, non-reset) inputs. It is shared across such modules so
/// they advance together on the clock — never instance-namespaced.
const STEP_LABEL: &str = "step";

/// R-MM-4c — Rewrite one instance's KMTS into a composition-ready form:
/// reader inputs and driver outputs are recast to shared `<net>_<v>` labels;
/// free local inputs are instance-namespaced. See the module docs.
///
/// * `reader_ports` — input-port → net for ports this instance *reads*.
/// * `driver_ports` — output-port → net for ports this instance *drives*.
///   The output value per source state comes from that state's valuation
///   (R-MM-4b surfaced it as `<port> = T/F`); states without the valuation
///   contribute no driver label (best-effort — the e2e path asserts it is
///   present).
/// * `instance` — the instance name, used to namespace free local labels.
pub fn prepare_instance_for_composition(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    reader_ports: &HashMap<String, String>,
    driver_ports: &HashMap<String, String>,
    instance: &str,
) -> CltsResult<Clts<DefaultStateIdx, DefaultLabelIdx>> {
    let mut builder = Clts::builder();

    // 1. Copy states (names, initial flags, variables).
    let mut state_mapping: HashMap<StateId<DefaultStateIdx>, StateId<DefaultStateIdx>> =
        HashMap::new();
    for state in clts.states() {
        let name = clts.state_name(state).unwrap_or("state").to_owned();
        if let Some(new_id) = builder.state_with_name(name) {
            if clts.initial_states().contains(&state) {
                builder.initial_state_id(new_id);
            }
            let vars = clts.state_variables(state);
            if !vars.is_empty() {
                builder.with_variables_for_state(new_id, vars.iter().map(|s| s.as_str()));
            }
            state_mapping.insert(state, new_id);
        }
    }

    // 2. Copy transitions, rewriting labels + synthesising driver labels.
    for state in clts.states() {
        let &source_new = match state_mapping.get(&state) {
            Some(id) => id,
            None => continue,
        };
        // Driver labels depend only on the SOURCE state's output values
        // (Moore) or its split-variant value (Mealy, already definite per
        // split-state) — compute once per source state.
        let driver_symbols = driver_labels_for_state(clts, state, driver_ports);

        for transition in clts.outgoing(state) {
            let &target_new = match state_mapping.get(&transition.target()) {
                Some(id) => id,
                None => continue,
            };
            let mut new_labels: SmallVec<[LabelId<DefaultLabelIdx>; 4]> = SmallVec::new();
            // 2a. Rewrite the transition's own labels (reader inputs → net;
            //     step kept; free inputs namespaced).
            for &label_id in transition.labels() {
                let payload = match clts.label_payload(label_id) {
                    Some(p) => p,
                    None => continue,
                };
                let renamed: Vec<String> = payload
                    .iter()
                    .map(|sym| rewrite_label_symbol(sym, reader_ports, instance))
                    .collect();
                let new_label_id = builder
                    .labels()
                    .intern(renamed.iter().map(|s| s.as_str()))?;
                if let Some(ctrl) = clts.label_controllability(label_id) {
                    builder.set_label_controllability(new_label_id, ctrl);
                }
                new_labels.push(new_label_id);
            }
            // 2b. Add the synthesised driver-output labels for this step.
            for sym in &driver_symbols {
                let id = builder.labels().intern([sym.as_str()])?;
                new_labels.push(id);
            }
            builder.transition_ids_with_modality(
                source_new,
                &new_labels,
                target_new,
                transition.modality().clone(),
            );
        }
    }

    // 3. Copy structured valuations + 3-valued predicates, instance-qualifying
    //    each KEY with `<instance>__` so the composed product references each
    //    module's signals unambiguously — two instances commonly share a
    //    register name (`state`), and the compose merge (R-MM-2) would
    //    otherwise collide them. A composed property names a signal as
    //    `<instance>__<signal>`. Each VALUE is normalised to its integer
    //    encoding (`normalize_signal_value`: the 1-bit surfaced outputs'
    //    `T`/`F` → `1`/`0`) so the composed product is all-numeric and the
    //    verify pipeline can bind `<instance>__<signal> == k` atoms (R-MM-5b).
    //
    //    R-MM-7 (port-equality realisation): for each output port this
    //    instance DRIVES onto a shared net, ALSO surface the value under the
    //    bare NET name (`valid`, not just `u_producer__valid`). The driver's
    //    surfaced output is a function of its state (Moore), so the net value
    //    is well-defined per composed state; the synchronous rendezvous on
    //    `<net>_<v>` labels already forces the reader side to agree, so the
    //    bare net valuation IS the connected ports' single agreed value — the
    //    §7.2 port-equality predicate, made first-class. A composed property
    //    can then reference the shared net directly (`valid == 1`) without
    //    knowing which instance drives it. Precision-only: it adds a queryable
    //    predicate, never changes the transition structure or a verdict.
    for state in clts.states() {
        if let (Some(&new_id), Some(valuation)) =
            (state_mapping.get(&state), clts.state_valuation(state))
        {
            let mut qualified: std::collections::BTreeMap<String, String> = valuation
                .iter()
                .map(|(k, v)| (format!("{instance}__{k}"), normalize_signal_value(v)))
                .collect();
            for (port, net) in driver_ports {
                if let Some(v) = valuation.get(port) {
                    qualified.insert(net.clone(), normalize_signal_value(v));
                }
            }
            builder.with_valuation_for_state(new_id, qualified);
        }
    }
    for (state, predicate, verdict) in clts
        .states()
        .flat_map(|s| {
            clts.state_3valued_predicate_entries(s)
                .into_iter()
                .map(move |(p, v)| (s, p.to_string(), v))
        })
        .collect::<Vec<_>>()
    {
        if let Some(&new_id) = state_mapping.get(&state) {
            builder.with_3valued_predicate(new_id, format!("{instance}__{predicate}"), verdict);
        }
    }

    builder.build()
}

/// Rewrite one label symbol for [`prepare_instance_for_composition`]:
/// a reader port's `<port>_<v>` → `<net>_<v>`; the `step` clock label is
/// kept; anything else is instance-namespaced.
fn rewrite_label_symbol(
    symbol: &str,
    reader_ports: &HashMap<String, String>,
    instance: &str,
) -> String {
    if symbol == STEP_LABEL {
        return STEP_LABEL.to_string();
    }
    if let Some(net_symbol) = relabel_reader_symbol(symbol, reader_ports) {
        return net_symbol;
    }
    format!("{instance}__{symbol}")
}

/// If `symbol` is `<port>_<suffix>` for some reader `port` (longest match
/// wins), return `<net>_<suffix>`; otherwise `None`.
fn relabel_reader_symbol(symbol: &str, reader_ports: &HashMap<String, String>) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for (port, net) in reader_ports {
        if let Some(suffix) = symbol
            .strip_prefix(port.as_str())
            .and_then(|rest| rest.strip_prefix('_'))
            && best.as_ref().is_none_or(|(len, _)| port.len() > *len)
        {
            best = Some((port.len(), format!("{net}_{suffix}")));
        }
    }
    best.map(|(_, s)| s)
}

/// The synthesised `<net>_<v>` driver labels for a source state: for each
/// output port this instance drives, read the port's value from the state
/// valuation (R-MM-4b's `T`/`F`, normalised to `1`/`0`) and form the shared
/// net label. States missing the valuation contribute nothing.
fn driver_labels_for_state(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    state: StateId<DefaultStateIdx>,
    driver_ports: &HashMap<String, String>,
) -> Vec<String> {
    if driver_ports.is_empty() {
        return Vec::new();
    }
    let Some(valuation) = clts.state_valuation(state) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (port, net) in driver_ports {
        if let Some(raw) = valuation.get(port) {
            out.push(format!("{net}_{}", normalize_signal_value(raw)));
        }
    }
    out
}

/// Normalise a 1-bit signal value to the integer encoding the rest of the
/// pipeline uses: the bit-blaster surfaces 1-bit combinational outputs as
/// `T`/`F` (R-MM-4b), but readers' value-encoded labels — and register
/// valuations — are decimal (`<sig>_<integer>`, `state = 2`). Map `T`→`1`,
/// `F`→`0`; a decimal value (multi-bit register, already numeric) passes
/// through unchanged.
///
/// Used both to synthesise driver rendezvous labels and to keep the
/// composed product's **valuations all-numeric** — the latter is what lets
/// the verify pipeline's `environment_for` wire the `abstract_states`
/// channel (gated on every valuation parsing as `i64`), so a composed
/// property atom like `u_consumer__state == 2` binds to the actual value
/// instead of falling through to the empty-bitset under-approximation. A
/// single stray `T`/`F` would disqualify the whole CLTS from that channel.
fn normalize_signal_value(raw: &str) -> String {
    match raw {
        "T" => "1".to_string(),
        "F" => "0".to_string(),
        other => other.to_string(),
    }
}

/// R-MM-4d — the composed KMTS for a multi-module SV design.
pub struct MultiModuleComposition {
    /// The composed product KMTS, ready for property evaluation.
    pub composed: Clts<DefaultStateIdx, DefaultLabelIdx>,
    /// Instance names folded into the product, in fold order.
    pub instances: Vec<String>,
}

/// R-MM-4d — Compose a multi-module SystemVerilog design into one KMTS.
///
/// Pipeline: yosys per-module BTOR2 with net-driving outputs surfaced +
/// top-netlist connectivity + port directions
/// ([`super::translate_sv_per_module_with_connectivity`]) → realise each
/// module *type*'s CTXDSL into a `Clts` (inline valuations carry the
/// surfaced output values) → rewrite each *instance* for composition
/// ([`prepare_instance_for_composition`], classifying each connected port as
/// a reader input or driver output by direction) → synchronously
/// fold-compose the instances ([`crate::composition::compose`]). The result
/// is one product KMTS where shared nets rendezvous on `<net>_<v>`.
///
/// Instance types not lifted (black-box / unsupported) are skipped — the
/// composition simply omits them (sound: fewer constraints, more behaviour).
pub fn compose_sv_multi_module(
    content: &str,
    options: &AdapterOptions,
    yopts: &YosysOptions,
) -> Result<MultiModuleComposition, AdapterError> {
    let (outputs, connectivity, directions) =
        super::translate_sv_per_module_with_connectivity(content, options, yopts)?;
    if connectivity.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: "adapter/yosys multi-module: no instance connectivity in the top netlist \
                      (single-module design? use translate_sv)"
                .into(),
            location: None,
        });
    }

    // Realise each module type's CTXDSL into a Clts once (cache by type).
    let mut clts_by_type: HashMap<String, Clts<DefaultStateIdx, DefaultLabelIdx>> = HashMap::new();
    for out in &outputs {
        let clts = realize_module_clts(&out.output.ctxdsl).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "adapter/yosys multi-module: realising module '{}' failed: {e}",
                out.module_name
            ),
            location: None,
        })?;
        clts_by_type.insert(out.module_name.clone(), clts);
    }

    // Rewrite each instance for composition (relabel readers + driver labels).
    let mut prepared: Vec<(String, Clts<DefaultStateIdx, DefaultLabelIdx>)> = Vec::new();
    for inst in &connectivity {
        let Some(base) = clts_by_type.get(&inst.module_type) else {
            continue; // type not lifted (black-box / unsupported) — skip
        };
        let dirs = directions.get(&inst.module_type);
        let mut readers: HashMap<String, String> = HashMap::new();
        let mut drivers: HashMap<String, String> = HashMap::new();
        for (port, net) in &inst.port_to_net {
            match dirs.and_then(|d| d.get(port)) {
                Some(BoundaryDirection::Output) => {
                    drivers.insert(port.clone(), net.clone());
                }
                // Input / Inout / unknown → reader (reads the net value).
                _ => {
                    readers.insert(port.clone(), net.clone());
                }
            }
        }
        let inst_clts = prepare_instance_for_composition(base, &readers, &drivers, &inst.instance)
            .map_err(|e| AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!(
                    "adapter/yosys multi-module: preparing instance '{}' failed: {e}",
                    inst.instance
                ),
                location: None,
            })?;
        prepared.push((inst.instance.clone(), inst_clts));
    }
    if prepared.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: "adapter/yosys multi-module: no liftable instances to compose".into(),
            location: None,
        });
    }

    // Synchronous fold-compose: each clock step advances every instance;
    // shared nets rendezvous on `<net>_<v>`, free inputs ride in the union.
    let comp_opts = CompositionOptions::new(CompositionSemantics::Synchronous);
    let mut iter = prepared.into_iter();
    let (first, mut acc) = iter.next().expect("prepared is non-empty");
    let mut instances = vec![first];
    for (name, next) in iter {
        acc = compose(&acc, &next, &comp_opts).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/yosys multi-module: composing '{name}' failed: {e}"),
            location: None,
        })?;
        instances.push(name);
    }

    Ok(MultiModuleComposition {
        composed: acc,
        instances,
    })
}

/// Realise one per-module CTXDSL (with inline valuations) into its `Clts`.
/// The BTOR2 adapter emits a single automaton per module; we take it.
fn realize_module_clts(ctxdsl: &str) -> Result<Clts<DefaultStateIdx, DefaultLabelIdx>, String> {
    let doc = crate::context_dsl::parser::parse(ctxdsl).map_err(|e| format!("parse: {e}"))?;
    let realized =
        crate::context_dsl::realize::realize(&doc, &[]).map_err(|e| format!("realize: {e}"))?;
    let name = realized
        .context
        .clts_names()
        .into_iter()
        .next()
        .ok_or_else(|| "no automaton in per-module ctxdsl".to_string())?;
    realized
        .context
        .clts(&name)
        .cloned()
        .ok_or_else(|| format!("automaton '{name}' missing after realise"))
}

/// R-MM-5a / CTXDSL Phase 1b — Serialise a `Clts` to CTXDSL by building a
/// single-automaton [`crate::adapter::ir::AdapterIR`] and reusing the
/// IR→CTXDSL emitter.
///
/// This is what lets a composed multi-module KMTS re-enter the standard
/// parse→realise→evaluate verify pipeline: the pipeline rebuilds the
/// predicate `Environment` from the inline state valuations, so no bespoke
/// direct-evaluation path is needed. The composed design becomes a
/// first-class, inspectable CTXDSL artifact.
///
/// The implementation moved to the frontend-neutral
/// [`crate::adapter::clts_to_ir`] module (CTXDSL Phase 1b) so the
/// predicate-cube / CEGAR path can reuse it. It now also carries each
/// state's 3-valued (Kleene) labels as `predicates_3v { … }` blocks — the
/// gap the prior multi-module note ("3-valued predicate labellings have no
/// CTXDSL syntax") flagged, now closed by Phase 1a (grammar) + 1b (emit).
/// Re-exported here for the existing R-MM multi-module callers.
pub use crate::adapter::clts_to_ir::clts_to_ctxdsl;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clts::{
        Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability, TransitionModality,
    };

    /// Helper: alphabet (label payload symbols) of a CLTS.
    fn alphabet(clts: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> Vec<String> {
        clts.alphabet()
    }

    #[test]
    fn reader_input_ports_are_relabelled_to_net_names() {
        // A consumer-like instance reading port `push` (value-encoded
        // labels push_0 / push_1) connected to net `valid`.
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        let s1 = b.state_with_name("s1".into()).unwrap();
        b.initial_state_id(s0);
        let push0 = b.labels().intern(["push_0"]).unwrap();
        let push1 = b.labels().intern(["push_1"]).unwrap();
        b.transition_ids(s0, &[push0], s0);
        b.transition_ids(s0, &[push1], s1);
        let clts = b.build().unwrap();

        let mut readers = HashMap::new();
        readers.insert("push".to_string(), "valid".to_string());
        let out =
            prepare_instance_for_composition(&clts, &readers, &HashMap::new(), "u_buffer").unwrap();

        let alpha = alphabet(&out);
        assert!(alpha.contains(&"valid_0".to_string()), "push_0 → valid_0");
        assert!(alpha.contains(&"valid_1".to_string()), "push_1 → valid_1");
        assert!(
            !alpha.contains(&"push_0".to_string()),
            "old reader label gone"
        );
    }

    #[test]
    fn driver_output_synthesises_net_label_from_valuation() {
        // A producer-like instance driving output `valid` (net `valid`).
        // s0: valid=F → leaving transitions carry valid_0; s1: valid=T →
        // valid_1. The free input `enable` is instance-namespaced.
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        let s1 = b.state_with_name("s1".into()).unwrap();
        b.initial_state_id(s0);
        let en0 = b.labels().intern(["enable_0"]).unwrap();
        let en1 = b.labels().intern(["enable_1"]).unwrap();
        b.transition_ids(s0, &[en1], s1); // from s0 (valid=F)
        b.transition_ids(s1, &[en0], s0); // from s1 (valid=T)
        let mut v0 = std::collections::BTreeMap::new();
        v0.insert("valid".to_string(), "F".to_string());
        b.with_valuation_for_state(s0, v0);
        let mut v1 = std::collections::BTreeMap::new();
        v1.insert("valid".to_string(), "T".to_string());
        b.with_valuation_for_state(s1, v1);
        let clts = b.build().unwrap();

        let mut drivers = HashMap::new();
        drivers.insert("valid".to_string(), "valid".to_string());
        let out = prepare_instance_for_composition(&clts, &HashMap::new(), &drivers, "u_producer")
            .unwrap();

        // The transition from s0 (valid=F) carries valid_0; from s1, valid_1.
        let s0n = out.state_id("s0").unwrap();
        let s1n = out.state_id("s1").unwrap();
        let labels_of = |st| {
            out.outgoing(st)
                .iter()
                .flat_map(|t| t.labels())
                .filter_map(|&l| out.label_payload(l))
                .flatten()
                .cloned()
                .collect::<Vec<String>>()
        };
        let from_s0 = labels_of(s0n);
        assert!(
            from_s0.contains(&"valid_0".to_string()),
            "s0 drives valid_0"
        );
        assert!(
            from_s0.contains(&"u_producer__enable_1".to_string()),
            "free input enable is instance-namespaced"
        );
        let from_s1 = labels_of(s1n);
        assert!(
            from_s1.contains(&"valid_1".to_string()),
            "s1 drives valid_1"
        );
    }

    #[test]
    fn driver_net_value_surfaced_under_bare_net_name() {
        // R-MM-7: a driver's output value is surfaced BOTH instance-qualified
        // (`u_p__valid`) AND under the bare connected-NET name — using the net
        // name (`handshake`), not the port name (`valid`) — so a composed
        // property can reference the shared net directly.
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        let s1 = b.state_with_name("s1".into()).unwrap();
        b.initial_state_id(s0);
        let step = b.labels().intern(["step"]).unwrap();
        b.transition_ids(s0, &[step], s1);
        b.transition_ids(s1, &[step], s0);
        let mut v0 = std::collections::BTreeMap::new();
        v0.insert("valid".to_string(), "F".to_string());
        b.with_valuation_for_state(s0, v0);
        let mut v1 = std::collections::BTreeMap::new();
        v1.insert("valid".to_string(), "T".to_string());
        b.with_valuation_for_state(s1, v1);
        let clts = b.build().unwrap();

        let mut drivers = HashMap::new();
        drivers.insert("valid".to_string(), "handshake".to_string());
        let out =
            prepare_instance_for_composition(&clts, &HashMap::new(), &drivers, "u_p").unwrap();

        let s1n = out.state_id("s1").unwrap();
        let v = out.state_valuation(s1n).unwrap();
        assert_eq!(
            v.get("u_p__valid").map(String::as_str),
            Some("1"),
            "instance-qualified valuation retained"
        );
        assert_eq!(
            v.get("handshake").map(String::as_str),
            Some("1"),
            "bare NET name surfaced (net `handshake`, not port `valid`)"
        );
        let s0n = out.state_id("s0").unwrap();
        let v0 = out.state_valuation(s0n).unwrap();
        assert_eq!(
            v0.get("handshake").map(String::as_str),
            Some("0"),
            "bare net value tracks the driver's state"
        );
    }

    #[test]
    fn step_label_is_kept_verbatim() {
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        b.initial_state_id(s0);
        let step = b.labels().intern(["step"]).unwrap();
        b.set_label_controllability(step, LabelControllability::Uncontrollable);
        b.transition_ids(s0, &[step], s0);
        let clts = b.build().unwrap();

        let out = prepare_instance_for_composition(&clts, &HashMap::new(), &HashMap::new(), "u_x")
            .unwrap();
        assert!(
            alphabet(&out).contains(&"step".to_string()),
            "step kept verbatim"
        );
    }

    #[test]
    fn modality_and_predicates_are_preserved() {
        use crate::clts::Tristate;
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        let s1 = b.state_with_name("s1".into()).unwrap();
        b.initial_state_id(s0);
        let lab = b.labels().intern(["enable_1"]).unwrap();
        b.transition_ids_with_modality(s0, &[lab], s1, TransitionModality::MayOnly);
        b.with_3valued_predicate(s1, "ready", Tristate::KleeneBot);
        let clts = b.build().unwrap();

        let out = prepare_instance_for_composition(&clts, &HashMap::new(), &HashMap::new(), "u_x")
            .unwrap();
        let s0n = out.state_id("s0").unwrap();
        let modality = out.outgoing(s0n)[0].modality().clone();
        assert_eq!(modality, TransitionModality::MayOnly, "modality preserved");
        let s1n = out.state_id("s1").unwrap();
        assert_eq!(
            out.state_3valued_predicate(s1n, "u_x__ready"),
            Some(Tristate::KleeneBot),
            "3-valued predicate preserved + instance-qualified"
        );
    }

    #[test]
    fn valuations_are_normalised_to_numeric_and_qualified() {
        // R-MM-5b — a 1-bit output surfaced as T/F plus a numeric register;
        // after prepare, the composed product must be all-numeric (T→1, F→0)
        // and instance-qualified so the verify pipeline's `abstract_states`
        // channel (gated on every valuation parsing as i64) binds composed
        // property atoms like `u_c__state == k`.
        let mut b = Clts::builder();
        let s0 = b.state_with_name("s0".into()).unwrap();
        b.initial_state_id(s0);
        let step = b.labels().intern(["step"]).unwrap();
        b.transition_ids(s0, &[step], s0);
        let mut v = std::collections::BTreeMap::new();
        v.insert("state".to_string(), "2".to_string());
        v.insert("valid".to_string(), "T".to_string());
        b.with_valuation_for_state(s0, v);
        let clts = b.build().unwrap();

        let out = prepare_instance_for_composition(&clts, &HashMap::new(), &HashMap::new(), "u_c")
            .unwrap();
        let s0n = out.state_id("s0").unwrap();
        let val = out.state_valuation(s0n).expect("valuation");
        assert_eq!(
            val.get("u_c__state").map(String::as_str),
            Some("2"),
            "numeric register stays numeric + instance-qualified"
        );
        assert_eq!(
            val.get("u_c__valid").map(String::as_str),
            Some("1"),
            "1-bit T → 1 (numeric) + instance-qualified"
        );
        assert!(
            val.values().all(|s| s.parse::<i64>().is_ok()),
            "all valuation values are numeric (the verify-pipeline gate); got {val:?}"
        );
    }

    #[test]
    fn qualified_numeric_atom_resolves_through_verify_pipeline() {
        // R-MM-5b — the open risk, closed in isolation (no yosys): a composed
        // product with instance-qualified valuations re-enters the verify
        // pipeline (prepare → clts_to_ctxdsl → parse → realise →
        // environment_for → evaluate) and a property atom `u_c__state == 2`
        // binds to the actual value at every state — proving multi-module
        // composed properties resolve.
        use crate::mu_calculus::evaluator::{EvaluationOptions, evaluate_with_options};

        // Consumer-like instance: idle(state=0,valid=F) --step--> busy(state=2,
        // valid=T), busy self-loops (a sink).
        let mut b = Clts::builder();
        let idle = b.state_with_name("idle".into()).unwrap();
        let busy = b.state_with_name("busy".into()).unwrap();
        b.initial_state_id(idle);
        let step = b.labels().intern(["step"]).unwrap();
        b.transition_ids(idle, &[step], busy);
        b.transition_ids(busy, &[step], busy);
        let mut vi = std::collections::BTreeMap::new();
        vi.insert("state".to_string(), "0".to_string());
        vi.insert("valid".to_string(), "F".to_string());
        b.with_valuation_for_state(idle, vi);
        let mut vb = std::collections::BTreeMap::new();
        vb.insert("state".to_string(), "2".to_string());
        vb.insert("valid".to_string(), "T".to_string());
        b.with_valuation_for_state(busy, vb);
        let clts = b.build().unwrap();

        // Prepare (normalises + qualifies), round-trip to CTXDSL, realise.
        let prepared =
            prepare_instance_for_composition(&clts, &HashMap::new(), &HashMap::new(), "u_c")
                .unwrap();
        let ctxdsl = clts_to_ctxdsl(&prepared, "system", "mm_system").unwrap();
        let doc = crate::context_dsl::parser::parse(&ctxdsl).expect("parse emitted ctxdsl");
        let realized =
            crate::context_dsl::realize::realize(&doc, &[]).expect("realise emitted ctxdsl");
        let name = realized.context.clts_names().into_iter().next().unwrap();
        let composed = realized.context.clts(&name).unwrap();
        let env = realized.environment_for(&name);
        let opts = EvaluationOptions::default();

        let holds_at_initial = |formula_src: &str| -> bool {
            let formula = crate::mu_calculus::parser::parse(formula_src).expect("parse formula");
            let result = evaluate_with_options(&formula, composed, &env, &opts).expect("evaluate");
            composed
                .initial_states()
                .iter()
                .all(|sid| result.get(sid.index()).map(|b| *b).unwrap_or(false))
        };

        // "state==2 is reachable" — TRUE (idle --step--> busy, where state==2).
        assert!(
            holds_at_initial("mu X. ((u_c__state == 2) || (<> X))"),
            "reachability binds the qualified atom u_c__state == 2"
        );
        // "never state==2" (safety) — FALSE (busy with state==2 is reachable).
        // Confirms the atom resolves to a non-vacuous bitset, not the
        // empty-bitset under-approximation a missing predicate would give.
        assert!(
            !holds_at_initial("nu X. ((!(u_c__state == 2)) && ([] X))"),
            "safety fails because state==2 is reachable (atom is non-vacuous)"
        );
    }

    /// A yosys WITH `read_slang` (the slang-only lift, 2026-11-04): a brew 0.59 skips.
    fn yosys_available() -> bool {
        super::super::yosys_can_read_slang()
    }

    /// R-MM-4d — end-to-end: compose `producer_consumer_top` (producer ⊗
    /// bounded_buffer ⊗ consumer) through the real yosys pipeline into one
    /// KMTS. Validates the whole driver: lift → surface net-driving outputs
    /// → prepare each instance → synchronous fold-compose. Structural
    /// assertions (no property eval): the shared-net rendezvous labels
    /// survived and the synchronisation pruned the naive product.
    #[test]
    fn compose_sv_multi_module_producer_consumer_top() {
        if !yosys_available() {
            eprintln!("skip: yosys not installed");
            return;
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog");
        let read = |f: &str| std::fs::read_to_string(dir.join(f));
        let (top, producer, consumer, buffer) = match (
            read("multi_producer_consumer_top.sv"),
            read("multi_producer.sv"),
            read("multi_consumer.sv"),
            read("bounded_buffer.sv"),
        ) {
            (Ok(t), Ok(p), Ok(c), Ok(b)) => (t, p, c, b),
            _ => {
                eprintln!("skip: multi-module fixtures not found");
                return;
            }
        };
        let yopts = YosysOptions {
            top: Some("producer_consumer_top".into()),
            per_module_btor: true,
            additional_sources: vec![
                ("multi_producer.sv".into(), producer),
                ("multi_consumer.sv".into(), consumer),
                ("bounded_buffer.sv".into(), buffer),
            ],
            ..Default::default()
        };

        let comp = compose_sv_multi_module(&top, &AdapterOptions::default(), &yopts)
            .expect("multi-module composition");

        // All three instances folded in.
        assert_eq!(comp.instances.len(), 3, "producer + buffer + consumer");

        let composed = &comp.composed;
        assert!(composed.state_count() > 1, "non-trivial product");

        // The shared-net `valid` rendezvous labels survived into the product
        // (proves the driver synthesised driver labels + relabelled readers).
        let alpha = composed.alphabet();
        assert!(
            alpha.iter().any(|l| l == "valid_0") && alpha.iter().any(|l| l == "valid_1"),
            "shared-net rendezvous labels present; got {alpha:?}"
        );

        // Synchronisation + reachability pruned the naive product. Each
        // module has at most 4 register-states, so a free product would be
        // up to 4×4×4 = 64; the rendezvous on `valid` constrains it.
        assert!(
            composed.state_count() < 64,
            "rendezvous + reachability pruned the product; got {}",
            composed.state_count()
        );

        // Composed states carry instance-qualified valuations (so a property
        // can reference each module's register unambiguously).
        let has_qualified = composed.states().any(|s| {
            composed
                .state_valuation(s)
                .is_some_and(|v| v.keys().any(|k| k.starts_with("u_producer__")))
        });
        assert!(
            has_qualified,
            "instance-qualified valuations present on the product"
        );
    }

    /// R-MM-7 — the bare shared-net valuation re-enters the verify pipeline:
    /// the composed `producer_consumer_top` carries `valid` (the net name,
    /// un-qualified) and a property referencing the net directly (`valid == 1`)
    /// binds non-vacuously through clts_to_ctxdsl → parse → realise → evaluate.
    #[test]
    fn bare_net_value_resolves_in_composed_property() {
        use crate::mu_calculus::evaluator::{EvaluationOptions, evaluate_with_options};
        let Some(comp) = compose_producer_consumer() else {
            eprintln!("skip: yosys / fixtures unavailable");
            return;
        };
        // The product carries the bare shared-net valuation `valid` (R-MM-7),
        // not only `u_producer__valid`.
        let has_bare_valid = comp.composed.states().any(|s| {
            comp.composed
                .state_valuation(s)
                .is_some_and(|v| v.contains_key("valid"))
        });
        assert!(
            has_bare_valid,
            "bare net `valid` valuation surfaced on the composed product"
        );

        let ctxdsl = clts_to_ctxdsl(&comp.composed, "system", "mm_system").unwrap();
        let doc = crate::context_dsl::parser::parse(&ctxdsl).unwrap();
        let realized = crate::context_dsl::realize::realize(&doc, &[]).unwrap();
        let name = realized.context.clts_names().into_iter().next().unwrap();
        let composed = realized.context.clts(&name).unwrap();
        let env = realized.environment_for(&name);
        let f = crate::mu_calculus::parser::parse("valid == 1").unwrap();
        let result =
            evaluate_with_options(&f, composed, &env, &EvaluationOptions::default()).unwrap();
        let sat = (0..composed.state_count())
            .filter(|i| result.get(*i).map(|b| *b).unwrap_or(false))
            .count();
        assert!(
            sat > 0 && sat < composed.state_count(),
            "bare-net atom `valid == 1` binds non-vacuously: {sat}/{}",
            composed.state_count()
        );
    }

    /// Compose `producer_consumer_top` via the real yosys pipeline, or
    /// `None` when yosys / the fixtures are unavailable (test skip).
    fn compose_producer_consumer() -> Option<MultiModuleComposition> {
        if !yosys_available() {
            return None;
        }
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        let top = read("multi_producer_consumer_top.sv")?;
        let producer = read("multi_producer.sv")?;
        let consumer = read("multi_consumer.sv")?;
        let buffer = read("bounded_buffer.sv")?;
        let yopts = YosysOptions {
            top: Some("producer_consumer_top".into()),
            per_module_btor: true,
            additional_sources: vec![
                ("multi_producer.sv".into(), producer),
                ("multi_consumer.sv".into(), consumer),
                ("bounded_buffer.sv".into(), buffer),
            ],
            ..Default::default()
        };
        compose_sv_multi_module(&top, &AdapterOptions::default(), &yopts).ok()
    }

    /// R-MM-5a — the composed KMTS round-trips through CTXDSL: emit it, then
    /// re-parse + realise, and confirm the structure (state count, the
    /// shared-net rendezvous label, instance-qualified valuations) survives.
    /// This is what makes the composed design re-enter the verify pipeline.
    #[test]
    fn clts_to_ctxdsl_round_trips_composed_design() {
        let Some(comp) = compose_producer_consumer() else {
            eprintln!("skip: yosys / fixtures unavailable");
            return;
        };
        let ctxdsl = clts_to_ctxdsl(&comp.composed, "system", "mm_system").expect("emit ctxdsl");

        // Re-parse + realise the emitted CTXDSL — proves it is valid, and
        // recovers an equivalent Clts via the standard pipeline.
        let doc = crate::context_dsl::parser::parse(&ctxdsl).expect("re-parse emitted ctxdsl");
        let realized =
            crate::context_dsl::realize::realize(&doc, &[]).expect("re-realise emitted ctxdsl");
        let name = realized
            .context
            .clts_names()
            .into_iter()
            .next()
            .expect("one automaton");
        let recovered = realized.context.clts(&name).expect("recovered clts");

        assert_eq!(
            recovered.state_count(),
            comp.composed.state_count(),
            "state count survives the CTXDSL round-trip"
        );
        let alpha = recovered.alphabet();
        assert!(
            alpha.iter().any(|l| l == "valid_0") && alpha.iter().any(|l| l == "valid_1"),
            "shared-net rendezvous labels survive the round-trip; got {alpha:?}"
        );
        // Instance-qualified valuations survive (so a composed property can
        // reference each module's signals after the round-trip).
        let has_qualified = recovered.states().any(|s| {
            recovered
                .state_valuation(s)
                .is_some_and(|v| v.keys().any(|k| k.starts_with("u_producer__")))
        });
        assert!(
            has_qualified,
            "instance-qualified valuations survive the round-trip"
        );
    }
}
