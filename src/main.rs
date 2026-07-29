mod config;
mod openalex;

use crate::config::{Config, FeedConfig};
use crate::openalex::{
    normalize_id, Author, AuthorsResponse, SourceRecord, SourcesResponse, Work, WorksResponse,
    API_BASE,
};
use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::http::Error;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use rss::extension::dublincore::DublinCoreExtension;
use rss::{Category, Channel, ChannelBuilder, GuidBuilder, ItemBuilder, Source, TextInput};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration as StdDuration;
use tokio::net::TcpListener;
use tokio::time::Instant;

lazy_static! {
    pub static ref RSS_CHANNELS: Arc<RwLock<HashMap<FeedRequest, Channel>>> =
        Arc::new(RwLock::new(HashMap::new()));
    pub static ref CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent("google-scholar-rss-feed")
        .build()
        .expect("failed to build HTTP client");
}

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

const DEFAULT_FROM_DAYS: u32 = 365;

/// Fully-resolved description of a feed, used as the cache key.
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct FeedRequest {
    /// Normalized, deduped, sorted OpenAlex author ids.
    pub author_ids: Vec<String>,
    /// Normalized, deduped, sorted OpenAlex source (journal) ids.
    pub source_ids: Vec<String>,
    /// Earliest publication date (YYYY-MM-DD).
    pub from: String,
    /// Sorted OpenAlex topic ids.
    pub topics: Vec<String>,
    /// Optional channel title carried alongside (not part of identity below).
    pub title: Option<String>,
}

impl FeedRequest {
    fn cache_key(&self) -> (Vec<String>, Vec<String>, String, Vec<String>) {
        (
            self.author_ids.clone(),
            self.source_ids.clone(),
            self.from.clone(),
            self.topics.clone(),
        )
    }
}

#[tokio::main]
async fn main() {
    let (address, config_path) = parse_cli_args();
    CONFIG_PATH.set(config_path.clone()).ok();

    let addr = SocketAddr::from_str(&address).unwrap();

    println!("Listening on {address}...");
    println!("Using config file: {}", config_path.display());
    let listener = TcpListener::bind(addr).await.unwrap();

    println!("Server started");
    let mut last_update = Instant::now();
    loop {
        // Clear hourly so feeds refresh and the cache doesn't grow unbounded.
        if last_update.elapsed() >= StdDuration::from_secs(3600) {
            println!("Clearing cache");
            RSS_CHANNELS.write().clear();
            last_update = Instant::now();
        }

        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);

            tokio::task::spawn(async move {
                match http1::Builder::new()
                    .serve_connection(io, service_fn(send_rss))
                    .await
                {
                    Ok(_) => (),
                    Err(err) => eprintln!("Error serving connection: {:?}", err),
                }
            });
        }
    }
}

/// Parse CLI args: first non-flag positional is the bind address, `--config <path>`
/// (or env `GSRF_CONFIG`) selects the feeds config file.
fn parse_cli_args() -> (String, PathBuf) {
    let mut address: Option<String> = None;
    let mut config: Option<String> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next(),
            other => {
                if address.is_none() {
                    address = Some(other.to_string());
                }
            }
        }
    }

    let address = address.unwrap_or_else(|| "127.0.0.1:3005".to_string());
    let config = config
        .or_else(|| env::var("GSRF_CONFIG").ok())
        .unwrap_or_else(|| "feeds.toml".to_string());

    (address, PathBuf::from(config))
}

async fn send_rss(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Error> {
    // Preserve repeated query params (e.g. multiple ?author_id=).
    let params: Vec<(String, String)> = request
        .uri()
        .query()
        .map(|v| {
            url::form_urlencoded::parse(v.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();

    let config = Config::load(config_path());

    let feed_request = match resolve_feed_request(&params, &config).await {
        Ok(Some(fr)) => fr,
        Ok(None) => {
            return Response::builder()
                .header("Access-Control-Allow-Origin", "*")
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(
                    "No authors or journals specified. Provide ?author_id=, ?orcid=, \
                     ?author=, ?source_id=, ?issn=, or ?journal=, or configure feeds in \
                     the config file and use ?feed=<name>.",
                )));
        }
        Err(message) => {
            return Response::builder()
                .header("Access-Control-Allow-Origin", "*")
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from(message)));
        }
    };

    let channel = generate_channel_if_needed(feed_request).await;

    Response::builder()
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("Access-Control-Allow-Origin", "*")
        .header(
            "Cache-Control",
            "public, max-age=300, s-maxage=7200, stale-while-revalidate=86400",
        )
        .status(StatusCode::OK)
        .body(Full::new(Bytes::from(channel.to_string())))
}

