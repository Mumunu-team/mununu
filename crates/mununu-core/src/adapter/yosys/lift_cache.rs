//! N0.2 (2026-11-03) — a content-addressed cache of the SV → BTOR2 lift's raw tool outputs.
//!
//! Every `e2e_` test, app test and corpus gate re-ran yosys on inputs identical to
//! the run before; the four OpenTitan e2e tests paid their lift on every sweep. With
//! `MUNUNU_LIFT_CACHE_DIR=<dir>` set, [`super::run_sv_flatten_btor2`] looks the lift up before
//! it runs a tool and stores it after. Unset, nothing changes.
//!
//! **What is keyed** — everything the tools are *told*, rendered by the same planner that
//! stages the real run ([`super::plan_lift`] against a placeholder directory): the staged
//! sources after `strip_port_net_types`, the exact yosys script, whether yosys runs inside the
//! staging directory, every file under an include directory a tool may read, the tool
//! identities (`yosys -V` banner, a plugin's path, size and mtime; the dev-only oracle's sv2v
//! argv and `--version`), the crate version and [`LIFT_CACHE_FORMAT`]. A change to any of it is
//! a miss.
//!
//! **What is stored** — the raw outputs of a *successful* lift (`design.btor`, `hier.json`,
//! `clocks.json`) plus the process facts (front end used, backend retry, the staged primary
//! path). Everything mununu computes *from* those — clock domains,
//! the multi-clock refusal, blackbox names, port directions — runs on a hit exactly as on a
//! miss, so a change to that Rust code takes effect without invalidating the cache.
//!
//! **A hit is verified byte for byte** against the stored manifest, so the 64-bit index hash
//! only has to be a good index, never a proof of identity. Entries are published atomically
//! (written to a staging directory, then renamed), so parallel e2e processes lifting the same
//! design never read a half-written entry.
//!
//! Failure paths (CLAUDE.md §Failure Paths): a cache directory that cannot be used, a manifest
//! that cannot be built (an include tree over the cap, an unreadable file) or an entry that
//! cannot be read is a *bypass* — the lift runs as if the cache were off — and the outcome
//! reaches the caller as [`LiftCacheOutcome::Bypassed`] with its reason, logged at `warn`.
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// The environment variable naming the cache directory; unset or empty = off.
pub const LIFT_CACHE_DIR_ENV: &str = "MUNUNU_LIFT_CACHE_DIR";

/// Bump when the way the tools are INVOKED changes in a way the manifest does not render —
/// a new environment variable handed to yosys, a flag outside the script text, a different
/// working directory rule. The script, the oracle's sv2v argv and the tool banners are in the
/// manifest, so a change to any of those misses on its own. 2 (N1, 2026-11-04): one attempt per
/// lift, the slang-only product front end.
pub(super) const LIFT_CACHE_FORMAT: u32 = 2;

/// Caps on the include-directory walk: above either, the manifest is not built and the lift
/// bypasses the cache (a walk that large would cost more than the lift it saves).
const MAX_INCLUDE_FILES: usize = 4_096;
const MAX_INCLUDE_BYTES: u64 = 64 * 1024 * 1024;

/// One tool the lift runs or loads: where it is and what identifies its build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ToolIdentity {
    pub path: String,
    /// `yosys -V` / `sv2v --version` output, or `len=…,mtime=…` for a plugin file.
    pub identity: String,
}

impl ToolIdentity {
    /// A plugin file's identity from its metadata (no subprocess): size and modification time.
    /// A bare name (yosys resolves `-m slang` from its plugin dir) is identified by the pinned
    /// image's plugin file when it exists, else by the name alone.
    pub(super) fn of_plugin(arg: &str) -> ToolIdentity {
        let candidate = if Path::new(arg).exists() {
            PathBuf::from(arg)
        } else {
            PathBuf::from("/opt/oss-cad-suite/share/yosys/plugins/slang.so")
        };
        let identity = match std::fs::metadata(&candidate) {
            Ok(m) => {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("len={},mtime={mtime}", m.len())
            }
            Err(_) => "unresolved".to_string(),
        };
        ToolIdentity {
            path: arg.to_string(),
            identity,
        }
    }
}

/// The dev-only oracle's sv2v pass: the binary and the exact argument vector (`test_support`
/// builds plan one; the product never does, and the field stays in the key so both builds
/// render the same manifest for the same lift).
#[derive(Debug, Serialize)]
#[cfg_attr(not(feature = "test_support"), allow(dead_code))]
pub(super) struct Sv2vManifest {
    pub tool: ToolIdentity,
    pub argv: Vec<String>,
}

