//! Causal reasoning: typed graphs per condition, weighted scoring with
//! evidence trails, and treatment-response-as-evidence.
//!
//! The graph is data (`content-packs/causal/*.toml`), validated by the L0
//! load gate (edges resolve, acyclic, condition refs exist) and hashed like
//! every other pack. Scoring is deliberately simple and debuggable:
//!
//!   score(root cause) = clamp(base + Σ lab signals + Σ treatment responses)
//!
//! - `base` is the strongest (max-product) causal path from the root cause to
//!   a symptom node of the condition — weakest link dominates.
//! - lab signals come from committed facts against each node's `probes`:
//!   a fact whose range status matches the probe's expectation adds the
//!   probe weight; a fact that contradicts it subtracts half.
//! - treatment responses are the patient's own history as evidence: a
//!   registered pattern (e.g. antibiotics cleared, then recurrence) shifts
//!   the nodes its pack entry names.
//!
//! Every score carries its full contribution trail — edge refs, fact ids,
//! response ledger ids — so the verifier and the caller can audit it.

use crate::error::AsclepiusError;
use crate::packs::assert_acyclic;
use crate::range::RangeStatus;
use crate::rules::LatestValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    #[serde(rename = "root_cause")]
    RootCause,
    #[serde(rename = "mechanism")]
    Mechanism,
    #[serde(rename = "symptom")]
    Symptom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    /// Analyte id (or derived id) whose committed fact probes this node.
    pub analyte: String,
    /// Expected range status if this node is causal: "above" or "below".
    #[serde(default)]
    pub expect: Option<String>,
    /// Score contribution when a committed fact matches the expectation.
    #[serde(default)]
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalNode {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub summary: String,
    /// Condition card ids this node's graph serves (validated by the gate);
    /// a shared mechanism serves several conditions — one node, many graphs.
    pub conditions: Vec<String>,
    /// Optional mechanism-family label linking kin mechanisms across
    /// conditions (e.g. gut_tryptophan_axis): distinct nodes, same machinery —
    /// evidence and hazards may transfer. Surfaced by hypothesis_space.
    #[serde(default)]
    pub mechanism_family: Option<String>,
    #[serde(default)]
    pub probes: Vec<Probe>,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEdge {
    pub from: String,
    pub to: String,
    pub weight: f64,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub node: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreatmentResponse {
    pub id: String,
    pub name: String,
    /// Human description of the observable pattern this entry encodes.
    pub pattern: String,
    #[serde(default)]
    pub effects: Vec<Effect>,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CausalFile {
    #[serde(default)]
    node: Vec<CausalNode>,
    #[serde(default)]
    edge: Vec<CausalEdge>,
}

#[derive(Debug, Deserialize)]
struct ResponseFile {
    #[serde(default)]
    response: Vec<TreatmentResponse>,
}

#[derive(Debug, Clone, Default)]
pub struct CausalGraph {
    nodes: Vec<CausalNode>,
    edges: Vec<CausalEdge>,
    responses: Vec<TreatmentResponse>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct RootCauseAnalysisInput {
    /// Condition card id to analyze (e.g. "acne_root_causes").
    pub condition: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Contribution {
    /// Evidence ref for this delta: "path:edge_weights", "ledger:facts/<id>",
    /// or "ledger:treatment_responses/<id>".
    pub source: String,
    pub detail: String,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoredCause {
    pub node: String,
    pub name: String,
    pub score: f64,
    pub base: f64,
    pub base_path: String,
    pub contributions: Vec<Contribution>,
    pub probes_total: usize,
    pub probes_tested: usize,
    pub untested_probes: Vec<String>,
    pub tier: String,
    pub summary: String,
    pub review_by: Option<String>,
    /// Registration-ready hypothesis statement (proposal-mode: the engine
    /// proposes, the user or their agent disposes via hypothesis_register).
    pub suggested_hypothesis: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub condition: String,
    pub causes: Vec<ScoredCause>,
    /// Fraction of candidate causes with any committed evidence (probe fact
    /// or treatment response). A high score on low coverage says "guess",
    /// not "conclusion".
    pub coverage: f64,
}

impl CausalGraph {
    pub fn from_parts(
        nodes: Vec<CausalNode>,
        edges: Vec<CausalEdge>,
        responses: Vec<TreatmentResponse>,
    ) -> Result<Self, AsclepiusError> {
        let graph = Self { nodes, edges, responses };
        graph.integrity_check()?;
        Ok(graph)
    }

    fn integrity_check(&self) -> Result<(), AsclepiusError> {
        let mut ids = std::collections::BTreeSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.as_str()) {
                return Err(AsclepiusError::CausalGraph(format!(
                    "duplicate node id '{}'",
                    node.id
                )));
            }
        }
        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) || !ids.contains(edge.to.as_str()) {
                return Err(AsclepiusError::CausalGraph(format!(
                    "edge {} -> {} references unknown node",
                    edge.from, edge.to
                )));
            }
        }
        let edge_pairs: Vec<(String, String)> = self
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        if let Err(cycle) = assert_acyclic(&edge_pairs) {
            return Err(AsclepiusError::CausalGraph(cycle));
        }
        Ok(())
    }

    /// Load from gate-admitted files: causal graphs plus treatment-response
    /// patterns (which may reference nodes across all causal files).
    pub fn load_paths(
        causal: &[std::path::PathBuf],
        responses: &[std::path::PathBuf],
    ) -> Result<Self, AsclepiusError> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for path in causal {
            let raw = std::fs::read_to_string(path)?;
            let parsed: CausalFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            nodes.extend(parsed.node);
            edges.extend(parsed.edge);
        }
        let node_ids: std::collections::BTreeSet<&str> =
            nodes.iter().map(|n| n.id.as_str()).collect();
        let mut responses_out = Vec::new();
        for path in responses {
            let raw = std::fs::read_to_string(path)?;
            let parsed: ResponseFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            for response in parsed.response {
                for effect in &response.effects {
                    if !node_ids.contains(effect.node.as_str()) {
                        return Err(AsclepiusError::CausalGraph(format!(
                            "response '{}' effects unknown node '{}'",
                            response.id, effect.node
                        )));
                    }
                }
                responses_out.push(response);
            }
        }
        Self::from_parts(nodes, edges, responses_out)
    }

    pub fn load_dir(root: &Path) -> Result<Self, AsclepiusError> {
        let list = |dir: &Path| -> Vec<std::path::PathBuf> {
            let mut paths: Vec<_> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            paths.sort();
            paths
        };
        Self::load_paths(&list(&root.join("causal")), &list(&root.join("treatment-responses")))
    }

    pub fn nodes(&self) -> &[CausalNode] {
        &self.nodes
    }

    pub fn edges(&self) -> &[CausalEdge] {
        &self.edges
    }

    pub fn responses(&self) -> &[TreatmentResponse] {
        &self.responses
    }

    pub fn get_response(&self, id: &str) -> Option<&TreatmentResponse> {
        self.responses.iter().find(|r| r.id == id)
    }

    pub fn has_condition(&self, condition: &str) -> bool {
        self.nodes.iter().any(|n| n.conditions.iter().any(|c| c == condition))
    }
}

/// One committed lab fact (latest per analyte) with its ledger id, available
/// to the scorer.
#[derive(Debug, Clone)]
pub struct EvidenceFact {
    pub fact_id: String,
    pub latest: LatestValue,
}

/// A treatment response the patient actually experienced, from the ledger.
#[derive(Debug, Clone)]
pub struct EvidenceResponse {
    pub ledger_id: String,
    pub response_id: String,
}

fn status_present(latest: &LatestValue, status: &str) -> bool {
    latest.assessments.iter().any(|a| {
        let s = match a.status {
            RangeStatus::Below => "below",
            RangeStatus::Within => "within",
            RangeStatus::Above => "above",
        };
        s == status
    })
}

fn opposite(status: &str) -> &str {
    match status {
        "above" => "below",
        "below" => "above",
        _ => "within",
    }
}

fn clamp_score(v: f64) -> f64 {
    v.clamp(0.0, 0.99)
}

/// Strongest causal path (max product of weights) from `from` to any node
/// matching `targets`. Node ids are globally unique and the graph is
/// acyclic (both enforced at load), so the walk needs no extra scoping —
/// condition scoping happens in `analyze` via node membership.
fn best_path(
    edges: &[CausalEdge],
    from: &str,
    targets: &dyn Fn(&str) -> bool,
) -> Option<(f64, String)> {
    fn walk(
        edges: &[CausalEdge],
        current: &str,
        acc: f64,
        path: &str,
        targets: &dyn Fn(&str) -> bool,
    ) -> Option<(f64, String)> {
        if targets(current) {
            return Some((acc, path.to_string()));
        }
        let mut best: Option<(f64, String)> = None;
        for edge in edges {
            if edge.from != current {
                continue;
            }
            if let Some((w, p)) = walk(
                edges,
                &edge.to,
                acc * edge.weight,
                &format!("{path} -> {}", edge.to),
                targets,
            ) {
                if best.as_ref().map(|(bw, _)| w > *bw).unwrap_or(true) {
                    best = Some((w, p));
                }
            }
        }
        best
    }
    walk(edges, from, 1.0, from, targets)
}

/// Score the candidate root causes of `condition` against the evidence.
pub fn analyze(
    graph: &CausalGraph,
    condition: &str,
    facts: &[EvidenceFact],
    responses: &[EvidenceResponse],
) -> Analysis {
    let fact_by_analyte: BTreeMap<&str, &EvidenceFact> =
        facts.iter().map(|f| (f.latest.analyte.as_str(), f)).collect();
    let symptoms: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.conditions.iter().any(|c| c == condition) && n.kind == NodeKind::Symptom)
        .map(|n| n.id.as_str())
        .collect();

    let mut causes: Vec<ScoredCause> = Vec::new();
    let mut touched = 0usize;
    let mut candidates = 0usize;

    for node in graph
        .nodes
        .iter()
        .filter(|n| n.conditions.iter().any(|c| c == condition) && n.kind == NodeKind::RootCause)
    {
        candidates += 1;
        let (base_weight, base_path) = best_path(
            &graph.edges,
            &node.id,
            &|id| symptoms.contains(&id),
        )
        .unwrap_or((0.0, node.id.clone()));
        let mut contributions = Vec::new();
        let mut tested = 0usize;
        let mut untested: Vec<String> = Vec::new();
        let mut any_touch = false;

        for probe in &node.probes {
            match fact_by_analyte.get(probe.analyte.as_str()) {
                None => untested.push(probe.analyte.clone()),
                Some(fact) => {
                    tested += 1;
                    any_touch = true;
                    let delta = match probe.expect.as_deref() {
                        None => 0.0, // existence-only probe: informative, no direction
                        Some(expected) => {
                            if status_present(&fact.latest, expected) {
                                probe.weight
                            } else if status_present(&fact.latest, opposite(expected)) {
                                -(probe.weight / 2.0)
                            } else {
                                0.0 // within range: no directional signal
                            }
                        }
                    };
                    if delta != 0.0 {
                        contributions.push(Contribution {
                            source: format!("ledger:facts/{}", fact.fact_id),
                            detail: format!(
                                "{} {} {} (expect {})",
                                probe.analyte,
                                fact.latest.value,
                                fact.latest
                                    .assessments
                                    .first()
                                    .map(|a| a.unit.label())
                                    .unwrap_or(""),
                                probe.expect.as_deref().unwrap_or("any")
                            ),
                            delta,
                        });
                    }
                }
            }
        }

        for evidence in responses {
            let Some(pattern) = graph.get_response(&evidence.response_id) else {
                continue;
            };
            if let Some(effect) = pattern.effects.iter().find(|e| e.node == node.id) {
                any_touch = true;
                contributions.push(Contribution {
                    source: format!("ledger:treatment_responses/{}", evidence.ledger_id),
                    detail: format!("{} ({})", pattern.name, pattern.id),
                    delta: effect.weight,
                });
            }
        }

        if any_touch {
            touched += 1;
        }
        let sum: f64 = contributions.iter().map(|c| c.delta).sum();
        let score = clamp_score(base_weight + sum);
        causes.push(ScoredCause {
            node: node.id.clone(),
            name: node.name.clone(),
            score,
            base: base_weight,
            base_path,
            contributions,
            probes_total: node.probes.len(),
            probes_tested: tested,
            untested_probes: untested,
            tier: node.tier.clone(),
            summary: node.summary.clone(),
            review_by: node.review_by.clone(),
            suggested_hypothesis: format!(
                "{} may be driven by {} (score {:.2}: base {:.2} {:+.2})",
                condition, node.name, score, base_weight, sum
            ),
        });
    }

    causes.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.cmp(&b.node))
    });
    let coverage = if candidates == 0 {
        0.0
    } else {
        touched as f64 / candidates as f64
    };
    Analysis { condition: condition.into(), causes, coverage }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::{Confidence, RangeAssessment, RangeKind};

    fn assessment(kind: RangeKind, status: RangeStatus) -> RangeAssessment {
        RangeAssessment {
            kind,
            low: 0.0,
            high: 1.0,
            unit: crate::units::Unit::Ratio,
            status,
            source: "t".into(),
            pmid: None,
            confidence: Confidence::High,
            review_by: None,
        }
    }

    fn node(id: &str, kind: NodeKind, probes: Vec<Probe>) -> CausalNode {
        CausalNode {
            id: id.into(),
            kind,
            name: id.to_string(),
            summary: String::new(),
            conditions: vec!["acne".into()],
            mechanism_family: None,
            probes,
            tier: "C".into(),
            sources: vec![],
            review_by: None,
        }
    }

    fn edge(from: &str, to: &str, weight: f64) -> CausalEdge {
        CausalEdge { from: from.into(), to: to.into(), weight, tier: "C".into(), sources: vec![], review_by: None }
    }

    fn graph() -> CausalGraph {
        CausalGraph::from_parts(
            vec![
                node("gut", NodeKind::RootCause, vec![]),
                node("androgen", NodeKind::RootCause, vec![]),
                node("sebum", NodeKind::Mechanism, vec![]),
                node("lesions", NodeKind::Symptom, vec![]),
            ],
            vec![
                edge("gut", "lesions", 0.35),
                edge("androgen", "sebum", 0.5),
                edge("sebum", "lesions", 0.6),
            ],
            vec![],
        )
        .unwrap()
    }

    fn fact(analyte: &str, status: RangeStatus, fact_id: &str) -> EvidenceFact {
        EvidenceFact {
            fact_id: fact_id.into(),
            latest: LatestValue {
                analyte: analyte.into(),
                value: 1.0,
                assessments: vec![assessment(RangeKind::Functional, status)],
            },
        }
    }

    #[test]
    fn base_scores_are_max_path_products() {
        let out = analyze(&graph(), "acne", &[], &[]);
        assert_eq!(out.causes.len(), 2);
        // gut: direct 0.35. androgen: max(0.5*0.6=0.30) -> 0.30.
        let gut = out.causes.iter().find(|c| c.node == "gut").unwrap();
        let androgen = out.causes.iter().find(|c| c.node == "androgen").unwrap();
        assert!((gut.base - 0.35).abs() < 1e-9);
        assert!((androgen.base - 0.30).abs() < 1e-9);
        assert_eq!(androgen.base_path, "androgen -> sebum -> lesions");
        assert!((out.coverage - 0.0).abs() < 1e-9);
    }

    #[test]
    fn probe_signals_move_scores_both_ways() {
        let mut g = graph();
        // gut gets probes: none (blood-invisible); androgen gets shbg/below.
        let androgen = g.nodes.iter_mut().find(|n| n.id == "androgen").unwrap();
        androgen.probes = vec![Probe {
            analyte: "shbg".into(),
            expect: Some("below".into()),
            weight: 0.2,
        }];
        // Committed fact: SHBG ABOVE functional -> contradicts -> -0.1.
        let facts = vec![fact("shbg", RangeStatus::Above, "lab_1")];
        let out = analyze(&g, "acne", &facts, &[]);
        let androgen = out.causes.iter().find(|c| c.node == "androgen").unwrap();
        assert!((androgen.score - (0.30 - 0.1)).abs() < 1e-9);
        assert_eq!(androgen.contributions.len(), 1);
        assert!(androgen.contributions[0].source.starts_with("ledger:facts/lab_1"));
        assert!((out.coverage - 0.5).abs() < 1e-9, "one of two causes touched");

        // Matching signal: SHBG BELOW -> +0.2, clamped at most to 0.99.
        let facts_match = vec![fact("shbg", RangeStatus::Below, "lab_2")];
        let out2 = analyze(&g, "acne", &facts_match, &[]);
        let a2 = out2.causes.iter().find(|c| c.node == "androgen").unwrap();
        assert!((a2.score - 0.50).abs() < 1e-9);
    }

    #[test]
    fn treatment_responses_are_evidence() {
        let g = CausalGraph::from_parts(
            graph().nodes().to_vec(),
            graph().edges.clone(),
            vec![TreatmentResponse {
                id: "antibiotic_transient".into(),
                name: "Antibiotic transient clearance".into(),
                pattern: "clears, recurs".into(),
                effects: vec![Effect { node: "gut".into(), weight: 0.5 }],
                tier: "C".into(),
                sources: vec![],
                review_by: None,
            }],
        )
        .unwrap();
        let responses = vec![EvidenceResponse {
            ledger_id: "tres_1".into(),
            response_id: "antibiotic_transient".into(),
        }];
        let out = analyze(&g, "acne", &[], &responses);
        let gut = out.causes.iter().find(|c| c.node == "gut").unwrap();
        assert!((gut.score - 0.85).abs() < 1e-9, "0.35 base + 0.5 response");
        assert!(gut.contributions[0].source.starts_with("ledger:treatment_responses/tres_1"));
        assert!((out.coverage - 0.5).abs() < 1e-9);
    }

    #[test]
    fn untested_probes_listed_and_coverage_counts_touches() {
        let mut g = graph();
        let gut = g.nodes.iter_mut().find(|n| n.id == "gut").unwrap();
        gut.probes = vec![
            Probe { analyte: "a_defined".into(), expect: Some("above".into()), weight: 0.1 },
            Probe { analyte: "b_missing".into(), expect: None, weight: 0.0 },
        ];
        let facts = vec![fact("a_defined", RangeStatus::Within, "lab_9")];
        let out = analyze(&g, "acne", &facts, &[]);
        let gut = out.causes.iter().find(|c| c.node == "gut").unwrap();
        assert_eq!(gut.probes_tested, 1);
        assert_eq!(gut.untested_probes, vec!["b_missing".to_string()]);
        // within-range on a directional probe: no delta, but it IS a touch.
        assert!(gut.contributions.is_empty());
        assert!((gut.score - 0.35).abs() < 1e-9);
        assert!((out.coverage - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unknown_condition_yields_empty_analysis() {
        let out = analyze(&graph(), "ghost_condition", &[], &[]);
        assert!(out.causes.is_empty());
        assert_eq!(out.coverage, 0.0);
    }

    #[test]
    fn cyclic_or_dangling_graphs_rejected() {
        let err = CausalGraph::from_parts(
            vec![node("a", NodeKind::RootCause, vec![])],
            vec![edge("a", "ghost", 0.5)],
            vec![],
        );
        assert!(err.is_err());
        let err2 = CausalGraph::from_parts(
            vec![
                node("a", NodeKind::RootCause, vec![]),
                node("b", NodeKind::Mechanism, vec![]),
            ],
            vec![edge("a", "b", 0.5), edge("b", "a", 0.5)],
            vec![],
        );
        assert!(err2.is_err());
    }
}
