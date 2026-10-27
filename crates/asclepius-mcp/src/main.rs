use asclepius_core::analyte::AnalyteRegistry;
use asclepius_core::conditions::{ConditionLibrary, ConditionQuery};
use asclepius_core::derived::{FreeTInput, HomaInput, free_testosterone_vermeulen, homa_ir};
use asclepius_core::panel::{
    AnalyteResult, CommitInput, PanelInput, normalize_panel, normalize_value,
};
use asclepius_core::profile::ProfileUpdate;
use asclepius_core::range::RangeSet;
use asclepius_core::rules::{LatestValue, RuleSet, interpret};
use asclepius_core::store::{
    Db, HistoryQuery, HypothesisListInput, HypothesisRegisterInput, HypothesisUpdateInput,
    LineageQuery,
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
    let db_path = data_home().join("asclepius.db");
    let db = Db::open(&db_path)?;
    eprintln!(
        "asclepius {} serving: {} analytes, {} ranges, {} rules, {} condition cards from {} | ledger {}",
        VERSION,
        registry.all().len(),
        ranges.len(),
        rules.len(),
        conditions.all().len(),
        root.display(),
        db_path.display()
    );
    let doctor = Asclepius {
        registry: Arc::new(registry),
        ranges: Arc::new(ranges),
        rules: Arc::new(rules),
        conditions: Arc::new(conditions),
        db: Arc::new(db),
    };
    let service = doctor
        .serve(stdio())
        .await
        .map_err(|e| format!("serve error: {e}"))?;
    service.waiting().await?;
    Ok(())
}
