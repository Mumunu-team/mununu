//! Shared sidecar abstraction-widening (sidecar-audit F1 fix, 2026-06-17).
//!
//! The CLI `context eval` loader and the verify-framework `sv-yosys`
//! orchestrator both consume a `.mununu.json` sidecar, but before this
//! module they diverged: the loader applied the R-S5 / R-S4 / R-S3 / R-S7
//! abstraction-widening + R-Y2 init-policy extraction, while the verify
//! path (the R4W-3.5 `sidecar = "..."` wiring) consumed the **raw**
//! sidecar. For the same `.mununu.json` this yielded a *coarser*
//! abstraction on the verify route — it dropped the typedef
//! `UNMATCHED_<n>` widening and the per-signal `init_policy: anyconst`
//! that bug detection (e.g. the Caliptra CWE-1245 analysis) relies on, so
//! a bug-detecting verdict could silently become a spurious "safe".
//! (See `.claude/reviews/sidecar-auditor/audit-2026-06-17.md` F1.)
//!
//! Both entry points now call [`widen_sidecar`], so the abstraction is
//! identical regardless of which surface drives the lift.

use std::collections::HashMap;

use crate::adapter::systemverilog::annotation::SvAnnotation;
use crate::adapter::systemverilog::{case_literal_extract, typedef_extract};
use crate::adapter::yosys::InitPolicyOverrides;

/// Result of applying the sidecar abstraction-widening chain.
#[derive(Debug, Default, Clone)]
pub struct SidecarWidening {
    /// Re-serialised widened annotation JSON. `None` when no stage fired
    /// (the caller keeps the raw sidecar).
    pub widened_json: Option<String>,
    /// R-Y2 per-signal init-policy overrides for `YosysOptions`.
    pub init_policy_overrides: InitPolicyOverrides,
    /// Per-stage human-readable summary (for CLI feedback / `tracing`).
    /// Empty when nothing changed.
    pub summary: Vec<String>,
}

