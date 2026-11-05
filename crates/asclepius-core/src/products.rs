//! Product grounding: mapping intervention rungs to purchasable, branded
//! products available in the patient's region.
//!
//! Products are region-pack data (`[[product]]` inside `regions/<cc>.toml`):
//! availability is inherently regional, so the same brand with a different
//! formulation in another country is a separate entry linked by `shared_brand`
//! — one entry per region, never smeared together. A product cites the
//! intervention ids it implements (`maps_to`); `disclosure` states honestly
//! whether the label meets the quality markers the intervention card names
//! (for probiotics: strain identities and CFU counts).
//!
//! Tier honesty: a product tier reflects the evidence for its *formulation
//! class*, and label-sourced products say so in `sources` — user-supplied
//! label data is provenance, not literature.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductCard {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: Option<String>,
    /// Intervention ids this product implements (gate-validated).
    pub maps_to: Vec<String>,
    /// What is actually in it, per the label.
    pub format: String,
    /// Honest statement on label disclosure vs the quality markers the
    /// intervention card names (strains, CFU, etc.).
    #[serde(default)]
    pub disclosure: Option<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub cost_estimate: Option<String>,
    /// Where it is sold in this region.
    #[serde(default)]
    pub availability: Option<String>,
    /// Cross-region link: same brand, different formulation elsewhere.
    #[serde(default)]
    pub shared_brand: Option<String>,
    pub tier: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub review_by: Option<String>,
}

/// A product attached to a protocol rung, with the layer it came from.
#[derive(Debug, Clone, Serialize)]
pub struct ProductAttachment {
    pub intervention_id: String,
    #[serde(flatten)]
    pub product: ProductCard,
    pub resolved_from: String,
}

/// Attach region products to protocol items: for every intervention in the
/// proposal, find products (from the resolved region) whose `maps_to`
/// contains it. Items without products are returned as unmatched ids so the
/// caller can state the gap instead of hiding it.
pub fn attach_products(
    intervention_ids: &[String],
    region_products: &dyn Fn(&str) -> Vec<(ProductCard, String)>,
) -> (Vec<ProductAttachment>, Vec<String>) {
    let mut attachments = Vec::new();
    let mut unmatched = Vec::new();
    for intervention_id in intervention_ids {
        let matches = region_products(intervention_id);
        if matches.is_empty() {
            unmatched.push(intervention_id.clone());
        }
        for (product, layer) in matches {
            attachments.push(ProductAttachment {
                intervention_id: intervention_id.clone(),
                product,
                resolved_from: layer,
            });
        }
    }
    (attachments, unmatched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(id: &str, maps_to: &[&str]) -> ProductCard {
        ProductCard {
            id: id.into(),
            name: id.to_string(),
            vendor: None,
            maps_to: maps_to.iter().map(|s| s.to_string()).collect(),
            format: "label".into(),
            disclosure: None,
            caveats: vec![],
            cost_estimate: None,
            availability: None,
            shared_brand: None,
            tier: "C".into(),
            sources: vec![],
            review_by: None,
        }
    }

    #[test]
    fn products_attach_by_maps_to_and_unmatched_surfaced() {
        let eftonia = product("eftonia", &["multispecies_probiotic_skin"]);
        let catalog = vec![eftonia.clone()];
        let lookup = |intervention: &str| {
            catalog
                .iter()
                .filter(|p| p.maps_to.iter().any(|m| m == intervention))
                .map(|p| (p.clone(), "gr".to_string()))
                .collect::<Vec<_>>()
        };
        let (attached, unmatched) = attach_products(
            &[
                "multispecies_probiotic_skin".to_string(),
                "zinc_picolinate".to_string(),
            ],
            &lookup,
        );
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].product.id, "eftonia");
        assert_eq!(attached[0].resolved_from, "gr");
        assert_eq!(attached[0].intervention_id, "multispecies_probiotic_skin");
        assert_eq!(unmatched, vec!["zinc_picolinate".to_string()]);
    }

    #[test]
    fn multi_intervention_products_attach_to_each() {
        // A synbiotic+Zn+D blend implements three rungs at once.
        let blend = product("blend", &["multispecies_probiotic_skin", "zinc_picolinate"]);
        let catalog = vec![blend];
        let lookup = |i: &str| {
            catalog
                .iter()
                .filter(|p| p.maps_to.iter().any(|m| m == i))
                .map(|p| (p.clone(), "gr".to_string()))
                .collect::<Vec<_>>()
        };
        let (attached, unmatched) = attach_products(
            &["multispecies_probiotic_skin".to_string(), "zinc_picolinate".to_string()],
            &lookup,
        );
        assert_eq!(attached.len(), 2);
        assert!(unmatched.is_empty());
    }
}
