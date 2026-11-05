use crate::error::AsclepiusError;
use crate::panel::AnalyteResult;
use crate::profile::Profile;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

pub const SCHEMA_VERSION: i64 = 7;

/// Current schema (v5). `patient_id` scopes every ledger row to one patient
/// (single-patient today, multi-patient-ready); `origin` on hypotheses
/// records who authored the hypothesis ('user' | 'engine' | 'agent').
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS profile (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS panels (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    collected_at TEXT,
    recorded_at INTEGER NOT NULL,
    note TEXT
);
CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    kind TEXT NOT NULL CHECK (kind IN ('lab', 'derived')),
    analyte TEXT NOT NULL,
    value REAL NOT NULL,
    unit TEXT NOT NULL,
    input_ids TEXT,
    formula_version TEXT,
    source TEXT NOT NULL,
    panel_id TEXT REFERENCES panels(id),
    collected_at TEXT,
    recorded_at INTEGER NOT NULL,
    superseded_by TEXT REFERENCES facts(id),
    note TEXT
);
CREATE INDEX IF NOT EXISTS idx_facts_analyte ON facts(analyte);
CREATE INDEX IF NOT EXISTS idx_facts_panel ON facts(panel_id);
CREATE TABLE IF NOT EXISTS hypotheses (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    statement TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'supported', 'refuted', 'superseded')),
    basis TEXT,
    origin TEXT NOT NULL DEFAULT 'user' CHECK (origin IN ('user', 'engine', 'agent')),
    created_at INTEGER NOT NULL,
    review_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_hypotheses_status ON hypotheses(status);
