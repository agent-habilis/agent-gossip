use std::fmt;
use std::str::FromStr;
use std::sync::Once;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::protocol::SwarmId;
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

/// What a join accepts: a literal swarm id, or a domain / git-repo URL
/// whose `/.well-known/agent-habilis-swarm` names one. The three accepted
/// input forms are classified and syntactically validated **once**, at the
/// boundary (clap `FromStr` / MCP entry), so `resolve` matches on the
/// variant instead of re-sniffing a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTarget {
    /// A literal `🐝…` id — resolves with no I/O.
    Swarm(SwarmId),
    /// A domain or git-repo URL; carries the resolved well-known URL.
    WellKnown(String),
}

/// A join target that isn't a swarm id and isn't a well-formed
/// domain/git-repo reference (e.g. a malformed git path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTargetError(String);

impl fmt::Display for JoinTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JoinTargetError {}

impl FromStr for JoinTarget {
    type Err = JoinTargetError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        // A literal `🐝…` id is the no-I/O case. Shallow `SwarmId`
        // validation here; the full structural decode happens in `resolve`.
        if let Ok(id) = trimmed.parse::<SwarmId>() {
            return Ok(JoinTarget::Swarm(id));
        }
        // Otherwise it's a domain or git-repo URL: classify + validate the
        // form now (provider prefixes, segment counts) and carry the
        // resolved well-known URL.
        resolve_url(trimmed)
            .map(JoinTarget::WellKnown)
            .map_err(|error| JoinTargetError(error.to_string()))
    }
}

pub(crate) async fn resolve(target: &JoinTarget) -> Result<Swarm> {
    match target {
        JoinTarget::Swarm(id) => id
            .as_str()
            .parse::<Swarm>()
            .map_err(|error| anyhow!("invalid swarm id: {error}")),
        JoinTarget::WellKnown(url) => fetch_and_parse(url).await,
    }
}

async fn fetch_and_parse(url: &str) -> Result<Swarm> {
    ensure_ring_provider();
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        // Do not follow redirects. The well-known endpoint is expected to
        // serve the JSON directly over HTTPS; following redirects would turn a
        // join target into an SSRF primitive (a hostile/compromised domain
        // could 30x-redirect the fetch to an internal address such as the
        // cloud metadata endpoint). A 3xx is surfaced as a failed fetch.
        .redirect(reqwest::redirect::Policy::none())
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{JoinTarget, MAX_BODY_BYTES, fetch_and_parse, resolve, resolve_url};

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
        use crate::protocol::swarm::{Swarm, SwarmConfig, SwarmName};
        Swarm::new(
            [1u8; 32],
            SwarmName::new("test").unwrap(),
            SwarmConfig::loopback(),
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
    async fn fetch_and_parse_does_not_follow_redirects() {
        // SSRF guard: a hostile well-known endpoint 302-redirects to another
        // host. With redirects disabled the fetch fails (3xx is not success)
        // and the redirect target is never contacted.
        let evil = MockServer::start().await;
        let id = known_swarm_id();
        // Would serve a valid swarm if reached — `.expect(0)` asserts it is not
        // (verified when `evil` drops at end of scope).
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(format!(r#"{{"as.swarm":"{id}"}}"#)),
            )
            .expect(0)
            .mount(&evil)
            .await;

        let redirector = MockServer::start().await;
        let evil_target = format!("{}/.well-known/agent-habilis-swarm", evil.uri());
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-habilis-swarm"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", evil_target.as_str()),
            )
            .mount(&redirector)
            .await;

        let url = format!("{}/.well-known/agent-habilis-swarm", redirector.uri());
        assert!(
            fetch_and_parse(&url).await.is_err(),
            "a redirect must not be followed"
        );
    }

    #[tokio::test]
    async fn resolve_passthrough_for_valid_swarm_id() {
        let id = known_swarm_id();
        let target: JoinTarget = id.parse().unwrap();
        let swarm = resolve(&target).await.unwrap();
        assert_eq!(swarm.to_string(), id);
    }

    #[test]
    fn join_target_classifies_inputs() {
        // A literal `🐝…` id ⇒ Swarm (no I/O to resolve).
        let id = known_swarm_id();
        assert!(matches!(id.parse::<JoinTarget>(), Ok(JoinTarget::Swarm(_))));
        // A bare domain ⇒ WellKnown carrying the well-known URL.
        assert_eq!(
            "example.com".parse::<JoinTarget>(),
            Ok(JoinTarget::WellKnown(
                "https://example.com/.well-known/agent-habilis-swarm".to_owned()
            ))
        );
        // A git-repo URL ⇒ WellKnown carrying the raw-file URL.
        assert_eq!(
            "github.com/alice/proj".parse::<JoinTarget>(),
            Ok(JoinTarget::WellKnown(
                "https://raw.githubusercontent.com/alice/proj/HEAD/.well-known/agent-habilis-swarm"
                    .to_owned()
            ))
        );
        // A malformed git path is rejected at parse, not mid-resolve.
        assert!("github.com/alice".parse::<JoinTarget>().is_err());
    }
}