fn config_path() -> &'static PathBuf {
    CONFIG_PATH.get().expect("config path not initialized")
}

/// Collect the values of a repeated query parameter.
fn collect_param(params: &[(String, String)], key: &str) -> Vec<String> {
    params
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.trim().is_empty())
        .collect()
}

fn first_param(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.trim().is_empty())
}

/// Merge a named feed (if any) with ad-hoc URL params, resolve every identifier to an
/// OpenAlex author id, and build the cache key. Returns:
/// - `Ok(Some(req))` when at least one author resolved,
/// - `Ok(None)` when no authors were specified at all,
/// - `Err(msg)` when a named feed was requested but not found.
async fn resolve_feed_request(
    params: &[(String, String)],
    config: &Config,
) -> Result<Option<FeedRequest>, String> {
    let feed_name = first_param(params, "feed");

    // Ad-hoc params.
    let adhoc_author_ids = collect_param(params, "author_id");
    let adhoc_orcids = collect_param(params, "orcid");
    let adhoc_authors = collect_param(params, "author");
    let adhoc_source_ids = collect_param(params, "source_id");
    let adhoc_issns = collect_param(params, "issn");
    let adhoc_journals = collect_param(params, "journal");
    let mut adhoc_topics = collect_param(params, "topic");
    adhoc_topics.extend(collect_param(params, "concept"));
    let adhoc_from = first_param(params, "from");

    let has_adhoc = !adhoc_author_ids.is_empty()
        || !adhoc_orcids.is_empty()
        || !adhoc_authors.is_empty()
        || !adhoc_source_ids.is_empty()
        || !adhoc_issns.is_empty()
        || !adhoc_journals.is_empty();

    // Determine the feed config to use.
    let feed: Option<&FeedConfig> = match &feed_name {
        Some(name) => match config.feeds.get(name) {
            Some(f) => Some(f),
            None => return Err(format!("Unknown feed \"{name}\".")),
        },
        None => {
            if has_adhoc {
                None
            } else {
                // Bare request: serve the default feed if configured.
                match &config.default_feed {
                    Some(name) => match config.feeds.get(name) {
                        Some(f) => Some(f),
                        None => {
                            return Err(format!("Configured default_feed \"{name}\" not found."))
                        }
                    },
                    None => return Ok(None),
                }
            }
        }
    };

    let empty = FeedConfig::default();
    let feed = feed.unwrap_or(&empty);

    // Gather author identifiers from feed config + ad-hoc params.
    let mut author_ids: Vec<String> = Vec::new();
    for id in feed.author_ids.iter().chain(adhoc_author_ids.iter()) {
        author_ids.push(normalize_id(id));
    }

    for orcid in feed.orcids.iter().chain(adhoc_orcids.iter()) {
        match resolve_orcid(orcid).await {
            Some(id) => author_ids.push(id),
            None => eprintln!("Could not resolve ORCID \"{orcid}\""),
        }
    }

    for name in feed.authors.iter().chain(adhoc_authors.iter()) {
        match resolve_author_name(name).await {
            Some((id, display)) => {
                println!("Resolved author \"{name}\" -> {id} ({display})");
                author_ids.push(id);
            }
            None => eprintln!("Could not resolve author name \"{name}\""),
        }
    }

    // Normalize the id set: dedupe + sort for a stable cache key.
    author_ids.sort();
    author_ids.dedup();

    // Gather journal (source) identifiers from feed config + ad-hoc params.
    let mut source_ids: Vec<String> = Vec::new();
    for id in feed.source_ids.iter().chain(adhoc_source_ids.iter()) {
        source_ids.push(normalize_id(id));
    }

    for issn in feed.issns.iter().chain(adhoc_issns.iter()) {
        match resolve_issn(issn).await {
            Some(id) => source_ids.push(id),
            None => eprintln!("Could not resolve ISSN \"{issn}\""),
        }
    }

    for name in feed.journals.iter().chain(adhoc_journals.iter()) {
        match resolve_journal_name(name).await {
            Some((id, display)) => {
                println!("Resolved journal \"{name}\" -> {id} ({display})");
                source_ids.push(id);
            }
            None => eprintln!("Could not resolve journal name \"{name}\""),
        }
    }

    source_ids.sort();
    source_ids.dedup();

    // A feed needs at least one author or journal to produce anything.
    if author_ids.is_empty() && source_ids.is_empty() {
        return Ok(None);
    }

    // Topics.
    let mut topics: Vec<String> = feed
        .topics
        .iter()
        .chain(adhoc_topics.iter())
        .map(|t| normalize_id(t))
        .collect();
    topics.sort();
    topics.dedup();

    // Recency window: ad-hoc `from` > feed `from` > settings.from_days > default.
    let from = adhoc_from
        .or_else(|| feed.from.clone())
        .unwrap_or_else(|| default_from_date(config.settings.from_days));

    Ok(Some(FeedRequest {
        author_ids,
        source_ids,
        from,
        topics,
        title: feed.title.clone(),
    }))
}

