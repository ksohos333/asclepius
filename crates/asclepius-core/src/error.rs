use crate::analyte::AnalyteRegistry;
use crate::units::Unit;

#[derive(Debug, thiserror::Error)]
pub enum AsclepiusError {
    #[error("unknown analyte: {0}")]
    UnknownAnalyte(String),
    #[error("unknown unit: {0}")]
    UnknownUnit(String),
    #[error("incompatible unit {from} for analyte {analyte} (canonical: {canonical})")]
    IncompatibleUnit {
        analyte: String,
        from: String,
        canonical: String,
    },
    #[error("implausible value {value} for {analyte}: plausible range is {min} to {max} {unit}")]
    ImplausibleValue {
        analyte: String,
        value: f64,
        min: f64,
        max: f64,
        unit: String,
    },
    #[error(transparent)]
    Unit(#[from] crate::units::UnitError),
    #[error("db error: {0}")]
    Db(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error in {file}: {source}")]
    Toml {
        file: String,
        source: toml::de::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn load_analytes(dir: &std::path::Path) -> Result<AnalyteRegistry, AsclepiusError> {
    AnalyteRegistry::load_dir(dir)
}

pub fn canonical_label(analyte: &crate::analyte::AnalyteDef) -> String {
    analyte.canonical_unit.label().to_string()
}

pub fn unit_label(u: Unit) -> String {
    u.label().to_string()
}
