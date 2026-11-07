//! Contract corpus — file-backed lookup of vetted black-box module
//! contracts.
//!
//! Document D task D1 in `docs/archive/design/contract-corpus-and-config.md`.
//! The corpus replaces the chaotic stub default (Document A §2) with a
//! specific, vetted contract whenever an entry exists for the
//! `(domain, name, parameters)` tuple at hand. Today the backend is a
//! plain directory tree (Phase 1 per §D.2.2); the schema and query
//! semantics are deliberately stable so a SQLite or remote-service
//! backend can be added later without touching callers.
//!
//! # Layout
//!
//! ```text
//! <corpus_root>/
//! ├── <domain>/
//! │   ├── <name>@<version>.json
//! │   ├── <name>@<version>-alt-<label>.json   (optional alternatives)
//! │   └── ...
//! ├── <domain>/
//! │   └── ...
//! ```
//!
//! Each `.json` file matches the [`ContractEntry`] schema below. The
//! filename is canonical and used for fast lookup; the `id` field
//! inside the file must match `<domain>/<name>`.
//!
//! # Query
//!
//! [`Corpus::query`] returns the list of candidate entries that match a
//! `(domain, name, parameters)` request, sorted by:
//!
//! 1. Parameter-match exactness (full match wins).
//! 2. Provenance trust tier (`MununuVerified` > `Vendor` > `Community`).
//! 3. Version recency (lexicographic descending on the `version`
//!    string; semver-aware parsing is future work, §D.7).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A single contract entry stored in the corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractEntry {
    /// Canonical `<domain>/<name>` identifier (e.g.
    /// `rtl_protocol/axi4_slave`).
    pub id: String,
    /// Human-readable version. Recommend semver for readability; the
    /// ranker treats this as an opaque string today.
    pub version: String,
    /// Domain bucket (`rtl_protocol`, `rtl_memory`,
    /// `software_library`, `software_protocol`, …).
    pub domain: String,
    /// Module / component / library / class name.
    pub name: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Parameters this entry was validated against. Stored as a map of
    /// param-name → JSON value. The query's `parameters` argument is
    /// scored against this map by exact equality per key.
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// Optional contract body — the canonical artefact callers
    /// consume (interface alphabet, controllability, automaton,
    /// formulas). Treated opaquely by the corpus layer; the contract
    /// subsystem owns its shape.
    #[serde(default)]
    pub contract: Option<serde_json::Value>,
    /// Named alternatives within this contract (e.g. "strict" vs
    /// "permissive"). Carrying alternatives inline lets a single
    /// entry serve multiple verification styles for the same IP.
    #[serde(default)]
    pub alternatives: Vec<Alternative>,
    /// Where the entry came from. Used both for ranking (trust tier)
    /// and for the audit trail.
    pub provenance: Provenance,
    /// Soundness flag — what classes of property the entry preserves.
    #[serde(default)]
    pub soundness_flag: Option<String>,
}

/// A named alternative within a `ContractEntry`. Different alternatives
/// for the same IP let the user pick the verification style (strict vs
/// permissive ordering, Mealy vs Moore output timing, fairness
/// assumptions on/off, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alternative {
    /// Stable identifier (e.g. `"strict"`).
    pub id: String,
    /// Display label.
    pub label: String,
    /// Optional description shown at HITL review time.
    #[serde(default)]
    pub description: Option<String>,
}

/// Origin of a corpus entry. Drives the ranking tier in
/// [`Corpus::query`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "snake_case")]
pub enum Provenance {
    /// Vetted by the mununu maintainers — highest trust tier.
    MununuVerified {
        /// Reference spec the entry was verified against
        /// (e.g. `"ARM AMBA AXI4 spec rev I"`).
        #[serde(default)]
        verified_against: Option<String>,
    },
    /// Contributed by a named vendor (e.g. ARM, Synopsys).
    Vendor {
        /// Vendor identifier.
        name: String,
        /// License covering the entry's use.
        #[serde(default)]
        license: Option<String>,
    },
    /// Contributed by the community.
    Community {
        /// Contributors crediting the entry. Free-form strings.
        #[serde(default)]
        contributors: Vec<String>,
    },
}

impl Provenance {
    /// Numeric trust tier — higher is better. Used by the ranker.
    pub fn trust_tier(&self) -> u8 {
        match self {
            Provenance::MununuVerified { .. } => 3,
            Provenance::Vendor { .. } => 2,
            Provenance::Community { .. } => 1,
        }
    }
}

/// A file-backed corpus rooted at a directory.
///
/// Loaded once at startup via [`Corpus::load`]; queries are pure
/// in-memory walks over the loaded entries.
#[derive(Debug, Clone)]
pub struct Corpus {
    entries: Vec<ContractEntry>,
}