/// Compute a YYYY-MM-DD date `from_days` (or the default window) in the past.
fn default_from_date(from_days: Option<u32>) -> String {
    let days = from_days.unwrap_or(DEFAULT_FROM_DAYS) as i64;
    let date = Utc::now().date_naive() - Duration::days(days);
    date.format("%Y-%m-%d").to_string()
}

fn mailto() -> Option<String> {
    env::var("GSRF_MAILTO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Resolve an ORCID to a bare OpenAlex author id.
async fn resolve_orcid(orcid: &str) -> Option<String> {
    let orcid = orcid.trim();
    let orcid = orcid.rsplit('/').next().unwrap_or(orcid);
    let mut url = url::Url::parse(&format!("{API_BASE}/authors/https://orcid.org/{orcid}")).ok()?;
    if let Some(m) = mailto() {
        url.query_pairs_mut().append_pair("mailto", &m);
    }
    let author = CLIENT
        .get(url)
        .send()
        .await
        .ok()?
        .json::<Author>()
        .await
        .ok()?;
    Some(normalize_id(&author.id?))
}

/// Resolve an author display name (best match) to (id, display_name).
async fn resolve_author_name(name: &str) -> Option<(String, String)> {
    let mut pairs = vec![
        ("search".to_string(), name.to_string()),
        ("per_page".to_string(), "1".to_string()),
    ];
    if let Some(m) = mailto() {
        pairs.push(("mailto".to_string(), m));
    }
    let url = url::Url::parse_with_params(&format!("{API_BASE}/authors"), &pairs).ok()?;
    let response = CLIENT
        .get(url)
        .send()
        .await
        .ok()?
        .json::<AuthorsResponse>()
        .await
        .ok()?;
    let author = response.results.into_iter().next()?;
    let id = normalize_id(&author.id?);
    let display = author.display_name.unwrap_or_else(|| id.clone());
    Some((id, display))
}

/// Resolve an ISSN to a bare OpenAlex source id.
async fn resolve_issn(issn: &str) -> Option<String> {
    let issn = issn.trim();
    let mut url = url::Url::parse(&format!("{API_BASE}/sources/issn:{issn}")).ok()?;
    if let Some(m) = mailto() {
        url.query_pairs_mut().append_pair("mailto", &m);
    }
    let source = CLIENT
        .get(url)
        .send()
        .await
        .ok()?
        .json::<SourceRecord>()
        .await
        .ok()?;
    Some(normalize_id(&source.id?))
}

/// Resolve a journal display name (best match) to (id, display_name).
async fn resolve_journal_name(name: &str) -> Option<(String, String)> {
    let mut pairs = vec![
        ("search".to_string(), name.to_string()),
        ("per_page".to_string(), "1".to_string()),
    ];
    if let Some(m) = mailto() {
        pairs.push(("mailto".to_string(), m));
    }
    let url = url::Url::parse_with_params(&format!("{API_BASE}/sources"), &pairs).ok()?;
    let response = CLIENT
        .get(url)
        .send()
        .await
        .ok()?
        .json::<SourcesResponse>()
        .await
        .ok()?;
    let source = response.results.into_iter().next()?;
    let id = normalize_id(&source.id?);
    let display = source.display_name.unwrap_or_else(|| id.clone());
    Some((id, display))
}

async fn generate_channel_if_needed(request: FeedRequest) -> Channel {
    let key = request.cache_key();
    if let Some(channel) = find_cached(&key) {
        return channel;
    }

    let channel = build_channel(&request).await;
    RSS_CHANNELS.write().insert(request, channel.clone());
    channel
}

fn find_cached(key: &(Vec<String>, Vec<String>, String, Vec<String>)) -> Option<Channel> {
    let channels = RSS_CHANNELS.read();
    channels
        .iter()
        .find(|(req, _)| &req.cache_key() == key)
        .map(|(_, channel)| channel.clone())
}

async fn build_channel(request: &FeedRequest) -> Channel {
    println!(
        "Building RSS channel for authors [{}] journals [{}] from {}",
        request.author_ids.join(", "),
        request.source_ids.join(", "),
        request.from
    );

    let (title, description) = channel_metadata(request);

    let mut channel = ChannelBuilder::default()
        .title(title)
        .link(String::from("https://openalex.org"))
        .description(description)
        .language(String::from("en-US"))
        .generator(String::from("google-scholar-rss-feed"))
        .ttl(String::from("60"))
        .docs(String::from("https://cyber.harvard.edu/rss/rss.html"))
        .text_input(TextInput {
            title: String::from("OpenAlex"),
            description: String::from("Search OpenAlex"),
            name: String::from("q"),
            link: String::from("https://openalex.org/works"),
        })
        .categories(vec![Category::from("Scientific Research")])
        .build();

    let works = fetch_works(request).await;
    let items = works.iter().map(work_to_item).collect::<Vec<_>>();
    channel.set_items(items);

    let now = Utc::now().to_rfc2822();
    channel.set_pub_date(now.clone());
    channel.set_last_build_date(now);

    channel
}

fn channel_metadata(request: &FeedRequest) -> (String, String) {
    if let Some(title) = &request.title {
        return (
            title.clone(),
            format!("{title}. Recent publications parsed from OpenAlex."),
        );
    }
    let authors = request.author_ids.len();
    let journals = request.source_ids.len();

    let subject = match (authors, journals) {
        (a, 0) => format!("{a} author(s)"),
        (0, j) => format!("{j} journal(s)"),
        (a, j) => format!("{a} author(s) and {j} journal(s)"),
    };

    let title = format!("Recent publications ({subject})");
    let description =
        format!("An RSS feed of recent publications for {subject}, parsed from OpenAlex.");
    (title, description)
}

/// Fetch works for the feed as the UNION of an author query and a journal query,
/// merged, deduplicated by work id, and sorted newest-first.
async fn fetch_works(request: &FeedRequest) -> Vec<Work> {
    let topic_suffix = if request.topics.is_empty() {
        String::new()
    } else {
        format!(",topics.id:{}", request.topics.join("|"))
    };
    let from_suffix = format!(",from_publication_date:{}", request.from);

    let author_filter = (!request.author_ids.is_empty()).then(|| {
        format!(
            "author.id:{}{from_suffix}{topic_suffix}",
            request.author_ids.join("|")
        )
    });
    let journal_filter = (!request.source_ids.is_empty()).then(|| {
        format!(
            "primary_location.source.id:{}{from_suffix}{topic_suffix}",
            request.source_ids.join("|")
        )
    });

    // Run the (up to two) queries concurrently.
    let (author_works, journal_works) = tokio::join!(
        fetch_works_for_filter(author_filter),
        fetch_works_for_filter(journal_filter),
    );

    merge_works(author_works, journal_works)
}

/// A signature that identifies a work well enough to treat two records as versions of
/// each other. Two works are grouped when *any* of their keys match.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum VersionKey {
    /// Identical DOIs always denote the same work.
    Doi(String),
    /// Normalized title plus the complete set of OpenAlex author ids.
    AuthorIds(String, Vec<String>),
    /// Normalized title plus the complete set of normalized author names. Catches the
    /// common case where OpenAlex minted two author entities for the same person.
    AuthorNames(String, Vec<String>),
}

/// Merge two result sets, deduplicating exact OpenAlex records and grouping versions that
/// share a DOI, or a normalized title together with the same author ids *or* author names.
fn merge_works(primary: Vec<Work>, secondary: Vec<Work>) -> Vec<Work> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut version_groups: HashMap<VersionKey, usize> = HashMap::new();
    let mut works: Vec<Work> = Vec::new();

    for work in primary.into_iter().chain(secondary) {
        if work.id.as_ref().is_some_and(|id| !seen.insert(id.clone())) {
            continue;
        }

        let keys = version_keys(&work);
        let group = keys.iter().find_map(|key| version_groups.get(key).copied());

        match group {
            Some(index) => {
                merge_work_version(&mut works[index], work);
                // Register the incoming record's keys too, so later records matching
                // either variant land in the same group.
                for key in keys {
                    version_groups.entry(key).or_insert(index);
                }
            }
            None => {
                let index = works.len();
                for key in keys {
                    version_groups.insert(key, index);
                }
                works.push(work);
            }
        }
    }

    works.sort_by(|a, b| {
        let da = a.publication_date.as_deref().unwrap_or("");
        let db = b.publication_date.as_deref().unwrap_or("");
        db.cmp(da)
    });

    works
}

