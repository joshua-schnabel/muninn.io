//! The HTTP endpoints.
//!
//! | Route | Answers |
//! |---|---|
//! | `GET /health/live` | Is muninn's own loop responsive? |
//! | `GET /health/ready` | Is muninn collecting? |
//! | `GET /status` | What is it doing, in detail a human reads |
//! | `GET /metrics` | muninn's own operational metrics |
//!
//! Liveness and readiness are different questions on purpose. A brief InfluxDB
//! outage must not fail liveness — muninn is fine, the network is not, and a
//! restart would help nothing. A dead Telegraf must fail readiness immediately,
//! because at that point nothing is being collected.
//!
//! Note `/metrics` here is **muninn's own** metrics. Host metrics come from
//! Telegraf on its own port. Scraping the wrong one is the most likely setup
//! mistake muninn invites, which is why it is called out in the README, the
//! annotated configuration and `docs/configuration.md`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing::{info, warn};

use crate::state::HealthState;

/// What the server needs to answer a request.
#[derive(Clone)]
pub struct ServerState {
    pub health: HealthState,
    pub muninn_version: &'static str,
}

pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/status", get(status))
        .route("/metrics", get(metrics))
        .with_state(Arc::new(state))
}

/// Serve until `shutdown` resolves.
///
/// Binding is fallible and reported, not fatal to the caller's decision: the
/// supervisor decides what a failed health listener means. Returning an error
/// rather than exiting keeps that decision in one place.
pub async fn serve(
    listen: SocketAddr,
    state: ServerState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .inspect_err(|e| {
            warn!(%listen, error = %e, "could not bind the health listener");
        })?;

    // The bound address, not the configured one: with port 0 they differ, and
    // the tests need to know where to connect.
    let bound = listener.local_addr().unwrap_or(listen);
    info!(%bound, "health server listening");

    crate::serve::serve_with_limits(listener, router(state), shutdown).await
}

/// Bind and return the address, so a caller that needs to know the port (a test,
/// or a `listen: 0` deployment) can learn it before serving starts.
pub async fn bind(listen: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(listen).await
}

/// Serve on an already-bound listener.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: ServerState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    crate::serve::serve_with_limits(listener, router(state), shutdown).await
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Live {
    status: &'static str,
    state: &'static str,
}

async fn live(AxumState(s): AxumState<Arc<ServerState>>) -> Response {
    let current = s.health.get();
    let body = Json(Live {
        status: if current.is_live() { "ok" } else { "down" },
        state: current.as_str(),
    });
    if current.is_live() {
        (StatusCode::OK, body).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
    }
}

#[derive(Serialize)]
struct Ready {
    status: &'static str,
    state: &'static str,
    telegraf: TelegrafStatus,
}

#[derive(Serialize)]
struct TelegrafStatus {
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Why it stopped, when it has. Present only after an exit, so its absence
    /// is itself information.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_exit: Option<String>,
}

async fn ready(AxumState(s): AxumState<Arc<ServerState>>) -> Response {
    let current = s.health.get();
    let d = s.health.details();

    let body = Json(Ready {
        status: if current.is_ready() {
            "ready"
        } else {
            "not_ready"
        },
        state: current.as_str(),
        telegraf: TelegrafStatus {
            running: d.telegraf_pid.is_some(),
            pid: d.telegraf_pid,
            version: d.telegraf_version.clone(),
            last_exit: d.last_telegraf_exit.clone(),
        },
    });

    if current.is_ready() {
        (StatusCode::OK, body).into_response()
    } else {
        // 503 rather than 500: this is "not yet" or "not any more", not an
        // error in handling the request.
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
    }
}

#[derive(Serialize)]
struct Status {
    muninn_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegraf_version: Option<String>,
    state: &'static str,
    ready: bool,
    uptime_seconds: u64,
    telegraf: TelegrafStatus,
    telegraf_restarts: u64,
    modules: Vec<String>,
    outputs: Vec<String>,
    module_checks: Vec<ModuleCheckStatus>,
}

