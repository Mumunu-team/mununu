//! Shared C-extraction types: `RegisterAccess`, `AccessFlow`, and
//! the CTXDSL automaton synthesiser.
//!
//! Historical note: this file used to host an AST-based C extractor
//! (slices 1, 2.a, 2.b, 2.c — PRs #33, #34, #35, #38). At phase L3
//! of the principled-lift plan (see
//! `~/.claude/plans/i-want-you-to-distributed-orbit.md`) the AST
//! backend reached parity with the IR-based backend at
//! [`crate::codesign::c_extract_llvm`] and was removed in favour of
//! it. What survives here are the **shared types** the LLVM
//! extractor still emits — they define the wire format the
//! downstream codesign pipeline expects.
//!
//! See `docs/archive/design/c-extraction-correctness-scope.md` for the
//! soundness framing that motivated the switch.

use crate::codesign::coupling::{AccessKind, rendezvous_label_name};
use crate::codesign::register_map::RegisterMap;
use serde::{Deserialize, Serialize};

/// A single register access lifted from a C function body.
///
/// The `accessor` field used to be the C expression as the
/// programmer typed it (back when the AST extractor was the
/// canonical lifter). With the IR-based extractor, this field now
/// holds an `(IR-resolved @<symbolic-address>)` diagnostic string
/// — the symbolic form is more informative than what the C surface
/// would have shown anyway, especially for macro-expanded
/// accessors. Downstream consumers should treat the field as
/// opaque metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAccess {
    /// Whether the access is a read or a write.
    pub kind: AccessKind,
    /// The register's `name` from the supplied
    /// [`RegisterMap`](crate::codesign::register_map::RegisterMap).
    pub register: String,
    /// The field's `name`. `None` when the matched accessor refers
    /// to the whole register (e.g. a data-payload register with no
    /// declared fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Diagnostic string describing the access — historical accessor
    /// expression for AST extractions, IR-resolved symbolic address
    /// for LLVM extractions.
    pub accessor: String,
    /// 1-based source-like ordering counter. For IR extractions
    /// this is a monotonic walker counter (the IR has no `!dbg`
    /// metadata by default).
    pub source_line: u32,
    /// Control-flow context for this access.
    #[serde(default, skip_serializing_if = "AccessFlow::is_linear")]
    pub flow: AccessFlow,
    /// Phase-L9 (gap 1 from the verification audit): user-supplied
    /// name for the SOURCE state of this access's transition,
    /// emitted by a `MUNUNU_STATE("Name")` marker call in the C
    /// source. The synthesiser uses this name instead of the
    /// default `S<i>` when rendering the transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_state_hint: Option<String>,
}

/// Control-flow context for a [`RegisterAccess`].
///
/// `Linear` is the default — one access produces one transition
/// from the previous state to a new state.
///
/// `PollingLoop` is the special case for `while (cond) ;` (or
/// `while (cond) {}` with an empty body) where `cond` is a single
/// register-access read. It produces *three* transitions on the
/// same label, all sharing one new state:
///
/// - `prev → Loop_<i>` (enter the loop — read returns "stay polling")
/// - `Loop_<i> → Loop_<i>` (loop iteration — read still busy)
/// - `Loop_<i> → next` (exit the loop — read returns "go")
///
/// Only the *exit* transition advances state. This is the smallest
/// faithful encoding of a polling loop: the verifier sees that the
/// firmware may stay in `Loop_<i>` arbitrarily long
/// (over-approximation — sound for safety) and that it eventually
/// leaves via the same label (matching the hand-authored Doc C
/// §C.4 `firmware.ctxdsl` shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessFlow {
    /// Default. One linear transition `prev → next` on the access
    /// label.
    #[default]
    Linear,
    /// Polling-loop pattern. Three transitions on the same label
    /// sharing one new state — see [`AccessFlow`] doc-comment.
    PollingLoop,
}

impl AccessFlow {
    /// Used by `#[serde(skip_serializing_if = "AccessFlow::is_linear")]`
    /// to keep the wire format minimal when the access is the default
    /// linear flow.
    pub fn is_linear(&self) -> bool {
        matches!(self, AccessFlow::Linear)
    }
}

