//! N0.1 (2026-11-03) — the per-row facts of a finished `sv verify-auto` run, for the executable
//! ledger (`measurements/ledger/`, `scripts/ledger_check.py`, `make ledger-check`).
//!
//! When `MUNUNU_VERIFY_AUTO_ROW_FACTS` names a path, [`write_from_env`] writes one NDJSON record
//! per property ONCE, after the report is final (every portfolio member merged, the ⊥ re-plan
//! done): `index`, `property`, `label`, `kind`, `formula`, `outcome`, `decided_by`,
//! `bottom_reason` and `cone_hash` — the identity of what decides the row
//! (`ModelDiagnostics::cone_hashes`), which is what lets the ledger re-run only the rows whose
//! cone changed. Seconds are not here: `MUNUNU_PROPERTY_TIMING` already prints them per row and
//! phase, and the ledger sums those lines.
//!
//! Unlike the breadcrumb (`partial_json`) this is not SIGKILL-survivable and does not try to be:
//! a run that dies leaves no facts file, and the ledger treats the design as not measured.
//!
//! **A diagnostic, never a gate** (CLAUDE.md §Failure Paths): a path that cannot be written is
//! one `warn`, and the run's report is unchanged.
use crate::adapter::slang::translate::SvaKind;
use crate::adapter::slang::verify_auto::{AutoVerifyReport, PropertyVerdict};

/// Path for the row-facts NDJSON. Unset ⇒ nothing is written (the default).
pub const ROW_FACTS_ENV: &str = "MUNUNU_VERIFY_AUTO_ROW_FACTS";

/// One record. `serde_json` rather than `format!`, so a label or formula with a quote or a
/// newline cannot split or forge a line.
pub fn facts_line(index: usize, p: &PropertyVerdict, cone_hash: Option<&str>) -> String {
    let kind = match p.kind {
        SvaKind::Assert => "assert",
        SvaKind::Assume => "assume",
        SvaKind::Cover => "cover",
    };
    let mut obj = serde_json::Map::new();
    obj.insert("index".into(), serde_json::Value::from(index));
    obj.insert("property".into(), serde_json::Value::from(p.name.as_str()));
    obj.insert("label".into(), serde_json::Value::from(p.label.as_deref()));
    obj.insert("kind".into(), serde_json::Value::from(kind));
    obj.insert(
        "formula".into(),
        serde_json::Value::from(p.formula.as_str()),
    );
    obj.insert("outcome".into(), serde_json::Value::from(p.outcome.label()));
    obj.insert(
        "decided_by".into(),
        serde_json::Value::from(p.decided_by.as_deref()),
    );
    obj.insert(
        "bottom_reason".into(),
        serde_json::Value::from(p.bottom_reason.as_ref().map(|r| r.tag())),
    );
    obj.insert("cone_hash".into(), serde_json::Value::from(cone_hash));
    serde_json::Value::Object(obj).to_string()
}

/// Write the report's rows to [`ROW_FACTS_ENV`]'s path, when set; a failure is one warning.
pub fn write_from_env(report: &AutoVerifyReport) {
    // The literal, not the const: `docs/env-vars.md`'s check greps for `env::var("MUNUNU_…")`.
    let Ok(path) = std::env::var("MUNUNU_VERIFY_AUTO_ROW_FACTS") else {
        return;
    };
    if path.trim().is_empty() {
        return;
    }
    if let Err(e) = write_to_path(&path, report) {
        tracing::warn!(
            "{ROW_FACTS_ENV}={path}: could not write the row facts ({e}); the report is unchanged"
        );
    }
}

/// The whole file at once: a reader never sees a partial one.
pub(crate) fn write_to_path(path: &str, report: &AutoVerifyReport) -> std::io::Result<()> {
    let mut out = String::new();
    for (i, p) in report.properties.iter().enumerate() {
        let hash = report
            .diagnostics
            .cone_hashes
            .iter()
            .find(|(name, _)| *name == p.name)
            .map(|(_, h)| h.as_str());
        out.push_str(&facts_line(i, p, hash));
        out.push('\n');
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::adapter::slang::verify_auto::{BottomReason, VerifyOutcome};

    fn row(name: &str, label: Option<&str>, outcome: VerifyOutcome) -> PropertyVerdict {
        PropertyVerdict {
            name: name.into(),
            label: label.map(String::from),
            kind: SvaKind::Assert,
            formula: "nu X. ((q != 3) && [] X)".into(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        }
    }

    /// Every field the ledger reads is on the line, a missing label / decider / hash is `null`,
    /// and a label with a quote and a newline stays one line.
    #[test]
    fn a_facts_line_carries_the_ledgers_fields_and_is_one_line() {
        let mut p = row(
            "top_sva_0",
            Some("a \"quoted\"\nlabel"),
            VerifyOutcome::Holds,
        );
        p.decided_by = Some("exact-symbolic".into());
        let line = facts_line(3, &p, Some("00ff00ff00ff00ff"));
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["index"], 3);
        assert_eq!(v["property"], "top_sva_0");
        assert_eq!(v["label"], "a \"quoted\"\nlabel");
        assert_eq!(v["kind"], "assert");
        assert_eq!(v["outcome"], "holds");
        assert_eq!(v["decided_by"], "exact-symbolic");
        assert_eq!(v["bottom_reason"], serde_json::Value::Null);
        assert_eq!(v["cone_hash"], "00ff00ff00ff00ff");
        let mut bottom = row(
            "top_sva_1",
            None,
            VerifyOutcome::Unknown { unknown_cells: 1 },
        );
        bottom.bottom_reason = Some(BottomReason::NotAttempted);
        let v: serde_json::Value = serde_json::from_str(&facts_line(0, &bottom, None)).unwrap();
        assert_eq!(v["label"], serde_json::Value::Null);
        assert_eq!(v["outcome"], "unknown");
        assert_eq!(v["bottom_reason"], "not-attempted");
        assert_eq!(v["cone_hash"], serde_json::Value::Null);
    }

    /// The file has one record per row in report order, each with its own cone hash (or none).
    #[test]
    fn the_file_has_one_record_per_row_with_its_cone_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facts.ndjson");
        let mut report = AutoVerifyReport::default();
        report
            .properties
            .push(row("p0", None, VerifyOutcome::Holds));
        report.properties.push(row(
            "p1",
            Some("l1"),
            VerifyOutcome::Violated { false_cells: 2 },
        ));
        report
            .diagnostics
            .cone_hashes
            .push(("p1".into(), "0123456789abcdef".into()));
        write_to_path(path.to_str().unwrap(), &report).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["property"], "p0");
        assert_eq!(lines[0]["cone_hash"], serde_json::Value::Null);
        assert_eq!(lines[1]["property"], "p1");
        assert_eq!(lines[1]["label"], "l1");
        assert_eq!(lines[1]["outcome"], "violated");
        assert_eq!(lines[1]["cone_hash"], "0123456789abcdef");
    }
}
