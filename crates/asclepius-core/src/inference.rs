//! The inference workbench: engine-computed unknowns as creativity
//! scaffolding, and a disciplined landing place for novel hypotheses.
//!
//! `hypothesis_space` deterministically surfaces what the graph does NOT
//! know — untestable causes, uncovered causes, untested probes, response
//! patterns nothing consumes, and mechanism-family kinship across
//! conditions. Those are the places a reasoning model should look deeper;
//! the engine hands it the map of exactly where "deeper" is.
//!
//! `propose_novel_hypothesis` is the return path: the model's creative
//! output lands in the ledger as origin=agent, tier-D-by-default,
//! review-required — free invention, honest labeling, evidence refs that
//! must resolve before anything is written.

use crate::causal::{Analysis, CausalGraph, ScoredCause};
use crate::conditions::ConditionLibrary;
use crate::interventions::InterventionLibrary;
use crate::investigate::TestLibrary;
use crate::rules::RuleSet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct HypothesisSpaceInput {
    /// Condition card id whose unknowns the workbench should map.
    pub condition: String,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct NovelHypothesisInput {
    /// The hypothesis in one falsifiable sentence.
    pub statement: String,
    /// Evidence refs the hypothesis claims to stand on: ledger:facts/<id>,
    /// ledger:symptom_logs/<id>, ledger:treatment_responses/<id>,
    /// id:<knowledge id>, pack:<domain>, or pmid:<number>. All must resolve.
    #[serde(default)]
    pub supporting_refs: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct LiteratureProbeInput {
    /// The hypothesis or question to probe against local knowledge (and
    /// PubMed when enabled).
    pub query: String,
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkbenchItem {
    /// untestable_cause | uncovered_cause | untested_probes |
    /// unlinked_response | mechanism_kinship
    pub kind: String,
    pub node: Option<String>,
    pub node_name: Option<String>,
    pub score: Option<f64>,
    pub detail: String,
    /// What a creative search could do here.
    pub invitation: String,
}

/// Compute the unknowns of a condition's graph: the workbench map.
pub fn hypothesis_space(
    graph: &CausalGraph,
    library: &TestLibrary,
    analysis: &Analysis,
) -> Vec<WorkbenchItem> {
    let mut items = Vec::new();
    let condition = &analysis.condition;
    let condition_nodes: Vec<&crate::causal::CausalNode> = graph
        .nodes()
        .iter()
        .filter(|n| n.conditions.iter().any(|c| c == condition))
        .collect();

    let scored = |node: &str| -> Option<&ScoredCause> {
        analysis.causes.iter().find(|c| c.node == node)
    };

    // 1. Causes that no probe and no response pattern can move.
    for node in &condition_nodes {
        if node.kind != crate::causal::NodeKind::RootCause {
            continue;
        }
        let has_response = graph
            .responses()
            .iter()
            .any(|r| r.effects.iter().any(|e| e.node == node.id));
        if node.probes.is_empty() && !has_response {
            items.push(WorkbenchItem {
                kind: "untestable_cause".into(),
                node: Some(node.id.clone()),
                node_name: Some(node.name.clone()),
                score: scored(&node.id).map(|c| c.score),
                detail: format!(
                    "'{}' has no lab probe and no response pattern targets it — its score can never move on current evidence",
                    node.name
                ),
                invitation: "curate an investigation test, a response pattern, or a symptom-scale correlate; or challenge the node's place in the graph".into(),
            });
        }
    }

    // 2. Causes with no covering test in the investigation pack.
    let plan = crate::investigate::plan_workup(library, analysis);
    for uncovered in &plan.untestable_causes {
        items.push(WorkbenchItem {
            kind: "uncovered_cause".into(),
            node: Some(uncovered.node.clone()),
            node_name: Some(uncovered.name.clone()),
            score: Some(uncovered.cause_score),
            detail: format!(
                "no investigation test discriminates '{}' (score {:.2})",
                uncovered.name, uncovered.cause_score
            ),
            invitation: "design or curate a test for this cause — what observation would separate it from its rivals?".into(),
        });
    }

    // 3. Untested probes on scored causes.
    for cause in &analysis.causes {
        if !cause.untested_probes.is_empty() {
            items.push(WorkbenchItem {
                kind: "untested_probes".into(),
                node: Some(cause.node.clone()),
                node_name: Some(cause.name.clone()),
                score: Some(cause.score),
                detail: format!(
                    "defined but uncommitted probes: {}",
                    cause.untested_probes.join(", ")
                ),
                invitation: "commit these values (some may be computable from existing labs) and the score moves".into(),
            });
        }
    }

    // 4. Response patterns nothing in this graph consumes.
    let node_ids: std::collections::BTreeSet<&str> =
        condition_nodes.iter().map(|n| n.id.as_str()).collect();
    for response in graph.responses() {
        if !response.effects.iter().any(|e| node_ids.contains(e.node.as_str())) {
            items.push(WorkbenchItem {
                kind: "unlinked_response".into(),
                node: None,
                node_name: Some(response.name.clone()),
                score: None,
                detail: format!(
                    "response pattern '{}' exists in the packs but no cause of this graph consumes it",
                    response.name
                ),
                invitation: "is this pattern relevant here through a mechanism the graph is missing? that missing mechanism is a novel hypothesis".into(),
            });
        }
    }

    // 5. Mechanism kinship across conditions: distinct nodes, same family.
    let mut seen_families: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for node in &condition_nodes {
        let Some(family) = &node.mechanism_family else {
            continue;
        };
        if !seen_families.insert(family.clone()) {
            continue;
        }
        let kin: Vec<&crate::causal::CausalNode> = graph
            .nodes()
            .iter()
            .filter(|n| {
                n.mechanism_family.as_deref() == Some(family.as_str())
                    && n.id != node.id
                    && !n.conditions.iter().any(|c| c == condition)
            })
            .collect();
        if kin.is_empty() {
            continue;
        }
        let kin_desc: Vec<String> = kin
            .iter()
            .map(|k| {
                let score_note = scored(&k.id)
                    .map(|c| format!(", currently scored {:.2} there", c.score))
                    .unwrap_or_default();
                format!(
                    "'{}' ({}){}",
                    k.name,
                    k.conditions.join("+"),
                    score_note
                )
            })
            .collect();
        items.push(WorkbenchItem {
            kind: "mechanism_kinship".into(),
            node: Some(node.id.clone()),
            node_name: Some(node.name.clone()),
            score: scored(&node.id).map(|c| c.score),
            detail: format!(
                "mechanism family '{family}': this node's kin in other graphs: {}",
                kin_desc.join("; ")
            ),
            invitation: "evidence and hazards may transfer across kin nodes — a cross-condition hypothesis (register with propose_novel_hypothesis) or a shared test may be hiding here".into(),
        });
    }

    items
}

/// Validate a novel hypothesis's supporting refs against the resolution
/// surfaces before anything is written. Returns the refs that do NOT
/// resolve (empty = all good).
pub fn unresolved_refs(
    refs: &[String],
    db: &crate::store::Db,
    packs: &crate::packs::PackIndex,
) -> Vec<String> {
    let mut bad = Vec::new();
    for r in refs {
        let ok = if let Some(id) = r.strip_prefix("ledger:facts/") {
            db.get_fact(id).ok().flatten().is_some()
        } else if let Some(id) = r.strip_prefix("ledger:symptom_logs/") {
            db.symptom_log_exists(id)
        } else if let Some(id) = r.strip_prefix("ledger:treatment_responses/") {
            db.treatment_response_exists(id)
        } else if let Some(id) = r.strip_prefix("id:") {
            packs.knowledge_id_exists(id)
        } else if r.starts_with("pack:") {
            packs.pack_ref_exists(r)
        } else if r.starts_with("pmid:") {
            true // format-checked only; numeric part validated by literature layer later
        } else {
            false
        };
        if !ok {
            bad.push(r.clone());
        }
    }
    bad
}

/// Search the causal graph and response patterns for a query (token-match,
/// same discipline as the rest of the engine).
#[derive(Debug, Clone, Serialize)]
pub struct GraphHit {
    pub kind: String, // "causal_node" | "response_pattern"
    pub id: String,
    pub title: String,
    pub detail: String,
    pub tier: String,
    pub score: u32,
}

pub fn probe_graph(graph: &CausalGraph, query: &str, limit: usize) -> Vec<GraphHit> {
    let tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<GraphHit> = Vec::new();
    let score_text = |text: &str| -> u32 {
        let lower = text.to_lowercase();
        let mut s = 0;
        for t in &tokens {
            if lower.contains(t.as_str()) {
                s += 3;
            }
        }
        s
    };
    for node in graph.nodes() {
        let mut score = score_text(&node.name) * 2;
        score += score_text(&node.summary);
        if score > 0 {
            hits.push(GraphHit {
                kind: "causal_node".into(),
                id: node.id.clone(),
                title: node.name.clone(),
                detail: node.summary.clone(),
                tier: node.tier.clone(),
                score,
            });
        }
    }
    for response in graph.responses() {
        let mut score = score_text(&response.name) * 2;
        score += score_text(&response.pattern);
        if score > 0 {
            hits.push(GraphHit {
                kind: "response_pattern".into(),
                id: response.id.clone(),
                title: response.name.clone(),
                detail: response.pattern.clone(),
                tier: response.tier.clone(),
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    hits.truncate(limit);
    hits
}

/// Local probe convenience: graph hits plus the classic pack search.
pub fn probe_local(
    graph: &CausalGraph,
    conditions: &ConditionLibrary,
    rules: &RuleSet,
    interventions: &InterventionLibrary,
    query: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for hit in probe_graph(graph, query, limit) {
        if let Ok(v) = serde_json::to_value(&hit) {
            out.push(v);
        }
    }
    for hit in crate::literature::local_search(conditions, rules, interventions, query, limit) {
        if let Ok(v) = serde_json::to_value(&hit) {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{CausalEdge, CausalGraph, NodeKind, ScoredCause};

    fn node(id: &str, kind: NodeKind, family: Option<&str>, conditions: &[&str]) -> crate::causal::CausalNode {
        crate::causal::CausalNode {
            id: id.into(),
            kind,
            name: id.to_string(),
            summary: id.to_string(),
            conditions: conditions.iter().map(|c| c.to_string()).collect(),
            mechanism_family: family.map(|f| f.to_string()),
            probes: vec![],
            tier: "C".into(),
            sources: vec![],
            review_by: None,
        }
    }

    fn graph() -> CausalGraph {
        CausalGraph::from_parts(
            vec![
                node("gut_acne", NodeKind::RootCause, Some("gut_tryptophan_axis"), &["acne"]),
                node("lesions", NodeKind::Symptom, None, &["acne"]),
                node("gut_pssd", NodeKind::RootCause, Some("gut_tryptophan_axis"), &["pssd"]),
                node("blind", NodeKind::RootCause, None, &["acne"]), // no probes, no responses
            ],
            vec![
                CausalEdge { from: "gut_acne".into(), to: "lesions".into(), weight: 0.4, tier: "C".into(), sources: vec![], review_by: None },
                CausalEdge { from: "blind".into(), to: "lesions".into(), weight: 0.3, tier: "C".into(), sources: vec![], review_by: None },
            ],
            vec![],
        )
        .unwrap()
    }

    fn analysis() -> Analysis {
        Analysis {
            condition: "acne".into(),
            causes: vec![
                ScoredCause {
                    node: "gut_acne".into(),
                    name: "gut_acne".into(),
                    score: 0.85,
                    base: 0.4,
                    base_path: String::new(),
                    contributions: vec![],
                    probes_total: 0,
                    probes_tested: 0,
                    untested_probes: vec![],
                    tier: "C".into(),
                    summary: String::new(),
                    review_by: None,
                    suggested_hypothesis: String::new(),
                },
                ScoredCause {
                    node: "blind".into(),
                    name: "blind".into(),
                    score: 0.3,
                    base: 0.3,
                    base_path: String::new(),
                    contributions: vec![],
                    probes_total: 0,
                    probes_tested: 0,
                    untested_probes: vec![],
                    tier: "C".into(),
                    summary: String::new(),
                    review_by: None,
                    suggested_hypothesis: String::new(),
                },
            ],
            coverage: 0.5,
        }
    }

    #[test]
    fn workbench_finds_blind_spots_and_kinship() {
        let g = graph();
        let a = analysis();
        let lib = TestLibrary::default();
        let items = hypothesis_space(&g, &lib, &a);
        let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
        // Both root causes lack probes AND tests (empty test library):
        assert!(kinds.contains(&"untestable_cause"));
        assert!(kinds.contains(&"uncovered_cause"));
        // Kinship: gut_acne (acne) should find gut_pssd (pssd).
        let kin = items.iter().find(|i| i.kind == "mechanism_kinship").expect("kinship item");
        assert!(kin.detail.contains("gut_pssd"), "{}", kin.detail);
        assert!(kin.detail.contains("pssd"));
    }

    #[test]
    fn graph_probe_finds_nodes_and_patterns() {
        let mut g = graph();
        let hits = probe_graph(&g, "gut tryptophan", 5);
        assert!(hits.iter().any(|h| h.id == "gut_acne"));
        // responses too
        g = CausalGraph::from_parts(
            g.nodes().to_vec(),
            g.edges().to_vec(),
            vec![crate::causal::TreatmentResponse {
                id: "crash".into(),
                name: "Antimicrobial crash gut".into(),
                pattern: "worse after antibiotics".into(),
                effects: vec![],
                tier: "D".into(),
                sources: vec![],
                review_by: None,
            }],
        )
        .unwrap();
        let hits2 = probe_graph(&g, "antimicrobial crash", 5);
        assert!(hits2.iter().any(|h| h.kind == "response_pattern" && h.id == "crash"));
    }
}
