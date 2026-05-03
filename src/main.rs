mod cache;
mod cli;
mod config;
mod dirs;
mod feed;

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs};

use atomicwrites::AtomicFile;
use eyre::{Report, WrapErr, eyre};
use futures::future;
use log::{LevelFilter, debug, error, info, warn};
use reqwest::Client as HttpClient;
use rss::Channel;
use simple_eyre::eyre;

use crate::cache::deserialise_cached_headers;
use crate::config::ConfigHash;
use crate::config::{ChannelConfig, Config};

use crate::feed::{ProcessResult, process_feed};

const RSSPLS_LOG: &str = "RSSPLS_LOG";

#[derive(Clone)]
pub struct Client {
    /// Whether file URLs are enabled
    file_urls: bool,
    /// HTTP client
    http: HttpClient,
}

#[tokio::main]
async fn main() -> ExitCode {
    match try_main().await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(report) => {
            error!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

async fn try_main() -> eyre::Result<bool> {
    simple_eyre::install()?;
    let mut logger = pretty_env_logger::formatted_builder();
    logger.filter_level(LevelFilter::Info);
    logger.parse_env(RSSPLS_LOG);
    logger.try_init()?;

    let cli = cli::parse_args().wrap_err("unable to parse CLI arguments")?;
    let Some(cli) = cli else {
        // Help or version info was printed and we should return
        return Ok(true);
    };

    let config = Config::read(cli.config_path)?;

    // Determine output directory
    let configured_output_dir = config
        .rsspls
        .output
        .as_ref()
        .map(|path| {
            dirs::home_dir()
                .ok_or_else(|| eyre!("unable to determine home directory"))
                .map(|home| expand_tilde(path, home))
        })
        .transpose()?;

    let output_dir = cli
        .output_path
        .or(configured_output_dir)
        .or_else(dirs::default_output_dir)
        .ok_or_else(|| {
            eyre!(
                "output directory must be supplied via --output, be present in configuration file, or RSSPLS_HOME must be set"
            )
        })?;

    // Ensure output directory exists
    if !output_dir.exists() {
        fs::create_dir_all(&output_dir).wrap_err_with(|| {
            format!(
                "unable to create output directory: {}",
                output_dir.display()
            )
        })?;
        info!("created output directory: {}", output_dir.display());
    }

    // Set up the HTTP client
    let connect_timeout = Duration::from_secs(10);
    let timeout = Duration::from_secs(30);
    let mut client_builder = HttpClient::builder()
        .connect_timeout(connect_timeout)
        .timeout(timeout);

    // Add proxy if provided
    if let Some(proxy) = config.rsspls.proxy {
        debug!("using proxy from configuration file: {proxy}");
        client_builder = client_builder.proxy(reqwest::Proxy::all(proxy)?);
    } else {
        if let Ok(proxy) = env::var("http_proxy") {
            debug!("using http proxy from 'http_proxy' env var: {proxy}");
            client_builder = client_builder.proxy(reqwest::Proxy::http(proxy)?);
        }
        if let Ok(proxy) = env::var("HTTPS_PROXY") {
            debug!("using https proxy from 'HTTPS_PROXY' env var: {proxy}");
            client_builder = client_builder.proxy(reqwest::Proxy::https(proxy)?);
        }
    }

    // Disable certificate verification if requested
    if config.rsspls.insecure_disable_certificate_verification {
        client_builder = client_builder.danger_accept_invalid_certs(true);
    }

    let client = Client {
        file_urls: config.rsspls.file_urls,
        http: client_builder
            .build()
            .wrap_err("unable to build HTTP client")?,
    };

    // Spawn the tasks
    let config_hash = Arc::new(config.hash.clone());
    let futures = config.feed.into_iter().map(|feed| {
        let client = client.clone(); // Client uses Arc internally
        let output_dir = output_dir.clone();
        let config_hash = Arc::clone(&config_hash);
        tokio::spawn(async move {
            let res = process(&feed, &client, ConfigHash(config_hash.as_str()), output_dir).await;
            if let Err(ref report) = res {
                // Eat errors when processing feeds so that we don't stop processing the others.
                // Errors are reported, then we return a boolean indicating success or not, which
                // is used to set the exit status of the program later.
                error!("{report:?}");
            }
            res.is_ok()
        })
    });

    // Run all the futures at the same time
    // The ? here will fail on an error if the JoinHandle fails
    let ok = future::try_join_all(futures)
        .await?
        .into_iter()
        .fold(true, |ok, succeeded| ok & succeeded);

    Ok(ok)
}

async fn process(
    feed: &ChannelConfig,
    client: &Client,
    config_hash: ConfigHash<'_>,
    output_dir: PathBuf,
) -> Result<(), Report> {
    // Generate paths up front so we report any errors before making requests
    let filename = Path::new(&feed.filename);
    let filename = filename
        .file_name()
        .map(Path::new)
        .ok_or_else(|| eyre!("{} is not a valid file name", filename.display()))?;
    let output_path = output_dir.join(filename);
    let cache_filename = filename.with_extension("toml");
    let cache_path =
        dirs::place_cache_file(&cache_filename).wrap_err("unable to create path to cache file")?;
    let cached_headers = deserialise_cached_headers(&cache_path, config_hash);

    let res = process_feed(client, feed, config_hash, cached_headers.as_ref())
        .await
        .wrap_err_with(|| format!("error processing feed for {}", feed.config.url))?;

    match res {
        ProcessResult::NotModified => Ok(()),
        ProcessResult::Ok { channel, headers } => {
            // TODO: channel.validate()
            write_channel(&channel, &output_path).wrap_err_with(|| {
                format!("unable to write output file: {}", output_path.display())
            })?;

            // Update the cache
            if let Some(headers) = headers {
                debug!("write cache {}", cache_path.display());
                fs::write(cache_path, headers).wrap_err("unable to write to cache")?;
            }

            run_hook(&feed.post_update_hook, &output_path).await?;
            Ok(())
        }
    }
}

async fn run_hook(hook: &[String], feed_path: &Path) -> eyre::Result<()> {
    if hook.is_empty() {
        return Ok(());
    }

    let cmd = &hook[0];
    let args = &hook[1..];

    info!(
        "running post-update hook: {} for {}",
        cmd,
        feed_path.display()
    );

    let output = tokio::process::Command::new(cmd)
        .args(args)
        .env("RSSPLS_FEED_FILE", feed_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .wrap_err_with(|| format!("failed to execute post-update hook: {cmd}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "post-update hook for '{}' exited with non-zero status.\ncmd: {}, status: {}\nstdout: {}\nstderr: {}",
            feed_path.display(),
            cmd,
            output.status,
            stdout,
            stderr,
        );
        return Err(eyre!(
            "post-update hook failed for '{}': command '{}' exited with status {}",
            feed_path.display(),
            cmd,
            output.status,
        ));
    }
    Ok(())
}

fn write_channel(channel: &Channel, output_path: &Path) -> Result<(), Report> {
    // Write the new file into a temporary location, then move it into place
    let file = AtomicFile::new(output_path, atomicwrites::AllowOverwrite);
    file.write(|f| {
        info!("write {}", output_path.display());
        channel
            .write_to(f)
            .map(drop)
            .wrap_err("unable to write feed")
    })
    .map_err(|err| match err {
        atomicwrites::Error::Internal(atomic_err) => atomic_err.into(),
        atomicwrites::Error::User(myerr) => myerr,
    })
}

#[must_use]
pub fn version_string() -> &'static str {
    concat!(
        env!("CARGO_PKG_NAME"),
        " version ",
        env!("CARGO_PKG_VERSION")
    )
}

const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn expand_tilde<P: Into<PathBuf>>(path: P, mut home: PathBuf) -> PathBuf {
    let path = path.into();

    // NOTE: starts_with only considers whole path components
    if path.starts_with("~") {
        if path == Path::new("~") {
            home
        } else {
            home.push(path.strip_prefix("~").unwrap());
            home
        }
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn test_home() {
        let expanded = expand_tilde("asdf", PathBuf::from("/home/foo"));
        assert_eq!(expanded, Path::new("asdf"));

        let expanded = expand_tilde("~asdf", PathBuf::from("/home/foo"));
        assert_eq!(expanded, Path::new("~asdf"));

        let expanded = expand_tilde("~/some/where", PathBuf::from("/home/foo"));
        assert_eq!(expanded, Path::new("/home/foo/some/where"));

        let expanded = expand_tilde("~/some/where", PathBuf::from("/"));
        assert_eq!(expanded, Path::new("/some/where"));
    }

    #[test]
    #[cfg(windows)]
    fn test_home_windows() {
        let expanded = expand_tilde("asdf", PathBuf::from(r"C:\Users\Foo"));
        assert_eq!(expanded, Path::new("asdf"));

        let expanded = expand_tilde("~asdf", PathBuf::from(r"C:\Users\Foo"));
        assert_eq!(expanded, Path::new("~asdf"));

        let expanded = expand_tilde(r"~\some\where", PathBuf::from(r"C:\Users\Foo"));
        assert_eq!(expanded, Path::new(r"C:\Users\Foo\some\where"));

        let expanded = expand_tilde(r"~\some\where", PathBuf::from(r"C:\"));
        assert_eq!(expanded, Path::new(r"C:\some\where"));
    }

    #[tokio::test]
    async fn test_run_hook() {
        let temp = env::temp_dir();
        let marker = temp.join("hook_marker");
        let feed = temp.join("feed.xml");

        #[cfg(not(windows))]
        let hook = vec![
            "sh".into(),
            "-c".into(),
            format!("echo $RSSPLS_FEED_FILE > {}", marker.display()),
        ];
        #[cfg(windows)]
        let hook = vec![
            "cmd".into(),
            "/c".into(),
            format!("echo %RSSPLS_FEED_FILE% > {}", marker.display()),
        ];

        run_hook(&hook, &feed).await.unwrap();
        assert!(
            fs::read_to_string(&marker)
                .unwrap()
                .contains(feed.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn test_process_triggers_hook() {
        let temp = env::temp_dir().join(format!("rsspls_test_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        let marker = temp.join("hook_run");
        let html = temp.join("t.html");
        fs::write(&html, "<h2>t</h2>").unwrap();

        #[cfg(not(windows))]
        let hook = vec!["touch".into(), marker.to_str().unwrap().into()];
        #[cfg(windows)]
        let hook = vec![
            "cmd".into(),
            "/c".into(),
            format!("echo. > {}", marker.display()),
        ];

        let feed = ChannelConfig {
            title: "T".into(),
            filename: "t.xml".into(),
            user_agent: None,
            post_update_hook: hook,
            config: crate::config::FeedConfig {
                url: url::Url::from_file_path(&html).unwrap().into(),
                item: "h2".into(),
                heading: "h2".into(),
                link: None,
                summary: vec![],
                date: None,
                media: None,
            },
        };

        let client = Client {
            file_urls: true,
            http: HttpClient::new(),
        };
        process(&feed, &client, ConfigHash("hash"), temp)
            .await
            .unwrap();
        assert!(marker.exists());
    }

    #[tokio::test]
    async fn test_run_hook_returns_err_on_non_zero_exit() {
        let feed = env::temp_dir().join("feed.xml");

        #[cfg(not(windows))]
        let hook = vec!["sh".into(), "-c".into(), "exit 7".into()];
        #[cfg(windows)]
        let hook = vec!["cmd".into(), "/c".into(), "exit /b 7".into()];

        let err = run_hook(&hook, &feed).await.unwrap_err();
        assert!(err.to_string().contains("post-update hook failed"));
    }
}
