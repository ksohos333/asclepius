use crate::conditions::ConditionLibrary;
use crate::error::AsclepiusError;
use crate::interventions::InterventionLibrary;
use crate::range::Confidence;
use crate::rules::RuleSet;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct LocalHit {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub detail: String,
    pub sources: Vec<String>,
    pub confidence: Confidence,
    pub score: u32,
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect()
}

pub fn local_search(
    conditions: &ConditionLibrary,
    rules: &RuleSet,
    interventions: &InterventionLibrary,
    query: &str,
    limit: usize,
) -> Vec<LocalHit> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<LocalHit> = Vec::new();
    for card in conditions.all() {
        let name = card.name.to_lowercase();
        let mut score = 0u32;
        for token in &tokens {
            if card.aliases.iter().any(|a| a.to_lowercase() == *token) || name == *token {
                score += 10;
            } else if name.contains(token.as_str())
                || card
                    .aliases
                    .iter()
                    .any(|a| a.to_lowercase().contains(token.as_str()))
            {
                score += 6;
            }
            if card
                .biomarkers
                .iter()
                .any(|b| b.to_lowercase().contains(token.as_str()))
            {
                score += 3;
            }
            if card
                .mechanisms
                .iter()
                .any(|m| m.to_lowercase().contains(token.as_str()))
            {
                score += 3;
            }
            if card.summary.to_lowercase().contains(token.as_str()) {
                score += 1;
            }
        }
        if score > 0 {
            hits.push(LocalHit {
                kind: "condition".into(),
                id: card.id.clone(),
                title: card.name.clone(),
                detail: card.summary.clone(),
                sources: card.sources.clone(),
                confidence: card.confidence,
                score,
            });
        }
    }
    for rule in rules.all() {
        let id = rule.id.to_lowercase();
        let desc = rule.description.to_lowercase();
        let mut score = 0u32;
        for token in &tokens {
            if id.contains(token.as_str()) {
                score += 8;
            }
            if desc.contains(token.as_str()) {
                score += 3;
            }
            if rule
                .suggests
                .iter()
                .any(|s| s.to_lowercase().contains(token.as_str()))
            {
                score += 2;
            }
        }
        if score > 0 {
            hits.push(LocalHit {
                kind: "rule".into(),
                id: rule.id.clone(),
                title: rule.description.clone(),
                detail: format!("severity: {:?}", rule.severity),
                sources: vec![rule.source.clone()],
                confidence: rule.confidence,
                score,
            });
        }
    }
    for card in interventions.all() {
        let name = card.name.to_lowercase();
        let mut score = 0u32;
        for token in &tokens {
            if card.id.to_lowercase() == *token || name == *token {
                score += 10;
            } else if name.contains(token.as_str()) {
                score += 6;
            }
            if card.summary.to_lowercase().contains(token.as_str()) {
                score += 1;
            }
            if card
                .sources
                .iter()
                .any(|s| s.to_lowercase().contains(token.as_str()))
            {
                score += 2;
            }
        }
        if score > 0 {
            hits.push(LocalHit {
                kind: "intervention".into(),
                id: card.id.clone(),
                title: card.name.clone(),
                detail: card.summary.clone(),
                sources: card.sources.clone(),
                confidence: card.confidence,
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    hits.truncate(limit);
    hits
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubMedArticle {
    pub pmid: String,
    pub title: String,
    pub abstract_text: String,
}

#[derive(Debug, Clone)]
pub struct PubMedClient {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub base: String,
}

impl Default for PubMedClient {
    fn default() -> Self {
        Self::from_env()
    }
}

impl PubMedClient {
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("ASCLEPIUS_PUBMED").ok().as_deref() == Some("1"),
            api_key: std::env::var("ASCLEPIUS_PUBMED_KEY").ok(),
            base: "https://eutils.ncbi.nlm.nih.gov/entrez/eutils".into(),
        }
    }

    pub fn search_pmids(&self, query: &str, max: usize) -> Result<Vec<String>, String> {
        let mut req = ureq::get(format!("{}/esearch.fcgi", self.base).as_str())
            .query("db", "pubmed")
            .query("retmode", "json")
            .query("retmax", max.to_string())
            .query("term", query);
        if let Some(key) = &self.api_key {
            req = req.query("api_key", key);
        }
        let mut resp = req
            .call()
            .map_err(|e| format!("pubmed esearch failed: {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("pubmed esearch returned {}", resp.status()));
        }
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("pubmed esearch body failed: {e}"))?;
        parse_esearch_ids(&text)
    }

    pub fn fetch_articles(&self, pmids: &[String]) -> Result<Vec<PubMedArticle>, String> {
        if pmids.is_empty() {
            return Ok(Vec::new());
        }
        let mut req = ureq::get(format!("{}/efetch.fcgi", self.base).as_str())
            .query("db", "pubmed")
            .query("retmode", "xml")
            .query("rettype", "abstract")
            .query("id", pmids.join(","));
        if let Some(key) = &self.api_key {
            req = req.query("api_key", key);
        }
        let mut resp = req
            .call()
            .map_err(|e| format!("pubmed efetch failed: {e}"))?;
        if resp.status().as_u16() != 200 {
            return Err(format!("pubmed efetch returned {}", resp.status()));
        }
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("pubmed efetch body failed: {e}"))?;
        Ok(parse_pubmed_xml(&text))
    }
}

pub fn parse_esearch_ids(json: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("esearch json invalid: {e}"))?;
    Ok(value
        .pointer("/esearchresult/idlist")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default())
}

