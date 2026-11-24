//! Extraction spec validation — checks line anchors against actual source code.
//!
//! Validates that an extraction spec's state_fields, method guards, and method
//! effects still match the source code at the referenced line numbers. Reports
//! exact matches, drifts (pattern found nearby), and mismatches.

use std::path::Path;

/// Result of validating a single line anchor.
#[derive(Debug, Clone)]
pub enum AnchorResult {
    /// Pattern found exactly at the expected line.
    Exact {
        spec_id: String,
        section: String,
        line: u32,
    },
    /// Pattern found nearby (within drift window).
    Drifted {
        spec_id: String,
        section: String,
        expected_line: u32,
        found_line: u32,
        drift: i32,
    },
    /// Pattern not found at or near the expected line.
    Mismatch {
        spec_id: String,
        section: String,
        expected_line: u32,
        expected_pattern: String,
        actual_at_line: String,
    },
    /// Line number out of range.
    Error {
        spec_id: String,
        section: String,
        message: String,
    },
}

impl AnchorResult {
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            AnchorResult::Exact { .. } | AnchorResult::Drifted { .. }
        )
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            AnchorResult::Mismatch { .. } | AnchorResult::Error { .. }
        )
    }
}

/// An uncovered state field access found in source but not in the spec.
#[derive(Debug, Clone)]
pub struct UncoveredAccess {
    pub line: u32,
    pub field: String,
    pub content: String,
}

/// Summary of validation results.
#[derive(Debug, Clone)]
pub struct ValidationSummary {
    pub total: usize,
    pub exact: usize,
    pub drifted: usize,
    pub mismatch: usize,
    pub error: usize,
    pub uncovered_accesses: usize,
}

/// Full validation report.
pub struct ValidationReport {
    pub anchors: Vec<AnchorResult>,
    pub uncovered: Vec<UncoveredAccess>,
    pub commit_match: Option<bool>,
    pub summary: ValidationSummary,
}

/// Check if a pattern appears at the expected line, or nearby within the drift window.
fn check_anchor(
    source_lines: &[&str],
    line: u32,
    pattern: &str,
    drift_window: usize,
) -> AnchorResult {
    let max_line = source_lines.len() as u32;
    if line < 1 || line > max_line {
        return AnchorResult::Error {
            spec_id: String::new(),
            section: String::new(),
            message: format!("line {line} out of range (1-{max_line})"),
        };
    }

    let actual = source_lines[(line - 1) as usize].trim();
    if actual.contains(pattern) {
        return AnchorResult::Exact {
            spec_id: String::new(),
            section: String::new(),
            line,
        };
    }

    // Search nearby
    for offset in 1..=drift_window {
        for &candidate in &[line as i64 - offset as i64, line as i64 + offset as i64] {
            if candidate >= 1 && candidate <= max_line as i64 {
                let candidate_text = source_lines[(candidate - 1) as usize].trim();
                if candidate_text.contains(pattern) {
                    return AnchorResult::Drifted {
                        spec_id: String::new(),
                        section: String::new(),
                        expected_line: line,
                        found_line: candidate as u32,
                        drift: (candidate as i32) - (line as i32),
                    };
                }
            }
        }
    }

    AnchorResult::Mismatch {
        spec_id: String::new(),
        section: String::new(),
        expected_line: line,
        expected_pattern: pattern.to_string(),
        actual_at_line: actual.to_string(),
    }
}

/// Set the spec_id and section on an AnchorResult.
fn tag(mut result: AnchorResult, spec_id: &str, section: &str) -> AnchorResult {
    match &mut result {
        AnchorResult::Exact {
            spec_id: id,
            section: s,
            ..
        }
        | AnchorResult::Drifted {
            spec_id: id,
            section: s,
            ..
        }
        | AnchorResult::Mismatch {
            spec_id: id,
            section: s,
            ..
        }
        | AnchorResult::Error {
            spec_id: id,
            section: s,
            ..
        } => {
            *id = spec_id.to_string();
            *s = section.to_string();
        }
    }
    result
}

