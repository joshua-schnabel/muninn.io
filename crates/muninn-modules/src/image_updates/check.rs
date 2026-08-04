//! Whether a newer image is available, under the same tag, for each running
//! container.
//!
//! # The invariant
//!
//! Exactly the one [the updates module](../updates/debian.rs) stands on:
//! `check_success=0` and no verdict, never a confident wrong answer. A
//! container whose image was built locally, or whose registry cannot be
//! reached, does not report "up to date" — it reports why it could not say.
//!
//! # Why the comparison is a digest, not a version string
//!
//! A tag like `latest` or `1.25` is a mutable pointer the registry can move at
//! any time; there is no "newer" to parse out of it. What can be compared is
//! *content*: the manifest digest the tag currently resolves to on the
//! registry, against the digest the daemon recorded when this container's
//! image was pulled ([`Container::image_id`](super::docker_api::Container) →
//! `RepoDigests`). Different digest, same tag, means the tag moved since this
//! container was created.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Endpoint;

use super::docker_api;

/// The measurement for the one check that covers the whole daemon: could the
/// running containers even be listed.
pub const MEASUREMENT_CHECK: &str = "muninn_image_updates";
/// The measurement for one line per running, filtered container.
pub const MEASUREMENT_CONTAINER: &str = "muninn_container_image_updates";

/// Why the daemon-level part of the check could not produce an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonReason {
    /// `--endpoint` was not `unix://...` or `tcp://...` with something after
    /// the scheme. Validation rejects this before it is ever rendered, so in
    /// practice this only fires when `muninn image-check` is run by hand.
    InvalidEndpoint,
    DockerUnreachable,
}

impl DaemonReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DaemonReason::InvalidEndpoint => "invalid_endpoint",
            DaemonReason::DockerUnreachable => "docker_unreachable",
        }
    }
}

/// Why one container's image could not be judged.
///
/// A closed set of short tokens, like [`updates::debian::Reason`](crate::updates::debian::Reason) —
/// this becomes a metric tag, so it stays a fixed vocabulary rather than a
/// place for a raw error to leak into a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerReason {
    /// The container's image reference is already pinned to a digest
    /// (`repo@sha256:...`). There is no tag for a newer image to appear under.
    DigestPinnedReference,
    /// The image was never pulled from, or pushed to, a registry — built
    /// locally, most likely. Nothing to compare its tag against.
    NoRepoDigest,
    /// `RepoDigests` holds entries, but none for the repository this
    /// container's tag names — a `docker tag` onto an image pulled under a
    /// different name, most likely.
    NoMatchingRepoDigest,
    /// `GET /images/{id}/json` failed.
    ImageInspectFailed,
    /// `GET /distribution/{name}/json` failed — the daemon could not resolve
    /// the tag against the registry: unreachable, requires authentication the
    /// daemon does not have, or the tag no longer exists.
    DistributionQueryFailed,
}

impl ContainerReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerReason::DigestPinnedReference => "digest_pinned_reference",
            ContainerReason::NoRepoDigest => "no_repo_digest",
            ContainerReason::NoMatchingRepoDigest => "no_matching_repo_digest",
            ContainerReason::ImageInspectFailed => "image_inspect_failed",
            ContainerReason::DistributionQueryFailed => "distribution_query_failed",
        }
    }

    /// Every reason, for the test that keeps the tokens unique.
    pub const ALL: [ContainerReason; 5] = [
        ContainerReason::DigestPinnedReference,
        ContainerReason::NoRepoDigest,
        ContainerReason::NoMatchingRepoDigest,
        ContainerReason::ImageInspectFailed,
        ContainerReason::DistributionQueryFailed,
    ];
}

/// The verdict for one container: never both a reason and an answer.
#[derive(Debug, Clone)]
pub struct ContainerCheck {
    pub name: String,
    pub image: String,
    /// `Ok(true)` — the registry's digest for this tag differs from the one
    /// this container was started with. `Ok(false)` — they match.
    pub outcome: Result<bool, ContainerReason>,
    pub at: u64,
    pub detail: Option<String>,
}

impl ContainerCheck {
    fn line_protocol(&self) -> String {
        let name = escape_tag(&self.name);
        let image = escape_tag(&self.image);
        match self.outcome {
            Err(reason) => format!(
                "{MEASUREMENT_CONTAINER},container_name={name},image={image},status=error,\
                 reason={} check_success=0i,check_timestamp_seconds={}i\n",
                reason.as_str(),
                self.at
            ),
            Ok(available) => format!(
                "{MEASUREMENT_CONTAINER},container_name={name},image={image},status=ok,\
                 reason=none check_success=1i,check_timestamp_seconds={}i\n\
                 {MEASUREMENT_CONTAINER},container_name={name},image={image} \
                 update_available={}i\n",
                self.at, available as i32,
            ),
        }
    }
}

