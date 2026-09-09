//! mununu#504 C7 — a SIGKILL-survivable breadcrumb of per-property verdicts.
//!
//! **Why this exists.** C6's memory ceiling degrades gracefully when mununu can observe itself.
//! It cannot help when the process is killed from OUTSIDE — the kernel OOM killer, a CI step
//! timeout, `docker stop`. `SIGKILL` runs no Rust code: no destructor, no panic handler, no final
//! report. A lane that verified 24 of 25 properties reports nothing at all, and the next run
//! starts from zero.
//!
//! **What it provides.** When `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` names a path, `sv verify-auto`
//! appends one JSON object per line as each property completes, flushing after every write. The
//! file is therefore correct-as-of-the-last-flush at every instant, including the instant a
//! `SIGKILL` arrives, so a consumer can recover the work that finished.
//!
//! **Read the LAST line per property name.** A verdict can change after the main loop: the ⊥
//! re-plan / escalation pass runs afterwards and may turn an `unknown` into a definite verdict.
//! Lines carry a `phase` (`"main"` then `"escalated"`) and a property may appear twice. Taking the
//! last occurrence gives the verdict the final report would have carried.
//!
//! **This is a diagnostic, never a gate.** Every failure here — an unwritable path, a full disk, a
//! serialization problem — is swallowed after one warning. A breadcrumb must not be able to fail a
//! verification run; that would trade a recoverable crash for an unconditional one.
//!
//! **One path, one run.** The file is truncated on open, so a process that calls `verify_auto`
//! more than once — the API server across requests, `sv mutate`'s baseline-then-mutant pair —
//! reuses the same path and the LAST opener wins; earlier runs' records are gone. The breadcrumb
//! is scoped to a single verification run, and a caller that needs more should point the variable
//! at a distinct path per run.
//!
//! **Not a substitute for the report.** It carries verdicts, not counterexamples, notes, or
//! seeded predicates. A run that completes normally still emits the full report, and consumers
//! should prefer it. The breadcrumb is for the run that does not get to finish.

use std::io::Write;

/// Path for the incremental NDJSON breadcrumb. Unset ⇒ no breadcrumb (the default).
pub const PARTIAL_JSON_ENV: &str = "MUNUNU_VERIFY_AUTO_PARTIAL_JSON";

/// Build one NDJSON line.
///
/// Uses `serde_json` rather than `format!` so a property name containing a quote, a backslash, or
/// a newline cannot break the framing — an SVA label is source-derived text, and hand-built JSON
/// would let it forge or split a record. The result is guaranteed to contain no raw newline, so
/// one record is always exactly one line.
pub fn breadcrumb_line(
    index: usize,
    name: &str,
    outcome: &str,
    phase: &str,
    elapsed_ms: Option<u64>,
) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("index".into(), serde_json::Value::from(index));
    obj.insert("property".into(), serde_json::Value::from(name));
    obj.insert("outcome".into(), serde_json::Value::from(outcome));
    obj.insert("phase".into(), serde_json::Value::from(phase));
    if let Some(ms) = elapsed_ms {
        obj.insert("elapsed_ms".into(), serde_json::Value::from(ms));
    }
    serde_json::Value::Object(obj).to_string()
}

/// The breadcrumb writer. `Disabled` when the env var is unset or the path could not be opened.
#[derive(Debug, Default)]
pub struct Breadcrumb {
    file: Option<std::fs::File>,
    /// How many properties have already been written, so [`Breadcrumb::record_pending`] can be
    /// called unconditionally and write only what is new.
    recorded: usize,
}

impl Breadcrumb {
    /// Open the breadcrumb named by [`PARTIAL_JSON_ENV`], truncating any previous content so a
    /// re-run cannot be misread as a continuation of an older one. Disabled (with one warning)
    /// when the path cannot be opened.
    pub fn from_env() -> Self {
        let Ok(path) = std::env::var(PARTIAL_JSON_ENV) else {
            return Self::default();
        };
        if path.trim().is_empty() {
            return Self::default();
        }
        match std::fs::File::create(&path) {
            Ok(file) => Self {
                file: Some(file),
                recorded: 0,
            },
            Err(e) => {
                tracing::warn!(
                    "{PARTIAL_JSON_ENV}={path}: could not open the partial-verdict breadcrumb \
                     ({e}); continuing without it"
                );
                Self::default()
            }
        }
    }

    /// True when a breadcrumb is actually being written.
    pub fn is_active(&self) -> bool {
        self.file.is_some()
    }

    /// Append one record and flush it. The flush is the entire point: an unflushed line is not on
    /// disk when a `SIGKILL` lands, which is precisely the case this exists for.
    ///
    /// Write errors are swallowed — see the module note on why a diagnostic must never fail a run.
    pub fn record(
        &mut self,
        index: usize,
        name: &str,
        outcome: &str,
        phase: &str,
        elapsed_ms: Option<u64>,
    ) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let line = breadcrumb_line(index, name, outcome, phase, elapsed_ms);
        if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
            tracing::warn!("{PARTIAL_JSON_ENV}: write failed ({e}); continuing without it");
            self.file = None; // stop retrying on every subsequent property
        }
    }
}

