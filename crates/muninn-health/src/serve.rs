//! Serving with a connection cap and a header-read timeout.
//!
//! `axum::serve` accepts without limit and applies no deadline to reading a
//! request head. A client that opens a socket and sends a partial request line
//! holds the connection, and its task, indefinitely — measured on huginn.io's
//! equivalent listeners as 4 000 idle half-open connections taking the image
//! from 29.5 MiB to 113.3 MiB with nothing refusing them.
//!
//! It matters more here than it did there. huginn's listeners are off by
//! default and bind loopback; muninn's health port is *meant* to be published,
//! because an orchestrator has to reach `/health/ready`. The exposure is the
//! normal deployment, not an unusual one.
//!
//! **A `tower` layer cannot fix it.** `TimeoutLayer` and `ConcurrencyLimitLayer`
//! wrap the service, and the service is not reached until hyper has parsed a
//! request; a request head that never completes never arrives. The limits sit
//! below the service instead:
//!
//!   * **the connection cap** is a semaphore permit taken *before* `accept`, so
//!     at capacity peers wait in the kernel's backlog rather than each costing a
//!     task and its buffers;
//!   * **the header-read timeout** is hyper's own, which needs the connection
//!     built by hand — `axum::serve` does not expose hyper's builder.
//!
//! Graceful shutdown is preserved, and that is the reason this is not simply
//! huginn's file copied across: muninn's stop sequence turns readiness off and
//! then waits, so a health request in flight when SIGTERM arrives is finished
//! rather than cut. `hyper_util`'s `GracefulShutdown` tracks the connections;
//! the accept loop stops on the signal and the tracked connections are drained.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

/// Connections served at once.
///
/// Generous for what this serves — liveness and readiness probes and a metrics
/// scrape are all single short requests — while bounding the memory an
/// unauthenticated peer can make the process use.
const MAX_CONNECTIONS: usize = 256;

/// How long a peer may take to send its request head.
///
/// Bounds the head, not the connection or the response.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long tracked connections may take to finish once shutdown begins.
///
/// Short on purpose: these are health endpoints, and the supervisor's own grace
/// period is the budget that matters. A scraper holding a connection must not
/// be able to delay the container's exit.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Serve `app` on `listener` until `shutdown` resolves.
pub(crate) async fn serve_with_limits(
    listener: TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_with(
        listener,
        app,
        shutdown,
        MAX_CONNECTIONS,
        HEADER_READ_TIMEOUT,
    )
    .await
}

/// The body of [`serve_with_limits`], with the limits as arguments.
///
/// Split out only so the tests can use a header timeout measured in
/// milliseconds instead of waiting ten seconds for the real one.
async fn serve_with(
    listener: TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    max_connections: usize,
    header_read_timeout: Duration,
) -> std::io::Result<()> {
    let permits = Arc::new(Semaphore::new(max_connections));
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        // Before `accept`, deliberately. Acquiring afterwards would mean the
        // connection — and its task and buffers — already exists, which is the
        // cost being avoided. Waiting here leaves the peer in the listen
        // backlog, and the kernel refuses it once that fills.
        let permit = Arc::clone(&permits)
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");

        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(v) => v,
                // One failed accept is not fatal — a peer that vanishes between
                // the SYN and our accept produces one, and returning would take
                // the listener down with it.
                Err(e) => {
                    warn!(error = %e, "accept failed");
                    continue;
                }
            },
            () = &mut shutdown => break,
        };

        // hyper's HTTP/1 builder directly, not hyper-util's `auto` one. `auto`
        // negotiates HTTP/2, which muninn has never served — it would pull h2,
        // tokio-util, fnv and futures-sink into the tree for a health endpoint
        // that answers three probes and a scrape. Adding a protocol nobody
        // asked for is not a side effect worth accepting for this.
        let mut builder = http1::Builder::new();
        // Not optional: hyper panics with "timeout `header_read_timeout` set,
        // but no timer set" the first time it arms the deadline. A runtime
        // panic, not a type error — nothing catches it at compile time.
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(header_read_timeout);

        let conn =
            builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()));
        let watched = graceful.watch(conn);

        tokio::spawn(async move {
            if let Err(e) = watched.await {
                // Expected in normal use: a scraper hanging up, a half-open
                // connection hitting the timeout above. Debug, not warn.
                debug!(%peer, error = %e, "connection closed");
            }
            drop(permit);
        });
    }

    // Finish what is in flight, but do not let it hold the process.
    if tokio::time::timeout(DRAIN_TIMEOUT, graceful.shutdown())
        .await
        .is_err()
    {
        warn!("health connections did not drain in time; closing anyway");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn app() -> Router {
        Router::new().route("/", axum::routing::get(|| async { "ok" }))
    }

    /// A connection that never finishes its request head must be dropped.
    ///
    /// This is the finding itself, and the reason a `tower` layer was the wrong
    /// tool: the service is never reached, so nothing above hyper can time it
    /// out. The assertion is that the *server* closes the socket — `read`
    /// returning 0 — not that a duration elapsed.
    #[tokio::test]
    async fn half_open_connection_is_closed_by_the_header_timeout() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_with(
            listener,
            app(),
            std::future::pending(),
            8,
            Duration::from_millis(200),
        ));

        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(b"GET / HTT").await.unwrap();

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
            .await
            .expect("the server never closed the half-open connection")
            .unwrap();
        assert_eq!(n, 0, "expected EOF, got {n} bytes");
    }

    /// A complete request is still answered — the timeout bounds the head.
    #[tokio::test]
    async fn a_complete_request_is_served() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_with(
            listener,
            app(),
            std::future::pending(),
            8,
            Duration::from_millis(200),
        ));

        let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
        sock.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
            .await
            .expect("no response")
            .unwrap();
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.starts_with("HTTP/1.1 200"), "unexpected: {head}");
    }

    /// The shutdown signal stops the accept loop and the call returns.
    ///
    /// muninn's stop sequence depends on this: `axum::serve`'s graceful
    /// shutdown was doing it before, and losing it would have left the health
    /// listener running past SIGTERM.
    #[tokio::test]
    async fn shutdown_signal_ends_the_server() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(serve_with(
            listener,
            app(),
            async {
                let _ = rx.await;
            },
            8,
            Duration::from_millis(200),
        ));

        tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("serve did not return after the shutdown signal")
            .unwrap()
            .unwrap();
    }
}