/// The result of one run: the daemon-level outcome, and one [`ContainerCheck`]
/// per running container that passed the include/exclude filters.
///
/// `containers` is always empty when `daemon_outcome` is `Err` — there was
/// nothing to enumerate, so there is nothing to report per container either.
#[derive(Debug, Clone)]
pub struct Report {
    pub daemon_outcome: Result<usize, DaemonReason>,
    pub containers: Vec<ContainerCheck>,
    pub at: u64,
    pub detail: Option<String>,
}

impl Report {
    pub fn daemon_succeeded(&self) -> bool {
        self.daemon_outcome.is_ok()
    }

    /// The influx line protocol Telegraf's `inputs.exec` parses. Follows the
    /// same shape as the updates module: a check line carrying `status` and
    /// `reason`, present whichever way the check went, then the data —
    /// here, one check line and one verdict line per container instead of one
    /// shared pair of counts.
    pub fn line_protocol(&self) -> String {
        match self.daemon_outcome {
            Err(reason) => format!(
                "{MEASUREMENT_CHECK},status=error,reason={} check_success=0i,\
                 check_timestamp_seconds={}i\n",
                reason.as_str(),
                self.at
            ),
            Ok(count) => {
                let mut out = format!(
                    "{MEASUREMENT_CHECK},status=ok,reason=none check_success=1i,\
                     check_timestamp_seconds={}i,containers_checked={count}i\n",
                    self.at
                );
                for c in &self.containers {
                    out.push_str(&c.line_protocol());
                }
                out
            }
        }
    }
}

/// Run one check against the Docker daemon named by `endpoint`
/// (`unix:///var/run/docker.sock` or `tcp://host:port`), waiting up to
/// `timeout` for each individual Docker API call.
///
/// `include`/`exclude` are container-name globs with the same semantics as
/// `modules.docker.container_include`/`container_exclude`: an include list is
/// an allow-list, and an exclude list is applied within it.
pub fn check(endpoint: &str, timeout: Duration, include: &[String], exclude: &[String]) -> Report {
    let at = now();

    // Validation rejects a malformed endpoint before it is ever rendered, so
    // this only fires when the command is run by hand with a typo — but this
    // is a documented command an operator can do that with, so it gets the
    // same honest failure as every other precondition here rather than a
    // panic.
    let Some(parsed) = crate::inputs::parse_docker_endpoint(endpoint, timeout) else {
        return Report {
            daemon_outcome: Err(DaemonReason::InvalidEndpoint),
            containers: Vec::new(),
            at,
            detail: Some(format!(
                "'{endpoint}' must be unix://<path> or tcp://<host>:<port>"
            )),
        };
    };
    let endpoint = &parsed;

    let containers = match docker_api::list_running_containers(endpoint) {
        Ok(c) => c,
        Err(e) => {
            return Report {
                daemon_outcome: Err(DaemonReason::DockerUnreachable),
                containers: Vec::new(),
                at,
                detail: Some(e),
            };
        }
    };

    let filtered: Vec<_> = containers
        .into_iter()
        .filter(|c| passes_filters(&c.name, include, exclude))
        .collect();

    let containers: Vec<ContainerCheck> = filtered
        .into_iter()
        .map(|c| check_one(endpoint, c))
        .collect();

    Report {
        daemon_outcome: Ok(containers.len()),
        containers,
        at,
        detail: None,
    }
}