/// Validate an extraction spec against the actual source file.
///
/// Reads the spec JSON and source file, checks every line anchor, and scans
/// for uncovered state field accesses.
pub fn validate_spec(
    spec_json: &str,
    source_path: &Path,
    drift_window: usize,
) -> Result<ValidationReport, String> {
    let source_content = std::fs::read_to_string(source_path).map_err(|e| {
        format!(
            "Failed to read source file '{}': {e}",
            source_path.display()
        )
    })?;

    let mut report = validate_spec_content(spec_json, &source_content, drift_window)?;

    // File-based commit check (only available when source_path is given)
    let spec: super::ast::ExtractionSpec =
        serde_json::from_str(spec_json).map_err(|e| format!("Failed to parse spec: {e}"))?;
    report.commit_match = check_commit_match(&spec, source_path);

    Ok(report)
}

/// Validate an extraction spec against source content (in-memory).
///
/// Like [`validate_spec`] but takes source content as a string instead of
/// a file path. Used by the API endpoint where content is provided directly.
/// Does not check commit match (requires file system access).
pub fn validate_spec_content(
    spec_json: &str,
    source_content: &str,
    drift_window: usize,
) -> Result<ValidationReport, String> {
    let spec: super::ast::ExtractionSpec =
        serde_json::from_str(spec_json).map_err(|e| format!("Failed to parse spec: {e}"))?;

    let source_lines: Vec<&str> = source_content.lines().collect();

    let mut anchors = Vec::new();

    // Validate state_fields
    for field in &spec.state_fields {
        if let (Some(line), Some(pattern)) = (field.line, field.pattern.as_deref()) {
            let result = check_anchor(&source_lines, line, pattern, drift_window);
            anchors.push(tag(result, &field.id, "state_fields"));
        }
    }

    // Validate method guards and effects
    for method in &spec.methods {
        for guard in &method.guards {
            if let Some(obj) = guard.as_object() {
                if obj.contains_key("ref") {
                    continue; // Skip references
                }
                if let (Some(line), Some(pattern)) = (
                    obj.get("line").and_then(|v| v.as_u64()),
                    obj.get("pattern").and_then(|v| v.as_str()),
                ) {
                    let field_id = obj.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                    let result = check_anchor(&source_lines, line as u32, pattern, drift_window);
                    anchors.push(tag(
                        result,
                        &format!("{}.guard.{}", method.id, field_id),
                        "methods.guards",
                    ));
                }
            }
        }

        for effect in &method.effects {
            if let Some(obj) = effect.as_object() {
                if obj.contains_key("ref") {
                    continue;
                }
                if let (Some(line), Some(pattern)) = (
                    obj.get("line").and_then(|v| v.as_u64()),
                    obj.get("pattern").and_then(|v| v.as_str()),
                ) {
                    let field_id = obj.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                    let result = check_anchor(&source_lines, line as u32, pattern, drift_window);
                    anchors.push(tag(
                        result,
                        &format!("{}.effect.{}", method.id, field_id),
                        "methods.effects",
                    ));
                }
            }
        }
    }

    // Scan for uncovered state field accesses
    let covered_lines: std::collections::HashSet<u32> = anchors
        .iter()
        .filter_map(|a| match a {
            AnchorResult::Exact { line, .. } => Some(*line),
            AnchorResult::Drifted { found_line, .. } => Some(*found_line),
            _ => None,
        })
        .collect();

    let field_names: Vec<&str> = spec
        .state_fields
        .iter()
        .filter_map(|f| f.field.as_deref())
        .collect();

    let mut uncovered = Vec::new();
    for (i, line) in source_lines.iter().enumerate() {
        let line_num = (i + 1) as u32;
        for &field_name in &field_names {
            let pattern = format!("this.{field_name}");
            if line.contains(&pattern) && !covered_lines.contains(&line_num) {
                uncovered.push(UncoveredAccess {
                    line: line_num,
                    field: field_name.to_string(),
                    content: line.trim().to_string(),
                });
            }
        }
    }

    // Summary
    let exact = anchors
        .iter()
        .filter(|a| matches!(a, AnchorResult::Exact { .. }))
        .count();
    let drifted = anchors
        .iter()
        .filter(|a| matches!(a, AnchorResult::Drifted { .. }))
        .count();
    let mismatch = anchors
        .iter()
        .filter(|a| matches!(a, AnchorResult::Mismatch { .. }))
        .count();
    let error = anchors
        .iter()
        .filter(|a| matches!(a, AnchorResult::Error { .. }))
        .count();

    Ok(ValidationReport {
        summary: ValidationSummary {
            total: anchors.len(),
            exact,
            drifted,
            mismatch,
            error,
            uncovered_accesses: uncovered.len(),
        },
        anchors,
        uncovered,
        commit_match: None, // Set by validate_spec() when file path is available
    })
}

