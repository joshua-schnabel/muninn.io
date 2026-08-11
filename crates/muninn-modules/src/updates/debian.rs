//! Reading the host's pending package updates from inside a container.
//!
//! This is the implementation of approach A, settled by the
//! [measured evidence](../../../../docs/updates-evidence.md) and recorded in
//! ADR-0009: mount the host filesystem read-only, point apt's directory options
//! at the host's dpkg status, sources and package indices, and let real apt do
//! the resolution.
//!
//! # The invariant
//!
//! If the host's package state cannot be read, this reports `check_success=0`
//! and **omits** the pending counts. It never reports zero. "No updates pending"
//! and "I could not look" are opposite conclusions, and an alert rule cannot
//! tell them apart afterwards if they share a representation.
//!
//! The type system carries that rule rather than a convention: the counts live
//! inside the `Ok` arm of [`Report::outcome`], so a failed check has nothing to
//! print them from.
//!
//! # Why apt runs at all
//!
//! Parsing dpkg's status file and apt's indices directly would keep the runtime
//! image distroless, and was rejected in the ADR for a reason that also governs
//! this file: `apt-get -s dist-upgrade` honours holds, pins and phased updates,
//! and the spike's measured agreement with each host's own answer is worth
//! exactly as much as it is *because* real apt produced it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::unix_now as now;

/// The influx measurement every line carries.
pub const MEASUREMENT: &str = "muninn_updates";

/// Why a check could not produce an answer.
///
/// A closed set of short tokens, because this becomes a metric tag. A path or an
/// error string here would make the series unbounded and, worse, would put a
/// host's directory layout into a metrics database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    HostfsNotMounted,
    DpkgStatusUnreadable,
    DpkgStatusEmpty,
    AptEtcMissing,
    AptListsMissing,
    AptListsEmpty,
    OsReleaseUnreadable,
    HostNotDebianFamily,
    ScratchUnavailable,
    AptFailed,
    /// apt did not finish inside the time it was given and was killed.
    ///
    /// Distinct from [`Reason::AptFailed`], which is apt answering with a
    /// non-zero status: that says something about the host's package state,
    /// this says apt never got that far. Added by F-06 of the 1.0 review,
    /// alongside the deadline that makes it reachable.
    AptTimedOut,
    ParseInconsistent,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::HostfsNotMounted => "hostfs_not_mounted",
            Reason::DpkgStatusUnreadable => "dpkg_status_unreadable",
            Reason::DpkgStatusEmpty => "dpkg_status_empty",
            Reason::AptEtcMissing => "apt_etc_missing",
            Reason::AptListsMissing => "apt_lists_missing",
            Reason::AptListsEmpty => "apt_lists_empty",
            Reason::OsReleaseUnreadable => "os_release_unreadable",
            Reason::HostNotDebianFamily => "host_not_debian_family",
            Reason::ScratchUnavailable => "scratch_unavailable",
            Reason::AptFailed => "apt_failed",
            Reason::AptTimedOut => "apt_timed_out",
            Reason::ParseInconsistent => "parse_inconsistent",
        }
    }

    /// Every reason, for the test that keeps the tokens unique and stable.
    pub const ALL: [Reason; 12] = [
        Reason::HostfsNotMounted,
        Reason::DpkgStatusUnreadable,
        Reason::DpkgStatusEmpty,
        Reason::AptEtcMissing,
        Reason::AptListsMissing,
        Reason::AptListsEmpty,
        Reason::OsReleaseUnreadable,
        Reason::HostNotDebianFamily,
        Reason::ScratchUnavailable,
        Reason::AptFailed,
        Reason::AptTimedOut,
        Reason::ParseInconsistent,
    ];
}

/// What a successful check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub all: u32,
    pub security: u32,
}

impl Counts {
    /// The only constructor, so the one relationship that must hold between the
    /// two numbers is checked in one place.
    ///
    /// Security updates are a subset of all updates. If the parse ever produces
    /// more of the former than the latter, both numbers are meaningless, and
    /// publishing either would be worse than publishing the failure.
    fn checked(all: u32, security: u32) -> Result<Self, Reason> {
        if security > all {
            return Err(Reason::ParseInconsistent);
        }
        Ok(Counts { all, security })
    }
}

/// The result of one check, in the form the metrics are written from.
#[derive(Debug, Clone)]
pub struct Report {
    /// The counts, or why there are none. Never both.
    pub outcome: Result<Counts, Reason>,
    /// Age of the newest package index, in seconds. Not a failure condition:
    /// stale lists still give a correct answer about a stale picture, and the
    /// number is what lets an alert tell "nothing pending" from "nobody has run
    /// `apt-get update` since March".
    pub lists_age_seconds: Option<i64>,
    /// When the check ran, in seconds since the Unix epoch.
    pub at: u64,
    /// A human-readable detail for logs — a path, or apt's own error. Never a
    /// metric tag, which is why it is free to be specific.
    pub detail: Option<String>,
}

