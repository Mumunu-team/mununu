//! N-source CTXDSL document assembler (A2.3 of the verify framework).
//!
//! Given N adapter outputs (already post-binding-rewrite from
//! [`crate::verify::binding`]), a composition spec, and a list of
//! resolved properties, produces a single parseable CTXDSL document
//! the orchestrator (A2.4) hands to `parse_context_doc` +
//! `realize_documents` for evaluation.
//!
//! ## Output shape
//!
//! The assembler emits two top-level contexts:
//!
//! ```text
//! context <project>Sources {
//!     <inner bodies of each source's emitted CTXDSL, concatenated>
//!     composition {
//!         <semantics> <composition_name> {
//!             members [<member_1>, <member_2>, ...];
//!         }
//!     }
//! }
//!
//! context <project>Props {
//!     mu_formulas {
//!         formula <name_1> { over <over_1>; body = <formula_1>; }
//!         ...
//!     }
//! }
//! ```
//!
//! The composition block goes inside the main context (alongside the
//! merged automata and alphabets) because CTXDSL composition members
//! must reference automata in the same context. Properties live in a
//! sidecar context per the `realize_documents(&main, &[sidecars])`
//! convention.
//!
//! ## Why string manipulation rather than AST round-tripping
//!
//! Each adapter emits a `context <name> { ... }` block; assembling N
//! of them into one document only requires extracting each block's
//! inner body (between the outer `{` and `}`) and concatenating. The
//! existing parser + realiser will then merge per-section content (the
//! CTXDSL grammar accepts multiple `alphabet { ... }`, `automata
//! { ... }`, etc. blocks within a single context). This mirrors the
//! pattern in
//! [`crate::codesign::compose::compose_codesign_ctxdsl`] which uses
//! the same brace-tracking splice approach for the codesign C+SV
//! case.
//!
//! Skipping ast round-tripping keeps the assembler:
//! - **Adapter-agnostic** — no need to understand each adapter's
//!   structured output shape beyond "it produces a `context { ... }`".
//! - **Preserving** — the original adapter text (comments, ordering,
//!   formatting hints) survives.
//! - **Predictable** — the brace-tracker is the same one
//!   `codesign::compose` already ships and proves at scale.

use std::collections::BTreeMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One source's emitted CTXDSL, tagged with its config-declared id.
///
/// The `ctxdsl` field is the **post-binding-rewrite** text — i.e. any
/// label renaming has already been applied (see
/// [`crate::verify::binding::apply_renamings_to_ctxdsl`]).
#[derive(Debug, Clone)]
pub struct SourceCtxdsl {
    /// Source id from `verify.toml`'s `[[sources]].id`.
    pub source_id: String,
    /// The CTXDSL document the adapter emitted for this source,
    /// optionally rewritten by the binding layer. Expected shape:
    /// `context <name> { ... }`.
    pub ctxdsl: String,
}

/// Composition spec — mirrors `verify::config::CompositionSection`
/// after defaults have been resolved.
#[derive(Debug, Clone)]
pub struct CompositionSpec {
    /// `"synchronous"`, `"asynchronous"`, or `"superset"`.
    pub semantics: String,
    /// Member source ids (each must match a `SourceCtxdsl.source_id`).
    /// The assembler resolves each id to the source's primary
    /// automaton name via [`AutomatonDiscovery`].
    pub members: Vec<String>,
    /// Composition name.
    pub name: String,
}

/// One property entry — mu-calculus formula already resolved (template
/// instantiation, if any, happens upstream).
#[derive(Debug, Clone)]
pub struct ResolvedProperty {
    /// Property name (unique within the document).
    pub name: String,
    /// Concrete mu-calculus formula text — no `${PARAM}` placeholders.
    pub formula: String,
    /// `over <name>` target — typically the composition's name.
    pub over: String,
}

