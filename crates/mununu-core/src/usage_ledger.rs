//! Track H3 — the opt-in **local** usage ledger: one NDJSON record per verification run, no
//! design content, written to a file the deployment names and nobody else reads.
//!
//! **Why a file and not a server.** The licence ([`crate::licence`]) phones nowhere, and neither
//! does this. A renewal or a volume conversation stands on evidence the customer holds: how many
//! runs, how many properties, which build, which licence. The ledger is that evidence — the
//! customer can mail it, feed their own dashboard, or delete it. It is also the raw material of
//! the cost estimator (roadmap J2) on the customer's own hardware.
//!
//! **What a record carries, and what it never carries.** Timestamps, the verb and the surface,
//! the build identity and version, the licence state / tier / team, the run's result and its
//! counts (properties, holds, violated, unknown, skipped, unsupported) and its wall. **No design
//! content**: no file name, no path, no module or property name, no formula, no counterexample,
//! no note text. The record type is a closed set of fields and a test pins the key set, so a
//! field that could carry any of those cannot be added without failing the build.
//!
//! **Opt-in, and never a gate.** The ledger is off unless `[licensing] ledger = "…"` in
//! `mununu.toml` or [`ENV_VAR`] names a file. A write that fails is reported where the operator
//! reads (a stderr warning on the CLI, a `warn` in the server log) and the run is unaffected —
//! a ledger must not be able to fail a verification.
//!
//! > Source of truth: [`record`] + [`UsageRecord::for_run`] — surface: CLI+API (`sv verify-auto`
//! > on both writes one record; `mununu licence status` / `GET /api/v1/licence` show the ledger's
//! > path as `ledger`)

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::adapter::slang::verify_auto::{AutoVerifyReport, VerifyOutcome};

/// Names the ledger file when `[licensing] ledger` in `mununu.toml` does not.
pub const ENV_VAR: &str = "MUNUNU_USAGE_LEDGER";

/// Which surface ran the verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Cli,
    Api,
}

/// How the run ended, as far as the ledger cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    /// A report was produced (whatever its verdicts).
    Report,
    /// The run failed before a report (a lift error, a refused option); the counts are zero.
    Error,
}

/// One ledger record. **Every field is a count, a label from a closed vocabulary, a timestamp or
/// an identity of the binary or the licence** — see the module note and the key-set test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub ts: String,
    /// The verb, e.g. `sv verify-auto`.
    pub verb: String,
    pub surface: Surface,
    /// `mununu-core`'s version.
    pub version: String,
    /// The build identity ([`crate::build_info::build_tag`]); absent when the binary has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// The licence state at the time of the run (`valid`, `evaluation`, …).
    pub licence_state: String,
    /// The licence tier, when a file's claims parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_tier: Option<String>,
    /// The licensed team's id, when a file's claims parsed — the invoice key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_team: Option<String>,
    pub result: RunResult,
    pub properties: usize,
    pub holds: usize,
    pub violated: usize,
    pub unknown: usize,
    pub skipped: usize,
    pub unsupported: usize,
    /// The run's wall, measured at the surface (lift included).
    pub wall_ms: u64,
}

/// The exact key set a record serialises to — the "no design content" falsifier as a constant: a
/// new field must be added here too, and reviewed against the module note.
pub const RECORD_KEYS: &[&str] = &[
    "ts",
    "verb",
    "surface",
    "version",
    "build",
    "licence_state",
    "licence_tier",
    "licence_team",
    "result",
    "properties",
    "holds",
    "violated",
    "unknown",
    "skipped",
    "unsupported",
    "wall_ms",
];

impl UsageRecord {
    /// The record of one run: `report` is `None` when the run failed before a report. The
    /// licence identity is read now ([`crate::licence::status`]), the build from
    /// [`crate::build_info::current`], the time from the host clock.
    pub fn for_run(
        verb: &str,
        surface: Surface,
        report: Option<&AutoVerifyReport>,
        wall_ms: u64,
    ) -> Self {
        let licence = crate::licence::status();
        Self::build_record(
            verb,
            surface,
            report,
            wall_ms,
            now_utc(),
            crate::build_info::current().map(crate::build_info::build_tag),
            wire_label(&licence.state),
            // mununu#938 — signed claims only: an `invalid` file's team and tier are whatever
            // its author wrote, and the ledger is the usage evidence a licence audit reads.
            licence.verified_claims().map(|c| wire_label(&c.tier)),
            licence.verified_claims().map(|c| c.team_id.clone()),
        )
    }

