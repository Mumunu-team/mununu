//! The deployment configuration (`mununu.toml`): what one installation of mununu exposes.
//!
//! > Source of truth: [`DeploymentConfig`] — surface: (CLI+API), through the global
//! > `--deployment-config FILE` flag, `mununu server --config FILE`, or `MUNUNU_DEPLOYMENT_CONFIG`.
//!
//! ```toml
//! # mununu.toml
//! enabled_domains = ["rtl", "universal"]   # optional: the template domains this deployment shows
//!
//! [templates]
//! files = ["packs/rtl-safety.json"]        # template packs, relative to this file
//!
//! [licensing]
//! file = "mununu.lic"                      # the signed licence file (Track H1, [`crate::licence`])
//! ledger = "usage.ndjson"                  # opt-in local usage ledger (Track H3, [`crate::usage_ledger`])
//! ```
//!
//! The configuration is process-wide: the CLI installs it once at start-up ([`install`]) and every
//! template lookup reads it ([`current`]); without an installed file, `MUNUNU_DEPLOYMENT_CONFIG`
//! names one; without either, the deployment is the defaults (built-in templates, every domain).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// A parsed `mununu.toml`, with every relative path resolved against the file's directory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeploymentConfig {
    /// Where the configuration came from (`None` for the defaults).
    pub source: Option<PathBuf>,
    /// Template pack files this deployment adds to the built-in catalog.
    pub template_files: Vec<PathBuf>,
    /// The template domains this deployment exposes (`None` = all).
    pub enabled_domains: Option<Vec<String>>,
    /// The licence file `[licensing] file` names, resolved against this file's directory; the
    /// first place [`crate::licence::locate`] looks.
    pub licence_file: Option<PathBuf>,
    /// Track H3 — the local usage ledger `[licensing] ledger` names (NDJSON, one record per
    /// run, no design content), resolved against this file's directory; the first place
    /// [`crate::usage_ledger::locate`] looks. `None` = the ledger is off unless the environment
    /// names one.
    pub usage_ledger: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    enabled_domains: Option<Vec<String>>,
    #[serde(default)]
    templates: Option<RawTemplates>,
    #[serde(default)]
    licensing: Option<RawLicensing>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplates {
    #[serde(default)]
    files: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLicensing {
    #[serde(default)]
    file: Option<PathBuf>,
    #[serde(default)]
    ledger: Option<PathBuf>,
}

impl DeploymentConfig {
    /// Read and validate a `mununu.toml`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("deployment config {}: {e}", path.display()))?;
        Self::parse(&text, path)
    }

    /// Parse `text` as if read from `path` (relative template files resolve against its directory).
    pub fn parse(text: &str, path: &Path) -> Result<Self, String> {
        let raw: RawConfig = toml::from_str(text)
            .map_err(|e| format!("deployment config {}: {e}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let resolve = |f: PathBuf| if f.is_absolute() { f } else { base.join(f) };
        let template_files = raw
            .templates
            .map(|t| t.files)
            .unwrap_or_default()
            .into_iter()
            .map(resolve)
            .collect();
        let (licence_file, usage_ledger) = match raw.licensing {
            Some(l) => (l.file.map(resolve), l.ledger.map(resolve)),
            None => (None, None),
        };
        Ok(DeploymentConfig {
            source: Some(path.to_path_buf()),
            template_files,
            enabled_domains: raw.enabled_domains,
            licence_file,
            usage_ledger,
        })
    }
}

static INSTALLED: OnceLock<DeploymentConfig> = OnceLock::new();

/// Install the process-wide configuration (the CLI does this once, from `--deployment-config` or
/// `server --config`). A second call is an error: the deployment does not change mid-process.
pub fn install(config: DeploymentConfig) -> Result<(), String> {
    INSTALLED
        .set(config)
        .map_err(|_| "the deployment configuration is already installed".to_string())
}

/// The configuration in force: the installed one, else the file `MUNUNU_DEPLOYMENT_CONFIG` names,
/// else the defaults. An unreadable `MUNUNU_DEPLOYMENT_CONFIG` file is an error, never a silent
/// fallback to the defaults.
pub fn current() -> Result<DeploymentConfig, String> {
    if let Some(c) = INSTALLED.get() {
        return Ok(c.clone());
    }
    match std::env::var_os("MUNUNU_DEPLOYMENT_CONFIG") {
        Some(p) if !p.is_empty() => DeploymentConfig::load(Path::new(&p)),
        _ => Ok(DeploymentConfig::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_resolves_pack_paths_against_its_directory() {
        let c = DeploymentConfig::parse(
            "enabled_domains = [\"rtl\"]\n[templates]\nfiles = [\"packs/a.json\", \"/abs/b.json\"]\n[licensing]\nfile = \"mununu.lic\"\nledger = \"usage.ndjson\"\n",
            Path::new("/etc/mununu/mununu.toml"),
        )
        .unwrap();
        // H3 — the ledger path resolves like the licence file's.
        assert_eq!(
            c.usage_ledger,
            Some(PathBuf::from("/etc/mununu/usage.ndjson"))
        );
        assert_eq!(
            c.template_files,
            vec![
                PathBuf::from("/etc/mununu/packs/a.json"),
                PathBuf::from("/abs/b.json")
            ]
        );
        assert_eq!(c.enabled_domains, Some(vec!["rtl".to_string()]));
        assert_eq!(
            c.licence_file,
            Some(PathBuf::from("/etc/mununu/mununu.lic"))
        );
    }

    #[test]
    fn a_licensing_table_names_only_the_file() {
        let err =
            DeploymentConfig::parse("[licensing]\nkey = \"x\"\n", Path::new("m.toml")).unwrap_err();
        assert!(err.contains("key"), "{err}");
        let none = DeploymentConfig::parse("[licensing]\n", Path::new("m.toml")).unwrap();
        assert_eq!(none.licence_file, None);
        assert_eq!(none.usage_ledger, None, "H3 — the ledger is opt-in");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let err =
            DeploymentConfig::parse("enabled_domain = []\n", Path::new("m.toml")).unwrap_err();
        assert!(err.contains("enabled_domain"), "{err}");
    }
}
