use std::sync::Once;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::protocol::swarm::Swarm;

/// `rustls-no-provider` ships no crypto backend, so a default
/// `CryptoProvider` must be installed before the first handshake. We use
/// ring (matches iroh's `tls-ring` pin). Idempotent: iroh may also install
/// one and ordering isn't guaranteed, so `Once` + ignore the
/// already-installed error.
fn ensure_ring_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

const WELL_KNOWN_PATH: &str = "/.well-known/agent-habilis-swarm";
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY_BYTES: usize = 64 * 1024;
const USER_AGENT: &str = concat!("agent-habilis-swarm/", env!("CARGO_PKG_VERSION"));

/// Prefix a user input can start with, and a template to turn the remaining
/// path into a raw-file URL. `{path}` in the template is replaced with the
/// substring after the prefix. Order matters: specific prefixes first.
struct Provider {
    prefix: &'static str,
    template: &'static str,
    /// Minimum `/`-separated segments required after the prefix (e.g. 2 for
    /// `owner/repo`). Exceeding this is rejected to avoid silent
    /// misclassification.
    min_segments: usize,
    max_segments: Option<usize>,
}

const PROVIDERS: &[Provider] = &[
    Provider {
        prefix: "github.com/",
        template: "https://raw.githubusercontent.com/{path}/HEAD/.well-known/agent-habilis-swarm",
        min_segments: 2,
        max_segments: Some(2),
    },
    Provider {
        prefix: "gitlab.com/",
        template: "https://gitlab.com/{path}/-/raw/HEAD/.well-known/agent-habilis-swarm",
        min_segments: 2,
        max_segments: None,
    },
    Provider {
        prefix: "bitbucket.org/",
        template: "https://bitbucket.org/{path}/raw/HEAD/.well-known/agent-habilis-swarm",
        min_segments: 2,
        max_segments: Some(2),
    },
];

pub(crate) fn resolve_url(arg: &str) -> Result<String> {
    let trimmed = arg.trim();
    let stripped = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed)
        .trim_end_matches('/');

    for provider in PROVIDERS {
        if let Some(rest) = stripped.strip_prefix(provider.prefix) {
            let segments = rest
                .split('/')
                .filter(|segment| !segment.is_empty())
                .count();
            if segments < provider.min_segments
                || provider.max_segments.is_some_and(|max| segments > max)
            {
                bail!(
                    "invalid {} path '{}': expected {} segments",
                    provider.prefix.trim_end_matches('/'),
                    rest,
                    describe_segments(provider),
                );
            }
            return Ok(provider.template.replace("{path}", rest));
        }
    }
    Ok(format!("https://{stripped}{WELL_KNOWN_PATH}"))
}

fn describe_segments(provider: &Provider) -> String {
    match provider.max_segments {
        Some(max) if max == provider.min_segments => format!("exactly {}", provider.min_segments),
        Some(max) => format!("{}-{}", provider.min_segments, max),
        None => format!("at least {}", provider.min_segments),
    }
}

#[derive(Debug, Deserialize)]
struct WellKnown {
    #[serde(rename = "as.swarm")]
    swarm: String,
}

pub(crate) async fn resolve(arg: &str) -> Result<Swarm> {
    if let Ok(swarm) = arg.parse::<Swarm>() {
        return Ok(swarm);
    }
    let url = resolve_url(arg)?;
    fetch_and_parse(&url).await
}

