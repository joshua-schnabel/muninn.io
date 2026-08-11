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
        //
        // Inside the `select!`, equally deliberately. Awaiting the permit on its
        // own put shutdown behind it: at capacity, nothing observed the stop
        // signal until a connection finished, so the drain below could not even
        // begin. A peer holding every permit could therefore stretch shutdown
        // past the Compose stop timeout (F-07). Shutdown is now visible whether
        // or not there is a permit to be had.
        let permit = tokio::select! {
            acquired = Arc::clone(&permits).acquire_owned() => match acquired {
                Ok(p) => p,
                // Only reachable if the semaphore were closed, which nothing
                // does. Stopping is the right answer either way, and it is the
                // answer that does not panic in a server loop.
                Err(_) => break,
            },
            () = &mut shutdown => break,
        };

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
            .header_read_timeout(header_read_timeout)
            // No keep-alive, and this is the keep-alive policy rather than an
            // oversight. The header deadline bounds one request head; it does
            // nothing about a peer that sends a complete, small, perfectly
            // valid request every few seconds on each of 256 connections. That
            // costs almost nothing to do and holds every permit, so genuine
            // probes wait in the backlog while the process looks idle (F-07).
            //
            // Cheap to give up here specifically: these endpoints answer three
            // probes and a scrape, each a single short request. A scraper pays
            // one extra TCP handshake per scrape interval, which is not a cost
            // worth a starvation vector. Telegraf's `:9273`, which serves the
            // host metrics, is a different listener with different traffic and
            // is not affected.
            .keep_alive(false);

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

    /// Shutdown is observed even when every permit is taken.
    ///
    /// The finding (F-07): the permit was awaited *before* the `select!` that
    /// watches for shutdown, so at capacity nothing saw the stop signal until a
    /// connection finished. A peer holding all the permits could stretch
    /// shutdown past the Compose stop timeout — with the process looking idle
    /// the whole time.
    ///
    /// The server is given one permit and a connection is left occupying it, so
    /// the loop is parked exactly where the bug was. The assertion is that the
    /// call *returns*; if it did not, the timeout below reports it rather than
    /// hanging the suite.
    #[tokio::test]
    async fn shutdown_is_observed_at_the_connection_limit() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        // One permit, and a header timeout long enough that the connection
        // below holds it for the whole test rather than being timed out into
        // releasing it — which would let the old code pass.
        let handle = tokio::spawn(serve_with(
            listener,
            app(),
            async {
                let _ = rx.await;
            },
            1,
            Duration::from_secs(30),
        ));

        // Take the only permit and keep it: a head that never completes.
        let mut hog = tokio::net::TcpStream::connect(addr).await.unwrap();
        hog.write_all(b"GET / HTT").await.unwrap();

        // Wait until the loop is actually parked on the permit, rather than
        // sleeping and hoping. A second connection cannot be served while the
        // first holds the only permit, so a read that does not complete is the
        // signal that capacity is reached.
        let mut queued = tokio::net::TcpStream::connect(addr).await.unwrap();
        queued
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        assert!(
            tokio::time::timeout(Duration::from_millis(300), queued.read(&mut buf))
                .await
                .is_err(),
            "the second request was served, so the limit was not reached"
        );

        tx.send(()).unwrap();

        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the server did not observe shutdown while at the connection limit")
            .unwrap()
            .unwrap();
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