impl Report {
    fn failed(reason: Reason, detail: impl Into<String>) -> Self {
        Report {
            outcome: Err(reason),
            lists_age_seconds: None,
            at: now(),
            detail: Some(detail.into()),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.outcome.is_ok()
    }

    /// The influx line protocol Telegraf's `inputs.exec` parses.
    ///
    /// The shape follows the metric names the design fixed, which are Prometheus
    /// names — `muninn_updates_pending{severity="all"}`. Telegraf joins the
    /// measurement and the field name, so the count is a field called `pending`
    /// carrying a `severity` tag, not two fields named after the severities.
    ///
    /// `status` and `reason` sit on the check line and are present in both the
    /// success and failure cases, `reason=none` when there is nothing to report.
    /// A tag that appeared only on failure would give the same metric two label
    /// sets, and both would be exposed together for one expiration interval
    /// after a check recovers.
    pub fn line_protocol(&self, security_metric: bool) -> String {
        match self.outcome {
            Err(reason) => format!(
                "{MEASUREMENT},status=error,reason={} check_success=0i,check_timestamp_seconds={}i\n",
                reason.as_str(),
                self.at
            ),
            Ok(counts) => {
                let mut out = format!(
                    "{MEASUREMENT},status=ok,reason=none check_success=1i,check_timestamp_seconds={}i",
                    self.at
                );
                if let Some(age) = self.lists_age_seconds {
                    out.push_str(&format!(",lists_age_seconds={age}i"));
                }
                out.push('\n');
                out.push_str(&format!(
                    "{MEASUREMENT},severity=all pending={}i\n",
                    counts.all
                ));
                if security_metric {
                    out.push_str(&format!(
                        "{MEASUREMENT},severity=security pending={}i\n",
                        counts.security
                    ));
                }
                out
            }
        }
    }
}

/// Where the host's state is expected, given the mount prefix.
struct HostPaths {
    dpkg_status: PathBuf,
    apt_etc: PathBuf,
    apt_lists: PathBuf,
}

impl HostPaths {
    fn under(hostfs: &Path) -> Self {
        HostPaths {
            dpkg_status: hostfs.join("var/lib/dpkg/status"),
            apt_etc: hostfs.join("etc/apt"),
            apt_lists: hostfs.join("var/lib/apt/lists"),
        }
    }
}

/// Run one check against the host filesystem mounted at `hostfs`.
///
/// `scratch_base` is where apt's cache directory is created — the one directory
/// apt genuinely writes to even in simulation. It is a parameter rather than a
/// constant because the documented deployment has a read-only root filesystem
/// with exactly one writable tmpfs, and that tmpfs is not always `/tmp`.
///
/// `limit` bounds apt itself: it is killed if it overruns, and the report says
/// [`Reason::AptTimedOut`] rather than a count. apt reads a mount muninn does
/// not control, so the wait is not muninn's to assume is finite — see
/// [`run_apt_bounded`]. It applies to each apt invocation separately.
///
/// `classify_security` runs the second pass that separates the security subset
/// ([`security_packages`]). Off when `modules.updates.security_only_metric` is
/// false, because there is then no security series to publish and the pass
/// would be a second walk of the host's package index for a number nobody
/// reads.
pub fn check(
    hostfs: &Path,
    scratch_base: &Path,
    limit: Duration,
    classify_security: bool,
) -> Report {
    let paths = HostPaths::under(hostfs);

    if let Err(report) = preconditions(hostfs, &paths) {
        return report;
    }

    let scratch = match Scratch::create(scratch_base) {
        Ok(s) => s,
        Err(e) => {
            return Report::failed(
                Reason::ScratchUnavailable,
                format!(
                    "cannot create a scratch directory below '{}': {e}. apt writes its cache \
                     there even when simulating; with a read-only root filesystem this needs a \
                     tmpfs",
                    scratch_base.display()
                ),
            );
        }
    };

    let output = match run_apt_bounded(&paths, scratch.path(), limit) {
        Ok(Some(o)) => o,
        Ok(None) => {
            // Killed, not merely given up on. Reporting the failure rather than
            // a count is the project's sharpest rule: `0 updates` from a check
            // that never finished is worse than no metric, because an alert
            // cannot tell them apart afterwards.
            return Report::failed(
                Reason::AptTimedOut,
                format!(
                    "apt-get -s dist-upgrade did not finish within {}s and was killed. It reads \
                     the host's package index through the mount, so a stalled or very slow mount \
                     looks like this",
                    limit.as_secs()
                ),
            );
        }
        Err(e) => {
            return Report::failed(
                Reason::AptFailed,
                format!("could not run apt-get: {e}. The runtime image must carry apt and dpkg"),
            );
        }
    };

    if !output.status.success() {
        // A non-zero apt is not "zero updates". The most common cause is a host
        // package index in a format this container's apt does not understand.
        return Report::failed(
            Reason::AptFailed,
            format!(
                "apt-get -s dist-upgrade exited with {}: {}",
                output.status,
                first_line(&String::from_utf8_lossy(&output.stderr))
            ),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pending = parse_pending(&stdout);
    let all = pending.len() as u32;

    // The second pass, and only when the answer is going to be used. With
    // `security_only_metric: false` there is no security series to publish, so
    // asking apt again would be a second parse of the host's package index for
    // a number nobody reads.
    let security = if classify_security {
        match security_packages(&paths, scratch.path(), &pending, limit) {
            Ok(set) => set.len() as u32,
            // Deliberately fatal to the whole check rather than "report the
            // total and skip the security series". An undercount is
            // indistinguishable from good news, and a missing series reads as
            // zero on most dashboards — which is precisely the shape of the
            // problem this pass was added to fix.
            Err((reason, detail)) => {
                return Report::failed(
                    reason,
                    format!(
                        "{detail}. The pending total was readable, but classifying the security \
                         subset was not — and reporting a total beside a security count that \
                         might be wrong is worse than reporting neither"
                    ),
                );
            }
        }
    } else {
        0
    };

    let counts = match Counts::checked(all, security) {
        Ok(c) => c,
        Err(reason) => {
            return Report::failed(
                reason,
                "apt's output did not parse: more security updates than updates in total, so \
                 neither number can be trusted"
                    .to_string(),
            );
        }
    };

    Report {
        outcome: Ok(counts),
        lists_age_seconds: lists_age(&paths.apt_lists, now()),
        at: now(),
        detail: None,
    }
}

/// Everything that has to be true before apt is worth running.
///
/// Checked one at a time so the reason is specific. Every one of these is a
/// deployment mistake that would otherwise produce a plausible wrong number
/// rather than an error.
fn preconditions(hostfs: &Path, paths: &HostPaths) -> Result<(), Report> {
    // Empty counts as absent, and that is not pedantry: the image creates
    // /hostfs so a bind mount has somewhere to land, so the directory always
    // exists and "you forgot the mount" would otherwise surface as a missing
    // dpkg status — a true statement that points at the wrong thing.
    if !hostfs.is_dir() || is_empty_dir(hostfs) {
        return Err(Report::failed(
            Reason::HostfsNotMounted,
            format!(
                "'{}' is missing or empty. Mount the host filesystem there: `-v /:{}:ro`",
                hostfs.display(),
                hostfs.display()
            ),
        ));
    }

    match fs::metadata(&paths.dpkg_status) {
        Err(e) => {
            return Err(Report::failed(
                Reason::DpkgStatusUnreadable,
                format!("cannot read '{}': {e}", paths.dpkg_status.display()),
            ));
        }
        Ok(meta) if meta.len() == 0 => {
            return Err(Report::failed(
                Reason::DpkgStatusEmpty,
                format!(
                    "'{}' is empty. An empty package database resolves to zero pending updates, \
                     which would be a confident wrong answer",
                    paths.dpkg_status.display()
                ),
            ));
        }
        Ok(_) => {}
    }
    // Metadata is not readability: the file can be stat-able and still refuse to
    // open, which is what a mount without the right permissions looks like.
    if fs::File::open(&paths.dpkg_status).is_err() {
        return Err(Report::failed(
            Reason::DpkgStatusUnreadable,
            format!(
                "'{}' exists but cannot be opened",
                paths.dpkg_status.display()
            ),
        ));
    }

    if !paths.apt_etc.is_dir() {
        return Err(Report::failed(
            Reason::AptEtcMissing,
            format!("'{}' is missing", paths.apt_etc.display()),
        ));
    }
    if !paths.apt_lists.is_dir() {
        return Err(Report::failed(
            Reason::AptListsMissing,
            format!("'{}' is missing", paths.apt_lists.display()),
        ));
    }

    // An apt lists directory with no package index means `apt-get update` has
    // never run on the host, or the mount points somewhere unexpected. Without
    // indices apt reports zero pending updates — correctly, from its point of
    // view, and completely misleadingly.
    if !has_package_index(&paths.apt_lists) {
        return Err(Report::failed(
            Reason::AptListsEmpty,
            format!(
                "'{}' holds no package index. apt would report zero pending updates from an \
                 empty index, which is indistinguishable from an up-to-date host",
                paths.apt_lists.display()
            ),
        ));
    }

    // Debian family only. A host running something else would otherwise get a
    // confident answer derived from a package manager it does not use.
    let Some(ids) = os_release_ids(hostfs) else {
        return Err(Report::failed(
            Reason::OsReleaseUnreadable,
            format!(
                "cannot read '{}' or '{}' — note the first is normally a symlink into /usr/lib",
                OS_RELEASE_LOCATIONS[0], OS_RELEASE_LOCATIONS[1]
            ),
        ));
    };
    if !ids.is_debian_family() {
        return Err(Report::failed(
            Reason::HostNotDebianFamily,
            format!(
                "the host reports ID={:?}, which is not Debian-family",
                ids.id
            ),
        ));
    }

    Ok(())
}

/// Where a host records what it is, in the order os-release(5) prefers.
pub const OS_RELEASE_LOCATIONS: [&str; 2] = ["etc/os-release", "usr/lib/os-release"];

/// What a host calls itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsRelease {
    pub id: String,
    pub id_like: String,
}

impl OsRelease {
    pub fn is_debian_family(&self) -> bool {
        [&self.id, &self.id_like]
            .iter()
            .any(|v| v.split_whitespace().any(|w| w == "debian" || w == "ubuntu"))
    }
}

/// Read the host's `ID` and `ID_LIKE` from either os-release location.
///
/// `None` only when neither file can be read at all.
///
/// # Why both files, field by field
///
/// `/etc/os-release` is normally a symlink to `../usr/lib/os-release` on Debian
/// and Ubuntu, so a mount carrying `/etc` but not `/usr` leaves it dangling —
/// one of the concrete reasons the documented mount is the whole root
/// (ADR-0005), and the reason this module declares `usr` among its host paths.
///
/// But taking the first readable *file* and stopping there is wrong too, and
/// this is not hypothetical: Docker Desktop's VM ships an `/etc/os-release`
/// containing only `PRETTY_NAME="Docker Desktop"`, while `/usr/lib/os-release`
/// holds `ID=debian`. Reading only the first reports "not a Debian host" about a
/// Debian host — a confident wrong answer, which is the failure mode this whole
/// module is built to avoid. So the files are consulted in order and the first
/// non-empty value of each field wins.
pub fn os_release_ids(hostfs: &Path) -> Option<OsRelease> {
    let texts: Vec<String> = OS_RELEASE_LOCATIONS
        .iter()
        .filter_map(|p| fs::read_to_string(hostfs.join(p)).ok())
        .collect();
    if texts.is_empty() {
        return None;
    }

    let field = |key: &str| {
        texts
            .iter()
            .filter_map(|t| os_release_field(t, key))
            .find(|v| !v.is_empty())
            .unwrap_or_default()
    };
    Some(OsRelease {
        id: field("ID"),
        id_like: field("ID_LIKE"),
    })
}

fn os_release_field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
        .map(|v| v.trim().trim_matches('"').to_string())
}

fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut e| e.next().is_none())
        // Unreadable is not empty; the next check says something more useful
        // about it than this one could.
        .unwrap_or(false)
}

