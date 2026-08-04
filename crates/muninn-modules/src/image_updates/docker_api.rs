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
//! rather than needing a response that says how long it is.
//!
//! # What bounds a bad peer
//!
//! Two things, and neither is a total deadline: `set_read_timeout` bounds each
//! individual `read`, not the exchange, so a peer dripping one byte per
//! interval keeps a connection alive indefinitely; and [`MAX_RESPONSE_BYTES`]
//! bounds how much of a response is ever held in memory. The total-time bound
//! is one level up — [`super::check::check`] carries a deadline across all
//! containers, which is what actually stops a slow daemon from running the
//! check past the point Telegraf would kill it.
//!
//! # Transport and parsing are separate
//!
//! Every response shape is parsed by a free function taking `&[u8]`
//! ([`parse_container_list`] and friends), so the mapping from the daemon's
//! JSON to this module's types is testable without a daemon, a socket or a
//! fake stream. [`Client`] is the only part that does I/O, and [`DockerApi`]
//! is the seam [`super::check`] tests its verdict logic against.

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

/// The most of a response this client will hold in memory.
///
/// Every endpoint it calls returns a small JSON document — a container list, an
/// image inspect, a manifest descriptor. Four megabytes is far above any of
/// them and far below a problem. Without it, `read_to_end` on a socket whose
/// peer is compromised, or simply wrong, is an unbounded allocation; the
/// Docker socket is root-equivalent so this is not a trust boundary, but an
/// unbounded read is not something to leave in a client that already refuses
/// chunked encoding for the same class of reason.
pub const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// One container the daemon reports as running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Without the leading `/` the Engine API prepends. Falls back to a
    /// shortened container ID on the (never observed, but not impossible)
    /// chance a container has no name.
    pub name: String,
    /// The reference the container was created with, e.g. `nginx:latest` or
    /// `ghcr.io/org/app:v2`. Exactly what `docker ps` shows under IMAGE —
    /// which, for a container whose tag has since been removed, is an image
    /// ID rather than a reference. [`super::check`] detects that case.
    pub image_reference: String,
    /// `sha256:...` — the specific local image this container is running,
    /// which is not necessarily the image its tag currently points to if the
    /// tag was re-pulled after this container was created.
    pub image_id: String,
}

/// The three calls this module makes, behind a trait.
///
/// Not an abstraction for its own sake: the verdict logic in [`super::check`]
/// is the part of this module worth testing hardest, and it is also the part
/// that would otherwise need a live Docker daemon to reach. With this seam a
/// unit test can hand it any combination of `RepoDigests` and registry digest
/// — including the failure combinations a real daemon is hard to talk into
/// producing — and assert on the verdict.
pub trait DockerApi {
    fn list_running_containers(&self) -> Result<Vec<Container>, String>;
    fn repo_digests(&self, image_id: &str) -> Result<Vec<String>, String>;
    fn remote_digest(&self, image_reference: &str) -> Result<String, String>;
}

/// A [`DockerApi`] backed by a real endpoint.
///
/// Carries two timeouts because the calls are not alike: `/containers/json`
/// and `/images/{id}/json` are answered by the daemon out of its own state,
/// while `/distribution/{ref}/json` makes the daemon perform a TLS handshake,
/// a token exchange and a manifest fetch against a possibly distant registry.
/// One timeout for both would have to be either too loose for the local calls
/// or too tight for the remote one — and too tight there reports
/// `distribution_query_failed` for a registry that was merely slow, which is
/// exactly the confident-wrong-ish answer this module exists to avoid.
pub struct Client {
    endpoint: Endpoint,
    registry_timeout: Duration,
}

impl Client {
    pub fn new(endpoint: Endpoint, registry_timeout: Duration) -> Self {
        Client {
            endpoint,
            registry_timeout,
        }
    }

    /// The same endpoint, with the registry timeout in place of the API one.
    fn registry_endpoint(&self) -> Endpoint {
        Endpoint {
            kind: self.endpoint.kind.clone(),
            timeout: self.registry_timeout,
        }
    }
}