    /// [`UsageRecord::for_run`] with every environmental input passed in (the testable seam).
    #[allow(clippy::too_many_arguments)]
    pub fn build_record(
        verb: &str,
        surface: Surface,
        report: Option<&AutoVerifyReport>,
        wall_ms: u64,
        ts: String,
        build: Option<String>,
        licence_state: String,
        licence_tier: Option<String>,
        licence_team: Option<String>,
    ) -> Self {
        let count = |pred: fn(&VerifyOutcome) -> bool| -> usize {
            report.map_or(0, |r| {
                r.properties.iter().filter(|p| pred(&p.outcome)).count()
            })
        };
        UsageRecord {
            ts,
            verb: verb.to_string(),
            surface,
            version: env!("CARGO_PKG_VERSION").to_string(),
            build,
            licence_state,
            licence_tier,
            licence_team,
            result: if report.is_some() {
                RunResult::Report
            } else {
                RunResult::Error
            },
            properties: report.map_or(0, |r| r.properties.len()),
            holds: count(|o| matches!(o, VerifyOutcome::Holds)),
            violated: count(|o| matches!(o, VerifyOutcome::Violated { .. })),
            unknown: count(|o| matches!(o, VerifyOutcome::Unknown { .. })),
            skipped: count(|o| matches!(o, VerifyOutcome::Skipped { .. })),
            unsupported: report.map_or(0, |r| r.unsupported.len()),
            wall_ms,
        }
    }

    /// The record as one NDJSON line (no raw newline: every value is a count, a label or an
    /// identity, and `serde_json` escapes the strings).
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            // A struct of counts and strings cannot fail to serialise; keep the signature honest.
            format!("{{\"error\":\"unserialisable usage record: {e}\"}}")
        })
    }
}

/// What happened to a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerWrite {
    /// No ledger is configured: nothing written, nothing to report.
    Off,
    /// Appended to this file.
    Written(PathBuf),
    /// The ledger is configured but the record could not be written; the run is unaffected and
    /// the caller surfaces this where the operator reads.
    Failed { path: PathBuf, error: String },
}

/// Where the ledger is, if anywhere: `[licensing] ledger` in the deployment configuration, then
/// [`ENV_VAR`]. An unreadable deployment configuration is an error (the licence's `locate`
/// reports the same one as the licence's reason).
pub fn locate() -> Result<Option<PathBuf>, String> {
    if let Some(p) = crate::deployment::current()?.usage_ledger {
        return Ok(Some(p));
    }
    Ok(crate::adapter::env_var::read(ENV_VAR)
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from))
}

/// Append one record to the configured ledger (no-op when none is configured).
pub fn record(rec: &UsageRecord) -> LedgerWrite {
    match locate() {
        Ok(None) => LedgerWrite::Off,
        Ok(Some(path)) => match append(&path, rec) {
            Ok(()) => LedgerWrite::Written(path),
            Err(error) => LedgerWrite::Failed { path, error },
        },
        Err(error) => LedgerWrite::Failed {
            path: PathBuf::from("(deployment configuration)"),
            error,
        },
    }
}

