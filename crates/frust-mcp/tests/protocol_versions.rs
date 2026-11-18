//! Which MCP protocol versions the server speaks, and how it answers each.
//!
//! - The `initialize` handshake at 2024-11-05, 2025-03-26, 2025-06-18 and
//!   2025-11-25 opens a stateful session and returns an `Mcp-Session-Id`.
//! - A request naming a session the server never issued is answered 404.
//! - A request with no session id that is not `initialize`, at a version that
//!   has the handshake, is answered 400.
//! - A 2026-07-28 request (no handshake, no session; protocol version and
//!   client capabilities in its own `_meta`) is served: `tools/list` returns
//!   the same tools a session sees, `tools/call` runs, and the request leaves
//!   no entry behind in the client registry.
//! - Origin: none, this server's `127.0.0.1` origin and its `localhost` origin
//!   are accepted; any other origin (including `null`) is refused with 403,
//!   on the stateful and the sessionless path alike.
//! - A refused request (400, 403) leaves no entry in the client registry.
//!
//! Driven over raw `tokio` `TcpStream`s like the other transport tests (see
//! `common/mcp.rs`). **No sleeps:** every wait is on an HTTP response or a
//! bounded yield-until-condition poll on the registry; the only durations
//! are failure deadlines.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use common::TEST_PROJECT_ROOT;
use common::mcp::{json_rpc_response, post, request_json, roundtrip, session_id_header};
use frust_drive::process::FakeProcessRunner;
use frust_mcp::{ClientRegistry, SessionEngine, SharedBackend};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

/// A failure deadline — never a pacing device.
const DEADLINE: Duration = Duration::from_secs(10);

/// The first protocol version without the `initialize` handshake.
const SESSIONLESS_VERSION: &str = "2026-07-28";

/// A running server plus the registry it reports connected clients to.
struct Server {
    addr: SocketAddr,
    registry: ClientRegistry,
    cancel: CancellationToken,
    join: JoinHandle<anyhow::Result<()>>,
}

impl Server {
    async fn start() -> Server {
        let backend: SharedBackend = Arc::new(SessionEngine::with_runner(
            TEST_PROJECT_ROOT,
            Arc::new(FakeProcessRunner::new()),
        ));
        let registry = ClientRegistry::new();
        let (ready_tx, ready_rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        let join = tokio::spawn(frust_mcp::serve_embedded(
            backend,
            registry.clone(),
            0,
            Some(ready_tx),
            cancel.clone(),
        ));
        let addr = timeout(DEADLINE, ready_rx)
            .await
            .expect("server did not report ready in time")
            .expect("ready channel dropped before send");
        Server {
            addr,
            registry,
            cancel,
            join,
        }
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    async fn shutdown(self) {
        self.cancel.cancel();
        timeout(DEADLINE, self.join)
            .await
            .expect("server task did not shut down within the deadline")
            .expect("server task panicked")
            .expect("server task returned an error");
    }

    /// Spins (yielding, never sleeping) until the registry count equals
    /// `expected` or the deadline elapses.
    async fn wait_for_count(&self, expected: usize) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if self.registry.count() == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "registry count did not reach {expected} within {DEADLINE:?} (still {})",
                self.registry.count()
            );
            tokio::task::yield_now().await;
        }
    }
}

/// An `initialize` POST naming `version`, with an optional `Origin` header.
fn initialize_at(host: &str, version: &str, origin: Option<&str>) -> String {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{version}","capabilities":{{}},"clientInfo":{{"name":"frust-mcp-test","version":"0.0.0"}}}}}}"#
    );
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {host}\r\n\
         {origin}\
         Accept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len(),
    )
}

/// A 2026-07-28 request: no session id, the version in both the
/// `MCP-Protocol-Version` header and `_meta`, plus the `Mcp-Method` (and,
/// for a named method, `Mcp-Name`) headers that version requires.
fn sessionless(host: &str, id: u64, method: &str, params: Value) -> String {
    sessionless_with_origin(host, id, method, params, None)
}

/// [`sessionless`] with an optional `Origin` header.
fn sessionless_with_origin(
    host: &str,
    id: u64,
    method: &str,
    mut params: Value,
    origin: Option<&str>,
) -> String {
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": SESSIONLESS_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "frust-mcp-test", "version": "0.0.0" },
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(|name| format!("Mcp-Name: {name}\r\n"))
        .unwrap_or_default();
    let body =
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {host}\r\n\
         {origin}\
         Accept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\n\
         MCP-Protocol-Version: {SESSIONLESS_VERSION}\r\n\
         Mcp-Method: {method}\r\n\
         {name}\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len(),
    )
}

/// Opens a session at `version`, asserting a 200, the server identity, the
/// negotiated version and a session id; returns the session id.
async fn open_session_at(server: &Server, version: &str) -> String {
    let response = roundtrip(
        server.addr,
        &initialize_at(&server.host(), version, None),
        "serverInfo",
    )
    .await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "initialize at {version} was: {response}"
    );
    let init = json_rpc_response(&response, 1);
    assert_eq!(init["result"]["serverInfo"]["name"], json!("frust-mcp"));
    assert_eq!(
        init["result"]["protocolVersion"],
        json!(version),
        "the server should agree to {version}: {init}"
    );
    let session_id = session_id_header(&response);
    assert!(!session_id.is_empty(), "empty Mcp-Session-Id at {version}");
    session_id
}

