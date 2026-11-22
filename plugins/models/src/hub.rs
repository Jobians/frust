//! A blocking Hugging Face Hub client: search, repository file listings and file URLs.
//!
//! Requests go through `ureq` on a TLS connector this module owns: rustls on the ring provider
//! with the bundled Mozilla roots, so no platform verifier is consulted on any target. The
//! access token is sent only to the Hub's own origin and never follows a redirect, so a CDN
//! never sees it. See the README's "Downloading" section.

use std::fmt;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use ureq::config::RedirectAuthHeaders;
use ureq::http::Response;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout,
    TcpConnector, Transport, TransportAdapter,
};
use ureq::{Agent, Body};

use crate::error::{HubError, ModelsError};

/// The public Hub.
pub const HUB_URL: &str = "https://huggingface.co";

const USER_AGENT: &str = concat!("frust-models/", env!("CARGO_PKG_VERSION"));
const JSON_LIMIT: u64 = 64 * 1024 * 1024;
const EXCERPT_LIMIT: usize = 256;
const MAX_TREE_PAGES: usize = 64;

/// Whether a repository asks users to accept terms before downloading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Gated {
    /// Open to everyone.
    #[default]
    No,
    /// Access is granted automatically once the terms are accepted.
    Auto,
    /// The owner approves each request.
    Manual,
}

/// Sort order of a search, always descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum HubSort {
    /// Most downloaded first.
    #[default]
    Downloads,
    /// Most liked first.
    Likes,
    /// Most recently modified first.
    LastModified,
    /// Trending first.
    Trending,
}

impl HubSort {
    fn as_query(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::LastModified => "lastModified",
            Self::Trending => "trendingScore",
        }
    }
}

/// What to search the Hub for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    /// Free-text query matched against repository ids.
    pub query: Option<String>,
    /// Restrict to one author or organisation.
    pub author: Option<String>,
    /// Restrict to a tag, for example `gguf` or `onnx`.
    pub filter: Option<String>,
    /// Maximum number of results.
    pub limit: u32,
    /// Result order.
    pub sort: HubSort,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            author: None,
            filter: None,
            limit: 20,
            sort: HubSort::Downloads,
        }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubModel {
    /// `author/repo`.
    pub id: String,
    /// Repository owner.
    pub author: Option<String>,
    /// Access gating.
    pub gated: Gated,
    /// Recent download count.
    pub downloads: u64,
    /// Like count.
    pub likes: u64,
    /// RFC 3339 time of the last commit.
    pub last_modified: Option<String>,
    /// Repository file paths.
    pub files: Vec<String>,
}

/// One file of a repository at a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubFile {
    /// Path inside the repository, `/`-separated.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 for files stored in LFS; small files carry none.
    pub sha256: Option<String>,
}

/// Blocking Hub client. Cheap to clone; clones share one connection pool.
#[derive(Clone)]
pub struct HubClient {
    agent: Agent,
    base_url: String,
    token: Option<String>,
}

