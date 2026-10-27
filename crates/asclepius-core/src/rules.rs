use crate::analyte::AnalyteRegistry;
use crate::error::AsclepiusError;
use crate::range::{Confidence, RangeAssessment, RangeKind, RangeStatus};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    #[serde(rename = "note")]
    Note,
    #[serde(rename = "attention")]
    Attention,
    #[serde(rename = "flag")]
    Flag,
}

fn default_confidence() -> Confidence {
    Confidence::Medium
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleCondition {
    pub analyte: String,
    pub kind: RangeKind,
    pub status: RangeStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub id: String,
    pub description: String,
    pub severity: Severity,
    pub conditions: Vec<RuleCondition>,
    #[serde(default)]
    pub suggests: Vec<String>,
    #[serde(default)]
    pub relates_to: Vec<String>,
    pub source: String,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub summary: String,
    pub suggests: Vec<String>,
    pub relates_to: Vec<String>,
    pub source: String,
    pub confidence: Confidence,
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatestValue {
    pub analyte: String,
    pub value: f64,
    pub assessments: Vec<RangeAssessment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interpretation {
    pub findings: Vec<Finding>,
    pub untested_suggestions: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    rule: Vec<Rule>,
}

impl RuleSet {
    pub fn from_vec(rules: Vec<Rule>, registry: &AnalyteRegistry) -> Result<Self, AsclepiusError> {
        for rule in &rules {
            for cond in &rule.conditions {
                if registry.get(&cond.analyte).is_none() {
                    return Err(AsclepiusError::UnknownAnalyte(format!(
                        "rule {} references unknown analyte {}",
                        rule.id, cond.analyte
                    )));
                }
            }
        }
        Ok(Self { rules })
    }

    pub fn load_dir(dir: &Path, registry: &AnalyteRegistry) -> Result<Self, AsclepiusError> {
        let mut rules = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        entries.sort();
        if entries.is_empty() {
            return Err(AsclepiusError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no .toml rule packs found in {}", dir.display()),
            )));
        }
        for path in entries {
            let raw = std::fs::read_to_string(&path)?;
            let parsed: RuleFile = toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                file: path.display().to_string(),
                source,
            })?;
            rules.extend(parsed.rule);
        }
        Self::from_vec(rules, registry)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn all(&self) -> &[Rule] {
        &self.rules
    }
}

fn condition_met(cond: &RuleCondition, latest: &[LatestValue]) -> bool {
    latest.iter().any(|lv| {
        lv.analyte == cond.analyte
            && lv
                .assessments
                .iter()
                .any(|a| a.kind == cond.kind && a.status == cond.status)
    })
}

pub fn interpret(rules: &RuleSet, latest: &[LatestValue]) -> Interpretation {
    let mut findings = Vec::new();
    let mut untested: Vec<String> = Vec::new();
    for rule in &rules.rules {
        if rule.conditions.is_empty() {
            continue;
        }
        if !rule.conditions.iter().all(|c| condition_met(c, latest)) {
            continue;
        }
        for suggest in &rule.suggests {
            if !latest.iter().any(|lv| lv.analyte == *suggest) && !untested.contains(suggest) {
                untested.push(suggest.clone());
            }
        }
        findings.push(Finding {
            rule_id: rule.id.clone(),
            severity: rule.severity,
            summary: rule.description.clone(),
            suggests: rule.suggests.clone(),
            relates_to: rule.relates_to.clone(),
            source: rule.source.clone(),
            confidence: rule.confidence,
            review_by: rule.review_by.clone(),
        });
    }
    Interpretation {
        findings,
        untested_suggestions: untested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assessment(kind: RangeKind, status: RangeStatus) -> RangeAssessment {
        RangeAssessment {
            kind,
            low: 0.0,
            high: 1.0,
            unit: crate::units::Unit::Ratio,
            status,
            source: "t".into(),
            pmid: None,
            confidence: Confidence::High,
            review_by: None,
        }
    }

    fn latest(analyte: &str, kinds: &[(RangeKind, RangeStatus)]) -> LatestValue {
        LatestValue {
            analyte: analyte.into(),
            value: 1.0,
            assessments: kinds.iter().map(|(k, s)| assessment(*k, *s)).collect(),
        }
    }

    fn rules() -> RuleSet {
        RuleSet {
            rules: vec![
                Rule {
                    id: "r_single".into(),
                    description: "single condition".into(),
                    severity: Severity::Note,
                    conditions: vec![RuleCondition {
                        analyte: "tsh".into(),
                        kind: RangeKind::Functional,
                        status: RangeStatus::Above,
                    }],
                    suggests: vec!["ferritin".into()],
                    relates_to: vec![],
                    source: "t".into(),
                    confidence: Confidence::High,
                    review_by: None,
                },
                Rule {
                    id: "r_and".into(),
                    description: "two conditions anded".into(),
                    severity: Severity::Attention,
                    conditions: vec![
                        RuleCondition {
                            analyte: "tsh".into(),
                            kind: RangeKind::Functional,
                            status: RangeStatus::Above,
                        },
                        RuleCondition {
                            analyte: "ferritin".into(),
                            kind: RangeKind::Functional,
                            status: RangeStatus::Below,
                        },
                    ],
                    suggests: vec!["crp".into()],
                    relates_to: vec![],
                    source: "t".into(),
                    confidence: Confidence::High,
                    review_by: None,
                },
            ],
        }
    }

    #[test]
    fn fires_single_and_and_rules() {
        let latest = vec![
            latest("tsh", &[(RangeKind::Functional, RangeStatus::Above)]),
            latest("ferritin", &[(RangeKind::Functional, RangeStatus::Below)]),
        ];
        let out = interpret(&rules(), &latest);
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.untested_suggestions, vec!["crp"]);
    }

    #[test]
    fn and_rule_requires_all_conditions() {
        let latest = vec![latest(
            "tsh",
            &[(RangeKind::Functional, RangeStatus::Above)],
        )];
        let out = interpret(&rules(), &latest);
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].rule_id, "r_single");
        assert_eq!(out.untested_suggestions, vec!["ferritin"]);
    }

    #[test]
    fn wrong_status_does_not_fire() {
        let latest = vec![latest(
            "tsh",
            &[(RangeKind::Population, RangeStatus::Above)],
        )];
        let out = interpret(&rules(), &latest);
        assert!(out.findings.is_empty());
        assert!(out.untested_suggestions.is_empty());
    }
}