/// Synthesise a CTXDSL `automaton { … }` block from a sequence of
/// register accesses.
///
/// The emitted automaton has `N+1` states for a sequence of `N`
/// linear accesses, plus one extra `Loop_<i>` state per
/// [`AccessFlow::PollingLoop`] access. The labels follow the
/// [`crate::codesign::coupling::rendezvous_label_name`] convention
/// so the firmware automaton synchronises with the peripheral
/// chaotic stub on the same alphabet.
///
/// All firmware-driven write labels are declared as
/// `controllable { … }`; reads are left uncontrollable (the
/// default). This matches Doc A §4 and the per-side classification
/// in [`crate::codesign::coupling::register_map_labels`].
pub fn synthesise_automaton_ctxdsl(
    function_name: &str,
    accesses: &[RegisterAccess],
    rm: &RegisterMap,
) -> String {
    use std::fmt::Write;

    let automaton_name = sanitise_ident_for_ctxdsl(function_name);
    let mut buf = String::new();
    let _ = writeln!(buf, "    automata {{");
    let _ = writeln!(buf, "        automaton {automaton_name} {{");

    // Labels — collect controllable writes for the controllable {…}
    // declaration. Reads default to uncontrollable.
    let mut controllable_labels: Vec<String> = Vec::new();
    for access in accesses {
        let label = rendezvous_label_name(&access.register, access.field.as_deref(), access.kind);
        if access.kind == AccessKind::Write && !controllable_labels.contains(&label) {
            controllable_labels.push(label);
        }
    }
    if !controllable_labels.is_empty() {
        // CTXDSL's `controllable { ... }` block expects each entry as
        // `label NAME;` (parser at
        // `crate::context_dsl::parser::parse_label_block` calls
        // `expect_keyword(Keyword::Label)`). Without the `label`
        // keyword the synthesised CTXDSL fails to parse when spliced
        // into a `context { ... }` block — which broke the end-to-end
        // "extract C → eval against CTXDSL" workflow before this fix.
        let _ = writeln!(buf, "            controllable {{");
        for label in &controllable_labels {
            let _ = writeln!(buf, "                label {label};");
        }
        let _ = writeln!(buf, "            }}");
        let _ = writeln!(buf);
    }

    // Phase L9 (gap 1): state-name resolution. `state_names[i]` is
    // the name of the state reached just before access[i] (so
    // `state_names[0]` is the initial state, and `state_names[N]` is
    // the terminal state after the last access). The default name is
    // `S{i}`; a `MUNUNU_STATE("Foo")` marker upstream of access[i]
    // populates `access[i].source_state_hint` and renames
    // `state_names[i]` to `Foo` (sanitised to a CTXDSL identifier).
    let mut state_names: Vec<String> = (0..=accesses.len()).map(|i| format!("S{i}")).collect();
    for (i, access) in accesses.iter().enumerate() {
        if let Some(hint) = &access.source_state_hint {
            state_names[i] = sanitise_state_name(hint);
        }
    }

    let _ = writeln!(buf, "            states {{");
    let _ = writeln!(buf, "                state {} initial;", state_names[0]);
    for (i, access) in accesses.iter().enumerate() {
        if access.flow == AccessFlow::PollingLoop {
            let _ = writeln!(buf, "                state Loop{i};");
        }
        let _ = writeln!(buf, "                state {};", state_names[i + 1]);
    }
    let _ = writeln!(buf, "            }}");
    let _ = writeln!(buf);

    let _ = writeln!(buf, "            transitions {{");
    for (i, access) in accesses.iter().enumerate() {
        let label = rendezvous_label_name(&access.register, access.field.as_deref(), access.kind);
        let from = &state_names[i];
        let to = &state_names[i + 1];
        match access.flow {
            AccessFlow::Linear => {
                let _ = writeln!(
                    buf,
                    "                transition {from} -> {to} on label {label}; // {accessor}",
                    accessor = access.accessor
                );
            }
            AccessFlow::PollingLoop => {
                let loop_state = format!("Loop{i}");
                let _ = writeln!(
                    buf,
                    "                transition {from} -> {loop_state} on label {label}; // {accessor} (enter loop)",
                    accessor = access.accessor
                );
                let _ = writeln!(
                    buf,
                    "                transition {loop_state} -> {loop_state} on label {label}; // {accessor} (loop iteration)",
                    accessor = access.accessor
                );
                let _ = writeln!(
                    buf,
                    "                transition {loop_state} -> {to} on label {label}; // {accessor} (exit loop)",
                    accessor = access.accessor
                );
            }
        }
    }
    let _ = writeln!(buf, "            }}");
    let _ = writeln!(buf, "        }}");
    let _ = writeln!(buf, "    }}");

    let _ = rm; // unused — kept for parity with coupling.rs's emitter signature.
    buf
}

