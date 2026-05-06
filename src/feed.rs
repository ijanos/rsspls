use std::fs;

use basic_toml as toml;
use log::{debug, error, info, warn};
use lol_html::{RewriteStrSettings, element, rewrite_str};
use mime_guess::mime;
use reqwest::header::HeaderMap;
use reqwest::{RequestBuilder, StatusCode};
use rss::{Channel, ChannelBuilder, EnclosureBuilder, GuidBuilder, Item, ItemBuilder};
use scraper::{ElementRef, Html, Selector};
use simple_eyre::eyre::{self, WrapErr, bail, eyre};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;
use tokio::task;
use url::Url;

use crate::Client;
use crate::cache::RequestCacheWrite;
use crate::config::{ChannelConfig, ConfigHash, DateConfig, FeedConfig};

#[derive(Debug)]
pub enum ProcessResult {
    NotModified,
    Ok {
        channel: Box<Channel>,
        headers: Option<String>,
    },
}

pub enum FetchResult {
    NotModified,
    Ok {
        html: String,
        headers: Option<String>,
    },
}

pub async fn process_feed(
    client: &Client,
    channel_config: &ChannelConfig,
    config_hash: ConfigHash<'_>,
    cached_headers: Option<&HeaderMap>,
) -> eyre::Result<ProcessResult> {
    let config = &channel_config.config;
    info!("processing {}", config.url);
    let url: Url = config
        .url
        .parse()
        .wrap_err_with(|| format!("unable to parse {} as a URL", config.url))?;

    let (html, serialised_headers) =
        match fetch_webpage(client, &url, cached_headers, channel_config, config_hash).await? {
            FetchResult::Ok { html, headers } => (html, headers),
            FetchResult::NotModified => return Ok(ProcessResult::NotModified),
        };

    let link_selector = config.link.as_ref().unwrap_or(&config.heading);

    let doc = Html::parse_document(&html);
    let item_selector = Selector::parse(&config.item)
        .map_err(|_| eyre!("invalid selector for item: {}", config.item))?;
    let base_url = Url::options().base_url(Some(&url));

    let mut items = Vec::new();
    for item in doc.select(&item_selector) {
        match process_item(config, item, link_selector, &base_url) {
            Ok(rss_item) => items.push(rss_item),
            Err(err) => {
                let report = err.wrap_err(format!(
                    "unable to process RSS item matching '{}'",
                    config.item
                ));
                error!("{report:?}");
            }
        }
    }

    let channel = ChannelBuilder::default()
        .title(&channel_config.title)
        .link(url.to_string())
        .generator(Some(crate::version_string().to_string()))
        .items(items)
        .build();

    Ok(ProcessResult::Ok {
        channel: Box::new(channel),
        headers: serialised_headers,
    })
}

async fn fetch_webpage(
    client: &Client,
    url: &Url,
    cached_headers: Option<&HeaderMap>,
    channel_config: &ChannelConfig,
    config_hash: ConfigHash<'_>,
) -> eyre::Result<FetchResult> {
    if url.scheme() == "file" {
        if client.file_urls {
            fetch_webpage_local(url).await
        } else {
            bail!("unable to fetch: {url} as file URLs are not enabled in config")
        }
    } else {
        fetch_webpage_http(client, url, cached_headers, channel_config, config_hash).await
    }
}

async fn fetch_webpage_http(
    client: &Client,
    url: &Url,
    cached_headers: Option<&HeaderMap>,
    channel_config: &ChannelConfig,
    config_hash: ConfigHash<'_>,
) -> eyre::Result<FetchResult> {
    let config = &channel_config.config;

    let req = add_headers(
        client.http.get(url.clone()),
        cached_headers,
        channel_config.user_agent.as_ref(),
    );

    let resp = req
        .send()
        .await
        .wrap_err_with(|| format!("unable to fetch {url}"))?;

    // Check response
    let status = resp.status();
    if status == StatusCode::NOT_MODIFIED {
        // Cache hit, nothing to do
        info!("{url} is unmodified");
        return Ok(FetchResult::NotModified);
    }

    if !status.is_success() {
        return Err(eyre!(
            "failed to fetch {}: {} {}",
            config.url,
            status.as_str(),
            status.canonical_reason().unwrap_or("Unknown Status")
        ));
    }

    if config.link.is_none() {
        info!(
            "no explicit link selector provided, falling back to heading selector: {:?}",
            config.heading
        );
    }

    // Collect the headers for later
    let headers: Vec<_> = resp
        .headers()
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|val| (name.as_str(), val)))
        .collect();
    let map = RequestCacheWrite {
        headers,
        version: crate::version(),
        config_hash,
    };
    let serialised_headers = toml::to_string(&map)
        .map_err(|err| warn!("unable to serialise headers: {err}"))
        .ok();

    // Read body
    let html = resp.text().await.wrap_err("unable to read response body")?;

    Ok(FetchResult::Ok {
        html,
        headers: serialised_headers,
    })
}

