//! mununu#938 — the usage ledger records a licence's team and tier only when its signature
//! verified. Before the fix it copied them from an `invalid` file's parsed claims, so a forged
//! file put any team and tier its author liked into the ledger a licence audit reads.
//!
//! Its own binary: it points the process's `MUNUNU_LICENSE_FILE` at a forged file.

use base64::Engine;
use mununu_core::usage_ledger::{Surface, UsageRecord};

#[test]
fn an_invalid_licence_files_team_and_tier_stay_out_of_the_ledger() {
    let dir = std::env::temp_dir().join(format!("mununu-938-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let claims = serde_json::json!({
        "licensee": "Nobody Signed This",
        "team_id": "forged-team-938",
        "tier": "team",
        "issued": "2026-01-01",
        "expires": "2099-01-01",
        "grace_days": 0,
        "key_id": "no-such-issuer-key",
    });
    let b64 = base64::engine::general_purpose::STANDARD;
    let file = serde_json::json!({
        "mununu_licence": mununu_core::licence::FORMAT_VERSION,
        "payload": b64.encode(claims.to_string()),
        "signature": b64.encode([0u8; 64]),
    });
    let path = dir.join("forged.lic");
    std::fs::write(&path, file.to_string()).expect("write the forged file");
    // SAFETY: this binary holds this one test, so no other thread reads the environment while
    // it is set.
    unsafe { std::env::set_var(mununu_core::licence::ENV_VAR, &path) };

    let status = mununu_core::licence::status();
    assert_eq!(
        serde_json::to_value(status.state).expect("state"),
        "invalid",
        "the fixture must be an invalid file whose claims parse: {status:?}"
    );
    let line = UsageRecord::for_run("sv verify-auto", Surface::Cli, None, 0).line();
    assert!(
        !line.contains("forged-team-938"),
        "an unsigned team reached the ledger: {line}"
    );
    let v: serde_json::Value = serde_json::from_str(&line).expect("a JSON line");
    assert!(
        v.get("licence_tier").is_none_or(serde_json::Value::is_null),
        "an unsigned tier reached the ledger: {line}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
