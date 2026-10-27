use crate::error::AsclepiusError;
use crate::units::Unit;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyteDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub loinc: Option<String>,
    #[serde(default)]
    pub molar_mass: Option<f64>,
    pub canonical_unit: Unit,
    pub plausible_min: f64,
    pub plausible_max: f64,
}

#[derive(Debug, Default)]
pub struct AnalyteRegistry {
    analytes: Vec<AnalyteDef>,
    index: HashMap<String, usize>,
}

pub fn normalize_key(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

#[derive(Debug, Deserialize)]
struct AnalyteFile {
    analyte: Vec<AnalyteDef>,
}

impl AnalyteRegistry {
    pub fn from_vec(analytes: Vec<AnalyteDef>) -> Self {
        let mut index = HashMap::new();
        for (i, a) in analytes.iter().enumerate() {
            let keys = std::iter::once(a.id.as_str())
                .chain(std::iter::once(a.name.as_str()))
                .chain(a.aliases.iter().map(|s| s.as_str()));
            for k in keys {
                index.entry(normalize_key(k)).or_insert(i);
            }
        }
        Self { analytes, index }
    }

    pub fn load_dir(dir: &Path) -> Result<Self, AsclepiusError> {
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
                format!("no .toml analyte packs found in {}", dir.display()),
            )));
        }
        for path in entries {
            let raw = std::fs::read_to_string(&path)?;
            let parsed: AnalyteFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            defs.extend(parsed.analyte);
        }
        Ok(Self::from_vec(defs))
    }

    pub fn resolve(&self, name: &str) -> Option<&AnalyteDef> {
        self.index
            .get(&normalize_key(name))
            .map(|&i| &self.analytes[i])
    }

    pub fn get(&self, id: &str) -> Option<&AnalyteDef> {
        self.resolve(id)
    }

    pub fn all(&self) -> &[AnalyteDef] {
        &self.analytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> AnalyteRegistry {
        AnalyteRegistry::from_vec(vec![AnalyteDef {
            id: "testosterone_total".into(),
            name: "Total Testosterone".into(),
            aliases: vec!["total t".into(), "Testosterone, Total".into()],
            loinc: None,
            molar_mass: Some(288.42),
            canonical_unit: Unit::NgDl,
            plausible_min: 20.0,
            plausible_max: 2000.0,
        }])
    }

    #[test]
    fn resolves_by_id_name_and_alias() {
        let r = reg();
        assert!(r.resolve("testosterone_total").is_some());
        assert!(r.resolve("Total Testosterone").is_some());
        assert!(r.resolve("total t").is_some());
        assert!(r.resolve("TESTOSTERONE, TOTAL").is_some());
        assert!(r.resolve("insulin").is_none());
    }

    #[test]
    fn first_registration_wins_on_collision() {
        let r = AnalyteRegistry::from_vec(vec![
            AnalyteDef {
                id: "a".into(),
                name: "Alpha".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::Ratio,
                plausible_min: 0.0,
                plausible_max: 1.0,
            },
            AnalyteDef {
                id: "b".into(),
                name: "Alpha".into(),
                aliases: vec![],
                loinc: None,
                molar_mass: None,
                canonical_unit: Unit::Ratio,
                plausible_min: 0.0,
                plausible_max: 1.0,
            },
        ]);
        assert_eq!(r.resolve("Alpha").unwrap().id, "a");
    }
}
