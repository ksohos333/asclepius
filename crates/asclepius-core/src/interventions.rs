use crate::error::AsclepiusError;
use crate::profile::Profile;
use crate::range::Confidence;
use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_confidence() -> Confidence {
    Confidence::Medium
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    #[serde(rename = "lifestyle")]
    Lifestyle,
    #[serde(rename = "diet")]
    Diet,
    #[serde(rename = "nutrient")]
    Nutrient,
    #[serde(rename = "botanical")]
    Botanical,
    #[serde(rename = "pharmaceutical")]
    Pharmaceutical,
}

impl Category {
    pub fn rank(self) -> u8 {
        match self {
            Category::Lifestyle => 0,
            Category::Diet => 1,
            Category::Nutrient => 2,
            Category::Botanical => 3,
            Category::Pharmaceutical => 4,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Category::Lifestyle => "lifestyle",
            Category::Diet => "diet",
            Category::Nutrient => "nutrient",
            Category::Botanical => "botanical",
            Category::Pharmaceutical => "pharmaceutical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DosingSpec {
    pub form: String,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ceiling {
    pub amount: f64,
    pub unit: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionSeverity {
    #[serde(rename = "caution")]
    Caution,
    #[serde(rename = "avoid")]
    Avoid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRef {
    pub with: String,
    pub effect: String,
    pub severity: InteractionSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionCard {
    pub id: String,
    pub name: String,
    pub category: Category,
    #[serde(default)]
    pub classes: Vec<String>,
    pub tier: String,
    pub summary: String,
    #[serde(default)]
    pub dosing: Vec<DosingSpec>,
    #[serde(default)]
    pub ceiling: Option<Ceiling>,
    #[serde(default)]
    pub contraindicated_if: Vec<String>,
    #[serde(default)]
    pub interactions: Vec<InteractionRef>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    #[serde(default)]
    pub review_by: Option<String>,
}

pub const FLAG_PHYSICIAN_REQUIRED: &str = "physician_required";
pub const FLAG_REQUIRES_BASELINE_LABS: &str = "requires_baseline_labs";

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ProtocolInput {
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InterventionLibrary {
    cards: Vec<InterventionCard>,
}

#[derive(Debug, Deserialize)]
struct InterventionFile {
    intervention: Vec<InterventionCard>,
}

impl InterventionLibrary {
    pub fn from_vec(cards: Vec<InterventionCard>) -> Self {
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
                format!("no .toml intervention packs found in {}", dir.display()),
            )));
        }
        for path in entries {
            let raw = std::fs::read_to_string(&path)?;
            let parsed: InterventionFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            cards.extend(parsed.intervention);
        }
        Ok(Self { cards })
    }

    pub fn all(&self) -> &[InterventionCard] {
        &self.cards
    }

    pub fn get(&self, id: &str) -> Option<&InterventionCard> {
        self.cards.iter().find(|c| c.id == id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InterventionHit {
    pub score: u32,
    #[serde(flatten)]
    pub card: InterventionCard,
}

impl InterventionLibrary {
    pub fn search(&self, query: &str, limit: usize) -> Vec<InterventionHit> {
        let tokens: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| t.len() > 1)
            .map(|t| t.to_string())
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<InterventionHit> = self
            .cards
            .iter()
            .filter_map(|card| {
                let name = card.name.to_lowercase();
                let mut score = 0u32;
                for token in &tokens {
                    if card.id.to_lowercase() == *token || name == *token {
                        score += 10;
                    } else if name.contains(token.as_str()) {
                        score += 6;
                    }
                    if card
                        .classes
                        .iter()
                        .any(|c| c.to_lowercase().contains(token.as_str()))
                    {
                        score += 4;
                    }
                    if card.summary.to_lowercase().contains(token.as_str()) {
                        score += 1;
                    }
                    if card
                        .sources
                        .iter()
                        .any(|s| s.to_lowercase().contains(token.as_str()))
                    {
                        score += 2;
                    }
                }
                (score > 0).then(|| InterventionHit {
                    score,
                    card: card.clone(),
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.card.id.cmp(&b.card.id)));
        hits.truncate(limit);
        hits
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalItem {
    pub id: String,
    pub name: String,
    pub category: String,
    pub tier: String,
    pub summary: String,
    pub dosing: Vec<DosingSpec>,
    pub ceiling: Option<Ceiling>,
    pub matched_targets: Vec<String>,
    pub physician_required: bool,
    pub requires_baseline_labs: bool,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcludedItem {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InteractionHit {
    pub a: String,
    pub b: String,
    pub effect: String,
    pub severity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub items: Vec<ProposalItem>,
    pub excluded: Vec<ExcludedItem>,
    pub interactions: Vec<InteractionHit>,
    pub serotonergic_burden: Option<String>,
    pub disclaimer: String,
}

fn matches_any(haystack: &[String], needles: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for needle in needles {
        let n = needle.to_lowercase();
        for hay in haystack {
            let h = hay.to_lowercase();
            if h.contains(&n) || n.contains(&h) {
                hits.push(needle.clone());
                break;
            }
        }
    }
    hits
}

pub fn propose(
    library: &InterventionLibrary,
    target_condition_ids: &[String],
    profile: &Profile,
) -> Proposal {
    let mut candidates: Vec<&InterventionCard> = library
        .cards
        .iter()
        .filter(|c| c.targets.iter().any(|t| target_condition_ids.contains(t)))
        .collect();
    candidates.sort_by(|a, b| {
        a.category
            .rank()
            .cmp(&b.category.rank())
            .then_with(|| a.tier.cmp(&b.tier))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut items = Vec::new();
    let mut excluded = Vec::new();
    for card in candidates {
        let profile_conditions = &profile.conditions;
        let med_context: Vec<String> = profile
            .medications
            .iter()
            .chain(profile.supplements.iter())
            .cloned()
            .collect();
        let condition_hits = matches_any(profile_conditions, &card.contraindicated_if);
        if !condition_hits.is_empty() {
            excluded.push(ExcludedItem {
                id: card.id.clone(),
                name: card.name.clone(),
                reason: format!(
                    "contraindicated by profile conditions: {}",
                    condition_hits.join(", ")
                ),
            });
            continue;
        }
        let med_hits = card
            .interactions
            .iter()
            .filter(|i| i.severity == InteractionSeverity::Avoid)
            .filter(|i| !matches_any(&med_context, std::slice::from_ref(&i.with)).is_empty())
            .map(|i| ExcludedItem {
                id: card.id.clone(),
                name: card.name.clone(),
                reason: format!(
                    "avoid interaction with profile medication/supplement: {}",
                    i.with
                ),
            })
            .collect::<Vec<_>>();
        if let Some(hit) = med_hits.first() {
            excluded.push(hit.clone());
            continue;
        }
        items.push(card);
    }

    let mut interactions = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (i, a) in items.iter().enumerate() {
        for b in items.iter().skip(i + 1) {
            for iref in &a.interactions {
                let refers_to_b = iref.with == b.id || b.classes.contains(&iref.with);
                if refers_to_b {
                    if iref.severity == InteractionSeverity::Avoid {
                        dropped.push(b.id.clone());
                    }
                    interactions.push(InteractionHit {
                        a: a.id.clone(),
                        b: b.id.clone(),
                        effect: iref.effect.clone(),
                        severity: match iref.severity {
                            InteractionSeverity::Caution => "caution".into(),
                            InteractionSeverity::Avoid => "avoid".into(),
                        },
                    });
                }
            }
        }
    }
    if !dropped.is_empty() {
        items.retain(|c| !dropped.contains(&c.id));
    }

    let serotonergic = items
        .iter()
        .filter(|c| c.classes.iter().any(|k| k == "serotonergic"))
        .count();
    let serotonergic_burden = (serotonergic >= 2).then(|| {
        format!(
            "{serotonergic} serotonergic-class interventions proposed together; serotonergic burden is a specific risk in post-SSRI contexts"
        )
    });

    let items: Vec<ProposalItem> = items
        .into_iter()
        .map(|c| ProposalItem {
            id: c.id.clone(),
            name: c.name.clone(),
            category: c.category.label().to_string(),
            tier: c.tier.clone(),
            summary: c.summary.clone(),
            dosing: c.dosing.clone(),
            ceiling: c.ceiling.clone(),
            matched_targets: c
                .targets
                .iter()
                .filter(|t| target_condition_ids.contains(t))
                .cloned()
                .collect(),
            physician_required: c.flags.contains(&FLAG_PHYSICIAN_REQUIRED.to_string())
                || c.category == Category::Pharmaceutical,
            requires_baseline_labs: c.flags.contains(&FLAG_REQUIRES_BASELINE_LABS.to_string()),
            confidence: c.confidence,
        })
        .collect();

    Proposal {
        items,
        excluded,
        interactions,
        serotonergic_burden,
        disclaimer: "Natural-first ordering: lifestyle, diet, nutrients, botanicals, then \
                     physician-required options. Guardrails (ceilings, baselines, interactions) \
                     are advisory output for you and your clinician, not a prescription. \
                     Physician involvement is required for pharmaceutical items."
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, category: Category, targets: &[&str]) -> InterventionCard {
        InterventionCard {
            id: id.into(),
            name: id.to_string(),
            category,
            classes: Vec::new(),
            tier: "C".into(),
            summary: String::new(),
            dosing: Vec::new(),
            ceiling: None,
            contraindicated_if: Vec::new(),
            interactions: Vec::new(),
            flags: Vec::new(),
            targets: targets.iter().map(|s| s.to_string()).collect(),
            sources: Vec::new(),
            confidence: Confidence::Low,
            review_by: None,
        }
    }

    fn library() -> InterventionLibrary {
        InterventionLibrary::from_vec(vec![
            card("zinc", Category::Nutrient, &["acne"]),
            card("sleep", Category::Lifestyle, &["acne", "low_t"]),
            card("trt", Category::Pharmaceutical, &["low_t"]),
            card("ashwagandha", Category::Botanical, &["low_t"]),
        ])
    }

    fn profile() -> Profile {
        Profile::placeholder()
    }

    #[test]
    fn natural_first_ordering() {
        let p = propose(&library(), &["acne".into(), "low_t".into()], &profile());
        let cats: Vec<&str> = p.items.iter().map(|i| i.category.as_str()).collect();
        assert_eq!(
            cats,
            vec!["lifestyle", "nutrient", "botanical", "pharmaceutical"]
        );
        assert!(p.items.last().unwrap().physician_required);
        assert!(!p.items.first().unwrap().physician_required);
    }

    #[test]
    fn targets_filter() {
        let p = propose(&library(), &["acne".into()], &profile());
        let ids: Vec<&str> = p.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["sleep", "zinc"]);
    }

    #[test]
    fn contraindication_excludes() {
        let mut lib = library();
        lib.cards
            .iter_mut()
            .find(|c| c.id == "zinc")
            .unwrap()
            .contraindicated_if = vec!["hemochromatosis".into()];
        let mut p = profile();
        p.conditions = vec!["hemochromatosis".into()];
        let out = propose(&lib, &["acne".into()], &p);
        assert!(out.items.iter().all(|i| i.id != "zinc"));
        assert!(out.excluded.iter().any(|e| e.id == "zinc"));
    }

    #[test]
    fn avoid_interaction_between_items_drops_later() {
        let mut lib = library();
        let zinc = lib.cards.iter_mut().find(|c| c.id == "zinc").unwrap();
        zinc.interactions = vec![InteractionRef {
            with: "ashwagandha".into(),
            effect: "test conflict".into(),
            severity: InteractionSeverity::Avoid,
        }];
        let out = propose(&lib, &["acne".into(), "low_t".into()], &profile());
        assert!(out.items.iter().all(|i| i.id != "ashwagandha"));
        assert!(
            out.interactions
                .iter()
                .any(|i| i.a == "zinc" && i.b == "ashwagandha" && i.severity == "avoid")
        );
    }

    #[test]
    fn serotonergic_burden_warns() {
        let lib = InterventionLibrary::from_vec(vec![
            {
                let mut c = card("s1", Category::Botanical, &["mood"]);
                c.classes = vec!["serotonergic".into()];
                c
            },
            {
                let mut c = card("s2", Category::Nutrient, &["mood"]);
                c.classes = vec!["serotonergic".into()];
                c
            },
        ]);
        let out = propose(&lib, &["mood".into()], &profile());
        assert!(out.serotonergic_burden.is_some());
    }
}