/// The lift as the tools would see it.
#[derive(Debug, Serialize)]
pub(super) struct AttemptManifest {
    pub frontend: &'static str,
    /// The yosys script with the staging directory rendered as a placeholder.
    pub script: String,
    /// yosys runs inside the staging directory (mununu#806) — the names it mints depend on it.
    pub cwd_in_staging_dir: bool,
    pub plugin: Option<ToolIdentity>,
    pub sv2v: Option<Sv2vManifest>,
}

/// What one lift tells its tools. Serialised it is the key's content; its hash is the index.
#[derive(Debug, Serialize)]
pub(super) struct LiftManifest {
    pub format: u32,
    pub crate_version: &'static str,
    pub yosys: ToolIdentity,
    pub attempt: AttemptManifest,
    /// The staged sources as the tools read them, relative to the staging directory, the
    /// primary (`work.sv`) first.
    pub staged: Vec<(String, String)>,
    /// Every file under an include directory a tool may read, `(path, content)`, sorted.
    pub include_files: Vec<(String, String)>,
}

/// The serialised manifest and its index.
#[derive(Debug, Clone)]
pub(super) struct LiftCacheKey {
    manifest: Vec<u8>,
    index: String,
}

impl LiftCacheKey {
    pub(super) fn new(manifest: &LiftManifest) -> Result<Self, String> {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|e| format!("the lift manifest does not serialise: {e}"))?;
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        Ok(LiftCacheKey {
            index: format!("{:016x}", h.finish()),
            manifest: bytes,
        })
    }

    pub(super) fn index(&self) -> &str {
        &self.index
    }
}

/// The raw outputs of one successful lift, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CachedLift {
    pub btor2: String,
    pub hier_json: String,
    pub flat_netlist_json: String,
    pub staged_primary: String,
    /// `slang` (`verilog` only from the dev-only oracle) — see [`frontend_tag`].
    pub frontend_used: String,
    pub backend_retry: Option<String>,
}

/// The stored tag of a concrete front end.
pub(super) fn frontend_tag(fe: super::SvFrontend) -> &'static str {
    match fe {
        super::SvFrontend::Slang => "slang",
        super::SvFrontend::Verilog => "verilog",
    }
}

/// The front end a stored tag names; `None` for anything else (a corrupt entry).
pub(super) fn frontend_from_tag(tag: &str) -> Option<super::SvFrontend> {
    match tag {
        "slang" => Some(super::SvFrontend::Slang),
        "verilog" => Some(super::SvFrontend::Verilog),
        _ => None,
    }
}

/// What the cache did for one lift. `Off` when [`LIFT_CACHE_DIR_ENV`] is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiftCacheOutcome {
    Off,
    /// The artifacts came from the cache; no tool ran.
    Hit,
    /// The tools ran and the result was stored (or storing failed — logged, never fatal).
    Miss,
    /// The cache was configured but could not be used for this lift; the tools ran.
    Bypassed(String),
}

impl fmt::Display for LiftCacheOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiftCacheOutcome::Off => f.write_str("off"),
            LiftCacheOutcome::Hit => f.write_str("hit"),
            LiftCacheOutcome::Miss => f.write_str("miss"),
            LiftCacheOutcome::Bypassed(reason) => write!(f, "bypassed: {reason}"),
        }
    }
}

/// A cache directory. One entry per index: `<dir>/<index>/{manifest.json, lift.json}`.
#[derive(Debug, Clone)]
pub(super) struct LiftCache {
    dir: PathBuf,
}

impl LiftCache {
    /// The cache named by [`LIFT_CACHE_DIR_ENV`], or `None` when unset or empty.
    pub(super) fn from_env() -> Option<Self> {
        // The literal, not the const: `docs/env-vars.md`'s check greps for `env::var("MUNUNU_…")`.
        let dir = std::env::var("MUNUNU_LIFT_CACHE_DIR").ok()?;
        let dir = dir.trim();
        (!dir.is_empty()).then(|| LiftCache::at(PathBuf::from(dir)))
    }

    pub(super) fn at(dir: PathBuf) -> Self {
        LiftCache { dir }
    }

    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_dir(&self, key: &LiftCacheKey) -> PathBuf {
        self.dir.join(key.index())
    }

    /// The stored lift for `key`, verified against its manifest byte for byte. `None` on a
    /// miss, a manifest mismatch (an index collision) or an entry that cannot be read.
    pub(super) fn lookup(&self, key: &LiftCacheKey) -> Option<CachedLift> {
        let entry = self.entry_dir(key);
        let stored = std::fs::read(entry.join("manifest.json")).ok()?;
        if stored != key.manifest {
            tracing::warn!(
                target: "mununu::lift_cache",
                index = key.index(),
                "lift cache: index collision — the stored manifest differs; treating as a miss"
            );
            return None;
        }
        let bytes = std::fs::read(entry.join("lift.json")).ok()?;
        match serde_json::from_slice::<CachedLift>(&bytes) {
            Ok(lift) if frontend_from_tag(&lift.frontend_used).is_some() => Some(lift),
            Ok(lift) => {
                tracing::warn!(
                    target: "mununu::lift_cache",
                    index = key.index(),
                    tag = lift.frontend_used,
                    "lift cache: entry names an unknown front end; treating as a miss"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    target: "mununu::lift_cache",
                    index = key.index(),
                    "lift cache: entry does not parse ({e}); treating as a miss"
                );
                None
            }
        }
    }

