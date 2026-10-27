use crate::error::AsclepiusError;
use crate::panel::AnalyteResult;
use crate::profile::Profile;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

pub const SCHEMA_VERSION: i64 = 1;

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
    collected_at TEXT,
    recorded_at INTEGER NOT NULL,
    note TEXT
);
CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY,
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
"#;

#[derive(Debug, Clone, Serialize)]
pub struct FactRow {
    pub id: String,
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

#[derive(Debug)]
pub struct Db {
    conn: Mutex<Connection>,
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
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn in_memory() -> Result<Self, AsclepiusError> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), AsclepiusError> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db_err)?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA).map_err(db_err)?;
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
            "INSERT INTO panels (id, collected_at, recorded_at, note) VALUES (?1, ?2, ?3, ?4)",
            params![panel_id, collected_at, now, note],
        )
        .map_err(db_err)?;
        let mut ids = Vec::with_capacity(results.len());
        for result in results {
            let id = format!("lab_{}", Uuid::new_v4());
            tx.execute(
                "INSERT INTO facts (id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note)
                 VALUES (?1, 'lab', ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?7, ?8, NULL, ?9)",
                params![
                    id,
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
                "SELECT id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note
                 FROM facts
                 WHERE superseded_by IS NULL AND (?1 IS NULL OR analyte = ?1)
                 ORDER BY recorded_at ASC, id ASC
                 LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![analyte, limit as i64], map_fact_row)
            .map_err(db_err)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn get_fact(&self, id: &str) -> Result<Option<FactRow>, AsclepiusError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, analyte, value, unit, input_ids, formula_version, source, panel_id, collected_at, recorded_at, superseded_by, note
                 FROM facts WHERE id = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query_map(params![id], map_fact_row).map_err(db_err)?;
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
}

fn map_fact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FactRow> {
    let input_ids: Option<String> = row.get(5)?;
    Ok(FactRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        analyte: row.get(2)?,
        value: row.get(3)?,
        unit: row.get(4)?,
        input_ids: input_ids.and_then(|s| serde_json::from_str(&s).ok()),
        formula_version: row.get(6)?,
        source: row.get(7)?,
        panel_id: row.get(8)?,
        collected_at: row.get(9)?,
        recorded_at: row.get(10)?,
        superseded_by: row.get(11)?,
        note: row.get(12)?,
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
}
