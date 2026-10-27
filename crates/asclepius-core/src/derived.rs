use crate::analyte::AnalyteRegistry;
use crate::error::AsclepiusError;
use crate::units::{Unit, convert};
use serde::{Deserialize, Serialize};

pub const VERMEULEN_K_SHBG: f64 = 1.0e9;
pub const VERMEULEN_K_ALB_PER_GDL: f64 = 1.0e6 / 66500.0;
pub const DEFAULT_ALBUMIN_GDL: f64 = 4.3;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DerivedValue {
    pub id: String,
    pub name: String,
    pub value: f64,
    pub unit: Unit,
    pub formula_version: String,
    pub inputs: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct FreeTInput {
    pub total_testosterone: f64,
    #[serde(default)]
    pub total_testosterone_unit: Option<String>,
    pub shbg: f64,
    #[serde(default)]
    pub shbg_unit: Option<String>,
    #[serde(default)]
    pub albumin: Option<f64>,
    #[serde(default)]
    pub albumin_unit: Option<String>,
}

fn parse_unit(raw: &Option<String>) -> Result<Option<Unit>, AsclepiusError> {
    match raw {
        None => Ok(None),
        Some(s) => Unit::parse(s)
            .map(Some)
            .ok_or_else(|| AsclepiusError::UnknownUnit(s.clone())),
    }
}

fn to_target(
    value: f64,
    unit: Option<Unit>,
    analyte_id: &str,
    target: Unit,
    registry: &AnalyteRegistry,
) -> Result<f64, AsclepiusError> {
    let analyte = registry
        .get(analyte_id)
        .ok_or_else(|| AsclepiusError::UnknownAnalyte(analyte_id.to_string()))?;
    let resolved = unit.unwrap_or(analyte.canonical_unit);
    if resolved == target {
        return Ok(value);
    }
    convert(value, resolved, target, analyte.molar_mass).map_err(|_| {
        AsclepiusError::IncompatibleUnit {
            analyte: analyte_id.to_string(),
            from: resolved.label().to_string(),
            canonical: target.label().to_string(),
        }
    })
}

pub fn free_testosterone_vermeulen(
    registry: &AnalyteRegistry,
    input: &FreeTInput,
) -> Result<DerivedValue, AsclepiusError> {
    let tt_unit = parse_unit(&input.total_testosterone_unit)?;
    let shbg_unit = parse_unit(&input.shbg_unit)?;
    let alb_unit = parse_unit(&input.albumin_unit)?;

    let tt_nmoll = to_target(
        input.total_testosterone,
        tt_unit,
        "testosterone_total",
        Unit::NmolL,
        registry,
    )?;
    let shbg_nmoll = to_target(input.shbg, shbg_unit, "shbg", Unit::NmolL, registry)?;
    let alb_gdl = match input.albumin {
        None => DEFAULT_ALBUMIN_GDL,
        Some(v) => to_target(v, alb_unit, "albumin", Unit::GDl, registry)?,
    };

    if shbg_nmoll <= 0.0 || tt_nmoll < 0.0 || alb_gdl <= 0.0 {
        return Err(AsclepiusError::ImplausibleValue {
            analyte: "free_testosterone_vermeulen inputs".into(),
            value: tt_nmoll,
            min: 0.0,
            max: f64::MAX,
            unit: "nmol/L".into(),
        });
    }

    let denominator = 1.0 + shbg_nmoll + VERMEULEN_K_ALB_PER_GDL * alb_gdl;
    let free_nmoll = tt_nmoll / denominator;
    let free_ngdl = convert(
        free_nmoll,
        Unit::NmolL,
        Unit::NgDl,
        registry
            .get("testosterone_total")
            .and_then(|a| a.molar_mass),
    )
    .unwrap_or(free_nmoll);

    Ok(DerivedValue {
        id: "free_testosterone_vermeulen".into(),
        name: "Calculated Free Testosterone (Vermeulen)".into(),
        value: free_ngdl,
        unit: Unit::NgDl,
        formula_version: "vermeulen1999".into(),
        inputs: serde_json::json!({
            "total_testosterone_nmol_l": tt_nmoll,
            "shbg_nmol_l": shbg_nmoll,
            "albumin_g_dl": alb_gdl,
            "albumin_assumed": input.albumin.is_none(),
            "k_shbg_l_per_mol": VERMEULEN_K_SHBG,
            "k_albumin_per_g_dl": VERMEULEN_K_ALB_PER_GDL,
        }),
    })
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct HomaInput {
    pub glucose: f64,
    #[serde(default)]
    pub glucose_unit: Option<String>,
    pub insulin: f64,
    #[serde(default)]
    pub insulin_unit: Option<String>,
}

pub fn homa_ir(
    registry: &AnalyteRegistry,
    input: &HomaInput,
) -> Result<DerivedValue, AsclepiusError> {
    let glu_unit = parse_unit(&input.glucose_unit)?;
    let ins_unit = parse_unit(&input.insulin_unit)?;

    let glucose_mgdl = to_target(input.glucose, glu_unit, "glucose", Unit::MgDl, registry)?;
    let insulin_uuml = to_target(input.insulin, ins_unit, "insulin", Unit::UuMl, registry)?;

    if glucose_mgdl <= 0.0 || insulin_uuml < 0.0 {
        return Err(AsclepiusError::ImplausibleValue {
            analyte: "homa_ir inputs".into(),
            value: glucose_mgdl,
            min: 0.0,
            max: f64::MAX,
            unit: "mg/dL".into(),
        });
    }

    let homa = glucose_mgdl * insulin_uuml / 405.0;

    Ok(DerivedValue {
        id: "homa_ir".into(),
        name: "HOMA-IR (fasting insulin resistance)".into(),
        value: homa,
        unit: Unit::Ratio,
        formula_version: "matthews1985".into(),
        inputs: serde_json::json!({
            "glucose_mg_dl": glucose_mgdl,
            "insulin_uu_ml": insulin_uuml,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyte::AnalyteDef;

    fn registry() -> AnalyteRegistry {
        AnalyteRegistry::from_vec(vec![
            AnalyteDef {
                id: "testosterone_total".into(),
                name: "Total Testosterone".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: Some(288.42),
                canonical_unit: Unit::NgDl,
                plausible_min: 20.0,
                plausible_max: 2000.0,
            },
            AnalyteDef {
                id: "shbg".into(),
                name: "Sex Hormone Binding Globulin".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::NmolL,
                plausible_min: 1.0,
                plausible_max: 300.0,
            },
            AnalyteDef {
                id: "albumin".into(),
                name: "Albumin".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::GDl,
                plausible_min: 1.0,
                plausible_max: 6.5,
            },
            AnalyteDef {
                id: "glucose".into(),
                name: "Glucose".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: Some(180.156),
                canonical_unit: Unit::MgDl,
                plausible_min: 30.0,
                plausible_max: 600.0,
            },
            AnalyteDef {
                id: "insulin".into(),
                name: "Insulin".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::UuMl,
                plausible_min: 0.1,
                plausible_max: 300.0,
            },
        ])
    }

    #[test]
    fn vermeulen_worked_example() {
        let r = registry();
        let out = free_testosterone_vermeulen(
            &r,
            &FreeTInput {
                total_testosterone: 500.0,
                total_testosterone_unit: Some("ng/dL".into()),
                shbg: 30.0,
                shbg_unit: Some("nmol/L".into()),
                albumin: Some(4.3),
                albumin_unit: Some("g/dL".into()),
            },
        )
        .unwrap();
        assert!(
            (out.value - 5.2).abs() < 0.3,
            "free T was {} ng/dL",
            out.value
        );
    }

    #[test]
    fn vermeulen_si_input_matches_us_input() {
        let r = registry();
        let si = free_testosterone_vermeulen(
            &r,
            &FreeTInput {
                total_testosterone: 17.34,
                total_testosterone_unit: Some("nmol/L".into()),
                shbg: 30.0,
                shbg_unit: None,
                albumin: None,
                albumin_unit: None,
            },
        )
        .unwrap();
        let us = free_testosterone_vermeulen(
            &r,
            &FreeTInput {
                total_testosterone: 500.0,
                total_testosterone_unit: Some("ng/dL".into()),
                shbg: 30.0,
                shbg_unit: None,
                albumin: Some(43.0),
                albumin_unit: Some("g/L".into()),
            },
        )
        .unwrap();
        assert!(
            (si.value - us.value).abs() < 0.05,
            "si {} vs us {}",
            si.value,
            us.value
        );
    }

    #[test]
    fn homa_worked_example() {
        let r = registry();
        let out = homa_ir(
            &r,
            &HomaInput {
                glucose: 90.0,
                glucose_unit: Some("mg/dL".into()),
                insulin: 8.0,
                insulin_unit: Some("uU/mL".into()),
            },
        )
        .unwrap();
        assert!((out.value - 1.7778).abs() < 0.01, "homa was {}", out.value);
    }

    #[test]
    fn homa_si_glucose_matches() {
        let r = registry();
        let si = homa_ir(
            &r,
            &HomaInput {
                glucose: 4.996,
                glucose_unit: Some("mmol/L".into()),
                insulin: 8.0,
                insulin_unit: None,
            },
        )
        .unwrap();
        let us = homa_ir(
            &r,
            &HomaInput {
                glucose: 90.0,
                glucose_unit: Some("mg/dL".into()),
                insulin: 8.0,
                insulin_unit: None,
            },
        )
        .unwrap();
        assert!((si.value - us.value).abs() < 0.02);
    }
}
