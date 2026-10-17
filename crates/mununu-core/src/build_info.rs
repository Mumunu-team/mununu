//! mununu#715 — the identity of the binary that produced a verdict.
//!
//! `mununu --version` stayed `0.4.0` from September to October while verdicts changed
//! (#695, #696, #702, #709, #711), so a consumer recording "which engine produced this verdict"
//! could not tell two builds apart. The version is an interface promise and stays one; the build
//! identity is a separate fact: the commit and whether the tree was clean.
//!
//! The identity belongs to the **binary**, not this library: the `mununu-cli` build script reads
//! it (an explicit `MUNUNU_BUILD_SHA` / `MUNUNU_BUILD_DIRTY` first, else git) and `main` calls
//! [`register`] once at startup. `mununu server` runs inside that binary, so the CLI and the HTTP
//! API report the same identity. A process that never registers one (a library use, a test)
//! reports none: the fields are then omitted, never filled with a guess.
//!
//! > Source of truth: [`register`] + [`current`] — surface: (CLI+API)

use std::sync::OnceLock;

/// The commit a binary was built from, and whether its tracked files matched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    /// The full commit sha.
    pub sha: String,
    /// `true` when no tracked file differed from the commit at build time.
    pub clean: bool,
}

static IDENTITY: OnceLock<BuildIdentity> = OnceLock::new();

/// Parse what a build script recorded: `sha` is the commit (empty or absent = not told), `dirty`
/// is `"0"` or `"1"`. Anything else is not an identity — no half-known build is reported.
pub fn identity_from(sha: Option<&str>, dirty: Option<&str>) -> Option<BuildIdentity> {
    let sha = sha?.trim();
    let well_formed = sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit());
    let clean = match dirty?.trim() {
        "0" => true,
        "1" => false,
        _ => return None,
    };
    well_formed.then(|| BuildIdentity {
        sha: sha.to_ascii_lowercase(),
        clean,
    })
}

/// Record this process's build identity. The first call wins; later calls are ignored, so the
/// identity cannot change under a running server.
pub fn register(identity: Option<BuildIdentity>) {
    if let Some(id) = identity {
        let _ = IDENTITY.set(id);
    }
}

/// The registered build identity, if any.
pub fn current() -> Option<&'static BuildIdentity> {
    IDENTITY.get()
}

/// The `--version` suffix: ` (e7a5b21)`, ` (e7a5b21-dirty)`, or empty when unknown.
pub fn version_suffix(identity: Option<&BuildIdentity>) -> String {
    match identity {
        Some(id) => format!(
            " ({}{})",
            &id.sha[..id.sha.len().min(7)],
            if id.clean { "" } else { "-dirty" }
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_commit_and_dirty_bit_is_an_identity() {
        let id = identity_from(Some("E7A5B21c0ffee"), Some("0")).unwrap();
        assert_eq!(id.sha, "e7a5b21c0ffee");
        assert!(id.clean);
        assert_eq!(version_suffix(Some(&id)), " (e7a5b21)");
        let dirty = identity_from(Some("e7a5b21c0ffee"), Some("1")).unwrap();
        assert_eq!(version_suffix(Some(&dirty)), " (e7a5b21-dirty)");
    }

    #[test]
    fn an_unknown_or_malformed_build_reports_nothing() {
        assert_eq!(identity_from(None, Some("0")), None);
        assert_eq!(identity_from(Some(""), Some("0")), None);
        assert_eq!(
            identity_from(Some("e7a5b21"), None),
            None,
            "the dirty bit is not guessed"
        );
        assert_eq!(identity_from(Some("not-a-sha"), Some("0")), None);
        assert_eq!(identity_from(Some("e7a5b21"), Some("maybe")), None);
        assert_eq!(version_suffix(None), "");
    }
}
