//! L0 load gate: pack loading, validation, content hashing, staleness.
//!
//! Every content pack file is read, hashed (sha256), parsed, and
//! cross-validated against the other domains at startup. Files that fail are
//! *excluded* and reported — the server starts in degraded mode rather than
//! silently proceeding with corrupt knowledge. `ping` exposes the exclusions.
//!
//! This module deliberately works on raw `toml::Value` tables: it is a gate,
//! not a second copy of the domain parsers. The domain loaders
//! (`AnalyteRegistry::load_paths` etc.) then load only the files this gate
//! admitted.

use crate::derived::DERIVED_IDS;
use crate::envelope::today_iso;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const DOMAINS: &[&str] = &[
    "analytes",
    "ranges",
    "rules",
    "conditions",
    "interventions",
    "triage",
    "causal",
    "treatment-responses",
    "tests",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedFile {
    pub rel_path: String,
    pub reason: String,
}

/// A forward reference that does not resolve today: a rule suggesting an
/// analyte the registry lacks, or a condition listing a biomarker nobody
/// defined. These are knowledge gaps made visible, not file errors — the
/// engine may legitimately say "investigate X next" for tests that cannot be
/// committed yet. `ping` surfaces them so the curation queue knows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGap {
    pub rel_path: String,
    pub kind: String, // "suggested_analyte" | "listed_biomarker"
    pub id: String,
}

#[derive(Debug, Clone, Default)]
pub struct PackIndex {
    /// "ranges/core.toml" -> "sha256:<hex>"
    files: BTreeMap<String, String>,
    excluded: Vec<ExcludedFile>,
    gaps: Vec<KnowledgeGap>,
    analyte_ids: BTreeSet<String>,
    rule_ids: BTreeSet<String>,
    triage_ids: BTreeSet<String>,
    condition_ids: BTreeSet<String>,
    intervention_ids: BTreeSet<String>,
    causal_node_ids: BTreeSet<String>,
    response_ids: BTreeSet<String>,
    test_ids: BTreeSet<String>,
    pub today: String,
}

impl PackIndex {
    #[cfg(test)]
    pub(crate) fn test_fixture(
        files: BTreeMap<String, String>,
        rule_ids: BTreeSet<String>,
        today: &str,
    ) -> Self {
        Self {
            files,
            excluded: Vec::new(),
            gaps: Vec::new(),
            analyte_ids: BTreeSet::new(),
            rule_ids,
            triage_ids: BTreeSet::new(),
            condition_ids: BTreeSet::new(),
            intervention_ids: BTreeSet::new(),
            causal_node_ids: BTreeSet::new(),
            response_ids: BTreeSet::new(),
            test_ids: BTreeSet::new(),
            today: today.into(),
        }
    }

