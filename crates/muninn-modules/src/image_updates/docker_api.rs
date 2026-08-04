//! A minimal, blocking Docker Engine API client.
//!
//! muninn is deliberately not a Docker client anywhere else —
//! `muninn/src/probe.rs` sends exactly one `GET /_ping` at startup and never
//! calls the API again, because Telegraf is the one that talks to Docker for
//! metrics. This module breaks that pattern on purpose: Telegraf has no plugin
//! that can answer "is a newer image available under this tag", so muninn asks
//! the question itself, the same way it already asks apt about pending package
//! updates. See ADR-0013.
//!
//! # Why the daemon, not the registry
//!
//! The obvious alternative — muninn speaking HTTPS directly to Docker Hub,
//! GHCR or a private registry — needs a TLS stack and a bearer-token auth flow
//! muninn has nowhere else. `GET /distribution/{name}/json` asks the *daemon*
//! to do that instead: the daemon already has a TLS stack (Go's, a separate
//! process from muninn's Rust) and already knows any registry credentials the
//! host is configured with. muninn stays a plaintext HTTP client talking to a
//! socket or a proxy, exactly as it already does for `/_ping` and exactly as
//! `deny.toml`'s note on OpenSSL says it does.
//!
//! # Why this is not a general HTTP client
//!
//! Three calls, three response shapes, one connection each. No keep-alive, no
//! chunked transfer encoding, no streaming endpoints — `Connection: close` is
//! requested on every request specifically so reading to EOF is always correct
//! and a hung peer is bounded by the endpoint's timeout rather than by a
//! response that never says how long it is.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Deserialize;

use crate::{Endpoint, EndpointKind};

/// `filters={"status":["running"]}`, percent-encoded.
///
/// Fixed and small enough to write out once by hand rather than carry a
/// URL-encoding routine in the tree for one query parameter whose value never
/// changes: this module only ever asks about running containers.
const RUNNING_FILTER: &str = "filters=%7B%22status%22%3A%5B%22running%22%5D%7D";

/// One container the daemon reports as running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Without the leading `/` the Engine API prepends. Falls back to a
    /// shortened container ID on the (never observed, but not impossible)
    /// chance a container has no name.
    pub name: String,
    /// The reference the container was created with, e.g. `nginx:latest` or
    /// `ghcr.io/org/app:v2`. Exactly what `docker ps` shows under IMAGE.
    pub image_reference: String,
    /// `sha256:...` — the specific local image this container is running,
    /// which is not necessarily the image its tag currently points to if the
    /// tag was re-pulled after this container was created.
    pub image_id: String,
}

/// The running containers the daemon at `endpoint` currently reports.
pub fn list_running_containers(endpoint: &Endpoint) -> Result<Vec<Container>, String> {
    #[derive(Deserialize)]
    struct ContainerSummary {
        #[serde(rename = "Id")]
        id: String,
        #[serde(default, rename = "Names")]
        names: Vec<String>,
        #[serde(rename = "Image")]
        image: String,
        #[serde(rename = "ImageID")]
        image_id: String,
    }

    let path = format!("/containers/json?{RUNNING_FILTER}");
    let resp = get(endpoint, &path)?;
    if resp.status != 200 {
        return Err(format!(
            "GET /containers/json answered {}: {}",
            resp.status,
            body_excerpt(&resp.body)
        ));
    }
    let raw: Vec<ContainerSummary> = serde_json::from_slice(&resp.body)
        .map_err(|e| format!("could not parse the container list: {e}"))?;

    Ok(raw
        .into_iter()
        .map(|c| Container {
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_else(|| c.id.chars().take(12).collect()),
            image_reference: c.image,
            image_id: c.image_id,
        })
        .collect())
}

/// The digests the daemon recorded when `image_id` was last pulled from a
/// registry. Empty for an image that was built locally and never pulled or
/// pushed — there is nothing to compare against a tag in that case.
pub fn repo_digests(endpoint: &Endpoint, image_id: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct ImageInspect {
        #[serde(default, rename = "RepoDigests")]
        repo_digests: Vec<String>,
    }

    let path = format!("/images/{image_id}/json");
    let resp = get(endpoint, &path)?;
    if resp.status != 200 {
        return Err(format!(
            "GET {path} answered {}: {}",
            resp.status,
            body_excerpt(&resp.body)
        ));
    }
    let inspect: ImageInspect = serde_json::from_slice(&resp.body)
        .map_err(|e| format!("could not parse the image inspect: {e}"))?;
    Ok(inspect.repo_digests)
}

