//! L3 verifier: independent recomputation of every claim before serialization.
//!
//! This module deliberately shares no code with the claim constructors or the
//! derived-value engine. The formulas below are reimplemented from their
//! published definitions (Vermeulen 1999; Matthews 1985) so that a bug in the
//! constructor path cannot hide inside the verifier. If verification fails,
//! the envelope is refused — the server never serializes a claim it cannot
//! re-derive.
//!
//! Evidence namespaces:
//! - `pack:...`      resolves against the L0 pack index (files + knowledge ids)
//! - `ledger:...`    resolves against the ledger (`ledger:facts/<id>` etc.)
//! - `engine:`, `gate:`, `query:`, `pubmed:`, `input:`  ephemeral namespaces,
//!   accepted only where an external record cannot exist (Meta, Reported, and
//!   direct-input Derived claims)

use crate::envelope::{
    earliest_overdue_review_by, Claim, ClaimKind, VerifierReport, ENVELOPE_SCHEMA_VERSION,
};
use crate::packs::PackIndex;
use crate::store::Db;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Physical constants used by the independent formula implementations.
/// Testosterone molar mass, g/mol (independently sourced from the analyte
/// definition; the constructor reads it from the registry, the verifier
/// hardcodes it — a registry data error would therefore be caught here).
const TESTOSTERONE_MOLAR_MASS_G: f64 = 288.42;
/// Vermeulen albumin binding constant, L/(g/dL): 1e6 / 66,500.
const VERMEULEN_K_ALB: f64 = 1.0e6 / 66500.0;

const EPHEMERAL_PREFIXES: &[&str] = &["engine:", "gate:", "query:", "pubmed:", "input:"];

fn is_ephemeral(r: &str) -> bool {
    EPHEMERAL_PREFIXES.iter().any(|p| r.starts_with(p))
}

fn close_enough(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * (1.0 + b.abs())
}

