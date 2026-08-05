use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use fofoca::protocol::Nickname;

use super::rpc::{A2aOp, A2aRequest, RpcError, parse_op};

/// Largest accepted request body. JSON-RPC calls carry one A2A Message
/// (whose wire form is itself capped by the gossip logical-body limit), so
/// this is generous headroom, not a real ceiling to engineer against.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// How many HTTP requests may be queued into the event loop at once —
/// backpressure so a local client cannot flood the daemon's select loop.
pub(crate) const REQUEST_QUEUE: usize = 32;

/// The bound (but not yet served) A2A binding: created early in setup so the
/// `ready` event can carry the real port, served once the event loop starts.
pub(crate) struct A2aBinding {
    pub listener: TcpListener,
    pub port: u16,
    /// The per-daemon bearer token every JSON-RPC call must present
    /// (`Authorization: Bearer <token>`). A localhost TCP port has none of
    /// the Unix socket's filesystem permissions, so possession of the
    /// token — written to the mode-600 session state file — is the gate.
    pub token: String,
}

/// Bind 127.0.0.1:`port` (`0` = OS-assigned) and mint the bearer token.
///
/// # Errors
/// The bind itself (port in use, no permission).
pub(crate) async fn bind(port: u16) -> Result<A2aBinding> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("failed to bind the A2A endpoint on 127.0.0.1:{port}"))?;
    let bound_port = listener
        .local_addr()
        .context("bound A2A listener has no local addr")?
        .port();
    let token = {
        use std::fmt::Write as _;
        let raw: [u8; 32] = rand::random();
        raw.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    };
    Ok(A2aBinding {
        listener,
        port: bound_port,
        token,
    })
}

/// Serve the binding: accept loop → per-connection HTTP/1.1 → each request
/// routed/authenticated here, then executed by the event loop via `req_tx`
/// (bounded; see [`REQUEST_QUEUE`]). Runs as a detached task for the
/// daemon's lifetime; it holds only channel ends, so daemon shutdown tears
/// it down with the runtime.
pub(crate) fn spawn(binding: A2aBinding, req_tx: mpsc::Sender<A2aRequest>) {
    let A2aBinding {
        listener, token, ..
    } = binding;
    tokio::spawn(async move {
        loop {
            let (stream, _addr) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!(%error, "a2a: accept failed");
                    continue;
                }
            };
            tokio::spawn(serve_connection(stream, token.clone(), req_tx.clone()));
        }
    });
}

/// Serve one accepted connection as HTTP/1.1, dispatching every request on it
/// through [`handle`].
async fn serve_connection(stream: TcpStream, token: String, req_tx: mpsc::Sender<A2aRequest>) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = service_fn(move |request| {
        let token = token.clone();
        let req_tx = req_tx.clone();
        async move { Ok::<_, std::convert::Infallible>(handle(request, &token, &req_tx).await) }
    });
    if let Err(error) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .await
    {
        tracing::debug!(%error, "a2a: connection ended with error");
    }
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("static response builds")
}

fn rpc_error_response(id: &serde_json::Value, error: &RpcError) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::OK,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        }),
    )
}

/// Route one request. The URL names the target agent: `/` and `/mesh` are
/// the mesh-collective endpoint (broadcast), `/peers/<nick>` a peer.
/// Cards are served unauthenticated (they are public on the mesh already);
/// every JSON-RPC call requires the bearer token.
async fn handle(
    request: Request<Incoming>,
    token: &str,
    req_tx: &mpsc::Sender<A2aRequest>,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_owned();
    match (request.method(), path.as_str()) {
        (&Method::GET, "/.well-known/agent-card.json") => {
            execute(A2aOp::OwnCard, req_tx).await.map_or_else(
                |error| card_error_response(&error),
                |card| json_response(StatusCode::OK, &card),
            )
        }
        (&Method::GET, _) if is_peer_card_path(&path) => {
            let Some(peer) = peer_card_nick(&path) else {
                return not_found();
            };
            execute(A2aOp::PeerCard { peer }, req_tx).await.map_or_else(
                |error| card_error_response(&error),
                |card| json_response(StatusCode::OK, &card),
            )
        }
        (&Method::POST, _) => rpc(request, token, req_tx).await,
        _ => not_found(),
    }
}

fn not_found() -> Response<Full<Bytes>> {
    json_response(
        StatusCode::NOT_FOUND,
        &serde_json::json!({ "error": "not found" }),
    )
}

fn card_error_response(error: &RpcError) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::NOT_FOUND,
        &serde_json::json!({ "error": error.message }),
    )
}

fn is_peer_card_path(path: &str) -> bool {
    path.starts_with("/peers/") && path.ends_with("/.well-known/agent-card.json")
}

fn peer_card_nick(path: &str) -> Option<Nickname> {
    let nick = path
        .strip_prefix("/peers/")?
        .strip_suffix("/.well-known/agent-card.json")?;
    Nickname::new(nick).ok()
}

/// The JSON-RPC POST path: authenticate, parse, resolve the URL target,
/// execute in the loop, wrap the result.
async fn rpc(
    request: Request<Incoming>,
    token: &str,
    req_tx: &mpsc::Sender<A2aRequest>,
) -> Response<Full<Bytes>> {
    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        == Some(token);
    if !authorized {
        return json_response(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({ "error": "missing or invalid bearer token" }),
        );
    }
    let target = match request.uri().path() {
        "/" | "/mesh" => None,
        path => match path.strip_prefix("/peers/").map(Nickname::new) {
            Some(Ok(nick)) => Some(nick),
            Some(Err(_)) | None => return not_found(),
        },
    };
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let envelope: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") },
                }),
            );
        }
    };
    let id = envelope["id"].clone();
    let Some(method) = envelope["method"].as_str() else {
        return rpc_error_response(&id, &RpcError::invalid_params("method is required"));
    };
    let op = match parse_op(method, &envelope["params"], target) {
        Ok(op) => op,
        Err(error) => return rpc_error_response(&id, &error),
    };
    match execute(op, req_tx).await {
        Ok(result) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        ),
        Err(error) => rpc_error_response(&id, &error),
    }
}

async fn read_body(request: Request<Incoming>) -> Result<Bytes, Box<Response<Full<Bytes>>>> {
    let limited = http_body_util::Limited::new(request.into_body(), MAX_BODY_BYTES);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(_) => Err(Box::new(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &serde_json::json!({ "error": "request body too large" }),
        ))),
    }
}

/// Hand one op to the event loop and await its outcome.
async fn execute(
    op: A2aOp,
    req_tx: &mpsc::Sender<A2aRequest>,
) -> Result<serde_json::Value, RpcError> {
    let (resp_tx, resp_rx) = oneshot::channel();
    let request = A2aRequest { op, resp: resp_tx };
    if req_tx.send(request).await.is_err() {
        return Err(RpcError {
            code: -32603,
            message: "daemon event loop is gone".to_string(),
        });
    }
    resp_rx.await.unwrap_or_else(|_| {
        Err(RpcError {
            code: -32603,
            message: "daemon dropped the request".to_string(),
        })
    })
}