CREATE TABLE IF NOT EXISTS trials (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    intervention_id TEXT NOT NULL,
    intervention_name TEXT NOT NULL,
    purpose TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('planned', 'active', 'completed', 'paused', 'aborted')),
    started_at INTEGER,
    ended_at INTEGER,
    baseline_fact_ids TEXT,
    outcome_analytes TEXT NOT NULL,
    dose TEXT,
    notes TEXT
);
CREATE INDEX IF NOT EXISTS idx_trials_status ON trials(status);
CREATE TABLE IF NOT EXISTS trial_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    trial_id TEXT NOT NULL REFERENCES trials(id),
    patient_id TEXT NOT NULL DEFAULT 'user',
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    text TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pubmed_cache (
    pmid TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    abstract TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS treatment_responses (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    response_id TEXT NOT NULL,
    note TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_treatment_responses_patient ON treatment_responses(patient_id);
CREATE TABLE IF NOT EXISTS symptom_logs (
    id TEXT PRIMARY KEY,
    patient_id TEXT NOT NULL DEFAULT 'user',
    scale_id TEXT NOT NULL,
    item_id TEXT,
    value REAL NOT NULL,
    observed_on TEXT,
    note TEXT,
    recorded_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_symptom_logs_scale ON symptom_logs(scale_id);
CREATE INDEX IF NOT EXISTS idx_symptom_logs_patient ON symptom_logs(patient_id);
"#;

/// v4 -> v5: patient scoping on every table + hypothesis origin column.
const MIGRATE_V4_TO_V5: &str = r#"
ALTER TABLE panels ADD COLUMN patient_id TEXT NOT NULL DEFAULT 'user';
ALTER TABLE facts ADD COLUMN patient_id TEXT NOT NULL DEFAULT 'user';
ALTER TABLE hypotheses ADD COLUMN patient_id TEXT NOT NULL DEFAULT 'user';
ALTER TABLE trials ADD COLUMN patient_id TEXT NOT NULL DEFAULT 'user';
ALTER TABLE trial_log ADD COLUMN patient_id TEXT NOT NULL DEFAULT 'user';
ALTER TABLE hypotheses ADD COLUMN origin TEXT NOT NULL DEFAULT 'user';
"#;

#[derive(Debug, Clone, Serialize)]
pub struct FactRow {
    pub id: String,
    pub patient_id: String,
    pub kind: String,
    pub analyte: String,
    pub value: f64,
    pub unit: String,
    pub input_ids: Option<Vec<String>>,
    pub formula_version: Option<String>,
    pub source: String,
    pub panel_id: Option<String>,
    pub collected_at: Option<String>,
    pub recorded_at: i64,
    pub superseded_by: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineageNode {
    pub fact: FactRow,
    pub inputs: Vec<LineageNode>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct HistoryQuery {
    #[serde(default)]
    pub analyte: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct LineageQuery {
    pub id: String,
    #[serde(default)]
    pub depth: Option<usize>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum HypothesisStatus {
    #[default]
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "refuted")]
    Refuted,
    #[serde(rename = "superseded")]
    Superseded,
}

impl HypothesisStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HypothesisStatus::Open => "open",
            HypothesisStatus::Supported => "supported",
            HypothesisStatus::Refuted => "refuted",
            HypothesisStatus::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HypothesisRow {
    pub id: String,
    pub patient_id: String,
    pub statement: String,
    pub status: HypothesisStatus,
    pub basis: Option<serde_json::Value>,
    pub origin: String,
    pub created_at: i64,
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HypothesisRegisterInput {
    pub statement: String,
    #[serde(default)]
    pub fact_ids: Vec<String>,
    #[serde(default)]
    pub rule_ids: Vec<String>,
    #[serde(default)]
    pub pmids: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
    /// Who authored this hypothesis: 'user' (default), 'engine', or 'agent'.
    #[serde(default)]
    pub origin: Option<String>,
}

impl HypothesisRegisterInput {
    pub fn origin_value(&self) -> &str {
        match self.origin.as_deref() {
            Some("engine") => "engine",
            Some("agent") => "agent",
            _ => "user",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct HypothesisListInput {
    #[serde(default)]
    pub status: Option<HypothesisStatus>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct HypothesisUpdateInput {
    pub id: String,
    pub status: HypothesisStatus,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum TrialStatus {
    #[default]
    #[serde(rename = "planned")]
    Planned,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "aborted")]
    Aborted,
}

impl TrialStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TrialStatus::Planned => "planned",
            TrialStatus::Active => "active",
            TrialStatus::Completed => "completed",
            TrialStatus::Paused => "paused",
            TrialStatus::Aborted => "aborted",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planned" => Some(TrialStatus::Planned),
            "active" => Some(TrialStatus::Active),
            "completed" => Some(TrialStatus::Completed),
            "paused" => Some(TrialStatus::Paused),
            "aborted" => Some(TrialStatus::Aborted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrialRow {
    pub id: String,
    pub patient_id: String,
    pub intervention_id: String,
    pub intervention_name: String,
    pub purpose: String,
    pub status: TrialStatus,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub baseline_fact_ids: Option<Vec<String>>,
    pub outcome_analytes: Vec<String>,
    pub dose: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrialLogRow {
    pub id: i64,
    pub trial_id: String,
    pub ts: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TrialBeginInput {
    pub intervention_id: String,
    pub purpose: String,
    pub outcome_analytes: Vec<String>,
    #[serde(default)]
    pub dose: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TrialLogInput {
    pub trial_id: String,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TrialStatusInput {
    pub trial_id: String,
    pub status: TrialStatus,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TrialListInput {
    #[serde(default)]
    pub status: Option<TrialStatus>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TrialReportInput {
    pub trial_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreatmentResponseRow {
    pub id: String,
    pub patient_id: String,
    pub response_id: String,
    pub note: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct TreatmentResponseRegisterInput {
    /// A response pattern id from the treatment-responses pack (validated).
    pub response_id: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymptomLogRow {
    pub id: String,
    pub patient_id: String,
    pub scale_id: String,
    pub item_id: Option<String>,
    pub value: f64,
    pub observed_on: Option<String>,
    pub note: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug)]
pub struct Db {
    conn: Mutex<Connection>,
    patient_id: String,
}

fn db_err(e: rusqlite::Error) -> AsclepiusError {
    AsclepiusError::Db(e.to_string())
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, AsclepiusError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        Self::migrate(&conn)?;
        let patient_id = std::env::var("ASCLEPIUS_PATIENT").unwrap_or_else(|_| "user".into());
        Ok(Self {
            conn: Mutex::new(conn),
            patient_id,
        })
    }

    pub fn in_memory() -> Result<Self, AsclepiusError> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            patient_id: "user".into(),
        })
    }

    /// The patient this Db instance reads and writes for.
    pub fn patient(&self) -> &str {
        &self.patient_id
    }

    pub fn panel_exists(&self, id: &str) -> bool {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM panels WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn hypothesis_exists(&self, id: &str) -> bool {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM hypotheses WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn trial_exists(&self, id: &str) -> bool {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM trials WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn treatment_response_exists(&self, id: &str) -> bool {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM treatment_responses WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn add_treatment_response(
        &self,
        input: &TreatmentResponseRegisterInput,
    ) -> Result<String, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let id = format!("tres_{}", Uuid::new_v4());
        conn.execute(
            "INSERT INTO treatment_responses (id, patient_id, response_id, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, self.patient_id, input.response_id, input.note, now_secs()],
        )
        .map_err(db_err)?;
        Ok(id)
    }

    /// Registered treatment responses for this patient, oldest first.
    pub fn treatment_responses(&self, limit: usize) -> Result<Vec<TreatmentResponseRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, response_id, note, created_at
                 FROM treatment_responses WHERE patient_id = ?1
                 ORDER BY created_at ASC, rowid ASC LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![self.patient_id, limit as i64], |row| {
                Ok(TreatmentResponseRow {
                    id: row.get(0)?,
                    patient_id: row.get(1)?,
                    response_id: row.get(2)?,
                    note: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn symptom_log_exists(&self, id: &str) -> bool {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM symptom_logs WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn get_symptom_log(&self, id: &str) -> Option<SymptomLogRow> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT id, patient_id, scale_id, item_id, value, observed_on, note, recorded_at
             FROM symptom_logs WHERE id = ?1 AND patient_id = ?2",
            params![id, self.patient_id],
            |row| {
                Ok(SymptomLogRow {
                    id: row.get(0)?,
                    patient_id: row.get(1)?,
                    scale_id: row.get(2)?,
                    item_id: row.get(3)?,
                    value: row.get(4)?,
                    observed_on: row.get(5)?,
                    note: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            },
        )
        .ok()
    }

    /// Append symptom observations (pre-validated against their scales).
    pub fn log_symptoms(
        &self,
        entries: &[crate::symptoms::SymptomEntryInput],
    ) -> Result<Vec<String>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut ids = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = format!("sym_{}", Uuid::new_v4());
            conn.execute(
                "INSERT INTO symptom_logs (id, patient_id, scale_id, item_id, value, observed_on, note, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    self.patient_id,
                    entry.scale_id,
                    entry.item_id,
                    entry.value,
                    entry.observed_on,
                    entry.note,
                    now_secs()
                ],
            )
            .map_err(db_err)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Symptom observations for this patient, oldest first; optional scale filter.
    pub fn symptom_logs(
        &self,
        scale_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymptomLogRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, scale_id, item_id, value, observed_on, note, recorded_at
                 FROM symptom_logs WHERE patient_id = ?1 AND (?2 IS NULL OR scale_id = ?2)
                 ORDER BY recorded_at DESC, rowid DESC LIMIT ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![self.patient_id, scale_id, limit as i64], |row| {
                Ok(SymptomLogRow {
                    id: row.get(0)?,
                    patient_id: row.get(1)?,
                    scale_id: row.get(2)?,
                    item_id: row.get(3)?,
                    value: row.get(4)?,
                    observed_on: row.get(5)?,
                    note: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Latest observation for a scale (and item, for instruments).
    pub fn latest_symptom(
        &self,
        scale_id: &str,
        item_id: Option<&str>,
    ) -> Option<SymptomLogRow> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT id, patient_id, scale_id, item_id, value, observed_on, note, recorded_at
             FROM symptom_logs
             WHERE patient_id = ?1 AND scale_id = ?2
               AND ((?3 IS NULL AND item_id IS NULL) OR item_id = ?3)
             ORDER BY recorded_at DESC, rowid DESC LIMIT 1",
            params![self.patient_id, scale_id, item_id],
            |row| {
                Ok(SymptomLogRow {
                    id: row.get(0)?,
                    patient_id: row.get(1)?,
                    scale_id: row.get(2)?,
                    item_id: row.get(3)?,
                    value: row.get(4)?,
                    observed_on: row.get(5)?,
                    note: row.get(6)?,
                    recorded_at: row.get(7)?,
                })
            },
        )
        .ok()
    }

    fn migrate(conn: &Connection) -> Result<(), AsclepiusError> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db_err)?;
        if version < SCHEMA_VERSION {
            // Idempotent base schema (fresh installs get v5 columns inline).
            conn.execute_batch(SCHEMA).map_err(db_err)?;
            if version >= 1 && version < 5 {
                // Existing pre-v5 ledger: add the columns it is missing.
                conn.execute_batch(MIGRATE_V4_TO_V5).map_err(db_err)?;
            }
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(db_err)?;
        }
        Ok(())
    }

    pub fn profile(&self) -> Result<Profile, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let data: Option<String> = conn
            .query_row("SELECT data FROM profile WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(db_err)?;
        match data {
            None => Ok(Profile::placeholder()),
            Some(json) => serde_json::from_str(&json).map_err(AsclepiusError::Json),
        }
    }

    pub fn set_profile(&self, profile: &Profile) -> Result<(), AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let json = serde_json::to_string(profile)?;
        conn.execute(
            "INSERT INTO profile (id, data) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![json],
        )
        .map_err(db_err)?;
        Ok(())
    }

    pub fn commit_panel(
        &self,
        collected_at: Option<&str>,
        note: Option<&str>,
        results: &[&AnalyteResult],
        source: &str,
    ) -> Result<(String, Vec<String>), AsclepiusError> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let panel_id = format!("pan_{}", Uuid::new_v4());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO panels (id, patient_id, collected_at, recorded_at, note) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![panel_id, self.patient_id, collected_at, now, note],
        )
        .map_err(db_err)?;
        let mut ids = Vec::with_capacity(results.len());
        for result in results {
            let id = format!("lab_{}", Uuid::new_v4());
            tx.execute(
                "INSERT INTO facts (id, patient_id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note)
                 VALUES (?1, ?2, 'lab', ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, ?9, NULL, ?10)",
                params![
                    id,
                    self.patient_id,
                    result.analyte_id,
                    result.canonical_value,
                    result.canonical_unit.label(),
                    source,
                    panel_id,
                    collected_at,
                    now,
                    result.input_unit.as_deref().map(|u| format!("reported as {} {}", result.input_value, u))
                ],
            )
            .map_err(db_err)?;
            ids.push(id);
        }
        tx.commit().map_err(db_err)?;
        Ok((panel_id, ids))
    }

    pub fn history(
        &self,
        analyte: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FactRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note
                 FROM facts
                 WHERE superseded_by IS NULL AND patient_id = ?1 AND (?2 IS NULL OR analyte = ?2)
                 ORDER BY recorded_at ASC, rowid ASC
                 LIMIT ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![self.patient_id, analyte, limit as i64], map_fact_row)
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn get_fact(&self, id: &str) -> Result<Option<FactRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note
                 FROM facts WHERE id = ?1 AND patient_id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![id, self.patient_id], map_fact_row)
            .map_err(db_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(db_err)?)),
            None => Ok(None),
        }
    }

    pub fn lineage(&self, id: &str, depth: usize) -> Result<Option<LineageNode>, AsclepiusError> {
        let fact = match self.get_fact(id)? {
            Some(f) => f,
            None => return Ok(None),
        };
        let inputs = if depth == 0 {
            Vec::new()
        } else {
            fact.input_ids
                .clone()
                .unwrap_or_default()
                .iter()
                .filter_map(|input_id| self.lineage(input_id, depth - 1).ok().flatten())
                .collect()
        };
        Ok(Some(LineageNode { fact, inputs }))
    }

    pub fn add_hypothesis(
        &self,
        input: &HypothesisRegisterInput,
    ) -> Result<String, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let id = format!("hyp_{}", Uuid::new_v4());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let basis = serde_json::to_value(input).ok().map(|v| v.to_string());
        conn.execute(
            "INSERT INTO hypotheses (id, patient_id, statement, status, basis, origin, created_at, review_by)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7)",
            params![
                id,
                self.patient_id,
                input.statement,
                basis,
                input.origin_value(),
                now,
                input.review_by
            ],
        )
        .map_err(db_err)?;
        Ok(id)
    }

    pub fn hypotheses(
        &self,
        status: Option<&HypothesisStatus>,
        limit: usize,
    ) -> Result<Vec<HypothesisRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let status_filter = status.map(|s| s.as_str());
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, statement, status, basis, origin, created_at, review_by
                 FROM hypotheses
                 WHERE patient_id = ?1 AND (?2 IS NULL OR status = ?2)
                 ORDER BY created_at DESC
                 LIMIT ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![self.patient_id, status_filter, limit as i64], |row| {
                let status_str: String = row.get(3)?;
                let basis: Option<String> = row.get(4)?;
                Ok(HypothesisRow {
                    id: row.get(0)?,
                    patient_id: row.get(1)?,
                    statement: row.get(2)?,
                    status: match status_str.as_str() {
                        "supported" => HypothesisStatus::Supported,
                        "refuted" => HypothesisStatus::Refuted,
                        "superseded" => HypothesisStatus::Superseded,
                        _ => HypothesisStatus::Open,
                    },
                    basis: basis.and_then(|b| serde_json::from_str(&b).ok()),
                    origin: row.get(5)?,
                    created_at: row.get(6)?,
                    review_by: row.get(7)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn set_hypothesis_status(
        &self,
        id: &str,
        status: HypothesisStatus,
    ) -> Result<bool, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let changed = conn
            .execute(
                "UPDATE hypotheses SET status = ?3 WHERE id = ?1 AND patient_id = ?2",
                params![id, self.patient_id, status.as_str()],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    pub fn begin_trial(
        &self,
        input: &TrialBeginInput,
        intervention_name: &str,
        baseline_fact_ids: Vec<String>,
    ) -> Result<String, AsclepiusError> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let id = format!("tri_{}", Uuid::new_v4());
        let now = now_secs();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO trials (id, patient_id, intervention_id, intervention_name, purpose, status, started_at, ended_at, baseline_fact_ids, outcome_analytes, dose, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, 'planned', NULL, NULL, ?6, ?7, ?8, ?9)",
            params![
                id,
                self.patient_id,
                input.intervention_id,
                intervention_name,
                input.purpose,
                serde_json::to_string(&baseline_fact_ids).ok(),
                serde_json::to_string(&input.outcome_analytes).ok(),
                input.dose,
                input.notes
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO trial_log (trial_id, patient_id, ts, kind, text) VALUES (?1, ?2, ?3, 'note', ?4)",
            params![id, self.patient_id, now, "trial created"],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(id)
    }

    pub fn append_trial_log(&self, input: &TrialLogInput) -> Result<bool, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let changed = conn
            .execute(
                "INSERT INTO trial_log (trial_id, patient_id, ts, kind, text)
                 SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS
                   (SELECT 1 FROM trials WHERE id = ?1 AND patient_id = ?2)",
                params![input.trial_id, self.patient_id, now_secs(), input.kind, input.text],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    pub fn set_trial_status(&self, input: &TrialStatusInput) -> Result<bool, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let now = now_secs();
        let scope = " AND patient_id = ?2";
        let changed = match input.status {
            TrialStatus::Active => conn
                .execute(
                    &format!(
                        "UPDATE trials SET status = 'active', started_at = COALESCE(started_at, ?3) WHERE id = ?1{scope}"
                    ),
                    params![input.trial_id, self.patient_id, now],
                )
                .map_err(db_err)?,
            TrialStatus::Completed | TrialStatus::Aborted => conn
                .execute(
                    &format!(
                        "UPDATE trials SET status = ?3, ended_at = ?4, started_at = COALESCE(started_at, ?4) WHERE id = ?1{scope}"
                    ),
                    params![input.trial_id, self.patient_id, input.status.as_str(), now],
                )
                .map_err(db_err)?,
            _ => conn
                .execute(
                    &format!("UPDATE trials SET status = ?3 WHERE id = ?1{scope}"),
                    params![input.trial_id, self.patient_id, input.status.as_str()],
                )
                .map_err(db_err)?,
        };
        Ok(changed > 0)
    }

    pub fn trials(
        &self,
        status: Option<TrialStatus>,
        limit: usize,
    ) -> Result<Vec<TrialRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let status_filter = status.map(|s| s.as_str());
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, intervention_id, intervention_name, purpose, status, started_at, ended_at, baseline_fact_ids, outcome_analytes, dose, notes
                 FROM trials
                 WHERE patient_id = ?1 AND (?2 IS NULL OR status = ?2)
                 ORDER BY rowid DESC
                 LIMIT ?3",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![self.patient_id, status_filter, limit as i64], map_trial_row)
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn get_trial(&self, id: &str) -> Result<Option<TrialRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, intervention_id, intervention_name, purpose, status, started_at, ended_at, baseline_fact_ids, outcome_analytes, dose, notes
                 FROM trials WHERE id = ?1 AND patient_id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![id, self.patient_id], map_trial_row)
            .map_err(db_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(db_err)?)),
            None => Ok(None),
        }
    }

    pub fn trial_log_entries(&self, trial_id: &str) -> Result<Vec<TrialLogRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, trial_id, ts, kind, text FROM trial_log
                 WHERE trial_id = ?1 AND patient_id = ?2 ORDER BY id ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![trial_id, self.patient_id], |row| {
                Ok(TrialLogRow {
                    id: row.get(0)?,
                    trial_id: row.get(1)?,
                    ts: row.get(2)?,
                    kind: row.get(3)?,
                    text: row.get(4)?,
                })
            })
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn pubmed_cache_get(&self, pmid: &str) -> Option<crate::literature::PubMedArticle> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row(
            "SELECT pmid, title, abstract FROM pubmed_cache WHERE pmid = ?1",
            params![pmid],
            |row| {
                Ok(crate::literature::PubMedArticle {
                    pmid: row.get(0)?,
                    title: row.get(1)?,
                    abstract_text: row.get(2)?,
                })
            },
        )
        .ok()
    }

    pub fn pubmed_cache_put(
        &self,
        articles: &[crate::literature::PubMedArticle],
    ) -> Result<(), AsclepiusError> {
        let mut conn = self.conn.lock().expect("db mutex poisoned");
        let tx = conn.transaction().map_err(db_err)?;
        for article in articles {
            tx.execute(
                "INSERT INTO pubmed_cache (pmid, title, abstract, fetched_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(pmid) DO UPDATE SET title = excluded.title, abstract = excluded.abstract, fetched_at = excluded.fetched_at",
                params![article.pmid, article.title, article.abstract_text, now_secs()],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn pubmed_cache_len(&self) -> usize {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.query_row("SELECT COUNT(*) FROM pubmed_cache", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0) as usize
    }

    pub fn latest_fact(&self, analyte: &str) -> Result<Option<FactRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, patient_id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note
                 FROM facts
                 WHERE superseded_by IS NULL AND patient_id = ?1 AND analyte = ?2
                 ORDER BY recorded_at DESC, rowid DESC
                 LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![self.patient_id, analyte], map_fact_row)
            .map_err(db_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(db_err)?)),
            None => Ok(None),
        }
    }

    pub fn latest_fact_ids(&self, analytes: &[String]) -> Vec<(String, Option<String>)> {
        analytes
            .iter()
            .map(|a| {
                let id = self.latest_fact(a).ok().and_then(|row| row.map(|r| r.id));
                (a.clone(), id)
            })
            .collect()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn map_trial_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrialRow> {
    let status_str: String = row.get(5)?;
    let baseline: Option<String> = row.get(8)?;
    let outcomes: Option<String> = row.get(9)?;
    Ok(TrialRow {
        id: row.get(0)?,
        patient_id: row.get(1)?,
        intervention_id: row.get(2)?,
        intervention_name: row.get(3)?,
        purpose: row.get(4)?,
        status: TrialStatus::parse(&status_str).unwrap_or(TrialStatus::Planned),
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        baseline_fact_ids: baseline.and_then(|b| serde_json::from_str(&b).ok()),
        outcome_analytes: outcomes
            .and_then(|o| serde_json::from_str(&o).ok())
            .unwrap_or_default(),
        dose: row.get(10)?,
        notes: row.get(11)?,
    })
}

fn map_fact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FactRow> {
    let input_ids: Option<String> = row.get(6)?;
    Ok(FactRow {
        id: row.get(0)?,
        patient_id: row.get(1)?,
        kind: row.get(2)?,
        analyte: row.get(3)?,
        value: row.get(4)?,
        unit: row.get(5)?,
        input_ids: input_ids.and_then(|s| serde_json::from_str(&s).ok()),
        formula_version: row.get(7)?,
        source: row.get(8)?,
        panel_id: row.get(9)?,
        collected_at: row.get(10)?,
        recorded_at: row.get(11)?,
        superseded_by: row.get(12)?,
        note: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::RangeAssessment;
    use crate::range::{Confidence, RangeKind, RangeStatus};
    use crate::units::Unit;

    fn result(analyte: &str, value: f64) -> AnalyteResult {
        AnalyteResult {
            analyte_id: analyte.into(),
            display_name: analyte.to_string(),
            input_value: value,
            input_unit: Some("ng/dL".into()),
            canonical_value: value,
            canonical_unit: Unit::NgDl,
            assessments: vec![RangeAssessment {
                kind: RangeKind::Population,
                low: 100.0,
                high: 500.0,
                unit: Unit::NgDl,
                status: RangeStatus::Within,
                source: "test".into(),
                pmid: None,
                confidence: Confidence::High,
                review_by: None,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn profile_roundtrip() {
        let db = Db::in_memory().unwrap();
        assert_eq!(db.profile().unwrap().name.as_deref(), Some("user"));
        let mut p = Profile::placeholder();
        p.sex = Some(crate::profile::Sex::Male);
        p.age_years = Some(30);
        p.conditions.push("cystic acne".into());
        db.set_profile(&p).unwrap();
        let back = db.profile().unwrap();
        assert_eq!(back.age_years, Some(30));
        assert_eq!(back.conditions, vec!["cystic acne".to_string()]);
    }

    #[test]
    fn commit_and_history() {
        let db = Db::in_memory().unwrap();
        let r1 = result("testosterone_total", 500.0);
        let (panel_id, ids) = db
            .commit_panel(Some("2026-10-01"), None, &[&r1], "lab_report")
            .unwrap();
        assert!(panel_id.starts_with("pan_"));
        assert_eq!(ids.len(), 1);
        let hist = db.history(Some("testosterone_total"), 10).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].id, ids[0]);
        assert_eq!(hist[0].panel_id.as_deref(), Some(panel_id.as_str()));
        assert_eq!(hist[0].collected_at.as_deref(), Some("2026-10-01"));
    }

    #[test]
    fn history_respects_limit_and_filter() {
        let db = Db::in_memory().unwrap();
        let r_t = result("tsh", 2.0);
        let r_f = result("ferritin", 90.0);
        db.commit_panel(None, None, &[&r_t], "lab_report").unwrap();
        db.commit_panel(None, None, &[&r_f], "lab_report").unwrap();
        db.commit_panel(None, None, &[&r_t], "lab_report").unwrap();
        assert_eq!(db.history(None, 100).unwrap().len(), 3);
        assert_eq!(db.history(Some("tsh"), 100).unwrap().len(), 2);
        assert_eq!(db.history(None, 2).unwrap().len(), 2);
    }

    #[test]
    fn lineage_of_lab_fact_is_leaf() {
        let db = Db::in_memory().unwrap();
        let r = result("ferritin", 90.0);
        let (_, ids) = db.commit_panel(None, None, &[&r], "lab_report").unwrap();
        let node = db.lineage(&ids[0], 4).unwrap().unwrap();
        assert_eq!(node.fact.analyte, "ferritin");
        assert!(node.inputs.is_empty());
        assert!(db.lineage("lab_nonexistent", 4).unwrap().is_none());
    }

    #[test]
    fn hypothesis_lifecycle() {
        let db = Db::in_memory().unwrap();
        let input = HypothesisRegisterInput {
            statement: "Low free T is driven by elevated SHBG".into(),
            fact_ids: vec!["lab_x".into()],
            rule_ids: vec!["shbg_above_functional".into()],
            pmids: vec![],
            note: Some("check again after thyroid optimization".into()),
            review_by: Some("2027-01-01".into()),
            origin: None,
        };
        let id = db.add_hypothesis(&input).unwrap();
        assert!(id.starts_with("hyp_"));
        let all = db.hypotheses(None, 10).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, HypothesisStatus::Open);
        assert!(all[0].basis.is_some());
        let open = db.hypotheses(Some(&HypothesisStatus::Open), 10).unwrap();
        assert_eq!(open.len(), 1);
        assert!(
            db.hypotheses(Some(&HypothesisStatus::Refuted), 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            db.set_hypothesis_status(&id, HypothesisStatus::Supported)
                .unwrap()
        );
        assert_eq!(
            db.hypotheses(None, 10).unwrap()[0].status,
            HypothesisStatus::Supported
        );
        assert!(
            !db.set_hypothesis_status("hyp_missing", HypothesisStatus::Refuted)
                .unwrap()
        );
    }

    #[test]
    fn history_same_second_ties_break_by_insertion_order() {
        let db = Db::in_memory().unwrap();
        let first = result("ferritin", 100.0);
        db.commit_panel(None, None, &[&first], "lab_report")
            .unwrap();
        let second = result("ferritin", 200.0);
        db.commit_panel(None, None, &[&second], "lab_report")
            .unwrap();
        let hist = db.history(Some("ferritin"), 10).unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(
            hist[0].value, 100.0,
            "insertion order must hold within a second"
        );
        assert_eq!(hist[1].value, 200.0);
    }

    #[test]
    fn trial_lifecycle_with_log() {
        let db = Db::in_memory().unwrap();
        let input = TrialBeginInput {
            intervention_id: "zinc_picolinate".into(),
            purpose: "test effect on skin".into(),
            outcome_analytes: vec!["testosterone_total".into()],
            dose: Some("25 mg/day".into()),
            notes: None,
        };
        let id = db
            .begin_trial(&input, "Zinc (picolinate)", vec!["lab_abc".into()])
            .unwrap();
        assert!(id.starts_with("tri_"));
        assert!(db.trial_exists(&id));
        let rows = db.trials(None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].patient_id, "user");
        assert_eq!(rows[0].status, TrialStatus::Planned);
        assert_eq!(
            rows[0].baseline_fact_ids.clone().unwrap(),
            vec!["lab_abc".to_string()]
        );
        assert!(
            db.append_trial_log(&TrialLogInput {
                trial_id: id.clone(),
                kind: "note".into(),
                text: "week 2: no change".into(),
            })
            .unwrap()
        );
        assert!(
            db.set_trial_status(&TrialStatusInput {
                trial_id: id.clone(),
                status: TrialStatus::Active,
            })
            .unwrap()
        );
        let row = db.get_trial(&id).unwrap().unwrap();
        assert_eq!(row.status, TrialStatus::Active);
        assert!(row.started_at.is_some());
        let log = db.trial_log_entries(&id).unwrap();
        assert_eq!(log.len(), 2);
        assert!(
            db.set_trial_status(&TrialStatusInput {
                trial_id: id.clone(),
                status: TrialStatus::Completed,
            })
            .unwrap()
        );
        assert!(db.get_trial(&id).unwrap().unwrap().ended_at.is_some());
        assert!(
            !db.set_trial_status(&TrialStatusInput {
                trial_id: "tri_missing".into(),
                status: TrialStatus::Aborted,
            })
            .unwrap()
        );
    }

    /// The v4 schema exactly as v0.1 shipped it (no patient_id, no origin).
    const V4_SCHEMA: &str = r#"
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS profile (id INTEGER PRIMARY KEY CHECK (id = 1), data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS panels (id TEXT PRIMARY KEY, collected_at TEXT, recorded_at INTEGER NOT NULL, note TEXT);
        CREATE TABLE IF NOT EXISTS facts (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('lab','derived')),
            analyte TEXT NOT NULL, value REAL NOT NULL, unit TEXT NOT NULL,
            input_ids TEXT, formula_version TEXT, source TEXT NOT NULL,
            panel_id TEXT REFERENCES panels(id), collected_at TEXT,
            recorded_at INTEGER NOT NULL, superseded_by TEXT REFERENCES facts(id), note TEXT
        );
        CREATE TABLE IF NOT EXISTS hypotheses (
            id TEXT PRIMARY KEY, statement TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('open','supported','refuted','superseded')),
            basis TEXT, created_at INTEGER NOT NULL, review_by TEXT
        );
        CREATE TABLE IF NOT EXISTS trials (
            id TEXT PRIMARY KEY, intervention_id TEXT NOT NULL, intervention_name TEXT NOT NULL,
            purpose TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('planned','active','completed','paused','aborted')),
            started_at INTEGER, ended_at INTEGER, baseline_fact_ids TEXT,
            outcome_analytes TEXT NOT NULL, dose TEXT, notes TEXT
        );
        CREATE TABLE IF NOT EXISTS trial_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT, trial_id TEXT NOT NULL REFERENCES trials(id),
            ts INTEGER NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS pubmed_cache (pmid TEXT PRIMARY KEY, title TEXT NOT NULL, abstract TEXT NOT NULL, fetched_at INTEGER NOT NULL);
    "#;

    #[test]
    fn v4_ledger_migrates_with_data_preserved() {
        let path = std::env::temp_dir().join("asclepius-migration-v4-test.db");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V4_SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO panels (id, collected_at, recorded_at) VALUES ('pan_old', '2026-10-01', 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO facts (id, kind, analyte, value, unit, source, panel_id, recorded_at)
                 VALUES ('lab_old', 'lab', 'tsh', 2.0, 'mIU/L', 'lab_report', 'pan_old', 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO hypotheses (id, statement, status, created_at)
                 VALUES ('hyp_old', 'gut dysbiosis drives acne', 'open', 1000)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }
        // Reopen through the new code: migration v4 -> v5 runs.
        let db = Db::open(&path).unwrap();
        let fact = db.get_fact("lab_old").unwrap().expect("fact survived migration");
        assert_eq!(fact.analyte, "tsh");
        assert_eq!(fact.value, 2.0);
        assert_eq!(fact.patient_id, "user");
        let hyp = db.hypotheses(None, 10).unwrap();
        assert_eq!(hyp.len(), 1);
        assert_eq!(hyp[0].id, "hyp_old");
        assert_eq!(hyp[0].origin, "user", "pre-v5 hypotheses default to user origin");
        assert_eq!(hyp[0].patient_id, "user");
        assert!(db.panel_exists("pan_old"));
        // Version bumped exactly once; reopening is a no-op.
        {
            let conn = Connection::open(&path).unwrap();
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, SCHEMA_VERSION);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hypothesis_origin_roundtrip() {
        let db = Db::in_memory().unwrap();
        let mut input = HypothesisRegisterInput {
            statement: "engine-seeded hypothesis".into(),
            ..HypothesisRegisterInput::default()
        };
        input.origin = Some("agent".into());
        db.add_hypothesis(&input).unwrap();
        // Unknown origin values collapse to 'user'.
        let mut rogue = HypothesisRegisterInput {
            statement: "rogue".into(),
            ..HypothesisRegisterInput::default()
        };
        rogue.origin = Some("skynet".into());
        db.add_hypothesis(&rogue).unwrap();
        let rows = db.hypotheses(None, 10).unwrap();
        let agent = rows.iter().find(|r| r.statement == "engine-seeded hypothesis").unwrap();
        assert_eq!(agent.origin, "agent");
        let rogue_row = rows.iter().find(|r| r.statement == "rogue").unwrap();
        assert_eq!(rogue_row.origin, "user");
    }

    #[test]
    fn treatment_response_roundtrip() {
        let db = Db::in_memory().unwrap();
        let input = TreatmentResponseRegisterInput {
            response_id: "antibiotic_transient_clearance".into(),
            note: Some("doxycycline 3 months, recurred within 2".into()),
        };
        let id = db.add_treatment_response(&input).unwrap();
        assert!(id.starts_with("tres_"));
        assert!(db.treatment_response_exists(&id));
        let rows = db.treatment_responses(50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].response_id, "antibiotic_transient_clearance");
        assert_eq!(rows[0].patient_id, "user");
        assert!(!db.treatment_response_exists("tres_missing"));
    }

    #[test]
    fn symptom_log_roundtrip() {
        let db = Db::in_memory().unwrap();
        let entries = vec![
            crate::symptoms::SymptomEntryInput {
                scale_id: "genital_sensation".into(),
                item_id: None,
                value: 4.0,
                observed_on: Some("2026-11-05".into()),
                note: Some("slight improvement this week".into()),
            },
            crate::symptoms::SymptomEntryInput {
                scale_id: "iief5_adapted".into(),
                item_id: Some("q1".into()),
                value: 3.0,
                observed_on: Some("2026-11-05".into()),
                note: None,
            },
        ];
        let ids = db.log_symptoms(&entries).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids[0].starts_with("sym_"));
        assert!(db.symptom_log_exists(&ids[0]));
        assert_eq!(db.get_symptom_log(&ids[0]).unwrap().value, 4.0);
        let all = db.symptom_logs(None, 50).unwrap();
        assert_eq!(all.len(), 2);
        let filtered = db.symptom_logs(Some("genital_sensation"), 50).unwrap();
        assert_eq!(filtered.len(), 1);
        let latest_single = db.latest_symptom("genital_sensation", None).unwrap();
        assert_eq!(latest_single.id, ids[0]);
        let latest_item = db.latest_symptom("iief5_adapted", Some("q1")).unwrap();
        assert_eq!(latest_item.value, 3.0);
        assert!(db.latest_symptom("iief5_adapted", Some("q2")).is_none());
        assert!(!db.symptom_log_exists("sym_missing"));
    }
}