/// Best-effort check if the source repo HEAD matches the spec's commit.
fn check_commit_match(spec: &super::ast::ExtractionSpec, source_path: &Path) -> Option<bool> {
    let expected = spec.source.commit.as_deref()?;

    // Find git root
    let mut dir = source_path.parent()?;
    loop {
        if dir.join(".git").exists() {
            break;
        }
        dir = dir.parent()?;
    }

    let mut git = std::process::Command::new("git");
    git.args(["rev-parse", "HEAD"]).current_dir(dir);
    // mununu#1196 — through the adapter's one waiting helper, so a cancelled run ends it.
    let (_status, stdout, _stderr) = crate::adapter::run_to_end(&mut git).ok()?;

    let actual = stdout.trim().to_string();
    Some(actual.starts_with(expected) || expected.starts_with(&actual))
}

/// Check provenance headers in a CTXDSL file.
pub fn check_provenance(content: &str) -> ProvenanceInfo {
    let mut info = ProvenanceInfo::default();
    for line in content.lines().take(20) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("// @generated-from:") {
            info.generated_from = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("// @model-source:") {
            info.model_source = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("// @spec:") {
            info.spec_path = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("// @commit:") {
            info.commit = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("// @mode:") {
            info.mode = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("// @source-file:") {
            info.source_file = Some(rest.trim().to_string());
        }
    }
    info
}

/// Parsed provenance information from CTXDSL file headers.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceInfo {
    pub generated_from: Option<String>,
    pub model_source: Option<String>,
    pub spec_path: Option<String>,
    pub commit: Option<String>,
    pub mode: Option<String>,
    pub source_file: Option<String>,
}

impl ProvenanceInfo {
    pub fn is_generated(&self) -> bool {
        self.generated_from.is_some()
    }

    pub fn is_specification_model(&self) -> bool {
        self.model_source.as_deref() == Some("specification")
    }

    pub fn has_any_header(&self) -> bool {
        self.generated_from.is_some() || self.model_source.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_anchor_exact() {
        let lines = vec!["line 1", "private _started: boolean = false", "line 3"];
        let result = check_anchor(&lines, 2, "private _started: boolean = false", 5);
        assert!(matches!(result, AnchorResult::Exact { line: 2, .. }));
    }

    #[test]
    fn check_anchor_drift() {
        let lines = vec!["line 1", "line 2", "line 3", "the pattern here", "line 5"];
        let result = check_anchor(&lines, 2, "the pattern here", 5);
        assert!(matches!(
            result,
            AnchorResult::Drifted {
                expected_line: 2,
                found_line: 4,
                drift: 2,
                ..
            }
        ));
    }

    #[test]
    fn check_anchor_mismatch() {
        let lines = vec!["line 1", "something else", "line 3"];
        let result = check_anchor(&lines, 2, "not found anywhere", 5);
        assert!(matches!(result, AnchorResult::Mismatch { .. }));
    }

    #[test]
    fn check_provenance_generated() {
        let content = r#"// @generated-from: extraction_spec_v1
// @spec: tools/extraction_specs/mcp.json
// @commit: abc123
// @mode: vulnerable
context test {"#;
        let info = check_provenance(content);
        assert!(info.is_generated());
        assert!(!info.is_specification_model());
        assert_eq!(info.generated_from.as_deref(), Some("extraction_spec_v1"));
        assert_eq!(info.commit.as_deref(), Some("abc123"));
        assert_eq!(info.mode.as_deref(), Some("vulnerable"));
    }

    #[test]
    fn check_provenance_specification() {
        let content = r#"// @model-source: specification
// @specification: 3GPP TS 38.331
// @not-extracted-from-source
context test {"#;
        let info = check_provenance(content);
        assert!(!info.is_generated());
        assert!(info.is_specification_model());
    }

    #[test]
    fn check_provenance_none() {
        let content = "// Some regular comment\ncontext test {";
        let info = check_provenance(content);
        assert!(!info.has_any_header());
    }
}