/// Append one record to `path`, creating the file (never its directory: a missing directory is
/// the operator's to create, and is reported).
pub fn append(path: &Path, rec: &UsageRecord) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    // mununu#945 — one `write_all` of the whole line on an append handle: `writeln!` may issue
    // several writes, and two runs finishing together (the API server) could interleave them.
    let mut line = rec.line();
    line.push('\n');
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// An enum's wire spelling (`valid`, `team`, …): the same string every other surface prints,
/// taken from its serde form so the ledger and the status surfaces cannot drift apart.
fn wire_label<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The host clock as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let rem = secs % 86_400;
    let (y, m, d) = crate::licence::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::slang::translate::SvaKind;
    use crate::adapter::slang::verify_auto::PropertyVerdict;

    fn row(name: &str, formula: &str, outcome: VerifyOutcome) -> PropertyVerdict {
        PropertyVerdict {
            name: name.into(),
            label: Some(format!("{name}_label")),
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome,
            seeded_predicates: vec!["seed_pred_text".into()],
            counterexample: None,
            bottom_reason: None,
            decided_by: Some("exact-symbolic".into()),
            vacuity: None,
            portfolio: None,
        }
    }

    /// The names a design would leak if any field carried design content.
    const SECRET_NAME: &str = "secret_module_sva_7";
    const SECRET_FORMULA: &str = "AG (hidden_reg == 0x5A)";
    const SECRET_PATH: &str = "/home/someone/rtl/secret_module.sv";

    fn report() -> AutoVerifyReport {
        let mut r = AutoVerifyReport {
            properties: vec![
                row(SECRET_NAME, SECRET_FORMULA, VerifyOutcome::Holds),
                row("p1", "AG q", VerifyOutcome::Violated { false_cells: 2 }),
                row("p2", "AG r", VerifyOutcome::Unknown { unknown_cells: 9 }),
                row(
                    "p3",
                    "AG s",
                    VerifyOutcome::Skipped {
                        reason: SECRET_PATH.into(),
                    },
                ),
            ],
            ..Default::default()
        };
        r.unsupported
            .push((SECRET_NAME.to_string(), SECRET_PATH.to_string()));
        r
    }

    fn sample() -> UsageRecord {
        UsageRecord::build_record(
            "sv verify-auto",
            Surface::Cli,
            Some(&report()),
            4321,
            format_utc(1_794_096_000),
            Some("e7a5b21c0ffee0000000000000000000000000000".into()),
            "valid".into(),
            Some("team".into()),
            Some("acme-eng".into()),
        )
    }

    /// The falsifier as a test: a record built from a report full of design names carries the
    /// COUNTS and none of the names, formulas, paths, reasons or predicates.
    #[test]
    fn a_record_carries_counts_and_identity_and_no_design_content() {
        let rec = sample();
        assert_eq!(
            (
                rec.properties,
                rec.holds,
                rec.violated,
                rec.unknown,
                rec.skipped,
                rec.unsupported
            ),
            (4, 1, 1, 1, 1, 1)
        );
        assert_eq!(rec.result, RunResult::Report);
        assert_eq!(rec.ts, "2026-11-08T00:00:00Z");
        let line = rec.line();
        for secret in [
            SECRET_NAME,
            SECRET_FORMULA,
            SECRET_PATH,
            "p1",
            "_label",
            "seed_pred_text",
            "exact-symbolic",
            "hidden_reg",
        ] {
            assert!(
                !line.contains(secret),
                "the ledger line carries design content `{secret}`: {line}"
            );
        }
        assert!(!line.contains('\n'), "one record is one line");
    }

    /// The key set is closed: a field that could carry design content cannot appear without
    /// changing this list, which is reviewed against the module note.
    #[test]
    fn the_key_set_is_exactly_the_allowlist() {
        let v: serde_json::Value = serde_json::from_str(&sample().line()).unwrap();
        // serde_json's map is sorted, not in field order: compare as sets.
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = RECORD_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        // An error run: no report, zero counts, the optional identities absent → still a subset.
        let err = UsageRecord::build_record(
            "sv verify-auto",
            Surface::Api,
            None,
            17,
            format_utc(0),
            None,
            "evaluation".into(),
            None,
            None,
        );
        assert_eq!(err.result, RunResult::Error);
        assert_eq!((err.properties, err.holds, err.unsupported), (0, 0, 0));
        let v: serde_json::Value = serde_json::from_str(&err.line()).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(RECORD_KEYS.contains(&k.as_str()), "unexpected key {k}");
        }
        assert!(v.get("build").is_none() && v.get("licence_team").is_none());
    }

    #[test]
    fn utc_timestamps_are_iso_8601_seconds() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_utc(1_794_096_000), "2026-11-08T00:00:00Z");
        assert_eq!(format_utc(1_794_096_000 + 3_661), "2026-11-08T01:01:01Z");
        let now = now_utc();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z') && now.starts_with("20"), "{now}");
    }

    /// The invoice falsifier: `grep -c` over the file reproduces the run count, and every line
    /// is one valid record.
    #[test]
    fn append_writes_one_line_per_record_and_grep_count_is_the_run_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.ndjson");
        for _ in 0..3 {
            append(&path, &sample()).expect("append");
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("\"verb\":\"sv verify-auto\""))
                .count(),
            3,
            "grep -c over the ledger is the run count"
        );
        for l in lines {
            let back: UsageRecord = serde_json::from_str(l).expect("every line round-trips");
            assert_eq!(back, sample());
        }
    }

    /// A ledger named through the environment is found; an unwritable one is REPORTED (the run
    /// goes on); none named is `Off`. The unwritable path sits under a regular file, which no
    /// user — root in the dev image included — can create a file inside.
    #[test]
    fn a_ledger_named_by_the_environment_is_written_and_a_bad_path_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.ndjson");
        {
            let _env = crate::adapter::env_var::override_for_test(ENV_VAR, &path);
            assert_eq!(locate().unwrap(), Some(path.clone()));
            assert_eq!(record(&sample()), LedgerWrite::Written(path.clone()));
            assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        }
        let blocker = dir.path().join("a-regular-file");
        std::fs::write(&blocker, "x").unwrap();
        let bad = blocker.join("ledger.ndjson");
        {
            let _env = crate::adapter::env_var::override_for_test(ENV_VAR, &bad);
            match record(&sample()) {
                LedgerWrite::Failed { path, error } => {
                    assert_eq!(path, bad);
                    assert!(!error.is_empty());
                }
                other => panic!("an unwritable ledger must be reported, got {other:?}"),
            }
        }
        {
            let _env = crate::adapter::env_var::unset_for_test(ENV_VAR);
            assert_eq!(locate().unwrap(), None);
            assert_eq!(record(&sample()), LedgerWrite::Off);
        }
    }

    /// mununu#945 — appends from concurrent runs (the API server) never interleave: every line of
    /// the file is one whole record.
    #[test]
    fn concurrent_appends_leave_whole_lines() {
        let dir = std::env::temp_dir().join(format!("mununu-945-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("usage.ndjson");
        let rec = sample();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..200 {
                        append(&path, &rec).expect("append");
                    }
                });
            }
        });
        let text = std::fs::read_to_string(&path).expect("read back");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1600);
        for l in lines {
            assert_eq!(l, rec.line(), "an interleaved or torn record");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
