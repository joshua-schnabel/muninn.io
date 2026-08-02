//! Semantic validation: the rules serde cannot express.
//!
//! Every rule here exists because breaking it produces a failure that is harder
//! to diagnose later than here — a listener that silently does not start, an
//! agent that collects nothing, a metric that is quietly wrong. Each carries a
//! comment saying which failure it prevents, because a rule whose reason has
//! been forgotten is a rule someone eventually deletes.
//!
//! # Errors versus warnings
//!
//! An error is a configuration that cannot do what it says. A warning is one
//! that can, but probably does not mean to.
//!
//! Warnings are *returned* rather than logged, because validation runs before
//! the tracing subscriber exists — the log level to initialise it with comes
//! from this very configuration. Anything logged here would go nowhere. The
//! caller emits them once logging is up.

use std::net::SocketAddr;

use crate::config::model::*;
use crate::error::{MuninnError, Result};
use crate::secret::Secret;

/// Validate `cfg`, returning warnings on success.
pub fn validate(cfg: &ConfigV1) -> Result<Vec<String>> {
    // The version is checked by the loader before we get here, so by this point
    // every key below is known to belong to this schema.
    let mut warnings = Vec::new();

    validate_agent(cfg, &mut warnings)?;
    validate_runtime(cfg, &mut warnings)?;
    validate_modules(cfg, &mut warnings)?;
    validate_outputs(cfg, &mut warnings)?;
    validate_listeners(cfg)?;

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

fn validate_agent(cfg: &ConfigV1, warnings: &mut Vec<String>) -> Result<()> {
    // A zero interval makes Telegraf's collection timer fire continuously.
    require_positive(cfg.agent.interval, "agent.interval")?;
    require_positive(cfg.agent.flush_interval, "agent.flush_interval")?;

    // Sub-second host metrics are not a resolution anyone needs, and asking for
    // them costs real CPU on the host being measured. Almost always a unit
    // mistake — `interval: 500ms` where `500s` was meant.
    if cfg.agent.interval.as_millis() < 1000 {
        return Err(MuninnError::config(format!(
            "agent.interval is {} — sub-second collection is not supported; use 1s or more",
            cfg.agent.interval
        )));
    }

    // Not an error: a long flush is a legitimate choice for a low-volume fleet.
    // But it delays every metric by up to that long, which surprises people who
    // set it while thinking about write volume.
    if cfg.agent.flush_interval > cfg.agent.interval {
        warnings.push(format!(
            "agent.flush_interval ({}) is longer than agent.interval ({}) — \
             metrics will be delayed by up to one flush period",
            cfg.agent.flush_interval, cfg.agent.interval
        ));
    }

    // `omit_hostname` with an explicit hostname is contradictory: one of the two
    // was almost certainly meant to be the other.
    if cfg.agent.omit_hostname && !cfg.agent.hostname.is_empty() {
        warnings.push(format!(
            "agent.omit_hostname is true, so agent.hostname ('{}') has no effect",
            cfg.agent.hostname
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

fn validate_runtime(cfg: &ConfigV1, warnings: &mut Vec<String>) -> Result<()> {
    require_positive(
        cfg.runtime.shutdown_grace_period,
        "runtime.shutdown_grace_period",
    )?;
    require_positive(
        cfg.runtime.telegraf_start_timeout,
        "runtime.telegraf_start_timeout",
    )?;

    // A relative path would be resolved against whatever directory the process
    // happens to start in, which in a container is not something the operator
    // controls or can predict.
    if !is_absolute_path(&cfg.runtime.generated_config_path) {
        return Err(MuninnError::config(format!(
            "runtime.generated_config_path '{}' must be an absolute path",
            cfg.runtime.generated_config_path
        )));
    }

    // The generated file holds resolved secrets in plaintext, so it belongs on a
    // tmpfs. muninn cannot verify that from the path alone, but a path outside
    // the conventional runtime directories is worth questioning — persisting it
    // would put credentials on disk. See ADR-0003.
    if !cfg.runtime.generated_config_path.starts_with("/run/")
        && !cfg.runtime.generated_config_path.starts_with("/tmp/")
        && !cfg.runtime.generated_config_path.starts_with("/dev/shm/")
    {
        warnings.push(format!(
            "runtime.generated_config_path '{}' is outside /run, /tmp and /dev/shm — \
             this file contains resolved secrets and must live on a tmpfs, never on persistent storage",
            cfg.runtime.generated_config_path
        ));
    }

    if !cfg.runtime.host_mount_prefix.is_empty() && !cfg.runtime.host_mount_prefix.starts_with('/')
    {
        return Err(MuninnError::config(format!(
            "runtime.host_mount_prefix '{}' must be an absolute path, or empty to mean \
             'running directly on the host'",
            cfg.runtime.host_mount_prefix
        )));
    }

    // What the grace period has to cover is a *write*, not a collection cycle.
    // Telegraf does not wait for the next flush tick on shutdown: it logs
    // "Hang on, flushing any cached metrics before shutdown" and flushes
    // immediately (agent.go, runOutputs). So the bound that matters is how long
    // one write attempt may take, which is the output timeout — comparing
    // against agent.flush_interval would be measuring the wrong thing.
    if cfg.outputs.influxdb.enabled
        && cfg.runtime.shutdown_grace_period <= cfg.outputs.influxdb.timeout
    {
        warnings.push(format!(
            "runtime.shutdown_grace_period ({}) does not exceed outputs.influxdb.timeout ({}) — \
             a shutdown flush cannot complete even one write attempt, so buffered metrics \
             will be lost on every restart",
            cfg.runtime.shutdown_grace_period, cfg.outputs.influxdb.timeout
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

fn validate_modules(cfg: &ConfigV1, warnings: &mut Vec<String>) -> Result<()> {
    let m = &cfg.modules;

    // An agent with outputs but no modules starts, connects, and reports
    // nothing — indistinguishable from a broken collector. There are no implicit
    // defaults, so this is always a mistake rather than a minimal setup.
    if !m.any_enabled() {
        return Err(MuninnError::config(
            "no module is enabled — muninn would collect nothing. \
             Enable at least one under `modules:` (see docs/modules.md)"
                .to_string(),
        ));
    }

    // Host metrics from a container need the prefix. Without it Telegraf reports
    // the *container's* CPU, memory and disks as the host's: plausible numbers
    // about the wrong machine, with no error anywhere. This cannot be fatal —
    // running muninn directly on a host is legitimate and wants an empty prefix
    // — so it is a warning the caller escalates once it knows it is in a
    // container.
    if cfg.runtime.host_mount_prefix.is_empty() && m.any_needs_host_mount() {
        warnings.push(
            "runtime.host_mount_prefix is empty, which means 'running directly on the host'. \
             In a container this reports the container's own CPU, memory and disks as if they \
             were the host's — set it to /hostfs and mount /:/hostfs:ro"
                .to_string(),
        );
    }

    reject_blank_patterns(
        &m.disks.exclude_filesystems,
        "modules.disks.exclude_filesystems",
    )?;
    reject_blank_patterns(
        &m.disks.exclude_mountpoints,
        "modules.disks.exclude_mountpoints",
    )?;
    reject_blank_patterns(
        &m.disks.include_mountpoints,
        "modules.disks.include_mountpoints",
    )?;
    reject_blank_patterns(
        &m.disk_io.include_devices,
        "modules.disk_io.include_devices",
    )?;
    reject_blank_patterns(
        &m.disk_io.exclude_devices,
        "modules.disk_io.exclude_devices",
    )?;
    reject_blank_patterns(
        &m.network.include_interfaces,
        "modules.network.include_interfaces",
    )?;
    reject_blank_patterns(
        &m.network.exclude_interfaces,
        "modules.network.exclude_interfaces",
    )?;
    reject_blank_patterns(
        &m.docker.container_include,
        "modules.docker.container_include",
    )?;
    reject_blank_patterns(
        &m.docker.container_exclude,
        "modules.docker.container_exclude",
    )?;

    // An include list is an allow-list: anything not matching stops being
    // collected. Combined with an exclude list that is a contradiction worth
    // pointing at, because the include already excluded everything else.
    warn_on_include_and_exclude(
        &m.disks.include_mountpoints,
        &m.disks.exclude_mountpoints,
        "modules.disks",
        "mountpoints",
        warnings,
    );
    warn_on_include_and_exclude(
        &m.disk_io.include_devices,
        &m.disk_io.exclude_devices,
        "modules.disk_io",
        "devices",
        warnings,
    );
    warn_on_include_and_exclude(
        &m.network.include_interfaces,
        &m.network.exclude_interfaces,
        "modules.network",
        "interfaces",
        warnings,
    );

    if m.docker.enabled {
        require_positive(m.docker.timeout, "modules.docker.timeout")?;
        if m.docker.endpoint.is_empty() {
            return Err(MuninnError::config(
                "modules.docker.endpoint must not be empty when the docker module is enabled"
                    .to_string(),
            ));
        }
        // unix:// and tcp:// are what the plugin accepts. Anything else is a
        // typo that would surface as a connection error after startup.
        if !m.docker.endpoint.starts_with("unix://") && !m.docker.endpoint.starts_with("tcp://") {
            return Err(MuninnError::config(format!(
                "modules.docker.endpoint '{}' must start with unix:// or tcp://",
                m.docker.endpoint
            )));
        }
        warnings.push(
            "the docker module is enabled: access to the Docker socket is equivalent to root \
             on the host, and mounting it read-only does not change that. \
             Consider a socket proxy — see docs/modules.md#docker"
                .to_string(),
        );
    }

    if m.updates.enabled {
        require_positive(m.updates.interval, "modules.updates.interval")?;
        // Package state changes on the scale of hours. A one-minute interval
        // would run a full apt resolution 60 times an hour for a number that
        // moves once a day.
        if m.updates.interval.as_secs() < 60 {
            return Err(MuninnError::config(format!(
                "modules.updates.interval is {} — package state does not change that fast; \
                 use 1m or more (1h is the default and is usually right)",
                m.updates.interval
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

fn validate_outputs(cfg: &ConfigV1, warnings: &mut Vec<String>) -> Result<()> {
    let o = &cfg.outputs;

    // Collecting metrics and sending them nowhere is a misconfiguration, not a
    // minimal setup.
    if !o.any_enabled() {
        return Err(MuninnError::config(
            "no output is enabled — muninn would collect metrics and send them nowhere. \
             Enable outputs.influxdb, outputs.prometheus, or both"
                .to_string(),
        ));
    }

    if o.influxdb.enabled {
        require_non_empty(&o.influxdb.url, "outputs.influxdb.url")?;
        require_non_empty(&o.influxdb.organization, "outputs.influxdb.organization")?;
        require_non_empty(&o.influxdb.bucket, "outputs.influxdb.bucket")?;
        require_non_empty(&o.influxdb.token_file, "outputs.influxdb.token_file")?;
        require_positive(o.influxdb.timeout, "outputs.influxdb.timeout")?;

        if !o.influxdb.url.starts_with("http://") && !o.influxdb.url.starts_with("https://") {
            return Err(MuninnError::config(format!(
                "outputs.influxdb.url '{}' must be an absolute URL starting with http:// or https://",
                o.influxdb.url
            )));
        }

        // Read it now rather than at first write. A missing token discovered ten
        // minutes in looks like an InfluxDB outage; discovered here it names the
        // path. The value is dropped immediately — this is a readability check.
        Secret::from_file(&o.influxdb.token_file)?;

        validate_tls(&o.influxdb.tls, "outputs.influxdb.tls", warnings)?;

        if o.influxdb.url.starts_with("http://") {
            warnings.push(format!(
                "outputs.influxdb.url '{}' is plaintext HTTP — the API token is sent with \
                 every write and is readable by anyone on the path",
                o.influxdb.url
            ));
        }
    }

    if o.prometheus.enabled {
        parse_listen(&o.prometheus.listen, "outputs.prometheus.listen")?;
        require_positive(
            o.prometheus.expiration_interval,
            "outputs.prometheus.expiration_interval",
        )?;

        if !o.prometheus.path.starts_with('/') {
            return Err(MuninnError::config(format!(
                "outputs.prometheus.path '{}' must start with '/'",
                o.prometheus.path
            )));
        }

        // Shorter than the collection interval and Prometheus sees gaps between
        // scrapes for metrics that are being collected perfectly well.
        if o.prometheus.expiration_interval < cfg.agent.interval {
            warnings.push(format!(
                "outputs.prometheus.expiration_interval ({}) is shorter than agent.interval ({}) — \
                 metrics will expire before they are refreshed and scrapes will show gaps",
                o.prometheus.expiration_interval, cfg.agent.interval
            ));
        }

        // Half a credential is not a credential: whichever half is missing, the
        // result is an endpoint that does not authenticate.
        let auth = &o.prometheus.basic_auth;
        match (&auth.username, &auth.password_file) {
            (Some(u), Some(p)) => {
                if u.is_empty() {
                    return Err(MuninnError::config(
                        "outputs.prometheus.basic_auth.username must not be empty".to_string(),
                    ));
                }
                Secret::from_file(p)?;
            }
            (Some(_), None) => {
                return Err(MuninnError::config(
                    "outputs.prometheus.basic_auth.username is set but password_file is not — \
                     set both, or neither to disable authentication"
                        .to_string(),
                ));
            }
            (None, Some(_)) => {
                return Err(MuninnError::config(
                    "outputs.prometheus.basic_auth.password_file is set but username is not — \
                     set both, or neither to disable authentication"
                        .to_string(),
                ));
            }
            (None, None) => {}
        }
    }

    Ok(())
}

fn validate_tls(tls: &TlsConfig, prefix: &str, warnings: &mut Vec<String>) -> Result<()> {
    // A client certificate without its key cannot be used, and a key without its
    // certificate is meaningless. Either way TLS would silently fall back to
    // server-only authentication.
    match (&tls.cert_file, &tls.key_file) {
        (Some(_), None) => {
            return Err(MuninnError::config(format!(
                "{prefix}.cert_file is set but key_file is not — mutual TLS needs both"
            )));
        }
        (None, Some(_)) => {
            return Err(MuninnError::config(format!(
                "{prefix}.key_file is set but cert_file is not — mutual TLS needs both"
            )));
        }
        _ => {}
    }

    for (path, key) in [
        (&tls.ca_file, "ca_file"),
        (&tls.cert_file, "cert_file"),
        (&tls.key_file, "key_file"),
    ] {
        if let Some(p) = path {
            if p.is_empty() {
                return Err(MuninnError::config(format!(
                    "{prefix}.{key} must not be empty (omit it to use the system trust store)"
                )));
            }
            if !std::path::Path::new(p).exists() {
                return Err(MuninnError::runtime(format!(
                    "{prefix}.{key} '{p}' does not exist"
                )));
            }
        }
    }

    if tls.insecure_skip_verify {
        warnings.push(format!(
            "{prefix}.insecure_skip_verify is true — certificate verification is DISABLED. \
             Anyone able to intercept this connection can read your metrics and inject \
             fabricated ones. Fix the certificate or set {prefix}.ca_file instead"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

fn validate_listeners(cfg: &ConfigV1) -> Result<()> {
    let health = parse_listen(&cfg.health.listen, "health.listen")?;

    if cfg.outputs.prometheus.enabled {
        let prom = parse_listen(&cfg.outputs.prometheus.listen, "outputs.prometheus.listen")?;
        if addresses_collide(&health, &prom) {
            return Err(MuninnError::config(format!(
                "health.listen ({}) and outputs.prometheus.listen ({}) cannot both bind — \
                 they share port {}. Give one of them a different port",
                cfg.health.listen,
                cfg.outputs.prometheus.listen,
                health.port()
            )));
        }
    }

    Ok(())
}

/// Whether two listeners would fight over the same socket.
///
/// Equality is not enough. `0.0.0.0:8080` and `127.0.0.1:8080` are different
/// strings, and the second bind still fails — a wildcard address already covers
/// every specific one. Comparing only for equality is the bug this function
/// exists to avoid; huginn.io's check has it, and the symptom is a listener that
/// silently never starts while readiness still reports true.
fn addresses_collide(a: &SocketAddr, b: &SocketAddr) -> bool {
    if a.port() != b.port() {
        return false;
    }
    a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_listen(value: &str, key: &str) -> Result<SocketAddr> {
    value.parse::<SocketAddr>().map_err(|_| {
        MuninnError::config(format!(
            "{key} '{value}' must be an address and port, e.g. '0.0.0.0:8080' or '[::]:8080'. \
             In a container use 0.0.0.0 — a published port never reaches the container's loopback"
        ))
    })
}

/// Whether `value` is an absolute path.
///
/// Accepts both a POSIX absolute path and whatever the host platform considers
/// absolute, and it has to accept both. muninn's artefact is a Linux container,
/// so a configuration naming `/run/muninn/telegraf.conf` must validate wherever
/// it is checked — `muninn validate` on a Windows laptop against a production
/// file is a case worth supporting. Meanwhile the tests, which run on the
/// developer's machine, need real host paths to work.
///
/// `Path::is_absolute` alone would reject the first; `starts_with('/')` alone
/// rejects the second.
fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/') || std::path::Path::new(value).is_absolute()
}

fn require_non_empty(value: &str, key: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MuninnError::config(format!("{key} must not be empty")));
    }
    Ok(())
}

fn require_positive(d: crate::duration::ConfigDuration, key: &str) -> Result<()> {
    if d.is_zero() {
        return Err(MuninnError::config(format!(
            "{key} must be greater than zero"
        )));
    }
    Ok(())
}

/// An empty or whitespace-only glob matches nothing and is always a mistake —
/// usually a stray `- ` left behind while editing a list.
fn reject_blank_patterns(patterns: &[String], key: &str) -> Result<()> {
    if let Some(index) = patterns.iter().position(|p| p.trim().is_empty()) {
        return Err(MuninnError::config(format!(
            "{key}[{index}] is empty — remove the entry, or give it a pattern"
        )));
    }
    Ok(())
}

fn warn_on_include_and_exclude(
    include: &[String],
    exclude: &[String],
    module: &str,
    noun: &str,
    warnings: &mut Vec<String>,
) {
    if !include.is_empty() && !exclude.is_empty() {
        warnings.push(format!(
            "{module} sets both include_{noun} and exclude_{noun} — the include list is already \
             an allow-list, so the exclusions only apply within it"
        ));
    }
}
