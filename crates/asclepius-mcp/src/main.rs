use asclepius_core::analyte::AnalyteRegistry;
use asclepius_core::conditions::{ConditionLibrary, ConditionQuery};
use asclepius_core::derived::{
    DERIVED_IDS, FreeTInput, HomaInput, free_testosterone_vermeulen, homa_ir,
};
use asclepius_core::interventions::{
    FLAG_REQUIRES_BASELINE_LABS, InterventionLibrary, ProtocolInput, propose,
};
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
    LineageQuery, TrialBeginInput, TrialListInput, TrialLogInput, TrialReportInput,
    TrialStatusInput,
};
use asclepius_core::{NAME, VERSION};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
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
    db: Arc<Db>,
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

#[tool_router]
impl Asclepius {
    #[tool(description = "Liveness probe: server identity, version, and loaded knowledge counts.")]
    fn ping(&self) -> Result<CallToolResult, McpError> {
        let payload = serde_json::json!({
            "server": NAME,
            "version": VERSION,
            "analytes_loaded": self.registry.all().len(),
            "ranges_loaded": self.ranges.len(),
        });
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Current patient profile: sex, age, conditions, medications, supplements, goals. Ranges and interpretations are scoped by this profile. Persisted in the local ledger."
    )]
    fn profile_get(&self) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let payload = serde_json::to_value(&profile)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
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
        let payload = serde_json::to_value(&profile)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
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
        let (panel_id, ids) = if refs.is_empty() {
            (serde_json::Value::Null, Vec::new())
        } else {
            let (pid, ids) = self
                .db
                .commit_panel(
                    collected_at.as_deref(),
                    note.as_deref(),
                    &refs,
                    "lab_report",
                )
                .map_err(domain_error)?;
            (serde_json::json!(pid), ids)
        };
        let committed: Vec<serde_json::Value> = valid
            .into_iter()
            .zip(ids)
            .map(|(res, id)| {
                serde_json::json!({
                    "id": id,
                    "analyte": res.analyte_id,
                    "canonical_value": res.canonical_value,
                    "canonical_unit": res.canonical_unit.label(),
                    "assessments": res.assessments,
                    "warnings": res.warnings,
                })
            })
            .collect();
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "panel_id": panel_id, "committed": committed, "rejected": rejected }),
        )?]))
    }

    #[tool(
        description = "Query the fact ledger: chronological lab facts (current, non-superseded). Optional analyte filter (common names accepted), limit defaults to 50. The latest fact per analyte carries fresh range assessments against the current profile."
    )]
    fn lab_history(
        &self,
        Parameters(HistoryQuery { analyte, limit }): Parameters<HistoryQuery>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let limit = limit.unwrap_or(50).clamp(1, 500);
        let rows = self
            .db
            .history(analyte.as_deref(), limit)
            .map_err(domain_error)?;
        let facts: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Null))
            .collect();
        let latest = rows.last();
        let latest_assessments: Vec<serde_json::Value> = latest
            .map(|r| {
                self.ranges
                    .assess(&r.analyte, profile.sex, profile.age_years, r.value)
                    .into_iter()
                    .filter_map(|a| serde_json::to_value(a).ok())
                    .collect()
            })
            .unwrap_or_default();
        let trend = if rows.len() >= 2 {
            let first = &rows[0];
            serde_json::json!({
                "analyte": latest.map(|l| l.analyte.clone()).unwrap_or_default(),
                "first_value": first.value,
                "latest_value": latest.map(|l| l.value).unwrap_or_default(),
                "n_points": rows.len(),
            })
        } else {
            serde_json::Value::Null
        };
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "facts": facts, "latest_assessments": latest_assessments, "trend": trend }),
        )?]))
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
        let payload = serde_json::to_value(&node)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Interpret the latest committed labs against the rule base: fires severity-tagged findings with sources, computes calculated free testosterone when total T + SHBG exist, and proactively lists untested analytes the fired rules suggest investigating next. The root-cause workup engine; requires labs committed via lab_commit."
    )]
    fn interpret_panel(&self) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let facts = self.db.history(None, 1000).map_err(domain_error)?;
        let mut latest: Vec<LatestValue> = Vec::new();
        for fact in &facts {
            let assessments =
                self.ranges
                    .assess(&fact.analyte, profile.sex, profile.age_years, fact.value);
            if let Some(slot) = latest.iter_mut().find(|l| l.analyte == fact.analyte) {
                slot.value = fact.value;
                slot.assessments = assessments;
            } else {
                latest.push(LatestValue {
                    analyte: fact.analyte.clone(),
                    value: fact.value,
                    assessments,
                });
            }
        }
        let interpretation = interpret(&self.rules, &latest);
        let mut derived_candidates: Vec<serde_json::Value> = Vec::new();
        let tt = latest.iter().find(|l| l.analyte == "testosterone_total");
        let shbg = latest.iter().find(|l| l.analyte == "shbg");
        let albumin = latest.iter().find(|l| l.analyte == "albumin");
        if let (Some(tt), Some(shbg)) = (tt, shbg) {
            let input = FreeTInput {
                total_testosterone: tt.value,
                total_testosterone_unit: Some("ng/dL".into()),
                shbg: shbg.value,
                shbg_unit: Some("nmol/L".into()),
                albumin: albumin.map(|a| a.value),
                albumin_unit: albumin.map(|_| "g/dL".into()),
            };
            if let Ok(derived) = free_testosterone_vermeulen(&self.registry, &input) {
                if let Ok(v) = serde_json::to_value(&derived) {
                    derived_candidates.push(v);
                }
            }
        }
        let glu = latest.iter().find(|l| l.analyte == "glucose");
        let ins = latest.iter().find(|l| l.analyte == "insulin");
        if let (Some(g), Some(i)) = (glu, ins) {
            let input = HomaInput {
                glucose: g.value,
                glucose_unit: Some("mg/dL".into()),
                insulin: i.value,
                insulin_unit: Some("uU/mL".into()),
            };
            if let Ok(derived) = homa_ir(&self.registry, &input) {
                if let Ok(v) = serde_json::to_value(&derived) {
                    derived_candidates.push(v);
                }
            }
        }
        let payload = serde_json::json!({
            "latest_values": latest,
            "findings": interpretation.findings,
            "untested_suggestions": interpretation.untested_suggestions,
            "derived_candidates": derived_candidates,
        });
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Search the condition-card library (root-cause knowledge base): mechanisms, biomarkers, and honest evidence notes for patterns like low testosterone, thyroid dysfunction, insulin resistance, iron status, and cystic acne. Free-text query; returns scored matches."
    )]
    fn explore_condition(
        &self,
        Parameters(ConditionQuery { query, limit }): Parameters<ConditionQuery>,
    ) -> Result<CallToolResult, McpError> {
        let hits = self
            .conditions
            .search(&query, limit.unwrap_or(5).clamp(1, 20));
        let payload = serde_json::json!({
            "query": query,
            "hits": hits,
        });
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Register a clinical hypothesis in the ledger, e.g. 'elevated SHBG is suppressing free T'. Hypotheses are model-inference by definition: they carry fact/rule/PMID references and a review date, and are explicitly labeled as non-facts until supported or refuted by evidence."
    )]
    fn hypothesis_register(
        &self,
        Parameters(input): Parameters<HypothesisRegisterInput>,
    ) -> Result<CallToolResult, McpError> {
        if input.statement.trim().is_empty() {
            return Err(McpError::invalid_params("statement is required", None));
        }
        let id = self.db.add_hypothesis(&input).map_err(domain_error)?;
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "id": id, "status": "open", "review_by": input.review_by }),
        )?]))
    }

    #[tool(
        description = "List ledger hypotheses, optionally filtered by status (open, supported, refuted, superseded), newest first."
    )]
    fn hypothesis_list(
        &self,
        Parameters(HypothesisListInput { status, limit }): Parameters<HypothesisListInput>,
    ) -> Result<CallToolResult, McpError> {
        let rows = self
            .db
            .hypotheses(status.as_ref(), limit.unwrap_or(20).clamp(1, 200))
            .map_err(domain_error)?;
        let payload = serde_json::to_value(&rows)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
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
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "id": id, "status": status.as_str() }),
        )?]))
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
        let profile = self.db.profile().map_err(domain_error)?;
        let proposal = propose(&self.interventions, &targets, &profile);
        let payload = serde_json::to_value(&proposal)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
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
        let payload = serde_json::json!({
            "hits": hits,
            "clear": hits.is_empty(),
            "note": if hits.is_empty() { "no red-flag patterns matched; proceed with root-cause work" } else { "red-flag pattern(s) matched: address conventional care first" },
        });
        Ok(CallToolResult::success(vec![json_content(payload)?]))
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
        for analyte in &input.outcome_analytes {
            let known =
                self.registry.get(analyte).is_some() || DERIVED_IDS.contains(&analyte.as_str());
            if !known {
                return Err(McpError::invalid_params(
                    format!(
                        "outcome analyte not recognized: {analyte}. use registry analyte ids or derived ids ({}); commit labs first",
                        DERIVED_IDS.join(", ")
                    ),
                    None,
                ));
            }
        }
        let mut baseline_ids: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let latest = self.db.latest_fact_ids(&input.outcome_analytes);
        for (analyte, fact_id) in &latest {
            match fact_id {
                Some(id) => baseline_ids.push(id.clone()),
                None => missing.push(analyte.clone()),
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
            .begin_trial(&input, &card.name, baseline_ids)
            .map_err(domain_error)?;
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "id": id, "status": "planned", "baseline_fact_ids": self.db.get_trial(&id).ok().flatten().and_then(|t| t.baseline_fact_ids) }),
        )?]))
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
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "ok": true }),
        )?]))
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
                format!("trial not found: {}", input.id),
                None,
            ));
        }
        let row = self
            .db
            .get_trial(&input.id)
            .map_err(domain_error)?
            .ok_or_else(|| McpError::internal_error("trial vanished", None))?;
        let payload = serde_json::to_value(&row)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "List n=1 trials, optionally filtered by status (planned, active, completed, paused, aborted), newest first."
    )]
    fn trial_list(
        &self,
        Parameters(TrialListInput { status }): Parameters<TrialListInput>,
    ) -> Result<CallToolResult, McpError> {
        let rows = self.db.trials(status, 50).map_err(domain_error)?;
        let payload = serde_json::to_value(&rows)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Full n=1 trial report: trial row, chronological log, and outcome comparison — baseline fact values vs latest committed values per outcome analyte, with deltas. Commit retest labs with lab_commit, then read this."
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
        let mut outcomes = Vec::new();
        for analyte in &row.outcome_analytes {
            let baseline = row
                .baseline_fact_ids
                .as_ref()
                .and_then(|ids| ids.first())
                .and_then(|id| self.db.get_fact(id).ok().flatten());
            let latest = self.db.latest_fact(analyte).ok().flatten();
            outcomes.push(serde_json::json!({
                "analyte": analyte,
                "baseline": baseline.as_ref().map(|f| serde_json::json!({"value": f.value, "unit": f.unit, "collected_at": f.collected_at})),
                "latest": latest.as_ref().map(|f| serde_json::json!({"value": f.value, "unit": f.unit, "collected_at": f.collected_at})),
                "delta": match (baseline.as_ref(), latest.as_ref()) {
                    (Some(b), Some(l)) => Some(l.value - b.value),
                    _ => None,
                },
            }));
        }
        let payload = serde_json::json!({
            "trial": row,
            "log": log,
            "outcomes": outcomes,
        });
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "Validate, unit-convert, and range-assess a panel of lab values. Input: list of {analyte, value, unit?} where analyte accepts common names (e.g. 'total testosterone', 'TSH', 'ferritin') and unit is optional lab notation (e.g. 'nmol/L', 'ng/dL'). Returns per-analyte canonical values, plausibility checks, and both population and functional range assessments with sources. Does not persist; use for interpretation of raw values."
    )]
    fn lab_normalize(
        &self,
        Parameters(PanelInput { panel }): Parameters<PanelInput>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self.db.profile().map_err(domain_error)?;
        let results = normalize_panel(&self.registry, &self.ranges, &profile, &panel);
        let out: Vec<serde_json::Value> = results
            .iter()
            .map(|r| match r {
                Ok(res) => serde_json::to_value(res)
                    .unwrap_or(serde_json::json!({"error": "serialization failed"})),
                Err(e) => serde_json::json!({"error": e.to_string()}),
            })
            .collect();
        Ok(CallToolResult::success(vec![json_content(
            serde_json::json!({ "results": out }),
        )?]))
    }

    #[tool(
        description = "Calculated free testosterone (Vermeulen 1999) from total testosterone, SHBG, and optional albumin (assumed 4.3 g/dL if omitted). All units optional; accepts mass or molar notation."
    )]
    fn derived_free_testosterone(
        &self,
        Parameters(input): Parameters<FreeTInput>,
    ) -> Result<CallToolResult, McpError> {
        let derived = free_testosterone_vermeulen(&self.registry, &input).map_err(domain_error)?;
        let payload = serde_json::to_value(&derived)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }

    #[tool(
        description = "HOMA-IR fasting insulin resistance index from fasting glucose and insulin (Matthews 1985). Values above ~2 suggest insulin resistance; interpret alongside functional ranges."
    )]
    fn derived_homa_ir(
        &self,
        Parameters(input): Parameters<HomaInput>,
    ) -> Result<CallToolResult, McpError> {
        let derived = homa_ir(&self.registry, &input).map_err(domain_error)?;
        let payload = serde_json::to_value(&derived)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![json_content(payload)?]))
    }
}