/// Apply the sidecar abstraction-widening chain to `sidecar_json`, using
/// `primary_sv` + `additional_sv` as the typedef / case-literal source:
///
/// - **R-S5** type-driven widening from SV typedef enums;
/// - **R-S4** equivalence-class seeding (before R-S3; mutually exclusive
///   per signal);
/// - **R-S3** case-literal discriminator seeding;
/// - **R-S7** property-syntactic discriminator seeding;
/// - **R-Y2** per-signal init-policy override extraction.
///
/// Returns the re-serialised widened JSON (when any value-widening stage
/// fired), the init-policy overrides, and a per-stage summary. Best-effort
/// and pure (no I/O, no logging): a sidecar that does not parse yields an
/// empty result, so the caller keeps the raw sidecar. Both the CLI loader
/// and the verify orchestrator call this so their abstraction is identical.
pub fn widen_sidecar(
    sidecar_json: &str,
    primary_sv: &str,
    additional_sv: &[(String, String)],
) -> SidecarWidening {
    let Ok(mut ann) = serde_json::from_str::<SvAnnotation>(sidecar_json) else {
        return SidecarWidening::default();
    };
    let mut summary = Vec::new();

    // R-S5 — typedef-driven widening from the primary + additional SV.
    let mut typedefs = HashMap::new();
    typedefs.extend(typedef_extract::extract_typedef_enums(primary_sv));
    for (_, content) in additional_sv {
        typedefs.extend(typedef_extract::extract_typedef_enums(content));
    }
    let widened = ann.apply_type_driven_widening(&typedefs);
    if !widened.is_empty() {
        summary.push(format!(
            "type-driven widening from SV typedefs: {}",
            widened
                .iter()
                .map(|(s, t, n)| format!("{s}:{t}({n} variants)"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Case literals (shared input for R-S4 + R-S3), deduped per signal.
    let mut case_literals: HashMap<String, Vec<u64>> = HashMap::new();
    for (_, value) in std::iter::once((String::new(), primary_sv.to_string()))
        .chain(additional_sv.iter().cloned())
    {
        for (sig, lits) in case_literal_extract::extract_case_literals(&value) {
            case_literals.entry(sig).or_default().extend(lits);
        }
    }
    for v in case_literals.values_mut() {
        v.sort_unstable();
        v.dedup();
    }

    // R-S4 — equivalence-class seeding (runs before R-S3).
    let class_seeded = ann.apply_equivalence_class_seeding(&case_literals);
    if !class_seeded.is_empty() {
        summary.push(format!(
            "equivalence-class seeding (+OTHER catch-all): {}",
            fmt_seeds(&class_seeded)
        ));
    }
    // R-S3 — case-literal discriminator seeding.
    let case_seeded = ann.apply_case_literal_seeding(&case_literals);
    if !case_seeded.is_empty() {
        summary.push(format!(
            "case-literal discriminators: {}",
            fmt_seeds(&case_seeded)
        ));
    }
    // R-S7 — property-syntactic discriminator seeding.
    let seeded = ann.apply_property_syntactic_seeding();
    if !seeded.is_empty() {
        summary.push(format!(
            "property-syntactic discriminators: {}",
            fmt_seeds(&seeded)
        ));
    }

    // Re-serialise only when a value-widening stage actually changed the
    // annotation; the init-policy overrides are returned separately
    // (they drive YosysOptions, not the sidecar JSON).
    let widened_json = if !widened.is_empty()
        || !class_seeded.is_empty()
        || !case_seeded.is_empty()
        || !seeded.is_empty()
    {
        serde_json::to_string_pretty(&ann).ok()
    } else {
        None
    };

    let init_policy_overrides = ann.init_policy_overrides();
    if !init_policy_overrides.is_empty() {
        summary.push(format!(
            "per-signal init-policy overrides: {}",
            init_policy_overrides
                .iter()
                .map(|(n, p)| format!("{n}={}", p.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    SidecarWidening {
        widened_json,
        init_policy_overrides,
        summary,
    }
}

fn fmt_seeds(seeds: &[(String, Vec<i64>)]) -> String {
    seeds
        .iter()
        .map(|(s, vs)| {
            format!(
                "{s}=[{}]",
                vs.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A sidecar whose signal declares `type_name` must auto-widen from
    // the SV typedef (R-S5): the widened JSON gains the enum variants +
    // the synthetic UNMATCHED_<n> entries for the unenumerated encodings.
    const SV_WITH_ENUM: &str = r#"
typedef enum logic [2:0] {
    A = 3'b000,
    B = 3'b001,
    C = 3'b100
} my_state_e;
module m(input logic clk); my_state_e st; endmodule
"#;

    const SIDECAR_TYPE_NAME: &str = r#"{
        "$schema": "mununu_sv_annotation_v1",
        "module": "m",
        "source": "m.sv",
        "signals": [
            { "name": "st", "abstraction": "discover", "type_name": "my_state_e" }
        ]
    }"#;

    #[test]
    fn type_name_sidecar_widens_from_typedef() {
        let w = widen_sidecar(SIDECAR_TYPE_NAME, SV_WITH_ENUM, &[]);
        let json = w
            .widened_json
            .expect("type-driven widening must produce a widened sidecar");
        // The widened sidecar carries the named variants + the
        // unenumerated-encoding discriminators (3-bit type, 3 used → 5
        // unmatched: 2,3,5,6,7 are not in {0,1,4}).
        assert!(
            json.contains("UNMATCHED"),
            "expected UNMATCHED_<n> widening; got: {json}"
        );
        assert!(
            w.summary
                .iter()
                .any(|s| s.starts_with("type-driven widening")),
            "summary must record the type-driven stage; got {:?}",
            w.summary
        );
    }

    /// mununu#1121 U2.7 — the per-stage summary the CLI prints speaks in the user's terms:
    /// each stage is named by what it did (the typedef it widened from), never by a roadmap
    /// tag (`R-S5:`), a tier, a phase or a Rust path.
    #[test]
    fn x1121_widening_summary_names_the_stage_without_a_roadmap_tag() {
        let w = widen_sidecar(SIDECAR_TYPE_NAME, SV_WITH_ENUM, &[]);
        assert!(
            !w.summary.is_empty(),
            "the typedef stage fires on this fixture"
        );
        for line in &w.summary {
            assert!(
                line.starts_with("type-driven widening from SV typedefs"),
                "the stage is named by what it did; got: {line}"
            );
            for tag in ["Tier", "Phase", "R-", "::"] {
                assert!(!line.contains(tag), "summary line carries `{tag}`: {line}");
            }
        }
    }

    #[test]
    fn unparseable_sidecar_yields_empty_widening() {
        let w = widen_sidecar("{ not valid json", SV_WITH_ENUM, &[]);
        assert!(w.widened_json.is_none());
        assert!(w.init_policy_overrides.is_empty());
        assert!(w.summary.is_empty());
    }

    #[test]
    fn sidecar_with_no_wideners_returns_none_json() {
        // A plain boolean-abstraction sidecar (no type_name, no case
        // literals, no init policy) leaves the JSON untouched.
        let sidecar = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "m",
            "source": "m.sv",
            "signals": [ { "name": "st", "abstraction": "boolean" } ]
        }"#;
        let w = widen_sidecar(sidecar, "module m(input logic clk); endmodule", &[]);
        assert!(
            w.widened_json.is_none(),
            "no widening stage fired → keep the raw sidecar; got {:?}",
            w.widened_json
        );
    }
}