fn json_f64(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn json_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Verify all claims. `db` and `packs` are the resolution surfaces; `None`
/// disables the checks that need them (unit-test convenience — the MCP server
/// always passes both).
pub fn verify_claims(
    claims: &[Claim],
    tool: &str,
    db: Option<&Db>,
    packs: Option<&PackIndex>,
    today: &str,
) -> VerifierReport {
    let mut failures: Vec<String> = Vec::new();
    let mut seen_ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

    for claim in claims {
        let cid = claim.id.as_str();
        if !seen_ids.insert(cid) {
            failures.push(format!("{cid}: duplicate claim id"));
        }
        if claim.statement.trim().is_empty() {
            failures.push(format!("{cid}: empty statement"));
        }
        if claim.evidence.is_empty() {
            failures.push(format!("{cid}: no evidence binding — unbacked claims cannot exist"));
            continue;
        }

        match claim.kind {
            ClaimKind::Measured => {
                verify_measured(claim, db, &mut failures);
            }
            ClaimKind::Derived => {
                verify_derived(claim, &mut failures);
            }
            ClaimKind::Curated => {
                verify_curated(claim, packs, &mut failures);
            }
            ClaimKind::Inferred => {
                verify_inferred(claim, packs, db, &mut failures);
            }
            ClaimKind::Reported | ClaimKind::Meta | ClaimKind::Conflict => {
                verify_declared(claim, db, packs, &mut failures);
            }
        }

        // L1 staleness: if the value embeds any overdue review_by date, the
        // claim must carry a stale flag naming the earliest one.
        if let Some(earliest) = earliest_overdue_review_by(&claim.value, today) {
            match &claim.stale {
                Some(flag) if flag.since == earliest => {}
                _ => failures.push(format!(
                    "{cid}: value contains overdue review_by {earliest} but claim is not flagged stale"
                )),
            }
        }
    }

    let digest = envelope_digest(tool, claims);
    VerifierReport {
        status: if failures.is_empty() { "pass".into() } else { "fail".into() },
        claims_checked: claims.len(),
        failures,
        digest,
    }
}

/// sha256 over the canonical JSON of (schema_version, tool, claims).
pub fn envelope_digest(tool: &str, claims: &[Claim]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ENVELOPE_SCHEMA_VERSION.to_le_bytes());
    hasher.update(tool.as_bytes());
    hasher.update(b"|");
    if let Ok(json) = serde_json::to_string(claims) {
        hasher.update(json.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn verify_measured(claim: &Claim, db: Option<&Db>, failures: &mut Vec<String>) {
    let Some(db) = db else {
        failures.push(format!("{}: measured claim but no ledger available to verify", claim.id));
        return;
    };
    for r in &claim.evidence {
        let Some(fact_id) = r.strip_prefix("ledger:facts/") else {
            failures.push(format!(
                "{}: measured claim evidence must be 'ledger:facts/<id>', got '{r}'",
                claim.id
            ));
            continue;
        };
        match db.get_fact(fact_id) {
            Err(e) => failures.push(format!(
                "{}: ledger read error verifying fact '{fact_id}': {e}",
                claim.id
            )),
            Ok(None) => failures.push(format!(
                "{}: measured claim cites unknown fact '{fact_id}'",
                claim.id
            )),
            Ok(Some(fact)) => {
                let value_ok = json_f64(&claim.value, "value")
                    .map(|v| close_enough(v, fact.value))
                    .unwrap_or(false);
                let analyte_ok = json_str(&claim.value, "analyte") == Some(fact.analyte.as_str());
                if !value_ok {
                    failures.push(format!(
                        "{}: measured value {} does not match ledger fact '{fact_id}' ({})",
                        claim.id,
                        json_f64(&claim.value, "value").unwrap_or(f64::NAN),
                        fact.value
                    ));
                }
                if !analyte_ok {
                    failures.push(format!(
                        "{}: measured analyte {:?} does not match ledger fact '{fact_id}' ({:?})",
                        claim.id,
                        json_str(&claim.value, "analyte"),
                        fact.analyte
                    ));
                }
            }
        }
    }
}

fn verify_derived(claim: &Claim, failures: &mut Vec<String>) {
    // Direct-input derived claims cite ephemeral evidence; ledger-derived ones
    // cite fact ids. Both are shape-acceptable; the arithmetic is what the
    // verifier recomputes.
    let Some(computation) = &claim.computation else {
        failures.push(format!("{}: derived claim without computation", claim.id));
        return;
    };
    let Some(reported) = json_f64(&claim.value, "value") else {
        failures.push(format!(
            "{}: derived claim value has no numeric 'value' field to check",
            claim.id
        ));
        return;
    };
    let inputs = &computation.inputs;
    let recomputed = match computation.formula.as_str() {
        "vermeulen1999" => {
            match (
                inputs.get("total_testosterone_nmol_l").and_then(Value::as_f64),
                inputs.get("shbg_nmol_l").and_then(Value::as_f64),
                inputs.get("albumin_g_dl").and_then(Value::as_f64),
            ) {
                (Some(tt), Some(shbg), Some(alb)) => {
                    let free_nmoll = tt / (1.0 + shbg + VERMEULEN_K_ALB * alb);
                    Some(free_nmoll * TESTOSTERONE_MOLAR_MASS_G / 10.0)
                }
                _ => None,
            }
        }
        "matthews1985" => {
            match (
                inputs.get("glucose_mg_dl").and_then(Value::as_f64),
                inputs.get("insulin_uu_ml").and_then(Value::as_f64),
            ) {
                (Some(glucose), Some(insulin)) => Some(glucose * insulin / 405.0),
                _ => None,
            }
        }
        "delta" => {
            match (
                inputs.get("baseline").and_then(Value::as_f64),
                inputs.get("latest").and_then(Value::as_f64),
            ) {
                (Some(base), Some(latest)) => Some(latest - base),
                _ => None,
            }
        }
        other => {
            failures.push(format!(
                "{}: derived claim uses unknown formula '{other}' — unverifiable",
                claim.id
            ));
            None
        }
    };
    if let Some(expected) = recomputed {
        if !close_enough(reported, expected) {
            failures.push(format!(
                "{}: derived value {reported} does not match independent recomputation {expected} ({})",
                claim.id,
                computation.formula
            ));
        }
    } else if !failures.iter().any(|f| f.contains(&claim.id)) {
        failures.push(format!(
            "{}: computation inputs incomplete for formula {}",
            claim.id, computation.formula
        ));
    }
}

fn verify_curated(claim: &Claim, packs: Option<&PackIndex>, failures: &mut Vec<String>) {
    let Some(packs) = packs else {
        failures.push(format!("{}: curated claim but no pack index available", claim.id));
        return;
    };
    for r in &claim.evidence {
        if let Some(pack_ref) = r.strip_prefix("pack:") {
            let full = format!("pack:{pack_ref}");
            if !packs.pack_ref_exists(&full) {
                failures.push(format!(
                    "{}: curated claim cites unknown pack ref '{full}'",
                    claim.id
                ));
            }
        } else {
            failures.push(format!(
                "{}: curated claim evidence must be pack refs, got '{r}'",
                claim.id
            ));
        }
    }
}

fn verify_inferred(
    claim: &Claim,
    packs: Option<&PackIndex>,
    db: Option<&Db>,
    failures: &mut Vec<String>,
) {
    let mut grounded = false;
    for r in &claim.evidence {
        if let Some(pack_ref) = r.strip_prefix("pack:") {
            let full = format!("pack:{pack_ref}");
            if let Some(packs) = packs {
                if packs.pack_ref_exists(&full) {
                    grounded = true;
                } else {
                    failures.push(format!(
                        "{}: inferred claim cites unknown pack ref '{full}'",
                        claim.id
                    ));
                }
            } else {
                grounded = true;
            }
        } else if let Some(id) = r.strip_prefix("id:") {
            if let Some(packs) = packs {
                if packs.knowledge_id_exists(id) {
                    grounded = true;
                } else {
                    failures.push(format!(
                        "{}: inferred claim cites unknown knowledge id '{id}'",
                        claim.id
                    ));
                }
            } else {
                grounded = true;
            }
        } else if let Some(fact_id) = r.strip_prefix("ledger:facts/") {
            if let Some(db) = db {
                if db.get_fact(fact_id).ok().flatten().is_some() {
                    grounded = true;
                } else {
                    failures.push(format!(
                        "{}: inferred claim cites unknown fact '{fact_id}'",
                        claim.id
                    ));
                }
            } else {
                grounded = true;
            }
        } else if let Some(tres_id) = r.strip_prefix("ledger:treatment_responses/") {
            if let Some(db) = db {
                if db.treatment_response_exists(tres_id) {
                    grounded = true;
                } else {
                    failures.push(format!(
                        "{}: inferred claim cites unknown treatment response '{tres_id}'",
                        claim.id
                    ));
                }
            } else {
                grounded = true;
            }
        } else if r == "ledger:profile" {
            grounded = true;
        } else if is_ephemeral(r) {
            // allowed as supporting context, does not ground the claim
        } else {
            failures.push(format!(
                "{}: inferred claim evidence '{r}' is in no known namespace",
                claim.id
            ));
        }
    }
    if !grounded {
        failures.push(format!(
            "{}: inferred claim has no resolvable grounding (pack, knowledge id, or fact)",
            claim.id
        ));
    }
}

fn verify_declared(claim: &Claim, db: Option<&Db>, packs: Option<&PackIndex>, failures: &mut Vec<String>) {
    for r in &claim.evidence {
        if let Some(rest) = r.strip_prefix("ledger:") {
            // "ledger:<table>" accepted as a whole-table reference;
            // "ledger:<table>/<id>" must resolve.
            if let Some((table, id)) = rest.split_once('/') {
                let known = match (table, db) {
                    ("facts", Some(db)) => db.get_fact(id).ok().flatten().is_some(),
                    ("panels", Some(db)) => db.panel_exists(id),
                    ("hypotheses", Some(db)) => db.hypothesis_exists(id),
                    ("trials", Some(db)) => db.trial_exists(id),
                    ("treatment_responses", Some(db)) => db.treatment_response_exists(id),
                    (_, Some(_)) => false,
                    (_, None) => true, // no db to check against in this call
                };
                if !known {
                    failures.push(format!("{}: ledger ref '{r}' does not resolve", claim.id));
                }
            } else if !matches!(
                rest,
                "profile" | "facts" | "panels" | "hypotheses" | "trials" | "treatment_responses"
            ) {
                failures.push(format!("{}: unknown ledger table in ref '{r}'", claim.id));
            }
        } else if let Some(pack_ref) = r.strip_prefix("pack:") {
            let full = format!("pack:{pack_ref}");
            if let Some(packs) = packs {
                if !packs.pack_ref_exists(&full) {
                    failures.push(format!("{}: pack ref '{full}' does not resolve", claim.id));
                }
            }
        } else if let Some(id) = r.strip_prefix("id:") {
            if let Some(packs) = packs {
                if !packs.knowledge_id_exists(id) {
                    failures.push(format!("{}: knowledge id '{id}' does not resolve", claim.id));
                }
            }
        } else if is_epemeral_accepted(claim.kind, r) {
            // fine
        } else {
            failures.push(format!(
                "{}: {} claim evidence '{r}' is in no accepted namespace",
                claim.id,
                claim.kind.as_str()
            ));
        }
    }
}

fn is_epemeral_accepted(kind: ClaimKind, r: &str) -> bool {
    match kind {
        ClaimKind::Reported | ClaimKind::Meta | ClaimKind::Conflict => is_ephemeral(r),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{EnvelopeBuilder, StaleFlag};
    use crate::panel::AnalyteResult;
    use crate::range::RangeAssessment;
    use crate::range::{Confidence, RangeKind, RangeStatus};
    use crate::units::Unit;
    use std::collections::BTreeMap;

    fn packs_fixture() -> PackIndex {
        let mut files = std::collections::BTreeMap::new();
        files.insert("conditions/skin.toml".into(), "sha256:abc".into());
        let mut rule_ids = std::collections::BTreeSet::new();
        rule_ids.insert("tsh_above_functional".into());
        PackIndex::test_fixture(files, rule_ids, "2026-11-05")
    }

    fn result(analyte: &str, value: f64) -> AnalyteResult {
        AnalyteResult {
            analyte_id: analyte.into(),
            display_name: analyte.to_string(),
            input_value: value,
            input_unit: Some("mIU/L".into()),
            canonical_value: value,
            canonical_unit: Unit::MIuL,
            assessments: vec![RangeAssessment {
                kind: RangeKind::Population,
                low: 0.4,
                high: 4.5,
                unit: Unit::MIuL,
                status: RangeStatus::Within,
                source: "test".into(),
                pmid: None,
                confidence: Confidence::High,
                review_by: None,
            }],
            warnings: Vec::new(),
        }
    }

    fn db_with_tsh(db: &Db) -> String {
        let r = result("tsh", 2.0);
        let (_, ids) = db.commit_panel(None, None, &[&r], "lab_report").unwrap();
        ids[0].clone()
    }

    fn builder_with(claim: Claim) -> Vec<Claim> {
        vec![claim]
    }

    fn base_claim(kind: ClaimKind, value: Value, evidence: Vec<String>) -> Claim {
        Claim {
            id: "c1".into(),
            kind,
            statement: "test claim".into(),
            value,
            evidence,
            computation: None,
            tier: None,
            stale: None,
        }
    }

    const TODAY: &str = "2026-11-05";

    #[test]
    fn valid_measured_claim_passes() {
        let db = Db::in_memory().unwrap();
        let fact_id = db_with_tsh(&db);
        let claims = builder_with(base_claim(
            ClaimKind::Measured,
            serde_json::json!({"analyte": "tsh", "value": 2.0, "unit": "mIU/L"}),
            vec![format!("ledger:facts/{fact_id}")],
        ));
        let report = verify_claims(&claims, "lab_history", Some(&db), None, TODAY);
        assert_eq!(report.status, "pass", "{:?}", report.failures);
        assert!(report.digest.starts_with("sha256:"));
        assert_eq!(report.claims_checked, 1);
    }

    #[test]
    fn corrupt_value_detected() {
        let db = Db::in_memory().unwrap();
        let fact_id = db_with_tsh(&db);
        let claims = builder_with(base_claim(
            ClaimKind::Measured,
            serde_json::json!({"analyte": "tsh", "value": 9.9}),
            vec![format!("ledger:facts/{fact_id}")],
        ));
        let report = verify_claims(&claims, "lab_history", Some(&db), None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures[0].contains("does not match ledger fact"));
    }

    #[test]
    fn fabricated_fact_id_detected() {
        let db = Db::in_memory().unwrap();
        let claims = builder_with(base_claim(
            ClaimKind::Measured,
            serde_json::json!({"analyte": "tsh", "value": 2.0}),
            vec!["ledger:facts/lab_fabricated".into()],
        ));
        let report = verify_claims(&claims, "lab_history", Some(&db), None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures.iter().any(|f| f.contains("fabricated")));
    }

    #[test]
    fn unbacked_claim_rejected() {
        let claims = builder_with(base_claim(
            ClaimKind::Meta,
            serde_json::json!("something"),
            vec![],
        ));
        let report = verify_claims(&claims, "ping", None, None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures[0].contains("no evidence binding"));
    }

    #[test]
    fn derived_vermeulen_tampering_detected() {
        let mut inputs = BTreeMap::new();
        inputs.insert("total_testosterone_nmol_l".into(), serde_json::json!(17.34));
        inputs.insert("shbg_nmol_l".into(), serde_json::json!(30.0));
        inputs.insert("albumin_g_dl".into(), serde_json::json!(4.3));
        let mut b = EnvelopeBuilder::new("derived_free_testosterone");
        b.derived(
            "free T",
            serde_json::json!({"value": 99.0}),
            vec!["input:derived".into()],
            "vermeulen1999",
            inputs,
        );
        let report = verify_claims(b.claims(), b.tool(), None, None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures[0].contains("independent recomputation"));
    }

    #[test]
    fn derived_vermeulen_correct_value_passes() {
        // Worked example: TT 17.34 nmol/L, SHBG 30, alb 4.3 -> ~5.23 ng/dL.
        let mut inputs = BTreeMap::new();
        inputs.insert("total_testosterone_nmol_l".into(), serde_json::json!(17.34));
        inputs.insert("shbg_nmol_l".into(), serde_json::json!(30.0));
        inputs.insert("albumin_g_dl".into(), serde_json::json!(4.3));
        let expected = 17.34 / (1.0 + 30.0 + (1.0e6 / 66500.0) * 4.3) * 28.842;
        let mut b = EnvelopeBuilder::new("derived_free_testosterone");
        b.derived(
            "free T",
            serde_json::json!({"value": expected}),
            vec!["input:derived".into()],
            "vermeulen1999",
            inputs,
        );
        let report = verify_claims(b.claims(), b.tool(), None, None, TODAY);
        assert_eq!(report.status, "pass", "{:?}", report.failures);
    }

    #[test]
    fn derived_homa_and_delta_math() {
        let mut b = EnvelopeBuilder::new("derived_homa_ir");
        let mut inputs = BTreeMap::new();
        inputs.insert("glucose_mg_dl".into(), serde_json::json!(90.0));
        inputs.insert("insulin_uu_ml".into(), serde_json::json!(8.0));
        b.derived(
            "homa",
            serde_json::json!({"value": 90.0 * 8.0 / 405.0}),
            vec!["input:derived".into()],
            "matthews1985",
            inputs,
        );
        let report = verify_claims(b.claims(), b.tool(), None, None, TODAY);
        assert_eq!(report.status, "pass", "{:?}", report.failures);

        let mut b2 = EnvelopeBuilder::new("trial_report");
        let mut din = BTreeMap::new();
        din.insert("baseline".into(), serde_json::json!(100.0));
        din.insert("latest".into(), serde_json::json!(85.0));
        b2.derived(
            "delta",
            serde_json::json!({"value": -15.0}),
            vec!["input:derived".into()],
            "delta",
            din,
        );
        let report2 = verify_claims(b2.claims(), b2.tool(), None, None, TODAY);
        assert_eq!(report2.status, "pass", "{:?}", report2.failures);
    }

    #[test]
    fn unknown_formula_rejected() {
        let mut inputs = BTreeMap::new();
        inputs.insert("x".into(), serde_json::json!(1.0));
        let mut b = EnvelopeBuilder::new("t");
        b.derived(
            "mystery",
            serde_json::json!({"value": 1.0}),
            vec!["input:derived".into()],
            "gut_feeling",
            inputs,
        );
        let report = verify_claims(b.claims(), b.tool(), None, None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures.iter().any(|f| f.contains("unverifiable")));
    }

    #[test]
    fn curated_unknown_pack_ref_rejected() {
        let claims = builder_with(base_claim(
            ClaimKind::Curated,
            serde_json::json!({"summary": "text"}),
            vec!["pack:conditions/ghost.toml".into()],
        ));
        let report = verify_claims(&claims, "explore_condition", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures[0].contains("unknown pack ref"));
    }

    #[test]
    fn curated_valid_pack_ref_passes() {
        let claims = builder_with(base_claim(
            ClaimKind::Curated,
            serde_json::json!({"summary": "text"}),
            vec!["pack:conditions/skin.toml".into()],
        ));
        let report = verify_claims(&claims, "explore_condition", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report.status, "pass", "{:?}", report.failures);
    }

    #[test]
    fn inferred_requires_grounding() {
        let claims = builder_with(base_claim(
            ClaimKind::Inferred,
            serde_json::json!({"finding": "x"}),
            vec!["query:text".into()], // ephemeral only: not grounded
        ));
        let report = verify_claims(&claims, "interpret_panel", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures.iter().any(|f| f.contains("no resolvable grounding")));

        let grounded = builder_with(base_claim(
            ClaimKind::Inferred,
            serde_json::json!({"finding": "x"}),
            vec!["id:tsh_above_functional".into()],
        ));
        let report2 = verify_claims(&grounded, "interpret_panel", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report2.status, "pass", "{:?}", report2.failures);
    }

    #[test]
    fn stale_review_by_requires_flag() {
        let claims = builder_with(base_claim(
            ClaimKind::Curated,
            serde_json::json!({"summary": "s", "review_by": "2026-01-01"}),
            vec!["pack:conditions/skin.toml".into()],
        ));
        let report = verify_claims(&claims, "explore_condition", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures.iter().any(|f| f.contains("not flagged stale")));

        let mut flagged = claims;
        flagged[0].stale = Some(StaleFlag { since: "2026-01-01".into() });
        let report2 = verify_claims(&flagged, "explore_condition", None, Some(&packs_fixture()), TODAY);
        assert_eq!(report2.status, "pass", "{:?}", report2.failures);
    }

    #[test]
    fn duplicate_claim_ids_detected() {
        let mut c1 = base_claim(
            ClaimKind::Meta,
            serde_json::json!(1),
            vec!["engine:self".into()],
        );
        c1.id = "c1".into();
        let mut c2 = base_claim(
            ClaimKind::Meta,
            serde_json::json!(2),
            vec!["engine:self".into()],
        );
        c2.id = "c1".into();
        let report = verify_claims(&[c1, c2], "ping", None, None, TODAY);
        assert_eq!(report.status, "fail");
        assert!(report.failures.iter().any(|f| f.contains("duplicate claim id")));
    }
}