#[derive(Serialize)]
struct ModuleCheckStatus {
    module: String,
    success: bool,
    at: u64,
}

/// Diagnostics for a human.
///
/// Deliberately not a configuration dump. No secrets, no file paths, no
/// rendered TOML — `/status` may be exposed to a scraper alongside `/metrics`,
/// and "diagnostic" is not a reason to hand out the agent's configuration.
async fn status(AxumState(s): AxumState<Arc<ServerState>>) -> Json<Status> {
    let current = s.health.get();
    let d = s.health.details();

    Json(Status {
        muninn_version: s.muninn_version,
        telegraf_version: d.telegraf_version.clone(),
        state: current.as_str(),
        ready: current.is_ready(),
        uptime_seconds: s.health.uptime().as_secs(),
        telegraf: TelegrafStatus {
            running: d.telegraf_pid.is_some(),
            pid: d.telegraf_pid,
            version: d.telegraf_version.clone(),
            last_exit: d.last_telegraf_exit.clone(),
        },
        telegraf_restarts: s.health.telegraf_restarts(),
        modules: d.modules.clone(),
        outputs: d.outputs.clone(),
        module_checks: d
            .module_checks
            .iter()
            .map(|(module, c)| ModuleCheckStatus {
                module: module.clone(),
                success: c.success,
                at: c.at,
            })
            .collect(),
    })
}