fn has_package_index(lists: &Path) -> bool {
    let Ok(entries) = fs::read_dir(lists) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("_Packages"))
}

/// How often the deadline below is checked while apt runs.
///
/// A poll rather than a blocking wait with a timeout, because `std` has no
/// such wait. 50 ms is far below any deadline worth setting and costs nothing
/// against a process that takes seconds.
const APT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run apt with a deadline, killing it if it overruns.
///
/// `Ok(None)` means the deadline passed and the process was killed.
///
/// # Why this is not `Command::output()`
///
/// `output()` waits forever. apt reads the host's entire package index from a
/// mount muninn does not control; a stuck NFS mount or a wedged filesystem
/// makes that wait unbounded. That wait used to sit on muninn's startup path
/// *before* the supervisor began multiplexing signals, so a `docker stop`
/// landing on a host in that state was answered by nothing until apt returned,
/// and the container was killed at the end of its grace period instead of
/// stopping (F-06).
///
/// Stopping *waiting* is not enough on its own, which is why this kills. A
/// `spawn_blocking` task cannot be cancelled, and dropping the Tokio runtime
/// waits for blocking tasks to finish — so abandoning the wait would move the
/// same hang from the signal handler to process exit.
fn run_apt_bounded(
    paths: &HostPaths,
    cache: &Path,
    limit: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    run_bounded(apt_command(paths, cache), limit)
}

/// Run `command` to completion, or kill it at `limit`.
///
/// Split out from [`run_apt_bounded`] so the deadline can be tested against a
/// process that is deliberately slow, without needing apt or a host tree. The
/// behaviour under test is the timing and the kill, neither of which is
/// specific to apt.
fn run_bounded(
    mut command: Command,
    limit: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::Read as _;

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Both pipes are drained on their own threads. apt's simulated output is
    // small on every host measured for ADR-0009, but "small" is a property of
    // the host's package list rather than of this code, and a full pipe buffer
    // with nobody reading is a deadlock that would first appear on the busiest
    // machine — the one it is least welcome on.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                // SIGKILL rather than SIGTERM: this is a simulation that writes
                // nothing outside its own scratch cache, so there is nothing to
                // let it flush, and a process already ignoring the clock is not
                // one to ask politely twice.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(APT_POLL_INTERVAL),
        }
    };

    // Joined either way, so neither thread outlives the call. After a kill the
    // pipes are closed, so both return promptly.
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    Ok(status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    }))
}