async fn fetch_webpage_local(url: &Url) -> eyre::Result<FetchResult> {
    let path = url
        .to_file_path()
        .map_err(|()| eyre!("unable to extract path from: {}", url))?;
    debug!("read {}", path.display());
    let html = task::spawn_blocking(move || {
        fs::read_to_string(&path).wrap_err_with(|| format!("error reading {}", path.display()))
    })
    .await
    .wrap_err_with(|| format!("error joining task for {url}"))??;

    Ok(FetchResult::Ok {
        html,
        headers: None,
    })
}

fn process_item(
    config: &FeedConfig,
    item: ElementRef<'_>,
    link_selector: &str,
    base_url: &url::ParseOptions,
) -> eyre::Result<Item> {
    let heading_selector = Selector::parse(&config.heading)
        .map_err(|_| eyre!("invalid selector for heading: {}", config.heading))?;
    let title = select_first_including_self(item, &heading_selector).ok_or_else(|| {
        eyre!(
            "heading selector did not match anything: {}",
            config.heading
        )
    })?;
    let link_selector = Selector::parse(link_selector)
        .map_err(|_| eyre!("invalid selector for link: {}", link_selector))?;
    let link = select_first_including_self(item, &link_selector)
        .ok_or_else(|| eyre!("link selector did not match anything"))?;
    // Keep the previous global href-rewrite behaviour without mutating the DOM.
    let link_url = link
        .value()
        .attr("href")
        .map(|href| rewrite_href_value(href, base_url))
        .ok_or_else(|| eyre!("element selected as link has no 'href' attribute"))?;
    let title_text = link_text(&title);
    let description = extract_description(config, &item, &title_text, base_url)?;
    let date = extract_pub_date(config, &item)?;
    let guid = GuidBuilder::default()
        .value(&link_url)
        .permalink(false)
        .build();

    let mut rss_item_builder = ItemBuilder::default();
    rss_item_builder
        .title(title_text)
        .link(base_url.parse(&link_url).ok().map(|u| u.to_string()))
        .guid(Some(guid))
        .pub_date(date.map(|date| date.format(&Rfc2822).unwrap()))
        .description(description);

    // Media enclosure
    if let Some(media_selector) = &config.media {
        debug!("checking for media matching {media_selector}");
        let media_selector = Selector::parse(media_selector)
            .map_err(|_| eyre!("invalid selector for media: {}", media_selector))?;
        let media = select_first_including_self(item, &media_selector)
            .ok_or_else(|| eyre!("media selector did not match anything"))?;

        let media_url = media
            .value()
            .attr("src")
            .or_else(|| media.value().attr("href"))
            .ok_or_else(|| eyre!("element selected as media has no 'src' or 'href' attribute"))?;

        let parsed_url = base_url
            .parse(media_url)
            .map_err(|e| eyre!("media enclosure url invalid: {e}"))?;

        // Guessing the MIME type from the url as we don't have the full media
        #[expect(clippy::map_unwrap_or)]
        let media_mime_type = parsed_url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(|media_filename| mime_guess::from_path(media_filename).first_or_octet_stream())
            .unwrap_or_else(|| mime::APPLICATION_OCTET_STREAM);

        let mut enclosure_bld = EnclosureBuilder::default();
        enclosure_bld.url(parsed_url.to_string());
        enclosure_bld.mime_type(media_mime_type.to_string());
        // "When an enclosure's size cannot be determined, a publisher should use a length of 0."
        // https://www.rssboard.org/rss-profile#element-channel-item-enclosure
        enclosure_bld.length("0".to_string());

        rss_item_builder.enclosure(Some(enclosure_bld.build()));
    }

    Ok(rss_item_builder.build())
}

