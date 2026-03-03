use google_scholar_query::scholar::{init_client, ScholarArgs};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::http::Error;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use rss::{
    Category, Channel, ChannelBuilder, Enclosure, GuidBuilder, ItemBuilder, Source, TextInput,
};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use chrono::Utc;
use tokio::net::TcpListener;
use tokio::time::Instant;

lazy_static! {
    pub static ref RSS_CHANNELS: Arc<RwLock<HashMap<UserIdentifier, Channel>>> =
        Arc::new(RwLock::new(HashMap::new()));
}

#[derive(Hash, Eq, PartialEq, Clone)]
pub enum UserIdentifier {
    Id(String),
    Username(String),
}

#[tokio::main]
async fn main() {
    let address = env::args().nth(1).unwrap_or_else(|| "127.0.0.1:3005".to_string());

    let addr = SocketAddr::from_str(&address).unwrap();

    // We create a TcpListener and bind it to 127.0.0.1:3000
    println!("Listening on {address}...");
    let listener = TcpListener::bind(addr).await.unwrap();

    println!("Server started");
    let mut last_update = Instant::now();
    loop {
        // Clear every day to avoid overflow
        if last_update.elapsed() >= Duration::from_secs(3600) {
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

async fn send_rss(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Error> {
    let params: HashMap<String, String> = request
        .uri()
        .query()
        .map(|v| {
            url::form_urlencoded::parse(v.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_else(HashMap::new);

    let response;

    if params.is_empty() || !params.contains_key("username") && !params.contains_key("id") {
        response = Response::builder()
            .header("Access-Control-Allow-Origin", "*")
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(
                "no \"username\" or \"id\" param provided",
            )))?;
    } else {
        let user_identifier = params
            .get("id")
            .map(|id| UserIdentifier::Id(id.clone()))
            .unwrap_or_else(|| UserIdentifier::Username(params.get("username").unwrap().clone()));
        let channel = generate_channel_if_needed(user_identifier).await;

        response = Response::builder()
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("Access-Control-Allow-Origin", "*")
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from(channel.to_string())))?;
    }

    Ok(response)
}

async fn generate_channel_if_needed(user: UserIdentifier) -> Channel {
    if !RSS_CHANNELS.read().contains_key(&user) {
        let (title, description) = match &user {
            UserIdentifier::Id(id) => (
                format!("Google Scholar Publications for User ID: {id}"),
               format!("An RSS feed for scientific publications associated with user ID {id}. Parsed from Google Scholar.")
            ),
            UserIdentifier::Username(username) => (
                format!("{username} scientific publications"),
               format!("An RSS feed for {username} scientific publications. Parsed from Google Scholar.")
            )
        };
        let mut new_channel = ChannelBuilder::default()
            .title(title)
            .description(description)
            .language(String::from("en-US"))
            .generator(String::from("google-scholar-rss-feed"))
            .copyright(String::from("© Google Scholar"))
            .ttl(String::from("60"))
            .docs(String::from("https://cyber.harvard.edu/rss/rss.html"))
            .text_input(TextInput {
                title: String::from("Google Scholar"),
                description: String::from("Search Google Scholar"),
                name: String::from("q"),
                link: String::from("https://scholar.google.com/scholar"),
            })
            .categories(vec![Category::from("Scientific Research")])
            .build();

        update_rss_channel(&user, &mut new_channel).await;

        RSS_CHANNELS.write().insert(user.clone(), new_channel);
    }

    RSS_CHANNELS.read().get(&user).unwrap().clone()
}

async fn update_rss_channel(user: &UserIdentifier, channel: &mut Channel) {
    match user {
        UserIdentifier::Id(id) => println!("Updating RSS channel for User ID \"{id}\""),
        UserIdentifier::Username(username) => println!("Updating RSS channel for \"{username}\""),
    };

    let query_params = match user {
        UserIdentifier::Id(_id) => unimplemented!(
            "query by user id is unsupported by `google_scholar_query` at the moment"
        ),
        UserIdentifier::Username(username) => format!("author:\"{}\"", username),
    };
    let client = init_client();
    let query = ScholarArgs {
        query: query_params,
        cite_id: None,
        from_year: None,
        to_year: None,
        sort_by: None,
        cluster_id: None,
        lang: None,
        lang_limit: None,
        limit: Some(100),
        offset: None,
        adult_filtering: None,
        include_similar_results: None,
        include_citations: None,
    };

    let results = match client.scrape_scholar(Box::from(query)).await {
        Ok(results) => results,
        Err(_e) => panic!("Google scholar query failed"),
    };

    let mut items = vec![];

    for result in results {
        let source_url = match result.domain.contains(".") {
            true => format!("https://{}", result.domain.clone()),
            false => format!("https://{}.com", result.domain),
        };

        let enclosure = match result.pdf_link {
            None => None,
            Some(pdf_link) => Some(Enclosure {
                url: pdf_link,
                length: String::from(""),
                mime_type: String::from("application/pdf"),
            }),
        };

        let description = match (result.conference, result.citations) {
            (None, None) => None,
            (Some(conference), None) => Some(conference),
            (None, Some(citations)) => Some(format!("Cited {citations} times")),
            (Some(conference), Some(citations)) => {
                Some(format!("{conference} - Cited {citations} times"))
            }
        };

        let item = ItemBuilder::default()
            .title(result.title)
            .author(result.author)
            .description(description)
            .link(result.link.clone())
            .guid(
                GuidBuilder::default()
                    .value(result.link)
                    .permalink(true)
                    .build(),
            )
            .source(Source {
                url: source_url,
                title: Some(String::from(&result.domain)),
            })
            .pub_date(result.year.map(|year| format!("{year}-01-01")))
            .enclosure(enclosure)
            .content(result.abs)
            .build();

        items.push(item);
    }

    channel.set_items(items);

    let now = Utc::now().to_rfc2822();
    channel.set_pub_date(now.clone());
    channel.set_last_build_date(now);
}