/// The simulated upgrade, with every writable apt directory redirected.
///
/// The option list is the spike's, unchanged. It is the part of this module that
/// was measured against four distributions, and the exact agreement recorded in
/// ADR-0009 belongs to these arguments rather than to the code around them.
fn apt_command(paths: &HostPaths, cache: &Path) -> Command {
    // Built as an OsString rather than formatted: a path that is not valid UTF-8
    // must reach apt unchanged, and `format!` would replace what it cannot
    // render — silently pointing apt at a different file.
    let opt = |k: &str, v: &Path| {
        let mut s = OsString::from(k);
        s.push("=");
        s.push(v.as_os_str());
        s
    };

    let mut cmd = Command::new("apt-get");
    cmd.arg("-s")
        .arg("dist-upgrade")
        .arg("-o")
        .arg(opt("Dir::State::status", &paths.dpkg_status))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::sourcelist",
            &paths.apt_etc.join("sources.list"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::sourceparts",
            &paths.apt_etc.join("sources.list.d"),
        ))
        .arg("-o")
        .arg(opt("Dir::Etc::trusted", &paths.apt_etc.join("trusted.gpg")))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::trustedparts",
            &paths.apt_etc.join("trusted.gpg.d"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::preferences",
            &paths.apt_etc.join("preferences"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::preferencesparts",
            &paths.apt_etc.join("preferences.d"),
        ))
        .arg("-o")
        .arg(opt("Dir::State::lists", &paths.apt_lists))
        // The one directory apt genuinely writes to.
        .arg("-o")
        .arg(opt("Dir::Cache", cache))
        // Do not try to take /var/lib/dpkg/lock — the mount is read-only.
        .arg("-o")
        .arg("Debug::NoLocking=1")
        .arg("-o")
        .arg("APT::Get::Show-Versions=false")
        // apt writes more than Dir::Cache: it also takes ordinary temp files —
        // `mkstemp /tmp/clearsigned.message.XXXXXX` — while reading signed
        // release files, and `-s` does not stop it. In the documented deployment
        // the root filesystem is read-only and `/tmp` is not a tmpfs, so apt
        // fails there with `GetTempFile (30: Read-only file system)` and the
        // module reports `apt_failed` on a host it could have read perfectly.
        //
        // Pointing TMPDIR at the scratch directory fixes it for every caller at
        // once, rather than depending on the environment muninn happens to have
        // been started with. Found by scripts/updates-test.sh, cell S12: the
        // rendered `inputs.exec` sets TMPDIR and worked, muninn's own startup
        // check inherited an empty environment and did not.
        .env("TMPDIR", cache)
        // C locale so the `Inst` lines this parses are not translated. The image
        // ships no other locale, so this changes nothing there — it keeps the
        // helper honest when it is run by hand on a developer's machine.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("DEBIAN_FRONTEND", "noninteractive");
    cmd
}

/// One package apt would install or upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    name: String,
    /// The version apt would move to. Named `candidate` because that is what
    /// apt calls it, and because the classification is a question about *this*
    /// version rather than about the package.
    candidate: String,
}

/// What apt would install or upgrade.
///
/// apt prints one line per package:
///
/// ```text
/// Inst libc6 [2.36-9+deb12u3] (2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])
/// ```
///
/// The name and the candidate version are taken; the origin apt prints here is
/// deliberately **not** used to classify — see [`security_packages`] for why.
fn parse_pending(stdout: &str) -> Vec<Pending> {
    let mut out = Vec::new();

    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("Inst ") else {
            continue;
        };
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        // "(2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])"
        let candidate = rest
            .split_once('(')
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .unwrap_or_default()
            .to_string();

        out.push(Pending {
            name: name.to_string(),
            candidate,
        });
    }

    out
}

/// Which of `pending` have a candidate version available from a security origin.
///
/// # Why this is not the origin apt already printed
///
/// The `Inst` line names **one** origin: whichever pocket apt resolved the
/// candidate through. On Debian that is accurate. On Ubuntu it is a lower bound,
/// because Ubuntu publishes security updates to `<release>-security` *and*
/// copies them into `<release>-updates` — so when apt resolves through the
/// latter, the line reads `Ubuntu:24.04/noble-updates` and a genuine security
/// update is not counted as one.
///
/// That was measurable rather than theoretical: the same Ubuntu 24.04 fixture
/// reported 66 pending / 34 security when it was built, and 66 pending / 0
/// security when rebuilt against a later archive. The packages had not changed;
/// the pocket holding the candidate had. A security count of zero on an Ubuntu
/// host was therefore not evidence that nothing security-relevant was pending,
/// which is the one thing a security metric has to be. Recorded as R8, closed
/// as F-04, and amended into ADR-0009.
///
/// `apt-cache policy` prints *every* origin a version is available from, so the
/// question becomes "is this exact version available from anywhere that is a
/// security suite" rather than "which pocket did apt happen to pick". Ubuntu's
/// own `apt-check` classifies the same way.
///
/// Returns `Err` rather than an incomplete answer. An undercount here is
/// indistinguishable from good news, which is the failure mode this module
/// exists to prevent.
fn security_packages(
    paths: &HostPaths,
    cache: &Path,
    pending: &[Pending],
    limit: Duration,
) -> Result<std::collections::BTreeSet<String>, (Reason, String)> {
    if pending.is_empty() {
        return Ok(Default::default());
    }

    let mut command = apt_cache_command(paths, cache);
    for p in pending {
        command.arg(&p.name);
    }

    let output = match run_bounded(command, limit) {
        Ok(Some(o)) => o,
        Ok(None) => {
            return Err((
                Reason::AptTimedOut,
                format!(
                    "apt-cache policy did not finish within {}s",
                    limit.as_secs()
                ),
            ));
        }
        Err(e) => {
            return Err((Reason::AptFailed, format!("could not run apt-cache: {e}")));
        }
    };

    if !output.status.success() {
        return Err((
            Reason::AptFailed,
            format!(
                "apt-cache policy exited with {}: {}",
                output.status,
                first_line(&String::from_utf8_lossy(&output.stderr))
            ),
        ));
    }

    Ok(parse_policy(
        &String::from_utf8_lossy(&output.stdout),
        pending,
    ))
}