impl Corpus {
    /// Load every `.json` file under `root` into an in-memory corpus.
    /// Subdirectories are scanned recursively so the `<domain>/<name>...`
    /// layout works out of the box.
    pub fn load(root: &Path) -> Result<Self, CorpusError> {
        let mut entries = Vec::new();
        Self::load_dir_into(root, &mut entries)?;
        Ok(Corpus { entries })
    }

    /// Construct an empty corpus — useful in tests and as the default
    /// when the corpus root is absent.
    pub fn empty() -> Self {
        Corpus {
            entries: Vec::new(),
        }
    }

    /// Build a corpus from an explicit list of entries — primarily a
    /// test convenience; production code uses [`Corpus::load`].
    pub fn from_entries(entries: Vec<ContractEntry>) -> Self {
        Corpus { entries }
    }

    /// Total number of loaded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Query the corpus by `(domain, name, parameters)`. Returns the
    /// candidate entries that match, sorted by:
    /// 1. Parameter-match score (full match first).
    /// 2. Provenance trust tier.
    /// 3. Version string (lexicographic descending).
    ///
    /// `parameters` keys missing from a candidate's `parameters` map
    /// are scored as "no information" rather than mismatch — they do
    /// not eliminate the candidate. A *value mismatch* on a present
    /// key does eliminate.
    pub fn query(
        &self,
        domain: &str,
        name: &str,
        parameters: &BTreeMap<String, serde_json::Value>,
    ) -> Vec<&ContractEntry> {
        let mut scored: Vec<(usize, &ContractEntry)> = Vec::new();
        for entry in &self.entries {
            if entry.domain != domain || entry.name != name {
                continue;
            }
            // Score: count of keys in the query that match this
            // entry's stored parameters. A *mismatch* (entry has a
            // different value for the same key) disqualifies.
            let mut matched: usize = 0;
            let mut conflict = false;
            for (k, want) in parameters {
                if let Some(got) = entry.parameters.get(k) {
                    if got == want {
                        matched += 1;
                    } else {
                        conflict = true;
                        break;
                    }
                }
            }
            if !conflict {
                scored.push((matched, entry));
            }
        }
        scored.sort_by(|(score_a, a), (score_b, b)| {
            score_b
                .cmp(score_a)
                .then_with(|| b.provenance.trust_tier().cmp(&a.provenance.trust_tier()))
                .then_with(|| b.version.cmp(&a.version))
        });
        scored.into_iter().map(|(_, e)| e).collect()
    }