#[tool_handler]
impl ServerHandler for Asclepius {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Asclepius, a root-cause holistic physician. Deterministic engine: unit conversion, dual reference ranges (population + functional), derived biomarkers. Every range carries a source and confidence. Prefer natural interventions; always surface red flags. Not a substitute for emergency or in-person care.",
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
    let registry = AnalyteRegistry::load_dir(&root.join("analytes"))?;
    let ranges = RangeSet::load_dir(&root.join("ranges"), &registry)?;
    let rules = RuleSet::load_dir(&root.join("rules"), &registry)?;
    let conditions = ConditionLibrary::load_dir(&root.join("conditions"))?;
    let interventions = InterventionLibrary::load_dir(&root.join("interventions"))?;
    let triage = TriageSet::load_dir(&root.join("triage"))?;
    let db_path = data_home().join("asclepius.db");
    let db = Db::open(&db_path)?;
    eprintln!(
        "asclepius {} serving: {} analytes, {} ranges, {} rules, {} condition cards, {} interventions, {} triage rules from {} | ledger {}",
        VERSION,
        registry.all().len(),
        ranges.len(),
        rules.len(),
        conditions.all().len(),
        interventions.all().len(),
        triage.len(),
        root.display(),
        db_path.display()
    );
    let doctor = Asclepius {
        registry: Arc::new(registry),
        ranges: Arc::new(ranges),
        rules: Arc::new(rules),
        conditions: Arc::new(conditions),
        interventions: Arc::new(interventions),
        triage: Arc::new(triage),
        db: Arc::new(db),
    };
    let service = doctor
        .serve(stdio())
        .await
        .map_err(|e| format!("serve error: {e}"))?;
    service.waiting().await?;
    Ok(())
}