impl fmt::Debug for HubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubClient")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl HubClient {
    /// A client for the public Hub without a token.
    pub fn new() -> Result<Self, ModelsError> {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(5)
            .redirect_auth_headers(RedirectAuthHeaders::Never)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .user_agent(USER_AGENT)
            .build();
        let connector =
            ().chain(ConnectProxyConnector::default())
                .chain(TcpConnector::default())
                .chain(RingTlsConnector::new()?);
        Ok(Self {
            agent: Agent::with_parts(config, connector, DefaultResolver::default()),
            base_url: HUB_URL.to_owned(),
            token: None,
        })
    }

    /// Uses `token` as a bearer token for requests to the Hub origin. It is never logged.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    /// Points the client at another Hub-compatible server, for example a local test server.
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_owned();
        self
    }

    /// The server this client talks to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Searches repositories. Returns one page of at most `options.limit` results.
    pub fn search(&self, options: &SearchOptions) -> Result<Vec<HubModel>, ModelsError> {
        let mut query = vec![
            ("sort", options.sort.as_query().to_owned()),
            ("direction", "-1".to_owned()),
            ("limit", options.limit.to_string()),
            ("full", "true".to_owned()),
        ];
        for (key, value) in [
            ("search", &options.query),
            ("author", &options.author),
            ("filter", &options.filter),
        ] {
            if let Some(value) = value {
                query.push((key, value.clone()));
            }
        }
        let url = format!("{}/api/models", self.base_url);
        let (body, _) = self.get_json(&url, &query)?;
        let raw: Vec<RawModel> = parse_json(&body)?;
        Ok(raw.into_iter().map(RawModel::into_model).collect())
    }

    /// Lists every file of `repo` at `revision`, following the listing's pagination.
    pub fn files(&self, repo: &str, revision: &str) -> Result<Vec<HubFile>, ModelsError> {
        check_repo(repo)?;
        let mut url = Some(format!(
            "{}/api/models/{repo}/tree/{}",
            self.base_url,
            encode_segment(revision)
        ));
        let mut query = vec![("recursive", "true".to_owned())];
        let mut files = Vec::new();
        let mut pages = 0;
        while let Some(next) = url.take() {
            pages += 1;
            if pages > MAX_TREE_PAGES {
                return Err(HubError::Json("file listing has too many pages".into()).into());
            }
            let (body, link) = self.get_json(&next, &query)?;
            query.clear();
            let raw: Vec<RawTreeEntry> = parse_json(&body)?;
            files.extend(raw.into_iter().filter_map(RawTreeEntry::into_file));
            url = link.as_deref().and_then(next_link);
        }
        Ok(files)
    }

    /// The download URL of `path` in `repo` at `revision`.
    pub fn resolve_url(&self, repo: &str, revision: &str, path: &str) -> String {
        let path = path
            .split('/')
            .map(encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "{}/{repo}/resolve/{}/{path}",
            self.base_url,
            encode_segment(revision)
        )
    }

    /// Whether the token may be sent to `url`: only to the origin of the base URL.
    fn may_authorize(&self, url: &str) -> bool {
        match (Origin::parse(url), Origin::parse(&self.base_url)) {
            (Some(target), Some(hub)) => target == hub,
            _ => false,
        }
    }

    /// Sends a GET to `url`, adding `Range` when `range_from` is set and the token when allowed.
    pub(crate) fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
        range_from: Option<u64>,
    ) -> Result<Response<Body>, HubError> {
        let mut request = self.agent.get(url);
        for (key, value) in query {
            request = request.query(*key, value);
        }
        if let Some(from) = range_from {
            request = request.header("Range", format!("bytes={from}-"));
        }
        if let Some(token) = &self.token
            && self.may_authorize(url)
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request
            .call()
            .map_err(|err| HubError::Transport(err.to_string()))
    }

    fn get_json(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<u8>, Option<String>), ModelsError> {
        let mut response = self.get(url, query, None)?;
        check_status(&mut response)?;
        let link = header(&response, "link");
        let body = response
            .body_mut()
            .with_config()
            .limit(JSON_LIMIT)
            .read_to_vec()
            .map_err(|err| HubError::Transport(err.to_string()))?;
        Ok((body, link))
    }
}

pub(crate) fn header(response: &Response<Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Maps a non-success status to its [`HubError`]. 2xx and 3xx pass.
pub(crate) fn check_status(response: &mut Response<Body>) -> Result<(), HubError> {
    let code = response.status().as_u16();
    if code < 400 {
        return Ok(());
    }
    Err(match code {
        401 => HubError::Unauthorized {
            gated: header(response, "x-error-code").as_deref() == Some("GatedRepo"),
        },
        403 => HubError::Forbidden,
        404 => HubError::NotFound,
        429 => HubError::RateLimited {
            retry_after: header(response, "retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs),
        },
        _ => HubError::Status {
            code,
            body_excerpt: excerpt(response),
        },
    })
}

fn excerpt(response: &mut Response<Body>) -> String {
    let mut bytes = Vec::with_capacity(EXCERPT_LIMIT);
    let _ = response
        .body_mut()
        .as_reader()
        .take(EXCERPT_LIMIT as u64)
        .read_to_end(&mut bytes);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    while text.len() > EXCERPT_LIMIT {
        text.pop();
    }
    text
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ModelsError> {
    serde_json::from_slice(body).map_err(|err| HubError::Json(err.to_string()).into())
}

/// The `rel="next"` target of a `Link` header.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| matches!(p.trim(), "rel=\"next\"" | "rel=next"));
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then(|| target.to_owned())
    })
}

fn check_repo(repo: &str) -> Result<(), ModelsError> {
    let valid_part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if valid_part(owner) && valid_part(name) => Ok(()),
        _ => Err(ModelsError::InvalidModelId(format!(
            "{repo:?}: repository id must be `author/repo`"
        ))),
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Scheme, host and port of a URL, compared to decide where the token may go.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    pub(crate) scheme: String,
    pub(crate) host: String,
    port: u16,
}

impl Origin {
    pub(crate) fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']')?;
            (host, after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        let default_port = match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        let port = match port {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    pub(crate) fn is_loopback(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }
}

#[derive(Deserialize)]
struct RawModel {
    id: String,
    author: Option<String>,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(rename = "lastModified")]
    last_modified: Option<String>,
    #[serde(default)]
    siblings: Vec<RawSibling>,
}

#[derive(Deserialize)]
struct RawSibling {
    rfilename: String,
}

impl RawModel {
    fn into_model(self) -> HubModel {
        let gated = match &self.gated {
            serde_json::Value::String(s) if s == "auto" => Gated::Auto,
            serde_json::Value::String(_) | serde_json::Value::Bool(true) => Gated::Manual,
            _ => Gated::No,
        };
        HubModel {
            author: self
                .author
                .or_else(|| self.id.split_once('/').map(|(a, _)| a.to_owned())),
            id: self.id,
            gated,
            downloads: self.downloads,
            likes: self.likes,
            last_modified: self.last_modified,
            files: self.siblings.into_iter().map(|s| s.rfilename).collect(),
        }
    }
}

