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
//!
//! # Why the repository name is normalised before it is compared
//!
//! The daemon spells one repository two ways. A container created as
//! `docker.io/library/nginx:latest` reports exactly that under `Image`, while
//! the image it runs records the *familiar* `nginx@sha256:...` under
//! `RepoDigests`. Comparing the two literally finds no match and reports
//! `no_matching_repo_digest` for an entirely ordinary container. Both sides go
//! through [`normalize_repository`] first.
//!
//! # Why there is a budget
//!
//! Each container costs up to two Docker API calls, one of which waits on a
//! registry. Telegraf kills an `inputs.exec` helper that overruns its timeout,
//! and a killed helper reports *nothing* — not even the failures it had
//! already established. So the check carries its own budget: when it runs out,
//! the containers not yet reached report `reason=budget_exceeded` and
//! everything already determined is still emitted. Partial truth beats a dead
//! process.

use std::time::{Duration, Instant};

use super::docker_api::{self, Container, DockerApi};
use crate::unix_now;

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
    /// The daemon reported an image *ID* where a reference belongs, which is
    /// what `docker ps` shows once every tag for a running container's image
    /// has been removed. There is no repository or tag left to resolve.
    ImageIdReference,
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
    /// The check ran out of its budget before reaching this container. Not a
    /// property of the container at all — a property of how many there were —
    /// but it is still the honest answer for this series, and it is emitted
    /// rather than dropped so the gap is visible instead of looking like a
    /// container that quietly stopped being collected.
    BudgetExceeded,
}

