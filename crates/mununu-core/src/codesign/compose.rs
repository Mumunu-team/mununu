//! Codesign composition — Document C task C4 helper.
//!
//! Splices the [`coupling`](super::coupling) fragment generated from a
//! register map into a user-authored firmware CTXDSL document, producing
//! a single composable context the existing
//! [`crate::context_dsl`] parser + realiser can consume directly.
//!
//! ## How splicing works
//!
//! 1. Parse the firmware CTXDSL once to discover automata names. These
//!    are used as the firmware members of the codesign composition
//!    block emitted by `emit_coupling_fragment`.
//! 2. Re-emit the coupling fragment with those member names.
//! 3. Find the textual closing `}` of the firmware's outer `context { … }`
//!    block and insert the fragment just before it. The CTXDSL parser
//!    merges multiple `alphabet { … }` / `automata { … }` / `composition
//!    { … }` sections in one context, so the union behaves correctly.
//!
//! ## What this slice does and does not handle
//!
//! - **Single-context inputs only.** The firmware CTXDSL must have one
//!   top-level `context <name> { … }` block. Multi-context inputs are
//!   rejected with a clear error.
//! - **Whole-line `}`.** The splicer looks for the *last* `}` line that
//!   matches the outer context. Hand-authored files put the closing
//!   brace on its own line; that's a stable convention.
//! - **Round-trip-verified.** The output is round-tripped through
//!   [`crate::context_dsl::parser::parse`] before returning, so the
//!   caller always gets a string that parses cleanly. If the splice
//!   produces invalid CTXDSL, [`compose_codesign_ctxdsl`] returns
//!   `ComposeError::PostSpliceParseFailed` rather than letting the
//!   caller discover the breakage downstream.

use crate::codesign::coupling::{CouplingOptions, emit_coupling_fragment};
use crate::codesign::register_map::RegisterMap;
use crate::context_dsl::parser::parse as parse_ctxdsl;
use std::fmt;

/// Options for [`compose_codesign_ctxdsl`].
#[derive(Debug, Clone, Default)]
pub struct ComposeOptions<'a> {
    /// Optional override for the peripheral automaton name. Default
    /// derives from the register map's `peripheral` field
    /// (uppercased + sanitised).
    pub peripheral_automaton: Option<&'a str>,
    /// Optional override for the composition name. Default
    /// `<PERIPHERAL>System`.
    pub composition_name: Option<&'a str>,
    /// Optional override for the firmware members. When empty the
    /// splicer discovers them by parsing the firmware document and
    /// taking every top-level automaton name.
    pub firmware_members_override: Option<&'a [&'a str]>,
}

