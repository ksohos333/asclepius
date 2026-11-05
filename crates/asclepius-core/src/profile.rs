use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Sex {
    #[serde(rename = "male")]
    Male,
    #[serde(rename = "female")]
    Female,
}

/// Private, local-only region scope: drives the pathways layer (how to get
/// tests, legal classes, physician routing). Never leaves the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RegionProfile {
    /// ISO 3166-1 alpha-2 country code, e.g. "GR".
    pub country: String,
    /// Legal bloc layer override, e.g. "EU" (normally resolved from the pack).
    #[serde(default)]
    pub bloc: Option<String>,
    /// Preferred language for request scripts, e.g. "el".
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: Option<String>,
    pub age_years: Option<u32>,
    pub sex: Option<Sex>,
    #[serde(default)]
    pub region: Option<RegionProfile>,
    #[serde(default)]
    pub conditions: Vec<String>,
    #[serde(default)]
    pub medications: Vec<String>,
    #[serde(default)]
    pub supplements: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
}

impl Profile {
    pub fn placeholder() -> Self {
        Self {
            name: Some("user".into()),
            age_years: None,
            sex: None,
            region: None,
            conditions: Vec::new(),
            medications: Vec::new(),
            supplements: Vec::new(),
            goals: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub age_years: Option<u32>,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub region: Option<RegionProfile>,
    #[serde(default)]
    pub conditions: Option<Vec<String>>,
    #[serde(default)]
    pub medications: Option<Vec<String>>,
    #[serde(default)]
    pub supplements: Option<Vec<String>>,
    #[serde(default)]
    pub goals: Option<Vec<String>>,
}

impl Profile {
    pub fn apply(&mut self, update: &ProfileUpdate) {
        if let Some(v) = &update.name {
            self.name = Some(v.clone());
        }
        if let Some(v) = update.age_years {
            self.age_years = Some(v);
        }
        if let Some(v) = update.sex {
            self.sex = Some(v);
        }
        if let Some(v) = &update.region {
            self.region = Some(v.clone());
        }
        if let Some(v) = &update.conditions {
            self.conditions = v.clone();
        }
        if let Some(v) = &update.medications {
            self.medications = v.clone();
        }
        if let Some(v) = &update.supplements {
            self.supplements = v.clone();
        }
        if let Some(v) = &update.goals {
            self.goals = v.clone();
        }
    }
}