    pub fn hashes(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    pub fn excluded(&self) -> &[ExcludedFile] {
        &self.excluded
    }

    pub fn gaps(&self) -> &[KnowledgeGap] {
        &self.gaps
    }

    /// Resolve a pack evidence reference. Accepted shapes:
    /// `pack:<domain>` (any file in that domain loaded),
    /// `pack:<domain>/<file>.toml`, `pack:<domain>/<file>.toml#anchor`
    /// (anchors are validated for file existence; entry anchors arrive with
    /// the Phase 1 causal pack, which carries ids).
    pub fn pack_ref_exists(&self, r: &str) -> bool {
        let Some(rest) = r.strip_prefix("pack:") else {
            return false;
        };
        let path_part = rest.split('#').next().unwrap_or(rest);
        if path_part.is_empty() {
            return false;
        }
        if !path_part.contains('/') {
            return DOMAINS.contains(&path_part) && self.domain_has_files(path_part);
        }
        self.files.contains_key(path_part)
    }

    fn domain_has_files(&self, domain: &str) -> bool {
        self.files.keys().any(|k| k.starts_with(&format!("{domain}/")))
    }

    /// Whether a knowledge id (rule, triage rule, condition, intervention,
    /// causal node, treatment-response pattern, investigation test) exists in
    /// the admitted packs.
    pub fn knowledge_id_exists(&self, id: &str) -> bool {
        self.rule_ids.contains(id)
            || self.triage_ids.contains(id)
            || self.condition_ids.contains(id)
            || self.intervention_ids.contains(id)
            || self.causal_node_ids.contains(id)
            || self.response_ids.contains(id)
            || self.test_ids.contains(id)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn domain_toml_files(root: &Path, domain: &str) -> Vec<PathBuf> {
    let dir = root.join(domain);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    paths
}

struct ParsedFile {
    rel_path: String,
    value: toml::Value,
}

/// Load, hash, parse and cross-validate every pack under `root`.
pub fn load_pack_index(root: &Path) -> PackIndex {
    let today = today_iso();
    let mut index = PackIndex {
        today: today.clone(),
        ..PackIndex::default()
    };

    // Pass 1: read, hash, parse. Parse failures exclude the file.
    let mut parsed: BTreeMap<&str, Vec<ParsedFile>> = BTreeMap::new();
    for domain in DOMAINS {
        for path in domain_toml_files(root, domain) {
            let rel_path = path
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string());
            let raw = match std::fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(e) => {
                    index.excluded.push(ExcludedFile {
                        rel_path,
                        reason: format!("unreadable: {e}"),
                    });
                    continue;
                }
            };
            match toml::from_str::<toml::Value>(&raw) {
                Ok(value) => {
                    index.files.insert(rel_path.clone(), sha256_hex(raw.as_bytes()));
                    parsed.entry(domain).or_default().push(ParsedFile { rel_path, value });
                }
                Err(e) => {
                    index.excluded.push(ExcludedFile {
                        rel_path,
                        reason: format!("toml parse error: {e}"),
                    });
                }
            }
        }
    }

    // Pass 2: collect ids from admitted files.
    for file in parsed.get("analytes").into_iter().flatten() {
        collect_ids(&file.value, "analyte", "id", &mut index.analyte_ids);
    }
    for file in parsed.get("rules").into_iter().flatten() {
        collect_ids(&file.value, "rule", "id", &mut index.rule_ids);
    }
    for file in parsed.get("triage").into_iter().flatten() {
        collect_ids(&file.value, "triage", "id", &mut index.triage_ids);
    }
    for file in parsed.get("conditions").into_iter().flatten() {
        collect_ids(&file.value, "condition", "id", &mut index.condition_ids);
    }
    for file in parsed.get("interventions").into_iter().flatten() {
        collect_ids(&file.value, "intervention", "id", &mut index.intervention_ids);
    }
    for file in parsed.get("causal").into_iter().flatten() {
        collect_ids(&file.value, "node", "id", &mut index.causal_node_ids);
    }
    for file in parsed.get("treatment-responses").into_iter().flatten() {
        collect_ids(&file.value, "response", "id", &mut index.response_ids);
    }
    for file in parsed.get("tests").into_iter().flatten() {
        collect_ids(&file.value, "test", "id", &mut index.test_ids);
    }

    // Pass 3: cross-domain referential integrity. A file with a dangling
    // reference is excluded whole, loudly — with the offending file named.
    let mut offenders: Vec<(String, String)> = Vec::new(); // (rel_path, reason)
    for file in parsed.get("ranges").into_iter().flatten() {
        if let Some(entries) = file.value.get("range").and_then(|v| v.as_array()) {
            for entry in entries {
                match entry.get("analyte").and_then(|v| v.as_str()) {
                    Some(analyte) => {
                        if !index.analyte_ids.contains(analyte) {
                            offenders.push((
                                file.rel_path.clone(),
                                format!("range references unknown analyte '{analyte}'"),
                            ));
                            break;
                        }
                    }
                    None => {
                        offenders.push((
                            file.rel_path.clone(),
                            "range entry missing 'analyte' field".into(),
                        ));
                        break;
                    }
                }
            }
        }
    }
    for file in parsed.get("rules").into_iter().flatten() {
        if let Some(entries) = file.value.get("rule").and_then(|v| v.as_array()) {
            for entry in entries {
                let rule_id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("<missing id>");
                let conds = entry
                    .get("conditions")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if conds.is_empty() {
                    offenders.push((
                        file.rel_path.clone(),
                        format!("rule '{rule_id}' has no conditions"),
                    ));
                }
                for cond in &conds {
                    if let Some(a) = cond.get("analyte").and_then(|v| v.as_str()) {
                        if !index.analyte_ids.contains(a) {
                            offenders.push((
                                file.rel_path.clone(),
                                format!(
                                    "rule '{rule_id}' condition references unknown analyte '{a}'"
                                ),
                            ));
                        }
                    }
                }
                // suggests may name tests that do not exist in the registry
                // yet: a visible gap, not a broken file.
                for s in entry
                    .get("suggests")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    if !index.analyte_ids.contains(s) && !DERIVED_IDS.contains(&s) {
                        index.gaps.push(KnowledgeGap {
                            rel_path: file.rel_path.clone(),
                            kind: "suggested_analyte".into(),
                            id: s.to_string(),
                        });
                    }
                }
            }
        }
    }
    for file in parsed.get("conditions").into_iter().flatten() {
        if let Some(entries) = file.value.get("condition").and_then(|v| v.as_array()) {
            for entry in entries {
                for biomarker in entry
                    .get("biomarkers")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    if !index.analyte_ids.contains(biomarker) && !DERIVED_IDS.contains(&biomarker)
                    {
                        // Same semantics: the card names a probe nobody
                        // defined yet. Visible gap, not exclusion.
                        index.gaps.push(KnowledgeGap {
                            rel_path: file.rel_path.clone(),
                            kind: "listed_biomarker".into(),
                            id: biomarker.to_string(),
                        });
                    }
                }
            }
        }
    }
    for file in parsed.get("interventions").into_iter().flatten() {
        if let Some(entries) = file.value.get("intervention").and_then(|v| v.as_array()) {
            for entry in entries {
                let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("<missing id>");
                for target in entry
                    .get("targets")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    if !index.condition_ids.contains(target) {
                        offenders.push((
                            file.rel_path.clone(),
                            format!("intervention '{id}' targets unknown condition '{target}'"),
                        ));
                    }
                }
            }
        }
    }
    // Causal packs: duplicate node ids, dangling edges, cycles, dead
    // condition links are file-level failures; undefined probe analytes are
    // visible gaps (forward references, same semantics as suggests).
    for file in parsed.get("causal").into_iter().flatten() {
        let nodes = file
            .value
            .get("node")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut file_node_ids: BTreeSet<&str> = BTreeSet::new();
        for node in &nodes {
            let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("<missing id>");
            if !file_node_ids.insert(id) {
                offenders.push((
                    file.rel_path.clone(),
                    format!("duplicate causal node id '{id}'"),
                ));
            }
            if let Some(condition) = node.get("condition").and_then(|v| v.as_str()) {
                if !index.condition_ids.contains(condition) {
                    offenders.push((
                        file.rel_path.clone(),
                        format!("causal node '{id}' belongs to unknown condition '{condition}'"),
                    ));
                }
            }
            for probe in node
                .get("probes")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(analyte) = probe.get("analyte").and_then(|v| v.as_str()) {
                    if !index.analyte_ids.contains(analyte) && !DERIVED_IDS.contains(&analyte) {
                        index.gaps.push(KnowledgeGap {
                            rel_path: file.rel_path.clone(),
                            kind: "causal_probe".into(),
                            id: analyte.to_string(),
                        });
                    }
                }
            }
        }
        let mut edge_pairs: Vec<(String, String)> = Vec::new();
        for edge in file
            .value
            .get("edge")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let from = edge.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let to = edge.get("to").and_then(|v| v.as_str()).unwrap_or("");
            if !index.causal_node_ids.contains(from) || !index.causal_node_ids.contains(to) {
                offenders.push((
                    file.rel_path.clone(),
                    format!("causal edge '{from}' -> '{to}' references unknown node"),
                ));
            } else {
                edge_pairs.push((from.to_string(), to.to_string()));
            }
        }
        if let Err(cycle) = assert_acyclic(&edge_pairs) {
            offenders.push((file.rel_path.clone(), format!("causal pack not a DAG: {cycle}")));
        }
    }
    // Apply exclusions: drop file + hash, record reason, and recompute every
    // id set from the surviving files so no dangling id survives the gate.
    if !offenders.is_empty() {
        let bad_files: BTreeSet<String> = offenders.iter().map(|(p, _)| p.clone()).collect();
        for (rel_path, reason) in offenders {
            index.excluded.push(ExcludedFile { rel_path, reason });
        }
        for rel in &bad_files {
            index.files.remove(rel);
        }
        index.analyte_ids.clear();
        index.rule_ids.clear();
        index.triage_ids.clear();
        index.condition_ids.clear();
        index.intervention_ids.clear();
        for file in parsed.get("analytes").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "analyte", "id", &mut index.analyte_ids);
            }
        }
        for file in parsed.get("rules").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "rule", "id", &mut index.rule_ids);
            }
        }
        for file in parsed.get("triage").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "triage", "id", &mut index.triage_ids);
            }
        }
        for file in parsed.get("conditions").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "condition", "id", &mut index.condition_ids);
            }
        }
        for file in parsed.get("interventions").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "intervention", "id", &mut index.intervention_ids);
            }
        }
        for file in parsed.get("causal").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "node", "id", &mut index.causal_node_ids);
            }
        }
        for file in parsed.get("treatment-responses").into_iter().flatten() {
            if !bad_files.contains(&file.rel_path) {
                collect_ids(&file.value, "response", "id", &mut index.response_ids);
            }
        }
    }
    // Pass 4: treatment responses and investigation tests reference causal
    // nodes; validated against the final (post-exclusion) node set so
    // cascades cannot leave dead references behind.
    let mut cascading_offenders: Vec<(String, String)> = Vec::new();
    for file in parsed.get("treatment-responses").into_iter().flatten() {
        if !index.files.contains_key(&file.rel_path) {
            continue;
        }
        for response in file
            .value
            .get("response")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let id = response.get("id").and_then(|v| v.as_str()).unwrap_or("<missing id>");
            for effect in response
                .get("effects")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(node) = effect.get("node").and_then(|v| v.as_str()) {
                    if !index.causal_node_ids.contains(node) {
                        cascading_offenders.push((
                            file.rel_path.clone(),
                            format!("response '{id}' effects unknown causal node '{node}'"),
                        ));
                    }
                }
            }
        }
    }
    for file in parsed.get("tests").into_iter().flatten() {
        if !index.files.contains_key(&file.rel_path) {
            continue;
        }
        for test in file
            .value
            .get("test")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let id = test.get("id").and_then(|v| v.as_str()).unwrap_or("<missing id>");
            for d in test
                .get("discriminates")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some(node) = d.get("node").and_then(|v| v.as_str()) {
                    if !index.causal_node_ids.contains(node) {
                        cascading_offenders.push((
                            file.rel_path.clone(),
                            format!("test '{id}' discriminates unknown causal node '{node}'"),
                        ));
                    }
                }
            }
        }
    }
    if !cascading_offenders.is_empty() {
        let bad: BTreeSet<String> = cascading_offenders
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        for (rel_path, reason) in cascading_offenders {
            index.excluded.push(ExcludedFile { rel_path, reason });
        }
        for rel in &bad {
            index.files.remove(rel);
        }
        index.response_ids.clear();
        index.test_ids.clear();
        for file in parsed.get("treatment-responses").into_iter().flatten() {
            if index.files.contains_key(&file.rel_path) {
                collect_ids(&file.value, "response", "id", &mut index.response_ids);
            }
        }
        for file in parsed.get("tests").into_iter().flatten() {
            if index.files.contains_key(&file.rel_path) {
                collect_ids(&file.value, "test", "id", &mut index.test_ids);
            }
        }
    }
    index
}