#[derive(Deserialize)]
struct RawTreeEntry {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    path: String,
    #[serde(default)]
    size: u64,
    lfs: Option<RawLfs>,
}

#[derive(Deserialize)]
struct RawLfs {
    oid: String,
    size: Option<u64>,
}

impl RawTreeEntry {
    fn into_file(self) -> Option<HubFile> {
        if self.kind.as_deref().is_some_and(|k| k != "file") {
            return None;
        }
        let size = self.lfs.as_ref().and_then(|l| l.size).unwrap_or(self.size);
        Some(HubFile {
            path: self.path,
            size,
            sha256: self.lfs.map(|l| l.oid.to_ascii_lowercase()),
        })
    }
}

/// Wraps an https connection in rustls with this crate's own client config.
#[derive(Debug)]
struct RingTlsConnector {
    config: Arc<rustls::ClientConfig>,
}

impl RingTlsConnector {
    fn new() -> Result<Self, ModelsError> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|err| HubError::Transport(format!("tls config: {err}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            config: Arc::new(config),
        })
    }
}

impl<In: Transport> Connector<In> for RingTlsConnector {
    type Out = Either<In, RingTlsTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let Some(transport) = chained else {
            return Ok(None);
        };
        if !details.needs_tls() || transport.is_tls() {
            return Ok(Some(Either::A(transport)));
        }
        let host = details.uri.host().ok_or(ureq::Error::Tls("missing host"))?;
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        let name = rustls::pki_types::ServerName::try_from(host)
            .map_err(|_| ureq::Error::Tls("invalid dns name"))?
            .to_owned();
        let mut conn = rustls::ClientConnection::new(self.config.clone(), name)?;
        let mut sock = TransportAdapter::new(transport.boxed(), details.current_time.clone());
        sock.set_timeout(details.timeout);
        conn.complete_io(&mut sock)?;
        Ok(Some(Either::B(RingTlsTransport {
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
            stream: rustls::StreamOwned { conn, sock },
        })))
    }
}

struct RingTlsTransport {
    buffers: LazyBuffers,
    stream: rustls::StreamOwned<rustls::ClientConnection, TransportAdapter>,
}

impl fmt::Debug for RingTlsTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingTlsTransport").finish_non_exhaustive()
    }
}

impl Transport for RingTlsTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let amount = self.stream.read(input)?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        self.stream.get_mut().get_mut().is_open()
    }

    fn is_tls(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_parsing_and_comparison() {
        let hub = Origin::parse("https://huggingface.co").unwrap();
        assert_eq!(
            hub,
            Origin::parse("https://HuggingFace.co:443/a/b?c").unwrap()
        );
        assert_ne!(
            hub,
            Origin::parse("https://cdn-lfs.huggingface.co/x").unwrap()
        );
        assert_ne!(hub, Origin::parse("http://huggingface.co/x").unwrap());
        assert_ne!(
            Origin::parse("http://127.0.0.1:1/").unwrap(),
            Origin::parse("http://127.0.0.1:2/").unwrap()
        );
        assert!(Origin::parse("http://[::1]:8080/x").unwrap().is_loopback());
        assert!(Origin::parse("http://127.0.0.1/x").unwrap().is_loopback());
        assert!(
            !Origin::parse("http://localhost.example.com/")
                .unwrap()
                .is_loopback()
        );
        assert!(Origin::parse("ftp://example.com/").is_none());
        assert_eq!(
            Origin::parse("https://user:pw@example.com/").unwrap().host,
            "example.com"
        );
    }

    #[test]
    fn link_header_next() {
        let link = "<https://huggingface.co/api/models?cursor=abc>; rel=\"next\"";
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://huggingface.co/api/models?cursor=abc")
        );
        assert_eq!(next_link("<https://x/>; rel=\"prev\""), None);
    }

    #[test]
    fn resolve_url_encodes_revision_and_path() {
        let client = HubClient::new().unwrap();
        assert_eq!(
            client.resolve_url("acme/tiny", "refs/pr/1", "onnx/model v2.onnx"),
            "https://huggingface.co/acme/tiny/resolve/refs%2Fpr%2F1/onnx/model%20v2.onnx"
        );
    }

    #[test]
    fn debug_redacts_token() {
        let client = HubClient::new()
            .unwrap()
            .with_token(Some("hf_secret".into()));
        let text = format!("{client:?}");
        assert!(!text.contains("hf_secret"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn repo_ids_are_checked() {
        assert!(check_repo("acme/tiny-1.0").is_ok());
        assert!(check_repo("acme").is_err());
        assert!(check_repo("acme/../x").is_err());
        assert!(check_repo("acme/a/b").is_err());
    }
}
