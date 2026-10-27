use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Unit {
    #[serde(rename = "ng_dl")]
    NgDl,
    #[serde(rename = "ng_ml")]
    NgMl,
    #[serde(rename = "ug_dl")]
    UgDl,
    #[serde(rename = "ug_ml")]
    UgMl,
    #[serde(rename = "mg_dl")]
    MgDl,
    #[serde(rename = "g_dl")]
    GDl,
    #[serde(rename = "g_l")]
    GL,
    #[serde(rename = "nmol_l")]
    NmolL,
    #[serde(rename = "pmol_l")]
    PmolL,
    #[serde(rename = "umol_l")]
    UmolL,
    #[serde(rename = "mmol_l")]
    MmolL,
    #[serde(rename = "miu_l")]
    MIuL,
    #[serde(rename = "uu_ml")]
    UuMl,
    #[serde(rename = "iu_l")]
    IuL,
    #[serde(rename = "percent")]
    Percent,
    #[serde(rename = "ratio")]
    Ratio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum UnitError {
    #[error("cannot convert {from} to {to} without a molar mass")]
    NeedsMolarMass { from: Unit, to: Unit },
    #[error("cannot convert between incompatible dimensions: {from} and {to}")]
    Incompatible { from: Unit, to: Unit },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Mass,
    Molar,
    Activity,
    Fraction,
    Ratio,
}

impl Unit {
    pub const fn dimension(self) -> Dimension {
        match self {
            Unit::NgDl
            | Unit::NgMl
            | Unit::UgDl
            | Unit::UgMl
            | Unit::MgDl
            | Unit::GDl
            | Unit::GL => Dimension::Mass,
            Unit::NmolL | Unit::PmolL | Unit::UmolL | Unit::MmolL => Dimension::Molar,
            Unit::MIuL | Unit::UuMl | Unit::IuL => Dimension::Activity,
            Unit::Percent => Dimension::Fraction,
            Unit::Ratio => Dimension::Ratio,
        }
    }

    pub const fn to_base_factor(self) -> f64 {
        match self {
            Unit::NgDl => 0.01,
            Unit::NgMl => 1.0,
            Unit::UgDl => 10.0,
            Unit::UgMl => 1000.0,
            Unit::MgDl => 10000.0,
            Unit::GDl => 1e7,
            Unit::GL => 1e6,
            Unit::NmolL => 1.0,
            Unit::PmolL => 0.001,
            Unit::UmolL => 1000.0,
            Unit::MmolL => 1e6,
            Unit::MIuL => 1.0,
            Unit::UuMl => 1.0,
            Unit::IuL => 1000.0,
            Unit::Percent => 1.0,
            Unit::Ratio => 1.0,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Unit::NgDl => "ng/dL",
            Unit::NgMl => "ng/mL",
            Unit::UgDl => "ug/dL",
            Unit::UgMl => "ug/mL",
            Unit::MgDl => "mg/dL",
            Unit::GDl => "g/dL",
            Unit::GL => "g/L",
            Unit::NmolL => "nmol/L",
            Unit::PmolL => "pmol/L",
            Unit::UmolL => "umol/L",
            Unit::MmolL => "mmol/L",
            Unit::MIuL => "mIU/L",
            Unit::UuMl => "uU/mL",
            Unit::IuL => "IU/L",
            Unit::Percent => "%",
            Unit::Ratio => "ratio",
        }
    }

    pub fn parse(input: &str) -> Option<Unit> {
        if input.trim() == "%" {
            return Some(Unit::Percent);
        }
        let normalized: String = input
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '\u{00b5}' | '\u{03bc}'))
            .map(|c| match c {
                '\u{00b5}' | '\u{03bc}' => 'u',
                other => other,
            })
            .collect();
        match normalized.as_str() {
            "ngdl" => Some(Unit::NgDl),
            "ngml" => Some(Unit::NgMl),
            "ugdl" => Some(Unit::UgDl),
            "ugml" => Some(Unit::UgMl),
            "mgdl" => Some(Unit::MgDl),
            "gdl" => Some(Unit::GDl),
            "gl" => Some(Unit::GL),
            "nmoll" | "nmol" => Some(Unit::NmolL),
            "pmoll" | "pmol" => Some(Unit::PmolL),
            "umoll" | "umol" => Some(Unit::UmolL),
            "mmoll" | "mmol" => Some(Unit::MmolL),
            "miul" | "miu" => Some(Unit::MIuL),
            "uuml" | "uiuml" | "uu" => Some(Unit::UuMl),
            "iul" | "ul" => Some(Unit::IuL),
            "percent" | "%" => Some(Unit::Percent),
            "ratio" => Some(Unit::Ratio),
            _ => None,
        }
    }
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