impl Breadcrumb {
    /// Write a record for every property beyond those already written, then advance the cursor.
    ///
    /// Called once at the TOP of each property iteration rather than at each of the nine
    /// `report.properties.push` sites in the loop — several of which sit behind a `continue`, so
    /// per-site wiring would be nine chances to miss one, and a missed site is an invisible gap in
    /// exactly the artifact a crash makes you depend on. The next iteration flushes the previous
    /// one's verdict, and the caller flushes once more after the loop.
    ///
    /// The residual window is from a property's push to the start of the next iteration — loop
    /// bookkeeping only, no verification work — so a kill during a property's own (long) execution
    /// still finds every earlier verdict on disk.
    pub fn record_pending<'a, I>(&mut self, all: I, phase: &str)
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        if self.file.is_none() {
            return;
        }
        let mut seen = 0usize;
        for (i, (name, outcome)) in all.into_iter().enumerate() {
            seen = i + 1;
            if i < self.recorded {
                continue;
            }
            self.record(i, name, outcome, phase, None);
        }
        self.recorded = self.recorded.max(seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_exactly_one_line_of_valid_json() {
        let line = breadcrumb_line(3, "fifo_sva_2", "holds", "main", Some(1250));
        assert!(
            !line.contains('\n'),
            "a record must not embed a raw newline"
        );
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(v["index"], 3);
        assert_eq!(v["property"], "fifo_sva_2");
        assert_eq!(v["outcome"], "holds");
        assert_eq!(v["phase"], "main");
        assert_eq!(v["elapsed_ms"], 1250);
    }

    #[test]
    fn elapsed_is_omitted_rather_than_null_when_untimed() {
        let line = breadcrumb_line(0, "p", "unknown", "main", None);
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert!(
            v.get("elapsed_ms").is_none(),
            "an absent timing is an absent key, not a null a consumer must special-case"
        );
    }

    #[test]
    fn a_hostile_property_name_cannot_break_the_framing() {
        // SVA labels are source-derived text. Hand-built JSON would let this forge a record or
        // split one across lines; serde_json escapes it instead.
        let nasty = "evil\", \"outcome\": \"holds\", \"x\": \"\n{\"injected\": true}";
        let line = breadcrumb_line(1, nasty, "violated", "main", None);
        assert!(!line.contains('\n'), "the embedded newline must be escaped");
        let v: serde_json::Value = serde_json::from_str(&line).expect("still one valid object");
        assert_eq!(v["property"], nasty, "the name round-trips verbatim");
        assert_eq!(
            v["outcome"], "violated",
            "the injected `outcome` must NOT have overridden the real one"
        );
        assert!(v.get("injected").is_none(), "no forged key appeared");
    }

    #[test]
    fn an_unwritable_path_disables_rather_than_failing() {
        // SAFETY: scoped to this test; the var is not read concurrently by other tests in-module.
        unsafe {
            std::env::set_var(
                PARTIAL_JSON_ENV,
                "/nonexistent-dir-mununu-c7/breadcrumb.ndjson",
            );
        }
        let mut b = Breadcrumb::from_env();
        assert!(!b.is_active(), "an unopenable path disables the breadcrumb");
        b.record(0, "p", "holds", "main", None); // must not panic
        unsafe {
            std::env::remove_var(PARTIAL_JSON_ENV);
        }
    }

    #[test]
    fn record_pending_writes_only_what_is_new() {
        let dir = std::env::temp_dir().join(format!("mununu-c7-cursor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("b.ndjson");
        // SAFETY: scoped to this test.
        unsafe {
            std::env::set_var(PARTIAL_JSON_ENV, &path);
        }
        let mut b = Breadcrumb::from_env();

        let mut props: Vec<(&str, &str)> = vec![("p0", "holds")];
        b.record_pending(props.clone(), "main");
        b.record_pending(props.clone(), "main"); // idempotent — nothing new
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            1,
            "re-flushing an unchanged list must not duplicate records"
        );

        props.push(("p1", "unknown"));
        b.record_pending(props.clone(), "main");
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "only the new property was appended");
        let v1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v1["property"], "p1");
        assert_eq!(
            v1["index"], 1,
            "the index is the property's position, not a write counter"
        );

        unsafe {
            std::env::remove_var(PARTIAL_JSON_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_pending_is_a_no_op_when_disabled() {
        // SAFETY: scoped to this test.
        unsafe {
            std::env::remove_var(PARTIAL_JSON_ENV);
        }
        let mut b = Breadcrumb::from_env();
        assert!(!b.is_active());
        b.record_pending(vec![("p", "holds")], "main"); // must not panic
    }

    #[test]
    fn records_survive_as_flushed_lines() {
        let dir = std::env::temp_dir().join(format!("mununu-c7-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("breadcrumb.ndjson");
        // SAFETY: scoped to this test.
        unsafe {
            std::env::set_var(PARTIAL_JSON_ENV, &path);
        }
        let mut b = Breadcrumb::from_env();
        assert!(b.is_active());
        b.record(0, "p0", "holds", "main", Some(10));
        b.record(1, "p1", "unknown", "main", None);
        // Read WITHOUT dropping the writer — this is the SIGKILL simulation: no destructor has
        // run, so anything visible here is visible to a post-mortem reader too.
        let content = std::fs::read_to_string(&path).expect("readable mid-run");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "both records are on disk before any drop");
        let v0: serde_json::Value = serde_json::from_str(lines[0]).expect("line 0");
        let v1: serde_json::Value = serde_json::from_str(lines[1]).expect("line 1");
        assert_eq!(v0["property"], "p0");
        assert_eq!(v1["outcome"], "unknown");
        unsafe {
            std::env::remove_var(PARTIAL_JSON_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
