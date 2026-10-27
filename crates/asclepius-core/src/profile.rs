use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Sex {
    #[serde(rename = "male")]
    Male,
    #[serde(rename = "female")]
    Female,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: Option<String>,
    pub age_years: Option<u32>,
    pub sex: Option<Sex>,
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
