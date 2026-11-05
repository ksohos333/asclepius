//! Investigation planning: which tests, in what order, to discriminate
//! between live root-cause hypotheses.
//!
//! Tests are pack data (`content-packs/tests/*.toml`): each names the causal
//! nodes it discriminates and with how much resolution, its specimen, cost
//! band, and honest caveats. The planner ranks tests by *expected
//! discrimination* — the sum of (cause score × test resolution) over the
//! causes a test would inform — divided by a cost factor, so a cheap test
//! that probes the leading hypothesis outranks an expensive one that probes
//! dead causes. Causes with no covering test are surfaced as gaps, never
//! silently dropped.

use crate::causal::{Analysis, ScoredCause};
use crate::error::AsclepiusError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CostBand {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "moderate")]
    Moderate,
    #[serde(rename = "high")]
    High,
}

impl CostBand {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "low" => Some(CostBand::Low),
            "moderate" => Some(CostBand::Moderate),
            "high" => Some(CostBand::High),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            CostBand::Low => "low",
            CostBand::Moderate => "moderate",
            CostBand::High => "high",
        }
    }
    /// Deterministic divisor: rank = discrimination / factor. Cheap tests get
    /// a mild edge over expensive ones of equal resolving power.
    pub fn factor(self) -> f64 {
        match self {
            CostBand::Low => 1.0,
            CostBand::Moderate => 1.25,
            CostBand::High => 1.75,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discriminates {
    /// Causal node id this test informs (validated against the causal packs).
    pub node: String,
    /// How much of the node's uncertainty this test resolves, 0..1.
    pub resolution: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCard {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub summary: String,
    pub discriminates: Vec<Discriminates>,
    pub specimen: String,
    pub cost_band: String,
    /// Availability prose; region-scoped routing arrives with the pathways pack.
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TestFile {
    test: Vec<TestCard>,
}

#[derive(Debug, Clone, Default)]
pub struct TestLibrary {
    cards: Vec<TestCard>,
}

impl TestLibrary {
    pub fn from_vec(cards: Vec<TestCard>) -> Self {
        Self { cards }
    }

    pub fn load_paths(paths: &[std::path::PathBuf]) -> Result<Self, AsclepiusError> {
        if paths.is_empty() {
            return Ok(Self::default());
        }
        let mut cards = Vec::new();
        for path in paths {
            let raw = std::fs::read_to_string(path)?;
            let parsed: TestFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            cards.extend(parsed.test);
        }
        Ok(Self { cards })
    }

    pub fn load_dir(dir: &Path) -> Result<Self, AsclepiusError> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        Self::load_paths(&paths)
    }

    pub fn all(&self) -> &[TestCard] {
        &self.cards
    }

    pub fn get(&self, id: &str) -> Option<&TestCard> {
        self.cards.iter().find(|t| t.id == id)
    }
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct PlanWorkupInput {
    /// Condition card id whose live root causes the workup should target.
    pub condition: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankedTest {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub summary: String,
    pub specimen: String,
    pub cost_band: String,
    pub availability: Option<String>,
    pub caveats: Vec<String>,
    pub tier: String,
    pub sources: Vec<String>,
    pub review_by: Option<String>,
    /// Expected discrimination: Σ (cause score × resolution) over the causes
    /// this test informs.
    pub discrimination: f64,
    /// discrimination / cost factor — the ranking key.
    pub rank_value: f64,
    /// Which live causes this test probes, with their current scores.
    pub probes_causes: Vec<ProbedCause>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbedCause {
    pub node: String,
    pub name: String,
    pub cause_score: f64,
    pub resolution: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkupPlan {
    pub condition: String,
    pub ranked_tests: Vec<RankedTest>,
    /// Candidate causes no available test discriminates — visible gaps.
    pub untestable_causes: Vec<UncoveredCause>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UncoveredCause {
    pub node: String,
    pub name: String,
    pub cause_score: f64,
    pub note: String,
}

/// Rank the tests in `library` against a scored root-cause analysis.
pub fn plan_workup(library: &TestLibrary, analysis: &Analysis) -> WorkupPlan {
    let causes_by_node: std::collections::BTreeMap<&str, &ScoredCause> = analysis
        .causes
        .iter()
        .map(|c| (c.node.as_str(), c))
        .collect();
    let mut ranked: Vec<RankedTest> = Vec::new();
    let mut covered: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

    for card in library.all() {
        let mut probes: Vec<ProbedCause> = Vec::new();
        let mut discrimination = 0.0;
        for d in &card.discriminates {
            if let Some(cause) = causes_by_node.get(d.node.as_str()) {
                // Coverage is about existence of a test; ranking is about
                // live uncertainty. A test for a dead cause is covered but
                // earns no rank.
                covered.insert(d.node.as_str());
                let contribution = cause.score * d.resolution.clamp(0.0, 1.0);
                discrimination += contribution;
                probes.push(ProbedCause {
                    node: cause.node.clone(),
                    name: cause.name.clone(),
                    cause_score: cause.score,
                    resolution: d.resolution,
                });
            }
        }
        if probes.is_empty() || discrimination <= f64::EPSILON {
            continue; // discriminates nothing, or nothing currently live
        }
        let band = CostBand::parse(&card.cost_band);
        let factor = band.map(|b| b.factor()).unwrap_or(1.25);
        ranked.push(RankedTest {
            id: card.id.clone(),
            name: card.name.clone(),
            kind: card.kind.clone(),
            summary: card.summary.clone(),
            specimen: card.specimen.clone(),
            cost_band: card.cost_band.clone(),
            availability: card.availability.clone(),
            caveats: card.caveats.clone(),
            tier: card.tier.clone(),
            sources: card.sources.clone(),
            review_by: card.review_by.clone(),
            discrimination,
            rank_value: discrimination / factor,
            probes_causes: probes,
        });
    }

    ranked.sort_by(|a, b| {
        b.rank_value
            .partial_cmp(&a.rank_value)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    let untestable = analysis
        .causes
        .iter()
        .filter(|c| !covered.contains(c.node.as_str()))
        .map(|c| UncoveredCause {
            node: c.node.clone(),
            name: c.name.clone(),
            cause_score: c.score,
            note: "no test in the investigation pack discriminates this cause yet — curate one or accept the gap"
                .into(),
        })
        .collect();

    WorkupPlan {
        condition: analysis.condition.clone(),
        ranked_tests: ranked,
        untestable_causes: untestable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::ScoredCause;

    fn cause(node: &str, score: f64) -> ScoredCause {
        ScoredCause {
            node: node.into(),
            name: node.to_string(),
            score,
            base: score,
            base_path: node.into(),
            contributions: vec![],
            probes_total: 0,
            probes_tested: 0,
            untested_probes: vec![],
            tier: "C".into(),
            summary: String::new(),
            review_by: None,
            suggested_hypothesis: String::new(),
        }
    }

    fn analysis(causes: Vec<ScoredCause>) -> Analysis {
        Analysis {
            condition: "acne".into(),
            causes,
            coverage: 0.5,
        }
    }

    fn card(id: &str, band: &str, discriminates: &[(&str, f64)]) -> TestCard {
        TestCard {
            id: id.into(),
            name: id.to_string(),
            kind: "blood".into(),
            summary: String::new(),
            discriminates: discriminates
                .iter()
                .map(|(n, r)| Discriminates { node: n.to_string(), resolution: *r })
                .collect(),
            specimen: "serum".into(),
            cost_band: band.into(),
            availability: None,
            caveats: vec![],
            tier: "C".into(),
            sources: vec![],
            review_by: None,
        }
    }

    #[test]
    fn discrimination_weighted_by_live_scores() {
        let lib = TestLibrary::from_vec(vec![
            card("sibo", "low", &[("gut", 0.6)]),
            card("dutch", "high", &[("androgen", 0.4)]),
        ]);
        // gut leads 0.8 vs androgen 0.4: sibo = 0.8*0.6/1.0 = 0.48;
        // dutch = 0.4*0.4/1.75 ≈ 0.091.
        let a = analysis(vec![cause("gut", 0.8), cause("androgen", 0.4)]);
        let plan = plan_workup(&lib, &a);
        assert_eq!(plan.ranked_tests[0].id, "sibo");
        assert!((plan.ranked_tests[0].discrimination - 0.48).abs() < 1e-9);
        assert!(plan.ranked_tests[0].rank_value > plan.ranked_tests[1].rank_value);
        assert!(plan.untestable_causes.is_empty());
    }

    #[test]
    fn cheap_beats_expensive_at_equal_power() {
        let lib = TestLibrary::from_vec(vec![
            card("cheap", "low", &[("gut", 0.6)]),
            card("pricey", "high", &[("gut", 0.6)]),
        ]);
        let a = analysis(vec![cause("gut", 0.9)]);
        let plan = plan_workup(&lib, &a);
        assert_eq!(plan.ranked_tests[0].id, "cheap");
        // Equal discrimination (0.54): cost factor decides.
        assert!((plan.ranked_tests[0].discrimination - plan.ranked_tests[1].discrimination).abs() < 1e-9);
    }

    #[test]
    fn test_for_dead_cause_is_dropped_but_counts_as_covered() {
        let lib = TestLibrary::from_vec(vec![card("sibo", "low", &[("gut", 0.6)])]);
        let a = analysis(vec![cause("gut", 0.0), cause("linoleic", 0.2)]);
        let plan = plan_workup(&lib, &a);
        assert!(plan.ranked_tests.is_empty(), "dead cause gives no discrimination");
        // gut is covered (a test exists — it is merely pointless right now);
        // linoleic has no covering test at all.
        let nodes: Vec<&str> = plan
            .untestable_causes
            .iter()
            .map(|u| u.node.as_str())
            .collect();
        assert!(!nodes.contains(&"gut"), "covered-by-existing-test: {:?}", nodes);
        assert!(nodes.contains(&"linoleic"));
    }

    #[test]
    fn multi_cause_tests_sum_their_discrimination() {
        let lib = TestLibrary::from_vec(vec![card("broad", "moderate", &[("gut", 0.5), ("androgen", 0.5)])]);
        let a = analysis(vec![cause("gut", 0.8), cause("androgen", 0.4)]);
        let plan = plan_workup(&lib, &a);
        assert!((plan.ranked_tests[0].discrimination - (0.8 * 0.5 + 0.4 * 0.5)).abs() < 1e-9);
        assert_eq!(plan.ranked_tests[0].probes_causes.len(), 2);
    }
}