fn version_keys(work: &Work) -> Vec<VersionKey> {
    let mut keys = Vec::new();

    if let Some(doi) = work.doi.as_deref().map(normalize_doi) {
        if !doi.is_empty() {
            keys.push(VersionKey::Doi(doi));
        }
    }

    let title = work
        .title
        .as_ref()
        .or(work.display_name.as_ref())
        .map(|title| normalize_title(title))
        .filter(|title| !title.is_empty());

    let (Some(title), Some(authorships)) = (title, work.authorships.as_ref()) else {
        return keys;
    };
    if authorships.is_empty() {
        return keys;
    }

    // Only usable when *every* authorship carries an id: a partial set would let two
    // genuinely different works collide.
    let author_ids = authorships
        .iter()
        .map(|authorship| {
            authorship
                .author
                .as_ref()?
                .id
                .as_ref()
                .map(|id| normalize_id(id))
        })
        .collect::<Option<Vec<_>>>();
    if let Some(mut author_ids) = author_ids {
        author_ids.sort();
        author_ids.dedup();
        if !author_ids.is_empty() {
            keys.push(VersionKey::AuthorIds(title.clone(), author_ids));
        }
    }

    // Author names come in two flavours that disagree surprisingly often (`F Vermeire`
    // vs the raw `Florence Vermeire`), so key on each variant independently.
    let name_sources: [fn(&crate::openalex::Authorship) -> Option<&str>; 3] = [
        |authorship| {
            authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
                .or(authorship.raw_author_name.as_deref())
        },
        |authorship| {
            authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
        },
        |authorship| authorship.raw_author_name.as_deref(),
    ];

    for source in name_sources {
        let author_names = authorships
            .iter()
            .map(|authorship| {
                let name = normalize_author_name(source(authorship)?);
                (!name.is_empty()).then_some(name)
            })
            .collect::<Option<Vec<_>>>();

        if let Some(mut author_names) = author_names {
            author_names.sort();
            author_names.dedup();
            if !author_names.is_empty() {
                let key = VersionKey::AuthorNames(title.clone(), author_names);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
    }

    keys
}

/// Reduce a DOI to a comparable form: bare lowercase `10.x/y` without the resolver prefix.
fn normalize_doi(doi: &str) -> String {
    let doi = doi.trim().to_lowercase();
    let doi = doi
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("dx.")
        .trim_start_matches("doi.org/")
        .trim_start_matches("doi:");
    doi.trim().trim_end_matches('/').to_string()
}

/// Normalize an author name into an order-independent set of name tokens, so that
/// `Tang, Sophia`, `Sophia Tang` and `Sophia  TANG` all compare equal. Single-character
/// initials are dropped so `S. Tang` also matches, unless that would leave nothing.
fn normalize_author_name(name: &str) -> String {
    let tokens = name
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();

    let mut significant = tokens
        .iter()
        .filter(|token| token.chars().count() > 1)
        .cloned()
        .collect::<Vec<_>>();
    if significant.is_empty() {
        significant = tokens;
    }

    significant.sort();
    significant.dedup();
    significant.join(" ")
}

fn normalize_title(title: &str) -> String {
    let mut normalized = String::new();
    let mut separated = false;

    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separated && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            separated = false;
        } else if !normalized.is_empty() {
            separated = true;
        }
    }

    normalized
}