/// The packages in `pending` whose candidate version has a security origin.
///
/// `apt-cache policy` prints, per package:
///
/// ```text
/// libc6:
///   Installed: 2.36-9+deb12u3
///   Candidate: 2.36-9+deb12u7
///   Version table:
///      2.36-9+deb12u7 500
///         500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages
///  *** 2.36-9+deb12u3 100
///         100 /var/lib/dpkg/status
/// ```
///
/// A version-table entry and one of its source lines are told apart by their
/// first token rather than by indentation: a source line begins with a priority
/// number, an entry does not. Indentation would have been the obvious rule and
/// is the fragile one — the `***` marker on the installed version shifts it.
///
/// The `-security` test is unchanged from before, and is applied to the source
/// lines instead of to the one origin apt chose to print. Matching the suffix
/// rather than a vendor name keeps Debian (`bookworm-security`), Ubuntu
/// (`noble-security`) and a third-party security suite all working with one
/// rule.
fn parse_policy(stdout: &str, pending: &[Pending]) -> std::collections::BTreeSet<String> {
    let wanted: std::collections::BTreeMap<&str, &str> = pending
        .iter()
        .map(|p| (p.name.as_str(), p.candidate.as_str()))
        .collect();

    let mut security = std::collections::BTreeSet::new();
    let mut package: Option<&str> = None;
    let mut in_candidate_block = false;

    for line in stdout.lines() {
        // A package header is the only thing at column zero.
        if !line.starts_with(char::is_whitespace) && line.ends_with(':') {
            package = Some(line.trim_end_matches(':'));
            in_candidate_block = false;
            continue;
        }

        let trimmed = line.trim();
        let trimmed = trimmed.strip_prefix("*** ").unwrap_or(trimmed);
        let mut tokens = trimmed.split_whitespace();
        let Some(first) = tokens.next() else { continue };

        if first.parse::<u32>().is_ok() {
            // A source line for whichever version block we are in.
            if in_candidate_block
                && let Some(name) = package
                && trimmed.to_ascii_lowercase().contains("-security")
            {
                security.insert(name.to_string());
            }
            continue;
        }

        // Otherwise: a version-table entry, if it ends in a priority.
        if tokens.next_back().is_some_and(|t| t.parse::<u32>().is_ok())
            && let Some(name) = package
        {
            in_candidate_block = wanted.get(name).is_some_and(|c| *c == first);
        }
    }

    security
}

/// `apt-cache policy`, pointed at the host's package state exactly as the
/// simulated upgrade is.
///
/// The directory options have to match [`apt_command`] or the two would answer
/// about different package sets — the second pass would classify versions the
/// first pass never saw.
fn apt_cache_command(paths: &HostPaths, cache: &Path) -> Command {
    let opt = |k: &str, v: &Path| {
        let mut s = OsString::from(k);
        s.push("=");
        s.push(v.as_os_str());
        s
    };

    let mut cmd = Command::new("apt-cache");
    cmd.arg("policy")
        .arg("-o")
        .arg(opt("Dir::State::status", &paths.dpkg_status))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::sourcelist",
            &paths.apt_etc.join("sources.list"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::sourceparts",
            &paths.apt_etc.join("sources.list.d"),
        ))
        .arg("-o")
        .arg(opt("Dir::Etc::trusted", &paths.apt_etc.join("trusted.gpg")))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::trustedparts",
            &paths.apt_etc.join("trusted.gpg.d"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::preferences",
            &paths.apt_etc.join("preferences"),
        ))
        .arg("-o")
        .arg(opt(
            "Dir::Etc::preferencesparts",
            &paths.apt_etc.join("preferences.d"),
        ))
        .arg("-o")
        .arg(opt("Dir::State::lists", &paths.apt_lists))
        .arg("-o")
        .arg(opt("Dir::Cache", cache))
        .arg("-o")
        .arg("Debug::NoLocking=1")
        .env("TMPDIR", cache)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("DEBIAN_FRONTEND", "noninteractive");
    cmd
}

/// Age of the newest package index, in seconds.
fn lists_age(lists: &Path, now: u64) -> Option<i64> {
    let newest = fs::read_dir(lists)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("_Packages"))
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .max()?;

    // Clamped at zero: a host clock ahead of the container's would otherwise
    // report a negative age, which reads as a bug rather than as clock skew.
    Some((now.saturating_sub(newest)) as i64)
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

/// A directory for apt's cache, removed when the check ends.
///
/// Not `tempfile`: that is a dev-dependency, and this runs in the shipped
/// binary.
///
/// The name is unpredictable and the directory is created **exclusively**, both
/// deliberately. In the shipped deployment `TMPDIR` points at the runtime tmpfs,
/// which is 0700 and reachable by nobody else — but `muninn update-check` is a
/// documented command an operator runs by hand, where `TMPDIR` is a shared
/// `/tmp`. A fixed or pid-derived name there is a symlink-attack target: another
/// local user pre-creates the path and chooses where apt's cache writes land.
struct Scratch(PathBuf);

impl Scratch {
    fn create(base: &Path) -> std::io::Result<Self> {
        // pid plus the sub-second clock. `create` rather than `create_dir_all`
        // is what actually closes the hole — it fails if anything is already at
        // the path, a symlink included, so losing the race is an error instead
        // of a redirect. The clock component only makes winning it a guess.
        let unique = format!(
            "muninn-update-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );
        let dir = base.join(unique);

        // `mut` is only used by the unix block below; on a developer's Windows
        // machine that block is compiled out and the binding is not mutated.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&dir)?;

        // Inside a directory this process just created and owns, so the
        // recursive form is safe here.
        fs::create_dir_all(dir.join("archives/partial"))?;
        Ok(Scratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: on a tmpfs a leftover directory costs nothing, and a
        // failure here must not turn a successful check into an error.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A minimal host tree: everything the preconditions look for.
    fn host_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("var/lib/dpkg")).unwrap();
        fs::create_dir_all(root.join("var/lib/apt/lists")).unwrap();
        fs::create_dir_all(root.join("etc/apt")).unwrap();
        fs::create_dir_all(root.join("usr/lib")).unwrap();
        fs::write(root.join("var/lib/dpkg/status"), "Package: libc6\n").unwrap();
        fs::write(
            root.join(
                "var/lib/apt/lists/deb.debian.org_debian_dists_bookworm_main_binary-amd64_Packages",
            ),
            "Package: libc6\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/os-release"),
            "ID=debian\nVERSION_ID=\"12\"\n",
        )
        .unwrap();
        dir
    }

    fn reason_of(dir: &Path) -> Option<Reason> {
        let paths = HostPaths::under(dir);
        preconditions(dir, &paths)
            .err()
            .map(|r| r.outcome.unwrap_err())
    }

    #[test]
    fn a_complete_host_tree_passes_the_preconditions() {
        let host = host_fixture();
        assert_eq!(reason_of(host.path()), None);
    }

