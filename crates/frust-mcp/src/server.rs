//! MCP server lifecycle: bind, serve, graceful shutdown.
//!
//! Runs the MCP Streamable HTTP transport (rmcp) behind an axum router
//! nested at `/mcp`, bound to `127.0.0.1` only — never configurable (see
//! [`crate::McpConfig`]'s doc comment). Bind/serve failures are returned as
//! `Err` rather than panicking the host process.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::clients::ClientRegistry;
use crate::handler::McpHandler;
use crate::{McpConfig, SharedBackend};

/// HTTP path the MCP endpoint is served under
/// (clients connect to `http://127.0.0.1:<port>/mcp`).
const MCP_HTTP_PATH: &str = "/mcp";

/// Bind `127.0.0.1:config.port`, serve until `cancel` fires, then return.
///
/// `ready`, when given, is notified with the actual bound address once the
/// listener is up — the seam [`crate::run_with_ready`] exposes to tests,
/// since `McpConfig` itself never carries a resolved port back out (needed
/// when `config.port == 0`, an OS-assigned port).
///
/// `backend` is shared by every MCP session the transport creates: the
/// factory below clones the `Arc` into each handler, so all of them drive
/// the same supervised apps. `run`/`run_with_*` have no external
/// client-count observer, so this builds a fresh, unshared
/// [`ClientRegistry`] per call — only [`serve_embedded`]'s caller needs one
/// that survives past a single server lifetime.
pub(crate) async fn serve(
    config: McpConfig,
    backend: SharedBackend,
    ready: Option<oneshot::Sender<SocketAddr>>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    serve_embedded(backend, ClientRegistry::new(), config.port, ready, cancel).await
}

/// The same MCP Streamable HTTP transport as [`serve`], but over a
/// caller-owned `backend` and `registry` rather than constructing them
/// internally — the entry point an embedder (e.g. `frust-tui`) starts and
/// stops at runtime, on its own [`CancellationToken`], without this crate
/// building or tearing down an engine on its behalf.
///
/// `registry` is shared across every MCP session this call accepts: the
/// service factory below registers a `ClientGuard` per session (and per
/// sessionless 2026-07-28 request) and moves it
/// into that session's [`McpHandler`], so the guard's `Drop` — the *only*
/// disconnect signal available (`clients.rs`'s module doc) — removes the
/// entry again. Repeated start/stop cycles handing the same `registry` back
/// in are safe: nothing here is process-global, and a registry with no live
/// guards left in it (the normal end state of a clean shutdown) is
/// indistinguishable from a fresh one.
pub async fn serve_embedded(
    backend: SharedBackend,
    registry: ClientRegistry,
    bind_port: u16,
    ready: Option<oneshot::Sender<SocketAddr>>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", bind_port)).await?;
    let addr = listener.local_addr()?;

    // rmcp's default `allowed_hosts` (`localhost`/`127.0.0.1`/`::1`, any
    // port) is exactly the DNS-rebinding guard this loopback-only server
    // needs, left untouched rather than widened — unlike fdemon's
    // configurable-bind server, this one has no other address to allow.
    //
    // rmcp leaves Origin validation off while `allowed_origins` is empty
    // (its default), so the allowlist is set here. A page served from any
    // loopback http origin, on any port (`127.0.0.1`, `localhost` or `[::1]`),
    // is accepted, so a browser tool running on another localhost port can
    // reach this server. Any other Origin (another site, `https`, `null`) is
    // refused with 403, and a request with no Origin header (every non-browser
    // MCP client) still passes. The `:*` form is used rather than a port-less
    // entry: rmcp matches a port-less entry against any port only for now and
    // is deprecating that, so `:*` states the intent that stays true.
    //
    // `legacy_session_mode` is set explicitly rather than taken from the
    // default: a client that runs the `initialize` handshake (protocol
    // versions before 2026-07-28) keeps a stateful session with an
    // `Mcp-Session-Id`, one handler per session, as it always has.
    //
    // A 2026-07-28 request carries its protocol version, client info and
    // capabilities in its own `_meta` and has no session. rmcp serves it
    // statelessly with no code here, and the handler advertises every
    // version rmcp knows, so it is served rather than refused. rmcp runs the
    // factory below once per such request, so a sessionless request shows
    // in the client registry as one transient entry for as long as its
    // handler runs; the entry is removed when the handler is dropped at the
    // end of the request, which keeps the registry's count accurate.
    let transport_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(true)
        .with_allowed_origins(["http://127.0.0.1:*", "http://localhost:*", "http://[::1]:*"])
        .with_cancellation_token(cancel.child_token());
    let service = StreamableHttpService::new(
        move || {
            let guard = registry.register();
            Ok::<_, std::io::Error>(McpHandler::new(Arc::clone(&backend)).with_client_guard(guard))
        },
        Arc::new(LocalSessionManager::default()),
        transport_config,
    );
    let router = axum::Router::new().nest_service(MCP_HTTP_PATH, service);

    log::info!("frust-mcp listening on http://{addr}{MCP_HTTP_PATH}");
    if let Some(ready) = ready {
        // The test/embedding caller may already have given up (e.g. the
        // smoke test's deadline fired) — a dropped receiver is not a server
        // error, so ignore the send failure and keep serving.
        let _ = ready.send(addr);
    }

    let shutdown = cancel.child_token();
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;
    log::info!("frust-mcp: server stopped");
    Ok(())
}
