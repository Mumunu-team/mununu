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
//! [licensing]                              # reserved: parsed and ignored until pack signing lands
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
    /// Whether a `[licensing]` table is present (reserved; not enforced).
    pub has_licensing: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    enabled_domains: Option<Vec<String>>,
    #[serde(default)]
    templates: Option<RawTemplates>,
    #[serde(default)]
    licensing: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplates {
    #[serde(default)]
    files: Vec<PathBuf>,
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
        let template_files = raw
            .templates
            .map(|t| t.files)
            .unwrap_or_default()
            .into_iter()
            .map(|f| if f.is_absolute() { f } else { base.join(f) })
            .collect();
        Ok(DeploymentConfig {
            source: Some(path.to_path_buf()),
            template_files,
            enabled_domains: raw.enabled_domains,
            has_licensing: raw.licensing.is_some(),
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
            "enabled_domains = [\"rtl\"]\n[templates]\nfiles = [\"packs/a.json\", \"/abs/b.json\"]\n[licensing]\nkey = \"x\"\n",
            Path::new("/etc/mununu/mununu.toml"),
        )
        .unwrap();
        assert_eq!(
            c.template_files,
            vec![
                PathBuf::from("/etc/mununu/packs/a.json"),
                PathBuf::from("/abs/b.json")
            ]
        );
        assert_eq!(c.enabled_domains, Some(vec!["rtl".to_string()]));
        assert!(c.has_licensing);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let err =
            DeploymentConfig::parse("enabled_domain = []\n", Path::new("m.toml")).unwrap_err();
        assert!(err.contains("enabled_domain"), "{err}");
    }
}