fn rewrite_href_value(href: &str, base_url: &url::ParseOptions) -> String {
    #[expect(clippy::map_unwrap_or)]
    base_url
        .parse(href)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| href.to_string())
}

fn rewrite_hrefs_in_html(html: &str, base_url: &url::ParseOptions) -> eyre::Result<String> {
    rewrite_str(
        html,
        RewriteStrSettings {
            element_content_handlers: vec![element!("*[href]", |el| {
                if let Some(href) = el.get_attribute("href") {
                    el.set_attribute("href", &rewrite_href_value(&href, base_url))?;
                }

                Ok(())
            })],
            ..RewriteStrSettings::default()
        },
    )
    .wrap_err("unable to rewrite hrefs in HTML fragment")
}

fn add_headers(
    mut req: RequestBuilder,
    cached_headers: Option<&HeaderMap>,
    user_agent: Option<&String>,
) -> RequestBuilder {
    use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT};

    if let Some(ua) = user_agent {
        debug!("add User-Agent: {ua:?}");
        req = req.header(USER_AGENT, ua);
    }

    let Some(headers) = cached_headers else {
        return req;
    };

    if let Some(last_modified) = headers.get(LAST_MODIFIED) {
        debug!("add If-Modified-Since: {:?}", last_modified.to_str().ok());
        req = req.header(IF_MODIFIED_SINCE, last_modified);
    }
    if let Some(etag) = headers.get(ETAG) {
        debug!("add If-None-Match: {:?}", etag.to_str().ok());
        req = req.header(IF_NONE_MATCH, etag);
    }
    req
}

fn extract_pub_date(
    config: &FeedConfig,
    item: &ElementRef<'_>,
) -> eyre::Result<Option<OffsetDateTime>> {
    config
        .date
        .as_ref()
        .map(|date| {
            let date_selector = Selector::parse(date.selector())
                .map_err(|_| eyre!("invalid selector for date: {}", date.selector()))?;
            Ok(select_first_including_self(*item, &date_selector)
                .and_then(|node| parse_date(date, &node)))
        })
        .transpose()
        .map(Option::flatten)
}

fn parse_date(date: &DateConfig, node: &ElementRef<'_>) -> Option<OffsetDateTime> {
    (node.value().name() == "time")
        .then(|| node.value().attr("datetime"))
        .flatten()
        .and_then(|datetime| {
            debug!("trying datetime attribute");
            date.parse(trim_date(datetime)).ok()
        })
        .inspect(|_x| {
            debug!("using datetime attribute");
        })
        .or_else(|| {
            let text = link_text(node);
            let text = trim_date(&text);
            date.parse(text)
                .map_err(|_err| {
                    warn!("unable to parse date '{text}'");
                })
                .ok()
        })
}

// Trim non-alphanumeric chars from either side of the string
fn trim_date(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_alphanumeric())
}

fn link_text(node: &ElementRef<'_>) -> String {
    node.text().collect()
}

fn select_first_including_self<'a>(
    element: ElementRef<'a>,
    selector: &Selector,
) -> Option<ElementRef<'a>> {
    if selector.matches(&element) {
        Some(element)
    } else {
        element.select(selector).next()
    }
}

fn select_including_self<'a>(element: ElementRef<'a>, selector: &Selector) -> Vec<ElementRef<'a>> {
    let mut elements = Vec::new();
    if selector.matches(&element) {
        elements.push(element);
    }
    elements.extend(element.select(selector));
    elements
}

fn extract_description(
    config: &FeedConfig,
    item: &ElementRef<'_>,
    title: &str,
    base_url: &url::ParseOptions,
) -> eyre::Result<Option<String>> {
    let mut description = String::new();

    for selector in &config.summary {
        let selector = Selector::parse(selector)
            .map_err(|_| {
                warn!(
                    "summary selector '{selector}' for item with title '{}' is invalid",
                    title.trim()
                );
            })
            .ok();
        let Some(selector) = selector else {
            continue;
        };

        for node in select_including_self(*item, &selector) {
            description.push_str(&rewrite_hrefs_in_html(&node.html(), base_url)?);
        }
    }

    if description.is_empty() {
        Ok(None)
    } else {
        Ok(Some(description))
    }
}

