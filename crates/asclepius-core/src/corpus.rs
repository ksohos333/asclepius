//! The curated corpus: key papers as pack data.
//!
//! For conditions with a small, dense literature (PSSD/PAWS), the ~50 papers
//! that matter can live in the packs as tier-tagged, review-dated entries —
//! searched locally by `literature_probe` and `literature_verify`, no
//! network needed. Entries cite author/year and honest summaries; PMIDs are
//! included only when verified (unverified citations stay descriptive, to be
//! confirmed via literature_verify with PubMed enabled).

use crate::error::AsclepiusError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Option<String>,
    pub year: u32,
    #[serde(default)]
    pub pmid: Option<String>,
    /// Evidence tier of what this paper supports.
    pub tier: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub summary: String,
    /// Abstract excerpt or key-findings summary (kept short).
    #[serde(default)]
    pub abstract_excerpt: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CorpusFile {
    paper: Vec<Paper>,
}

#[derive(Debug, Clone, Default)]
pub struct Corpus {
    papers: Vec<Paper>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusHit {
    pub id: String,
    pub title: String,
    pub year: u32,
    pub tier: String,
    pub summary: String,
    pub pmid: Option<String>,
    pub score: u32,
}

impl Corpus {
    pub fn from_vec(papers: Vec<Paper>) -> Self {
        Self { papers }
    }

    pub fn load_paths(paths: &[std::path::PathBuf]) -> Result<Self, AsclepiusError> {
        if paths.is_empty() {
            return Ok(Self::default());
        }
        let mut papers = Vec::new();
        for path in paths {
            let raw = std::fs::read_to_string(path)?;
            let parsed: CorpusFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            papers.extend(parsed.paper);
        }
        Ok(Self { papers })
    }

    pub fn load_dir(dir: &Path) -> Result<Self, AsclepiusError> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        Self::load_paths(&paths)
    }

    pub fn all(&self) -> &[Paper] {
        &self.papers
    }

    pub fn get(&self, id: &str) -> Option<&Paper> {
        self.papers.iter().find(|p| p.id == id)
    }

    /// Token search over title, summary, excerpt, and tags — the same
    /// matching discipline as the rest of the engine.
    pub fn search(&self, query: &str, limit: usize) -> Vec<CorpusHit> {
        let tokens: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| t.len() > 1)
            .map(|t| t.to_string())
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<CorpusHit> = self
            .papers
            .iter()
            .filter_map(|p| {
                let title = p.title.to_lowercase();
                let summary = p.summary.to_lowercase();
                let excerpt = p
                    .abstract_excerpt
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase();
                let tags = p.tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>();
                let mut score = 0u32;
                for t in &tokens {
                    if title.contains(t.as_str()) {
                        score += 8;
                    }
                    if tags.iter().any(|tag| tag.contains(t.as_str())) {
                        score += 5;
                    }
                    if summary.contains(t.as_str()) {
                        score += 3;
                    }
                    if excerpt.contains(t.as_str()) {
                        score += 2;
                    }
                }
                (score > 0).then(|| CorpusHit {
                    id: p.id.clone(),
                    title: p.title.clone(),
                    year: p.year,
                    tier: p.tier.clone(),
                    summary: p.summary.clone(),
                    pmid: p.pmid.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, title: &str, tags: &[&str]) -> Paper {
        Paper {
            id: id.into(),
            title: title.into(),
            authors: None,
            year: 2019,
            pmid: None,
            tier: "C".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            summary: format!("summary of {title}"),
            abstract_excerpt: None,
            url: None,
            review_by: None,
        }
    }

    #[test]
    fn search_ranks_title_and_tags() {
        let corpus = Corpus::from_vec(vec![
            paper("hogan2019", "Persistent sexual dysfunction after antidepressants", &["pssd"]),
            paper("smith2007", "Glycemic load and acne", &["acne"]),
        ]);
        let hits = corpus.search("sexual dysfunction antidepressant pssd", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "hogan2019");
        assert!(hits[0].score >= 8);
        let none = corpus.search("quantum", 5);
        assert!(none.is_empty());
    }
}
