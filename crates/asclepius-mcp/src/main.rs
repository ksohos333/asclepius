use asclepius_core::analyte::AnalyteRegistry;
use asclepius_core::bridge::Bridge;
use asclepius_core::causal::{
    self, CausalGraph, EvidenceFact, EvidenceResponse, RootCauseAnalysisInput,
};
use asclepius_core::conditions::{ConditionLibrary, ConditionQuery};
use asclepius_core::derived::{
    DERIVED_IDS, FreeTInput, HomaInput, free_testosterone_vermeulen, homa_ir,
};
use asclepius_core::envelope::{add_days_iso, today_iso, ClaimKind, Envelope, EnvelopeBuilder};
use asclepius_core::inference::{
    HypothesisSpaceInput, LiteratureProbeInput, NovelHypothesisInput, hypothesis_space, probe_graph,
    unresolved_refs,
};
use asclepius_core::interventions::{
    FLAG_REQUIRES_BASELINE_LABS, InterventionLibrary, ProtocolInput, propose,
};
use asclepius_core::investigate::{PlanWorkupInput, TestLibrary, plan_workup};
use asclepius_core::literature::{LiteratureInput, PubMedClient, local_search};
use asclepius_core::navigate::{PhysicianBriefInput, RegionSet};
use asclepius_core::packs::{self, PackIndex};
use asclepius_core::panel::{
    AnalyteResult, CommitInput, PanelInput, normalize_panel, normalize_value,
};
use asclepius_core::profile::ProfileUpdate;
use asclepius_core::range::RangeSet;
use asclepius_core::rules::{
    LatestValue, RuleSet, TriageInput, TriageSet, interpret, triage_check,
};
use asclepius_core::store::{
    Db, HistoryQuery, HypothesisListInput, HypothesisRegisterInput, HypothesisUpdateInput,
    LineageNode, LineageQuery, TreatmentResponseRegisterInput, TrialBeginInput, TrialListInput,
    TrialLogInput, TrialReportInput, TrialStatusInput,
};
use asclepius_core::symptoms::{SymptomHistoryInput, SymptomLogInput, SymptomSet};
use asclepius_core::verify::verify_claims;
use asclepius_core::{NAME, VERSION};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct Asclepius {
    registry: Arc<AnalyteRegistry>,
    ranges: Arc<RangeSet>,
    rules: Arc<RuleSet>,
    conditions: Arc<ConditionLibrary>,
    interventions: Arc<InterventionLibrary>,
    triage: Arc<TriageSet>,
    causal: Arc<CausalGraph>,
    tests: Arc<TestLibrary>,
    regions: Arc<RegionSet>,
    symptoms: Arc<SymptomSet>,
    pubmed: Arc<PubMedClient>,
    bridge: Arc<Bridge>,
    db: Arc<Db>,
    packs: Arc<PackIndex>,
}

fn data_home() -> PathBuf {
    if let Ok(p) = std::env::var("ASCLEPIUS_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/asclepius")
}

fn packs_root() -> PathBuf {
    if let Ok(p) = std::env::var("ASCLEPIUS_PACKS") {
        return PathBuf::from(p);
    }
    let cwd = PathBuf::from("content-packs");
    if cwd.exists() {
        return cwd;
    }
    data_home().join("content-packs")
}

fn domain_error(e: asclepius_core::error::AsclepiusError) -> McpError {
    McpError::invalid_params(e.to_string(), None)
}