impl DockerApi for Client {
    fn list_running_containers(&self) -> Result<Vec<Container>, String> {
        let path = format!("/containers/json?{RUNNING_FILTER}");
        let resp = get(&self.endpoint, &path)?;
        if resp.status != 200 {
            return Err(format!(
                "GET /containers/json answered {}: {}",
                resp.status,
                body_excerpt(&resp.body)
            ));
        }
        parse_container_list(&resp.body)
    }

    fn repo_digests(&self, image_id: &str) -> Result<Vec<String>, String> {
        let path = format!("/images/{image_id}/json");
        let resp = get(&self.endpoint, &path)?;
        if resp.status != 200 {
            return Err(format!(
                "GET {path} answered {}: {}",
                resp.status,
                body_excerpt(&resp.body)
            ));
        }
        parse_repo_digests(&resp.body)
    }

    fn remote_digest(&self, image_reference: &str) -> Result<String, String> {
        // Neither `/` nor `:` needs percent-encoding in a path segment (RFC 3986
        // pchar), and the daemon's own routing for this endpoint expects the
        // reference exactly as `docker pull` would take it — encoding it would be
        // asking for a path this route does not match.
        let path = format!("/distribution/{image_reference}/json");
        let resp = get(&self.registry_endpoint(), &path)?;
        if resp.status != 200 {
            return Err(format!(
                "GET {path} answered {}: {}",
                resp.status,
                body_excerpt(&resp.body)
            ));
        }
        parse_remote_digest(&resp.body)
    }
}

// ---------------------------------------------------------------------------
// Parsing — no I/O, so every shape below is unit-testable on its own
// ---------------------------------------------------------------------------

/// The running containers in a `GET /containers/json` response.
pub fn parse_container_list(body: &[u8]) -> Result<Vec<Container>, String> {
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

    let raw: Vec<ContainerSummary> = serde_json::from_slice(body)
        .map_err(|e| format!("could not parse the container list: {e}"))?;

    Ok(raw
        .into_iter()
        .map(|c| Container {
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| c.id.chars().take(12).collect()),
            image_reference: c.image,
            image_id: c.image_id,
        })
        .collect())
}

/// The digests the daemon recorded when the inspected image was last pulled
/// from, or pushed to, a registry. Empty for an image that was built locally —
/// there is nothing to compare against a tag in that case.
pub fn parse_repo_digests(body: &[u8]) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct ImageInspect {
        #[serde(default, rename = "RepoDigests")]
        repo_digests: Vec<String>,
    }

    let inspect: ImageInspect = serde_json::from_slice(body)
        .map_err(|e| format!("could not parse the image inspect: {e}"))?;
    Ok(inspect.repo_digests)
}