fn merge_work_version(existing: &mut Work, mut candidate: Work) {
    if version_quality(&candidate) > version_quality(existing) {
        std::mem::swap(existing, &mut candidate);
    }

    let selected_link = existing.best_link();
    let mut alternate_links = std::mem::take(&mut existing.alternate_links);
    alternate_links.append(&mut candidate.alternate_links);
    if let Some(link) = candidate.best_link() {
        alternate_links.push(link);
    }
    alternate_links.retain(|link| Some(link) != selected_link.as_ref());
    alternate_links.sort();
    alternate_links.dedup();
    existing.alternate_links = alternate_links;
}

fn version_quality(work: &Work) -> (bool, bool, bool, &str) {
    let has_pdf = work.oa_pdf_url().is_some();
    let has_abstract = work
        .abstract_inverted_index
        .as_ref()
        .is_some_and(|index| !index.is_empty());
    let is_published = [&work.primary_location, &work.best_oa_location]
        .into_iter()
        .flatten()
        .any(|location| location.version.as_deref() == Some("publishedVersion"));
    let publication_date = work.publication_date.as_deref().unwrap_or("");

    (has_pdf, has_abstract, is_published, publication_date)
}

/// Run a single `/works` query for the given filter (or return empty if `None`).
async fn fetch_works_for_filter(filter: Option<String>) -> Vec<Work> {
    let filter = match filter {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut pairs = vec![
        ("filter".to_string(), filter),
        ("sort".to_string(), "publication_date:desc".to_string()),
        ("per_page".to_string(), "50".to_string()),
    ];
    if let Some(m) = mailto() {
        pairs.push(("mailto".to_string(), m));
    }

    let url = match url::Url::parse_with_params(&format!("{API_BASE}/works"), &pairs) {
        Ok(url) => url,
        Err(err) => {
            eprintln!("Failed to build OpenAlex URL: {err}");
            return Vec::new();
        }
    };

    let response = match CLIENT.get(url).send().await {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("OpenAlex request failed: {err}");
            return Vec::new();
        }
    };

    match response.json::<WorksResponse>().await {
        Ok(body) => body.results,
        Err(err) => {
            eprintln!("Failed to parse OpenAlex response: {err}");
            Vec::new()
        }
    }
}

