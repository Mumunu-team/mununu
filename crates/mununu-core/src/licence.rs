//! Track H1 — the signed offline licence file (`mununu.lic`) and its status.
//!
//! The licence is a legal instrument plus an honest status line, not DRM: the file names the
//! licensee, the team, the tier and the term, and carries an Ed25519 signature the binary checks
//! against the issuer's compiled-in public key. Verification is a pure function over the file's
//! text, the host's date and the key set — no network, no clock server, no hardware fingerprint
//! (a team licence is "one team, unlimited users"; node-locking buys nothing and breaks CI
//! runners).
//!
//! **A licence never touches a verdict.** Nothing under `adapter/`, `mu_calculus/`, `planner/`,
//! `verify/` or the CTXDSL evaluator reads this module — `tests/licence_never_reaches_a_verdict_h1.rs`
//! keeps it so. The status rides the channel the build identity uses (mununu#715): `mununu
//! licence status`, `GET /api/v1/licence`, the `licence` field of `/api/v1/health` and of every
//! `sv verify-auto` report.
//!
//! # Policy (H1b, decided 2026-11-08)
//!
//! What the tool does in each state is [`decide`], applied once at start-up by the CLI and the
//! server and nowhere else — never on a verdict path:
//!
//! | state | behaviour |
//! |---|---|
//! | `valid` | runs |
//! | no file → `evaluation` | runs for [`EVALUATION_DAYS`] counted from the first run on this host (`~/.mununu/first-run`), one stderr line on every start |
//! | `grace` | runs, a loud stderr line |
//! | `expired` (a file past its grace days, or the evaluation term over) | `mununu` exits 1 with the renewal text; `mununu server` refuses to start |
//! | `invalid` (bad signature, tampered, unknown key) | treated as no file — the evaluation term governs — and says so |
//! | `missing` | no file *and* the first run could not be recorded (an unwritable home): runs, says so |
//!
//! `mununu licence status` always runs, so the reason is always readable.
//!
//! # File format
//!
//! ```json
//! {
//!   "mununu_licence": 1,
//!   "payload": "<base64 of the claims JSON>",
//!   "signature": "<base64 of the Ed25519 signature over the decoded payload bytes>"
//! }
//! ```
//!
//! The claims JSON: `{"licensee": "…", "team_id": "…", "tier": "evaluation" | "team",
//! "issued": "YYYY-MM-DD", "expires": "YYYY-MM-DD", "grace_days": N, "key_id": "…"}`. The
//! signature covers the payload's raw bytes, so any Ed25519 signer (RFC 8032; `openssl pkeyutl
//! -sign -rawin`) can issue a file; the issuer and its private key are private (Track H2). A
//! claim this version does not know is kept under the signature and ignored, so an older binary
//! still reads a newer file.
//!
//! # Location
//!
//! First match wins: `[licensing] file = "…"` in `mununu.toml` ([`crate::deployment`]), then
//! `MUNUNU_LICENSE_FILE`, then [`SYSTEM_PATH`]. No file is the `missing` state, reported as such.
//!
//! > Source of truth: [`status`] + [`evaluate`] — surface: (CLI+API)

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The licence file every deployment looks at last.
pub const SYSTEM_PATH: &str = "/etc/mununu/mununu.lic";

/// The environment variable that names a licence file when `mununu.toml` does not.
pub const ENV_VAR: &str = "MUNUNU_LICENSE_FILE";

/// The format version this binary reads.
pub const FORMAT_VERSION: u32 = 1;

/// The evaluation term without a licence file, in days from the first run on a host (business
/// plan §19, the 2026-10-23 proposal, adopted 2026-11-08 with H1b).
pub const EVALUATION_DAYS: i64 = 30;

/// The first-run stamp's directory under the home directory, and the file in it.
pub const STAMP_DIR: &str = ".mununu";
pub const STAMP_FILE: &str = "first-run";

/// An issuer's public key, named by the `key_id` a licence's claims carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssuerKey {
    /// The id the claims name (`key_id`).
    pub key_id: &'static str,
    /// The raw 32-byte Ed25519 public key.
    pub public_key: [u8; 32],
}

/// The issuer keys this binary trusts. Rotation = a new entry with a new id; a file signed by a
/// retired key is `invalid` with the id in its reason.
pub const ISSUER_KEYS: &[IssuerKey] = &[IssuerKey {
    key_id: "mununu-2026-11",
    public_key: [
        0xcd, 0x37, 0xde, 0x58, 0x30, 0xda, 0xba, 0x16, 0xac, 0xb4, 0xa6, 0x51, 0x53, 0x45, 0x84,
        0xe8, 0xca, 0x23, 0x89, 0x36, 0xf8, 0xb6, 0x84, 0x8b, 0x3e, 0x24, 0x1d, 0x03, 0xc9, 0xbb,
        0x4f, 0xdf,
    ],
}];