fn check_one(endpoint: &Endpoint, container: docker_api::Container) -> ContainerCheck {
    let docker_api::Container {
        name,
        image_reference: image,
        image_id,
    } = container;

    let Some((repo, tag)) = split_reference(&image) else {
        return ContainerCheck {
            name,
            image,
            outcome: Err(ContainerReason::DigestPinnedReference),
            at: now(),
            detail: None,
        };
    };

    let digests = match docker_api::repo_digests(endpoint, &image_id) {
        Ok(d) => d,
        Err(e) => {
            return ContainerCheck {
                name,
                image,
                outcome: Err(ContainerReason::ImageInspectFailed),
                at: now(),
                detail: Some(e),
            };
        }
    };

    let prefix = format!("{repo}@");
    let local_digest = digests.iter().find_map(|d| d.strip_prefix(&prefix));

    let Some(local_digest) = local_digest else {
        let reason = if digests.is_empty() {
            ContainerReason::NoRepoDigest
        } else {
            ContainerReason::NoMatchingRepoDigest
        };
        return ContainerCheck {
            name,
            image,
            outcome: Err(reason),
            at: now(),
            detail: None,
        };
    };
    let local_digest = local_digest.to_string();

    let remote_digest = match docker_api::remote_digest(endpoint, &format!("{repo}:{tag}")) {
        Ok(d) => d,
        Err(e) => {
            return ContainerCheck {
                name,
                image,
                outcome: Err(ContainerReason::DistributionQueryFailed),
                at: now(),
                detail: Some(e),
            };
        }
    };

    ContainerCheck {
        name,
        image,
        outcome: Ok(local_digest != remote_digest),
        at: now(),
        detail: None,
    }
}

/// Split a Docker image reference into `(repository, tag)`.
///
/// `None` for a reference already pinned to a digest (`repo@sha256:...`) —
/// there is no tag there for a newer image to appear under.
///
/// The tag is the part after the last `:` that comes after the last `/`, so a
/// registry port (`registry.example.com:5000/app`) is never mistaken for one.
/// No tag at all defaults to `latest`, matching what the daemon itself assumes.
fn split_reference(image: &str) -> Option<(String, String)> {
    if image.contains('@') {
        return None;
    }
    let search_from = image.rfind('/').map(|i| i + 1).unwrap_or(0);
    match image[search_from..].rfind(':') {
        Some(rel) => {
            let idx = search_from + rel;
            Some((image[..idx].to_string(), image[idx + 1..].to_string()))
        }
        None => Some((image.to_string(), "latest".to_string())),
    }
}

/// `*` matches any run of characters, `?` matches exactly one. The vocabulary
/// every include/exclude example in the docs already uses (`"veth*"`,
/// `"/snap*"`), and no more — Telegraf's own filters do the matching for every
/// other module, but this module enumerates containers itself, so the
/// filtering has to happen in muninn rather than being rendered into a plugin
/// option.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_from = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_from = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            match_from += 1;
            ti = match_from;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn passes_filters(name: &str, include: &[String], exclude: &[String]) -> bool {
    if !include.is_empty() && !include.iter().any(|p| glob_match(p, name)) {
        return false;
    }
    !exclude.iter().any(|p| glob_match(p, name))
}