fn work_to_item(work: &Work) -> rss::Item {
    let link = work.best_link();

    let guid = link
        .clone()
        .map(|value| GuidBuilder::default().value(value).permalink(true).build());

    let source = work.venue().map(|name| Source {
        url: work
            .best_link()
            .unwrap_or_else(|| String::from("https://openalex.org")),
        title: Some(name),
    });

    let mut description = match (work.venue(), work.cited_by_count) {
        (Some(venue), Some(cites)) if cites > 0 => Some(format!("{venue} — cited {cites} times")),
        (Some(venue), _) => Some(venue),
        (None, Some(cites)) if cites > 0 => Some(format!("Cited {cites} times")),
        (None, _) => None,
    };

    if !work.alternate_links.is_empty() {
        let label = if work.alternate_links.len() == 1 {
            "Alternate version"
        } else {
            "Alternate versions"
        };
        let alternates = format!("{label}: {}", work.alternate_links.join(", "));
        description = Some(match description {
            Some(current) => format!("{current}\n{alternates}"),
            None => alternates,
        });
    }

    if let Some(pdf_url) = work.oa_pdf_url() {
        let pdf = format!("Open-access PDF: {pdf_url}");
        description = Some(match description {
            Some(current) => format!("{current}\n{pdf}"),
            None => pdf,
        });
    }

    let author_names = work.author_names();
    let dublin_core = (!author_names.is_empty()).then(|| DublinCoreExtension {
        creators: author_names,
        ..DublinCoreExtension::default()
    });

    ItemBuilder::default()
        .title(Some(work.best_title()))
        .description(description)
        .link(link)
        .guid(guid)
        .source(source)
        .pub_date(work.publication_date.as_deref().and_then(to_rfc2822))
        .dublin_core_ext(dublin_core)
        .content(work.abstract_text())
        .build()
}

