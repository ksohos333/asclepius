pub mod analyte;
pub mod bridge;
pub mod conditions;
pub mod derived;
pub mod error;
pub mod interventions;
pub mod literature;
pub mod panel;
pub mod profile;
pub mod range;
pub mod rules;
pub mod store;
pub mod units;

pub const NAME: &str = "asclepius";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