/// The licence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// A time-boxed evaluation, full features.
    Evaluation,
    /// One team, unlimited users and runs, CI included.
    Team,
}

/// What the issuer signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct LicenceClaims {
    /// The legal licensee.
    pub licensee: String,
    /// The team the licence is for (one licence, one team).
    pub team_id: String,
    /// The tier.
    pub tier: Tier,
    /// The issue date, `YYYY-MM-DD` (UTC). A date after today is reported as a tamper note.
    pub issued: String,
    /// The last day the licence is valid, `YYYY-MM-DD` (UTC), inclusive.
    pub expires: String,
    /// Days after `expires` during which the state is `grace` rather than `expired`.
    #[serde(default)]
    pub grace_days: u32,
    /// Which issuer key signed this file.
    pub key_id: String,
}

/// The file's envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenceFile {
    /// [`FORMAT_VERSION`].
    pub mununu_licence: u32,
    /// Base64 (standard alphabet, padded) of the claims JSON.
    pub payload: String,
    /// Base64 of the 64-byte Ed25519 signature over the decoded payload.
    pub signature: String,
}

/// The state a licence is in. Every state is reported; none changes what the engines compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum LicenceState {
    /// Signed by a trusted key and within its term.
    Valid,
    /// Past `expires`, within `grace_days`.
    Grace,
    /// Past `expires` and past its grace days.
    Expired,
    /// A file was found but is not a licence this binary accepts; `reason` says why. The
    /// evaluation term governs what runs (`evaluation` carries it).
    Invalid,
    /// No licence file; within the evaluation term counted from this host's first run.
    Evaluation,
    /// No licence file and the first run could not be recorded (the home directory is not
    /// writable), so no term can be counted; `reason` says so.
    Missing,
}

/// The evaluation term a host without a licence file is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct EvaluationTerm {
    /// The first run on this host, `YYYY-MM-DD` (the stamp's content).
    pub started: String,
    /// The last day of the term, inclusive.
    pub ends: String,
    /// Days until `ends`: `0` on the last day, negative after it.
    pub days_left: i64,
}

/// What the tool does in a licence state, decided once at start-up ([`decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run, say nothing.
    Run,
    /// Run, and print this line on stderr first.
    Warn(String),
    /// Do not run; print this and exit 1 (the server refuses to start).
    Refuse(String),
}

/// The full status, as `mununu licence status --json` and `GET /api/v1/licence` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct LicenceStatus {
    /// The state.
    pub state: LicenceState,
    /// The file the status describes; absent when `missing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The claims, when the payload parsed (present for `invalid` too when the file was readable
    /// as claims, so a wrong-key or expired file still says whose it is).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claims: Option<LicenceClaims>,
    /// Days until `expires`: `0` on the last valid day, negative after it. Present when the
    /// dates parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_left: Option<i64>,
    /// Why the state is `invalid` or `missing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A tamper note: `issued` is after today, so the file or the host clock is wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The evaluation term this host is on when no valid file governs (`evaluation`, `invalid`,
    /// and `expired` with no claims); absent when a valid or expired file governs or no term
    /// could be counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<EvaluationTerm>,
    /// Track H3 — the local usage ledger this deployment writes (`[licensing] ledger` or
    /// `MUNUNU_USAGE_LEDGER`), as a path; absent when the ledger is off. Filled by [`status`]
    /// only: the ledger is a deployment fact, not a property of the licence file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
}

/// The three-field summary `/api/v1/health` and every report carry next to `build`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "api", derive(schemars::JsonSchema))]
pub struct LicenceSummary {
    /// The state.
    pub state: LicenceState,
    /// The tier, when the claims parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// The expiry date, when the claims parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

fn no_file_reason() -> String {
    format!(
        "no licence file: none named by `[licensing] file` in mununu.toml, by {ENV_VAR}, or at {SYSTEM_PATH}"
    )
}

impl LicenceStatus {
    fn invalid(source: Option<String>, claims: Option<LicenceClaims>, reason: String) -> Self {
        LicenceStatus {
            state: LicenceState::Invalid,
            source,
            claims,
            days_left: None,
            reason: Some(reason),
            note: None,
            evaluation: None,
            ledger: None,
        }
    }

    /// The `missing` status: no file, and no evaluation term could be counted (`why` says what
    /// stopped the first-run stamp).
    pub fn missing(why: &str) -> Self {
        LicenceStatus {
            state: LicenceState::Missing,
            source: None,
            claims: None,
            days_left: None,
            reason: Some(format!(
                "{}; the first run could not be recorded: {why}",
                no_file_reason()
            )),
            note: None,
            evaluation: None,
            ledger: None,
        }
    }