/// Convert an OpenAlex `YYYY-MM-DD` date into an RFC-2822 timestamp.
fn to_rfc2822(date: &str) -> Option<String> {
    let naive = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let datetime = naive.and_time(NaiveTime::from_hms_opt(0, 0, 0)?);
    let utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc);
    Some(utc.to_rfc2822())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(id: Option<&str>, date: Option<&str>) -> Work {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "publication_date": date,
        }))
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn version_work(
        id: &str,
        doi: &str,
        title: &str,
        date: &str,
        author_ids: &[&str],
        version: &str,
        pdf_url: Option<&str>,
        has_abstract: bool,
    ) -> Work {
        let authorships = author_ids
            .iter()
            .map(|id| serde_json::json!({"author": {"id": id}}))
            .collect::<Vec<_>>();
        let best_oa_location = pdf_url.map(|url| {
            serde_json::json!({
                "pdf_url": url,
                "version": version
            })
        });
        let abstract_index = has_abstract.then(|| serde_json::json!({"Abstract": [0]}));

        serde_json::from_value(serde_json::json!({
            "id": id,
            "doi": doi,
            "title": title,
            "publication_date": date,
            "authorships": authorships,
            "primary_location": {
                "landing_page_url": doi,
                "version": version
            },
            "best_oa_location": best_oa_location,
            "abstract_inverted_index": abstract_index
        }))
        .unwrap()
    }

    fn ids(works: &[Work]) -> Vec<Option<String>> {
        works.iter().map(|w| w.id.clone()).collect()
    }

    #[test]
    fn merge_dedupes_by_id_across_both_sets() {
        let primary = vec![
            work(Some("W1"), Some("2025-01-01")),
            work(Some("W2"), Some("2025-03-01")),
        ];
        let secondary = vec![
            work(Some("W2"), Some("2025-03-01")),
            work(Some("W3"), Some("2025-02-01")),
        ];

        let merged = merge_works(primary, secondary);

        // W2 appears once; result sorted newest-first: W2 (03), W3 (02), W1 (01).
        assert_eq!(
            ids(&merged),
            vec![
                Some("W2".to_string()),
                Some("W3".to_string()),
                Some("W1".to_string())
            ]
        );
    }

    #[test]
    fn merge_keeps_items_without_id_and_puts_missing_dates_last() {
        let primary = vec![work(None, Some("2025-05-01")), work(None, None)];
        let secondary = vec![work(Some("W9"), None)];

        let merged = merge_works(primary, secondary);

        assert_eq!(merged.len(), 3);
        // The dated item comes first; the two date-less items follow.
        assert_eq!(merged[0].publication_date.as_deref(), Some("2025-05-01"));
        assert!(merged[1].publication_date.is_none());
        assert!(merged[2].publication_date.is_none());
    }

    #[test]
    fn merge_groups_versions_and_keeps_the_richest_accessible_record() {
        let published = version_work(
            "W-published",
            "https://doi.org/10.1109/example",
            "DNAS-Bench: Deterministic Nucleic Acid Screener Benchmarking",
            "2026-05-21",
            &["A1", "A2", "A3"],
            "publishedVersion",
            None,
            false,
        );
        let preprint = version_work(
            "W-preprint",
            "https://doi.org/10.64898/example",
            "DNAS Bench — Deterministic Nucleic Acid Screener Benchmarking",
            "2026-07-20",
            &["A3", "A1", "A2"],
            "acceptedVersion",
            Some("https://example.com/preprint.pdf"),
            true,
        );

        let merged = merge_works(vec![published], vec![preprint]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id.as_deref(), Some("W-preprint"));
        assert_eq!(
            merged[0].alternate_links,
            vec!["https://doi.org/10.1109/example"]
        );
        assert!(work_to_item(&merged[0])
            .description
            .as_deref()
            .is_some_and(|description| description.contains("Alternate version:")));
    }

    #[test]
    fn merge_keeps_same_title_with_different_authors_separate() {
        let first = version_work(
            "W1",
            "https://doi.org/10.example/one",
            "Shared title",
            "2026-05-21",
            &["A1", "A2"],
            "publishedVersion",
            None,
            false,
        );
        let second = version_work(
            "W2",
            "https://doi.org/10.example/two",
            "Shared title",
            "2026-05-22",
            &["A1", "A3"],
            "publishedVersion",
            None,
            false,
        );

        assert_eq!(merge_works(vec![first], vec![second]).len(), 2);
    }

    #[test]
    fn merge_groups_duplicate_author_entities_with_same_title() {
        // OpenAlex sometimes mints two author ids for the same person, which used to
        // defeat the author-id-only version key ("Expanding Flow Maps").
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7171268167",
            "title": "Expanding Flow Maps",
            "publication_date": "2026-07-23",
            "authorships": [
                {"author": {"id": "https://openalex.org/A5143604557", "display_name": "Sophia Tang"}},
                {"author": {"id": "https://openalex.org/A5016342562", "display_name": "Pranam Chatterjee"}}
            ]
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7170527570",
            "doi": "https://doi.org/10.48550/arxiv.2607.21585",
            "title": "Expanding Flow Maps",
            "publication_date": "2026-07-23",
            "authorships": [
                {"author": {"id": "https://openalex.org/A5143533688"}, "raw_author_name": "Tang, Sophia"},
                {"author": {"id": "https://openalex.org/A5016342562", "display_name": "Pranam Chatterjee"}}
            ]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 1);
    }

    #[test]
    fn merge_groups_records_sharing_a_doi() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "doi": "https://doi.org/10.1234/Example",
            "title": "One title"
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "W2",
            "doi": "doi:10.1234/example",
            "title": "A differently recorded title"
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 1);
    }

    #[test]
    fn merge_groups_versions_when_only_raw_author_names_agree() {
        // chemRxiv v1/v2: display names diverge (`F Vermeire` vs `Florence H. Vermeire`)
        // while the raw names recorded on the authorship agree.
        let v1: Work = serde_json::from_value(serde_json::json!({
            "id": "W-v1",
            "doi": "https://doi.org/10.26434/chemrxiv.15003516/v1",
            "title": "QuantumPioneer: Scalable generation of quantum chemical data",
            "publication_date": "2026-05-18",
            "authorships": [
                {"author": {"id": "A5090585248", "display_name": "F Vermeire"}, "raw_author_name": "Florence Vermeire"},
                {"author": {"id": "A5051475129", "display_name": "William H. Green"}, "raw_author_name": "William H. Green"}
            ]
        }))
        .unwrap();
        let v2: Work = serde_json::from_value(serde_json::json!({
            "id": "W-v2",
            "doi": "https://doi.org/10.26434/chemrxiv.15003516/v2",
            "title": "QuantumPioneer: Scalable generation of quantum chemical data",
            "publication_date": "2026-07-12",
            "authorships": [
                {"author": {"id": "A5022062241", "display_name": "Florence H. Vermeire"}, "raw_author_name": "Florence Vermeire"},
                {"author": {"id": "A5051475129", "display_name": "William H. Green"}, "raw_author_name": "William H. Green"}
            ]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![v1], vec![v2]).len(), 1);
    }

    #[test]
    fn merge_keeps_same_title_with_different_author_names_separate() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "title": "Shared title",
            "authorships": [{"author": {"display_name": "Ada Lovelace"}}]
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "W2",
            "title": "Shared title",
            "authorships": [{"author": {"display_name": "Grace Hopper"}}]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 2);
    }

    #[test]
    fn author_names_normalize_order_case_and_initials() {
        assert_eq!(
            normalize_author_name("Tang, Sophia"),
            normalize_author_name("Sophia  TANG")
        );
        assert_eq!(
            normalize_author_name("S. Tang"),
            normalize_author_name("Tang")
        );
        assert_ne!(
            normalize_author_name("Ada Lovelace"),
            normalize_author_name("Grace Hopper")
        );
    }

    #[test]
    fn item_uses_dublin_core_creators_and_omits_unknown_length_enclosure() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "title": "Example",
            "authorships": [
                {"author": {"display_name": "Ada Lovelace"}},
                {"author": {"display_name": "Grace Hopper"}}
            ],
            "best_oa_location": {
                "pdf_url": "https://example.com/paper.pdf"
            }
        }))
        .unwrap();

        let item = work_to_item(&work);

        assert!(item.author.is_none());
        assert!(item.enclosure.is_none());
        assert_eq!(
            item.dublin_core_ext.unwrap().creators,
            vec!["Ada Lovelace", "Grace Hopper"]
        );
        assert!(item
            .description
            .as_deref()
            .is_some_and(|description| description.contains("https://example.com/paper.pdf")));
    }
}