/// Influx line protocol escaping for a tag value: comma, equals sign and space
/// are the delimiters the format itself uses.
fn escape_tag(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace('=', "\\=")
        .replace(' ', "\\ ")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The top-level entry point ───────────────────────────────────────────

    /// Validation rejects this before rendering, but `image-check` is a
    /// documented command an operator can run by hand — a typo there must
    /// produce the same honest failure as every other precondition, not a
    /// panic.
    #[test]
    fn an_endpoint_with_no_known_scheme_is_a_daemon_level_failure() {
        let report = check("/var/run/docker.sock", Duration::from_secs(1), &[], &[]);
        assert_eq!(report.daemon_outcome, Err(DaemonReason::InvalidEndpoint));
        assert!(report.containers.is_empty());
    }

    /// Port 0 on the loopback is never listening, so this exercises the real
    /// connection-refused path rather than a fake stream — the same technique
    /// `muninn/src/probe.rs` uses for the same reason.
    #[test]
    fn an_unreachable_daemon_is_a_daemon_level_failure_not_a_panic() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        drop(held);

        let report = check(
            &format!("tcp://127.0.0.1:{port}"),
            Duration::from_secs(2),
            &[],
            &[],
        );
        assert_eq!(report.daemon_outcome, Err(DaemonReason::DockerUnreachable));
        assert!(report.containers.is_empty());
        assert!(report.detail.is_some());
    }

    // ── Reference splitting ─────────────────────────────────────────────────

    #[test]
    fn a_bare_name_defaults_to_the_latest_tag() {
        assert_eq!(
            split_reference("nginx"),
            Some(("nginx".to_string(), "latest".to_string()))
        );
    }

    #[test]
    fn an_explicit_tag_is_split_off() {
        assert_eq!(
            split_reference("nginx:1.25"),
            Some(("nginx".to_string(), "1.25".to_string()))
        );
    }

    /// The case the naive `rsplit_once(':')` would get wrong: a registry port
    /// must not be mistaken for a tag separator.
    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(
            split_reference("registry.example.com:5000/team/app"),
            Some((
                "registry.example.com:5000/team/app".to_string(),
                "latest".to_string()
            ))
        );
        assert_eq!(
            split_reference("registry.example.com:5000/team/app:v2"),
            Some((
                "registry.example.com:5000/team/app".to_string(),
                "v2".to_string()
            ))
        );
    }

    #[test]
    fn a_digest_pinned_reference_has_no_tag() {
        assert_eq!(
            split_reference("nginx@sha256:abcdef0123456789"),
            None,
            "a digest-pinned reference has nothing to compare a tag against"
        );
    }

    // ── Glob matching ───────────────────────────────────────────────────────

    #[test]
    fn a_star_matches_any_suffix() {
        assert!(glob_match("build-*", "build-agent-1"));
        assert!(!glob_match("build-*", "worker-1"));
    }

    #[test]
    fn an_exact_pattern_matches_only_itself() {
        assert!(glob_match("redis", "redis"));
        assert!(!glob_match("redis", "redis-2"));
    }

    #[test]
    fn a_lone_star_matches_everything_including_empty() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(glob_match("db?", "db1"));
        assert!(!glob_match("db?", "db12"));
    }

    // ── Filtering ────────────────────────────────────────────────────────────

    #[test]
    fn an_empty_include_list_admits_everything_not_excluded() {
        assert!(passes_filters("anything", &[], &[]));
        assert!(!passes_filters(
            "build-agent",
            &[],
            &["build-*".to_string()]
        ));
    }

    #[test]
    fn a_non_empty_include_list_is_an_allow_list() {
        let include = vec!["app-*".to_string()];
        assert!(passes_filters("app-1", &include, &[]));
        assert!(!passes_filters("worker-1", &include, &[]));
    }

    // ── Line protocol ────────────────────────────────────────────────────────

    #[test]
    fn a_daemon_failure_omits_every_container_line() {
        let report = Report {
            daemon_outcome: Err(DaemonReason::DockerUnreachable),
            containers: Vec::new(),
            at: 1_754_000_000,
            detail: Some("connection refused".to_string()),
        };
        let out = report.line_protocol();
        assert!(out.contains("check_success=0i"), "{out}");
        assert!(out.contains("reason=docker_unreachable"), "{out}");
        assert!(!out.contains(MEASUREMENT_CONTAINER), "{out}");
    }

    #[test]
    fn an_available_update_is_reported_as_one() {
        let check = ContainerCheck {
            name: "web".to_string(),
            image: "nginx:latest".to_string(),
            outcome: Ok(true),
            at: 1_754_000_000,
            detail: None,
        };
        let out = check.line_protocol();
        assert!(
            out.contains("muninn_container_image_updates,container_name=web,image=nginx:latest update_available=1i"),
            "{out}"
        );
        assert!(out.contains("check_success=1i"), "{out}");
    }

    #[test]
    fn an_up_to_date_container_reports_zero_not_a_missing_metric() {
        let check = ContainerCheck {
            name: "web".to_string(),
            image: "nginx:latest".to_string(),
            outcome: Ok(false),
            at: 1_754_000_000,
            detail: None,
        };
        assert!(check.line_protocol().contains("update_available=0i"));
    }

    /// The module's whole reason to exist, stated as a test: a container whose
    /// verdict could not be determined must never carry `update_available`
    /// either way — that would be indistinguishable from a real answer.
    #[test]
    fn a_failed_container_check_never_reports_update_available() {
        for reason in ContainerReason::ALL {
            let check = ContainerCheck {
                name: "web".to_string(),
                image: "nginx:latest".to_string(),
                outcome: Err(reason),
                at: 1_754_000_000,
                detail: None,
            };
            let out = check.line_protocol();
            assert!(out.contains("check_success=0i"), "{reason:?}: {out}");
            assert!(
                !out.contains("update_available"),
                "{reason:?} must not carry a verdict: {out}"
            );
        }
    }

    #[test]
    fn container_reasons_are_unique_low_cardinality_tokens() {
        let mut seen = std::collections::HashSet::new();
        for r in ContainerReason::ALL {
            let s = r.as_str();
            assert!(seen.insert(s), "{s} appears twice");
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{s} is not a plain token"
            );
        }
    }

    #[test]
    fn tag_values_with_line_protocol_delimiters_are_escaped() {
        assert_eq!(escape_tag("a,b=c d"), "a\\,b\\=c\\ d");
    }
}