    #[test]
    fn a_missing_mount_is_named_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-mounted");
        assert_eq!(reason_of(&missing), Some(Reason::HostfsNotMounted));
    }

    /// The image creates `/hostfs` so a bind mount has somewhere to land, so
    /// forgetting the mount leaves an empty directory rather than no directory.
    /// Reporting that as a missing dpkg status would be true and useless.
    #[test]
    fn an_empty_mount_point_is_a_missing_mount_not_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(reason_of(dir.path()), Some(Reason::HostfsNotMounted));
    }

    /// The failure the module exists to prevent: an empty package database
    /// resolves to zero pending updates, and zero is a believable answer.
    #[test]
    fn an_empty_dpkg_status_fails_rather_than_resolving_to_zero() {
        let host = host_fixture();
        fs::write(host.path().join("var/lib/dpkg/status"), "").unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::DpkgStatusEmpty));
    }

    #[test]
    fn a_missing_dpkg_status_is_distinct_from_an_empty_one() {
        let host = host_fixture();
        fs::remove_file(host.path().join("var/lib/dpkg/status")).unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::DpkgStatusUnreadable));
    }

    #[test]
    fn each_missing_apt_directory_has_its_own_reason() {
        let host = host_fixture();
        fs::remove_dir_all(host.path().join("etc/apt")).unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::AptEtcMissing));

        let host = host_fixture();
        fs::remove_dir_all(host.path().join("var/lib/apt/lists")).unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::AptListsMissing));
    }

    /// Indices that were never fetched look exactly like an up-to-date host to
    /// apt, so this has to be caught before apt is asked.
    #[test]
    fn package_lists_without_an_index_are_refused() {
        let host = host_fixture();
        for entry in fs::read_dir(host.path().join("var/lib/apt/lists")).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        assert_eq!(reason_of(host.path()), Some(Reason::AptListsEmpty));
    }

    /// `/etc/os-release` is a symlink into `/usr/lib` on Debian and Ubuntu. The
    /// fallback is why the module declares `usr` among its host paths.
    #[test]
    fn os_release_is_read_from_usr_lib_when_etc_has_none() {
        let host = host_fixture();
        fs::remove_file(host.path().join("etc/os-release")).unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::OsReleaseUnreadable));

        fs::write(host.path().join("usr/lib/os-release"), "ID=ubuntu\n").unwrap();
        assert_eq!(reason_of(host.path()), None);
    }

    #[test]
    fn a_non_debian_host_is_refused_rather_than_answered() {
        let host = host_fixture();
        fs::write(host.path().join("etc/os-release"), "ID=fedora\n").unwrap();
        assert_eq!(reason_of(host.path()), Some(Reason::HostNotDebianFamily));
    }

    #[test]
    fn debian_derivatives_are_recognised_through_id_like() {
        let family = |text: &str| {
            let host = host_fixture();
            fs::write(host.path().join("etc/os-release"), text).unwrap();
            os_release_ids(host.path()).unwrap().is_debian_family()
        };
        assert!(family("ID=debian\n"));
        assert!(family("ID=ubuntu\n"));
        assert!(family("ID=raspbian\nID_LIKE=debian\n"));
        assert!(family("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"));
        assert!(!family("ID=fedora\nID_LIKE=\"rhel centos\"\n"));
        // Substring matching would accept this; whole-word matching does not.
        assert!(!family("ID=notdebianatall\n"));
    }

    /// The failure the Linux suite found on a real machine: Docker Desktop's VM
    /// ships an `/etc/os-release` carrying only `PRETTY_NAME`, while
    /// `/usr/lib/os-release` carries `ID=debian`. Taking the first readable file
    /// and stopping there reports "not a Debian host" about a Debian host.
    #[test]
    fn a_partial_etc_os_release_falls_through_to_usr_lib() {
        let host = host_fixture();
        fs::write(
            host.path().join("etc/os-release"),
            "PRETTY_NAME=\"Docker Desktop\"\n",
        )
        .unwrap();
        fs::write(
            host.path().join("usr/lib/os-release"),
            "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\nVERSION_ID=\"12\"\n",
        )
        .unwrap();

        let ids = os_release_ids(host.path()).expect("both files are readable");
        assert_eq!(ids.id, "debian", "the second file holds the real fields");
        assert!(ids.is_debian_family());
        assert_eq!(
            reason_of(host.path()),
            None,
            "a Debian host must not be refused"
        );
    }

    /// The other direction still holds: a complete `/etc/os-release` wins over
    /// whatever `/usr/lib` says, which is what os-release(5) specifies.
    #[test]
    fn a_complete_etc_os_release_takes_precedence() {
        let host = host_fixture();
        fs::write(host.path().join("etc/os-release"), "ID=ubuntu\n").unwrap();
        fs::write(host.path().join("usr/lib/os-release"), "ID=debian\n").unwrap();
        assert_eq!(os_release_ids(host.path()).unwrap().id, "ubuntu");
    }

    // ── Parsing ─────────────────────────────────────────────────────────────

    /// Real output, from the spike's debian:12 fixture.
    const APT_OUTPUT: &str = "\
NOTE: This is only a simulation!
Reading package lists...
Building dependency tree...
The following packages will be upgraded:
  libc6 libssl3 tzdata