async fn metrics(AxumState(s): AxumState<Arc<ServerState>>) -> Response {
    let body = crate::metrics::render(&s.health, s.muninn_version);
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    /// Start a server on an ephemeral port and return its address plus a handle
    /// that stops it on drop.
    async fn start(health: HealthState) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = ServerState {
            health,
            muninn_version: "0.1.0",
        };
        let handle = tokio::spawn(async move {
            let _ = serve_on(listener, state, std::future::pending::<()>()).await;
        });
        (addr, handle)
    }

    async fn get_raw(addr: SocketAddr, path: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        let code = response
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (code, body)
    }

    #[tokio::test]
    async fn liveness_holds_while_starting_but_readiness_does_not() {
        let health = HealthState::new();
        let (addr, handle) = start(health.clone()).await;

        let (code, body) = get_raw(addr, "/health/live").await;
        assert_eq!(code, 200, "starting is live: {body}");
        assert!(body.contains("\"status\":\"ok\""), "{body}");

        let (code, body) = get_raw(addr, "/health/ready").await;
        assert_eq!(code, 503, "starting is not ready: {body}");
        assert!(body.contains("\"status\":\"not_ready\""), "{body}");

        handle.abort();
    }

    /// The endpoints must reflect the state *after* a transition, not the state
    /// they were started with — otherwise they only ever report the beginning.
    #[tokio::test]
    async fn readiness_follows_the_state_as_it_changes() {
        let health = HealthState::new();
        let (addr, handle) = start(health.clone()).await;

        health.set(State::Ready);
        health.update(|d| {
            d.telegraf_pid = Some(4242);
            d.telegraf_version = Some("1.39.2".into());
        });
        let (code, body) = get_raw(addr, "/health/ready").await;
        assert_eq!(code, 200, "{body}");
        assert!(body.contains("\"running\":true"), "{body}");
        assert!(body.contains("\"pid\":4242"), "{body}");

        // ...and back again, which is what a shutdown does.
        health.set(State::Stopping);
        let (code, _) = get_raw(addr, "/health/ready").await;
        assert_eq!(code, 503, "stopping must stop reporting ready");

        handle.abort();
    }

    /// A failing non-critical module must not take a working agent out of
    /// service.
    #[tokio::test]
    async fn degraded_still_reports_ready() {
        let health = HealthState::new();
        health.set(State::Degraded);
        health.update(|d| d.telegraf_pid = Some(1));
        let (addr, handle) = start(health).await;

        let (code, body) = get_raw(addr, "/health/ready").await;
        assert_eq!(code, 200, "{body}");
        assert!(body.contains("\"state\":\"degraded\""), "{body}");

        handle.abort();
    }

    /// The whole reason these endpoints are muninn's and not Telegraf's: they
    /// must still answer when Telegraf is gone.
    #[tokio::test]
    async fn a_failed_agent_still_serves_and_says_why() {
        let health = HealthState::new();
        health.set(State::Failed);
        health.update(|d| d.last_telegraf_exit = Some("exit code 137".into()));
        let (addr, handle) = start(health).await;

        let (code, body) = get_raw(addr, "/health/ready").await;
        assert_eq!(code, 503, "{body}");
        assert!(body.contains("exit code 137"), "should say why: {body}");

        let (code, body) = get_raw(addr, "/health/live").await;
        assert_eq!(code, 503, "a failed agent is not live either: {body}");

        handle.abort();
    }

    #[tokio::test]
    async fn status_reports_what_is_enabled() {
        let health = HealthState::new();
        health.set(State::Ready);
        health.update(|d| {
            d.telegraf_pid = Some(17);
            d.telegraf_version = Some("1.39.2".into());
            d.modules = vec!["cpu".into(), "memory".into()];
            d.outputs = vec!["prometheus".into()];
        });
        health.record_module_check("cpu", true);
        let (addr, handle) = start(health).await;

        let (code, body) = get_raw(addr, "/status").await;
        assert_eq!(code, 200);
        assert!(body.contains("\"muninn_version\":\"0.1.0\""), "{body}");
        assert!(body.contains("\"telegraf_version\":\"1.39.2\""), "{body}");
        assert!(body.contains("\"cpu\""), "{body}");
        assert!(body.contains("\"prometheus\""), "{body}");
        assert!(body.contains("\"ready\":true"), "{body}");

        handle.abort();
    }

    /// `/status` may be exposed to a scraper. "Diagnostic" is not a reason to
    /// hand out the agent's configuration.
    #[tokio::test]
    async fn status_carries_no_secret_and_no_configuration_dump() {
        let health = HealthState::new();
        health.set(State::Ready);
        health.update(|d| {
            d.telegraf_pid = Some(17);
            d.modules = vec!["cpu".into()];
        });
        let (addr, handle) = start(health).await;

        let (_, body) = get_raw(addr, "/status").await;
        for forbidden in ["token", "password", "secret", "[[inputs", "telegraf.conf"] {
            assert!(
                !body.contains(forbidden),
                "/status leaked {forbidden:?}: {body}"
            );
        }

        handle.abort();
    }

    #[tokio::test]
    async fn metrics_are_served_in_prometheus_text_format() {
        let health = HealthState::new();
        health.set(State::Ready);
        health.update(|d| d.telegraf_pid = Some(17));
        let (addr, handle) = start(health).await;

        let (code, body) = get_raw(addr, "/metrics").await;
        assert_eq!(code, 200);
        assert!(body.contains("# TYPE muninn_ready gauge"), "{body}");
        assert!(body.contains("muninn_ready 1"), "{body}");

        handle.abort();
    }

    #[tokio::test]
    async fn an_unknown_route_is_404_not_an_error() {
        let (addr, handle) = start(HealthState::new()).await;
        let (code, _) = get_raw(addr, "/nope").await;
        assert_eq!(code, 404);
        handle.abort();
    }

    /// A port already in use must be reported rather than panicking — the
    /// supervisor decides what a failed listener means.
    #[tokio::test]
    async fn binding_an_occupied_port_reports_an_error() {
        let held = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = held.local_addr().unwrap();
        let state = ServerState {
            health: HealthState::new(),
            muninn_version: "0.1.0",
        };
        let result = serve(addr, state, std::future::pending::<()>()).await;
        assert!(result.is_err(), "a second bind on {addr} should fail");
    }
}