/// The manifest digest the registry currently serves for `image_reference`
/// (e.g. `nginx:latest`), as resolved by the daemon.
///
/// This is the one call that reaches outside the host: the daemon contacts the
/// registry to answer it, using whatever registry credentials it is already
/// configured with. An anonymous, unauthenticated lookup is what happens for a
/// public image, which is the only case this module is verified against.
pub fn remote_digest(endpoint: &Endpoint, image_reference: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct DistributionInspect {
        #[serde(rename = "Descriptor")]
        descriptor: Descriptor,
    }
    #[derive(Deserialize)]
    struct Descriptor {
        digest: String,
    }

    // Neither `/` nor `:` needs percent-encoding in a path segment (RFC 3986
    // pchar), and the daemon's own routing for this endpoint expects the
    // reference exactly as `docker pull` would take it — encoding it would be
    // asking for a path this route does not match.
    let path = format!("/distribution/{image_reference}/json");
    let resp = get(endpoint, &path)?;
    if resp.status != 200 {
        return Err(format!(
            "GET {path} answered {}: {}",
            resp.status,
            body_excerpt(&resp.body)
        ));
    }
    let inspect: DistributionInspect = serde_json::from_slice(&resp.body)
        .map_err(|e| format!("could not parse the distribution inspect: {e}"))?;
    Ok(inspect.descriptor.digest)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Response {
    status: u16,
    body: Vec<u8>,
}

fn get(endpoint: &Endpoint, path: &str) -> Result<Response, String> {
    match &endpoint.kind {
        EndpointKind::UnixSocket(socket_path) => unix(socket_path, path, endpoint.timeout),
        EndpointKind::Tcp(addr) => tcp(addr, path, endpoint.timeout),
    }
}

#[cfg(unix)]
fn unix(socket_path: &str, path: &str, timeout: Duration) -> Result<Response, String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("cannot connect to '{socket_path}': {e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    exchange(&mut stream, path)
}

/// muninn ships as a Linux container; this exists only so the workspace builds
/// and tests on a developer's non-Linux machine.
#[cfg(not(unix))]
fn unix(_socket_path: &str, _path: &str, _timeout: Duration) -> Result<Response, String> {
    Err("unix sockets are not available on this platform".to_string())
}

fn tcp(addr: &str, path: &str, timeout: Duration) -> Result<Response, String> {
    let resolved = addr
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve '{addr}': {e}"))?
        .next()
        .ok_or_else(|| format!("'{addr}' resolved to no address"))?;

    let mut stream = TcpStream::connect_timeout(&resolved, timeout)
        .map_err(|e| format!("cannot connect to '{addr}': {e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    exchange(&mut stream, path)
}

fn exchange<S: Read + Write>(stream: &mut S, path: &str) -> Result<Response, String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: docker\r\nAccept: application/json\r\nUser-Agent: muninn\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("request to '{path}' failed: {e}"))?;

    // `Connection: close` was just asked for, so the peer closing the stream is
    // the correct end-of-response signal — the same reasoning that makes
    // HTTP/1.0 sufficient for the `/_ping` probe, extended to a request that
    // also needs the body.
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("reading the response to '{path}' failed: {e}"))?;

    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "the response had no header terminator".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();

    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("no status code in '{status_line}'"))?;

    // Every endpoint this client calls returns a small JSON body, never a
    // stream. A chunked response would be silently mis-parsed as one giant
    // malformed body below, which is worse than refusing it here by name.
    if header(&head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        return Err(
            "the response used chunked transfer encoding, which this client does not \
                     support — none of the endpoints it calls should ever stream"
                .to_string(),
        );
    }

    if let Some(len) = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(len);
    }

    Ok(Response { status, body })
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// The daemon's own error responses are `{"message": "..."}`; fall back to the
/// raw bytes, capped, for anything else that answers on the socket.
fn body_excerpt(body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }
    if let Ok(e) = serde_json::from_slice::<ErrorBody>(body) {
        return e.message;
    }
    String::from_utf8_lossy(body).chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
    fn a_json_body_is_read_in_full() {
        let mut f = Fake::new(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"Id\":\"abc\"}",
        );
        let resp = exchange(&mut f, "/containers/json").unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"{\"Id\":\"abc\"}");
        assert!(String::from_utf8_lossy(&f.written).starts_with("GET /containers/json HTTP/1.1"));
        assert!(String::from_utf8_lossy(&f.written).contains("Connection: close"));
    }

    /// Content-Length is a safety net, not the primary signal — the body must
    /// still be readable when it is absent, because `Connection: close` alone
    /// already delimits the response.
    #[test]
    fn a_body_without_content_length_is_still_read() {
        let mut f = Fake::new("HTTP/1.1 200 OK\r\n\r\n{\"RepoDigests\":[]}");
        let resp = exchange(&mut f, "/images/x/json").unwrap();
        assert_eq!(resp.body, b"{\"RepoDigests\":[]}");
    }

    #[test]
    fn a_non_200_is_reported_with_its_status() {
        let mut f = Fake::new(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 27\r\n\r\n{\"message\":\"no such image\"}",
        );
        let resp = exchange(&mut f, "/images/x/json").unwrap();
        assert_eq!(resp.status, 404);
        assert_eq!(body_excerpt(&resp.body), "no such image");
    }

    #[test]
    fn a_chunked_response_is_refused_by_name() {
        let mut f = Fake::new(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n",
        );
        let e = exchange(&mut f, "/x").unwrap_err();
        assert!(e.contains("chunked"), "{e}");
    }

    #[test]
    fn a_response_with_no_header_terminator_is_rejected() {
        let mut f = Fake::new("not an http response");
        assert!(exchange(&mut f, "/x").is_err());
    }

    #[test]
    fn an_unparseable_error_body_falls_back_to_raw_text() {
        assert_eq!(
            body_excerpt(b"plain text, not json"),
            "plain text, not json"
        );
    }

    #[test]
    fn the_running_filter_decodes_to_the_expected_json() {
        let decoded = RUNNING_FILTER
            .strip_prefix("filters=")
            .unwrap()
            .replace("%7B", "{")
            .replace("%22", "\"")
            .replace("%3A", ":")
            .replace("%5B", "[")
            .replace("%5D", "]")
            .replace("%7D", "}");
        assert_eq!(decoded, r#"{"status":["running"]}"#);
    }
}