/// Phase L9 (gap 1): sanitise a user-supplied state-name hint from
/// a `MUNUNU_STATE("…")` marker into a CTXDSL state identifier.
/// CTXDSL states are parsed as plain identifiers, so any non-alnum
/// becomes `_`; a leading digit is prefixed with `S_` so the result
/// starts with a letter; empty input falls back to `S`.
fn sanitise_state_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "S".to_string();
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert_str(0, "S_");
    }
    out
}

/// Sanitise a C identifier into a CTXDSL automaton name: first
/// char uppercase, rest preserved (CTXDSL is case-sensitive but
/// convention is PascalCase for automaton names; we cheaply convert
/// `uart_send` → `Uart_send`). Non-alnum becomes `_`. Empty input
/// produces `Func`.
fn sanitise_ident_for_ctxdsl(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut first = true;
    for c in name.chars() {
        let safe = if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        };
        if first {
            out.push(safe.to_ascii_uppercase());
            first = false;
        } else {
            out.push(safe);
        }
    }
    if out.is_empty() {
        out.push_str("Func");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codesign::register_map::{
        AccessPath, Field, Register, RegisterDirection, RegisterMap, VisibilityClass,
    };

    fn uart_register_map() -> RegisterMap {
        RegisterMap {
            peripheral: "UART_LITE".to_string(),
            base_address: "0x40010000".to_string(),
            description: None,
            contract_uri: None,
            registers: vec![Register {
                name: "CTRL".to_string(),
                offset: 0,
                width_bits: 32,
                direction: RegisterDirection::Rw,
                visibility_class: VisibilityClass::Control,
                access_path: AccessPath::MmioDirect,
                description: None,
                fields: vec![Field {
                    name: "tx_start".to_string(),
                    bits: [0, 0],
                    sv_signal: None,
                    c_accessor: None,
                    description: None,
                }],
            }],
        }
    }

    #[test]
    fn sanitise_ident_pascal_cases_first_char() {
        assert_eq!(sanitise_ident_for_ctxdsl("uart_send"), "Uart_send");
        assert_eq!(sanitise_ident_for_ctxdsl("Foo"), "Foo");
        assert_eq!(sanitise_ident_for_ctxdsl(""), "Func");
    }

    #[test]
    fn linear_access_emits_one_state_per_access() {
        let accesses = vec![RegisterAccess {
            kind: AccessKind::Write,
            register: "CTRL".to_string(),
            field: Some("tx_start".to_string()),
            accessor: "(IR)".to_string(),
            source_line: 1,
            flow: AccessFlow::Linear,
            source_state_hint: None,
        }];
        let ctxdsl = synthesise_automaton_ctxdsl("fire", &accesses, &uart_register_map());
        assert!(ctxdsl.contains("state S0 initial"));
        assert!(ctxdsl.contains("state S1"));
        assert!(!ctxdsl.contains("Loop"));
        assert!(ctxdsl.contains("S0 -> S1 on label wr_ctrl_tx_start"));
    }

    #[test]
    fn polling_loop_access_emits_three_transitions_on_same_label() {
        let accesses = vec![RegisterAccess {
            kind: AccessKind::Read,
            register: "CTRL".to_string(),
            field: Some("tx_start".to_string()),
            accessor: "(IR)".to_string(),
            source_line: 1,
            flow: AccessFlow::PollingLoop,
            source_state_hint: None,
        }];
        let ctxdsl = synthesise_automaton_ctxdsl("poll", &accesses, &uart_register_map());
        assert!(ctxdsl.contains("state Loop0"));
        assert!(ctxdsl.contains("state S1"));
        let n = ctxdsl.matches("rd_ctrl_tx_start").count();
        assert!(n >= 3, "expected ≥3 enter/iterate/exit; got {n}");
    }

    #[test]
    fn sanitise_state_name_handles_edge_cases() {
        assert_eq!(sanitise_state_name("Polling"), "Polling");
        assert_eq!(sanitise_state_name("Tx Ready"), "Tx_Ready");
        assert_eq!(sanitise_state_name("3State"), "S_3State");
        assert_eq!(sanitise_state_name(""), "S");
        assert_eq!(sanitise_state_name("with-dashes"), "with_dashes");
    }

    #[test]
    fn state_hint_renames_source_states_in_synthesised_automaton() {
        let accesses = vec![
            RegisterAccess {
                kind: AccessKind::Read,
                register: "CTRL".to_string(),
                field: Some("tx_start".to_string()),
                accessor: "(IR)".to_string(),
                source_line: 1,
                flow: AccessFlow::PollingLoop,
                source_state_hint: Some("Polling".to_string()),
            },
            RegisterAccess {
                kind: AccessKind::Write,
                register: "CTRL".to_string(),
                field: Some("tx_start".to_string()),
                accessor: "(IR)".to_string(),
                source_line: 2,
                flow: AccessFlow::Linear,
                source_state_hint: Some("Ready".to_string()),
            },
        ];
        let ctxdsl = synthesise_automaton_ctxdsl("uart_send", &accesses, &uart_register_map());
        // Initial state is renamed by the first hint.
        assert!(
            ctxdsl.contains("state Polling initial"),
            "expected initial state renamed to 'Polling'; got:\n{ctxdsl}"
        );
        // Second hint renames the state between the two accesses.
        assert!(ctxdsl.contains("state Ready;"));
        // Terminal state after the last access keeps its default
        // name since no further hint applies.
        assert!(ctxdsl.contains("state S2;"));
        // Transitions reference the renamed states.
        assert!(
            ctxdsl.contains("Polling -> Loop0"),
            "expected polling-loop entry from 'Polling' state; got:\n{ctxdsl}"
        );
        assert!(
            ctxdsl.contains("Loop0 -> Ready"),
            "expected polling-loop exit into 'Ready' state; got:\n{ctxdsl}"
        );
        assert!(
            ctxdsl.contains("Ready -> S2"),
            "expected linear transition Ready -> S2; got:\n{ctxdsl}"
        );
    }

    #[test]
    fn synthesised_automaton_parses_back_through_ctxdsl_parser() {
        // Regression for the missing-`label`-keyword bug: the
        // synthesised `controllable { ... }` used to emit
        // `wr_data_byte;` without the `label` keyword, which the
        // CTXDSL parser rejected (it calls
        // `expect_keyword(Keyword::Label)` on every entry). The fix
        // emits `label wr_data_byte;` instead. This test wraps the
        // synthesised fragment in a `context { ... }` block and
        // confirms it round-trips through the parser.
        let accesses = vec![
            RegisterAccess {
                kind: AccessKind::Read,
                register: "CTRL".to_string(),
                field: Some("tx_start".to_string()),
                accessor: "(IR)".to_string(),
                source_line: 1,
                flow: AccessFlow::PollingLoop,
                source_state_hint: None,
            },
            RegisterAccess {
                kind: AccessKind::Write,
                register: "CTRL".to_string(),
                field: Some("tx_start".to_string()),
                accessor: "(IR)".to_string(),
                source_line: 2,
                flow: AccessFlow::Linear,
                source_state_hint: None,
            },
        ];
        let fragment = synthesise_automaton_ctxdsl("demo", &accesses, &uart_register_map());
        // The fragment is the `automata { ... }` block alone; wrap it
        // in a context for the parser.
        let full = format!("context Demo {{\n{fragment}}}\n");
        let parsed = crate::context_dsl::parse(&full).unwrap_or_else(|e| {
            panic!("synthesised CTXDSL failed to parse: {e:?}\n--- source ---\n{full}")
        });
        assert_eq!(parsed.automata.len(), 1);
        let auto = &parsed.automata[0];
        assert_eq!(auto.name.name, "Demo");
        // The controllable block carries the write label.
        assert!(
            auto.controllable
                .iter()
                .any(|l| l.name.name == "wr_ctrl_tx_start"),
            "expected wr_ctrl_tx_start in controllable block; got {:?}",
            auto.controllable
        );
    }
}