/// The manifest digest a `GET /distribution/{ref}/json` response carries.
///
/// For a multi-architecture image this is the digest of the manifest *list*,
/// which is also what `RepoDigests` records on a `docker pull` — so the two
/// sides of the comparison are the same kind of digest.
pub fn parse_remote_digest(body: &[u8]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct DistributionInspect {
        #[serde(rename = "Descriptor")]
        descriptor: Descriptor,
    }
    #[derive(Deserialize)]
    struct Descriptor {
        digest: String,
    }

    let inspect: DistributionInspect = serde_json::from_slice(body)
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

/// The one choke point every request in this module passes through, which is
/// exactly where a request line built from daemon-reported strings — an image
/// reference, an image ID — has to be checked before it is sent.
///
/// A real Docker daemon never returns a reference containing a control
/// character; `distribution/reference`'s own grammar forbids it. This does not
/// rely on that holding: a control character here would land in the request
/// line this module writes by hand (`GET {path} HTTP/1.1\r\n...`), and `\r` or
/// `\n` there is a request-line/header injection into whatever is listening on
/// the other end of the socket — the daemon itself, or, in the recommended
/// deployment, a socket proxy. Refusing it here costs one check and removes
/// the assumption entirely rather than resting on an upstream guarantee this
/// module has no way to verify.
fn get(endpoint: &Endpoint, path: &str) -> Result<Response, String> {
    if path.chars().any(|c| c.is_control() || c == ' ') {
        return Err(format!(
            "refusing to send a request whose path contains a control character or space: {path:?}"
        ));
    }
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
    // also needs the body. Read one byte past the cap so hitting it is
    // distinguishable from a response that happens to be exactly that long.
    let mut raw = Vec::new();
    Read::take(&mut *stream, MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| format!("reading the response to '{path}' failed: {e}"))?;
    if raw.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(format!(
            "the response to '{path}' exceeded {MAX_RESPONSE_BYTES} bytes — none of the endpoints \
             this client calls returns anything close to that"
        ));
    }

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

    // ── Parsing ─────────────────────────────────────────────────────────────

    /// A trimmed-down but otherwise verbatim `GET /containers/json` response.
    const CONTAINER_LIST: &str = r#"[
      {"Id":"9c1f2b3a4d5e6f708192a3b4c5d6e7f8",
       "Names":["/web"],
       "Image":"nginx:1.25",
       "ImageID":"sha256:aaaa",
       "State":"running"},
      {"Id":"1a2b3c4d5e6f708192a3b4c5d6e7f809",
       "Names":["/db","/compose_db_1"],
       "Image":"postgres:16",
       "ImageID":"sha256:bbbb",
       "State":"running"}
    ]"#;

    #[test]
    fn the_container_list_maps_names_images_and_ids() {
        let got = parse_container_list(CONTAINER_LIST.as_bytes()).unwrap();
        assert_eq!(
            got,
            vec![
                Container {
                    name: "web".to_string(),
                    image_reference: "nginx:1.25".to_string(),
                    image_id: "sha256:aaaa".to_string(),
                },
                Container {
                    name: "db".to_string(),
                    image_reference: "postgres:16".to_string(),
                    image_id: "sha256:bbbb".to_string(),
                },
            ]
        );
    }

    /// The Engine API prepends `/` to every name, and muninn's metric tag must
    /// not carry it — `container_name="/web"` would not match what an operator
    /// types into `container_include`.
    #[test]
    fn the_leading_slash_the_engine_api_adds_is_stripped() {
        let got = parse_container_list(
            br#"[{"Id":"abc","Names":["/web"],"Image":"nginx","ImageID":"sha256:a"}]"#,
        )
        .unwrap();
        assert_eq!(got[0].name, "web");
    }

    #[test]
    fn a_container_with_no_name_falls_back_to_a_short_id() {
        let got = parse_container_list(
            br#"[{"Id":"9c1f2b3a4d5e6f708192a3b4c5d6e7f8","Names":[],"Image":"nginx","ImageID":"sha256:a"}]"#,
        )
        .unwrap();
        assert_eq!(got[0].name, "9c1f2b3a4d5e");
    }

    /// `Names` absent entirely, not merely empty — the field is `#[serde(default)]`
    /// precisely so an older API version cannot make the whole list unparseable.
    #[test]
    fn a_missing_names_field_does_not_fail_the_whole_list() {
        let got = parse_container_list(
            br#"[{"Id":"abcdefghijklmno","Image":"nginx","ImageID":"sha256:a"}]"#,
        )
        .unwrap();
        assert_eq!(got[0].name, "abcdefghijkl");
    }

    #[test]
    fn an_empty_container_list_is_not_an_error() {
        assert!(parse_container_list(b"[]").unwrap().is_empty());
    }

    #[test]
    fn a_container_list_that_is_not_json_is_named_as_such() {
        let e = parse_container_list(b"<html>nope</html>").unwrap_err();
        assert!(e.contains("container list"), "{e}");
    }

    #[test]
    fn repo_digests_are_read_from_the_image_inspect() {
        let got = parse_repo_digests(
            br#"{"Id":"sha256:aaaa","RepoTags":["nginx:1.25"],"RepoDigests":["nginx@sha256:dead","nginx@sha256:beef"]}"#,
        )
        .unwrap();
        assert_eq!(got, vec!["nginx@sha256:dead", "nginx@sha256:beef"]);
    }

    /// A locally built image has no `RepoDigests` at all in some API versions
    /// and an empty array in others. Both must mean the same thing, and
    /// neither may be an error — the caller turns emptiness into a reason.
    #[test]
    fn an_image_that_was_never_pulled_has_no_repo_digests() {
        assert!(
            parse_repo_digests(br#"{"Id":"sha256:aaaa"}"#)
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_repo_digests(br#"{"Id":"sha256:aaaa","RepoDigests":[]}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_remote_digest_is_read_from_the_descriptor() {
        let got = parse_remote_digest(
            br#"{"Descriptor":{"mediaType":"application/vnd.oci.image.index.v1+json","digest":"sha256:cafe","size":1234},"Platforms":[]}"#,
        )
        .unwrap();
        assert_eq!(got, "sha256:cafe");
    }

    #[test]
    fn a_distribution_response_without_a_descriptor_is_an_error() {
        let e = parse_remote_digest(br#"{"Platforms":[]}"#).unwrap_err();
        assert!(e.contains("distribution inspect"), "{e}");
    }

    // ── Transport ───────────────────────────────────────────────────────────

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

    /// `read_to_end` on a socket is an unbounded allocation, and none of the
    /// endpoints this client calls has any business returning megabytes.
    #[test]
    fn a_response_past_the_size_cap_is_refused_rather_than_buffered() {
        let mut oversized = "HTTP/1.1 200 OK\r\n\r\n".to_string();
        oversized.push_str(&"a".repeat(MAX_RESPONSE_BYTES as usize + 1));
        let mut f = Fake::new(&oversized);
        let e = exchange(&mut f, "/containers/json").unwrap_err();
        assert!(e.contains("exceeded"), "{e}");
    }

    /// `get` is the one choke point every path this module builds passes
    /// through, so the check belongs there rather than at each call site —
    /// and it must run *before* any I/O, which this proves by pointing the
    /// endpoint at a port nothing listens on: the error names the control
    /// character, not a connection failure.
    #[test]
    fn a_control_character_in_the_path_is_refused_before_any_request_is_sent() {
        let endpoint = Endpoint {
            kind: EndpointKind::Tcp("127.0.0.1:0".to_string()),
            timeout: Duration::from_millis(50),
        };
        for path in ["/images/x\r\nGET /secret HTTP/1.1/json", "/images/x y/json"] {
            let e = get(&endpoint, path).unwrap_err();
            assert!(
                e.contains("control character") || e.contains("space"),
                "{path:?}: {e}"
            );
        }
    }

    #[test]
    fn an_ordinary_path_is_not_affected_by_the_guard() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        drop(held);
        let endpoint = Endpoint {
            kind: EndpointKind::Tcp(addr.to_string()),
            timeout: Duration::from_millis(50),
        };
        let e = get(&endpoint, "/distribution/nginx:latest/json").unwrap_err();
        assert!(
            e.contains("cannot connect"),
            "an ordinary path should reach the connection attempt: {e}"
        );
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

    /// The registry call is the only one that leaves the host, and it is the
    /// only one that gets the longer timeout — a 5s default that is right for
    /// a local socket call is wrong for a TLS handshake plus a token exchange
    /// plus a manifest fetch.
    #[test]
    fn only_the_registry_call_uses_the_registry_timeout() {
        let client = Client::new(
            Endpoint {
                kind: EndpointKind::Tcp("127.0.0.1:1".to_string()),
                timeout: Duration::from_secs(5),
            },
            Duration::from_secs(30),
        );
        assert_eq!(client.endpoint.timeout, Duration::from_secs(5));
        assert_eq!(client.registry_endpoint().timeout, Duration::from_secs(30));
        assert_eq!(client.registry_endpoint().kind, client.endpoint.kind);
    }
}
