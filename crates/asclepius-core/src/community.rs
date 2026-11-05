//! Community signals: structured patient-community outcome reports.
//!
//! For conditions with almost no formal trial data (PSSD/PAWS), the
//! community's accumulated helped/hurt experience is real information —
//! tier-D, approximate, and better structured than buried in prose.
//! A signal names an item, a direction, an approximate report count, and a
//! review date. Harm signals at or above the hurt threshold screen items
//! out of proposals (listed, never hidden); helped signals flag community
//! support WITHOUT inflating the item's evidence tier.

use crate::error::AsclepiusError;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Report count at or above which a hurt signal excludes an item from
/// proposals. Below the threshold the signal is advisory only.
pub const HURT_THRESHOLD: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunitySignal {
    pub id: String,
    /// Intervention id this signal is about (gate-validated).
    pub item_id: String,
    /// "hurt" | "helped"
    pub direction: String,
    /// Approximate report count as of the review date — a curatorial claim,
    /// not a measurement.
    pub reports: u32,
    pub summary: String,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SignalFile {
    signal: Vec<CommunitySignal>,
}

#[derive(Debug, Clone, Default)]
pub struct CommunitySignals {
    signals: Vec<CommunitySignal>,
}

impl CommunitySignals {
    pub fn from_vec(signals: Vec<CommunitySignal>) -> Self {
        Self { signals }
    }

    pub fn load_paths(paths: &[std::path::PathBuf]) -> Result<Self, AsclepiusError> {
        if paths.is_empty() {
            return Ok(Self::default());
        }
        let mut signals = Vec::new();
        for path in paths {
            let raw = std::fs::read_to_string(path)?;
            let parsed: SignalFile =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            signals.extend(parsed.signal);
        }
        Ok(Self { signals })
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

    pub fn all(&self) -> &[CommunitySignal] {
        &self.signals
    }

    pub fn for_item(&self, item_id: &str) -> Vec<&CommunitySignal> {
        self.signals.iter().filter(|s| s.item_id == item_id).collect()
    }

    /// True when a hurt signal at/above threshold screens this item out.
    pub fn screens_out(&self, item_id: &str) -> Option<&CommunitySignal> {
        self.signals
            .iter()
            .find(|s| s.item_id == item_id && s.direction == "hurt" && s.reports >= HURT_THRESHOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(id: &str, item: &str, direction: &str, reports: u32) -> CommunitySignal {
        CommunitySignal {
            id: id.into(),
            item_id: item.into(),
            direction: direction.into(),
            reports,
            summary: String::new(),
            tier: "D".into(),
            sources: vec![],
            review_by: None,
        }
    }

    #[test]
    fn hurt_threshold_screens_and_helped_flags() {
        let set = CommunitySignals::from_vec(vec![
            signal("s1", "berberine", "hurt", 30),
            signal("s2", "b_complex", "hurt", 8), // below threshold: advisory
            signal("s3", "saffron", "helped", 25),
        ]);
        assert!(set.screens_out("berberine").is_some());
        assert!(set.screens_out("b_complex").is_none(), "below threshold stays advisory");
        assert!(set.screens_out("saffron").is_none(), "helped never screens");
        assert_eq!(set.for_item("saffron").len(), 1);
    }
}
