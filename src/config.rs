use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Top-level feeds configuration, parsed from a TOML file.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Name of the feed served at bare `/`.
    pub default_feed: Option<String>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub feeds: HashMap<String, FeedConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Settings {
    /// Contact used for OpenAlex's polite pool (`mailto`).
    pub mailto: Option<String>,
    /// Default recency window in days when a feed omits `from`.
    pub from_days: Option<u32>,
}

/// A single named feed definition.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct FeedConfig {
    /// Optional RSS channel title override.
    pub title: Option<String>,
    #[serde(default)]
    pub author_ids: Vec<String>,
    #[serde(default)]
    pub orcids: Vec<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub issns: Vec<String>,
    #[serde(default)]
    pub journals: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// Explicit earliest publication date (YYYY-MM-DD); overrides `from_days`.
    pub from: Option<String>,
}

impl Config {
    /// Load config from `path`. A missing file yields an empty config so the
    /// server still works in pure ad-hoc-param mode.
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str::<Config>(&contents) {
                Ok(config) => config,
                Err(err) => {
                    eprintln!("Failed to parse config {}: {err}", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }
}