3 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.
Inst libc6 [2.36-9+deb12u3] (2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])
Inst libssl3 [3.0.11-1~deb12u2] (3.0.14-1~deb12u2 Debian:12.6/stable [amd64])
Inst tzdata [2024a-0+deb12u1] (2025b-0+deb12u1 Debian:12.11/stable [all])
Conf libc6 (2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])
";

    #[test]
    fn inst_lines_are_counted_and_conf_lines_are_not() {
        let pending = parse_pending(APT_OUTPUT);
        assert_eq!(pending.len(), 3, "Conf lines must not be counted");
        assert_eq!(pending[0].name, "libc6");
        assert_eq!(pending[0].candidate, "2.36-9+deb12u7");
    }

    #[test]
    fn nothing_pending_parses_as_nothing() {
        let out = "NOTE: This is only a simulation!\n0 upgraded, 0 newly installed.\n";
        assert!(parse_pending(out).is_empty());
    }

    // ── Classification ──────────────────────────────────────────────────────
    //
    // The origin apt prints on the `Inst` line is no longer what decides. See
    // `security_packages` for why, and R8 for the measurement that forced it.

    fn pending_of(name: &str, candidate: &str) -> Pending {
        Pending {
            name: name.to_string(),
            candidate: candidate.to_string(),
        }
    }

    #[test]
    fn a_candidate_from_a_security_suite_is_classified_as_one() {
        let policy = concat!(
            "libc6:\n",
            "  Installed: 2.36-9+deb12u3\n",
            "  Candidate: 2.36-9+deb12u7\n",
            "  Version table:\n",
            "     2.36-9+deb12u7 500\n",
            "        500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
            " *** 2.36-9+deb12u3 100\n",
            "        100 /var/lib/dpkg/status\n",
        );
        let found = parse_policy(policy, &[pending_of("libc6", "2.36-9+deb12u7")]);
        assert_eq!(found.len(), 1);
        assert!(found.contains("libc6"));
    }

    /// **The finding.** Ubuntu publishes a security update to `-security` and
    /// copies it into `-updates`; apt resolves the candidate through whichever
    /// it prefers and prints only that one. Reading the `Inst` line, this
    /// package is not a security update. Reading every origin the version is
    /// available from, it plainly is.
    ///
    /// This is R8 in a dozen lines: same package, same version, and the answer
    /// used to depend on which pocket apt happened to name.
    #[test]
    fn an_ubuntu_candidate_copied_into_updates_is_still_a_security_update() {
        let policy = concat!(
            "libc6:\n",
            "  Installed: 2.39-0ubuntu8.2\n",
            "  Candidate: 2.39-0ubuntu8.3\n",
            "  Version table:\n",
            "     2.39-0ubuntu8.3 500\n",
            "        500 http://archive.ubuntu.com/ubuntu noble-updates/main amd64 Packages\n",
            "        500 http://security.ubuntu.com/ubuntu noble-security/main amd64 Packages\n",
            " *** 2.39-0ubuntu8.2 100\n",
            "        100 /var/lib/dpkg/status\n",
        );
        let found = parse_policy(policy, &[pending_of("libc6", "2.39-0ubuntu8.3")]);
        assert!(
            found.contains("libc6"),
            "the security origin was on the second source line"
        );
    }

    /// Only the *candidate*'s origins count. A security version further down
    /// the table is one apt is not going to install, and counting it would
    /// over-report — the opposite error, and just as wrong.
    #[test]
    fn a_security_origin_on_a_version_apt_will_not_install_is_ignored() {
        let policy = concat!(
            "tzdata:\n",
            "  Installed: 2024a-0+deb12u1\n",
            "  Candidate: 2025b-0+deb12u1\n",
            "  Version table:\n",
            "     2025b-0+deb12u1 500\n",
            "        500 http://deb.debian.org/debian bookworm/main amd64 Packages\n",
            "     2024b-0+deb12u1 500\n",
            "        500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
            " *** 2024a-0+deb12u1 100\n",
            "        100 /var/lib/dpkg/status\n",
        );
        let found = parse_policy(policy, &[pending_of("tzdata", "2025b-0+deb12u1")]);
        assert!(
            found.is_empty(),
            "classified by the wrong version: {found:?}"
        );
    }

    /// A package whose *name* contains `-security` is not a security update.
    /// Only the source lines of its candidate decide.
    #[test]
    fn a_package_named_security_is_not_counted_as_one() {
        let policy = concat!(
            "gnome-security:\n",
            "  Installed: 1.0\n",
            "  Candidate: 1.1\n",
            "  Version table:\n",
            "     1.1 500\n",
            "        500 http://deb.debian.org/debian bookworm/main amd64 Packages\n",
        );
        assert!(parse_policy(policy, &[pending_of("gnome-security", "1.1")]).is_empty());
    }

    /// Several packages in one invocation, which is how it is actually called —
    /// one `apt-cache policy` for every pending package rather than one each.
    #[test]
    fn several_packages_are_classified_independently() {
        let policy = concat!(
            "libc6:\n",
            "  Installed: 1.0\n",
            "  Candidate: 1.1\n",
            "  Version table:\n",
            "     1.1 500\n",
            "        500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
            "tzdata:\n",
            "  Installed: 2.0\n",
            "  Candidate: 2.1\n",
            "  Version table:\n",
            "     2.1 500\n",
            "        500 http://deb.debian.org/debian bookworm/main amd64 Packages\n",
        );
        let found = parse_policy(
            policy,
            &[pending_of("libc6", "1.1"), pending_of("tzdata", "2.1")],
        );
        assert_eq!(found.len(), 1);
        assert!(found.contains("libc6"));
    }

    /// The `***` marker on the installed version shifts the indentation, which
    /// is why entries and source lines are told apart by their first token
    /// rather than by how far in they sit.
    #[test]
    fn the_installed_version_marker_does_not_confuse_the_parse() {
        let policy = concat!(
            "libc6:\n",
            "  Installed: 1.1\n",
            "  Candidate: 1.1\n",
            "  Version table:\n",
            " *** 1.1 100\n",
            "        100 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
            "        100 /var/lib/dpkg/status\n",
        );
        assert!(parse_policy(policy, &[pending_of("libc6", "1.1")]).contains("libc6"));
    }

    /// If more security updates than updates come out of the parse, the parse is
    /// wrong. Reporting either number would be worse than reporting the failure.
    ///
    /// The guard is checked on the constructor rather than through `parse`,
    /// because `parse` cannot currently reach the state — it only counts a
    /// security update on a line it has already counted as an update. That is
    /// the property this test exists to protect: the day someone splits those
    /// two counts apart, the guard is still there and still tested.
    #[test]
    fn a_security_count_above_the_total_is_a_failure_not_a_number() {
        assert_eq!(Counts::checked(3, 4), Err(Reason::ParseInconsistent));
        assert_eq!(
            Counts::checked(3, 3),
            Ok(Counts {
                all: 3,
                security: 3
            })
        );

        // The two passes cannot disagree by construction either: the security
        // set is built from the pending list, so it can never name a package
        // the total did not count.
        let pending = parse_pending(APT_OUTPUT);
        let policy = concat!(
            "libc6:\n",
            "  Candidate: 2.36-9+deb12u7\n",
            "     2.36-9+deb12u7 500\n",
            "        500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
            "not-pending:\n",
            "  Candidate: 9.9\n",
            "     9.9 500\n",
            "        500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n",
        );
        let security = parse_policy(policy, &pending);
        assert!(
            security.len() <= pending.len(),
            "{security:?} against {pending:?}"
        );
        assert!(
            !security.contains("not-pending"),
            "a package apt is not upgrading must not reach the security count"
        );
    }

    // ── Line protocol ───────────────────────────────────────────────────────

    fn ok_report() -> Report {
        Report {
            outcome: Ok(Counts {
                all: 41,
                security: 3,
            }),
            lists_age_seconds: Some(7200),
            at: 1_754_000_000,
            detail: None,
        }
    }

    #[test]
    fn a_successful_check_emits_the_documented_metric_names() {
        let out = ok_report().line_protocol(true);
        // Telegraf joins measurement and field: these become
        // muninn_updates_pending{severity="all"} and _check_success.
        assert!(
            out.contains("muninn_updates,severity=all pending=41i"),
            "{out}"
        );
        assert!(
            out.contains("muninn_updates,severity=security pending=3i"),
            "{out}"
        );
        assert!(out.contains("check_success=1i"), "{out}");
        assert!(out.contains("lists_age_seconds=7200i"), "{out}");
        assert!(out.contains("check_timestamp_seconds=1754000000i"), "{out}");
        assert!(out.ends_with('\n'), "line protocol must end with a newline");
    }

    /// The invariant, stated as a test: a failed check reports the failure and
    /// nothing else. A zero here would be indistinguishable from an up-to-date
    /// host for anyone reading the metric later.
    #[test]
    fn a_failed_check_omits_the_counts_entirely() {
        let report = Report::failed(Reason::DpkgStatusEmpty, "detail for the log");
        let out = report.line_protocol(true);
        assert!(out.contains("check_success=0i"), "{out}");
        assert!(out.contains("reason=dpkg_status_empty"), "{out}");
        assert!(
            !out.contains("pending"),
            "a failed check must not report a count at all: {out}"
        );
    }

    /// Every reason has to survive as a tag, in both directions: unique, and
    /// containing nothing that would make the series unbounded.
    #[test]
    fn reasons_are_unique_low_cardinality_tokens() {
        let mut seen = std::collections::HashSet::new();
        for r in Reason::ALL {
            let s = r.as_str();
            assert!(seen.insert(s), "{s} appears twice");
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{s} is not a plain token"
            );
        }
        assert_eq!(seen.len(), Reason::ALL.len());
    }

    #[test]
    fn the_security_metric_can_be_switched_off() {
        let out = ok_report().line_protocol(false);
        assert!(out.contains("severity=all"), "{out}");
        assert!(
            !out.contains("severity=security"),
            "security_only_metric=false must drop that series: {out}"
        );
    }

    #[test]
    fn the_lists_age_is_the_newest_index_not_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let lists = dir.path();
        for name in ["a_Packages", "b_Packages"] {
            fs::File::create(lists.join(name))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let age = lists_age(lists, now()).expect("indices exist");
        assert!((0..60).contains(&age), "freshly written files, got {age}s");
    }

    /// A host clock ahead of the container's must not produce a negative age.
    #[test]
    fn a_future_index_reports_zero_rather_than_a_negative_age() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("x_Packages"), "x").unwrap();
        assert_eq!(lists_age(dir.path(), 0), Some(0));
    }

    /// The scratch directory is apt's only writable path, and it must not
    /// survive the check — on a read-only deployment it lives on a small tmpfs.
    #[test]
    fn the_scratch_directory_is_created_and_removed() {
        let base = tempfile::tempdir().unwrap();
        let path = {
            let scratch = Scratch::create(base.path()).unwrap();
            assert!(scratch.path().join("archives/partial").is_dir());
            scratch.path().to_path_buf()
        };
        assert!(!path.exists(), "the scratch directory outlived the check");
    }

    /// Two checks must not pick the same path. A pid-derived name alone would
    /// collide here, and in a shared `/tmp` a predictable one is a symlink
    /// target rather than merely a collision.
    #[test]
    fn scratch_directories_do_not_share_a_name() {
        let base = tempfile::tempdir().unwrap();
        let a = Scratch::create(base.path()).unwrap();
        let b = Scratch::create(base.path()).unwrap();
        assert_ne!(a.path(), b.path());
    }

    /// Created exclusively: anything already sitting at the path is an error,
    /// not something to write into. This is what makes losing the race a
    /// failure instead of a redirect — the clock component only makes winning
    /// it a guess.
    #[test]
    fn an_occupied_scratch_path_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(base.path()).unwrap();

        // The same path a second time, which is what an attacker who guessed it
        // would have pre-created.
        let mut builder = fs::DirBuilder::new();
        assert!(
            builder.recursive(false).create(scratch.path()).is_err(),
            "an existing path must not be adopted"
        );
    }

    /// apt's cache holds nothing secret, but the directory is created in a
    /// world-writable `/tmp` and other users have no business reading it.
    #[cfg(unix)]
    #[test]
    fn the_scratch_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(base.path()).unwrap();

        let mode = fs::metadata(scratch.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {mode:o}");
    }

    /// The whole path, end to end, against a synthetic host tree.
    ///
    /// What it asserts is the invariant rather than a number: with or without
    /// apt present — a developer's Windows machine has none, the runtime image
    /// does — the result is either counts or a reason, and never a count without
    /// a successful check. The measured numbers are the system tests' job
    /// (`scripts/updates-test.sh`), because only a real host tree has a truth to
    /// compare against.
    /// A process that outlives its deadline is killed, and the call returns.
    ///
    /// The property that matters is not the message but the *bound*: apt reads
    /// the host's package index through a mount muninn does not control, and
    /// this used to be an unbounded `output()` on the startup path (F-06). The
    /// assertion is therefore on elapsed time — if the kill did not happen, the
    /// test would sit here for thirty seconds rather than fail.
    #[cfg(unix)]
    #[test]
    fn a_process_that_overruns_its_deadline_is_killed() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");

        let started = Instant::now();
        let result = run_bounded(cmd, Duration::from_millis(300)).unwrap();
        let elapsed = started.elapsed();

        assert!(result.is_none(), "an overrun must not look like an answer");
        assert!(
            elapsed < Duration::from_secs(5),
            "took {elapsed:?} — the deadline did not stop it"
        );
    }

    /// And a process that finishes in time is not disturbed: its output comes
    /// back whole. A deadline that truncated normal output would be worse than
    /// none, because the parser would then be reading a partial answer.
    #[cfg(unix)]
    #[test]
    fn a_process_that_finishes_in_time_returns_all_of_its_output() {
        let mut cmd = Command::new("printf");
        cmd.arg("Inst one\nInst two\n");

        let output = run_bounded(cmd, Duration::from_secs(10))
            .unwrap()
            .expect("finished well inside the deadline");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Inst one\nInst two\n"
        );
    }

    /// `apt_timed_out` is its own reason and not folded into `apt_failed`: one
    /// says the host's package state could not be resolved, the other says apt
    /// never got that far, and the fix differs.
    #[test]
    fn a_timeout_is_a_distinct_reason_from_a_failure() {
        assert_ne!(Reason::AptTimedOut.as_str(), Reason::AptFailed.as_str());
        assert_eq!(Reason::AptTimedOut.as_str(), "apt_timed_out");
    }

    #[test]
    fn a_check_either_counts_or_reports_why_not() {
        let host = host_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let report = check(
            host.path(),
            scratch.path(),
            crate::updates::APT_TIMEOUT,
            true,
        );
        let line = report.line_protocol(true);

        match report.outcome {
            Ok(_) => assert!(
                line.contains("check_success=1i") && line.contains("pending="),
                "{line}"
            ),
            Err(_) => assert!(
                line.contains("check_success=0i") && !line.contains("pending"),
                "a failed check must never carry a count: {line}"
            ),
        }
    }
}