fn json_content(value: serde_json::Value) -> Result<ContentBlock, McpError> {
    ContentBlock::json(value).map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn inputs_map(inputs: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    if let Some(obj) = inputs.as_object() {
        for (k, v) in obj {
            map.insert(k.clone(), v.clone());
        }
    }
    map
}

fn lineage_claims(node: &LineageNode, b: &mut EnvelopeBuilder) {
    b.claim(
        ClaimKind::Measured,
        format!("fact {}: {} {}", node.fact.analyte, node.fact.value, node.fact.unit),
        serde_json::to_value(&node.fact).unwrap_or(serde_json::Value::Null),
        vec![format!("ledger:facts/{}", node.fact.id)],
    );
    for input in &node.inputs {
        lineage_claims(input, b);
    }
}

#[tool_router]
impl Asclepius {
    #[tool(
        description = "Liveness probe: server identity, version, patient scope, envelope schema, loaded knowledge counts, and any packs excluded by the load gate (degraded mode is reported here, never silent)."
    )]
    fn ping(&self) -> Result<CallToolResult, McpError> {
        let mut b = self.builder("ping");
        b.claim(
            ClaimKind::Meta,
            "server identity",
            serde_json::json!(NAME),
            vec!["engine:self".into()],
        )
        .claim(
            ClaimKind::Meta,
            "server version",
            serde_json::json!(VERSION),
            vec!["engine:self".into()],
        )
        .claim(
            ClaimKind::Meta,
            "envelope schema version",
            serde_json::json!(asclepius_core::envelope::ENVELOPE_SCHEMA_VERSION),
            vec!["engine:self".into()],
        )
        .claim(
            ClaimKind::Meta,
            "patient scope",
            serde_json::json!(self.db.patient()),
            vec!["engine:patient".into()],
        )
        .claim(
            ClaimKind::Meta,
            "loaded knowledge counts",
            serde_json::json!({
                "analytes": self.registry.all().len(),
                "ranges": self.ranges.len(),
                "rules": self.rules.len(),
                "condition_cards": self.conditions.all().len(),
                "interventions": self.interventions.all().len(),
                "triage_rules": self.triage.len(),
                "symptom_scales": self.symptoms.all().len(),
            }),
            vec![
                "pack:analytes".into(),
                "pack:ranges".into(),
                "pack:rules".into(),
                "pack:conditions".into(),
                "pack:interventions".into(),
                "pack:triage".into(),
                "pack:symptoms".into(),
            ],
        )
        .claim(
            ClaimKind::Meta,
            "packs excluded by the load gate (degraded mode)",
            serde_json::json!(self.packs.excluded()),
            vec!["engine:packs".into()],
        )
        .claim(
            ClaimKind::Meta,
            "knowledge gaps: analytes suggested/listed but not yet defined (curation queue)",
            serde_json::json!(self.packs.gaps()),
            vec!["engine:packs".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Current patient profile: sex, age, conditions, medications, supplements, goals. Ranges and interpretations are scoped by this profile. Persisted in the local ledger."
    )]
    fn profile_get(&self) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let mut b = self.builder("profile_get");
        b.claim(
            ClaimKind::Reported,
            "current patient profile as reported by the user",
            serde_json::to_value(&profile).unwrap_or(serde_json::Value::Null),
            vec!["ledger:profile".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Update the patient profile. Only provided fields change; omitted fields keep current values. List fields replace entirely when provided. Example: {\"sex\": \"male\", \"age_years\": 30, \"conditions\": [\"cystic acne\"], \"goals\": [\"clear skin\", \"optimize hormones\"]}"
    )]
    fn profile_set(
        &self,
        Parameters(update): Parameters<ProfileUpdate>,
    ) -> Result<CallToolResult, McpError> {
        let mut profile = self.db.profile().map_err(domain_error)?;
        profile.apply(&update);
        self.db.set_profile(&profile).map_err(domain_error)?;
        let mut b = self.builder("profile_set");
        b.claim(
            ClaimKind::Reported,
            "patient profile after this update, as stored in the ledger",
            serde_json::to_value(&profile).unwrap_or(serde_json::Value::Null),
            vec!["ledger:profile".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Commit a lab panel to the persistent append-only ledger. Each value is validated, unit-converted, and range-assessed against the current profile; valid items are stored as provenance-carrying facts under one panel id, invalid items are rejected individually with reasons. Use lab_normalize for dry-run interpretation without persisting."
    )]
    fn lab_commit(
        &self,
        Parameters(CommitInput {
            panel,
            collected_at,
            note,
        }): Parameters<CommitInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let profile = self.db.profile().map_err(domain_error)?;
        let mut valid: Vec<AnalyteResult> = Vec::new();
        let mut rejected: Vec<serde_json::Value> = Vec::new();
        for input in &panel {
            match normalize_value(&self.registry, &self.ranges, &profile, input) {
                Ok(res) => valid.push(res),
                Err(e) => rejected.push(serde_json::json!({
                    "analyte": input.analyte,
                    "error": e.to_string()
                })),
            }
        }
        let refs: Vec<&AnalyteResult> = valid.iter().collect();
        let mut b = self.builder("lab_commit");
        if refs.is_empty() && rejected.is_empty() {
            b.abstention(
                "lab_commit",
                "empty panel: nothing to commit",
                vec!["at least one {analyte, value}".into()],
            );
            return self.finish(b);
        }
        let committed_count = valid.len();
        if !refs.is_empty() {
            let (pid, ids) = self
                .db
                .commit_panel(
                    collected_at.as_deref(),
                    note.as_deref(),
                    &refs,
                    "lab_report",
                )
                .map_err(domain_error)?;
            b.claim(
                ClaimKind::Meta,
                "panel committed to the append-only ledger",
                serde_json::json!({
                    "panel_id": pid,
                    "committed": committed_count,
                    "rejected": rejected.len(),
                }),
                vec![format!("ledger:panels/{pid}")],
            );
            for (res, id) in valid.into_iter().zip(ids) {
                b.claim(
                    ClaimKind::Measured,
                    format!(
                        "{} = {} {} (committed)",
                        res.analyte_id, res.canonical_value, res.canonical_unit.label()
                    ),
                    serde_json::json!({
                        "analyte": res.analyte_id,
                        "value": res.canonical_value,
                        "unit": res.canonical_unit.label(),
                        "display_name": res.display_name,
                        "assessments": res.assessments,
                        "warnings": res.warnings,
                    }),
                    vec![format!("ledger:facts/{id}")],
                )
                .mark_stale_from_value(&today);
            }
        }
        for r in &rejected {
            b.claim(
                ClaimKind::Meta,
                format!("rejected: {}", r["analyte"].as_str().unwrap_or("?")),
                r.clone(),
                vec!["gate:normalize".into()],
            );
        }
        self.bridge.post(
            "status",
            &format!(
                "panel committed: {committed_count} analytes stored, {} rejected",
                rejected.len()
            ),
        );
        self.finish(b)
    }

    #[tool(
        description = "Query the fact ledger: chronological lab facts (current, non-superseded). Optional analyte filter (common names accepted), limit defaults to 50. The latest fact per analyte carries fresh range assessments against the current profile."
    )]
    fn lab_history(
        &self,
        Parameters(HistoryQuery { analyte, limit }): Parameters<HistoryQuery>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let profile = self.db.profile().map_err(domain_error)?;
        let limit = limit.unwrap_or(50).clamp(1, 500);
        let rows = self
            .db
            .history(analyte.as_deref(), limit)
            .map_err(domain_error)?;
        let mut b = self.builder("lab_history");
        if rows.is_empty() {
            b.abstention(
                "lab_history",
                "no committed facts match this query",
                vec![analyte.unwrap_or_else(|| "any analyte".into())],
            );
            return self.finish(b);
        }
        for r in &rows {
            b.claim(
                ClaimKind::Measured,
                format!("{}: {} {}", r.analyte, r.value, r.unit),
                serde_json::to_value(r).unwrap_or(serde_json::Value::Null),
                vec![format!("ledger:facts/{}", r.id)],
            );
        }
        let latest = rows.last().unwrap();
        let assessments = self
            .ranges
            .assess(&latest.analyte, profile.sex, profile.age_years, latest.value);
        b.claim(
            ClaimKind::Inferred,
            format!("range assessments for latest {} value", latest.analyte),
            serde_json::json!({
                "analyte": latest.analyte,
                "assessments": assessments,
            }),
            vec![format!("ledger:facts/{}", latest.id), "pack:ranges".into()],
        )
        .mark_stale_from_value(&today);
        if rows.len() >= 2 {
            let first = &rows[0];
            b.claim(
                ClaimKind::Meta,
                format!("trend for {}", latest.analyte),
                serde_json::json!({
                    "analyte": latest.analyte,
                    "first_value": first.value,
                    "latest_value": latest.value,
                    "n_points": rows.len(),
                }),
                vec![
                    format!("ledger:facts/{}", first.id),
                    format!("ledger:facts/{}", latest.id),
                ],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Log patient-reported symptom observations: single-item scales (0-10 style) and instrument items, each validated against its pack-defined scale before commit. Entries are patient-scoped rows in the append-only ledger — the symptom counterpart of lab_commit. Use symptom_history to read trends. Examples: {entries: [{scale_id: \"genital_sensation\", value: 4}, {scale_id: \"iief5_adapted\", item_id: \"q1\", value: 3}]}"
    )]
    fn symptom_log(
        &self,
        Parameters(input): Parameters<SymptomLogInput>,
    ) -> Result<CallToolResult, McpError> {
        if input.entries.is_empty() {
            return Err(McpError::invalid_params("entries is required", None));
        }
        let mut b = self.builder("symptom_log");
        if input.entries.is_empty() {
            b.abstention(
                "symptom_log",
                "no entries submitted",
                vec!["at least one {scale_id, value}".into()],
            );
            return self.finish(b);
        }
        let mut valid = Vec::new();
        for entry in &input.entries {
            match self.symptoms.get(&entry.scale_id) {
                None => {
                    b.claim(
                        ClaimKind::Meta,
                        format!("rejected: unknown scale {}", entry.scale_id),
                        serde_json::json!({
                            "scale_id": entry.scale_id,
                            "error": "unknown scale id; see pack symptom scales"
                        }),
                        vec!["gate:symptom_scale".into()],
                    );
                }
                Some(scale) => match scale.check_value(entry.item_id.as_deref(), entry.value) {
                    Ok(()) => valid.push(entry.clone()),
                    Err(e) => {
                        b.claim(
                            ClaimKind::Meta,
                            format!("rejected: {}", entry.scale_id),
                            serde_json::json!({
                                "scale_id": entry.scale_id,
                                "item_id": entry.item_id,
                                "value": entry.value,
                                "error": e
                            }),
                            vec!["gate:symptom_range".into()],
                        );
                    }
                },
            }
        }
        if valid.is_empty() {
            b.abstention(
                "symptom_log",
                "every submitted entry was rejected by scale validation",
                vec!["check scale ids, item ids, and value ranges".into()],
            );
            return self.finish(b);
        }
        let observed_default = asclepius_core::envelope::today_iso();
        let entries_with_dates: Vec<asclepius_core::symptoms::SymptomEntryInput> = valid
            .into_iter()
            .map(|mut e| {
                if e.observed_on.is_none() {
                    e.observed_on = Some(observed_default.clone());
                }
                e
            })
            .collect();
        let ids = self
            .db
            .log_symptoms(&entries_with_dates)
            .map_err(domain_error)?;
        for (entry, id) in entries_with_dates.iter().zip(ids) {
            let scale = self.symptoms.get(&entry.scale_id).expect("validated above");
            b.claim(
                ClaimKind::Measured,
                format!(
                    "{}{} = {} ({})",
                    scale.name,
                    entry.item_id.as_deref().map(|i| format!(":{i}")).unwrap_or_default(),
                    entry.value,
                    entry.observed_on.as_deref().unwrap_or("today")
                ),
                serde_json::json!({
                    "analyte": entry.scale_id,
                    "value": entry.value,
                    "unit": "score",
                    "item_id": entry.item_id,
                    "observed_on": entry.observed_on,
                    "note": entry.note,
                }),
                vec![format!("ledger:symptom_logs/{id}")],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Query the symptom ledger: observations newest-first, optional scale filter. Patient-reported outcomes are measured facts in this system — same provenance, same verifier."
    )]
    fn symptom_history(
        &self,
        Parameters(input): Parameters<SymptomHistoryInput>,
    ) -> Result<CallToolResult, McpError> {
        let limit = input.limit.unwrap_or(50).clamp(1, 500);
        let rows = self
            .db
            .symptom_logs(input.scale_id.as_deref(), limit)
            .map_err(domain_error)?;
        let mut b = self.builder("symptom_history");
        if rows.is_empty() {
            b.abstention(
                "symptom_history",
                "no symptom observations match this query",
                vec![input.scale_id.unwrap_or_else(|| "any scale".into())],
            );
            return self.finish(b);
        }
        b.claim(
            ClaimKind::Meta,
            format!("{} symptom observations, newest first", rows.len()),
            serde_json::to_value(&rows).unwrap_or(serde_json::Value::Null),
            vec!["ledger:symptom_logs".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Full provenance audit of a fact: recursively resolves derived-fact input chains down to raw lab facts. Every derived claim in this system can be traced to its measured inputs."
    )]
    fn fact_lineage(
        &self,
        Parameters(LineageQuery { id, depth }): Parameters<LineageQuery>,
    ) -> Result<CallToolResult, McpError> {
        let node = self
            .db
            .lineage(&id, depth.unwrap_or(4))
            .map_err(domain_error)?
            .ok_or_else(|| McpError::invalid_params(format!("fact not found: {id}"), None))?;
        let mut b = self.builder("fact_lineage");
        lineage_claims(&node, &mut b);
        self.finish(b)
    }

    #[tool(
        description = "Interpret the latest committed labs against the rule base: fires severity-tagged findings with sources, computes calculated free testosterone when total T + SHBG exist, and proactively lists untested analytes the fired rules suggest investigating next. The root-cause workup engine; requires labs committed via lab_commit."
    )]
    fn interpret_panel(&self) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let profile = self.db.profile().map_err(domain_error)?;
        if self.db.history(None, 1000).map_err(domain_error)?.is_empty() {
            let mut b = self.builder("interpret_panel");
            b.abstention(
                "interpret_panel",
                "no committed facts to interpret",
                vec!["commit labs with lab_commit first".into()],
            );
            return self.finish(b);
        }
        let latest = self.latest_facts(&profile)?;
        let interpretation = interpret(
            &self.rules,
            &latest.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        );
        let mut b = self.builder("interpret_panel");
        for (lv, fact_id) in &latest {
            b.claim(
                ClaimKind::Measured,
                format!("latest {}: {} {}", lv.analyte, lv.value, "canonical"),
                serde_json::json!({
                    "analyte": lv.analyte,
                    "value": lv.value,
                    "assessments": lv.assessments,
                }),
                vec![format!("ledger:facts/{fact_id}")],
            )
            .mark_stale_from_value(&today);
        }
        for finding in &interpretation.findings {
            b.claim(
                ClaimKind::Inferred,
                format!("rule fired: {}", finding.rule_id),
                serde_json::to_value(finding).unwrap_or(serde_json::Value::Null),
                vec![format!("id:{}", finding.rule_id), "pack:rules".into()],
            )
            .mark_stale_from_value(&today);
        }
        // Derived candidates: free T and HOMA-IR over committed facts.
        let find = |name: &str| latest.iter().find(|(l, _)| l.analyte == name);
        if let (Some((tt, tt_id)), Some((shbg, shbg_id))) =
            (find("testosterone_total"), find("shbg"))
        {
            let albumin = find("albumin");
            let input = FreeTInput {
                total_testosterone: tt.value,
                total_testosterone_unit: Some("ng/dL".into()),
                shbg: shbg.value,
                shbg_unit: Some("nmol/L".into()),
                albumin: albumin.map(|(a, _)| a.value),
                albumin_unit: albumin.map(|_| "g/dL".into()),
            };
            if let Ok(derived) = free_testosterone_vermeulen(&self.registry, &input) {
                let mut evidence = vec![format!("ledger:facts/{tt_id}"), format!("ledger:facts/{shbg_id}")];
                if let Some((_, alb_id)) = albumin {
                    evidence.push(format!("ledger:facts/{alb_id}"));
                }
                let inputs = inputs_map(&serde_json::json!({
                    "total_testosterone_nmol_l": derived.inputs["total_testosterone_nmol_l"],
                    "shbg_nmol_l": derived.inputs["shbg_nmol_l"],
                    "albumin_g_dl": derived.inputs["albumin_g_dl"],
                }));
                b.derived(
                    "calculated free testosterone (Vermeulen 1999)",
                    serde_json::to_value(&derived).unwrap_or(serde_json::Value::Null),
                    evidence,
                    "vermeulen1999",
                    inputs,
                );
            }
        }
        if let (Some((glu, glu_id)), Some((ins, ins_id))) = (find("glucose"), find("insulin")) {
            let input = HomaInput {
                glucose: glu.value,
                glucose_unit: Some("mg/dL".into()),
                insulin: ins.value,
                insulin_unit: Some("uU/mL".into()),
            };
            if let Ok(derived) = homa_ir(&self.registry, &input) {
                let inputs = inputs_map(&serde_json::json!({
                    "glucose_mg_dl": derived.inputs["glucose_mg_dl"],
                    "insulin_uu_ml": derived.inputs["insulin_uu_ml"],
                }));
                b.derived(
                    "calculated HOMA-IR (Matthews 1985)",
                    serde_json::to_value(&derived).unwrap_or(serde_json::Value::Null),
                    vec![format!("ledger:facts/{glu_id}"), format!("ledger:facts/{ins_id}")],
                    "matthews1985",
                    inputs,
                );
            }
        }
        if !interpretation.untested_suggestions.is_empty() {
            let fired: Vec<String> = interpretation
                .findings
                .iter()
                .map(|f| format!("id:{}", f.rule_id))
                .collect();
            b.claim(
                ClaimKind::Meta,
                "untested analytes the fired rules suggest investigating next",
                serde_json::json!(interpretation.untested_suggestions),
                fired,
            );
        }
        if interpretation.findings.is_empty() {
            b.abstention(
                "interpret_panel",
                "no rules fired: committed values sit inside every assessed range for this profile",
                vec!["more granular rules or broader panels".into()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Search the condition-card library (root-cause knowledge base): mechanisms, biomarkers, and honest evidence notes for patterns like low testosterone, thyroid dysfunction, insulin resistance, iron status, and cystic acne. Free-text query; returns scored matches."
    )]
    fn explore_condition(
        &self,
        Parameters(ConditionQuery { query, limit }): Parameters<ConditionQuery>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let limit = limit.unwrap_or(5).clamp(1, 20);
        let condition_hits = self.conditions.search(&query, limit);
        let intervention_hits = self.interventions.search(&query, limit);
        let mut b = self.builder("explore_condition");
        b.claim(
            ClaimKind::Meta,
            "search query",
            serde_json::json!(&query),
            vec!["query:explore_condition".into()],
        );
        for hit in &condition_hits {
            b.claim(
                ClaimKind::Curated,
                format!("condition card: {}", hit.card.name),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec!["pack:conditions".into()],
            )
            .mark_stale_from_value(&today);
        }
        for hit in &intervention_hits {
            b.claim(
                ClaimKind::Curated,
                format!("intervention card: {}", hit.card.name),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec!["pack:interventions".into()],
            )
            .mark_stale_from_value(&today);
        }
        if condition_hits.is_empty() && intervention_hits.is_empty() {
            b.abstention(
                "explore_condition",
                "no condition or intervention card matched the query",
                vec![query.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Register a clinical hypothesis in the ledger, e.g. 'elevated SHBG is suppressing free T'. Hypotheses are model-inference by definition: they carry fact/rule/PMID references and a review date, and are explicitly labeled as non-facts until supported or refuted by evidence. origin: 'user' (default), 'engine', or 'agent'."
    )]
    fn hypothesis_register(
        &self,
        Parameters(input): Parameters<HypothesisRegisterInput>,
    ) -> Result<CallToolResult, McpError> {
        if input.statement.trim().is_empty() {
            return Err(McpError::invalid_params("statement is required", None));
        }
        let id = self.db.add_hypothesis(&input).map_err(domain_error)?;
        let mut b = self.builder("hypothesis_register");
        b.claim(
            ClaimKind::Meta,
            "hypothesis registered as open in the ledger",
            serde_json::json!({
                "id": id,
                "status": "open",
                "origin": input.origin_value(),
                "review_by": input.review_by,
            }),
            vec![format!("ledger:hypotheses/{id}")],
        );
        self.finish(b)
    }

    #[tool(
        description = "List ledger hypotheses, optionally filtered by status (open, supported, refuted, superseded), newest first."
    )]
    fn hypothesis_list(
        &self,
        Parameters(HypothesisListInput { status, limit }): Parameters<HypothesisListInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let rows = self
            .db
            .hypotheses(status.as_ref(), limit.unwrap_or(20).clamp(1, 200))
            .map_err(domain_error)?;
        let mut b = self.builder("hypothesis_list");
        if rows.is_empty() {
            b.abstention(
                "hypothesis_list",
                "no hypotheses match this filter",
                vec!["register hypotheses with hypothesis_register".into()],
            );
            return self.finish(b);
        }
        b.claim(
            ClaimKind::Meta,
            "ledger hypotheses, newest first",
            serde_json::to_value(&rows).unwrap_or(serde_json::Value::Null),
            vec!["ledger:hypotheses".into()],
        )
        .mark_stale_from_value(&today);
        self.finish(b)
    }

    #[tool(
        description = "Update a hypothesis status as evidence accumulates: open, supported, refuted, or superseded. The statement itself is immutable; the lifecycle is the point."
    )]
    fn hypothesis_update(
        &self,
        Parameters(HypothesisUpdateInput { id, status }): Parameters<HypothesisUpdateInput>,
    ) -> Result<CallToolResult, McpError> {
        let changed = self
            .db
            .set_hypothesis_status(&id, status)
            .map_err(domain_error)?;
        if !changed {
            return Err(McpError::invalid_params(
                format!("hypothesis not found: {id}"),
                None,
            ));
        }
        let mut b = self.builder("hypothesis_update");
        b.claim(
            ClaimKind::Meta,
            "hypothesis status updated",
            serde_json::json!({ "id": id, "status": status.as_str() }),
            vec![format!("ledger:hypotheses/{id}")],
        );
        self.finish(b)
    }

    #[tool(
        description = "Root-cause analysis over the causal graph: ranks the candidate root causes of a condition (e.g. \"acne_root_causes\") by evidence — graph base rates, committed lab facts against each cause's probes (matching status adds, contradicting subtracts), and registered treatment responses as Bayesian-style shifts. Every score carries its full contribution trail and a coverage ratio; untested probes are listed per cause. Proposal-mode: suggested hypotheses are registration-ready payloads for hypothesis_register — the engine proposes, you dispose."
    )]
    fn root_cause_analysis(
        &self,
        Parameters(input): Parameters<RootCauseAnalysisInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let known: Vec<&str> = self
            .conditions
            .all()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        if !known.contains(&input.condition.as_str()) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown condition: {}. known cards: {}",
                    input.condition,
                    known.join(", ")
                ),
                None,
            ));
        }
        if !self.causal.has_condition(&input.condition) {
            let mut b = self.builder("root_cause_analysis");
            b.abstention(
                "root_cause_analysis",
                format!(
                    "no causal graph exists for '{}' yet — the condition card has prose but no traversable nodes",
                    input.condition
                ),
                vec![format!("curate content-packs/causal/ for {}", input.condition)],
            );
            return self.finish(b);
        }
        let profile = self.db.profile().map_err(domain_error)?;
        let latest = self.latest_facts(&profile)?;
        let facts: Vec<EvidenceFact> = latest
            .iter()
            .map(|(lv, id)| EvidenceFact {
                fact_id: id.clone(),
                latest: lv.clone(),
            })
            .collect();
        let responses: Vec<EvidenceResponse> = self
            .db
            .treatment_responses(200)
            .map_err(domain_error)?
            .into_iter()
            .map(|row| EvidenceResponse {
                ledger_id: row.id,
                response_id: row.response_id,
            })
            .collect();
        let analysis = causal::analyze(&self.causal, &input.condition, &facts, &responses);
        let mut b = self.builder("root_cause_analysis");
        b.claim(
            ClaimKind::Meta,
            "condition under analysis and evidence coverage",
            serde_json::json!({
                "condition": analysis.condition,
                "candidate_causes": analysis.causes.len(),
                "coverage": analysis.coverage,
                "committed_facts": facts.len(),
                "registered_treatment_responses": responses.len(),
            }),
            vec!["pack:causal".into()],
        );
        for cause in &analysis.causes {
            let mut evidence = vec![format!("id:{}", cause.node), "pack:causal".into()];
            for c in &cause.contributions {
                evidence.push(c.source.clone());
            }
            b.claim(
                ClaimKind::Inferred,
                format!("{}: score {:.2} (base {:.2})", cause.name, cause.score, cause.base),
                serde_json::to_value(cause).unwrap_or(serde_json::Value::Null),
                evidence,
            )
            .mark_stale_from_value(&today)
            .tier(&cause.tier);
        }
        b.claim(
            ClaimKind::Meta,
            "registration-ready hypotheses (proposal-mode: commit accepted ones via hypothesis_register)",
            serde_json::json!(analysis
                .causes
                .iter()
                .map(|c| serde_json::json!({
                    "statement": c.suggested_hypothesis,
                    "origin": "user",
                    "note": "engine-proposed via root_cause_analysis",
                }))
                .collect::<Vec<_>>()),
            vec!["pack:causal".into()],
        );
        if analysis.causes.is_empty() {
            b.abstention(
                "root_cause_analysis",
                "graph has no candidate root causes for this condition",
                vec![input.condition.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Register a treatment response the patient actually experienced, e.g. antibiotics cleared the acne and it recurred within months (response_id: antibiotic_transient_clearance). The pattern's effects shift the causal graph's root-cause scores: treatment history is diagnostic evidence. Response ids and their patterns come from the treatment-responses pack — call with no arguments to see the error listing known patterns."
    )]
    fn treatment_response_register(
        &self,
        Parameters(input): Parameters<TreatmentResponseRegisterInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let Some(pattern) = self.causal.get_response(&input.response_id) else {
            let ids: Vec<&str> = self.causal.responses().iter().map(|r| r.id.as_str()).collect();
            return Err(McpError::invalid_params(
                format!(
                    "unknown response pattern: {}. known patterns: {}",
                    input.response_id,
                    ids.join(", ")
                ),
                None,
            ));
        };
        let id = self
            .db
            .add_treatment_response(&input)
            .map_err(domain_error)?;
        let mut b = self.builder("treatment_response_register");
        b.claim(
            ClaimKind::Curated,
            format!("pattern: {}", pattern.name),
            serde_json::json!({
                "id": pattern.id,
                "name": pattern.name,
                "pattern": pattern.pattern,
                "effects": pattern.effects,
                "tier": pattern.tier,
                "sources": pattern.sources,
                "review_by": pattern.review_by,
            }),
            vec!["pack:treatment-responses".into()],
        )
        .mark_stale_from_value(&today)
        .tier(&pattern.tier);
        b.claim(
            ClaimKind::Meta,
            "treatment response committed to the ledger as diagnostic evidence",
            serde_json::json!({
                "ledger_id": id,
                "response_id": input.response_id,
                "note": input.note,
            }),
            vec![format!("ledger:treatment_responses/{id}")],
        );
        self.finish(b)
    }

    #[tool(
        description = "Workup planner: ranks investigation tests by expected discrimination between the live root-cause hypotheses of a condition (e.g. \"acne_root_causes\") — discrimination = Σ (cause score × test resolution), divided by cost band. Each recommendation names which causes it probes at their current scores, specimen, availability notes, and honest caveats; causes with no covering test are surfaced as gaps. Region-scoped routing (which doctor orders it, how to get it covered) arrives with the pathways pack."
    )]
    fn plan_workup(
        &self,
        Parameters(input): Parameters<PlanWorkupInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let known: Vec<&str> = self
            .conditions
            .all()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        if !known.contains(&input.condition.as_str()) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown condition: {}. known cards: {}",
                    input.condition,
                    known.join(", ")
                ),
                None,
            ));
        }
        if !self.causal.has_condition(&input.condition) {
            let mut b = self.builder("plan_workup");
            b.abstention(
                "plan_workup",
                format!(
                    "no causal graph exists for '{}' — nothing to discriminate",
                    input.condition
                ),
                vec![format!("curate content-packs/causal/ for {}", input.condition)],
            );
            return self.finish(b);
        }
        let profile = self.db.profile().map_err(domain_error)?;
        let latest = self.latest_facts(&profile)?;
        let facts: Vec<EvidenceFact> = latest
            .iter()
            .map(|(lv, id)| EvidenceFact {
                fact_id: id.clone(),
                latest: lv.clone(),
            })
            .collect();
        let responses: Vec<EvidenceResponse> = self
            .db
            .treatment_responses(200)
            .map_err(domain_error)?
            .into_iter()
            .map(|row| EvidenceResponse {
                ledger_id: row.id,
                response_id: row.response_id,
            })
            .collect();
        let analysis = causal::analyze(&self.causal, &input.condition, &facts, &responses);
        let plan = plan_workup(&self.tests, &analysis);
        let mut b = self.builder("plan_workup");
        b.claim(
            ClaimKind::Meta,
            "workup target and planner inputs",
            serde_json::json!({
                "condition": plan.condition,
                "candidate_causes": analysis.causes.len(),
                "coverage": analysis.coverage,
                "tests_ranked": plan.ranked_tests.len(),
                "tests_in_library": self.tests.all().len(),
            }),
            vec!["pack:tests".into(), "pack:causal".into()],
        );
        for test in &plan.ranked_tests {
            let mut evidence = vec![format!("id:{}", test.id), "pack:tests".into()];
            for probed in &test.probes_causes {
                evidence.push(format!("id:{}", probed.node));
            }
            b.claim(
                ClaimKind::Inferred,
                format!(
                    "{}: discrimination {:.2} (rank {:.2}, {} cost)",
                    test.name, test.discrimination, test.rank_value, test.cost_band
                ),
                serde_json::to_value(test).unwrap_or(serde_json::Value::Null),
                evidence,
            )
            .mark_stale_from_value(&today)
            .tier(&test.tier);
        }
        for uncovered in &plan.untestable_causes {
            b.abstention(
                format!("plan_workup:{}", uncovered.node),
                format!(
                    "no available test discriminates '{}' (score {:.2}); {}",
                    uncovered.name, uncovered.cause_score, uncovered.note
                ),
                vec![format!("test covering {}", uncovered.node)],
            );
        }
        if plan.ranked_tests.is_empty() && plan.untestable_causes.is_empty() {
            b.abstention(
                "plan_workup",
                "no candidate causes and no applicable tests for this condition",
                vec![input.condition.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "The waiting-room advocate: builds a physician brief for a condition in YOUR region (profile_set {region: {country: \"GR\"}} first). Routes specialties in evidence order (e.g. gastroenterology first when the gut hypothesis leads), gives bilingual request scripts (what to say, verbatim), acquisition routes for the ranked workup tests (private lab vs referral, cost bands), and legal classes of relevant items. Region data is last-reviewed pack content, never inferred — missing region or missing routes abstain loudly. Legal classes are data, not legal advice."
    )]
    fn physician_brief(
        &self,
        Parameters(input): Parameters<PhysicianBriefInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let known: Vec<&str> = self
            .conditions
            .all()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        if !known.contains(&input.condition.as_str()) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown condition: {}. known cards: {}",
                    input.condition,
                    known.join(", ")
                ),
                None,
            ));
        }
        let profile = self.db.profile().map_err(domain_error)?;
        let Some(region_profile) = &profile.region else {
            let mut b = self.builder("physician_brief");
            b.abstention(
                "physician_brief",
                "no region set — routing, coverage, and legal classes are region data and are never inferred",
                vec!["profile_set {region: {country: \"GR\"}}".into()],
            );
            return self.finish(b);
        };
        let Some(region) = self.regions.resolve(&region_profile.country) else {
            let mut b = self.builder("physician_brief");
            b.abstention(
                "physician_brief",
                format!(
                    "no region pack for country '{}' — curate content-packs/regions/{}.toml or pick a supported country",
                    region_profile.country,
                    region_profile.country.to_lowercase()
                ),
                self.regions.countries().iter().map(|c| c.to_string()).collect(),
            );
            return self.finish(b);
        };
        let mut b = self.builder("physician_brief");
        b.claim(
            ClaimKind::Meta,
            "resolved region layers (deepest wins; provenance per entry)",
            serde_json::json!({
                "country": region.country,
                "name": region.country_name,
                "bloc": region.bloc,
                "locale": region.locale,
                "layers": region.layers,
            }),
            vec!["pack:regions".into()],
        );
        // Specialty routing for the condition.
        match region.physician_route(&input.condition) {
            Some(route) => {
                for entry in &route.value.rank {
                    b.claim(
                        ClaimKind::Curated,
                        format!("specialty: {}", entry.specialty),
                        serde_json::json!({
                            "specialty": entry.specialty,
                            "when": entry.when,
                            "script_el": entry.script_el,
                            "script_en": entry.script_en,
                            "resolved_from": route.resolved_from,
                        }),
                        vec!["pack:regions".into()],
                    )
                    .mark_stale_from_value(&today);
                }
                if let Some(note) = &route.value.note {
                    b.claim(
                        ClaimKind::Curated,
                        "routing note",
                        serde_json::json!({ "note": note, "resolved_from": route.resolved_from }),
                        vec!["pack:regions".into()],
                    )
                    .mark_stale_from_value(&today);
                }
            }
            None => {
                b.abstention(
                    format!("physician_brief:{}", input.condition),
                    "no physician route pack entry for this condition in this region",
                    vec![format!("physician_route for {} in regions/{}", input.condition, region.country.to_lowercase())],
                );
            }
        }
        // Acquisition routes for the ranked workup tests of this condition.
        let latest = self.latest_facts(&profile)?;
        let facts: Vec<EvidenceFact> = latest
            .iter()
            .map(|(lv, id)| EvidenceFact {
                fact_id: id.clone(),
                latest: lv.clone(),
            })
            .collect();
        let responses: Vec<EvidenceResponse> = self
            .db
            .treatment_responses(200)
            .map_err(domain_error)?
            .into_iter()
            .map(|row| EvidenceResponse {
                ledger_id: row.id,
                response_id: row.response_id,
            })
            .collect();
        let analysis = causal::analyze(&self.causal, &input.condition, &facts, &responses);
        let plan = plan_workup(&self.tests, &analysis);
        for test in &plan.ranked_tests {
            match region.test_route(&test.id) {
                Some(route) => {
                    b.claim(
                        ClaimKind::Inferred,
                        format!("how to get: {}", test.name),
                        serde_json::json!({
                            "test_id": test.id,
                            "discrimination": test.discrimination,
                            "requires_referral": route.value.requires_referral,
                            "routes": route.value.routes,
                            "notes": route.value.notes,
                            "resolved_from": route.resolved_from,
                        }),
                        vec![format!("id:{}", test.id), "pack:regions".into()],
                    )
                    .mark_stale_from_value(&today);
                }
                None => {
                    b.abstention(
                        format!("physician_brief:{}", test.id),
                        format!("no acquisition route for '{}' in {}", test.name, region.country),
                        vec![format!("test_route for {} in regions/{}", test.id, region.country.to_lowercase())],
                    );
                }
            }
        }
        // Legal classes for the condition's protocol items. Lifestyle and
        // diet items have no legal class by nature — no abstention for them.
        let proposal = propose(&self.interventions, &[input.condition.clone()], &profile);
        for item in &proposal.items {
            let class_by_nature = matches!(
                item.category.as_str(),
                "lifestyle" | "diet"
            );
            match region.legal_class(&item.id) {
                Some(lc) => {
                    b.claim(
                        ClaimKind::Inferred,
                        format!("legal class: {} ({})", item.name, lc.value.class),
                        serde_json::json!({
                            "item_id": item.id,
                            "class": lc.value.class,
                            "note": lc.value.note,
                            "resolved_from": lc.resolved_from,
                            "not_legal_advice": true,
                        }),
                        vec![format!("id:{}", item.id), "pack:regions".into()],
                    )
                    .mark_stale_from_value(&today);
                }
                None if !class_by_nature => {
                    b.abstention(
                        format!("physician_brief:{}", item.id),
                        format!("no legal class for '{}' in {}", item.name, region.country),
                        vec![format!("legal_class for {} in regions/{}", item.id, region.country.to_lowercase())],
                    );
                }
                None => {} // lifestyle/diet: not a legal-class item
            }
        }
        self.finish(b)
    }

    #[tool(
        description = "Region coverage report for the current profile region: which layers resolved, how many test routes / legal classes / physician routes exist, and the unknowns — tests and interventions with no route or legal class in this region. The curation queue for geography. Abstains when no region is set."
    )]
    fn region_report(&self) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let Some(region_profile) = &profile.region else {
            let mut b = self.builder("region_report");
            b.abstention(
                "region_report",
                "no region set — set one with profile_set {region: {country: \"GR\"}}",
                vec!["country code (ISO 3166-1 alpha-2)".into()],
            );
            return self.finish(b);
        };
        match self.regions.resolve(&region_profile.country) {
            None => {
                let mut b = self.builder("region_report");
                b.abstention(
                    "region_report",
                    format!(
                        "no region pack for country '{}' (available: {})",
                        region_profile.country,
                        self.regions.countries().join(", ")
                    ),
                    self.regions.countries().iter().map(|c| c.to_string()).collect(),
                );
                self.finish(b)
            }
            Some(region) => {
                let known_tests: Vec<String> =
                    self.tests.all().iter().map(|t| t.id.clone()).collect();
                let known_items: Vec<String> = self
                    .interventions
                    .all()
                    .iter()
                    .map(|i| i.id.clone())
                    .chain(known_tests.iter().cloned())
                    .collect();
                let unrouted = region.unrouted_tests(&known_tests);
                let unclassed = region.unclassed_items(&known_items);
                let mut b = self.builder("region_report");
                b.claim(
                    ClaimKind::Meta,
                    "resolved region and coverage counts",
                    serde_json::json!({
                        "country": region.country,
                        "name": region.country_name,
                        "bloc": region.bloc,
                        "layers": region.layers,
                        "test_routes": region.test_route_count(),
                        "legal_classes": region.legal_class_count(),
                        "physician_routes": region.physician_route_count(),
                        "known_tests": known_tests.len(),
                        "unrouted_tests": unrouted.len(),
                        "unclassed_items": unclassed.len(),
                    }),
                    vec!["pack:regions".into(), "pack:tests".into()],
                )
                .claim(
                    ClaimKind::Meta,
                    "curation queue: tests without a route here",
                    serde_json::json!(unrouted),
                    vec!["pack:regions".into(), "pack:tests".into()],
                )
                .claim(
                    ClaimKind::Meta,
                    "curation queue: items without a legal class here",
                    serde_json::json!(unclassed),
                    vec!["pack:regions".into(), "pack:interventions".into()],
                );
                self.finish(b)
            }
        }
    }

    #[tool(
        description = "The inference workbench: maps what the causal graph does NOT know for a condition — untestable causes (no probe, no response pattern can move them), uncovered causes (no discriminating test), uncommitted probes, response patterns nothing consumes, and mechanism-family kinship across conditions (e.g. gut-tryptophan-axis nodes in the acne and PSSD graphs). These computed unknowns are where a reasoning model should look deeper — the engine hands it the map of exactly where 'deeper' is. Feed novel ideas back via propose_novel_hypothesis."
    )]
    fn hypothesis_space(
        &self,
        Parameters(input): Parameters<HypothesisSpaceInput>,
    ) -> Result<CallToolResult, McpError> {
        let known: Vec<&str> = self
            .conditions
            .all()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        if !known.contains(&input.condition.as_str()) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown condition: {}. known cards: {}",
                    input.condition,
                    known.join(", ")
                ),
                None,
            ));
        }
        if !self.causal.has_condition(&input.condition) {
            let mut b = self.builder("hypothesis_space");
            b.abstention(
                "hypothesis_space",
                format!("no causal graph exists for '{}' — nothing to explore", input.condition),
                vec![format!("curate content-packs/causal/ for {}", input.condition)],
            );
            return self.finish(b);
        }
        let profile = self.db.profile().map_err(domain_error)?;
        let latest = self.latest_facts(&profile)?;
        let facts: Vec<EvidenceFact> = latest
            .iter()
            .map(|(lv, id)| EvidenceFact { fact_id: id.clone(), latest: lv.clone() })
            .collect();
        let responses: Vec<EvidenceResponse> = self
            .db
            .treatment_responses(200)
            .map_err(domain_error)?
            .into_iter()
            .map(|row| EvidenceResponse { ledger_id: row.id, response_id: row.response_id })
            .collect();
        let analysis = causal::analyze(&self.causal, &input.condition, &facts, &responses);
        let items = hypothesis_space(&self.causal, &self.tests, &analysis);
        let mut b = self.builder("hypothesis_space");
        b.claim(
            ClaimKind::Meta,
            "workbench target",
            serde_json::json!({
                "condition": input.condition,
                "open_unknowns": items.len(),
                "by_kind": {
                    "untestable_cause": items.iter().filter(|i| i.kind == "untestable_cause").count(),
                    "uncovered_cause": items.iter().filter(|i| i.kind == "uncovered_cause").count(),
                    "untested_probes": items.iter().filter(|i| i.kind == "untested_probes").count(),
                    "unlinked_response": items.iter().filter(|i| i.kind == "unlinked_response").count(),
                    "mechanism_kinship": items.iter().filter(|i| i.kind == "mechanism_kinship").count(),
                },
            }),
            vec!["pack:causal".into(), "pack:tests".into()],
        );
        for item in &items {
            let mut evidence = vec!["pack:causal".into()];
            if let Some(node) = &item.node {
                evidence.insert(0, format!("id:{node}"));
            }
            if item.kind == "uncovered_cause" {
                evidence.push("pack:tests".into());
            }
            b.claim(
                ClaimKind::Inferred,
                format!("{}: {}", item.kind, item.node_name.clone().unwrap_or_default()),
                serde_json::to_value(item).unwrap_or(serde_json::Value::Null),
                evidence,
            );
        }
        if items.is_empty() {
            b.abstention(
                "hypothesis_space",
                "the graph has no open unknowns for this condition — an unusual state worth double-checking",
                vec![input.condition.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Register a NOVEL hypothesis from a reasoning model (or user): one falsifiable sentence plus supporting_refs that must ALL resolve before anything is written (ledger:facts/<id>, ledger:symptom_logs/<id>, ledger:treatment_responses/<id>, id:<knowledge id>, pack:<domain>, pmid:<n>). Lands in the ledger as origin=agent, tier D by default, review-required (defaults to 90 days out). Creativity is welcome here; unproven-ness is labeled on arrival."
    )]
    fn propose_novel_hypothesis(
        &self,
        Parameters(input): Parameters<NovelHypothesisInput>,
    ) -> Result<CallToolResult, McpError> {
        if input.statement.trim().is_empty() {
            return Err(McpError::invalid_params("statement is required", None));
        }
        let bad = unresolved_refs(&input.supporting_refs, &self.db, &self.packs);
        if !bad.is_empty() {
            return Err(McpError::invalid_params(
                format!(
                    "supporting_refs that do not resolve (nothing written): {}",
                    bad.join(", ")
                ),
                None,
            ));
        }
        let review_by = input
            .review_by
            .clone()
            .unwrap_or_else(|| add_days_iso(&today_iso(), 90));
        let register = asclepius_core::store::HypothesisRegisterInput {
            statement: input.statement.clone(),
            fact_ids: input
                .supporting_refs
                .iter()
                .filter_map(|r| r.strip_prefix("ledger:facts/").map(|s| s.to_string()))
                .collect(),
            note: input.note.clone(),
            review_by: Some(review_by.clone()),
            origin: Some("agent".into()),
            pmids: input
                .supporting_refs
                .iter()
                .filter_map(|r| r.strip_prefix("pmid:").map(|s| s.to_string()))
                .collect(),
            rule_ids: vec![],
        };
        let id = self.db.add_hypothesis(&register).map_err(domain_error)?;
        let mut b = self.builder("propose_novel_hypothesis");
        b.claim(
            ClaimKind::Meta,
            "novel hypothesis registered (origin: agent)",
            serde_json::json!({
                "id": id,
                "statement": input.statement,
                "supporting_refs": input.supporting_refs,
                "origin": "agent",
                "tier_default": "D",
                "review_by": review_by,
            }),
            vec![format!("ledger:hypotheses/{id}")],
        )
        .claim(
            ClaimKind::Meta,
            "honesty contract for novel hypotheses",
            serde_json::json!(
                "unproven by definition: tier D, review-required, and explicitly non-fact until supported or refuted by evidence; narration must always carry this label"
            ),
            vec!["engine:guardrails".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Probe a hypothesis or question against local knowledge BEFORE believing or registering it: causal nodes and treatment-response patterns plus condition/rule/intervention cards, then PubMed when enabled (ASCLEPIUS_PUBMED=1). The pre-flight for propose_novel_hypothesis — is this idea already tested, contradicted, or genuinely novel?"
    )]
    fn literature_probe(
        &self,
        Parameters(input): Parameters<LiteratureProbeInput>,
    ) -> Result<CallToolResult, McpError> {
        if input.query.trim().is_empty() {
            return Err(McpError::invalid_params("query is required", None));
        }
        let limit = input.max_results.unwrap_or(5).clamp(1, 20);
        let mut b = self.builder("literature_probe");
        b.claim(
            ClaimKind::Meta,
            "probe query",
            serde_json::json!(&input.query),
            vec!["query:literature_probe".into()],
        );
        let graph_hits = probe_graph(&self.causal, &input.query, limit);
        for hit in &graph_hits {
            let pack_ref = if hit.kind == "causal_node" {
                "pack:causal"
            } else {
                "pack:treatment-responses"
            };
            b.claim(
                ClaimKind::Curated,
                format!("graph hit: {} ({})", hit.title, hit.kind),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec![pack_ref.into()],
            );
        }
        let local_hits = local_search(
            &self.conditions,
            &self.rules,
            &self.interventions,
            &input.query,
            limit,
        );
        for hit in &local_hits {
            let pack_ref = match hit.kind.as_str() {
                "condition" => "pack:conditions",
                "rule" => "pack:rules",
                "intervention" => "pack:interventions",
                _ => "pack:rules",
            };
            b.claim(
                ClaimKind::Curated,
                format!("local hit: {} {}", hit.kind, hit.id),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec![pack_ref.into()],
            );
        }
        let pubmed_section: serde_json::Value = if !self.pubmed.enabled {
            serde_json::json!({ "enabled": false, "note": "set ASCLEPIUS_PUBMED=1 to probe PubMed too (query text is sent to NCBI eutils)" })
        } else {
            match self.pubmed_search_and_cache(&input.query, limit) {
                Ok(section) => section,
                Err(e) => serde_json::json!({ "enabled": true, "error": e }),
            }
        };
        b.claim(
            ClaimKind::Meta,
            "PubMed section (opt-in egress)",
            pubmed_section,
            vec!["pubmed:query".into()],
        );
        if graph_hits.is_empty() && local_hits.is_empty() {
            b.abstention(
                "literature_probe",
                "no local knowledge touches this query — possibly genuinely novel territory",
                vec![input.query.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Propose a root-cause protocol for condition-card targets (e.g. [\"testosterone_patterns\", \"acne_root_causes\"]). Natural-first ordering (lifestyle, diet, nutrients, botanicals, then physician-required), screened against the profile: contraindications exclude items, avoid-level interactions are removed, serotonergic burden is flagged, dosing ceilings are listed. Use explore_condition to find valid target ids."
    )]
    fn propose_protocol(
        &self,
        Parameters(ProtocolInput { targets }): Parameters<ProtocolInput>,
    ) -> Result<CallToolResult, McpError> {
        if targets.is_empty() {
            return Err(McpError::invalid_params("targets is required", None));
        }
        let known: Vec<&str> = self
            .conditions
            .all()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let unknown: Vec<&String> = targets
            .iter()
            .filter(|t| !known.contains(&t.as_str()))
            .collect();
        if !unknown.is_empty() {
            return Err(McpError::invalid_params(
                format!(
                    "unknown condition targets: {}. known cards: {}",
                    unknown
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    known.join(", ")
                ),
                None,
            ));
        }
        let today = self.packs.today.clone();
        let profile = self.db.profile().map_err(domain_error)?;
        let proposal = propose(&self.interventions, &targets, &profile);
        let mut b = self.builder("propose_protocol");
        b.claim(
            ClaimKind::Meta,
            "requested targets",
            serde_json::json!(&targets),
            vec!["query:propose_protocol".into()],
        );
        for item in &proposal.items {
            b.claim(
                ClaimKind::Curated,
                format!("protocol rung: {} ({})", item.name, item.category),
                serde_json::to_value(item).unwrap_or(serde_json::Value::Null),
                vec!["pack:interventions".into()],
            )
            .mark_stale_from_value(&today)
            .tier(&item.tier);
        }
        for ex in &proposal.excluded {
            b.claim(
                ClaimKind::Inferred,
                format!("excluded by profile screening: {}", ex.name),
                serde_json::to_value(ex).unwrap_or(serde_json::Value::Null),
                vec!["pack:interventions".into(), "ledger:profile".into()],
            );
        }
        // Product grounding: attach purchasable regional products to rungs.
        match profile.region.as_ref().and_then(|r| self.regions.resolve(&r.country)) {
            Some(region) => {
                let intervention_ids: Vec<String> =
                    proposal.items.iter().map(|i| i.id.clone()).collect();
                let mut unmatched_any = false;
                for item_id in &intervention_ids {
                    let matches = region.products_for(item_id);
                    if matches.is_empty() {
                        unmatched_any = true;
                        continue;
                    }
                    for product in matches {
                        b.claim(
                            ClaimKind::Curated,
                            format!(
                                "product for {}: {} (tier {})",
                                item_id, product.value.name, product.value.tier
                            ),
                            serde_json::json!({
                                "intervention_id": item_id,
                                "product": product.value,
                                "resolved_from": product.resolved_from,
                            }),
                            vec!["pack:regions".into()],
                        )
                        .mark_stale_from_value(&today)
                        .tier(&product.value.tier);
                    }
                }
                if unmatched_any {
                    b.abstention(
                        "propose_protocol:products",
                        format!(
                            "some rungs have no product mapped in {} — compound-class rungs only; curate [[product]] entries in regions/{}.toml to ground them",
                            region.country,
                            region.country.to_lowercase()
                        ),
                        vec!["region product entries".into()],
                    );
                }
            }
            None => {
                b.abstention(
                    "propose_protocol:products",
                    "no region set — rungs are compound classes only; set profile.region to attach purchasable products",
                    vec!["profile_set {region: {country: \"GR\"}}".into()],
                );
            }
        }
        b.claim(
            ClaimKind::Meta,
            "interactions between proposed items",
            serde_json::json!(&proposal.interactions),
            vec!["pack:interventions".into()],
        )
        .claim(
            ClaimKind::Meta,
            "serotonergic burden watch",
            serde_json::json!(&proposal.serotonergic_burden),
            vec!["pack:interventions".into()],
        )
        .claim(
            ClaimKind::Meta,
            "guardrail disclaimer",
            serde_json::json!(&proposal.disclaimer),
            vec!["engine:guardrails".into()],
        );
        if proposal.items.is_empty() && proposal.excluded.is_empty() {
            b.abstention(
                "propose_protocol",
                "no interventions in the packs target these conditions",
                vec![targets.join(", ")],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Safety gate: check free-text symptom descriptions for red-flag patterns requiring emergency, urgent, or prompt conventional care. Always call this before or during any protocol discussion involving new or worsening symptoms. It overrides optimization priorities."
    )]
    fn triage_check(
        &self,
        Parameters(TriageInput { text }): Parameters<TriageInput>,
    ) -> Result<CallToolResult, McpError> {
        if text.trim().is_empty() {
            return Err(McpError::invalid_params("text is required", None));
        }
        let hits = triage_check(&self.triage, &text);
        if let Some(hit) = hits
            .iter()
            .find(|h| matches!(h.urgency, asclepius_core::rules::Urgency::Emergency))
            .or_else(|| {
                hits.iter()
                    .find(|h| matches!(h.urgency, asclepius_core::rules::Urgency::Urgent))
            })
        {
            self.bridge.post(
                "status",
                &format!("triage {}: {}", hit.rule_id, hit.message),
            );
        }
        let mut b = self.builder("triage_check");
        let mut hit_refs: Vec<String> = Vec::new();
        for hit in &hits {
            hit_refs.push(format!("id:{}", hit.rule_id));
            b.claim(
                ClaimKind::Inferred,
                format!("red-flag pattern matched: {}", hit.rule_id),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec![format!("id:{}", hit.rule_id), "pack:triage".into()],
            );
        }
        if hits.is_empty() {
            b.claim(
                ClaimKind::Meta,
                "triage clear: no red-flag patterns matched",
                serde_json::json!({ "clear": true }),
                vec!["engine:triage".into()],
            );
        } else {
            b.claim(
                ClaimKind::Meta,
                "red-flag patterns matched: address conventional care first",
                serde_json::json!({
                    "clear": false,
                    "count": hits.len(),
                }),
                hit_refs,
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Verify a medical claim against evidence. Local knowledge base first (condition cards, rules, interventions with their sources and confidence); then PubMed when enabled (env ASCLEPIUS_PUBMED=1 — note: the claim text is sent to NCBI eutils; abstracts are cached locally). Returns evidence only, never a verdict: compare the claim against returned sources and PMIDs yourself, cite what you use. This is the anti-confabulation gate for medical assertions."
    )]
    fn literature_verify(
        &self,
        Parameters(LiteratureInput { claim, max_results }): Parameters<LiteratureInput>,
    ) -> Result<CallToolResult, McpError> {
        if claim.trim().is_empty() {
            return Err(McpError::invalid_params("claim is required", None));
        }
        let today = self.packs.today.clone();
        let limit = max_results.unwrap_or(5).clamp(1, 20);
        let local_hits = local_search(
            &self.conditions,
            &self.rules,
            &self.interventions,
            &claim,
            limit,
        );
        let pubmed_section: serde_json::Value = if !self.pubmed.enabled {
            serde_json::json!({
                "enabled": false,
                "note": "PubMed egress disabled. Set ASCLEPIUS_PUBMED=1 to enable (claim text is sent to NCBI eutils).",
            })
        } else {
            match self.pubmed_search_and_cache(&claim, limit) {
                Ok(sections) => sections,
                Err(e) => serde_json::json!({ "enabled": true, "error": e }),
            }
        };
        let mut b = self.builder("literature_verify");
        b.claim(
            ClaimKind::Meta,
            "claim under verification",
            serde_json::json!(&claim),
            vec!["query:literature_verify".into()],
        );
        for hit in &local_hits {
            let pack_ref = match hit.kind.as_str() {
                "condition" => "pack:conditions",
                "rule" => "pack:rules",
                "intervention" => "pack:interventions",
                _ => "pack:rules",
            };
            b.claim(
                ClaimKind::Curated,
                format!("local evidence: {} {}", hit.kind, hit.id),
                serde_json::to_value(hit).unwrap_or(serde_json::Value::Null),
                vec![pack_ref.into()],
            )
            .mark_stale_from_value(&today);
        }
        b.claim(
            ClaimKind::Meta,
            "PubMed section (opt-in egress)",
            pubmed_section,
            vec!["pubmed:query".into()],
        )
        .claim(
            ClaimKind::Meta,
            "instruction to caller",
            serde_json::json!("evidence only — you decide supports/contradicts/neutral; cite local hit ids or PMIDs for any assertion you make"),
            vec!["engine:self".into()],
        );
        if local_hits.is_empty() {
            b.abstention(
                "literature_verify",
                "local knowledge base has no entry relevant to this claim",
                vec![claim.clone()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Begin a structured n=1 trial of an intervention (e.g. zinc_picolinate for skin). Outcome analytes must be registry names or derived ids (free_testosterone_vermeulen, homa_ir). If the intervention requires baseline labs, every outcome analyte must have a committed fact first — the gate rejects otherwise. The trial starts in planned status; set active with trial_update_status when you begin."
    )]
    fn trial_begin(
        &self,
        Parameters(input): Parameters<TrialBeginInput>,
    ) -> Result<CallToolResult, McpError> {
        let card = self
            .interventions
            .get(&input.intervention_id)
            .ok_or_else(|| {
                let ids: Vec<&str> = self
                    .interventions
                    .all()
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                McpError::invalid_params(
                    format!(
                        "unknown intervention: {}. available: {}",
                        input.intervention_id,
                        ids.join(", ")
                    ),
                    None,
                )
            })?;
        if card
            .flags
            .contains(&asclepius_core::interventions::FLAG_PHYSICIAN_REQUIRED.to_string())
        {
            return Err(McpError::invalid_params(
                format!(
                    "{} is physician_required: n=1 self-trials are blocked for this intervention. Discuss with a clinician instead.",
                    input.intervention_id
                ),
                None,
            ));
        }
        let profile = self.db.profile().map_err(domain_error)?;
        let contra_hits: Vec<String> = card
            .contraindicated_if
            .iter()
            .filter(|c| {
                profile
                    .conditions
                    .iter()
                    .any(|p| p.to_lowercase().contains(&c.to_lowercase()))
            })
            .cloned()
            .collect();
        if !contra_hits.is_empty() {
            return Err(McpError::invalid_params(
                format!(
                    "{} is contraindicated by profile conditions: {}",
                    input.intervention_id,
                    contra_hits.join(", ")
                ),
                None,
            ));
        }
        for analyte in &input.outcome_analytes {
            if let Some(scale) = self.symptoms.get(analyte) {
                if scale.kind != "single_item" {
                    return Err(McpError::invalid_params(
                        format!(
                            "outcome '{analyte}' is a multi-item instrument: track its items via symptom_log instead (sum-score trial outcomes arrive later)"
                        ),
                        None,
                    ));
                }
                continue; // valid single-item symptom scale outcome
            }
            let known =
                self.registry.get(analyte).is_some() || DERIVED_IDS.contains(&analyte.as_str());
            if !known {
                return Err(McpError::invalid_params(
                    format!(
                        "outcome analyte not recognized: {analyte}. use registry analyte ids, derived ids ({}), or single-item symptom scale ids; commit values first",
                        DERIVED_IDS.join(", ")
                    ),
                    None,
                ));
            }
        }
        let mut baseline_ids: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for outcome in &input.outcome_analytes {
            if self.symptoms.get(outcome).is_some() {
                match self.db.latest_symptom(outcome, None) {
                    Some(log) => baseline_ids.push(log.id),
                    None => missing.push(outcome.clone()),
                }
            } else {
                match self.db.latest_fact(outcome).ok().flatten() {
                    Some(fact) => baseline_ids.push(fact.id),
                    None => missing.push(outcome.clone()),
                }
            }
        }
        if card
            .flags
            .contains(&FLAG_REQUIRES_BASELINE_LABS.to_string())
            && !missing.is_empty()
        {
            return Err(McpError::invalid_params(
                format!(
                    "baseline gate: {} requires baseline labs before a trial. commit values first for: {}. use lab_commit",
                    input.intervention_id,
                    missing.join(", ")
                ),
                None,
            ));
        }
        let id = self
            .db
            .begin_trial(&input, &card.name, baseline_ids.clone())
            .map_err(domain_error)?;
        self.bridge
            .post("status", &format!("trial begun: {} ({})", card.name, id));
        let mut b = self.builder("trial_begin");
        b.claim(
            ClaimKind::Meta,
            format!("n=1 trial of {} registered as planned", card.name),
            serde_json::json!({
                "id": id,
                "intervention": card.name,
                "status": "planned",
                "baseline_fact_ids": baseline_ids,
            }),
            vec![format!("ledger:trials/{id}")],
        );
        if !missing.is_empty() {
            b.abstention(
                "trial_begin",
                "some outcome analytes have no baseline facts; deltas will be partial",
                missing.clone(),
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Append a log entry to a trial. kind: note, dose_change, side_effect, or outcome."
    )]
    fn trial_log(
        &self,
        Parameters(input): Parameters<TrialLogInput>,
    ) -> Result<CallToolResult, McpError> {
        if self
            .db
            .get_trial(&input.trial_id)
            .map_err(domain_error)?
            .is_none()
        {
            return Err(McpError::invalid_params(
                format!("trial not found: {}", input.trial_id),
                None,
            ));
        }
        self.db.append_trial_log(&input).map_err(domain_error)?;
        let mut b = self.builder("trial_log");
        b.claim(
            ClaimKind::Meta,
            format!("log entry appended to {}", input.trial_id),
            serde_json::json!({ "ok": true, "kind": input.kind }),
            vec![format!("ledger:trials/{}", input.trial_id)],
        );
        self.finish(b)
    }

    #[tool(
        description = "Advance trial status: planned -> active -> completed (or paused, aborted). Start and end timestamps are captured automatically."
    )]
    fn trial_update_status(
        &self,
        Parameters(input): Parameters<TrialStatusInput>,
    ) -> Result<CallToolResult, McpError> {
        let changed = self.db.set_trial_status(&input).map_err(domain_error)?;
        if !changed {
            return Err(McpError::invalid_params(
                format!("trial not found: {}", input.trial_id),
                None,
            ));
        }
        let row = self
            .db
            .get_trial(&input.trial_id)
            .map_err(domain_error)?
            .ok_or_else(|| McpError::internal_error("trial vanished", None))?;
        self.bridge.post(
            "status",
            &format!("trial {} -> {}", input.trial_id, input.status.as_str()),
        );
        let mut b = self.builder("trial_update_status");
        b.claim(
            ClaimKind::Meta,
            format!("trial {} status now {}", row.id, row.status.as_str()),
            serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
            vec![format!("ledger:trials/{}", row.id)],
        );
        self.finish(b)
    }

    #[tool(
        description = "List n=1 trials, optionally filtered by status (planned, active, completed, paused, aborted), newest first."
    )]
    fn trial_list(
        &self,
        Parameters(TrialListInput { status }): Parameters<TrialListInput>,
    ) -> Result<CallToolResult, McpError> {
        let rows = self.db.trials(status, 50).map_err(domain_error)?;
        let mut b = self.builder("trial_list");
        if rows.is_empty() {
            b.abstention(
                "trial_list",
                "no trials match this filter",
                vec!["begin trials with trial_begin".into()],
            );
            return self.finish(b);
        }
        b.claim(
            ClaimKind::Meta,
            "n=1 trials, newest first",
            serde_json::to_value(&rows).unwrap_or(serde_json::Value::Null),
            vec!["ledger:trials".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Full n=1 trial report: trial row, chronological log, and outcome comparison — baseline fact values vs latest committed values per outcome analyte, with deltas recomputed and independently verified. Commit retest labs with lab_commit, then read this. Reminder: n=1 trials are unblinded — regression to the mean, placebo, and natural course are always live explanations for improvement."
    )]
    fn trial_report(
        &self,
        Parameters(TrialReportInput { trial_id }): Parameters<TrialReportInput>,
    ) -> Result<CallToolResult, McpError> {
        let row = self
            .db
            .get_trial(&trial_id)
            .map_err(domain_error)?
            .ok_or_else(|| {
                McpError::invalid_params(format!("trial not found: {trial_id}"), None)
            })?;
        let log = self.db.trial_log_entries(&trial_id).map_err(domain_error)?;
        let mut b = self.builder("trial_report");
        b.claim(
            ClaimKind::Meta,
            "trial record",
            serde_json::to_value(&row).unwrap_or(serde_json::Value::Null),
            vec![format!("ledger:trials/{trial_id}")],
        )
        .claim(
            ClaimKind::Meta,
            "chronological trial log",
            serde_json::to_value(&log).unwrap_or(serde_json::Value::Null),
            vec![format!("ledger:trials/{trial_id}")],
        );
        for (index, analyte) in row.outcome_analytes.iter().enumerate() {
            // Outcomes are index-aligned with the baseline ids captured at
            // trial_begin — labs and symptom logs alike.
            let baseline_id = row
                .baseline_fact_ids
                .as_ref()
                .and_then(|ids| ids.get(index))
                .cloned();
            let baseline_symptom = baseline_id
                .as_ref()
                .and_then(|id| self.db.get_symptom_log(id));
            let baseline_fact = baseline_id
                .as_ref()
                .and_then(|id| self.db.get_fact(id).ok().flatten());
            let is_scale = self.symptoms.get(analyte).is_some();
            let latest_symptom = if is_scale {
                self.db.latest_symptom(analyte, None)
            } else {
                None
            };
            let latest_fact = if !is_scale {
                self.db.latest_fact(analyte).ok().flatten()
            } else {
                None
            };
            if let Some(base) = &baseline_symptom {
                b.claim(
                    ClaimKind::Measured,
                    format!("baseline {}: {} (score)", analyte, base.value),
                    serde_json::json!({
                        "analyte": analyte,
                        "role": "baseline",
                        "value": base.value,
                        "unit": "score",
                        "observed_on": base.observed_on,
                    }),
                    vec![format!("ledger:symptom_logs/{}", base.id)],
                );
            }
            if let Some(base) = &baseline_fact {
                b.claim(
                    ClaimKind::Measured,
                    format!("baseline {}: {} {}", analyte, base.value, base.unit),
                    serde_json::json!({
                        "analyte": analyte,
                        "role": "baseline",
                        "value": base.value,
                        "unit": base.unit,
                        "collected_at": base.collected_at,
                    }),
                    vec![format!("ledger:facts/{}", base.id)],
                );
            }
            if let Some(last) = &latest_symptom {
                b.claim(
                    ClaimKind::Measured,
                    format!("latest {}: {} (score)", analyte, last.value),
                    serde_json::json!({
                        "analyte": analyte,
                        "role": "latest",
                        "value": last.value,
                        "unit": "score",
                        "observed_on": last.observed_on,
                    }),
                    vec![format!("ledger:symptom_logs/{}", last.id)],
                );
            }
            if let Some(last) = &latest_fact {
                b.claim(
                    ClaimKind::Measured,
                    format!("latest {}: {} {}", analyte, last.value, last.unit),
                    serde_json::json!({
                        "analyte": analyte,
                        "role": "latest",
                        "value": last.value,
                        "unit": last.unit,
                        "collected_at": last.collected_at,
                    }),
                    vec![format!("ledger:facts/{}", last.id)],
                );
            }
            let base_value = baseline_symptom
                .as_ref()
                .map(|s| s.value)
                .or(baseline_fact.as_ref().map(|f| f.value));
            let base_ref = baseline_symptom
                .as_ref()
                .map(|s| format!("ledger:symptom_logs/{}", s.id))
                .or(baseline_fact.as_ref().map(|f| format!("ledger:facts/{}", f.id)));
            let last_value = latest_symptom
                .as_ref()
                .map(|s| s.value)
                .or(latest_fact.as_ref().map(|f| f.value));
            let last_ref = latest_symptom
                .as_ref()
                .map(|s| format!("ledger:symptom_logs/{}", s.id))
                .or(latest_fact.as_ref().map(|f| format!("ledger:facts/{}", f.id)));
            if let (Some(base_v), Some(last_v), Some(b_ref), Some(l_ref)) =
                (base_value, last_value, base_ref, last_ref)
            {
                let mut inputs = BTreeMap::new();
                inputs.insert("baseline".into(), serde_json::json!(base_v));
                inputs.insert("latest".into(), serde_json::json!(last_v));
                b.derived(
                    format!("delta for {analyte} (latest minus baseline)"),
                    serde_json::json!({
                        "analyte": analyte,
                        "value": last_v - base_v,
                        "unit": "delta",
                    }),
                    vec![b_ref, l_ref],
                    "delta",
                    inputs,
                );
            }
            if baseline_symptom.is_none()
                && baseline_fact.is_none()
                && latest_symptom.is_none()
                && latest_fact.is_none()
            {
                b.abstention(
                    format!("trial_report:{analyte}"),
                    "no baseline or latest observation for this outcome",
                    vec![format!("commit {analyte} values")],
                );
            }
        }
        // The n=1 honesty boilerplate travels with every report, by design.
        b.claim(
            ClaimKind::Meta,
            "n=1 caution",
            serde_json::json!(
                "unblinded single-subject trial: regression to the mean, placebo response, and natural course remain live explanations; deltas are observations, not proof of mechanism"
            ),
            vec!["engine:guardrails".into()],
        );
        self.finish(b)
    }

    #[tool(
        description = "Validate, unit-convert, and range-assess a panel of lab values. Input: list of {analyte, value, unit?} where analyte accepts common names (e.g. 'total testosterone', 'TSH', 'ferritin') and unit is optional lab notation (e.g. 'nmol/L', 'ng/dL'). Returns per-analyte canonical values, plausibility checks, and both population and functional range assessments with sources. Does not persist; use for interpretation of raw values."
    )]
    fn lab_normalize(
        &self,
        Parameters(PanelInput { panel }): Parameters<PanelInput>,
    ) -> Result<CallToolResult, McpError> {
        let today = self.packs.today.clone();
        let profile = self.db.profile().map_err(domain_error)?;
        let results = normalize_panel(&self.registry, &self.ranges, &profile, &panel);
        let mut b = self.builder("lab_normalize");
        b.claim(
            ClaimKind::Meta,
            "panel as submitted (dry run, nothing persisted)",
            serde_json::json!(&panel),
            vec!["query:lab_normalize".into()],
        );
        let mut abstain_all = true;
        for (input, r) in panel.iter().zip(results.iter()) {
            b.claim(
                ClaimKind::Reported,
                format!("reported {}: {} {:?}", input.analyte, input.value, input.unit),
                serde_json::to_value(input).unwrap_or(serde_json::Value::Null),
                vec!["query:lab_normalize".into()],
            );
            match r {
                Ok(res) => {
                    abstain_all = false;
                    b.claim(
                        ClaimKind::Inferred,
                        format!(
                            "normalized: {} = {} {}",
                            res.analyte_id, res.canonical_value, res.canonical_unit.label()
                        ),
                        serde_json::to_value(res).unwrap_or(serde_json::Value::Null),
                        vec![
                            "query:lab_normalize".into(),
                            "pack:ranges".into(),
                            "pack:analytes".into(),
                        ],
                    )
                    .mark_stale_from_value(&today);
                }
                Err(e) => {
                    b.claim(
                        ClaimKind::Meta,
                        format!("rejected: {}", input.analyte),
                        serde_json::json!({ "analyte": input.analyte, "error": e.to_string() }),
                        vec!["gate:normalize".into()],
                    );
                }
            }
        }
        if abstain_all && !panel.is_empty() {
            b.abstention(
                "lab_normalize",
                "every submitted value was rejected by the plausibility/unit gates",
                vec!["verify analyte names, units, and values".into()],
            );
        }
        self.finish(b)
    }

    #[tool(
        description = "Calculated free testosterone (Vermeulen 1999) from total testosterone, SHBG, and optional albumin (assumed 4.3 g/dL if omitted). All units optional; accepts mass or molar notation. The result is independently recomputed by the verifier before it is returned."
    )]
    fn derived_free_testosterone(
        &self,
        Parameters(input): Parameters<FreeTInput>,
    ) -> Result<CallToolResult, McpError> {
        let derived = free_testosterone_vermeulen(&self.registry, &input).map_err(domain_error)?;
        let mut b = self.builder("derived_free_testosterone");
        let inputs = inputs_map(&serde_json::json!({
            "total_testosterone_nmol_l": derived.inputs["total_testosterone_nmol_l"],
            "shbg_nmol_l": derived.inputs["shbg_nmol_l"],
            "albumin_g_dl": derived.inputs["albumin_g_dl"],
        }));
        b.derived(
            "calculated free testosterone (Vermeulen 1999)",
            serde_json::to_value(&derived).unwrap_or(serde_json::Value::Null),
            vec!["input:derived".into()],
            "vermeulen1999",
            inputs,
        );
        self.finish(b)
    }

    #[tool(
        description = "HOMA-IR fasting insulin resistance index from fasting glucose and insulin (Matthews 1985). Values above ~2 suggest insulin resistance; interpret alongside functional ranges. The result is independently recomputed by the verifier before it is returned."
    )]
    fn derived_homa_ir(
        &self,
        Parameters(input): Parameters<HomaInput>,
    ) -> Result<CallToolResult, McpError> {
        let derived = homa_ir(&self.registry, &input).map_err(domain_error)?;
        let mut b = self.builder("derived_homa_ir");
        let inputs = inputs_map(&serde_json::json!({
            "glucose_mg_dl": derived.inputs["glucose_mg_dl"],
            "insulin_uu_ml": derived.inputs["insulin_uu_ml"],
        }));
        b.derived(
            "calculated HOMA-IR (Matthews 1985)",
            serde_json::to_value(&derived).unwrap_or(serde_json::Value::Null),
            vec!["input:derived".into()],
            "matthews1985",
            inputs,
        );
        self.finish(b)
    }
}

impl Asclepius {
    /// Latest committed value per analyte, keeping the fact id for evidence.
    fn latest_facts(
        &self,
        profile: &asclepius_core::profile::Profile,
    ) -> Result<Vec<(LatestValue, String)>, McpError> {
        let facts = self.db.history(None, 1000).map_err(domain_error)?;
        let mut latest: Vec<(LatestValue, String)> = Vec::new();
        for fact in &facts {
            let assessments =
                self.ranges
                    .assess(&fact.analyte, profile.sex, profile.age_years, fact.value);
            match latest.iter_mut().find(|(l, _)| l.analyte == fact.analyte) {
                Some(slot) => {
                    slot.0.value = fact.value;
                    slot.0.assessments = assessments;
                    slot.1 = fact.id.clone();
                }
                None => latest.push((
                    LatestValue {
                        analyte: fact.analyte.clone(),
                        value: fact.value,
                        assessments,
                    },
                    fact.id.clone(),
                )),
            }
        }
        Ok(latest)
    }

    fn builder(&self, tool: &str) -> EnvelopeBuilder {
        let mut b = EnvelopeBuilder::new(tool);
        b.set_pack_hashes(self.packs.hashes().clone());
        b
    }

    /// L3 gate: verify every claim, then serialize. A failing envelope is
    /// refused — never sent to the client. Every verified envelope is posted
    /// to the anticonf bridge (when enabled) as the L5 external audit record.
    fn finish(&self, builder: EnvelopeBuilder) -> Result<CallToolResult, McpError> {
        let tool = builder.tool().to_string();
        let report = verify_claims(
            builder.claims(),
            &tool,
            Some(&self.db),
            Some(&self.packs),
            &self.packs.today,
        );
        if report.status != "pass" {
            self.bridge.post(
                "verifier_refusal",
                &format!("{tool}: {}", report.failures.join("; ")),
            );
            return Err(McpError::internal_error(
                format!(
                    "verifier refused output for {tool}: {}",
                    report.failures.join("; ")
                ),
                None,
            ));
        }
        let envelope: Envelope = builder.finish(NAME, VERSION, report);
        self.bridge.post_json("envelope", &envelope.audit_view());
        let payload = serde_json::to_value(&envelope)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    fn pubmed_search_and_cache(
        &self,
        claim: &str,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        let pmids = self.pubmed.search_pmids(claim, limit)?;
        let mut articles: Vec<serde_json::Value> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for pmid in &pmids {
            match self.db.pubmed_cache_get(pmid) {
                Some(cached) => articles.push(serde_json::json!({
                    "pmid": cached.pmid,
                    "title": cached.title,
                    "abstract_excerpt": cached.abstract_text.chars().take(600).collect::<String>(),
                    "cached": true,
                })),
                None => missing.push(pmid.clone()),
            }
        }
        let mut fetched_count = 0usize;
        if !missing.is_empty() {
            let fetched = self.pubmed.fetch_articles(&missing)?;
            fetched_count = fetched.len();
            self.db
                .pubmed_cache_put(&fetched)
                .map_err(|e| format!("cache write failed: {e}"))?;
            let fetched_map: std::collections::HashMap<
                String,
                &asclepius_core::literature::PubMedArticle,
            > = fetched.iter().map(|a| (a.pmid.clone(), a)).collect();
            for pmid in &pmids {
                if let Some(article) = fetched_map.get(pmid) {
                    articles.push(serde_json::json!({
                        "pmid": article.pmid,
                        "title": article.title,
                        "abstract_excerpt": article.abstract_text.chars().take(600).collect::<String>(),
                        "cached": false,
                    }));
                }
            }
        }
        let cached_count = pmids.len().saturating_sub(fetched_count);
        Ok(serde_json::json!({
            "enabled": true,
            "query": claim,
            "pmids": articles,
            "fetched": fetched_count,
            "cached": cached_count,
        }))
    }
}

#[tool_handler]
impl ServerHandler for Asclepius {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Asclepius, a root-cause holistic physician. Deterministic engine: unit conversion, dual reference ranges (population + functional), derived biomarkers. Every tool response is an assertion envelope: claims carry evidence bindings, pack content hashes, and an independent verifier report — a claim without evidence cannot be emitted. Prefer natural interventions; always surface red flags. Not a substitute for emergency or in-person care.",
            );
        let mut info = Implementation::default();
        info.name = NAME.into();
        info.title = Some("Asclepius".into());
        info.version = VERSION.into();
        info.description = Some("Holistic physician MCP".into());
        config.server_info = info;
        config
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = packs_root();
    // L0 load gate: hash, parse, and cross-validate every pack file before
    // anything else. Invalid files are excluded and reported; the server
    // starts degraded rather than silently wrong.
    let index = packs::load_pack_index(&root);
    for excluded in index.excluded() {
        eprintln!(
            "asclepius: pack EXCLUDED by load gate: {} ({})",
            excluded.rel_path, excluded.reason
        );
    }
    if index.is_empty() {
        return Err(format!(
            "no content packs admitted from {} — check the gate output above",
            root.display()
        )
        .into());
    }
    let registry = AnalyteRegistry::load_paths(&packs::allowed_files(&root, "analytes", &index))?;
    let ranges = RangeSet::load_paths(&packs::allowed_files(&root, "ranges", &index), &registry)?;
    let rules = RuleSet::load_paths(&packs::allowed_files(&root, "rules", &index), &registry)?;
    let conditions =
        ConditionLibrary::load_paths(&packs::allowed_files(&root, "conditions", &index))?;
    let interventions =
        InterventionLibrary::load_paths(&packs::allowed_files(&root, "interventions", &index))?;
    let triage = TriageSet::load_paths(&packs::allowed_files(&root, "triage", &index))?;
    let causal = CausalGraph::load_paths(
        &packs::allowed_files(&root, "causal", &index),
        &packs::allowed_files(&root, "treatment-responses", &index),
    )?;
    let tests = TestLibrary::load_paths(&packs::allowed_files(&root, "tests", &index))?;
    let regions = RegionSet::load_paths(&packs::allowed_files(&root, "regions", &index))?;
    let symptoms = SymptomSet::load_paths(&packs::allowed_files(&root, "symptoms", &index))?;
    let pubmed = PubMedClient::from_env();
    let bridge = Bridge::from_env();
    let db_path = data_home().join("asclepius.db");
    let db = Db::open(&db_path)?;
    eprintln!(
        "asclepius {} serving patient '{}': {} analytes, {} ranges, {} rules, {} condition cards, {} interventions, {} triage rules, {} causal nodes, {} response patterns, {} investigation tests from {} | ledger {} | pubmed {} | bridge {} | packs excluded: {}",
        VERSION,
        db.patient(),
        registry.all().len(),
        ranges.len(),
        rules.len(),
        conditions.all().len(),
        interventions.all().len(),
        triage.len(),
        causal.nodes().len(),
        causal.responses().len(),
        tests.all().len(),
        root.display(),
        db_path.display(),
        if pubmed.enabled { "enabled" } else { "disabled" },
        if bridge.enabled() { "enabled" } else { "disabled" },
        index.excluded().len(),
    );
    let doctor = Asclepius {
        registry: Arc::new(registry),
        ranges: Arc::new(ranges),
        rules: Arc::new(rules),
        conditions: Arc::new(conditions),
        interventions: Arc::new(interventions),
        triage: Arc::new(triage),
        causal: Arc::new(causal),
        tests: Arc::new(tests),
        regions: Arc::new(regions),
        symptoms: Arc::new(symptoms),
        pubmed: Arc::new(pubmed),
        bridge: Arc::new(bridge),
        db: Arc::new(db),
        packs: Arc::new(index),
    };
    let service = doctor
        .serve(stdio())
        .await
        .map_err(|e| format!("serve error: {e}"))?;
    service.waiting().await?;
    Ok(())
}
