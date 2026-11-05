//! Region model and physician advocacy: how to actually get the test, which
//! doctor to see, and what to say.
//!
//! Region packs are layered data (`content-packs/regions/`):
//!
//!   _base.toml   (region-independent fallbacks: bloc-less notes)
//!   eu.toml      (bloc layer: EU-wide supplement/test regulation semantics)
//!   gr.toml      (country layer: EOPYY vs private labs, legal classes,
//!                 specialty routing, bilingual request scripts)
//!
//! Resolution is deepest-layer-wins, and every resolved entry records which
//! layer supplied it (`resolved_from`) — provenance applies to geography the
//! same way it applies to lab facts. Region data is never inferred: a missing
//! region or a missing route is an abstention, not a guess. Legal classes are
//! last-reviewed data, not legal advice.

use crate::error::AsclepiusError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionDef {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub bloc: Option<String>,
    pub name: String,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub kind: String,
    #[serde(default)]
    pub specialty: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub cost_estimate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRoute {
    pub test_id: String,
    #[serde(default)]
    pub requires_referral: bool,
    #[serde(default)]
    pub routes: Vec<Route>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegalClass {
    /// Intervention id or test id.
    pub item_id: String,
    /// otc | rx_only | controlled | unavailable | grey
    pub class: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecialtyEntry {
    pub specialty: String,
    /// When this specialty is the right door, in one sentence.
    pub when: String,
    #[serde(default)]
    pub script_el: Option<String>,
    #[serde(default)]
    pub script_en: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhysicianRoute {
    pub condition: String,
    pub rank: Vec<SpecialtyEntry>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RegionFileDef {
    region: Option<RegionDef>,
    #[serde(default)]
    test_route: Vec<TestRoute>,
    #[serde(default)]
    legal_class: Vec<LegalClass>,
    #[serde(default)]
    physician_route: Vec<PhysicianRoute>,
}

#[derive(Debug, Clone)]
struct RegionLayer {
    /// "_base", bloc code ("eu"), or country code ("gr") — lowercase.
    layer: String,
    def: RegionFileDef,
}

#[derive(Debug, Clone, Default)]
pub struct RegionSet {
    layers: Vec<RegionLayer>,
}

/// An entry plus the layer it was resolved from.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct PhysicianBriefInput {
    /// Condition card id the brief should route for (e.g. "acne_root_causes").
    pub condition: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resolved<T> {
    pub value: T,
    pub resolved_from: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedRegion {
    pub country: String,
    pub country_name: String,
    pub bloc: Option<String>,
    pub locale: Option<String>,
    /// Layers in application order (e.g. ["_base", "eu", "gr"]).
    pub layers: Vec<String>,
    test_routes: BTreeMap<String, Resolved<TestRoute>>,
    legal_classes: BTreeMap<String, Resolved<LegalClass>>,
    physician_routes: BTreeMap<String, Resolved<PhysicianRoute>>,
}

impl ResolvedRegion {
    pub fn test_route(&self, test_id: &str) -> Option<&Resolved<TestRoute>> {
        self.test_routes.get(test_id)
    }
    pub fn legal_class(&self, item_id: &str) -> Option<&Resolved<LegalClass>> {
        self.legal_classes.get(item_id)
    }
    pub fn physician_route(&self, condition: &str) -> Option<&Resolved<PhysicianRoute>> {
        self.physician_routes.get(condition)
    }
    pub fn test_route_count(&self) -> usize {
        self.test_routes.len()
    }
    pub fn legal_class_count(&self) -> usize {
        self.legal_classes.len()
    }
    pub fn physician_route_count(&self) -> usize {
        self.physician_routes.len()
    }
    /// Tests known to the engine that have no route in this region — the
    /// visible curation gap, never silently guessed.
    pub fn unrouted_tests(&self, known_test_ids: &[String]) -> Vec<String> {
        known_test_ids
            .iter()
            .filter(|t| !self.test_routes.contains_key(t.as_str()))
            .cloned()
            .collect()
    }
    /// Interventions with no legal class in this region.
    pub fn unclassed_items(&self, known_item_ids: &[String]) -> Vec<String> {
        known_item_ids
            .iter()
            .filter(|t| !self.legal_classes.contains_key(t.as_str()))
            .cloned()
            .collect()
    }
}

impl RegionSet {
    pub fn load_paths(paths: &[std::path::PathBuf]) -> Result<Self, AsclepiusError> {
        let mut layers = Vec::new();
        for path in paths {
            let raw = std::fs::read_to_string(path)?;
            let def: RegionFileDef =
                toml::from_str(&raw).map_err(|source| AsclepiusError::Toml {
                    file: path.display().to_string(),
                    source,
                })?;
            let Some(region) = &def.region else {
                return Err(AsclepiusError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("region file {} missing [region] header", path.display()),
                )));
            };
            let layer = match (&region.country, &region.bloc) {
                (Some(c), _) => c.to_lowercase(),
                (None, Some(b)) => b.to_lowercase(),
                (None, None) => "_base".into(),
            };
            layers.push(RegionLayer { layer, def });
        }
        // Deterministic application order: _base, then bloc layers, then
        // country layers (2-letter codes).
        layers.sort_by_key(|l| match l.layer.as_str() {
            "_base" => 0,
            n if n.len() != 2 => 1,
            _ => 2,
        });
        Ok(Self { layers })
    }

    pub fn load_dir(dir: &std::path::Path) -> Result<Self, AsclepiusError> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        Self::load_paths(&paths)
    }

    /// Countries with a dedicated layer file.
    pub fn countries(&self) -> Vec<&str> {
        self.layers
            .iter()
            .filter(|l| l.layer.len() == 2 && l.layer != "_base")
            .map(|l| l.layer.as_str())
            .collect()
    }

    /// Resolve the layered region for a country code (case-insensitive).
    /// Missing country data is None — the caller abstains.
    pub fn resolve(&self, country: &str) -> Option<ResolvedRegion> {
        let cc = country.to_lowercase();
        let country_layer = self
            .layers
            .iter()
            .find(|l| l.layer == cc && l.def.region.as_ref().is_some_and(|r| r.country.is_some()))?;
        let region = country_layer.def.region.as_ref().expect("checked above");
        let bloc = region.bloc.as_ref().map(|b| b.to_lowercase());
        let bloc_layer = bloc
            .as_ref()
            .and_then(|b| self.layers.iter().find(|l| l.layer == *b));
        let base_layer = self.layers.iter().find(|l| l.layer == "_base");

        let mut test_routes: BTreeMap<String, Resolved<TestRoute>> = BTreeMap::new();
        let mut legal_classes: BTreeMap<String, Resolved<LegalClass>> = BTreeMap::new();
        let mut physician_routes: BTreeMap<String, Resolved<PhysicianRoute>> = BTreeMap::new();
        let mut layers_applied: Vec<String> = Vec::new();

        for layer in [base_layer, bloc_layer, Some(country_layer)].into_iter().flatten() {
            layers_applied.push(layer.layer.clone());
            for tr in &layer.def.test_route {
                test_routes.insert(
                    tr.test_id.clone(),
                    Resolved { value: tr.clone(), resolved_from: layer.layer.clone() },
                );
            }
            for lc in &layer.def.legal_class {
                legal_classes.insert(
                    lc.item_id.clone(),
                    Resolved { value: lc.clone(), resolved_from: layer.layer.clone() },
                );
            }
            for pr in &layer.def.physician_route {
                physician_routes.insert(
                    pr.condition.clone(),
                    Resolved { value: pr.clone(), resolved_from: layer.layer.clone() },
                );
            }
        }

        Some(ResolvedRegion {
            country: cc.to_uppercase(),
            country_name: region.name.clone(),
            bloc: bloc.map(|b| b.to_uppercase()),
            locale: region.locale.clone(),
            layers: layers_applied,
            test_routes,
            legal_classes,
            physician_routes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(layer: &str, def: RegionFileDef) -> RegionLayer {
        RegionLayer { layer: layer.into(), def }
    }

    fn base_and_gr() -> RegionSet {
        RegionSet {
            layers: vec![
                layer(
                    "_base",
                    RegionFileDef {
                        region: Some(RegionDef {
                            country: None,
                            bloc: None,
                            name: "Base layer".into(),
                            locale: None,
                        }),
                        test_route: vec![TestRoute {
                            test_id: "generic_blood".into(),
                            requires_referral: false,
                            routes: vec![Route { kind: "any_lab".into(), specialty: None, note: None, cost_estimate: None }],
                            notes: None,
                            review_by: None,
                        }],
                        legal_class: vec![LegalClass {
                            item_id: "zinc_picolinate".into(),
                            class: "otc".into(),
                            note: None,
                            review_by: None,
                        }],
                        physician_route: vec![],
                    },
                ),
                layer(
                    "gr",
                    RegionFileDef {
                        region: Some(RegionDef {
                            country: Some("GR".into()),
                            bloc: Some("EU".into()),
                            name: "Greece".into(),
                            locale: Some("el".into()),
                        }),
                        test_route: vec![TestRoute {
                            test_id: "generic_blood".into(),
                            requires_referral: true,
                            routes: vec![],
                            notes: Some("country overrides base".into()),
                            review_by: None,
                        }],
                        legal_class: vec![],
                        physician_route: vec![PhysicianRoute {
                            condition: "acne".into(),
                            rank: vec![SpecialtyEntry {
                                specialty: "gastroenterology".into(),
                                when: "gut symptoms or SIBO suspicion".into(),
                                script_el: Some("el".into()),
                                script_en: Some("en".into()),
                            }],
                            note: None,
                            review_by: None,
                        }],
                    },
                ),
            ],
        }
    }

    #[test]
    fn deepest_layer_wins_with_provenance() {
        let set = base_and_gr();
        let gr = set.resolve("gr").unwrap();
        assert_eq!(gr.country, "GR");
        assert_eq!(gr.bloc.as_deref(), Some("EU"));
        assert_eq!(gr.layers, vec!["_base", "gr"]);
        let route = gr.test_route("generic_blood").unwrap();
        assert_eq!(route.resolved_from, "gr", "country overrides base");
        assert!(route.value.requires_referral);
        // legal class inherited from base layer keeps its provenance
        let lc = gr.legal_class("zinc_picolinate").unwrap();
        assert_eq!(lc.resolved_from, "_base");
        assert_eq!(lc.value.class, "otc");
    }

    #[test]
    fn unknown_country_resolves_none() {
        let set = base_and_gr();
        assert!(set.resolve("ru").is_none());
        assert!(set.countries().contains(&"gr"));
    }

    #[test]
    fn unrouted_and_unclassed_surfaced() {
        let gr = base_and_gr().resolve("GR").unwrap();
        let unrouted = gr.unrouted_tests(&["generic_blood".into(), "sibo_lactulose_breath".into()]);
        assert_eq!(unrouted, vec!["sibo_lactulose_breath".to_string()]);
        let unclassed = gr.unclassed_items(&["zinc_picolinate".into(), "testosterone_therapy".into()]);
        assert_eq!(unclassed, vec!["testosterone_therapy".to_string()]);
    }
}