    /// No file: the evaluation term counted from `first_run` governs — `evaluation` within it,
    /// `expired` (with no claims) after it.
    pub fn on_evaluation(first_run: Day, today: Day) -> Self {
        let term = evaluation_term(first_run, today);
        let within = term.days_left >= 0;
        LicenceStatus {
            state: if within {
                LicenceState::Evaluation
            } else {
                LicenceState::Expired
            },
            source: None,
            claims: None,
            days_left: Some(term.days_left),
            reason: Some(if within {
                no_file_reason()
            } else {
                format!(
                    "the {EVALUATION_DAYS}-day evaluation that started on {} ended on {}; {}",
                    term.started,
                    term.ends,
                    no_file_reason()
                )
            }),
            note: None,
            evaluation: Some(term),
            ledger: None,
        }
    }

    /// The summary for `/health` and the reports.
    pub fn summary(&self) -> LicenceSummary {
        LicenceSummary {
            state: self.state,
            tier: self
                .claims
                .as_ref()
                .map(|c| c.tier)
                .or((self.state == LicenceState::Evaluation).then_some(Tier::Evaluation)),
            expires: self
                .claims
                .as_ref()
                .map(|c| c.expires.clone())
                .or_else(|| self.evaluation.as_ref().map(|e| e.ends.clone())),
        }
    }
}

/// The evaluation term counted from a host's first run.
pub fn evaluation_term(first_run: Day, today: Day) -> EvaluationTerm {
    let ends = Day(first_run.0 + EVALUATION_DAYS - 1);
    EvaluationTerm {
        started: first_run.to_string(),
        ends: ends.to_string(),
        days_left: ends.0 - today.0,
    }
}

/// What the tool does in this state. Pure: the CLI and the server call it once at start-up.
pub fn decide(status: &LicenceStatus) -> Decision {
    let renew = "a licence file makes mununu run again — see docs/licensing.md; `mununu licence status` shows what was found";
    match status.state {
        LicenceState::Valid => Decision::Run,
        LicenceState::Grace => {
            let c = status.claims.as_ref();
            Decision::Warn(format!(
                "the licence{} expired on {}; {} grace day(s) left, then mununu stops — renew now (docs/licensing.md)",
                c.map(|c| format!(" for {}", c.licensee))
                    .unwrap_or_default(),
                c.map(|c| c.expires.as_str()).unwrap_or("?"),
                c.map(|c| i64::from(c.grace_days) + status.days_left.unwrap_or(0))
                    .unwrap_or(0)
                    .max(0),
            ))
        }
        LicenceState::Expired => match (&status.claims, &status.evaluation) {
            (Some(c), _) => Decision::Refuse(format!(
                "the licence for {} expired on {} and its {} grace days have passed; {renew}",
                c.licensee, c.expires, c.grace_days
            )),
            (None, Some(t)) => Decision::Refuse(format!(
                "the {EVALUATION_DAYS}-day evaluation on this host ended on {}; {renew}",
                t.ends
            )),
            (None, None) => Decision::Refuse(format!("the licence has expired; {renew}")),
        },
        LicenceState::Evaluation => {
            let left = status.evaluation.as_ref().map(|t| t.days_left).unwrap_or(0);
            Decision::Warn(format!(
                "evaluation: {left} day(s) left of {EVALUATION_DAYS} without a licence file (docs/licensing.md)"
            ))
        }
        LicenceState::Invalid => match &status.evaluation {
            Some(t) if t.days_left >= 0 => Decision::Warn(format!(
                "the licence file is not accepted ({}); running as an evaluation, {} day(s) left (docs/licensing.md)",
                status.reason.as_deref().unwrap_or("invalid"),
                t.days_left
            )),
            Some(t) => Decision::Refuse(format!(
                "the licence file is not accepted ({}) and the {EVALUATION_DAYS}-day evaluation on this host ended on {}; {renew}",
                status.reason.as_deref().unwrap_or("invalid"),
                t.ends
            )),
            None => Decision::Warn(format!(
                "the licence file is not accepted ({}); no evaluation term could be counted (docs/licensing.md)",
                status.reason.as_deref().unwrap_or("invalid")
            )),
        },
        LicenceState::Missing => Decision::Warn(format!(
            "no licence file and no evaluation term: {} (docs/licensing.md)",
            status
                .reason
                .as_deref()
                .unwrap_or("the first run could not be recorded")
        )),
    }
}

impl std::fmt::Display for Day {
    /// `YYYY-MM-DD` (the inverse of [`Day::parse`]).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, m, d) = civil_from_days(self.0);
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

/// Civil date of days since 1970-01-01 (Howard Hinnant's algorithm, the inverse of
/// `days_from_civil`).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The first-run stamp: read `dir/first-run`, or write today's date into it on the first call.
/// A stamp that will not parse is rewritten (and said so in the error-free path by being today).
pub fn first_run_day(dir: &Path, today: Day) -> Result<Day, String> {
    let path = dir.join(STAMP_FILE);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(day) = Day::parse(text.trim())
    {
        return Ok(day);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(&path, format!("{today}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(today)
}

/// The stamp directory: `$HOME/.mununu` (`%USERPROFILE%\.mununu` on Windows).
pub fn stamp_dir() -> Option<PathBuf> {
    crate::adapter::env_var::read("HOME")
        .or_else(|| crate::adapter::env_var::read("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(STAMP_DIR))
}

/// A calendar day as days since 1970-01-01 (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day(pub i64);

impl Day {
    /// Today, from the host clock (UTC).
    pub fn today() -> Day {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Day(secs.div_euclid(86_400))
    }

    /// Parse a strict `YYYY-MM-DD`.
    pub fn parse(s: &str) -> Result<Day, String> {
        let bad = || format!("`{s}` is not a YYYY-MM-DD date");
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return Err(bad());
        }
        let num = |range: std::ops::Range<usize>| -> Result<i64, String> {
            let part = &s[range];
            if !part.bytes().all(|c| c.is_ascii_digit()) {
                return Err(bad());
            }
            part.parse::<i64>().map_err(|_| bad())
        };
        let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
        if !(1..=12).contains(&m) || d < 1 || d > i64::from(days_in_month(y, m as u32)) {
            return Err(bad());
        }
        Ok(Day(days_from_civil(y, m as u32, d as u32)))
    }
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
    }
}

/// Days since 1970-01-01 of a proleptic-Gregorian civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Evaluate a licence file's text against `keys` on `today`. Pure: the same text, day and keys
/// give the same status on every host.
pub fn evaluate(
    text: &str,
    today: Day,
    keys: &[IssuerKey],
    source: Option<String>,
) -> LicenceStatus {
    let file: LicenceFile = match serde_json::from_str(text) {
        Ok(f) => f,
        Err(e) => return LicenceStatus::invalid(source, None, format!("not a licence file: {e}")),
    };
    if file.mununu_licence != FORMAT_VERSION {
        return LicenceStatus::invalid(
            source,
            None,
            format!(
                "licence format version {} (this binary reads version {FORMAT_VERSION})",
                file.mununu_licence
            ),
        );
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let payload = match b64.decode(file.payload.trim()) {
        Ok(p) => p,
        Err(e) => {
            return LicenceStatus::invalid(source, None, format!("payload is not base64: {e}"));
        }
    };
    let claims: LicenceClaims = match serde_json::from_slice(&payload) {
        Ok(c) => c,
        Err(e) => return LicenceStatus::invalid(source, None, format!("claims do not parse: {e}")),
    };
    let signature = match b64.decode(file.signature.trim()) {
        Ok(s) => s,
        Err(e) => {
            return LicenceStatus::invalid(
                source,
                Some(claims),
                format!("signature is not base64: {e}"),
            );
        }
    };
    let Ok(signature) = Signature::from_slice(&signature) else {
        return LicenceStatus::invalid(
            source,
            Some(claims),
            format!("signature is {} bytes, not 64", signature.len()),
        );
    };
    let Some(key) = keys.iter().find(|k| k.key_id == claims.key_id) else {
        return LicenceStatus::invalid(
            source,
            Some(claims.clone()),
            format!("unknown issuer key id `{}`", claims.key_id),
        );
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&key.public_key) else {
        return LicenceStatus::invalid(
            source,
            Some(claims.clone()),
            format!(
                "issuer key `{}` is not a valid Ed25519 public key",
                key.key_id
            ),
        );
    };
    if verifying.verify_strict(&payload, &signature).is_err() {
        return LicenceStatus::invalid(
            source,
            Some(claims),
            "signature does not match the claims".to_string(),
        );
    }
    let (issued, expires) = match (Day::parse(&claims.issued), Day::parse(&claims.expires)) {
        (Ok(i), Ok(e)) => (i, e),
        (Err(e), _) | (_, Err(e)) => {
            return LicenceStatus::invalid(source, Some(claims), format!("claims: {e}"));
        }
    };
    let days_left = expires.0 - today.0;
    let state = if days_left >= 0 {
        LicenceState::Valid
    } else if -days_left <= i64::from(claims.grace_days) {
        LicenceState::Grace
    } else {
        LicenceState::Expired
    };
    let note = (issued > today).then(|| {
        format!(
            "issued {} is after today: the file or this host's clock is wrong",
            claims.issued
        )
    });
    LicenceStatus {
        state,
        source,
        claims: Some(claims),
        days_left: Some(days_left),
        reason: None,
        note,
        evaluation: None,
        ledger: None,
    }
}

/// Where the licence file is, if anywhere: `[licensing] file` in the deployment configuration,
/// then [`ENV_VAR`], then [`SYSTEM_PATH`] when it exists. An unreadable deployment configuration
/// is an error, never a silent `missing`.
pub fn locate() -> Result<Option<PathBuf>, String> {
    if let Some(p) = crate::deployment::current()?.licence_file {
        return Ok(Some(p));
    }
    if let Some(p) = crate::adapter::env_var::read(ENV_VAR).filter(|s| !s.is_empty()) {
        return Ok(Some(PathBuf::from(p)));
    }
    let system = Path::new(SYSTEM_PATH);
    if system.is_file() {
        return Ok(Some(system.to_path_buf()));
    }
    Ok(None)
}

/// The status of this deployment's licence today, against the compiled-in issuer keys, with
/// the evaluation term from this host's first-run stamp where no valid file governs.
pub fn status() -> LicenceStatus {
    let mut st = status_at(Day::today(), stamp_dir().as_deref());
    // H3 — where the usage ledger goes, if anywhere. A deployment-configuration error is already
    // this status's `reason` (the licence's `locate` reads the same file), so `Err` adds nothing.
    st.ledger = crate::usage_ledger::locate()
        .ok()
        .flatten()
        .map(|p| p.display().to_string());
    st
}

/// [`status`] on a given day with a given stamp directory (`None` = no home directory).
pub fn status_at(today: Day, stamp: Option<&Path>) -> LicenceStatus {
    let term = |st: LicenceStatus| -> LicenceStatus {
        // A file that is not accepted is treated as no file: the evaluation term governs.
        match stamp.map(|d| first_run_day(d, today)) {
            Some(Ok(first)) => LicenceStatus {
                evaluation: Some(evaluation_term(first, today)),
                ..st
            },
            _ => st,
        }
    };
    let path = match locate() {
        Ok(Some(p)) => p,
        Ok(None) => {
            return match stamp.map(|d| first_run_day(d, today)) {
                Some(Ok(first)) => LicenceStatus::on_evaluation(first, today),
                Some(Err(why)) => LicenceStatus::missing(&why),
                None => LicenceStatus::missing("no home directory (HOME / USERPROFILE unset)"),
            };
        }
        Err(e) => return term(LicenceStatus::invalid(None, None, e)),
    };
    let source = Some(path.display().to_string());
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let st = evaluate(&text, today, ISSUER_KEYS, source);
            if st.state == LicenceState::Invalid {
                term(st)
            } else {
                st
            }
        }
        Err(e) => term(LicenceStatus::invalid(
            source,
            None,
            format!("cannot read the licence file: {e}"),
        )),
    }
}

/// Write the envelope for a signed payload (the issuer's half of the format; the private key is
/// what makes a licence, not this function).
pub fn envelope(payload: &[u8], signature: &[u8; 64]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let file = LicenceFile {
        mununu_licence: FORMAT_VERSION,
        payload: b64.encode(payload),
        signature: b64.encode(signature),
    };
    serde_json::to_string_pretty(&file).unwrap_or_default() + "\n"
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A test issuer: a fixed seed, so every test signs with the same key and the key set is
    //! injected through `evaluate`'s `keys` — the compiled-in key never signs in a test.
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub const TEST_KEY_ID: &str = "test-issuer";
    const SEED: [u8; 32] = [7; 32];

    pub fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&SEED)
    }

    pub fn keys() -> Vec<IssuerKey> {
        vec![IssuerKey {
            key_id: TEST_KEY_ID,
            public_key: signing_key().verifying_key().to_bytes(),
        }]
    }

    pub fn claims(issued: &str, expires: &str, grace_days: u32) -> LicenceClaims {
        LicenceClaims {
            licensee: "Example Ltd".to_string(),
            team_id: "example-hw".to_string(),
            tier: Tier::Team,
            issued: issued.to_string(),
            expires: expires.to_string(),
            grace_days,
            key_id: TEST_KEY_ID.to_string(),
        }
    }

    /// Sign `payload` with the test key and wrap it.
    pub fn issue_bytes(payload: &[u8]) -> String {
        let sig = signing_key().sign(payload);
        envelope(payload, &sig.to_bytes())
    }

    pub fn issue(claims: &LicenceClaims) -> String {
        issue_bytes(&serde_json::to_vec(claims).expect("claims serialise"))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const TODAY: Day = Day(20_765); // 2026-11-08

    #[test]
    fn a_day_parses_and_counts_from_the_epoch() {
        assert_eq!(Day::parse("1970-01-01").unwrap(), Day(0));
        assert_eq!(Day::parse("2000-03-01").unwrap(), Day(11_017));
        assert_eq!(Day::parse("2026-11-08").unwrap(), TODAY);
        assert_eq!(Day::parse("2024-02-29").unwrap(), Day(19_782));
        for bad in [
            "2026-11-8",
            "2026-13-01",
            "2023-02-29",
            "20261108",
            "2026-11-08T00:00",
        ] {
            assert!(Day::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_signed_file_within_its_term_is_valid() {
        let text = issue(&claims("2026-11-01", "2027-11-01", 14));
        let s = evaluate(&text, TODAY, &keys(), Some("x.lic".into()));
        assert_eq!(s.state, LicenceState::Valid, "{s:?}");
        assert_eq!(s.days_left, Some(358));
        assert_eq!(s.claims.as_ref().unwrap().licensee, "Example Ltd");
        assert_eq!(s.reason, None);
        assert_eq!(s.note, None);
        let summary = s.summary();
        assert_eq!(summary.tier, Some(Tier::Team));
        assert_eq!(summary.expires.as_deref(), Some("2027-11-01"));
    }

    #[test]
    fn the_last_day_is_valid_then_grace_then_expired() {
        let text = issue(&claims("2026-01-01", "2026-11-08", 3));
        assert_eq!(
            evaluate(&text, TODAY, &keys(), None).state,
            LicenceState::Valid
        );
        let s = evaluate(&text, Day(TODAY.0 + 1), &keys(), None);
        assert_eq!(s.state, LicenceState::Grace);
        assert_eq!(s.days_left, Some(-1));
        assert_eq!(
            evaluate(&text, Day(TODAY.0 + 3), &keys(), None).state,
            LicenceState::Grace
        );
        let s = evaluate(&text, Day(TODAY.0 + 4), &keys(), None);
        assert_eq!(s.state, LicenceState::Expired);
        assert_eq!(s.days_left, Some(-4));
        // No grace days: expired the day after.
        let text = issue(&claims("2026-01-01", "2026-11-08", 0));
        assert_eq!(
            evaluate(&text, Day(TODAY.0 + 1), &keys(), None).state,
            LicenceState::Expired
        );
    }

    #[test]
    fn a_future_issue_date_is_valid_with_a_tamper_note() {
        let text = issue(&claims("2026-12-01", "2027-12-01", 0));
        let s = evaluate(&text, TODAY, &keys(), None);
        assert_eq!(s.state, LicenceState::Valid);
        assert!(s.note.unwrap().contains("after today"));
    }

    #[test]
    fn every_way_a_file_can_be_wrong_is_invalid_with_its_reason() {
        let good = claims("2026-11-01", "2027-11-01", 14);
        let cases: Vec<(&str, String, &str)> = vec![
            ("not json", "nonsense".to_string(), "not a licence file"),
            (
                "wrong format version",
                issue(&good).replace("\"mununu_licence\": 1", "\"mununu_licence\": 2"),
                "format version 2",
            ),
            (
                "payload not base64",
                serde_json::to_string(&LicenceFile {
                    mununu_licence: 1,
                    payload: "@@".into(),
                    signature: "AA==".into(),
                })
                .unwrap(),
                "payload is not base64",
            ),
            (
                "claims do not parse",
                issue_bytes(b"{\"licensee\": 1}"),
                "claims do not parse",
            ),
            (
                "unknown key id",
                issue(&LicenceClaims {
                    key_id: "retired-2020".into(),
                    ..good.clone()
                }),
                "unknown issuer key id `retired-2020`",
            ),
            (
                "bad date",
                issue(&LicenceClaims {
                    expires: "2027-02-30".into(),
                    ..good.clone()
                }),
                "not a YYYY-MM-DD date",
            ),
        ];
        for (name, text, reason) in cases {
            let s = evaluate(&text, TODAY, &keys(), None);
            assert_eq!(s.state, LicenceState::Invalid, "{name}: {s:?}");
            assert!(
                s.reason.as_deref().unwrap().contains(reason),
                "{name}: {s:?}"
            );
        }
    }

    #[test]
    fn an_edited_claim_breaks_the_signature_and_the_claims_still_say_whose_it_was() {
        // Re-encode the payload with a later expiry; the signature was over the original.
        let text = issue(&claims("2026-11-01", "2026-12-01", 0));
        let file: LicenceFile = serde_json::from_str(&text).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let edited = String::from_utf8(b64.decode(&file.payload).unwrap())
            .unwrap()
            .replace("2026-12-01", "2099-12-01");
        let tampered = serde_json::to_string(&LicenceFile {
            payload: b64.encode(edited.as_bytes()),
            ..file.clone()
        })
        .unwrap();
        let s = evaluate(&tampered, TODAY, &keys(), None);
        assert_eq!(s.state, LicenceState::Invalid);
        assert_eq!(
            s.reason.as_deref(),
            Some("signature does not match the claims")
        );
        assert_eq!(s.claims.unwrap().expires, "2099-12-01");
        // A signature from another key, same claims: also invalid.
        let other = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let payload = serde_json::to_vec(&claims("2026-11-01", "2026-12-01", 0)).unwrap();
        let sig = ed25519_dalek::Signer::sign(&other, &payload);
        let forged = envelope(&payload, &sig.to_bytes());
        assert_eq!(
            evaluate(&forged, TODAY, &keys(), None).state,
            LicenceState::Invalid
        );
    }

    #[test]
    fn a_claim_this_version_does_not_know_is_kept_under_the_signature() {
        let payload = b"{\"licensee\":\"Example Ltd\",\"team_id\":\"t\",\"tier\":\"evaluation\",\"issued\":\"2026-11-01\",\"expires\":\"2026-12-01\",\"key_id\":\"test-issuer\",\"seats\":12}";
        let s = evaluate(&issue_bytes(payload), TODAY, &keys(), None);
        assert_eq!(s.state, LicenceState::Valid, "{s:?}");
        assert_eq!(s.claims.unwrap().grace_days, 0);
    }

    /// RFC 8032 §7.1 test 1: the verifier accepts a signature any standard Ed25519 tool produces,
    /// so the private issuer (openssl `pkeyutl -sign -rawin`) and this binary agree by
    /// construction.
    #[test]
    fn the_verifier_accepts_the_rfc_8032_test_vector() {
        fn hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }
        let public = hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let signature = hex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );
        let key = VerifyingKey::from_bytes(&public.try_into().unwrap()).unwrap();
        let sig = Signature::from_slice(&signature).unwrap();
        assert!(key.verify_strict(b"", &sig).is_ok());
        // And the compiled-in key is a well-formed Ed25519 point.
        for k in ISSUER_KEYS {
            assert!(
                VerifyingKey::from_bytes(&k.public_key).is_ok(),
                "{}",
                k.key_id
            );
        }
    }

    #[test]
    fn a_day_prints_as_it_parses() {
        for s in [
            "1970-01-01",
            "2000-02-29",
            "2026-11-08",
            "2099-12-31",
            "1969-12-31",
        ] {
            assert_eq!(Day::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(Day(TODAY.0 + 29).to_string(), "2026-12-07");
    }

    #[test]
    fn the_first_run_stamp_is_written_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(STAMP_DIR);
        assert_eq!(first_run_day(&stamp, TODAY).unwrap(), TODAY);
        assert_eq!(
            std::fs::read_to_string(stamp.join(STAMP_FILE))
                .unwrap()
                .trim(),
            "2026-11-08"
        );
        // A later day reads the first one back, never today.
        assert_eq!(first_run_day(&stamp, Day(TODAY.0 + 40)).unwrap(), TODAY);
        // A stamp that does not parse is rewritten as today.
        std::fs::write(stamp.join(STAMP_FILE), "garbage").unwrap();
        assert_eq!(
            first_run_day(&stamp, Day(TODAY.0 + 2)).unwrap(),
            Day(TODAY.0 + 2)
        );
        // An unwritable location is an error, never a silent day 0. A directory under a regular
        // FILE cannot be created by anyone, root included (the dev image runs its tests as root,
        // where "/nonexistent-root" is perfectly creatable — the first CI run of this test).
        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let err = first_run_day(&file.join(STAMP_DIR), TODAY).unwrap_err();
        assert!(err.starts_with("cannot create"), "{err}");
    }

    #[test]
    fn no_file_is_an_evaluation_until_the_term_ends_then_expired() {
        let s = LicenceStatus::on_evaluation(TODAY, TODAY);
        assert_eq!(s.state, LicenceState::Evaluation);
        let t = s.evaluation.clone().unwrap();
        assert_eq!(
            (t.started.as_str(), t.ends.as_str(), t.days_left),
            ("2026-11-08", "2026-12-07", 29)
        );
        assert_eq!(s.days_left, Some(29));
        assert!(s.reason.as_deref().unwrap().starts_with("no licence file"));
        assert_eq!(
            s.summary(),
            LicenceSummary {
                state: LicenceState::Evaluation,
                tier: Some(Tier::Evaluation),
                expires: Some("2026-12-07".into())
            }
        );
        // The last day of the term is still the evaluation; the day after is expired.
        assert_eq!(
            LicenceStatus::on_evaluation(TODAY, Day(TODAY.0 + 29)).state,
            LicenceState::Evaluation
        );
        let gone = LicenceStatus::on_evaluation(TODAY, Day(TODAY.0 + 30));
        assert_eq!(gone.state, LicenceState::Expired);
        assert_eq!(gone.days_left, Some(-1));
        assert!(gone.claims.is_none());
        assert!(
            gone.reason
                .as_deref()
                .unwrap()
                .contains("ended on 2026-12-07"),
            "{gone:?}"
        );
    }

    #[test]
    fn missing_is_no_file_and_no_term() {
        let s = LicenceStatus::missing("cannot create /x/.mununu: read-only");
        assert_eq!(s.state, LicenceState::Missing);
        let reason = s.reason.clone().unwrap();
        assert!(
            reason.contains("[licensing] file")
                && reason.contains(ENV_VAR)
                && reason.contains(SYSTEM_PATH)
                && reason.contains("read-only")
        );
        assert_eq!(
            s.summary(),
            LicenceSummary {
                state: LicenceState::Missing,
                tier: None,
                expires: None
            }
        );
    }

    /// The policy table, one row per state (the H1b decision of 2026-11-08).
    #[test]
    fn the_decision_follows_the_table() {
        let k = keys();
        let run = |text: &str, day: Day| evaluate(text, day, &k, None);
        // valid → run, silently.
        assert_eq!(
            decide(&run(&issue(&claims("2026-11-01", "2027-11-01", 14)), TODAY)),
            Decision::Run
        );
        // grace → run with a loud line naming the expiry and the days left.
        let grace = run(&issue(&claims("2026-01-01", "2026-11-05", 7)), TODAY);
        assert_eq!(grace.state, LicenceState::Grace);
        match decide(&grace) {
            Decision::Warn(m) => assert!(
                m.contains("expired on 2026-11-05") && m.contains("4 grace day"),
                "{m}"
            ),
            d => panic!("{d:?}"),
        }
        // an expired file → refuse, naming the licensee.
        let expired = run(&issue(&claims("2026-01-01", "2026-10-01", 7)), TODAY);
        assert_eq!(expired.state, LicenceState::Expired);
        match decide(&expired) {
            Decision::Refuse(m) => assert!(
                m.contains("Example Ltd") && m.contains("docs/licensing.md"),
                "{m}"
            ),
            d => panic!("{d:?}"),
        }
        // no file within the term → run with the evaluation line.
        match decide(&LicenceStatus::on_evaluation(TODAY, Day(TODAY.0 + 10))) {
            Decision::Warn(m) => assert!(m.contains("19 day(s) left of 30"), "{m}"),
            d => panic!("{d:?}"),
        }
        // no file past the term → refuse.
        match decide(&LicenceStatus::on_evaluation(TODAY, Day(TODAY.0 + 31))) {
            Decision::Refuse(m) => assert!(
                m.contains("evaluation on this host ended on 2026-12-07"),
                "{m}"
            ),
            d => panic!("{d:?}"),
        }
        // an invalid file is treated as no file: the term decides, the reason is kept.
        let mut invalid = LicenceStatus::invalid(
            Some("x.lic".into()),
            None,
            "signature does not match the claims".into(),
        );
        invalid.evaluation = Some(evaluation_term(TODAY, Day(TODAY.0 + 3)));
        match decide(&invalid) {
            Decision::Warn(m) => assert!(
                m.contains("signature does not match") && m.contains("26 day(s) left"),
                "{m}"
            ),
            d => panic!("{d:?}"),
        }
        invalid.evaluation = Some(evaluation_term(TODAY, Day(TODAY.0 + 60)));
        assert!(matches!(decide(&invalid), Decision::Refuse(_)));
        invalid.evaluation = None;
        assert!(matches!(decide(&invalid), Decision::Warn(_)));
        // missing (no term could be counted) → run, say so.
        assert!(matches!(
            decide(&LicenceStatus::missing("read-only home")),
            Decision::Warn(_)
        ));
    }

    #[test]
    fn the_env_var_names_the_file_when_the_deployment_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mununu.lic");
        std::fs::write(&path, issue(&claims("2026-11-01", "2027-11-01", 0))).unwrap();
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, path.to_str().unwrap());
        assert_eq!(locate().unwrap(), Some(path.clone()));
        // The compiled-in key did not sign it: the status is invalid, naming the test key id, and
        // the evaluation term from the stamp directory governs.
        let stamp = dir.path().join(STAMP_DIR);
        let s = status_at(TODAY, Some(&stamp));
        assert_eq!(s.state, LicenceState::Invalid, "{s:?}");
        assert!(
            s.reason
                .as_deref()
                .unwrap()
                .contains("unknown issuer key id `test-issuer`")
        );
        assert_eq!(s.source, Some(path.display().to_string()));
        assert_eq!(s.evaluation.as_ref().map(|t| t.days_left), Some(29));
        // Without a stamp directory the term cannot be counted; the status says only invalid.
        assert_eq!(status_at(TODAY, None).evaluation, None);
    }

    #[test]
    fn no_file_reads_the_stamp_or_says_why_it_could_not() {
        let dir = tempfile::tempdir().unwrap();
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, "");
        let stamp = dir.path().join(STAMP_DIR);
        let s = status_at(TODAY, Some(&stamp));
        assert_eq!(s.state, LicenceState::Evaluation, "{s:?}");
        assert_eq!(
            status_at(Day(TODAY.0 + 30), Some(&stamp)).state,
            LicenceState::Expired
        );
        // A stamp directory under a regular file: uncreatable for root too (CI runs as root).
        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let s = status_at(TODAY, Some(&file.join(STAMP_DIR)));
        assert_eq!(s.state, LicenceState::Missing);
        assert!(
            s.reason.as_deref().unwrap().contains("cannot create"),
            "{s:?}"
        );
        assert_eq!(status_at(TODAY, None).state, LicenceState::Missing);
    }

    #[test]
    fn an_unreadable_named_file_is_invalid_not_missing() {
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, "/nonexistent/dir/mununu.lic");
        let s = status_at(TODAY, None);
        assert_eq!(s.state, LicenceState::Invalid);
        assert!(
            s.reason
                .unwrap()
                .starts_with("cannot read the licence file")
        );
    }
}
