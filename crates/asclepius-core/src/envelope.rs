//! Assertion envelopes: the only shape tool responses may take.
//!
//! The engine never generates language — it generates claims. The only prose a
//! claim may carry is (a) its short `statement` label and (b) text quoted from
//! a content pack, referenced by id in `evidence`. A claim without evidence
//! bindings cannot be constructed; the verifier (L3) recomputes every claim
//! independently before serialization.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ENVELOPE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// A value measured in a lab and committed to the ledger.
    Measured,
    /// Data the patient (or their agent) asserted about themselves.
    Reported,
    /// A value computed from a published formula over measured/reported inputs.
    Derived,
    /// Text or structure quoted from a content pack, referenced by id.
    Curated,
    /// Engine reasoning over evidence (rule findings, scores, exclusions).
    Inferred,
    /// Contradictory evidence surfaced, not averaged away.
    Conflict,
    /// Operational facts (ids, counts, gate outcomes).
    Meta,
}

impl ClaimKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimKind::Measured => "measured",
            ClaimKind::Reported => "reported",
            ClaimKind::Derived => "derived",
            ClaimKind::Curated => "curated",
            ClaimKind::Inferred => "inferred",
            ClaimKind::Conflict => "conflict",
            ClaimKind::Meta => "meta",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Computation {
    /// Formula identifier, e.g. "vermeulen1999", "matthews1985", "delta".
    pub formula: String,
    /// The canonical-unit inputs the formula was applied to.
    pub inputs: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleFlag {
    /// The review_by date that was missed.
    pub since: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub kind: ClaimKind,
    pub statement: String,
    pub value: serde_json::Value,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computation: Option<Computation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<StaleFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Abstention {
    pub scope: String,
    pub reason: String,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifierReport {
    /// "pass" or "fail". A failed envelope is refused, never serialized.
    pub status: String,
    pub claims_checked: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub server: String,
    pub server_version: String,
    pub tool: String,
    pub claims: Vec<Claim>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub abstentions: Vec<Abstention>,
    /// Content hashes of every pack file this output was computed against.
    pub pack_hashes: BTreeMap<String, String>,
    pub verifier: VerifierReport,
}

impl Envelope {
    /// Compact view for the external audit ring (L5): every claim's id,
    /// kind, statement, and evidence, plus every top-level numeric field the
    /// claim carries (measured values, derived computations, inferred
    /// scores) — the numbers an LLM narrator is most likely to misquote. The
    /// bridge posts this after verification; the audit subagent diffs
    /// narration against it.
    pub fn audit_view(&self) -> serde_json::Value {
        let claims: Vec<serde_json::Value> = self
            .claims
            .iter()
            .map(|c| {
                let mut v = serde_json::json!({
                    "id": c.id,
                    "kind": c.kind.as_str(),
                    "statement": c.statement,
                    "evidence": c.evidence,
                });
                // Every top-level numeric field on the claim's value is
                // exposed to the auditor (measured `value`s, derived
                // computations, inferred `score`/`base`, trend counts) —
                // those are the numbers narration misquotes.
                if let Some(obj) = c.value.as_object() {
                    let numbers: serde_json::Map<String, serde_json::Value> = obj
                        .iter()
                        .filter_map(|(k, val)| {
                            val.as_f64().map(|n| (k.clone(), serde_json::json!(n)))
                        })
                        .collect();
                    if !numbers.is_empty() {
                        v["numbers"] = serde_json::Value::Object(numbers);
                    }
                }
                if let Some(tier) = &c.tier {
                    v["tier"] = serde_json::json!(tier);
                }
                if c.stale.is_some() {
                    v["stale"] = serde_json::json!(true);
                }
                v
            })
            .collect();
        serde_json::json!({
            "schema_version": self.schema_version,
            "server": self.server,
            "server_version": self.server_version,
            "tool": self.tool,
            "digest": self.verifier.digest,
            "verifier_status": self.verifier.status,
            "claims_checked": self.verifier.claims_checked,
            "pack_count": self.pack_hashes.len(),
            "claims": claims,
            "abstentions": self.abstentions.len(),
        })
    }
}

/// Incremental builder; claim ids are assigned on push (c1, c2, ...).
#[derive(Debug, Clone)]
pub struct EnvelopeBuilder {
    tool: String,
    claims: Vec<Claim>,
    abstentions: Vec<Abstention>,
    pack_hashes: BTreeMap<String, String>,
}

impl EnvelopeBuilder {
    pub fn new(tool: &str) -> Self {
        Self {
            tool: tool.into(),
            claims: Vec::new(),
            abstentions: Vec::new(),
            pack_hashes: BTreeMap::new(),
        }
    }

    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn set_pack_hashes(&mut self, hashes: BTreeMap<String, String>) -> &mut Self {
        self.pack_hashes = hashes;
        self
    }

    pub fn claim(
        &mut self,
        kind: ClaimKind,
        statement: impl Into<String>,
        value: serde_json::Value,
        evidence: Vec<String>,
    ) -> &mut Self {
        let id = format!("c{}", self.claims.len() + 1);
        self.claims.push(Claim {
            id,
            kind,
            statement: statement.into(),
            value,
            evidence,
            computation: None,
            tier: None,
            stale: None,
        });
        self
    }

    pub fn derived(
        &mut self,
        statement: impl Into<String>,
        value: serde_json::Value,
        evidence: Vec<String>,
        formula: &str,
        inputs: BTreeMap<String, serde_json::Value>,
    ) -> &mut Self {
        let id = format!("c{}", self.claims.len() + 1);
        self.claims.push(Claim {
            id,
            kind: ClaimKind::Derived,
            statement: statement.into(),
            value,
            evidence,
            computation: Some(Computation {
                formula: formula.into(),
                inputs,
            }),
            tier: None,
            stale: None,
        });
        self
    }

    pub fn tier(&mut self, tier: &str) -> &mut Self {
        if let Some(last) = self.claims.last_mut() {
            last.tier = Some(tier.into());
        }
        self
    }

    /// Mark the most recent claim stale if its value embeds an overdue
    /// `review_by` date (L1 flag-and-continue policy).
    pub fn mark_stale_from_value(&mut self, today: &str) -> &mut Self {
        if let Some(claim) = self.claims.last_mut() {
            if claim.stale.is_none() {
                if let Some(earliest) = earliest_overdue_review_by(&claim.value, today) {
                    claim.stale = Some(StaleFlag { since: earliest });
                }
            }
        }
        self
    }

    pub fn abstention(
        &mut self,
        scope: impl Into<String>,
        reason: impl Into<String>,
        missing: Vec<String>,
    ) -> &mut Self {
        self.abstentions.push(Abstention {
            scope: scope.into(),
            reason: reason.into(),
            missing,
        });
        self
    }

    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    pub fn abstentions(&self) -> &[Abstention] {
        &self.abstentions
    }

    pub fn pack_hashes(&self) -> &BTreeMap<String, String> {
        &self.pack_hashes
    }

    /// Assemble the final envelope. The verifier report must be produced by
    /// `crate::verify::verify_claims` over these exact claims; callers wire
    /// the two together (see asclepius-mcp).
    pub fn finish(
        self,
        server: &str,
        server_version: &str,
        verifier: VerifierReport,
    ) -> Envelope {
        Envelope {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            server: server.into(),
            server_version: server_version.into(),
            tool: self.tool,
            claims: self.claims,
            abstentions: self.abstentions,
            pack_hashes: self.pack_hashes,
            verifier,
        }
    }
}

/// Today as an ISO date (UTC), computed without external crates.
pub fn today_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// True when an ISO `review_by` date is strictly before `today`.
pub fn date_is_stale(review_by: &str, today: &str) -> bool {
    // Both are YYYY-MM-DD; lexicographic comparison is chronological
    // whenever both are well-formed. Malformed dates are not stale claims,
    // they are pack errors caught at load time.
    !review_by.is_empty() && review_by < today
}

/// Recursively find the earliest overdue `review_by` date inside a JSON
/// value. Shared by the builders (who must set the stale flag) and the
/// verifier (who re-checks they did).
pub fn earliest_overdue_review_by(value: &serde_json::Value, today: &str) -> Option<String> {
    let mut earliest: Option<String> = None;
    scan_review_by(value, today, &mut earliest);
    earliest
}

fn scan_review_by(value: &serde_json::Value, today: &str, out: &mut Option<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == "review_by" {
                    if let Some(date) = v.as_str() {
                        if date_is_stale(date, today) {
                            let better = match out {
                                Some(current) => date < current.as_str(),
                                None => true,
                            };
                            if better {
                                *out = Some(date.to_string());
                            }
                        }
                    }
                } else {
                    scan_review_by(v, today, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scan_review_by(item, today, out);
            }
        }
        _ => {}
    }
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Hinnant's days-from-civil inverse.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Add n days to an ISO date string (YYYY-MM-DD), returning YYYY-MM-DD.
pub fn add_days_iso(date: &str, n: i64) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return date.to_string();
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        parts[0].parse::<i64>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        return date.to_string();
    };
    let (y, m, d) = civil_from_days(days_from_civil(y, m, d) + n);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_assigns_sequential_ids() {
        let mut b = EnvelopeBuilder::new("ping");
        b.claim(
            ClaimKind::Meta,
            "server",
            serde_json::json!("asclepius"),
            vec!["engine:self".into()],
        )
        .claim(
            ClaimKind::Meta,
            "version",
            serde_json::json!("0.2.0"),
            vec!["engine:self".into()],
        );
        assert_eq!(b.claims()[0].id, "c1");
        assert_eq!(b.claims()[1].id, "c2");
    }

    #[test]
    fn audit_view_carries_numbers_and_handles_only() {
        let mut b = EnvelopeBuilder::new("derived_homa_ir");
        let mut inputs = BTreeMap::new();
        inputs.insert("glucose_mg_dl".into(), serde_json::json!(90.0));
        inputs.insert("insulin_uu_ml".into(), serde_json::json!(8.0));
        b.derived(
            "HOMA-IR",
            serde_json::json!({"value": 1.7778}),
            vec!["input:derived".into()],
            "matthews1985",
            inputs,
        )
        .claim(
            ClaimKind::Inferred,
            "cause score",
            serde_json::json!({"score": 0.85, "base": 0.35, "node": "gut_dysbiosis"}),
            vec!["id:gut_dysbiosis".into(), "pack:causal".into()],
        )
        .claim(
            ClaimKind::Meta,
            "context",
            serde_json::json!({"secret_detail": "not for audit"}),
            vec!["engine:self".into()],
        );
        let envelope = b.finish(
            "asclepius",
            "0.7.0",
            VerifierReport {
                status: "pass".into(),
                claims_checked: 3,
                failures: vec![],
                digest: "sha256:test".into(),
            },
        );
        let view = envelope.audit_view();
        assert_eq!(view["tool"], "derived_homa_ir");
        assert_eq!(view["digest"], "sha256:test");
        let derived = &view["claims"][0];
        assert_eq!(derived["kind"], "derived");
        assert_eq!(derived["numbers"]["value"], 1.7778);
        let inferred = &view["claims"][1];
        assert_eq!(inferred["numbers"]["score"], 0.85, "scores are auditable numbers");
        assert_eq!(inferred["numbers"]["base"], 0.35);
        let meta = &view["claims"][2];
        assert!(meta.get("numbers").is_none(), "non-numeric values stay out of the audit view");
    }

    #[test]
    fn derived_claim_carries_computation() {
        let mut b = EnvelopeBuilder::new("derived_homa_ir");
        let mut inputs = BTreeMap::new();
        inputs.insert("glucose_mg_dl".into(), serde_json::json!(90.0));
        inputs.insert("insulin_uu_ml".into(), serde_json::json!(8.0));
        b.derived(
            "HOMA-IR",
            serde_json::json!(1.778),
            vec!["query:derived".into()],
            "matthews1985",
            inputs,
        );
        assert_eq!(b.claims()[0].computation.as_ref().unwrap().formula, "matthews1985");
    }

    #[test]
    fn staleness_compare() {
        assert!(date_is_stale("2026-01-01", "2026-11-05"));
        assert!(!date_is_stale("2027-01-01", "2026-11-05"));
        assert!(!date_is_stale("", "2026-11-05"));
    }

    #[test]
    fn civil_date_algorithm_known_epoch() {
        // 2026-11-05 is day 20762 since 1970-01-01 (UTC):
        // 56 years (56*365 + 14 leap days) + 308 days into 2026.
        assert_eq!(civil_from_days(20_762), (2026, 11, 5));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }
}
