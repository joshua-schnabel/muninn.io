//! Is a service actually answering?
//!
//! Existence is not reachability, and for the Docker module the difference is
//! the whole point. A mounted socket file whose daemon is gone, a socket proxy
//! that is up but refusing the API, a `tcp://` endpoint pointing at something
//! that is not Docker at all — every one of those produces a Telegraf that
//! starts happily and reports **no containers**. On a host that genuinely runs
//! no containers, that is the correct answer. The two are indistinguishable
//! from the metrics, which is why they have to be distinguished before start.
//!
//! # Why an HTTP request rather than a connect
//!
//! Connecting proves something is listening. The Docker Engine API exposes
//! `GET /_ping`, which answers `200 OK` and nothing else — the cheapest request
//! in the API. Sending it proves the thing listening speaks Docker. That
//! distinction matters most for the recommended deployment: a socket proxy that
//! is running but has `PING=0` set accepts the connection and denies the call,
//! and only the request sees it.
//!
//! Read as: this is deliberately not a Docker client. muninn never talks to the
//! API again — Telegraf does. One request, one status line, no dependency.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use muninn_modules::{Endpoint, EndpointKind};

/// The Engine API's cheapest call. `HTTP/1.0` so the server closes the
/// connection itself and there is no keep-alive to time out against.
const PING: &[u8] = b"GET /_ping HTTP/1.0\r\nHost: localhost\r\nUser-Agent: muninn\r\n\r\n";

/// Probe `endpoint`, returning why it is unusable.
///
/// `Ok(())` means something answered the Docker API there.
pub fn docker(endpoint: &Endpoint) -> Result<(), String> {
    match &endpoint.kind {
        EndpointKind::UnixSocket(path) => unix(path, endpoint.timeout),
        EndpointKind::Tcp(addr) => tcp(addr, endpoint.timeout),
    }
}

#[cfg(unix)]
fn unix(path: &str, timeout: Duration) -> Result<(), String> {
    use std::os::unix::net::UnixStream;

    // std has no connect_timeout for unix sockets, and does not need one:
    // connecting to a local socket either succeeds or fails at once. The
    // timeouts that matter are on the exchange, and those are set below.
    let mut stream = UnixStream::connect(path).map_err(|e| {
        format!(
            "cannot connect to '{path}': {e}. The file may be mounted while the daemon behind it \
             is gone — mount the live socket, or point the module at a socket proxy"
        )
    })?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    exchange(&mut stream, &format!("unix://{path}"))
}

/// On a non-unix host there is no socket to connect to, so there is nothing to
/// report. muninn ships as a Linux container; this exists so the workspace
/// still builds and tests on the development machine.
#[cfg(not(unix))]
fn unix(path: &str, _timeout: Duration) -> Result<(), String> {
    let _ = path;
    Ok(())
}

fn tcp(addr: &str, timeout: Duration) -> Result<(), String> {
    let resolved = addr
        .to_socket_addrs()
        .map_err(|e| {
            format!(
                "cannot resolve '{addr}': {e}. In compose this is the service name of the socket \
                 proxy, and it only resolves once both containers share a network"
            )
        })?
        .next()
        .ok_or_else(|| format!("'{addr}' resolved to no address"))?;

    let mut stream = TcpStream::connect_timeout(&resolved, timeout).map_err(|e| {
        format!(
            "cannot connect to '{addr}': {e}. Check the proxy is running and that the port is \
             the one it listens on"
        )
    })?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    exchange(&mut stream, &format!("tcp://{addr}"))
}

/// Send the ping and judge the status line.
fn exchange<S: Read + Write>(stream: &mut S, shown: &str) -> Result<(), String> {
    stream
        .write_all(PING)
        .map_err(|e| format!("connected to '{shown}' but the request failed: {e}"))?;

    // The status line is all that is needed, and reading only what is needed
    // means a service that connects and then says nothing hits the read timeout
    // instead of streaming into memory.
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).map_err(|e| {
        format!(
            "connected to '{shown}' but got no answer: {e}. Something is listening there that is \
             not the Docker API"
        )
    })?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let status = head.lines().next().unwrap_or("").trim();

    if status.contains(" 200") {
        return Ok(());
    }

    // A proxy is the likeliest cause of a non-200, and its fix is specific
    // enough to be worth naming here rather than in the documentation alone.
    if status.contains(" 403") || status.contains(" 401") {
        return Err(format!(
            "'{shown}' answered '{status}'. A socket proxy is refusing the call — allow the \
             endpoints the module needs (PING, CONTAINERS, INFO)"
        ));
    }
    if status.is_empty() {
        return Err(format!(
            "'{shown}' accepted the connection and closed it without answering. That is not the \
             Docker API"
        ));
    }
    Err(format!(
        "'{shown}' answered '{status}' instead of 200 to GET /_ping"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A stream that records what was written and replays a canned answer.
    struct Fake {
        response: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Fake {
        fn new(response: &str) -> Self {
            Fake {
                response: Cursor::new(response.as_bytes().to_vec()),
                written: Vec::new(),
            }
        }
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.response.read(buf)
        }
    }

    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_200_is_reachable() {
        let mut f = Fake::new("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
        assert!(exchange(&mut f, "unix:///x").is_ok());
        assert!(
            String::from_utf8_lossy(&f.written).starts_with("GET /_ping"),
            "the probe must ask the API, not just connect"
        );
    }

    /// The failure a plain connect check cannot see: a socket proxy that is up
    /// and denying the call. Telegraf would start and report zero containers.
    #[test]
    fn a_403_names_the_proxy_as_the_cause() {
        let mut f = Fake::new("HTTP/1.1 403 Forbidden\r\n\r\n");
        let e = exchange(&mut f, "tcp://proxy:2375").unwrap_err();
        assert!(e.contains("proxy"), "{e}");
        assert!(e.contains("PING"), "should name what to allow: {e}");
    }

    /// Something is listening on the port, but it is not Docker.
    #[test]
    fn a_silent_peer_is_not_the_docker_api() {
        let mut f = Fake::new("");
        let e = exchange(&mut f, "tcp://localhost:5432").unwrap_err();
        assert!(e.contains("not the Docker API"), "{e}");
    }

    #[test]
    fn any_other_status_is_reported_verbatim() {
        let mut f = Fake::new("HTTP/1.1 500 Internal Server Error\r\n\r\n");
        let e = exchange(&mut f, "unix:///x").unwrap_err();
        assert!(e.contains("500"), "{e}");
    }

    /// A refused connection must say so, not hang. Port 0 on the loopback is
    /// never listening.
    #[test]
    fn an_unreachable_tcp_endpoint_fails_quickly() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        drop(held); // now nothing is listening there

        let e = tcp(&format!("127.0.0.1:{port}"), Duration::from_secs(2)).unwrap_err();
        assert!(e.contains("cannot connect"), "{e}");
    }

    #[test]
    fn an_unresolvable_host_names_the_compose_case() {
        let e = tcp("muninn-no-such-host.invalid:2375", Duration::from_secs(2)).unwrap_err();
        assert!(e.contains("resolve"), "{e}");
    }
}
