//! Symptom ledger: patient-reported outcomes as first-class facts.
//!
//! For lab-centric conditions the fact ledger suffices; for symptom-centric
//! conditions (PSSD, PAWS, skin severity) progress lives in daily observation.
//! Scales are pack data (`content-packs/symptoms/*.toml`): single-item 0-10
//! self-reports and multi-item instruments with sum scoring. Adapted
//! instrument wordings are labeled as such — a validated administration
//! requires the original copyrighted item text and is cited, not copied.
//!
//! Symptom logs enter the same provenance discipline as lab facts: patient-
//! scoped rows in the append-only ledger, verifiable by claim id.

use crate::error::AsclepiusError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleItem {
    pub id: String,
    pub prompt: String,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreSpec {
    /// Scoring method: "sum" over items.
    pub kind: String,
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub interpret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymptomScale {
    pub id: String,
    pub name: String,
    /// "single_item" (one prompt, one number) or "instrument" (items + score).
    pub kind: String,
    /// Condition card ids this scale tracks (gate-validated).
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Prompt for single-item scales.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub items: Vec<ScaleItem>,
    #[serde(default)]
    pub score: Option<ScoreSpec>,
    /// True when the wording is adapted (not a validated administration).
    #[serde(default)]
    pub adapted: Option<bool>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

impl SymptomScale {
    /// Validate a value against this scale (and item, for instruments).
    pub fn check_value(&self, item_id: Option<&str>, value: f64) -> Result<(), String> {
        match self.kind.as_str() {
            "single_item" => {
                if item_id.is_some() {
                    return Err(format!("scale {} is single-item: do not pass item_id", self.id));
                }
                let (min, max) = self.single_range();
                if !(min..=max).contains(&value) {
                    return Err(format!("value {value} outside scale range {min}-{max}"));
                }
                Ok(())
            }
            "instrument" => {
                let Some(item) = item_id.and_then(|i| self.items.iter().find(|x| x.id == i)) else {
                    return Err(format!(
                        "scale {} is an instrument: item_id must be one of [{}]",
                        self.id,
                        self.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                };
                if !(item.min..=item.max).contains(&value) {
                    return Err(format!(
                        "value {value} outside item {} range {}-{}",
                        item.id, item.min, item.max
                    ));
                }
                Ok(())
            }
            other => Err(format!("unknown scale kind '{other}'")),
        }
    }

    fn single_range(&self) -> (f64, f64) {
        self.score
            .as_ref()
            .map(|s| (s.min, s.max))
            .unwrap_or((0.0, 10.0))
    }
}

#[derive(Debug, Deserialize)]
struct SymptomFile {
    scale: Vec<SymptomScale>,
}

#[derive(Debug, Clone, Default)]
pub struct SymptomSet {
    scales: Vec<SymptomScale>,
}

impl SymptomSet {
    pub fn from_vec(scales: Vec<SymptomScale>) -> Self {
        Self { scales }
    }

    pub fn load_paths(paths: &[std::path::PathBuf]) -> Result<Self, AsclepiusError> {
        if paths.is_empty() {
            return Ok(Self::default());
        }
        let mut scales = Vec::new();
        for path in paths {
            let raw = std::fs::read_to_string(path)?;
            let parsed: SymptomFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            scales.extend(parsed.scale);
        }
        Ok(Self { scales })
    }

    pub fn load_dir(dir: &Path) -> Result<Self, AsclepiusError> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        Self::load_paths(&paths)
    }

    pub fn all(&self) -> &[SymptomScale] {
        &self.scales
    }

    pub fn get(&self, id: &str) -> Option<&SymptomScale> {
        self.scales.iter().find(|s| s.id == id)
    }
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct SymptomEntryInput {
    pub scale_id: String,
    /// Item id for instrument scales; omit for single-item scales.
    #[serde(default)]
    pub item_id: Option<String>,
    pub value: f64,
    /// YYYY-MM-DD the observation refers to (defaults to today UTC).
    #[serde(default)]
    pub observed_on: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct SymptomLogInput {
    pub entries: Vec<SymptomEntryInput>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct SymptomHistoryInput {
    #[serde(default)]
    pub scale_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single() -> SymptomScale {
        SymptomScale {
            id: "genital_sensation".into(),
            name: "Genital sensation".into(),
            kind: "single_item".into(),
            conditions: vec!["pssd".into()],
            prompt: Some("0 = complete numbness, 10 = normal sensation".into()),
            items: vec![],
            score: Some(ScoreSpec {
                kind: "single".into(),
                min: 0.0,
                max: 10.0,
                interpret: None,
            }),
            adapted: None,
            sources: vec![],
            review_by: None,
        }
    }

    fn instrument() -> SymptomScale {
        SymptomScale {
            id: "iief5_adapted".into(),
            name: "Adapted 5-item erectile function track".into(),
            kind: "instrument".into(),
            conditions: vec!["pssd".into()],
            prompt: None,
            items: vec![
                ScaleItem { id: "q1".into(), prompt: "confidence".into(), min: 1.0, max: 5.0 },
                ScaleItem { id: "q2".into(), prompt: "firmness".into(), min: 1.0, max: 5.0 },
            ],
            score: Some(ScoreSpec {
                kind: "sum".into(),
                min: 2.0,
                max: 10.0,
                interpret: Some("lower = worse".into()),
            }),
            adapted: Some(true),
            sources: vec!["adapted from IIEF-5 domains (Rosen et al. 1999); not a validated administration".into()],
            review_by: None,
        }
    }

    #[test]
    fn single_item_validation() {
        let s = single();
        assert!(s.check_value(None, 7.0).is_ok());
        assert!(s.check_value(None, 11.0).is_err());
        assert!(s.check_value(Some("q1".into()), 7.0).is_err());
    }

    #[test]
    fn instrument_validation() {
        let s = instrument();
        assert!(s.check_value(Some("q1".into()), 4.0).is_ok());
        assert!(s.check_value(Some("q1".into()), 9.0).is_err());
        assert!(s.check_value(Some("ghost".into()), 4.0).is_err());
        assert!(s.check_value(None, 4.0).is_err());
    }
}