    fn load_dir_into(dir: &Path, into: &mut Vec<ContractEntry>) -> Result<(), CorpusError> {
        let read = std::fs::read_dir(dir).map_err(|e| CorpusError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        for entry in read {
            let entry = entry.map_err(|e| CorpusError::Io {
                path: dir.to_path_buf(),
                message: e.to_string(),
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| CorpusError::Io {
                path: path.clone(),
                message: e.to_string(),
            })?;
            if file_type.is_dir() {
                Self::load_dir_into(&path, into)?;
            } else if file_type.is_file() && path.extension().is_some_and(|e| e == "json") {
                let body = std::fs::read_to_string(&path).map_err(|e| CorpusError::Io {
                    path: path.clone(),
                    message: e.to_string(),
                })?;
                let parsed: ContractEntry =
                    serde_json::from_str(&body).map_err(|e| CorpusError::ParseEntry {
                        path: path.clone(),
                        message: e.to_string(),
                    })?;
                into.push(parsed);
            }
        }
        Ok(())
    }
}

/// Errors raised by the corpus loader.
#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("corpus: I/O error at {}: {message}", path.display())]
    Io { path: PathBuf, message: String },
    #[error("corpus: failed to parse {} as a ContractEntry: {message}", path.display())]
    ParseEntry { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(
        id: &str,
        version: &str,
        domain: &str,
        name: &str,
        parameters: BTreeMap<String, serde_json::Value>,
        provenance: Provenance,
    ) -> ContractEntry {
        ContractEntry {
            id: id.to_string(),
            version: version.to_string(),
            domain: domain.to_string(),
            name: name.to_string(),
            description: None,
            parameters,
            contract: None,
            alternatives: Vec::new(),
            provenance,
            soundness_flag: None,
        }
    }

    #[test]
    fn empty_corpus_returns_no_candidates() {
        let c = Corpus::empty();
        let res = c.query("rtl_protocol", "axi4_slave", &BTreeMap::new());
        assert!(res.is_empty());
    }

    #[test]
    fn exact_domain_and_name_match_wins() {
        let entries = vec![
            entry(
                "rtl_protocol/axi4_slave",
                "2.0.0",
                "rtl_protocol",
                "axi4_slave",
                BTreeMap::new(),
                Provenance::Community {
                    contributors: vec![],
                },
            ),
            entry(
                "rtl_protocol/axi4_master",
                "1.0.0",
                "rtl_protocol",
                "axi4_master",
                BTreeMap::new(),
                Provenance::Community {
                    contributors: vec![],
                },
            ),
        ];
        let c = Corpus::from_entries(entries);
        let hits = c.query("rtl_protocol", "axi4_slave", &BTreeMap::new());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "rtl_protocol/axi4_slave");
    }

    #[test]
    fn parameter_match_score_orders_candidates() {
        let mut params_partial = BTreeMap::new();
        params_partial.insert("addr_width".to_string(), json!(32));
        let mut params_full = params_partial.clone();
        params_full.insert("data_width".to_string(), json!(64));

        let entries = vec![
            entry(
                "rtl_protocol/axi4_slave",
                "1.0.0",
                "rtl_protocol",
                "axi4_slave",
                params_partial.clone(),
                Provenance::Community {
                    contributors: vec![],
                },
            ),
            entry(
                "rtl_protocol/axi4_slave",
                "1.0.0",
                "rtl_protocol",
                "axi4_slave",
                params_full.clone(),
                Provenance::Community {
                    contributors: vec![],
                },
            ),
        ];
        let c = Corpus::from_entries(entries);
        let mut query = BTreeMap::new();
        query.insert("addr_width".to_string(), json!(32));
        query.insert("data_width".to_string(), json!(64));
        let hits = c.query("rtl_protocol", "axi4_slave", &query);
        assert_eq!(hits.len(), 2);
        // The full-match entry wins.
        assert_eq!(hits[0].parameters.len(), 2);
        assert_eq!(hits[1].parameters.len(), 1);
    }

    #[test]
    fn provenance_tier_breaks_ties() {
        let entries = vec![
            entry(
                "rtl_protocol/axi4_slave",
                "1.0.0",
                "rtl_protocol",
                "axi4_slave",
                BTreeMap::new(),
                Provenance::Community {
                    contributors: vec![],
                },
            ),
            entry(
                "rtl_protocol/axi4_slave",
                "1.0.0",
                "rtl_protocol",
                "axi4_slave",
                BTreeMap::new(),
                Provenance::MununuVerified {
                    verified_against: Some("AXI4 spec rev I".to_string()),
                },
            ),
        ];
        let c = Corpus::from_entries(entries);
        let hits = c.query("rtl_protocol", "axi4_slave", &BTreeMap::new());
        assert_eq!(hits.len(), 2);
        // mununu-verified beats community on tie.
        assert!(matches!(
            hits[0].provenance,
            Provenance::MununuVerified { .. }
        ));
    }

    #[test]
    fn parameter_value_mismatch_disqualifies() {
        let mut params = BTreeMap::new();
        params.insert("addr_width".to_string(), json!(32));
        let entries = vec![entry(
            "rtl_protocol/axi4_slave",
            "1.0.0",
            "rtl_protocol",
            "axi4_slave",
            params,
            Provenance::Community {
                contributors: vec![],
            },
        )];
        let c = Corpus::from_entries(entries);
        let mut query = BTreeMap::new();
        query.insert("addr_width".to_string(), json!(64));
        let hits = c.query("rtl_protocol", "axi4_slave", &query);
        assert!(hits.is_empty(), "value mismatch should eliminate candidate");
    }

    #[test]
    fn unknown_query_param_does_not_eliminate() {
        let entries = vec![entry(
            "rtl_protocol/axi4_slave",
            "1.0.0",
            "rtl_protocol",
            "axi4_slave",
            BTreeMap::new(),
            Provenance::Community {
                contributors: vec![],
            },
        )];
        let c = Corpus::from_entries(entries);
        let mut query = BTreeMap::new();
        query.insert("data_width".to_string(), json!(64));
        // Entry has no `data_width` constraint → still matches.
        let hits = c.query("rtl_protocol", "axi4_slave", &query);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn load_from_directory_picks_up_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let domain_dir = root.join("rtl_protocol");
        std::fs::create_dir_all(&domain_dir).unwrap();
        let entry_body = json!({
            "id": "rtl_protocol/uart_lite",
            "version": "1.0.0",
            "domain": "rtl_protocol",
            "name": "uart_lite",
            "parameters": {"data_bits": 8},
            "provenance": {
                "tier": "mununu_verified",
                "verified_against": "Open Cores UART-Lite reference"
            }
        });
        std::fs::write(
            domain_dir.join("uart_lite@1.0.0.json"),
            serde_json::to_string_pretty(&entry_body).unwrap(),
        )
        .unwrap();
        // Also write a non-JSON file that should be ignored.
        std::fs::write(domain_dir.join("README.md"), "not a corpus file").unwrap();
        let c = Corpus::load(root).unwrap();
        assert_eq!(c.len(), 1);
        let mut q = BTreeMap::new();
        q.insert("data_bits".to_string(), json!(8));
        let hits = c.query("rtl_protocol", "uart_lite", &q);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].version, "1.0.0");
    }

    #[test]
    fn load_returns_io_error_for_missing_root() {
        let res = Corpus::load(Path::new("/definitely/does/not/exist"));
        assert!(matches!(res, Err(CorpusError::Io { .. })));
    }
}