#[expect(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::{env, process};

    use reqwest::Client as HttpClient;

    use super::*;

    const HTML: &str = include_str!("../tests/local.html");

    struct RmOnDrop(PathBuf);

    impl RmOnDrop {
        fn new(path: PathBuf) -> Self {
            RmOnDrop(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for RmOnDrop {
        fn drop(&mut self) {
            fs::remove_file(&self.0).expect("failed to remove temporary file");
        }
    }

    fn test_config() -> FeedConfig {
        FeedConfig {
            url: String::new(),
            item: String::new(),
            heading: String::new(),
            link: None,
            summary: Vec::new(),
            date: None,
            media: None,
        }
    }

    fn test_date(format: &str) -> DateConfig {
        test_date_with_selector("", format)
    }

    fn test_date_with_selector(selector: &str, format: &str) -> DateConfig {
        toml::from_str(&format!("selector = {selector:?}\nformat = {format:?}")).unwrap()
    }

    const HUNGARIAN_FORMAT: &str =
        "[year]. [month]. [day]. [hour]:[minute][end trailing_input:discard]";

    #[test]
    fn test_date_hungarian_style() {
        let date = test_date(HUNGARIAN_FORMAT);
        assert!(date.parse("2024. 03. 15. 09:45").is_ok());
        assert!(date.parse("2024. 03. 15. 09:45 some trailing text").is_ok());
        assert!(date.parse("1999. 12. 31. 23:59").is_ok());
    }

    #[test]
    fn test_process_local_html_hungarian_date() {
        let html = r#"<html><body>
            <article class="post">
                <h2><a href="/post/1">First Post</a></h2>
                <span class="date">2024. 03. 15. 09:45</span>
            </article>
            <article class="post">
                <h2><a href="/post/2">Second Post</a></h2>
                <span class="date">1999. 12. 31. 23:59 - trailing content</span>
            </article>
        </body></html>"#;

        let html_file_name = format!("rsspls.local.hungarian.{}.html", process::id());
        let local_html = RmOnDrop::new(env::temp_dir().join(&html_file_name));
        fs::write(local_html.path(), html.as_bytes()).expect("unable to write test HTML");

        let url = Url::from_file_path(local_html.path())
            .expect("unable to construct file URL for test HTML");

        let client = Client {
            file_urls: true,
            http: HttpClient::new(),
        };

        let config = FeedConfig {
            url: url.to_string(),
            item: "article.post".to_string(),
            heading: "h2".to_string(),
            link: Some("a".to_string()),
            date: Some(test_date_with_selector(".date", HUNGARIAN_FORMAT)),
            ..test_config()
        };
        let channel_config = ChannelConfig {
            title: "Site".to_string(),
            filename: Path::new(&html_file_name)
                .with_extension("rss")
                .to_string_lossy()
                .into_owned(),
            user_agent: None,
            post_update_hook: None,
            config,
        };
        let config_hash = ConfigHash(&html_file_name);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let res = runtime
            .block_on(process_feed(&client, &channel_config, config_hash, None))
            .expect("unable to process local feed");

        let ProcessResult::Ok { channel, .. } = res else {
            panic!("expected ProcessResult::Ok but got: {res:?}")
        };

        assert_eq!(channel.items().len(), 2);
        assert_eq!(channel.items()[0].title, Some("First Post".to_string()));
        assert_eq!(
            channel.items()[0].pub_date,
            Some("Fri, 15 Mar 2024 09:45:00 +0000".to_string())
        );
        assert_eq!(channel.items()[1].title, Some("Second Post".to_string()));
        assert_eq!(
            channel.items()[1].pub_date,
            Some("Fri, 31 Dec 1999 23:59:00 +0000".to_string())
        );
    }

    #[test]
    fn test_trim_date() {
        assert_eq!(trim_date("2021-05-20 —"), "2021-05-20");
        assert_eq!(
            trim_date("2022-04-20T06:38:27+10:00"),
            "2022-04-20T06:38:27+10:00"
        );
    }

    #[test]
    fn test_rewrite_hrefs_in_html() {
        let html = r#"<html><body><a href="/cool">cool thing</a> <div href="dont-do-this">ok</div><a href="http://example.com">example</a></body></html>"#;
        let expected = r#"<html><body><a href="http://example.com/cool">cool thing</a> <div href="http://example.com/dont-do-this">ok</div><a href="http://example.com/">example</a></body></html>"#;
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));
        let rewritten = rewrite_hrefs_in_html(html, &base).unwrap();
        assert_eq!(rewritten, expected);
    }

    #[test]
    fn test_process_item_normalizes_link_and_guid() {
        let html = r#"<html><body><article class="post"><h2><a href="/post/1">First Post</a></h2></article></body></html>"#;
        let doc = Html::parse_document(html);
        let item_selector = Selector::parse("article.post").unwrap();
        let item = doc.select(&item_selector).next().unwrap();
        let config = FeedConfig {
            heading: "h2".to_string(),
            link: Some("a".to_string()),
            ..test_config()
        };
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let rss_item = process_item(&config, item, "a", &base).unwrap();

        assert_eq!(rss_item.link.as_deref(), Some("http://example.com/post/1"));
        assert_eq!(
            rss_item.guid.as_ref().map(rss::Guid::value),
            Some("http://example.com/post/1")
        );
    }

    #[test]
    fn test_extract_description_rewrites_hrefs_to_absolute_urls() {
        let html = r#"<html><body><article class="item"><p class="summary">Read <a href="/more">more</a></p></article></body></html>"#;
        let doc = Html::parse_document(html);
        let item_selector = Selector::parse(".item").unwrap();
        let item = doc.select(&item_selector).next().unwrap();
        let config = FeedConfig {
            summary: vec![".summary".to_string()],
            ..test_config()
        };
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let description = extract_description(&config, &item, "title", &base)
            .unwrap()
            .unwrap();

        assert_eq!(
            description,
            r#"<p class="summary">Read <a href="http://example.com/more">more</a></p>"#
        );
    }

    #[test]
    fn test_extract_description_leaves_src_attributes_unchanged() {
        let html = r#"<html><body><article class="item"><p class="summary"><img src="/image.jpg"><a href="/more">more</a></p></article></body></html>"#;
        let doc = Html::parse_document(html);
        let item_selector = Selector::parse(".item").unwrap();
        let item = doc.select(&item_selector).next().unwrap();
        let config = FeedConfig {
            summary: vec![".summary".to_string()],
            ..test_config()
        };
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let description = extract_description(&config, &item, "title", &base)
            .unwrap()
            .unwrap();

        assert_eq!(
            description,
            r#"<p class="summary"><img src="/image.jpg"><a href="http://example.com/more">more</a></p>"#
        );
    }

    #[test]
    fn test_rewrite_hrefs_in_html_leaves_invalid_hrefs_unchanged() {
        let html = r#"<p><a href="http://[::1">broken</a></p>"#;
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let rewritten = rewrite_hrefs_in_html(html, &base).unwrap();

        assert_eq!(rewritten, html);
    }

    #[test]
    fn test_extract_description_multi() {
        // Test CSS selector for description that matches multiple elements
        let html = r#"<html><body><div class="item"><p>one</p><span>two</span></body></html>"#;
        let doc = Html::parse_document(html);
        let item_selector = Selector::parse(".item").unwrap();
        let item = doc.select(&item_selector).next().unwrap();
        let config = FeedConfig {
            summary: vec!["span, p".to_string()],
            ..test_config()
        };
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let description = extract_description(&config, &item, "title", &base)
            .unwrap()
            .unwrap();

        // Items come out in DOM order
        assert_eq!(description, "<p>one</p><span>two</span>");
    }

    #[test]
    fn test_extract_description_array() {
        // Test CSS selector for description that matches multiple elements
        let html = r#"<html><body><div class="item"><p>one</p><span>two</span></body></html>"#;
        let doc = Html::parse_document(html);
        let item_selector = Selector::parse(".item").unwrap();
        let item = doc.select(&item_selector).next().unwrap();
        let config = FeedConfig {
            summary: vec!["span".to_string(), "p".to_string()],
            ..test_config()
        };
        let base_url = "http://example.com".parse().unwrap();
        let base = Url::options().base_url(Some(&base_url));

        let description = extract_description(&config, &item, "title", &base)
            .unwrap()
            .unwrap();

        // Items come out in the order of the selector array
        assert_eq!(description, "<span>two</span><p>one</p>");
    }

    #[test]
    fn test_advanced_css_selectors() {
        let html = r#"<html><body>
            <div id="d1"><h1><a href="/title-1">Title 1</a></h1></div>
            <div id="d2"><p>Only paragraph</p></div>
            <div id="d3"></div>
            <div id="d4"><h1><a href="/title-2">Title 2</a></h1></div>
            <div id="d5"><span>No title</span></div>
        </body></html>"#;

        let html_file_name = format!("rsspls.advanced-css.{}.html", process::id());
        let local_html = RmOnDrop::new(env::temp_dir().join(&html_file_name));
        fs::write(local_html.path(), html.as_bytes()).expect("unable to write test HTML");

        let url = Url::from_file_path(local_html.path())
            .expect("unable to construct file URL for test HTML");

        let client = Client {
            file_urls: true,
            http: HttpClient::new(),
        };

        let config = FeedConfig {
            url: url.to_string(),
            item: "div:has(h1)".to_string(),
            heading: "h1".to_string(),
            link: Some("h1 a".to_string()),
            ..test_config()
        };
        let channel_config = ChannelConfig {
            title: "Advanced CSS".to_string(),
            filename: Path::new(&html_file_name)
                .with_extension("rss")
                .to_string_lossy()
                .into_owned(),
            user_agent: None,
            post_update_hook: None,
            config,
        };
        let config_hash = ConfigHash(&html_file_name);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let res = runtime
            .block_on(process_feed(&client, &channel_config, config_hash, None))
            .expect("unable to process local feed");

        let ProcessResult::Ok { channel, .. } = res else {
            panic!("expected ProcessResult::Ok but got: {res:?}")
        };

        assert_eq!(channel.items().len(), 2);
    }

    #[test]
    fn test_process_local_html() {
        let html_file_name = format!("rsspls.local.{}.html", process::id());
        let local_html = RmOnDrop::new(env::temp_dir().join(&html_file_name));
        fs::write(local_html.path(), HTML.as_bytes()).expect("unable to write test HTML");

        let url = Url::from_file_path(local_html.path())
            .expect("unable to construct file URL for test HTML");

        let client = Client {
            file_urls: true,
            http: HttpClient::new(),
        };

        let config = FeedConfig {
            url: url.to_string(),
            item: "nav a".to_string(),
            heading: "a".to_string(),
            ..test_config()
        };
        let channel_config = ChannelConfig {
            title: "Local Site".to_string(),
            filename: Path::new(&html_file_name)
                .with_extension("rss")
                .to_string_lossy()
                .into_owned(),
            user_agent: None,
            post_update_hook: None,
            config,
        };
        let config_hash = ConfigHash(&html_file_name);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let res = runtime
            .block_on(process_feed(&client, &channel_config, config_hash, None))
            .expect("unable to process local feed");

        let ProcessResult::Ok { channel, .. } = res else {
            panic!("expected ProcessResult::Ok but got: {res:?}")
        };

        assert_eq!(channel.items().len(), 5);
        assert_eq!(channel.items()[0].title, Some("Install".to_string()));
    }

    #[test]
    fn test_process_local_files_disabled() {
        let html_file_name = "rsspls.local.html";
        let local_html = env::temp_dir().join(html_file_name);
        let url =
            Url::from_file_path(&local_html).expect("unable to construct file URL for test HTML");

        let client = Client {
            file_urls: false,
            http: HttpClient::new(),
        };

        let config = FeedConfig {
            url: url.to_string(),
            item: "nav a".to_string(),
            heading: "a".to_string(),
            ..test_config()
        };
        let channel_config = ChannelConfig {
            title: "Local Site".to_string(),
            filename: Path::new(&html_file_name)
                .with_extension("rss")
                .to_string_lossy()
                .into_owned(),
            post_update_hook: None,
            user_agent: None,
            config,
        };
        let config_hash = ConfigHash(html_file_name);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let res = runtime.block_on(process_feed(&client, &channel_config, config_hash, None));

        let Err(err) = res else {
            panic!("expected error, got: {res:?}")
        };

        assert!(
            err.to_string()
                .contains("file URLs are not enabled in config")
        );
    }
}