/// Strategy for picking the automaton name a composition member's
/// `source_id` resolves to.
///
/// Most adapters emit a single automaton per source; in that case
/// [`AutomatonDiscovery::SourceId`] (use the source id verbatim) or
/// [`AutomatonDiscovery::FirstAutomaton`] (peek into the source's
/// CTXDSL and pick the first `automaton <name>` declaration) both
/// work. Multi-automaton adapters (XState parallel regions, the C
/// extractor with multiple entry-point functions) need
/// [`AutomatonDiscovery::Explicit`] to disambiguate.
#[derive(Debug, Clone)]
pub enum AutomatonDiscovery {
    /// Member id is also the automaton name. Simplest case.
    SourceId,
    /// Scan the source's CTXDSL for the first `automaton <name>`
    /// declaration and use that name.
    FirstAutomaton,
    /// Explicit `source_id → automaton_name` map. Wins over the other
    /// strategies for any source id present in the map.
    Explicit(BTreeMap<String, String>),
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors raised by [`assemble_unified_ctxdsl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembleError {
    /// A `SourceCtxdsl` was not a parseable `context { ... }` block —
    /// the opening keyword, brace, or matching close brace couldn't
    /// be located.
    NoContextBlock { source_id: String },
    /// A composition member referenced a source id with no
    /// corresponding `SourceCtxdsl`.
    UnknownMember { id: String },
    /// `AutomatonDiscovery::FirstAutomaton` was chosen for a source
    /// whose CTXDSL contains no `automaton <name> { ... }` declaration.
    NoAutomatonFound { source_id: String },
}

impl fmt::Display for AssembleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssembleError::NoContextBlock { source_id } => write!(
                f,
                "source `{source_id}` did not contain a parseable `context <name> {{ ... }}` block"
            ),
            AssembleError::UnknownMember { id } => write!(
                f,
                "[composition].members references unknown source id `{id}` (not present in the sources list)"
            ),
            AssembleError::NoAutomatonFound { source_id } => write!(
                f,
                "source `{source_id}` has no `automaton <name> {{ ... }}` declaration; cannot resolve composition member"
            ),
        }
    }
}

impl std::error::Error for AssembleError {}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Assemble N source CTXDSL documents + a composition + properties
/// into a single CTXDSL string ready for
/// `context_dsl::parse` → `realize_documents`.
///
/// `project_name` becomes the prefix of the wrapping context name
/// (`<project>Sources`) and the sidecar (`<project>Props`).
pub fn assemble_unified_ctxdsl(
    project_name: &str,
    sources: &[SourceCtxdsl],
    composition: &CompositionSpec,
    properties: &[ResolvedProperty],
    discovery: &AutomatonDiscovery,
) -> Result<String, AssembleError> {
    // Pull each source's inner body, plus the first-automaton hint
    // and the full list of automata-per-source for the `<src>.*`
    // wildcard syntax.
    // mununu#591 — names are the ones each source REALIZES to (a parameterised automaton's
    // instances, not its declaration), grouped by declaration: "the first automaton" of a
    // source whose first declaration is a template is every instance of that template.
    let mut bodies: Vec<&str> = Vec::with_capacity(sources.len());
    let mut first_automaton_by_source: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut all_automata_by_source: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for s in sources {
        let body =
            extract_context_body(&s.ctxdsl).ok_or_else(|| AssembleError::NoContextBlock {
                source_id: s.source_id.clone(),
            })?;
        let groups = realized_automaton_groups(&s.ctxdsl).unwrap_or_default();
        if let Some(first) = groups.first() {
            first_automaton_by_source.insert(s.source_id.as_str(), first.clone());
        }
        all_automata_by_source.insert(s.source_id.as_str(), groups.into_iter().flatten().collect());
        bodies.push(body);
    }

    // Resolve composition members → automaton names. Supports the
    // `<source_id>.*` wildcard form: a single member entry expands
    // to one composition member per automaton the source emits. Bare
    // member entries keep the legacy single-automaton behaviour.
    let mut resolved_members: Vec<String> = Vec::with_capacity(composition.members.len());
    for m in &composition.members {
        if let Some(src_id) = m.strip_suffix(".*") {
            if !sources.iter().any(|s| s.source_id == src_id) {
                return Err(AssembleError::UnknownMember {
                    id: src_id.to_string(),
                });
            }
            let names = all_automata_by_source
                .get(src_id)
                .cloned()
                .unwrap_or_default();
            if names.is_empty() {
                return Err(AssembleError::NoAutomatonFound {
                    source_id: src_id.to_string(),
                });
            }
            resolved_members.extend(names);
            continue;
        }
        // Source must exist.
        if !sources.iter().any(|s| s.source_id == *m) {
            return Err(AssembleError::UnknownMember { id: m.clone() });
        }
        let names: Vec<String> = match discovery {
            AutomatonDiscovery::SourceId => vec![m.clone()],
            AutomatonDiscovery::FirstAutomaton => first_automaton_by_source
                .get(m.as_str())
                .cloned()
                .ok_or_else(|| AssembleError::NoAutomatonFound {
                    source_id: m.clone(),
                })?,
            AutomatonDiscovery::Explicit(map) => match map.get(m) {
                Some(name) => vec![name.clone()],
                None => first_automaton_by_source
                    .get(m.as_str())
                    .cloned()
                    .unwrap_or_else(|| vec![m.clone()]),
            },
        };
        resolved_members.extend(names);
    }

    // ---- Emit the main context ----------------------------------
    let mut out = String::new();
    out.push_str(&format!("context {project_name}Sources {{\n"));
    for (idx, body) in bodies.iter().enumerate() {
        out.push_str(&format!(
            "    // ---- source `{src}` ----\n",
            src = sources[idx].source_id
        ));
        // Indent each line of body for readability; preserve as-is.
        for line in body.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("\n    composition {\n");
    out.push_str(&format!(
        "        {kind} {name} {{\n            members [{members}];\n        }}\n",
        kind = composition.semantics,
        name = composition.name,
        members = resolved_members.join(", "),
    ));
    out.push_str("    }\n");
    out.push_str("}\n");

    // ---- Emit the sidecar properties context --------------------
    if !properties.is_empty() {
        out.push_str(&format!("\ncontext {project_name}Props {{\n"));
        out.push_str("    mu_formulas {\n");
        for p in properties {
            out.push_str(&format!(
                "        formula {name} {{ over {over}; body = {body}; }}\n",
                name = p.name,
                over = p.over,
                body = p.formula,
            ));
        }
        out.push_str("    }\n");
        out.push_str("}\n");
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers — brace-tracking substring extraction
// ---------------------------------------------------------------------------

/// Find the inner body of a `context <name> { ... }` block.
///
/// Returns the substring between the outer braces (excluding the
/// braces themselves), or `None` if no parseable context block is
/// present. Skips braces inside `//` line comments and `/* ... */`
/// block comments.
pub fn extract_context_body(ctxdsl: &str) -> Option<&str> {
    let context_kw = ctxdsl.find("context")?;
    let after_kw = &ctxdsl[context_kw + "context".len()..];
    let rel_open = after_kw.find('{')?;
    let open_abs = context_kw + "context".len() + rel_open;
    let close_abs = matching_close_brace(ctxdsl, open_abs)?;
    Some(&ctxdsl[open_abs + 1..close_abs])
}

/// Given the byte offset of an opening `{`, return the byte offset of
/// the matching `}`. Skips braces inside `//` and `/* */` comments.
fn matching_close_brace(text: &str, open_at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if open_at >= bytes.len() || bytes[open_at] != b'{' {
        return None;
    }
    let mut i = open_at + 1;
    let mut depth: usize = 1;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2; // skip the closing `*/`
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Scan a context body for **every** `automaton <name> { ... }`
/// declaration and return the names in declaration order. Used by
/// the `<source_id>.*` wildcard composition-member syntax: a single
/// `members = ["crew.*"]` entry expands to one composition member per
/// automaton the source emits.
///
/// Matches the keyword `automaton` followed by whitespace + an
/// identifier. Does not look inside string literals or comments — the
/// CTXDSL grammar's lexical rules make this safe in practice; if a
/// false positive ever bites we'll tighten the scan.
/// mununu#591 — the automaton names a source REALIZES to, grouped by declaration: a plain
/// automaton is one name; a parameterised one (`parameters { param i in R; }`) is every instance
/// name its range expands to (`Worker_0`, `Worker_1`), which is what the composition must
/// reference — the declared `Worker` does not exist after expansion. Parses the source; a source
/// that does not parse here falls back to the textual scan of declared names
/// ([`all_automaton_names`]), one name per group, and realization reports whatever is wrong.
pub(crate) fn realized_automaton_groups(ctxdsl: &str) -> Option<Vec<Vec<String>>> {
    match crate::context_dsl::parse(ctxdsl) {
        Ok(doc) => Some(crate::context_dsl::realized_automaton_names(&doc)),
        Err(_) => extract_context_body(ctxdsl).map(|body| {
            all_automaton_names(body)
                .into_iter()
                .map(|n| vec![n.to_string()])
                .collect()
        }),
    }
}

pub(crate) fn all_automaton_names(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = body[cursor..].find("automaton") {
        let kw = cursor + rel;
        let prev_ok = kw == 0
            || body
                .as_bytes()
                .get(kw - 1)
                .is_some_and(|b| !(b.is_ascii_alphanumeric() || *b == b'_'));
        let after_kw = &body[kw + "automaton".len()..];
        cursor = kw + "automaton".len();
        if !prev_ok {
            continue;
        }
        // The next non-whitespace char must start an identifier.
        let trimmed = after_kw.trim_start();
        let name_end = trimmed
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(trimmed.len());
        if name_end == 0 {
            continue;
        }
        let name = &trimmed[..name_end];
        // De-dupe; also skip the `automata { ... }` block keyword
        // (handled by the prev_ok guard above — `automata` is a
        // longer match that contains `automaton` as a prefix only
        // when the source contains a literal substring; the body
        // never does in practice).
        if !out.contains(&name) {
            out.push(name);
        }
        cursor += name_end;
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, ctxdsl: &str) -> SourceCtxdsl {
        SourceCtxdsl {
            source_id: id.to_string(),
            ctxdsl: ctxdsl.to_string(),
        }
    }

    // --------------------------------------------------------------
    // Brace tracker
    // --------------------------------------------------------------

    #[test]
    fn extracts_simple_context_body() {
        let s = "context Foo { hello }";
        assert_eq!(extract_context_body(s).unwrap().trim(), "hello");
    }

    #[test]
    fn extracts_nested_braces_correctly() {
        let s = "context Foo { automata { automaton X { states {} } } }";
        let body = extract_context_body(s).unwrap();
        assert!(body.contains("automaton X"));
        assert!(body.contains("automaton X { states {} }"));
    }

    #[test]
    fn skips_braces_inside_line_comments() {
        let s = "context Foo {\n  // }\n  hello\n}";
        let body = extract_context_body(s).unwrap();
        assert!(body.contains("hello"));
    }

    #[test]
    fn skips_braces_inside_block_comments() {
        let s = "context Foo {\n  /* { } */\n  hello\n}";
        let body = extract_context_body(s).unwrap();
        assert!(body.contains("hello"));
    }

    #[test]
    fn unmatched_braces_return_none() {
        assert!(extract_context_body("context Foo { hello").is_none());
        // No `context` keyword at all.
        assert!(extract_context_body("foo bar baz").is_none());
    }

    #[test]
    fn first_automaton_name_finds_identifier_after_keyword() {
        let body = "automata { automaton Toaster { states {} } }";
        assert_eq!(
            all_automaton_names(body).into_iter().next(),
            Some("Toaster")
        );
    }

    #[test]
    fn all_automaton_names_returns_every_declaration_in_order() {
        let body = "
            automata {
                automaton Agent_Researcher { states {} }
                automaton Agent_Writer { states {} }
                automaton Supervisor { states {} }
            }
        ";
        assert_eq!(
            all_automaton_names(body),
            vec!["Agent_Researcher", "Agent_Writer", "Supervisor"]
        );
    }

    #[test]
    fn all_automaton_names_skips_keyword_prefix_collisions() {
        // The `automata { … }` block keyword is a longer-prefix match
        // — `automaton` is not the same as `automata`. Ensure the scan
        // doesn't catch the `a` in `automata` and start hunting for
        // an identifier inside the `{ … }` brace pair.
        let body = "automata { automaton X { states {} } }";
        assert_eq!(all_automaton_names(body), vec!["X"]);
    }

    #[test]
    fn wildcard_member_expands_to_every_source_automaton() {
        // Composing one source that emits three automata via the
        // `<src>.*` wildcard form.
        let s = source(
            "crew",
            "context Crew {
                automata {
                    automaton Agent_A { states { state s0 initial; } transitions {} }
                    automaton Agent_B { states { state s0 initial; } transitions {} }
                    automaton Supervisor { states { state s0 initial; } transitions {} }
                }
            }",
        );
        let comp = CompositionSpec {
            semantics: "asynchronous".to_string(),
            members: vec!["crew.*".to_string()],
            name: "CrewSystem".to_string(),
        };
        let out = assemble_unified_ctxdsl(
            "Demo",
            &[s],
            &comp,
            &[],
            &AutomatonDiscovery::FirstAutomaton,
        )
        .expect("wildcard assembly succeeded");
        // All three automata land in the composition members list.
        assert!(out.contains("members [Agent_A, Agent_B, Supervisor]"));
    }

    #[test]
    fn wildcard_member_with_unknown_source_errors() {
        let comp = CompositionSpec {
            semantics: "asynchronous".to_string(),
            members: vec!["ghost.*".to_string()],
            name: "X".to_string(),
        };
        let err =
            assemble_unified_ctxdsl("Demo", &[], &comp, &[], &AutomatonDiscovery::FirstAutomaton)
                .unwrap_err();
        assert!(matches!(err, AssembleError::UnknownMember { id } if id == "ghost"));
    }

    // --------------------------------------------------------------
    // Assembler
    // --------------------------------------------------------------

    #[test]
    fn assembles_two_sources_into_one_document() {
        let src_a = source(
            "a",
            "context SourceA {\n  alphabet { label go; }\n  automata { automaton A { states { state S0 initial; } transitions { transition S0 -> S0 on label go; } } }\n}",
        );
        let src_b = source(
            "b",
            "context SourceB {\n  alphabet { label go; }\n  automata { automaton B { states { state S0 initial; } transitions { transition S0 -> S0 on label go; } } }\n}",
        );
        let comp = CompositionSpec {
            semantics: "synchronous".to_string(),
            members: vec!["a".to_string(), "b".to_string()],
            name: "Pair".to_string(),
        };
        let out = assemble_unified_ctxdsl(
            "Demo",
            &[src_a, src_b],
            &comp,
            &[],
            &AutomatonDiscovery::FirstAutomaton,
        )
        .unwrap();
        // Single wrapping context with the right name.
        assert!(out.contains("context DemoSources {"));
        // Both automata appear.
        assert!(out.contains("automaton A {"));
        assert!(out.contains("automaton B {"));
        // Composition declaration.
        assert!(out.contains("synchronous Pair {"));
        assert!(out.contains("members [A, B];"));
        // No properties → no sidecar context.
        assert!(!out.contains("DemoProps"));
    }

    #[test]
    fn includes_properties_as_sidecar_context() {
        let src = source(
            "fw",
            "context Fw {\n  alphabet { label go; }\n  automata { automaton Firmware { states { state S0 initial; } transitions { transition S0 -> S0 on label go; } } }\n}",
        );
        let comp = CompositionSpec {
            semantics: "asynchronous".to_string(),
            members: vec!["fw".to_string()],
            name: "System".to_string(),
        };
        let props = vec![
            ResolvedProperty {
                name: "reach_init".to_string(),
                formula: "mu X. (true || <> X)".to_string(),
                over: "System".to_string(),
            },
            ResolvedProperty {
                name: "no_deadlock".to_string(),
                formula: "nu X. (<> true && [] X)".to_string(),
                over: "System".to_string(),
            },
        ];
        let out = assemble_unified_ctxdsl(
            "Demo",
            &[src],
            &comp,
            &props,
            &AutomatonDiscovery::FirstAutomaton,
        )
        .unwrap();
        assert!(out.contains("context DemoSources {"));
        assert!(out.contains("context DemoProps {"));
        assert!(out.contains("mu_formulas {"));
        assert!(out.contains("formula reach_init"));
        assert!(out.contains("formula no_deadlock"));
        assert!(out.contains("over System"));
    }

    #[test]
    fn resolves_member_via_source_id_strategy() {
        let src = source(
            "a",
            "context Whatever {\n  automata { automaton InternalName { states { state S0 initial; } } }\n}",
        );
        let comp = CompositionSpec {
            semantics: "synchronous".to_string(),
            members: vec!["a".to_string()],
            name: "Lone".to_string(),
        };
        let out =
            assemble_unified_ctxdsl("Demo", &[src], &comp, &[], &AutomatonDiscovery::SourceId)
                .unwrap();
        // SourceId strategy → composition member is the source id verbatim.
        assert!(out.contains("synchronous Lone {"));
        assert!(out.contains("members [a];"));
    }

    #[test]
    fn resolves_member_via_explicit_map() {
        let src = source(
            "fw",
            "context Fw {\n  automata { automaton FirmwareAutomaton { states { state S0 initial; } } }\n}",
        );
        let comp = CompositionSpec {
            semantics: "asynchronous".to_string(),
            members: vec!["fw".to_string()],
            name: "System".to_string(),
        };
        let mut explicit = BTreeMap::new();
        explicit.insert("fw".to_string(), "CustomAutomatonAlias".to_string());
        let out = assemble_unified_ctxdsl(
            "Demo",
            &[src],
            &comp,
            &[],
            &AutomatonDiscovery::Explicit(explicit),
        )
        .unwrap();
        assert!(out.contains("asynchronous System {"));
        assert!(out.contains("members [CustomAutomatonAlias];"));
    }

    #[test]
    fn unknown_member_is_an_error() {
        let src = source("a", "context A { automata { automaton A { states {} } } }");
        let comp = CompositionSpec {
            semantics: "synchronous".to_string(),
            members: vec!["ghost".to_string()],
            name: "Pair".to_string(),
        };
        let err = assemble_unified_ctxdsl(
            "Demo",
            &[src],
            &comp,
            &[],
            &AutomatonDiscovery::FirstAutomaton,
        )
        .unwrap_err();
        assert!(matches!(err, AssembleError::UnknownMember { id } if id == "ghost"));
    }

    #[test]
    fn source_without_context_block_is_an_error() {
        let src = SourceCtxdsl {
            source_id: "bogus".to_string(),
            ctxdsl: "this is not ctxdsl".to_string(),
        };
        let comp = CompositionSpec {
            semantics: "synchronous".to_string(),
            members: vec!["bogus".to_string()],
            name: "X".to_string(),
        };
        let err = assemble_unified_ctxdsl(
            "Demo",
            &[src],
            &comp,
            &[],
            &AutomatonDiscovery::FirstAutomaton,
        )
        .unwrap_err();
        assert!(matches!(err, AssembleError::NoContextBlock { source_id } if source_id == "bogus"));
    }

    #[test]
    fn output_round_trips_through_ctxdsl_parser() {
        // Smoke test: the assembled output is parseable by the real
        // context_dsl parser. Catches grammar drift between the
        // assembler's emitted form and what the realiser accepts.
        let src_a = source(
            "a",
            "context A {\n  alphabet { label go; }\n  automata { automaton A { states { state S0 initial; } transitions { transition S0 -> S0 on label go; } } }\n}",
        );
        let src_b = source(
            "b",
            "context B {\n  alphabet { label go; }\n  automata { automaton B { states { state S0 initial; } transitions { transition S0 -> S0 on label go; } } }\n}",
        );
        let comp = CompositionSpec {
            semantics: "synchronous".to_string(),
            members: vec!["a".to_string(), "b".to_string()],
            name: "Pair".to_string(),
        };
        let out = assemble_unified_ctxdsl(
            "Demo",
            &[src_a, src_b],
            &comp,
            &[ResolvedProperty {
                name: "p1".to_string(),
                formula: "true".to_string(),
                over: "Pair".to_string(),
            }],
            &AutomatonDiscovery::FirstAutomaton,
        )
        .unwrap();
        // Main context: parseable.
        let main_end = out.find("\ncontext DemoProps").unwrap_or(out.len());
        let main = &out[..main_end];
        crate::context_dsl::parse(main).expect("main context parses cleanly");
        // Sidecar: parseable.
        if main_end < out.len() {
            let sidecar = &out[main_end + 1..];
            crate::context_dsl::parse(sidecar).expect("sidecar context parses cleanly");
        }
    }
}