    /// Publish `lift` under `key`: written to a staging directory, then renamed into place,
    /// so a concurrent reader sees either no entry or a whole one. An entry already there
    /// (a parallel process won the race, or it was corrupt) is replaced.
    pub(super) fn store(&self, key: &LiftCacheKey, lift: &CachedLift) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create {}: {e}", self.dir.display()))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let staging = self.dir.join(format!(
            ".staging-{}-{}-{nanos}",
            key.index(),
            std::process::id()
        ));
        let publish = || -> Result<(), String> {
            std::fs::create_dir_all(&staging)
                .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
            std::fs::write(staging.join("manifest.json"), &key.manifest)
                .map_err(|e| format!("cannot write manifest: {e}"))?;
            let bytes = serde_json::to_vec(lift).map_err(|e| format!("cannot serialise: {e}"))?;
            std::fs::write(staging.join("lift.json"), bytes)
                .map_err(|e| format!("cannot write lift: {e}"))?;
            let entry = self.entry_dir(key);
            if entry.exists() {
                std::fs::remove_dir_all(&entry)
                    .map_err(|e| format!("cannot replace {}: {e}", entry.display()))?;
            }
            std::fs::rename(&staging, &entry)
                .map_err(|e| format!("cannot publish {}: {e}", entry.display()))
        };
        let out = publish();
        if out.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        out
    }
}

