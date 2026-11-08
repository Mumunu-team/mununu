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
//! `sv verify-auto` report. What the tool *does* in each state (run, warn, refuse to start) is
//! policy, decided and shipped separately; this module only reports.
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
    /// A file was found but is not a licence this binary accepts; `reason` says why.
    Invalid,
    /// No licence file at any of the three locations.
    Missing,
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

impl LicenceStatus {
    fn invalid(source: Option<String>, claims: Option<LicenceClaims>, reason: String) -> Self {
        LicenceStatus {
            state: LicenceState::Invalid,
            source,
            claims,
            days_left: None,
            reason: Some(reason),
            note: None,
        }
    }

    /// The `missing` status, naming where a file was looked for.
    pub fn missing() -> Self {
        LicenceStatus {
            state: LicenceState::Missing,
            source: None,
            claims: None,
            days_left: None,
            reason: Some(format!(
                "no licence file: none named by `[licensing] file` in mununu.toml, by {ENV_VAR}, or at {SYSTEM_PATH}"
            )),
            note: None,
        }
    }

    /// The summary for `/health` and the reports.
    pub fn summary(&self) -> LicenceSummary {
        LicenceSummary {
            state: self.state,
            tier: self.claims.as_ref().map(|c| c.tier),
            expires: self.claims.as_ref().map(|c| c.expires.clone()),
        }
    }
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

/// The status of this deployment's licence today, against the compiled-in issuer keys.
pub fn status() -> LicenceStatus {
    let path = match locate() {
        Ok(Some(p)) => p,
        Ok(None) => return LicenceStatus::missing(),
        Err(e) => return LicenceStatus::invalid(None, None, e),
    };
    let source = Some(path.display().to_string());
    match std::fs::read_to_string(&path) {
        Ok(text) => evaluate(&text, Day::today(), ISSUER_KEYS, source),
        Err(e) => {
            LicenceStatus::invalid(source, None, format!("cannot read the licence file: {e}"))
        }
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
    fn missing_names_the_three_places_it_looked() {
        let s = LicenceStatus::missing();
        assert_eq!(s.state, LicenceState::Missing);
        let reason = s.reason.clone().unwrap();
        assert!(
            reason.contains("[licensing] file")
                && reason.contains(ENV_VAR)
                && reason.contains(SYSTEM_PATH)
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

    #[test]
    fn the_env_var_names_the_file_when_the_deployment_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mununu.lic");
        std::fs::write(&path, issue(&claims("2026-11-01", "2027-11-01", 0))).unwrap();
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, path.to_str().unwrap());
        assert_eq!(locate().unwrap(), Some(path.clone()));
        // The compiled-in key did not sign it: the status is invalid, naming the test key id.
        let s = status();
        assert_eq!(s.state, LicenceState::Invalid, "{s:?}");
        assert!(
            s.reason
                .unwrap()
                .contains("unknown issuer key id `test-issuer`")
        );
        assert_eq!(s.source, Some(path.display().to_string()));
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, "");
        // An empty variable is "not set".
        let _ = locate();
    }

    #[test]
    fn an_unreadable_named_file_is_invalid_not_missing() {
        let _g = crate::adapter::env_var::override_for_test(ENV_VAR, "/nonexistent/dir/mununu.lic");
        let s = status();
        assert_eq!(s.state, LicenceState::Invalid);
        assert!(
            s.reason
                .unwrap()
                .starts_with("cannot read the licence file")
        );
    }
}