pub fn convert(
    value: f64,
    from: Unit,
    to: Unit,
    molar_mass: Option<f64>,
) -> Result<f64, UnitError> {
    if from == to {
        return Ok(value);
    }
    let fd = from.dimension();
    let td = to.dimension();
    if fd == td {
        return Ok(value * from.to_base_factor() / to.to_base_factor());
    }
    match (fd, td) {
        (Dimension::Mass, Dimension::Molar) => {
            let mw = molar_mass.ok_or(UnitError::NeedsMolarMass { from, to })?;
            let base_mass = value * from.to_base_factor();
            let base_molar = base_mass * 1000.0 / mw;
            Ok(base_molar / to.to_base_factor())
        }
        (Dimension::Molar, Dimension::Mass) => {
            let mw = molar_mass.ok_or(UnitError::NeedsMolarMass { from, to })?;
            let base_molar = value * from.to_base_factor();
            let base_mass = base_molar * mw / 1000.0;
            Ok(base_mass / to.to_base_factor())
        }
        _ => Err(UnitError::Incompatible { from, to }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn testosterone_ngdl_to_nmol() {
        let mw = 288.42;
        let v = convert(500.0, Unit::NgDl, Unit::NmolL, Some(mw)).unwrap();
        assert!(close(v, 17.34, 0.02), "got {v}");
        let back = convert(v, Unit::NmolL, Unit::NgDl, Some(mw)).unwrap();
        assert!(close(back, 500.0, 0.01));
    }

    #[test]
    fn vitamin_d_ngml_to_nmol() {
        let v = convert(30.0, Unit::NgMl, Unit::NmolL, Some(400.64)).unwrap();
        assert!(close(v, 74.9, 0.1), "got {v}");
    }

    #[test]
    fn glucose_mgdl_to_mmol() {
        let v = convert(90.0, Unit::MgDl, Unit::MmolL, Some(180.156)).unwrap();
        assert!(close(v, 4.996, 0.01), "got {v}");
    }

    #[test]
    fn activity_units_are_numerically_stable() {
        let v = convert(8.0, Unit::UuMl, Unit::MIuL, None).unwrap();
        assert!(close(v, 8.0, 1e-9));
        let v = convert(2.0, Unit::IuL, Unit::MIuL, None).unwrap();
        assert!(close(v, 2000.0, 1e-6));
    }

    #[test]
    fn incompatible_dimensions_rejected() {
        assert!(convert(1.0, Unit::MIuL, Unit::NgDl, Some(288.42)).is_err());
    }

    #[test]
    fn mass_to_molar_without_mw_rejected() {
        assert!(convert(1.0, Unit::NgDl, Unit::NmolL, None).is_err());
    }

    #[test]
    fn parse_accepts_lab_notation() {
        assert_eq!(Unit::parse("ng/dL"), Some(Unit::NgDl));
        assert_eq!(Unit::parse("NG/DL"), Some(Unit::NgDl));
        assert_eq!(Unit::parse("µIU/mL"), Some(Unit::UuMl));
        assert_eq!(Unit::parse("mIU/L"), Some(Unit::MIuL));
        assert_eq!(Unit::parse("%"), Some(Unit::Percent));
        assert_eq!(Unit::parse("g/dL"), Some(Unit::GDl));
        assert_eq!(Unit::parse("bogus"), None);
    }
}