/// Every regular file under `dirs` (recursively), as `(path, content)` sorted by path, for the
/// manifest. Directories that do not exist are skipped (a tool would not read them either);
/// an unreadable file or a tree over the caps is an error, so the lift bypasses the cache
/// rather than key on a partial view.
pub(super) fn collect_include_files(dirs: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    let mut roots: Vec<PathBuf> = dirs.to_vec();
    roots.sort();
    roots.dedup();
    let mut files: Vec<(String, String)> = Vec::new();
    let mut bytes: u64 = 0;
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| format!("cannot read include dir {}: {e}", dir.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| format!("cannot read include dir {}: {e}", dir.display()))?;
                let path = entry.path();
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
                if meta.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }
                bytes += meta.len();
                if files.len() + 1 > MAX_INCLUDE_FILES || bytes > MAX_INCLUDE_BYTES {
                    return Err(format!(
                        "include tree under {} exceeds the cache's walk cap ({MAX_INCLUDE_FILES} \
                         files / {} MiB)",
                        root.display(),
                        MAX_INCLUDE_BYTES / (1024 * 1024)
                    ));
                }
                let content = std::fs::read(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                files.push((
                    path.to_string_lossy().into_owned(),
                    String::from_utf8_lossy(&content).into_owned(),
                ));
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn manifest(primary: &str) -> LiftManifest {
        LiftManifest {
            format: LIFT_CACHE_FORMAT,
            crate_version: "test",
            yosys: ToolIdentity {
                path: "yosys".into(),
                identity: "Yosys 0.68".into(),
            },
            attempt: AttemptManifest {
                frontend: "slang",
                script:
                    "read_slang --ignore-assertions --ignore-timing /P/work.sv; hierarchy -auto-top"
                        .into(),
                cwd_in_staging_dir: false,
                plugin: None,
                sv2v: None,
            },
            staged: vec![("work.sv".into(), primary.into())],
            include_files: Vec::new(),
        }
    }

    fn lift() -> CachedLift {
        CachedLift {
            btor2: "1 sort bitvec 1\n2 state 1 q\n".into(),
            hier_json: "{}".into(),
            flat_netlist_json: "{}".into(),
            staged_primary: "/tmp/mununu-yosys-1-2-3-4/work.sv".into(),
            frontend_used: "verilog".into(),
            backend_retry: None,
        }
    }

    /// Store, then look up: the same manifest returns the same lift; a different source is a miss.
    #[test]
    fn a_stored_lift_is_found_by_its_manifest_and_not_by_another() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LiftCache::at(dir.path().to_path_buf());
        let key = LiftCacheKey::new(&manifest("module m; endmodule")).unwrap();
        assert!(cache.lookup(&key).is_none(), "empty cache");
        cache.store(&key, &lift()).unwrap();
        assert_eq!(cache.lookup(&key), Some(lift()));
        let other = LiftCacheKey::new(&manifest("module m; reg q; endmodule")).unwrap();
        assert_ne!(other.index(), key.index());
        assert!(cache.lookup(&other).is_none());
        // No staging directory is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// An entry under the key's index whose manifest differs (an index collision) is a miss,
    /// never a hit: the hash is an index, the manifest is the identity.
    #[test]
    fn an_index_collision_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LiftCache::at(dir.path().to_path_buf());
        let key = LiftCacheKey::new(&manifest("module m; endmodule")).unwrap();
        cache.store(&key, &lift()).unwrap();
        std::fs::write(dir.path().join(key.index()).join("manifest.json"), b"{}").unwrap();
        assert!(cache.lookup(&key).is_none());
    }

    /// A corrupt entry (truncated lift, or a front end tag the code does not know) is a miss
    /// and `store` replaces it.
    #[test]
    fn a_corrupt_entry_is_a_miss_and_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LiftCache::at(dir.path().to_path_buf());
        let key = LiftCacheKey::new(&manifest("module m; endmodule")).unwrap();
        cache.store(&key, &lift()).unwrap();
        let entry = dir.path().join(key.index());
        std::fs::write(entry.join("lift.json"), b"{\"btor2\": ").unwrap();
        assert!(cache.lookup(&key).is_none());
        let mut bad_tag = lift();
        bad_tag.frontend_used = "verific".into();
        cache.store(&key, &bad_tag).unwrap();
        assert!(cache.lookup(&key).is_none());
        cache.store(&key, &lift()).unwrap();
        assert_eq!(cache.lookup(&key), Some(lift()));
    }

    /// The manifest's index moves with anything the tools are told — the script, whether yosys
    /// runs in the staging dir, a plugin's identity, an include file — and stays put when nothing moved.
    #[test]
    fn the_index_follows_what_the_tools_are_told() {
        let base = LiftCacheKey::new(&manifest("module m; endmodule")).unwrap();
        let again = LiftCacheKey::new(&manifest("module m; endmodule")).unwrap();
        assert_eq!(base.index(), again.index());
        let mut m = manifest("module m; endmodule");
        m.attempt.script.push_str("; opt_clean");
        assert_ne!(LiftCacheKey::new(&m).unwrap().index(), base.index());
        let mut m = manifest("module m; endmodule");
        m.attempt.cwd_in_staging_dir = true;
        assert_ne!(LiftCacheKey::new(&m).unwrap().index(), base.index());
        let mut m = manifest("module m; endmodule");
        m.attempt.plugin = Some(ToolIdentity {
            path: "slang".into(),
            identity: "len=1,mtime=2".into(),
        });
        assert_ne!(LiftCacheKey::new(&m).unwrap().index(), base.index());
        let mut m = manifest("module m; endmodule");
        m.include_files
            .push(("/inc/a.svh".into(), "`define A 1".into()));
        assert_ne!(LiftCacheKey::new(&m).unwrap().index(), base.index());
        let mut m = manifest("module m; endmodule");
        m.yosys.identity = "Yosys 0.69".into();
        assert_ne!(LiftCacheKey::new(&m).unwrap().index(), base.index());
    }

    /// The include walk reads every regular file under the dirs, sorted, skips a missing dir,
    /// and refuses a tree over the file cap.
    #[test]
    fn the_include_walk_is_sorted_complete_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let inc = dir.path().join("inc");
        std::fs::create_dir_all(inc.join("nested")).unwrap();
        std::fs::write(inc.join("b.svh"), "b").unwrap();
        std::fs::write(inc.join("a.svh"), "a").unwrap();
        std::fs::write(inc.join("nested").join("c.vh"), "c").unwrap();
        let missing = dir.path().join("absent");
        let files = collect_include_files(&[inc.clone(), missing, inc.clone()]).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|(p, _)| p.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(names, ["a.svh", "b.svh", "c.vh"]);
        assert_eq!(files[2].1, "c");
        let many = dir.path().join("many");
        std::fs::create_dir_all(&many).unwrap();
        for i in 0..=MAX_INCLUDE_FILES {
            std::fs::write(many.join(format!("f{i}.vh")), "x").unwrap();
        }
        let err = collect_include_files(&[many]).unwrap_err();
        assert!(err.contains("walk cap"), "{err}");
    }

    #[test]
    fn frontend_tags_round_trip_and_reject_unknowns() {
        for fe in [
            super::super::SvFrontend::Verilog,
            super::super::SvFrontend::Slang,
        ] {
            assert_eq!(frontend_from_tag(frontend_tag(fe)), Some(fe));
        }
        assert_eq!(frontend_from_tag("auto"), None);
        assert_eq!(frontend_from_tag(""), None);
    }
}