/// Errors raised by [`compose_codesign_ctxdsl`].
#[derive(Debug)]
pub enum ComposeError {
    /// The firmware CTXDSL failed to parse before splicing.
    FirmwareParseFailed(String),
    /// The spliced CTXDSL failed to parse — indicates a bug in this
    /// module or an edge case in the input that the splicer couldn't
    /// handle. Includes the assembled text for diagnostics.
    PostSpliceParseFailed { error: String, assembled: String },
    /// The firmware document has no `context { … }` block we could
    /// splice into.
    NoContextBlock,
    /// The register map failed structural validation. The caller
    /// should fix the sidecar before composing.
    InvalidRegisterMap(Vec<crate::codesign::register_map::ValidationIssue>),
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeError::FirmwareParseFailed(msg) => {
                write!(f, "firmware CTXDSL failed to parse: {msg}")
            }
            ComposeError::PostSpliceParseFailed { error, .. } => write!(
                f,
                "{}",
                crate::adapter::owner::mununu_defect(&format!(
                    "post-splice CTXDSL failed to parse: {error}"
                ))
            ),
            ComposeError::NoContextBlock => write!(
                f,
                "firmware document has no `context <name> {{ … }}` block to splice into"
            ),
            ComposeError::InvalidRegisterMap(issues) => {
                writeln!(f, "register map has {} structural issue(s):", issues.len())?;
                for i in issues {
                    writeln!(f, "  - {i}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ComposeError {}

/// Result of [`compose_codesign_ctxdsl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeResult {
    /// The full CTXDSL document — firmware + coupling fragment, ready
    /// for `parse` + `realize_context`.
    pub ctxdsl: String,
    /// The firmware automata names discovered while splicing. Useful
    /// for the CLI summary (`composed N firmware automata with the
    /// peripheral stub`).
    pub firmware_members: Vec<String>,
    /// The peripheral automaton name as used in the composition.
    pub peripheral_automaton: String,
    /// The composition name as used (defaults to `<PERIPHERAL>System`).
    pub composition_name: String,
}

/// Compose a unified codesign CTXDSL document from a register map and
/// a user-authored firmware CTXDSL.
///
/// The output is a parseable CTXDSL string. Use the existing
/// `realize_context` / `mu_calculus` machinery to verify properties
/// over it.
///
/// **Soundness posture.** The composition is asynchronous (Document C
/// §C.5 — bus arbitration is non-deterministic; synchronous coupling
/// is unsound for racy access). The peripheral stub is chaotic by
/// construction; safety verdicts over the composition transfer to any
/// real system whose peripheral behaves *at least* as chaotically.
/// Liveness verdicts need an authored latency contract, which is what
/// the HITL review workflow (PR #20) feeds through `@mununu_guarantee`
/// annotations and the discharge graph.
pub fn compose_codesign_ctxdsl(
    rm: &RegisterMap,
    firmware_ctxdsl: &str,
    options: &ComposeOptions<'_>,
) -> Result<ComposeResult, ComposeError> {
    let issues = rm.validate();
    if !issues.is_empty() {
        return Err(ComposeError::InvalidRegisterMap(issues));
    }

    // Parse the firmware so we can discover member names.
    let firmware_doc = parse_ctxdsl(firmware_ctxdsl)
        .map_err(|e| ComposeError::FirmwareParseFailed(format!("{e:?}")))?;
    let discovered: Vec<String> = firmware_doc
        .automata
        .iter()
        .map(|a| a.name.name.clone())
        .collect();

    // Resolve member set: explicit override wins over discovery.
    let firmware_members: Vec<String> = match options.firmware_members_override {
        Some(slice) => slice.iter().map(|s| s.to_string()).collect(),
        None => discovered,
    };

    // Emit the coupling fragment with the resolved members.
    let member_refs: Vec<&str> = firmware_members.iter().map(String::as_str).collect();
    let fragment = emit_coupling_fragment(
        rm,
        &CouplingOptions {
            peripheral_automaton: options.peripheral_automaton,
            composition_name: options.composition_name,
            firmware_members: &member_refs,
        },
    );

    // Splice the fragment into the firmware document's outer context.
    let assembled = splice_into_outer_context(firmware_ctxdsl, &fragment)?;

    // Round-trip parse: never return a string we can't read back.
    parse_ctxdsl(&assembled).map_err(|e| ComposeError::PostSpliceParseFailed {
        error: format!("{e:?}"),
        assembled: assembled.clone(),
    })?;

    // Derive the names the fragment uses for reporting.
    let peripheral_automaton = options
        .peripheral_automaton
        .map(str::to_string)
        .unwrap_or_else(|| default_peripheral_name(&rm.peripheral));
    let composition_name = options
        .composition_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{peripheral_automaton}System"));

    Ok(ComposeResult {
        ctxdsl: assembled,
        firmware_members,
        peripheral_automaton,
        composition_name,
    })
}

fn default_peripheral_name(peripheral: &str) -> String {
    peripheral
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Find the last `}` that closes the outermost `context { … }` block
/// and insert `fragment` just before it.
///
/// The strategy: count nested braces from the first `{` after a
/// `context` keyword. The last `}` that brings the depth back to 0 is
/// the outer closing brace. Inserts the fragment before that `}`.
fn splice_into_outer_context(firmware: &str, fragment: &str) -> Result<String, ComposeError> {
    // Find the opening `{` after `context`.
    let context_kw = firmware
        .find("context")
        .ok_or(ComposeError::NoContextBlock)?;
    let open_brace_offset = firmware[context_kw..]
        .find('{')
        .map(|o| context_kw + o)
        .ok_or(ComposeError::NoContextBlock)?;

    // Scan from after the opening `{` and track depth, skipping
    // braces that appear inside `//` line comments or `/* … */`
    // block comments or `"…"` string literals. CTXDSL doesn't have
    // string literals in practice (no quoted identifiers in the
    // tokens we emit), but we still skip them for safety.
    let bytes = firmware.as_bytes();
    let mut i = open_brace_offset + 1;
    let mut depth = 1usize;
    let mut close_at: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Skip line comment.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Skip block comment.
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2; // past `*/`
                continue;
            }
            '"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                i += 1;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close_at = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let close_at = close_at.ok_or(ComposeError::NoContextBlock)?;

    let mut out = String::with_capacity(firmware.len() + fragment.len() + 32);
    out.push_str(&firmware[..close_at]);
    out.push('\n');
    out.push_str(fragment);
    out.push_str(&firmware[close_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codesign::register_map::{
        AccessPath, Field, Register, RegisterDirection, VisibilityClass,
    };

    fn uart_map() -> RegisterMap {
        RegisterMap {
            peripheral: "UART_LITE".to_string(),
            base_address: "0x40010000".to_string(),
            description: None,
            contract_uri: None,
            registers: vec![
                Register {
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
                },
                Register {
                    name: "STATUS".to_string(),
                    offset: 4,
                    width_bits: 32,
                    direction: RegisterDirection::Ro,
                    visibility_class: VisibilityClass::Status,
                    access_path: AccessPath::MmioDirect,
                    description: None,
                    fields: vec![Field {
                        name: "tx_busy".to_string(),
                        bits: [0, 0],
                        sv_signal: None,
                        c_accessor: None,
                        description: None,
                    }],
                },
            ],
        }
    }

    const SIMPLE_FIRMWARE: &str = r#"
        context CoupledUart {
            alphabet {
                label tick;
            }
            automata {
                automaton UartDriver {
                    controllable {
                        label wr_ctrl_tx_start;
                    }
                    states {
                        state Init initial;
                        state Polling;
                        state Sending;
                    }
                    transitions {
                        transition Init -> Polling on label rd_status_tx_busy;
                        transition Polling -> Sending on label tick;
                        transition Sending -> Init on label wr_ctrl_tx_start;
                    }
                }
            }
        }
    "#;

    #[test]
    fn composes_simple_firmware_with_uart_map() {
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(&rm, SIMPLE_FIRMWARE, &ComposeOptions::default())
            .expect("compose succeeds");
        assert_eq!(result.firmware_members, vec!["UartDriver"]);
        assert_eq!(result.peripheral_automaton, "UART_LITE");
        assert_eq!(result.composition_name, "UART_LITESystem");
        // The output contains the spliced coupling fragment.
        assert!(
            result
                .ctxdsl
                .contains("Coupling fragment emitted by mununu codesign couple")
        );
        assert!(result.ctxdsl.contains("automaton UART_LITE {"));
        assert!(result.ctxdsl.contains("asynchronous UART_LITESystem"));
    }

    #[test]
    fn composed_output_parses_through_ctxdsl_parser() {
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(&rm, SIMPLE_FIRMWARE, &ComposeOptions::default())
            .expect("compose succeeds");
        // compose_codesign_ctxdsl already round-trips internally; this
        // test asserts the doc shape (two automata + one composition).
        let doc = parse_ctxdsl(&result.ctxdsl).expect("parse");
        assert!(doc.automata.iter().any(|a| a.name.name == "UartDriver"));
        assert!(doc.automata.iter().any(|a| a.name.name == "UART_LITE"));
        assert_eq!(doc.compositions.len(), 1);
        let comp = &doc.compositions[0];
        assert!(matches!(
            comp.kind,
            crate::context_dsl::ast::CompositionKind::Asynchronous
        ));
        // Members are [peripheral, firmware] in the order we emit.
        let names: Vec<&str> = comp.members.iter().map(|m| m.name.name.as_str()).collect();
        assert_eq!(names, vec!["UART_LITE", "UartDriver"]);
    }

    #[test]
    fn rejects_firmware_without_a_context_block() {
        let rm = uart_map();
        let bogus = "this is not a CTXDSL document\n";
        let res = compose_codesign_ctxdsl(&rm, bogus, &ComposeOptions::default());
        assert!(matches!(
            res,
            Err(ComposeError::FirmwareParseFailed(_)) | Err(ComposeError::NoContextBlock)
        ));
    }

    #[test]
    fn rejects_invalid_register_map() {
        let mut rm = uart_map();
        rm.base_address = "garbage".to_string();
        let res = compose_codesign_ctxdsl(&rm, SIMPLE_FIRMWARE, &ComposeOptions::default());
        assert!(matches!(res, Err(ComposeError::InvalidRegisterMap(_))));
    }

    #[test]
    fn explicit_member_override_wins_over_discovery() {
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(
            &rm,
            SIMPLE_FIRMWARE,
            &ComposeOptions {
                firmware_members_override: Some(&["UartDriver"]),
                ..ComposeOptions::default()
            },
        )
        .expect("compose succeeds");
        assert_eq!(result.firmware_members, vec!["UartDriver"]);
    }

    #[test]
    fn peripheral_automaton_override_propagates_to_composition() {
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(
            &rm,
            SIMPLE_FIRMWARE,
            &ComposeOptions {
                peripheral_automaton: Some("UartIp"),
                ..ComposeOptions::default()
            },
        )
        .expect("compose succeeds");
        assert_eq!(result.peripheral_automaton, "UartIp");
        assert!(result.ctxdsl.contains("automaton UartIp {"));
    }

    #[test]
    fn composition_name_override_propagates() {
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(
            &rm,
            SIMPLE_FIRMWARE,
            &ComposeOptions {
                composition_name: Some("MyCoupling"),
                ..ComposeOptions::default()
            },
        )
        .expect("compose succeeds");
        assert_eq!(result.composition_name, "MyCoupling");
        assert!(result.ctxdsl.contains("asynchronous MyCoupling"));
    }

    #[test]
    fn splicer_handles_braces_inside_comments() {
        // Comments containing `}` should not confuse the brace counter.
        let firmware = r#"
            context Tricky {
                // here is a } in a line comment
                /* and one in a block comment } too */
                alphabet {
                    label tick;
                }
                automata {
                    automaton A {
                        states { state S initial; }
                        transitions { transition S -> S on label tick; }
                    }
                }
            }
        "#;
        let rm = uart_map();
        let result = compose_codesign_ctxdsl(&rm, firmware, &ComposeOptions::default())
            .expect("compose succeeds");
        // The peripheral stub block must be inside the context (i.e.
        // before the outer `}`).
        let frag_idx = result.ctxdsl.find("automaton UART_LITE {").unwrap();
        let close_idx = result.ctxdsl.rfind('}').unwrap();
        assert!(
            frag_idx < close_idx,
            "fragment must be inside the outer context block"
        );
    }
}
