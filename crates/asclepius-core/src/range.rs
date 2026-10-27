use crate::analyte::{AnalyteDef, AnalyteRegistry};
use crate::error::AsclepiusError;
use crate::profile::Sex;
use crate::units::{Unit, convert};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeKind {
    #[serde(rename = "population")]
    Population,
    #[serde(rename = "functional")]
    Functional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SexScope {
    #[serde(rename = "male")]
    Male,
    #[serde(rename = "female")]
    Female,
    #[default]
    #[serde(rename = "any")]
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    #[serde(rename = "high")]
    High,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "low")]
    Low,
}

fn default_confidence() -> Confidence {
    Confidence::Medium
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeDef {
    pub analyte: String,
    pub kind: RangeKind,
    #[serde(default)]
    pub sex: SexScope,
    #[serde(default)]
    pub age_min: Option<u32>,
    #[serde(default)]
    pub age_max: Option<u32>,
    pub low: f64,
    pub high: f64,
    pub unit: Unit,
    pub source: String,
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeStatus {
    #[serde(rename = "below")]
    Below,
    #[serde(rename = "within")]
    Within,
    #[serde(rename = "above")]
    Above,
}

#[derive(Debug, Clone, Serialize)]
pub struct RangeAssessment {
    pub kind: RangeKind,
    pub low: f64,
    pub high: f64,
    pub unit: Unit,
    pub status: RangeStatus,
    pub source: String,
    pub pmid: Option<String>,
    pub confidence: Confidence,
    pub review_by: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedRange {
    def: RangeDef,
    canonical_unit: Unit,
    low_canonical: f64,
    high_canonical: f64,
}

#[derive(Debug, Clone, Default)]
pub struct RangeSet {
    prepared: Vec<PreparedRange>,
}

#[derive(Debug, Deserialize)]
struct RangeFile {
    range: Vec<RangeDef>,
}

fn prepare(def: RangeDef, analyte: &AnalyteDef) -> Result<PreparedRange, AsclepiusError> {
    let mw = analyte.molar_mass;
    let low_canonical = convert(def.low, def.unit, analyte.canonical_unit, mw).map_err(|_| {
        AsclepiusError::IncompatibleUnit {
            analyte: def.analyte.clone(),
            from: def.unit.label().to_string(),
            canonical: analyte.canonical_unit.label().to_string(),
        }
    })?;
    let high_canonical = convert(def.high, def.unit, analyte.canonical_unit, mw).map_err(|_| {
        AsclepiusError::IncompatibleUnit {
            analyte: def.analyte.clone(),
            from: def.unit.label().to_string(),
            canonical: analyte.canonical_unit.label().to_string(),
        }
    })?;
    Ok(PreparedRange {
        def,
        canonical_unit: analyte.canonical_unit,
        low_canonical,
        high_canonical,
    })
}

impl RangeSet {
    pub fn from_vec(
        defs: Vec<RangeDef>,
        registry: &AnalyteRegistry,
    ) -> Result<Self, AsclepiusError> {
        let mut prepared = Vec::with_capacity(defs.len());
        for def in defs {
            let analyte = registry
                .get(&def.analyte)
                .ok_or_else(|| AsclepiusError::UnknownAnalyte(def.analyte.clone()))?;
            prepared.push(prepare(def, analyte)?);
        }
        Ok(Self { prepared })
    }

    pub fn load_dir(dir: &Path, registry: &AnalyteRegistry) -> Result<Self, AsclepiusError> {
        let mut defs = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        entries.sort();
        if entries.is_empty() {
            return Err(AsclepiusError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no .toml range packs found in {}", dir.display()),
            )));
        }
        for path in entries {
            let raw = std::fs::read_to_string(&path)?;
            let parsed: RangeFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            defs.extend(parsed.range);
        }
        Self::from_vec(defs, registry)
    }

    pub fn assess(
        &self,
        analyte_id: &str,
        sex: Option<Sex>,
        age: Option<u32>,
        canonical_value: f64,
    ) -> Vec<RangeAssessment> {
        self.prepared
            .iter()
            .filter(|p| p.def.analyte == analyte_id)
            .filter(|p| match (p.def.sex, sex) {
                (SexScope::Any, _) => true,
                (scope, Some(s)) => {
                    (scope == SexScope::Male && s == Sex::Male)
                        || (scope == SexScope::Female && s == Sex::Female)
                }
                (_, None) => false,
            })
            .filter(|p| match (p.def.age_min, p.def.age_max, age) {
                (None, None, _) => true,
                (_, _, None) => false,
                (min, max, Some(a)) => min.is_none_or(|m| a >= m) && max.is_none_or(|m| a <= m),
            })
            .map(|p| RangeAssessment {
                kind: p.def.kind,
                low: p.low_canonical,
                high: p.high_canonical,
                unit: p.canonical_unit,
                status: if canonical_value < p.low_canonical {
                    RangeStatus::Below
                } else if canonical_value > p.high_canonical {
                    RangeStatus::Above
                } else {
                    RangeStatus::Within
                },
                source: p.def.source.clone(),
                pmid: p.def.pmid.clone(),
                confidence: p.def.confidence,
                review_by: p.def.review_by.clone(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AnalyteRegistry {
        AnalyteRegistry::from_vec(vec![AnalyteDef {
            id: "tsh".into(),
            name: "Thyroid Stimulating Hormone".into(),
            aliases: vec!["thyrotropin".into()],
            loinc: None,
            molar_mass: None,
            canonical_unit: Unit::MIuL,
            plausible_min: 0.01,
            plausible_max: 100.0,
        }])
    }

    fn ranges() -> RangeSet {
        RangeSet::from_vec(
            vec![
                RangeDef {
                    analyte: "tsh".into(),
                    kind: RangeKind::Population,
                    sex: SexScope::Any,
                    age_min: None,
                    age_max: None,
                    low: 0.4,
                    high: 4.5,
                    unit: Unit::MIuL,
                    source: "classic lab range".into(),
                    pmid: None,
                    confidence: Confidence::High,
                    review_by: None,
                },
                RangeDef {
                    analyte: "tsh".into(),
                    kind: RangeKind::Functional,
                    sex: SexScope::Any,
                    age_min: None,
                    age_max: None,
                    low: 1.0,
                    high: 2.5,
                    unit: Unit::MIuL,
                    source: "integrative seed".into(),
                    pmid: None,
                    confidence: Confidence::Medium,
                    review_by: None,
                },
            ],
            &registry(),
        )
        .unwrap()
    }

    #[test]
    fn dual_range_assessment() {
        let rs = ranges();
        let a = rs.assess("tsh", None, Some(30), 3.0);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].status, RangeStatus::Within);
        assert_eq!(a[1].status, RangeStatus::Above);
    }

    #[test]
    fn unit_conversion_on_ranges() {
        let reg = AnalyteRegistry::from_vec(vec![AnalyteDef {
            id: "vitamin_d_25oh".into(),
            name: "25-Hydroxy Vitamin D".into(),
            aliases: vec![],
            loinc: None,
            molar_mass: Some(400.64),
            canonical_unit: Unit::NgMl,
            plausible_min: 1.0,
            plausible_max: 200.0,
        }]);
        let rs = RangeSet::from_vec(
            vec![RangeDef {
                analyte: "vitamin_d_25oh".into(),
                kind: RangeKind::Population,
                sex: SexScope::Any,
                age_min: None,
                age_max: None,
                low: 75.0,
                high: 250.0,
                unit: Unit::NmolL,
                source: "SI source".into(),
                pmid: None,
                confidence: Confidence::High,
                review_by: None,
            }],
            &reg,
        )
        .unwrap();
        let a = rs.assess("vitamin_d_25oh", None, None, 45.0);
        assert_eq!(a.len(), 1);
        assert!((a[0].low - 30.0).abs() < 0.3, "low was {}", a[0].low);
        assert!((a[0].high - 100.0).abs() < 1.0, "high was {}", a[0].high);
        assert_eq!(a[0].status, RangeStatus::Within);
    }

    #[test]
    fn unknown_analyte_in_range_pack_rejected() {
        let e = RangeSet::from_vec(
            vec![RangeDef {
                analyte: "nope".into(),
                kind: RangeKind::Population,
                sex: SexScope::Any,
                age_min: None,
                age_max: None,
                low: 1.0,
                high: 2.0,
                unit: Unit::Ratio,
                source: "x".into(),
                pmid: None,
                confidence: Confidence::Low,
                review_by: None,
            }],
            &registry(),
        );
        assert!(e.is_err());
    }
}
