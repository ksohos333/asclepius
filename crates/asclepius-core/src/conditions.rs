use crate::error::AsclepiusError;
use crate::range::Confidence;
use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_confidence() -> Confidence {
    Confidence::Medium
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionCard {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub mechanisms: Vec<String>,
    #[serde(default)]
    pub biomarkers: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CardHit {
    pub score: u32,
    #[serde(flatten)]
    pub card: ConditionCard,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ConditionQuery {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ConditionLibrary {
    cards: Vec<ConditionCard>,
}

#[derive(Debug, Deserialize)]
struct ConditionFile {
    condition: Vec<ConditionCard>,
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect()
}

impl ConditionLibrary {
    pub fn from_vec(cards: Vec<ConditionCard>) -> Self {
        Self { cards }
    }

    pub fn load_dir(dir: &Path) -> Result<Self, AsclepiusError> {
        let mut cards = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        entries.sort();
        if entries.is_empty() {
            return Err(AsclepiusError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no .toml condition packs found in {}", dir.display()),
            )));
        }
        for path in entries {
            let raw = std::fs::read_to_string(&path)?;
            let parsed: ConditionFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            cards.extend(parsed.condition);
        }
        Ok(Self { cards })
    }

    pub fn all(&self) -> &[ConditionCard] {
        &self.cards
    }

    pub fn get(&self, id: &str) -> Option<&ConditionCard> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<CardHit> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<CardHit> = self
            .cards
            .iter()
            .filter_map(|card| {
                let mut score = 0u32;
                let name = card.name.to_lowercase();
                for token in &tokens {
                    if name == *token || card.aliases.iter().any(|a| a.to_lowercase() == *token) {
                        score += 10;
                    } else if name.contains(token.as_str())
                        || card
                            .aliases
                            .iter()
                            .any(|a| a.to_lowercase().contains(token.as_str()))
                    {
                        score += 6;
                    }
                    if card
                        .biomarkers
                        .iter()
                        .any(|b| b.to_lowercase().contains(token.as_str()))
                    {
                        score += 3;
                    }
                    if card
                        .mechanisms
                        .iter()
                        .any(|m| m.to_lowercase().contains(token.as_str()))
                    {
                        score += 3;
                    }
                    if card.summary.to_lowercase().contains(token.as_str()) {
                        score += 1;
                    }
                }
                (score > 0).then(|| CardHit {
                    score,
                    card: card.clone(),
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.card.id.cmp(&b.card.id))
        });
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> ConditionLibrary {
        ConditionLibrary::from_vec(vec![
            ConditionCard {
                id: "acne_root_causes".into(),
                name: "Cystic Acne Root Causes".into(),
                aliases: vec!["acne".into()],
                summary: "Androgen activity, IGF-1, insulin resistance, gut-skin axis".into(),
                mechanisms: vec!["androgen activity".into(), "igf-1 dairy glycemic".into()],
                biomarkers: vec!["testosterone_total".into(), "insulin".into()],
                notes: None,
                sources: vec!["seed".into()],
                confidence: Confidence::Low,
                review_by: None,
            },
            ConditionCard {
                id: "thyroid_patterns".into(),
                name: "Thyroid Function Patterns".into(),
                aliases: vec!["hypothyroid".into()],
                summary: "TSH elevation with peripheral conversion issues".into(),
                mechanisms: vec!["autoimmune tpo".into()],
                biomarkers: vec!["tsh".into()],
                notes: None,
                sources: vec!["seed".into()],
                confidence: Confidence::Medium,
                review_by: None,
            },
        ])
    }

    #[test]
    fn alias_exact_outranks_summary() {
        let hits = library().search("acne", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].card.id, "acne_root_causes");
        assert!(hits[0].score >= 10);
    }

    #[test]
    fn token_in_mechanism_matches() {
        let hits = library().search("autoimmune", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].card.id, "thyroid_patterns");
    }

    #[test]
    fn no_hit_returns_empty() {
        assert!(library().search("quantum", 5).is_empty());
    }
}
