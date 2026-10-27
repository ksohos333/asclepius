use crate::analyte::AnalyteRegistry;
use crate::error::AsclepiusError;
use crate::profile::Profile;
use crate::range::{RangeAssessment, RangeSet};
use crate::units::{Unit, convert};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct LabValueInput {
    pub analyte: String,
    pub value: f64,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct PanelInput {
    pub panel: Vec<LabValueInput>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct CommitInput {
    pub panel: Vec<LabValueInput>,
    #[serde(default)]
    pub collected_at: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyteResult {
    pub analyte_id: String,
    pub display_name: String,
    pub input_value: f64,
    pub input_unit: Option<String>,
    pub canonical_value: f64,
    pub canonical_unit: Unit,
    pub assessments: Vec<RangeAssessment>,
    pub warnings: Vec<String>,
}

pub fn normalize_value(
    registry: &AnalyteRegistry,
    ranges: &RangeSet,
    profile: &Profile,
    input: &LabValueInput,
) -> Result<AnalyteResult, AsclepiusError> {
    let analyte = registry
        .resolve(&input.analyte)
        .ok_or_else(|| AsclepiusError::UnknownAnalyte(input.analyte.clone()))?;

    let mut warnings = Vec::new();

    let parsed_unit = match &input.unit {
        None => None,
        Some(raw) => match Unit::parse(raw) {
            Some(u) => Some(u),
            None => return Err(AsclepiusError::UnknownUnit(raw.clone())),
        },
    };

    let canonical_value =
        match parsed_unit {
            None => input.value,
            Some(u) if u == analyte.canonical_unit => input.value,
            Some(u) => convert(input.value, u, analyte.canonical_unit, analyte.molar_mass)
                .map_err(|_| AsclepiusError::IncompatibleUnit {
                    analyte: analyte.id.clone(),
                    from: u.label().to_string(),
                    canonical: analyte.canonical_unit.label().to_string(),
                })?,
        };

    if canonical_value < analyte.plausible_min || canonical_value > analyte.plausible_max {
        return Err(AsclepiusError::ImplausibleValue {
            analyte: analyte.id.clone(),
            value: canonical_value,
            min: analyte.plausible_min,
            max: analyte.plausible_max,
            unit: analyte.canonical_unit.label().to_string(),
        });
    }

    if canonical_value == 0.0 || !canonical_value.is_finite() {
        return Err(AsclepiusError::ImplausibleValue {
            analyte: analyte.id.clone(),
            value: canonical_value,
            min: analyte.plausible_min,
            max: analyte.plausible_max,
            unit: analyte.canonical_unit.label().to_string(),
        });
    }

    let edge_span = analyte.plausible_max - analyte.plausible_min;
    let near_low = canonical_value - analyte.plausible_min;
    let near_high = analyte.plausible_max - canonical_value;
    if near_low / edge_span < 0.05 {
        warnings.push(format!(
            "value is within 5% of the plausibility floor for {}",
            analyte.id
        ));
    } else if near_high / edge_span < 0.05 {
        warnings.push(format!(
            "value is within 5% of the plausibility ceiling for {}",
            analyte.id
        ));
    }

    let assessments = ranges.assess(&analyte.id, profile.sex, profile.age_years, canonical_value);

    if assessments.is_empty() {
        warnings.push(format!(
            "no reference range loaded matches {} for this profile (sex/age scope)",
            analyte.id
        ));
    }

    Ok(AnalyteResult {
        analyte_id: analyte.id.clone(),
        display_name: analyte.name.clone(),
        input_value: input.value,
        input_unit: input.unit.clone(),
        canonical_value,
        canonical_unit: analyte.canonical_unit,
        assessments,
        warnings,
    })
}

pub fn normalize_panel(
    registry: &AnalyteRegistry,
    ranges: &RangeSet,
    profile: &Profile,
    inputs: &[LabValueInput],
) -> Vec<Result<AnalyteResult, AsclepiusError>> {
    inputs
        .iter()
        .map(|input| normalize_value(registry, ranges, profile, input))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyte::AnalyteDef;
    use crate::range::{Confidence, RangeDef, RangeKind, SexScope};

    fn registry() -> AnalyteRegistry {
        AnalyteRegistry::from_vec(vec![
            AnalyteDef {
                id: "testosterone_total".into(),
                name: "Total Testosterone".into(),
                aliases: vec!["total t".into()],
                loinc: None,
                molar_mass: Some(288.42),
                canonical_unit: Unit::NgDl,
                plausible_min: 20.0,
                plausible_max: 2000.0,
            },
            AnalyteDef {
                id: "tsh".into(),
                name: "Thyroid Stimulating Hormone".into(),
                aliases: vec!["thyrotropin".into()],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::MIuL,
                plausible_min: 0.01,
                plausible_max: 100.0,
            },
        ])
    }

    fn ranges() -> RangeSet {
        RangeSet::from_vec(
            vec![RangeDef {
                analyte: "testosterone_total".into(),
                kind: RangeKind::Population,
                sex: SexScope::Male,
                age_min: Some(19),
                age_max: Some(39),
                low: 264.0,
                high: 916.0,
                unit: Unit::NgDl,
                source: "Bhasin et al. JCEM 2011".into(),
                pmid: None,
                confidence: Confidence::High,
                review_by: None,
            }],
            &registry(),
        )
        .unwrap()
    }

    fn profile() -> Profile {
        Profile {
            name: Some("test".into()),
            age_years: Some(30),
            sex: Some(crate::profile::Sex::Male),
            ..Profile::placeholder()
        }
    }

    #[test]
    fn happy_path_with_conversion() {
        let reg = registry();
        let rs = ranges();
        let r = normalize_value(
            &reg,
            &rs,
            &profile(),
            &LabValueInput {
                analyte: "Total T".into(),
                value: 17.34,
                unit: Some("nmol/L".into()),
            },
        )
        .unwrap();
        assert!(
            (r.canonical_value - 500.0).abs() < 1.0,
            "got {}",
            r.canonical_value
        );
        assert_eq!(r.canonical_unit, Unit::NgDl);
        assert_eq!(r.assessments.len(), 1);
        assert_eq!(r.assessments[0].status, crate::range::RangeStatus::Within);
    }

    #[test]
    fn implausible_rejected() {
        let e = normalize_value(
            &registry(),
            &ranges(),
            &profile(),
            &LabValueInput {
                analyte: "testosterone_total".into(),
                value: 30000.0,
                unit: Some("ng/dL".into()),
            },
        );
        assert!(matches!(e, Err(AsclepiusError::ImplausibleValue { .. })));
    }

    #[test]
    fn unknown_analyte_rejected() {
        let e = normalize_value(
            &registry(),
            &ranges(),
            &profile(),
            &LabValueInput {
                analyte: "unicorn_horn".into(),
                value: 1.0,
                unit: None,
            },
        );
        assert!(matches!(e, Err(AsclepiusError::UnknownAnalyte(_))));
    }

    #[test]
    fn bad_unit_rejected() {
        let e = normalize_value(
            &registry(),
            &ranges(),
            &profile(),
            &LabValueInput {
                analyte: "tsh".into(),
                value: 2.0,
                unit: Some("furlongs".into()),
            },
        );
        assert!(matches!(e, Err(AsclepiusError::UnknownUnit(_))));
    }

    #[test]
    fn incompatible_dimension_rejected() {
        let e = normalize_value(
            &registry(),
            &ranges(),
            &profile(),
            &LabValueInput {
                analyte: "tsh".into(),
                value: 2.0,
                unit: Some("ng/dL".into()),
            },
        );
        assert!(matches!(e, Err(AsclepiusError::IncompatibleUnit { .. })));
    }

    #[test]
    fn panel_collects_mixed_results() {
        let inputs = vec![
            LabValueInput {
                analyte: "testosterone_total".into(),
                value: 600.0,
                unit: Some("ng/dL".into()),
            },
            LabValueInput {
                analyte: "nope".into(),
                value: 1.0,
                unit: None,
            },
        ];
        let out = normalize_panel(&registry(), &ranges(), &profile(), &inputs);
        assert_eq!(out.len(), 2);
        assert!(out[0].is_ok());
        assert!(out[1].is_err());
    }
}
