//! Codesign-shorthand translator (A2.6 of the verify framework).
//!
//! Converts a [`crate::codesign::project_config::ProjectConfig`]
//! (`mununu.codesign.toml`, the concise codesign-specific schema)
//! into a general [`crate::verify::config::VerifyConfig`]
//! (`verify.toml`) so both surfaces drive the same orchestrator
//! (`crate::verify::verify_project`).
//!
//! The codesign schema is a strict subset of the verify schema:
//!
//! | codesign schema (`mununu.codesign.toml`) | verify schema (`verify.toml`)              |
//! |------------------------------------------|--------------------------------------------|
//! | `[project] name + peripheral`            | `[project] name`                           |
//! | `[register_map] svd|json + peripheral_name` | `[alphabet] strategy="register_map", register_map=…` |
//! | `[firmware] sources + include_paths + …` | `[[sources]] adapter="c-codesign", files, options{…}` |
//! | `[rtl] sources + top_module`             | `[[sources]] adapter="sv-yosys", files, options{top}` |
//! | implicit `<peripheral>System` composition | `[composition] semantics="asynchronous", members=["firmware","rtl"], name=…` |
//! | `[[properties]]`                         | `[[properties]]` (verbatim)                |
//!
//! ## Adapter dispatch dependency
//!
//! [`codesign_to_verify`] produces a [`VerifyConfig`] that names the
//! `"c-codesign"` and `"sv-yosys"` adapters, both supported by
//! [`crate::verify::orchestrator::dispatch_adapter`]. Under the
//! `register_map` alphabet strategy the orchestrator reconciles the
//! firmware rendezvous labels against the SV peripheral via
//! [`crate::verify::register_map_rewriter`]; the SV side emits
//! `<signal>_<value>` labels under the KMTS route exactly as the native
//! route did (see `adapter::btor2::bit_blast`), so the renaming
//! derivation is route-agnostic and the migration to `sv-yosys` needs
//! no change to the rewriter. The remaining gap is the CLI/manifest
//! wiring that feeds a `mununu.codesign.toml` through this translator
//! into `verify_project`.
//!
//! ## SVD vs JSON register-map sources
//!
//! The codesign schema accepts either `[register_map] svd = …` or
//! `[register_map] json = …`. The verify schema's
//! `[alphabet] register_map = …` field takes a JSON-sidecar path
//! only. For SVD inputs the translator records the **SVD path
//! verbatim** under a `register_map_svd_path` field in the assembled
//! verify config's annotation map; the orchestrator (A2.6b) is
//! responsible for routing the SVD through
//! `codesign::svd_import::import_svd` before consuming it. This
//! keeps the translator side-effect-free.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::codesign::project_config::ProjectConfig as CodesignProjectConfig;
use crate::verify::config::{
    AlphabetSection, CompositionSection, ProjectSection, PropertySection, SourceSection,
    VerifyConfig,
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Translate a `mununu.codesign.toml` into a `verify.toml`.
///
/// Mechanical mapping (see module docs for the table). Does not run
/// the codesign config's [`validate`](CodesignProjectConfig::validate)
/// — the caller should do that first to surface codesign-specific
/// issues with their original error messages. The translator's output
/// is itself validated by [`VerifyConfig::validate`].
///
/// [`validate`]: crate::codesign::project_config::ProjectConfig::validate
pub fn codesign_to_verify(codesign: &CodesignProjectConfig) -> VerifyConfig {
    let project = ProjectSection {
        name: codesign.project.name.clone(),
        description: codesign.project.description.clone(),
        // Codesign shorthand targets composed firmware+RTL properties, not raw btor2
        // safety-cube passes; leave the opt-in cube off.
        safety_cube: false,
    };

    // Sources: one for the firmware C, one for the RTL SV.
    let firmware_source = build_firmware_source(codesign);
    let rtl_source = build_rtl_source(codesign);
    let sources = vec![firmware_source, rtl_source];

    // Alphabet: always register_map strategy. The codesign flow's
    // entire point is that firmware accesses ↔ peripheral signals
    // align on the rendezvous-label alphabet derived from the
    // register-map sidecar.
    //
    // The verify schema's `register_map` field takes a JSON path. If
    // the codesign schema supplied `svd = …` instead, we point the
    // verify config at the SVD path and tag it via a placeholder
    // ".svd" extension so the orchestrator's loader can route it
    // through `svd_import` before consuming.
    let register_map_path = codesign
        .register_map
        .json
        .clone()
        .or_else(|| codesign.register_map.svd.clone());
    let alphabet = AlphabetSection {
        strategy: "register_map".to_string(),
        renamings: Vec::new(),
        register_map: register_map_path,
        // Codesign flows almost always want the firmware to exercise
        // a SUBSET of the register map's labels (firmware reads/writes
        // some fields, peripheral exposes all of them). Mirror the
        // existing `mununu codesign reconcile-labels
        // --allow-peripheral-superset` default for codesign-shorthand
        // configs.
        allow_peripheral_superset: true,
    };

    // Composition: asynchronous (Doc C §C.5 — bus arbitration is
    // non-deterministic; synchronous coupling is unsound for racy
    // access). Composition name defaults to `<peripheral>System`,
    // matching `codesign::compose::compose_codesign_ctxdsl` output
    // and the property `over` defaults in the codesign schema.
    let composition_name = format!("{}System", codesign.project.peripheral);
    let composition = CompositionSection {
        semantics: "asynchronous".to_string(),
        members: vec!["firmware".to_string(), "rtl".to_string()],
        name: Some(composition_name),
    };

    // Properties: verbatim translation of each codesign property.
    let properties: Vec<PropertySection> = codesign
        .properties
        .iter()
        .map(|p| PropertySection {
            name: p.name.clone(),
            template: p.template.clone(),
            formula: p.formula.clone(),
            args: p.args.clone(),
            over: p.over.clone(),
        })
        .collect();

    VerifyConfig {
        project,
        sources,
        alphabet,
        composition,
        properties,
        // R4W-3 — codesign shorthand does not tune clustered-COI; the
        // verify path uses the recommended 0.5 default.
        cluster_similarity_floor: None,

        counterexample_max_steps: None,
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn build_firmware_source(codesign: &CodesignProjectConfig) -> SourceSection {
    let mut options: BTreeMap<String, toml::Value> = BTreeMap::new();
    options.insert(
        "include_paths".to_string(),
        toml::Value::Array(
            codesign
                .firmware
                .include_paths
                .iter()
                .map(|p| toml::Value::String(p.display().to_string()))
                .collect(),
        ),
    );
    options.insert(
        "defines".to_string(),
        toml::Value::Array(
            codesign
                .firmware
                .defines
                .iter()
                .map(|d| toml::Value::String(d.clone()))
                .collect(),
        ),
    );
    options.insert(
        "cmsis_stubs".to_string(),
        toml::Value::Boolean(codesign.firmware.cmsis_stubs),
    );
    if let Some(prefix) = &codesign.firmware.cmsis_header_vendor_prefix {
        options.insert(
            "cmsis_header_vendor_prefix".to_string(),
            toml::Value::String(prefix.clone()),
        );
    }
    // The c-codesign adapter (when A2.6b lands) needs to know which
    // register-map to bind firmware accesses against. Mirror the
    // alphabet binding's path so the adapter doesn't have to read
    // the parent verify config.
    if let Some(rm_path) = codesign
        .register_map
        .json
        .as_ref()
        .or(codesign.register_map.svd.as_ref())
    {
        options.insert(
            "register_map".to_string(),
            toml::Value::String(rm_path.display().to_string()),
        );
    }
    options.insert(
        "synthesize_automaton".to_string(),
        toml::Value::Boolean(true),
    );

    SourceSection {
        id: "firmware".to_string(),
        adapter: "c-codesign".to_string(),
        files: codesign.firmware.sources.clone(),
        options,
        count: None,
        memory_abstraction: None,
    }
}

fn build_rtl_source(codesign: &CodesignProjectConfig) -> SourceSection {
    let mut options: BTreeMap<String, toml::Value> = BTreeMap::new();
    // The KMTS `sv-yosys` route reads `top` as the elaboration root
    // (consumed on the multi-module path; harmless on the single-module
    // path the codesign flow uses today — one peripheral, one top).
    // The codesign schema's `rtl.frontend` selector is no longer
    // emitted: the native `custom-sv` frontend is gone (S.2b
    // singular-pipeline commitment), so sv2v → Yosys → BTOR2 is the
    // only SV frontend. The vestigial schema field is retired
    // separately (S.2b-3, Tier D).
    options.insert(
        "top".to_string(),
        toml::Value::String(codesign.rtl.top_module.clone()),
    );

    SourceSection {
        id: "rtl".to_string(),
        adapter: "sv-yosys".to_string(),
        files: codesign.rtl.sources.clone(),
        options,
        count: None,
        memory_abstraction: None,
    }
}

/// Resolve a codesign config's register-map source path. Returns the
/// JSON path if set, otherwise the SVD path, otherwise `None`.
///
/// Exposed because the orchestrator (A2.6b) needs to know whether to
/// route the path through `svd_import` before parsing as a
/// [`crate::codesign::register_map::RegisterMap`].
pub fn codesign_register_map_path(codesign: &CodesignProjectConfig) -> Option<PathBuf> {
    codesign
        .register_map
        .json
        .clone()
        .or_else(|| codesign.register_map.svd.clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const CODESIGN_JSON_TOML: &str = r#"
[project]
name = "uart_codesign"
peripheral = "UART_LITE"
description = "UART firmware + RTL peripheral demo."

[register_map]
json = "register_map.json"

[firmware]
sources = ["firmware/main.c", "firmware/uart.c"]
include_paths = ["firmware/include"]
defines = ["F_CPU=64000000"]
cmsis_stubs = true
cmsis_header_vendor_prefix = "VENDOR_"

[rtl]
sources = ["rtl/uart.sv"]
top_module = "uart_lite"
frontend = "custom-sv"

[[properties]]
name = "no_deadlock"
template = "no_deadlock"
over = "UART_LITESystem"

[[properties]]
name = "init_reachable"
formula = "mu X. (Init || <> X)"
"#;

    const CODESIGN_SVD_TOML: &str = r#"
[project]
name = "nrf52_twim"
peripheral = "TWIM0"

[register_map]
svd = "upstream/nrf52840.svd"
peripheral_name = "TWIM0"

[firmware]
sources = ["fw.c"]

[rtl]
sources = ["rtl/twim.sv"]
top_module = "TWIM0"
"#;

    fn parse_codesign(toml_src: &str) -> CodesignProjectConfig {
        CodesignProjectConfig::from_toml(toml_src).expect("valid codesign TOML")
    }

    // -----------------------------------------------------------------------
    // Core translation
    // -----------------------------------------------------------------------

    #[test]
    fn translates_project_section_verbatim() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        assert_eq!(verify.project.name, "uart_codesign");
        assert_eq!(
            verify.project.description.as_deref(),
            Some("UART firmware + RTL peripheral demo.")
        );
        // Peripheral lives in the codesign side; verify schema has no
        // peripheral field — it surfaces via the composition name.
        assert_eq!(verify.composition_name(), "UART_LITESystem");
    }

    #[test]
    fn translates_register_map_json_into_alphabet_register_map_strategy() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        assert_eq!(verify.alphabet.strategy, "register_map");
        assert_eq!(
            verify
                .alphabet
                .register_map
                .as_deref()
                .and_then(|p| p.to_str()),
            Some("register_map.json")
        );
        // Codesign's default allows the peripheral to be a label
        // superset of the firmware (firmware exercises a subset).
        assert!(verify.alphabet.allow_peripheral_superset);
    }

    #[test]
    fn translates_register_map_svd_path_through_register_map_field() {
        // SVD path is recorded verbatim in the verify config's
        // register_map field; the orchestrator's loader (A2.6b) routes
        // it through svd_import based on the extension.
        let codesign = parse_codesign(CODESIGN_SVD_TOML);
        let verify = codesign_to_verify(&codesign);
        assert_eq!(verify.alphabet.strategy, "register_map");
        assert_eq!(
            verify
                .alphabet
                .register_map
                .as_deref()
                .and_then(|p| p.to_str()),
            Some("upstream/nrf52840.svd")
        );
    }

    #[test]
    fn emits_firmware_source_with_c_codesign_adapter_and_options() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        let fw = verify
            .sources
            .iter()
            .find(|s| s.id == "firmware")
            .expect("firmware source present");
        assert_eq!(fw.adapter, "c-codesign");
        assert_eq!(
            fw.files,
            vec![
                PathBuf::from("firmware/main.c"),
                PathBuf::from("firmware/uart.c")
            ]
        );
        // Options pass through include_paths, defines, cmsis_stubs,
        // vendor prefix, register_map, synthesize_automaton.
        assert_eq!(
            fw.options.get("cmsis_stubs"),
            Some(&toml::Value::Boolean(true))
        );
        assert_eq!(
            fw.options.get("synthesize_automaton"),
            Some(&toml::Value::Boolean(true))
        );
        assert_eq!(
            fw.options.get("cmsis_header_vendor_prefix"),
            Some(&toml::Value::String("VENDOR_".to_string()))
        );
        // include_paths is a TOML array of strings; check shape.
        match fw.options.get("include_paths") {
            Some(toml::Value::Array(arr)) => {
                assert_eq!(arr.len(), 1);
                assert_eq!(arr[0], toml::Value::String("firmware/include".to_string()));
            }
            other => panic!("expected include_paths array, got {other:?}"),
        }
        // Register-map path mirrors the [alphabet] field so the
        // c-codesign adapter doesn't need to look up its parent
        // verify config.
        assert_eq!(
            fw.options.get("register_map"),
            Some(&toml::Value::String("register_map.json".to_string()))
        );
    }

    #[test]
    fn emits_rtl_source_with_sv_yosys_adapter_and_options() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        let rtl = verify
            .sources
            .iter()
            .find(|s| s.id == "rtl")
            .expect("rtl source present");
        // S.2b: the codesign RTL source drives the KMTS `sv-yosys`
        // route — the sole surviving SV frontend.
        assert_eq!(rtl.adapter, "sv-yosys");
        assert_eq!(rtl.files, vec![PathBuf::from("rtl/uart.sv")]);
        // `top_module` from the codesign schema becomes the sv-yosys
        // `top` elaboration root.
        assert_eq!(
            rtl.options.get("top"),
            Some(&toml::Value::String("uart_lite".to_string()))
        );
        // The native `frontend` selector and the legacy `top_module`
        // option are no longer emitted: sv2v → Yosys → BTOR2 is the only
        // SV frontend now, and the option key is `top`.
        assert!(!rtl.options.contains_key("frontend"));
        assert!(!rtl.options.contains_key("top_module"));
    }

    #[test]
    fn composition_is_asynchronous_peripheral_system() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        assert_eq!(verify.composition.semantics, "asynchronous");
        assert_eq!(verify.composition.members, vec!["firmware", "rtl"]);
        assert_eq!(verify.composition.name.as_deref(), Some("UART_LITESystem"));
    }

    #[test]
    fn properties_translate_verbatim() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        assert_eq!(verify.properties.len(), 2);
        let no_dl = &verify.properties[0];
        assert_eq!(no_dl.name, "no_deadlock");
        assert_eq!(no_dl.template.as_deref(), Some("no_deadlock"));
        assert_eq!(no_dl.over.as_deref(), Some("UART_LITESystem"));
        let init = &verify.properties[1];
        assert_eq!(init.name, "init_reachable");
        assert_eq!(init.formula.as_deref(), Some("mu X. (Init || <> X)"));
        assert!(init.over.is_none()); // falls back to composition default
    }

    // -----------------------------------------------------------------------
    // Round-trip and validation
    // -----------------------------------------------------------------------

    #[test]
    fn translated_verify_config_passes_validation() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let verify = codesign_to_verify(&codesign);
        // The translator should produce a config that itself validates
        // cleanly — no follow-up massaging needed.
        let issues = verify.validate();
        assert!(
            issues.is_empty(),
            "expected no validation issues, got: {issues:?}"
        );
    }

    #[test]
    fn translated_svd_config_passes_validation() {
        let codesign = parse_codesign(CODESIGN_SVD_TOML);
        let verify = codesign_to_verify(&codesign);
        let issues = verify.validate();
        assert!(
            issues.is_empty(),
            "expected no validation issues, got: {issues:?}"
        );
    }

    #[test]
    fn codesign_register_map_path_prefers_json_over_svd() {
        let codesign = parse_codesign(CODESIGN_JSON_TOML);
        let path = codesign_register_map_path(&codesign).unwrap();
        assert_eq!(path.to_str(), Some("register_map.json"));

        let codesign_svd = parse_codesign(CODESIGN_SVD_TOML);
        let path = codesign_register_map_path(&codesign_svd).unwrap();
        assert_eq!(path.to_str(), Some("upstream/nrf52840.svd"));
    }
}