fn collect_ids(table: &toml::Value, array_key: &str, id_key: &str, out: &mut BTreeSet<String>) {
    if let Some(entries) = table.get(array_key).and_then(|v| v.as_array()) {
        for entry in entries {
            if let Some(id) = entry.get(id_key).and_then(|v| v.as_str()) {
                out.insert(id.to_string());
            }
        }
    }
}

/// The sorted, gate-admitted `.toml` paths for a domain (excluded files
/// removed), for the domain loaders.
pub fn allowed_files(root: &Path, domain: &str, index: &PackIndex) -> Vec<PathBuf> {
    domain_toml_files(root, domain)
        .into_iter()
        .filter(|p| {
            let rel = p
                .strip_prefix(root)
                .map(|x| x.display().to_string())
                .unwrap_or_default();
            index.files.contains_key(&rel)
        })
        .collect()
}

/// Generic DAG check over (from, to) pairs. Used by the Phase 1 causal pack
/// loader; shipped and tested now so the gate precedes the knowledge.
pub fn assert_acyclic(edges: &[(String, String)]) -> Result<(), String> {
    use std::collections::BTreeMap as Map;
    let mut adjacency: Map<&str, Vec<&str>> = Map::new();
    for (a, b) in edges {
        adjacency.entry(a.as_str()).or_default().push(b.as_str());
        adjacency.entry(b.as_str()).or_default();
    }
    // Three-color marking, iterative DFS. 0 = unvisited, 1 = on stack, 2 = done.
    let mut state: Map<&str, u8> = adjacency.keys().map(|n| (*n, 0)).collect();
    for start in adjacency.keys().copied().collect::<Vec<_>>() {
        if state[start] != 0 {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        state.insert(start, 1);
        while let Some((node, next)) = stack.pop() {
            let neighbors = &adjacency[node];
            if next >= neighbors.len() {
                state.insert(node, 2);
                continue;
            }
            // Re-push the node with the advanced cursor, then descend.
            stack.push((node, next + 1));
            let target = neighbors[next];
            match state[target] {
                1 => {
                    return Err(format!(
                        "cycle detected: edge '{node}' -> '{target}' closes a loop"
                    ));
                }
                0 => {
                    state.insert(target, 1);
                    stack.push((target, 0));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("asclepius-packs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_and_ids_collected() {
        let root = tmp_dir("ok");
        write(
            &root,
            "analytes/core.toml",
            r#"[[analyte]]
id = "tsh"
name = "TSH"
canonical_unit = "mIU/L"
plausible_min = 0.01
plausible_max = 100.0
"#,
        );
        write(
            &root,
            "rules/core.toml",
            r#"[[rule]]
id = "tsh_high"
description = "tsh above functional range"
severity = "flag"
conditions = [{ analyte = "tsh", kind = "functional", status = "above" }]
source = "seed"
"#,
        );
        let index = load_pack_index(&root);
        assert_eq!(index.files.len(), 2);
        assert!(index.files["analytes/core.toml"].starts_with("sha256:"));
        assert!(index.knowledge_id_exists("tsh_high"));
        assert!(!index.knowledge_id_exists("nope"));
        assert!(index.pack_ref_exists("pack:rules"));
        assert!(index.pack_ref_exists("pack:rules/core.toml"));
        assert!(index.pack_ref_exists("pack:rules/core.toml#anchor"));
        assert!(!index.pack_ref_exists("pack:missing-domain"));
        assert!(!index.pack_ref_exists("pack:rules/other.toml"));
    }

    #[test]
    fn malformed_toml_excluded_file_kept_others() {
        let root = tmp_dir("malformed");
        write(
            &root,
            "analytes/core.toml",
            r#"[[analyte]]
id = "tsh"
name = "TSH"
canonical_unit = "mIU/L"
plausible_min = 0.01
plausible_max = 100.0
"#,
        );
        write(&root, "rules/broken.toml", "not [ valid toml");
        write(
            &root,
            "rules/core.toml",
            r#"[[rule]]
id = "tsh_high"
description = "d"
severity = "flag"
conditions = [{ analyte = "tsh", kind = "functional", status = "above" }]
source = "seed"
"#,
        );
        let index = load_pack_index(&root);
        assert_eq!(index.excluded.len(), 1);
        assert!(index.excluded[0].rel_path.ends_with("rules/broken.toml"));
        assert!(index.knowledge_id_exists("tsh_high"));
        assert!(!index.pack_ref_exists("pack:rules/broken.toml"));
        // The loader filter must drop the excluded file.
        let allowed = allowed_files(&root, "rules", &index);
        assert_eq!(allowed.len(), 1);
        assert!(allowed[0].ends_with("core.toml"));
    }

    #[test]
    fn dangling_reference_excludes_offending_file_only() {
        let root = tmp_dir("dangling");
        write(
            &root,
            "analytes/core.toml",
            r#"[[analyte]]
id = "tsh"
name = "TSH"
canonical_unit = "mIU/L"
plausible_min = 0.01
plausible_max = 100.0
"#,
        );
        write(
            &root,
            "ranges/good.toml",
            r#"[[range]]
analyte = "tsh"
kind = "population"
low = 0.4
high = 4.5
unit = "mIU/L"
source = "seed"
"#,
        );
        write(
            &root,
            "ranges/bad.toml",
            r#"[[range]]
analyte = "ghost_analyte"
kind = "population"
low = 1.0
high = 2.0
unit = "mIU/L"
source = "seed"
"#,
        );
        let index = load_pack_index(&root);
        assert_eq!(index.excluded.len(), 1);
        assert!(index.excluded[0].rel_path.ends_with("ranges/bad.toml"));
        assert!(index.pack_ref_exists("pack:ranges/good.toml"));
        assert!(!index.pack_ref_exists("pack:ranges/bad.toml"));
    }

    #[test]
    fn forward_references_become_gaps_not_exclusions() {
        let root = tmp_dir("gaps");
        write(
            &root,
            "analytes/core.toml",
            r#"[[analyte]]
id = "tsh"
name = "TSH"
canonical_unit = "mIU/L"
plausible_min = 0.01
plausible_max = 100.0
"#,
        );
        write(
            &root,
            "rules/thyroid.toml",
            r#"[[rule]]
id = "tsh_high"
description = "tsh above functional range"
severity = "flag"
conditions = [{ analyte = "tsh", kind = "functional", status = "above" }]
suggests = ["free_t3", "reverse_t3"]
source = "seed"
"#,
        );
        write(
            &root,
            "conditions/thyroid.toml",
            r#"[[condition]]
id = "thyroid_patterns"
name = "Thyroid"
summary = "s"
biomarkers = ["tsh", "free_t3", "tpo_antibodies"]
"#,
        );
        let index = load_pack_index(&root);
        assert!(index.excluded.is_empty(), "{:?}", index.excluded);
        // File and rule survive; forward refs are surfaced as gaps.
        assert!(index.pack_ref_exists("pack:rules/thyroid.toml"));
        assert!(index.knowledge_id_exists("tsh_high"));
        let gap_ids: Vec<&str> = index.gaps().iter().map(|g| g.id.as_str()).collect();
        assert!(gap_ids.contains(&"free_t3"));
        assert!(gap_ids.contains(&"reverse_t3"));
        assert!(gap_ids.contains(&"tpo_antibodies"));
        // 'tsh' is defined: not a gap.
        assert!(!gap_ids.contains(&"tsh"));
    }

    #[test]
    fn acyclic_graphs_pass_cycles_fail() {
        assert!(assert_acyclic(&[("a".into(), "b".into()), ("b".into(), "c".into())]).is_ok());
        assert!(assert_acyclic(&[]).is_ok());
        let err = assert_acyclic(&[
            ("a".into(), "b".into()),
            ("b".into(), "c".into()),
            ("c".into(), "a".into()),
        ]);
        assert!(err.is_err());
        let msg = err.unwrap_err();
        assert!(msg.contains("cycle"), "message was: {msg}");
        // Self-loop is a cycle.
        assert!(assert_acyclic(&[("a".into(), "a".into())]).is_err());
    }
}