fn all_text(node: roxmltree::Node<'_, '_>) -> String {
    let mut out = String::new();
    for descendant in node.descendants() {
        if descendant.is_text() {
            if let Some(t) = descendant.text() {
                out.push_str(t);
                out.push(' ');
            }
        }
    }
    out.trim().to_string()
}

pub fn parse_pubmed_xml(xml: &str) -> Vec<PubMedArticle> {
    let mut cleaned = std::borrow::Cow::Borrowed(xml);
    if let Some(start) = xml.find("<!DOCTYPE") {
        if let Some((_, rest)) = xml[start..].split_once('>') {
            cleaned = std::borrow::Cow::Owned(format!("{}{}", &xml[..start], rest));
        }
    }
    let Ok(doc) = roxmltree::Document::parse(&cleaned) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.has_tag_name("PubmedArticle"))
        .filter_map(|article| {
            let citation = article
                .descendants()
                .find(|n| n.has_tag_name("MedlineCitation"))?;
            let pmid = citation
                .children()
                .find(|n| n.has_tag_name("PMID"))
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())?;
            let title = article
                .descendants()
                .find(|n| n.has_tag_name("ArticleTitle"))
                .map(all_text)
                .unwrap_or_default();
            let abstract_text = article
                .descendants()
                .filter(|n| n.has_tag_name("AbstractText"))
                .map(all_text)
                .collect::<Vec<_>>()
                .join(" ");
            Some(PubMedArticle {
                pmid,
                title,
                abstract_text,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct LiteratureInput {
    pub claim: String,
    #[serde(default)]
    pub max_results: Option<usize>,
}

pub fn load_packs_error(path: &Path) -> AsclepiusError {
    AsclepiusError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("packs directory missing: {}", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esearch_fixture_parses() {
        let fixture = r#"{"esearchresult":{"count":"3","idlist":["111","222","333"]}}"#;
        assert_eq!(
            parse_esearch_ids(fixture).unwrap(),
            vec!["111", "222", "333"]
        );
        assert!(parse_esearch_ids("not json").is_err());
        assert!(
            parse_esearch_ids(r#"{"esearchresult":{}}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pubmed_xml_fixture_parses() {
        let fixture = r#"<?xml version="1.0"?>
        <!DOCTYPE PubmedArticleSet PUBLIC "-//NLM//DTD PubMedArticle, 1st January 2025//EN" "https://dtd.nlm.nih.gov/ncbi/pubmed/out/pubmed_250101.dtd">
        <PubmedArticleSet>
          <PubmedArticle>
            <MedlineCitation>
              <PMID>12345</PMID>
              <Article>
                <ArticleTitle>Zinc and acne: a trial</ArticleTitle>
                <Abstract>
                  <AbstractText>Objective: to test zinc.</AbstractText>
                  <AbstractText>Results were modest.</AbstractText>
                </Abstract>
              </Article>
            </MedlineCitation>
          </PubmedArticle>
          <PubmedArticle>
            <MedlineCitation>
              <PMID>67890</PMID>
              <Article>
                <ArticleTitle>Unrelated study</ArticleTitle>
              </Article>
            </MedlineCitation>
          </PubmedArticle>
        </PubmedArticleSet>"#;
        let articles = parse_pubmed_xml(fixture);
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].pmid, "12345");
        assert!(articles[0].title.contains("Zinc"));
        assert!(articles[0].abstract_text.contains("Objective"));
        assert!(articles[0].abstract_text.contains("Results"));
        assert_eq!(articles[1].abstract_text, "");
    }

    #[test]
    fn local_search_finds_acne_cards() {
        let conditions =
            crate::conditions::ConditionLibrary::from_vec(vec![crate::conditions::ConditionCard {
                id: "acne_root_causes".into(),
                name: "Cystic Acne Root Causes".into(),
                aliases: vec!["acne".into()],
                summary: "androgens and igf".into(),
                mechanisms: vec![],
                biomarkers: vec![],
                notes: None,
                sources: vec!["seed".into()],
                confidence: crate::range::Confidence::Low,
                review_by: None,
            }]);
        let hits = local_search(
            &conditions,
            &RuleSet::default(),
            &InterventionLibrary::from_vec(Vec::new()),
            "zinc for acne",
            5,
        );
        assert!(hits.iter().any(|h| h.id == "acne_root_causes"));
    }
}
