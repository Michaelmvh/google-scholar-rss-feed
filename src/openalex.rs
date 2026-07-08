use serde::Deserialize;
use std::collections::HashMap;

pub const API_BASE: &str = "https://api.openalex.org";

/// A single author record from `/authors`.
#[derive(Debug, Deserialize)]
pub struct Author {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthorsResponse {
    pub results: Vec<Author>,
}

/// A source (journal, repository, etc.) record from `/sources`.
#[derive(Debug, Deserialize)]
pub struct SourceRecord {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourcesResponse {
    pub results: Vec<SourceRecord>,
}

/// A single work record from `/works`.
#[derive(Debug, Deserialize)]
pub struct Work {
    pub id: Option<String>,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub publication_date: Option<String>,
    pub cited_by_count: Option<u64>,
    pub authorships: Option<Vec<Authorship>>,
    pub primary_location: Option<Location>,
    pub best_oa_location: Option<Location>,
    pub abstract_inverted_index: Option<HashMap<String, Vec<u32>>>,
}

#[derive(Debug, Deserialize)]
pub struct Authorship {
    pub author: Option<Author>,
}

#[derive(Debug, Deserialize)]
pub struct Location {
    pub landing_page_url: Option<String>,
    pub pdf_url: Option<String>,
    pub source: Option<Source>,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorksResponse {
    pub results: Vec<Work>,
}

impl Work {
    /// Best available human-readable title.
    pub fn best_title(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.display_name.clone())
            .unwrap_or_else(|| String::from("Untitled"))
    }

    /// Best available link for the work: DOI, then landing page, then OpenAlex id.
    pub fn best_link(&self) -> Option<String> {
        if let Some(doi) = &self.doi {
            return Some(doi.clone());
        }
        if let Some(loc) = &self.primary_location {
            if let Some(url) = &loc.landing_page_url {
                return Some(url.clone());
            }
        }
        self.id.clone()
    }

    /// Comma-separated author display names.
    pub fn authors_joined(&self) -> Option<String> {
        let names: Vec<String> = self
            .authorships
            .as_ref()?
            .iter()
            .filter_map(|a| a.author.as_ref().and_then(|au| au.display_name.clone()))
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(names.join(", "))
        }
    }

    /// Venue (journal / repository) name.
    pub fn venue(&self) -> Option<String> {
        self.primary_location
            .as_ref()
            .and_then(|loc| loc.source.as_ref())
            .and_then(|s| s.display_name.clone())
    }

    /// Open-access PDF url, if any.
    pub fn oa_pdf_url(&self) -> Option<String> {
        self.best_oa_location
            .as_ref()
            .and_then(|loc| loc.pdf_url.clone())
            .or_else(|| {
                self.primary_location
                    .as_ref()
                    .and_then(|loc| loc.pdf_url.clone())
            })
    }

    /// Reconstruct the abstract text from OpenAlex's inverted index.
    pub fn abstract_text(&self) -> Option<String> {
        let index = self.abstract_inverted_index.as_ref()?;
        if index.is_empty() {
            return None;
        }
        let mut positions: Vec<(u32, &str)> = Vec::new();
        for (word, locs) in index {
            for &pos in locs {
                positions.push((pos, word.as_str()));
            }
        }
        positions.sort_by_key(|(pos, _)| *pos);
        let text = positions
            .into_iter()
            .map(|(_, word)| word)
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Normalize any OpenAlex id or url (author `A…`, source `S…`, topic `T…`) to its bare
/// id form by stripping the `https://openalex.org/` prefix.
pub fn normalize_id(raw: &str) -> String {
    raw.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .to_string()
}