fn tool_names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result has no tools array: {list}"))
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("tool without a name")
                .to_owned()
        })
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn initialize_at_2024_11_05_opens_a_session() {
    let server = Server::start().await;
    open_session_at(&server, "2024-11-05").await;
    server.wait_for_count(1).await;
    server.shutdown().await;
}

#[tokio::test]
async fn initialize_at_2025_03_26_opens_a_session() {
    let server = Server::start().await;
    open_session_at(&server, "2025-03-26").await;
    server.wait_for_count(1).await;
    server.shutdown().await;
}

#[tokio::test]
async fn initialize_at_2025_06_18_opens_a_session() {
    let server = Server::start().await;
    open_session_at(&server, "2025-06-18").await;
    server.wait_for_count(1).await;
    server.shutdown().await;
}

#[tokio::test]
async fn initialize_at_2025_11_25_opens_a_session() {
    let server = Server::start().await;
    open_session_at(&server, "2025-11-25").await;
    server.wait_for_count(1).await;
    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_session_id_is_answered_404() {
    let server = Server::start().await;
    let request = post(
        &server.host(),
        "not-a-session-this-server-issued",
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#,
    );
    let response = roundtrip(server.addr, &request, "\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "expected 404 for an unknown Mcp-Session-Id, got: {response}"
    );
    assert_eq!(server.registry.count(), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn a_legacy_request_without_a_session_id_is_answered_400() {
    let server = Server::start().await;
    let body = r#"{"jsonrpc":"2.0","id":8,"method":"tools/list","params":{}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {}\r\n\
         Accept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\n\
         MCP-Protocol-Version: 2025-06-18\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        server.host(),
        body.len(),
    );
    let response = roundtrip(server.addr, &request, "\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "expected 400 for a session-less non-initialize request, got: {response}"
    );
    server.wait_for_count(0).await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_2026_07_28_request_is_served_without_a_session() {
    let server = Server::start().await;

    // The tool set a session sees, for comparison.
    let session_id = open_session_at(&server, "2025-06-18").await;
    let session_list = request_json(
        server.addr,
        &post(
            &server.host(),
            &session_id,
            r#"{"jsonrpc":"2.0","id":20,"method":"tools/list","params":{}}"#,
        ),
        20,
    )
    .await;
    let session_tools = tool_names(&session_list["result"]);
    server.wait_for_count(1).await;

    let list = request_json(
        server.addr,
        &sessionless(&server.host(), 21, "tools/list", json!({})),
        21,
    )
    .await;
    assert!(
        list.get("error").is_none(),
        "sessionless tools/list: {list}"
    );
    assert_eq!(
        tool_names(&list["result"]),
        session_tools,
        "a sessionless client must see the same tools as a session"
    );

    let call = request_json(
        server.addr,
        &sessionless(
            &server.host(),
            22,
            "tools/call",
            json!({ "name": "ping", "arguments": {} }),
        ),
        22,
    )
    .await;
    assert!(
        call.get("error").is_none(),
        "sessionless tools/call: {call}"
    );
    assert_ne!(
        call["result"]["isError"],
        json!(true),
        "ping failed: {call}"
    );
    assert_eq!(call["result"]["structuredContent"]["ok"], json!(true));

    // Each sessionless request is a transient registry entry: once its
    // handler is gone, only the one real session is left.
    server.wait_for_count(1).await;

    server.shutdown().await;
}

#[tokio::test]
async fn origin_is_checked_against_this_servers_loopback_origin() {
    let server = Server::start().await;
    let port = server.addr.port();

    for origin in [
        None,
        Some(format!("http://127.0.0.1:{port}")),
        Some(format!("http://localhost:{port}")),
    ] {
        let request = initialize_at(&server.host(), "2025-06-18", origin.as_deref());
        let response = roundtrip(server.addr, &request, "serverInfo").await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "Origin {origin:?} should be accepted, got: {response}"
        );
        session_id_header(&response);
    }

    for origin in [
        "http://evil.example".to_owned(),
        format!("http://evil.example:{port}"),
    ] {
        let request = initialize_at(&server.host(), "2025-06-18", Some(&origin));
        let response = roundtrip(server.addr, &request, "\r\n\r\n").await;
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "Origin {origin} should be refused, got: {response}"
        );
        server.wait_for_count(3).await;
    }

    server.shutdown().await;
}

#[tokio::test]
async fn origin_null_on_initialize_is_refused() {
    let server = Server::start().await;
    let request = initialize_at(&server.host(), "2025-06-18", Some("null"));
    let response = roundtrip(server.addr, &request, "\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.1 403"),
        "Origin: null should be refused, got: {response}"
    );
    server.wait_for_count(0).await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_foreign_origin_on_a_sessionless_request_is_refused() {
    let server = Server::start().await;
    let call = json!({ "name": "ping", "arguments": {} });

    let refused = sessionless_with_origin(
        &server.host(),
        30,
        "tools/call",
        call.clone(),
        Some("http://evil.example"),
    );
    let response = roundtrip(server.addr, &refused, "\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.1 403"),
        "a foreign Origin on a sessionless call should be refused, got: {response}"
    );
    server.wait_for_count(0).await;

    let accepted = request_json(
        server.addr,
        &sessionless(&server.host(), 31, "tools/call", call),
        31,
    )
    .await;
    assert!(
        accepted.get("error").is_none(),
        "the same call without an Origin should succeed: {accepted}"
    );
    server.wait_for_count(0).await;

    server.shutdown().await;
}