impl ContainerReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerReason::DigestPinnedReference => "digest_pinned_reference",
            ContainerReason::ImageIdReference => "image_id_reference",
            ContainerReason::NoRepoDigest => "no_repo_digest",
            ContainerReason::NoMatchingRepoDigest => "no_matching_repo_digest",
            ContainerReason::ImageInspectFailed => "image_inspect_failed",
            ContainerReason::DistributionQueryFailed => "distribution_query_failed",
            ContainerReason::BudgetExceeded => "budget_exceeded",
        }
    }

    /// Every reason, for the test that keeps the tokens unique.
    pub const ALL: [ContainerReason; 7] = [
        ContainerReason::DigestPinnedReference,
        ContainerReason::ImageIdReference,
        ContainerReason::NoRepoDigest,
        ContainerReason::NoMatchingRepoDigest,
        ContainerReason::ImageInspectFailed,
        ContainerReason::DistributionQueryFailed,
        ContainerReason::BudgetExceeded,
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
/// (`unix:///var/run/docker.sock` or `tcp://host:port`).
///
/// `timeout` bounds each call the daemon answers out of its own state;
/// `registry_timeout` bounds the one call that makes the daemon reach a
/// registry; `budget` bounds the whole run. See the module documentation for
/// why the last two are not the first.
///
/// `include`/`exclude` are container-name globs with the same semantics as
/// `modules.docker.container_include`/`container_exclude`: an include list is
/// an allow-list, and an exclude list is applied within it.
pub fn check(
    endpoint: &str,
    timeout: Duration,
    registry_timeout: Duration,
    budget: Duration,
    include: &[String],
    exclude: &[String],
) -> Report {
    // Validation rejects a malformed endpoint before it is ever rendered, so
    // this only fires when the command is run by hand with a typo — but this
    // is a documented command an operator can do that with, so it gets the
    // same honest failure as every other precondition here rather than a
    // panic.
    let Some(parsed) = crate::inputs::parse_docker_endpoint(endpoint, timeout) else {
        return Report {
            daemon_outcome: Err(DaemonReason::InvalidEndpoint),
            containers: Vec::new(),
            at: unix_now(),
            detail: Some(format!(
                "'{endpoint}' must be unix://<path> or tcp://<host>:<port>"
            )),
        };
    };

    run(
        &docker_api::Client::new(parsed, registry_timeout),
        budget,
        include,
        exclude,
    )
}

/// The whole check, against anything that can answer the three calls.
///
/// Separate from [`check`], which is only endpoint parsing and client
/// construction, so that everything below this line is testable without a
/// daemon, a socket or a fake stream.
pub fn run<A: DockerApi>(
    api: &A,
    budget: Duration,
    include: &[String],
    exclude: &[String],
) -> Report {
    let at = unix_now();
    let started = Instant::now();

    let containers = match api.list_running_containers() {
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

    let containers: Vec<ContainerCheck> = containers
        .into_iter()
        .filter(|c| passes_filters(&c.name, include, exclude))
        .map(|c| {
            if started.elapsed() >= budget {
                ContainerCheck {
                    name: c.name,
                    image: c.image_reference,
                    outcome: Err(ContainerReason::BudgetExceeded),
                    at: unix_now(),
                    detail: None,
                }
            } else {
                check_one(api, c)
            }
        })
        .collect();

    Report {
        daemon_outcome: Ok(containers.len()),
        containers,
        at,
        detail: None,
    }
}

fn check_one<A: DockerApi>(api: &A, container: Container) -> ContainerCheck {
    let Container {
        name,
        image_reference: image,
        image_id,
    } = container;

    let fail = |reason, detail| ContainerCheck {
        name: name.clone(),
        image: image.clone(),
        outcome: Err(reason),
        at: unix_now(),
        detail,
    };

    if is_image_id(&image, &image_id) {
        return fail(ContainerReason::ImageIdReference, None);
    }

    let Some((repo, tag)) = split_reference(&image) else {
        return fail(ContainerReason::DigestPinnedReference, None);
    };

    let digests = match api.repo_digests(&image_id) {
        Ok(d) => d,
        Err(e) => return fail(ContainerReason::ImageInspectFailed, Some(e)),
    };

    let Some(local_digest) = find_local_digest(&digests, &repo) else {
        return fail(
            if digests.is_empty() {
                ContainerReason::NoRepoDigest
            } else {
                ContainerReason::NoMatchingRepoDigest
            },
            None,
        );
    };

    let remote_digest = match api.remote_digest(&format!("{repo}:{tag}")) {
        Ok(d) => d,
        Err(e) => return fail(ContainerReason::DistributionQueryFailed, Some(e)),
    };

    ContainerCheck {
        name,
        image,
        outcome: Ok(local_digest != remote_digest),
        at: unix_now(),
        detail: None,
    }
}

/// Whether the daemon put an image ID where a reference belongs.
///
/// `docker ps` shows the image ID under IMAGE once every tag for a running
/// container's image has been removed. Splitting that as a reference would
/// produce a repository of `sha256` and a tag of the hex digest, which then
/// matches no `RepoDigests` entry and reports `no_matching_repo_digest` — a
/// true answer to a nonsense question. Only the two forms the daemon actually
/// emits are recognised; bare hex is not, because a tag is allowed to be hex.
fn is_image_id(image: &str, image_id: &str) -> bool {
    image == image_id || image.starts_with("sha256:")
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

/// A repository name in the one spelling both sides of the comparison can be
/// held to.
///
/// Docker's own normalisation, and only the part of it this module needs: a
/// first component containing `.` or `:`, or equal to `localhost`, is a
/// registry host; anything else is Docker Hub, where a single-component name
/// lives under `library/`. `index.docker.io` is the older spelling of
/// `docker.io` and still appears in configurations, so it folds into it.
fn normalize_repository(repo: &str) -> String {
    let (first, rest) = match repo.split_once('/') {
        Some((first, rest)) => (first, Some(rest)),
        None => (repo, None),
    };

    let is_registry_host = first.contains('.') || first.contains(':') || first == "localhost";

    match (is_registry_host, rest) {
        // Docker Hub, spelled long. Everything else with a registry host is
        // already canonical.
        (true, Some(rest)) if first == "docker.io" || first == "index.docker.io" => {
            if rest.contains('/') {
                format!("docker.io/{rest}")
            } else {
                format!("docker.io/library/{rest}")
            }
        }
        (true, _) => repo.to_string(),
        // Docker Hub, spelled short.
        (false, Some(rest)) => format!("docker.io/{first}/{rest}"),
        (false, None) => format!("docker.io/library/{first}"),
    }
}

/// The digest `RepoDigests` records for `repo`, if it holds one.
///
/// Entries are `repository@sha256:...`, and a repository name can never
/// contain `@`, so the first one splits the two halves.
fn find_local_digest(digests: &[String], repo: &str) -> Option<String> {
    let want = normalize_repository(repo);
    digests.iter().find_map(|d| {
        let (entry_repo, digest) = d.split_once('@')?;
        (normalize_repository(entry_repo) == want).then(|| digest.to_string())
    })
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
///
/// Control characters are **replaced, not escaped**, because line protocol has
/// no escape for them: a newline ends the line, and everything after it parses
/// as another measurement. Both values this is applied to — a container name
/// and an image reference — come from the daemon rather than from muninn,
/// which is the same reason `docker_api::get` refuses a control character in a
/// request path. Docker's grammar cannot produce one; neither place rests on
/// that. Fabricating a metric series is the line-protocol equivalent of the
/// request-line injection that guard exists for.
fn escape_tag(v: &str) -> String {
    v.chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace('=', "\\=")
        .replace(' ', "\\ ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A [`DockerApi`] that answers from a script rather than from a daemon.
    ///
    /// This is the point of the seam: every branch of [`check_one`] is
    /// reachable here, including the ones a real daemon is hard to talk into
    /// producing — an image whose `RepoDigests` names a different repository,
    /// a distribution query that fails while the image inspect succeeded.
    struct FakeApi {
        containers: Result<Vec<Container>, String>,
        digests: Vec<(String, Result<Vec<String>, String>)>,
        remote: Vec<(String, Result<String, String>)>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeApi {
        fn with(containers: Vec<Container>) -> Self {
            FakeApi {
                containers: Ok(containers),
                digests: Vec::new(),
                remote: Vec::new(),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn unreachable() -> Self {
            FakeApi {
                containers: Err("connection refused".to_string()),
                digests: Vec::new(),
                remote: Vec::new(),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn digest(mut self, image_id: &str, digests: Result<Vec<String>, String>) -> Self {
            self.digests.push((image_id.to_string(), digests));
            self
        }

        fn remote(mut self, reference: &str, digest: Result<String, String>) -> Self {
            self.remote.push((reference.to_string(), digest));
            self
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    impl DockerApi for FakeApi {
        fn list_running_containers(&self) -> Result<Vec<Container>, String> {
            self.containers.clone()
        }

        fn repo_digests(&self, image_id: &str) -> Result<Vec<String>, String> {
            self.asked.borrow_mut().push(format!("inspect {image_id}"));
            self.digests
                .iter()
                .find(|(id, _)| id == image_id)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| Err(format!("no scripted inspect for {image_id}")))
        }

        fn remote_digest(&self, reference: &str) -> Result<String, String> {
            self.asked.borrow_mut().push(format!("resolve {reference}"));
            self.remote
                .iter()
                .find(|(r, _)| r == reference)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| Err(format!("no scripted resolve for {reference}")))
        }
    }

    fn container(name: &str, image: &str, image_id: &str) -> Container {
        Container {
            name: name.to_string(),
            image_reference: image.to_string(),
            image_id: image_id.to_string(),
        }
    }

    /// Long enough that no test in this file is about the budget unless it
    /// says so.
    const GENEROUS: Duration = Duration::from_secs(60);

    fn only(report: &Report) -> &ContainerCheck {
        assert_eq!(report.containers.len(), 1, "expected exactly one container");
        &report.containers[0]
    }

    // ── The verdict ─────────────────────────────────────────────────────────

    #[test]
    fn a_tag_that_moved_since_the_container_started_is_an_available_update() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec!["nginx@sha256:old".to_string()]))
            .remote("nginx:1.25", Ok("sha256:new".to_string()));
        assert_eq!(only(&run(&api, GENEROUS, &[], &[])).outcome, Ok(true));
    }

    #[test]
    fn a_tag_that_still_resolves_to_the_running_image_is_up_to_date() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec!["nginx@sha256:same".to_string()]))
            .remote("nginx:1.25", Ok("sha256:same".to_string()));
        assert_eq!(only(&run(&api, GENEROUS, &[], &[])).outcome, Ok(false));
    }

    /// A missing tag means `latest`, and the resolve has to ask for it
    /// explicitly — `/distribution/nginx/json` is a different route.
    #[test]
    fn a_bare_reference_is_resolved_against_the_latest_tag() {
        let api = FakeApi::with(vec![container("web", "nginx", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec!["nginx@sha256:x".to_string()]))
            .remote("nginx:latest", Ok("sha256:x".to_string()));
        assert_eq!(only(&run(&api, GENEROUS, &[], &[])).outcome, Ok(false));
        assert!(api.asked().contains(&"resolve nginx:latest".to_string()));
    }

    /// The image is looked up by the ID the container is *running*, not by its
    /// tag — that is what makes the verdict about this container rather than
    /// about whatever the tag points to on the host right now. ADR-0013.
    #[test]
    fn the_image_is_inspected_by_the_running_id_not_by_the_tag() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:running")])
            .digest("sha256:running", Ok(vec!["nginx@sha256:x".to_string()]))
            .remote("nginx:1.25", Ok("sha256:x".to_string()));
        run(&api, GENEROUS, &[], &[]);
        assert!(
            api.asked().contains(&"inspect sha256:running".to_string()),
            "{:?}",
            api.asked()
        );
    }

    // ── Repository normalisation ────────────────────────────────────────────

    /// The regression this normalisation exists for: a container created as
    /// `docker.io/library/nginx` runs an image that records the familiar
    /// `nginx@sha256:...`, and comparing those literally reports
    /// `no_matching_repo_digest` for an entirely ordinary container.
    #[test]
    fn a_fully_qualified_hub_reference_matches_the_familiar_repo_digest() {
        let api = FakeApi::with(vec![container(
            "web",
            "docker.io/library/nginx:1.25",
            "sha256:aaaa",
        )])
        .digest("sha256:aaaa", Ok(vec!["nginx@sha256:x".to_string()]))
        .remote("docker.io/library/nginx:1.25", Ok("sha256:y".to_string()));
        let report = run(&api, GENEROUS, &[], &[]);
        assert_eq!(
            only(&report).outcome,
            Ok(true),
            "{:?}",
            only(&report).detail
        );
    }

    #[test]
    fn the_library_prefix_alone_also_matches() {
        let api = FakeApi::with(vec![container("web", "library/nginx:1.25", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec!["nginx@sha256:x".to_string()]))
            .remote("library/nginx:1.25", Ok("sha256:x".to_string()));
        assert_eq!(only(&run(&api, GENEROUS, &[], &[])).outcome, Ok(false));
    }

    #[test]
    fn every_spelling_of_a_hub_repository_normalises_to_one() {
        for spelling in [
            "nginx",
            "library/nginx",
            "docker.io/library/nginx",
            "index.docker.io/library/nginx",
        ] {
            assert_eq!(
                normalize_repository(spelling),
                "docker.io/library/nginx",
                "{spelling}"
            );
        }
        for spelling in ["myuser/myapp", "docker.io/myuser/myapp"] {
            assert_eq!(
                normalize_repository(spelling),
                "docker.io/myuser/myapp",
                "{spelling}"
            );
        }
    }

    /// A registry host is left alone: `ghcr.io/org/app` has no familiar short
    /// form, and inventing one would break the match in the other direction.
    #[test]
    fn a_private_registry_repository_is_left_as_it_is() {
        for repo in [
            "ghcr.io/org/app",
            "registry.example.com:5000/team/app",
            "localhost:5000/app",
        ] {
            assert_eq!(normalize_repository(repo), repo);
        }
    }

    #[test]
    fn a_digest_for_a_different_repository_does_not_count_as_a_match() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:aaaa")]).digest(
            "sha256:aaaa",
            Ok(vec!["ghcr.io/org/nginx@sha256:x".to_string()]),
        );
        assert_eq!(
            only(&run(&api, GENEROUS, &[], &[])).outcome,
            Err(ContainerReason::NoMatchingRepoDigest)
        );
    }

    // ── The reasons ─────────────────────────────────────────────────────────

    #[test]
    fn a_locally_built_image_says_so_rather_than_reporting_up_to_date() {
        let api = FakeApi::with(vec![container("app", "myapp:dev", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec![]));
        assert_eq!(
            only(&run(&api, GENEROUS, &[], &[])).outcome,
            Err(ContainerReason::NoRepoDigest)
        );
    }

    #[test]
    fn a_digest_pinned_container_has_no_verdict() {
        let api = FakeApi::with(vec![container("web", "nginx@sha256:abcdef", "sha256:aaaa")]);
        assert_eq!(
            only(&run(&api, GENEROUS, &[], &[])).outcome,
            Err(ContainerReason::DigestPinnedReference)
        );
    }

    /// `docker ps` shows the image ID under IMAGE once the last tag for a
    /// running container's image is removed.
    #[test]
    fn an_image_id_where_a_reference_belongs_is_named_as_such() {
        for image in ["sha256:aaaa", "sha256:0123456789abcdef"] {
            let api = FakeApi::with(vec![container("orphan", image, "sha256:aaaa")]);
            assert_eq!(
                only(&run(&api, GENEROUS, &[], &[])).outcome,
                Err(ContainerReason::ImageIdReference),
                "{image}"
            );
        }
    }

    #[test]
    fn a_failed_image_inspect_carries_the_daemons_own_message() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:aaaa")])
            .digest("sha256:aaaa", Err("no such image".to_string()));
        let report = run(&api, GENEROUS, &[], &[]);
        assert_eq!(
            only(&report).outcome,
            Err(ContainerReason::ImageInspectFailed)
        );
        assert_eq!(only(&report).detail.as_deref(), Some("no such image"));
    }

    #[test]
    fn an_unreachable_registry_degrades_only_that_container() {
        let api = FakeApi::with(vec![
            container("web", "nginx:1.25", "sha256:aaaa"),
            container("db", "postgres:16", "sha256:bbbb"),
        ])
        .digest("sha256:aaaa", Ok(vec!["nginx@sha256:x".to_string()]))
        .remote("nginx:1.25", Err("i/o timeout".to_string()))
        .digest("sha256:bbbb", Ok(vec!["postgres@sha256:y".to_string()]))
        .remote("postgres:16", Ok("sha256:y".to_string()));

        let report = run(&api, GENEROUS, &[], &[]);
        assert_eq!(
            report.containers[0].outcome,
            Err(ContainerReason::DistributionQueryFailed)
        );
        assert_eq!(
            report.containers[1].outcome,
            Ok(false),
            "one container's failure must not touch another's verdict"
        );
        assert!(report.daemon_succeeded());
    }

    #[test]
    fn an_unlistable_daemon_reports_no_containers_at_all() {
        let report = run(&FakeApi::unreachable(), GENEROUS, &[], &[]);
        assert_eq!(report.daemon_outcome, Err(DaemonReason::DockerUnreachable));
        assert!(report.containers.is_empty());
        assert_eq!(report.detail.as_deref(), Some("connection refused"));
    }

    // ── The budget ──────────────────────────────────────────────────────────

    /// A zero budget is the boundary case of the real one. Telegraf killing an
    /// overrunning helper loses every result; here the containers that were
    /// not reached say so, and nothing is silently missing.
    #[test]
    fn containers_not_reached_within_the_budget_report_that_rather_than_vanishing() {
        let api = FakeApi::with(vec![
            container("a", "nginx:1", "sha256:a"),
            container("b", "nginx:2", "sha256:b"),
        ]);
        let report = run(&api, Duration::ZERO, &[], &[]);
        assert!(report.daemon_succeeded());
        assert_eq!(report.containers.len(), 2);
        for c in &report.containers {
            assert_eq!(c.outcome, Err(ContainerReason::BudgetExceeded));
        }
        assert!(
            api.asked().is_empty(),
            "an exhausted budget must not still make calls: {:?}",
            api.asked()
        );
    }

    #[test]
    fn a_budget_that_is_not_exhausted_changes_nothing() {
        let api = FakeApi::with(vec![container("web", "nginx:1.25", "sha256:aaaa")])
            .digest("sha256:aaaa", Ok(vec!["nginx@sha256:x".to_string()]))
            .remote("nginx:1.25", Ok("sha256:x".to_string()));
        assert_eq!(only(&run(&api, GENEROUS, &[], &[])).outcome, Ok(false));
    }

    // ── Filtering ───────────────────────────────────────────────────────────

    #[test]
    fn a_filtered_out_container_costs_no_api_call() {
        let api = FakeApi::with(vec![
            container("app-1", "nginx:1", "sha256:a"),
            container("build-1", "nginx:1", "sha256:b"),
        ])
        .digest("sha256:a", Ok(vec![]));
        let report = run(&api, GENEROUS, &["app-*".to_string()], &[]);
        assert_eq!(report.containers.len(), 1);
        assert_eq!(report.containers[0].name, "app-1");
        assert!(
            !api.asked().iter().any(|c| c.contains("sha256:b")),
            "{:?}",
            api.asked()
        );
    }

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

    // ── The top-level entry point ───────────────────────────────────────────

    /// Validation rejects this before rendering, but `image-check` is a
    /// documented command an operator can run by hand — a typo there must
    /// produce the same honest failure as every other precondition, not a
    /// panic.
    #[test]
    fn an_endpoint_with_no_known_scheme_is_a_daemon_level_failure() {
        let report = check(
            "/var/run/docker.sock",
            Duration::from_secs(1),
            Duration::from_secs(1),
            GENEROUS,
            &[],
            &[],
        );
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
            Duration::from_secs(2),
            GENEROUS,
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

    // ── Line protocol ───────────────────────────────────────────────────────

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

    /// Line protocol has no escape for a newline: it ends the line, and
    /// whatever follows parses as another measurement. A tag value muninn did
    /// not choose must not be able to fabricate a series, however firmly
    /// Docker's own grammar says it cannot contain one.
    #[test]
    fn a_control_character_in_a_tag_value_cannot_start_a_new_line() {
        let check = ContainerCheck {
            name: "web\nmuninn_container_image_updates,container_name=fake".to_string(),
            image: "nginx:latest".to_string(),
            outcome: Ok(false),
            at: 1_754_000_000,
            detail: None,
        };
        let out = check.line_protocol();
        assert_eq!(
            out.lines().count(),
            2,
            "one check line and one verdict line, no injected third: {out}"
        );
        assert!(!out.contains("web\n"), "{out}");
    }
}