async fn fetch_and_parse(url: &str) -> Result<Swarm> {
    ensure_ring_provider();
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .context("build http client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "well-known fetch failed: HTTP {} from {}",
            resp.status(),
            url
        );
    }
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        bail!("well-known response too large: {len} bytes from {url} (limit {MAX_BODY_BYTES})");
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading well-known body from {url}"))?;
    if bytes.len() > MAX_BODY_BYTES {
        bail!(
            "well-known response too large: {} bytes from {} (limit {})",
            bytes.len(),
            url,
            MAX_BODY_BYTES
        );
    }
    let body: WellKnown = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing well-known JSON from {url}"))?;
    body.swarm
        .parse::<Swarm>()
        .map_err(|error| anyhow!("invalid swarm id in well-known at {url}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn resolve_url_maps_inputs_to_expected_urls() {
        let cases = [
            (
                "github.com/alice/proj",
                "https://raw.githubusercontent.com/alice/proj/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "https://github.com/alice/proj",
                "https://raw.githubusercontent.com/alice/proj/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "https://github.com/alice/proj/",
                "https://raw.githubusercontent.com/alice/proj/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "gitlab.com/alice/proj",
                "https://gitlab.com/alice/proj/-/raw/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "gitlab.com/grp/sub/repo",
                "https://gitlab.com/grp/sub/repo/-/raw/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "bitbucket.org/alice/proj",
                "https://bitbucket.org/alice/proj/raw/HEAD/.well-known/agent-habilis-swarm",
            ),
            (
                "example.com",
                "https://example.com/.well-known/agent-habilis-swarm",
            ),
            (
                "https://example.com/",
                "https://example.com/.well-known/agent-habilis-swarm",
            ),
            (
                "http://example.com",
                "https://example.com/.well-known/agent-habilis-swarm",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(resolve_url(input).unwrap(), expected, "input: {input}");
        }
    }

    #[test]
    fn resolve_url_rejects_malformed_git_paths() {
        assert!(resolve_url("github.com/alice").is_err());
        assert!(resolve_url("github.com/alice/proj/extra").is_err());
        assert!(resolve_url("gitlab.com/alice").is_err());
    }

    fn known_swarm_id() -> String {
        use crate::protocol::swarm::{Swarm, SwarmMode, SwarmName};
        Swarm::new(
            SwarmMode::Private,
            [1u8; 32],
            SwarmName::new("test").unwrap(),
        )
        .to_string()
    }

    async fn mock_well_known(body: &str, status: u16) -> (MockServer, String) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-habilis-swarm"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        let url = format!("{}/.well-known/agent-habilis-swarm", server.uri());
        (server, url)
    }

    #[tokio::test]
    async fn fetch_and_parse_valid_json() {
        let id = known_swarm_id();
        let (_s, url) = mock_well_known(&format!(r#"{{"as.swarm":"{id}"}}"#), 200).await;
        let swarm = fetch_and_parse(&url).await.unwrap();
        assert_eq!(swarm.to_string(), id);
    }

    #[tokio::test]
    async fn fetch_and_parse_http_404() {
        let (_s, url) = mock_well_known("", 404).await;
        let err = fetch_and_parse(&url).await.unwrap_err();
        assert!(err.to_string().contains("404"), "got: {err}");
    }

    #[tokio::test]
    async fn fetch_and_parse_malformed_json() {
        let (_s, url) = mock_well_known("not json", 200).await;
        assert!(fetch_and_parse(&url).await.is_err());
    }

    #[tokio::test]
    async fn fetch_and_parse_missing_field() {
        let (_s, url) = mock_well_known(r#"{"other":"x"}"#, 200).await;
        assert!(fetch_and_parse(&url).await.is_err());
    }

    #[tokio::test]
    async fn fetch_and_parse_invalid_swarm_id() {
        let (_s, url) = mock_well_known(r#"{"as.swarm":"not-a-swarm"}"#, 200).await;
        let err = fetch_and_parse(&url).await.unwrap_err();
        assert!(err.to_string().contains("invalid swarm id"), "got: {err}");
    }

    #[tokio::test]
    async fn fetch_and_parse_rejects_oversized_body() {
        let big = "x".repeat(MAX_BODY_BYTES + 1);
        let (_s, url) = mock_well_known(&big, 200).await;
        let err = fetch_and_parse(&url).await.unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn resolve_passthrough_for_valid_swarm_id() {
        let id = known_swarm_id();
        let swarm = resolve(&id).await.unwrap();
        assert_eq!(swarm.to_string(), id);
    }
}
