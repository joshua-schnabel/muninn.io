//! `ConfigV1` — the serde target, mirroring `muninn.yaml` one to one.
//!
//! This layer does no resolution: paths stay strings, secrets stay paths,
//! addresses stay unparsed. Its only jobs are to accept exactly the documented
//! shape and to reject everything else. Resolution happens in
//! [`crate::config::normalised`], after validation has run.
//!
//! # `deny_unknown_fields` everywhere
//!
//! Every struct here carries it, and that is not negotiable. A misspelled
//! `exclude_mountpoint` that serde silently ignores leaves the operator
//! believing an exclusion is in effect while it is not — the configuration reads
//! correctly, the agent runs, and the metrics are wrong. Failing the load is the
//! only outcome that cannot be mistaken for success.
//!
//! # Everything is off by default
//!
//! `enabled` defaults to `false` for every module and output. A module the YAML
//! does not name does not collect. This is why there are no profiles: what the
//! file says is exactly what runs.

use serde::Deserialize;

use crate::duration::ConfigDuration;

/// The schema version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Top level
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV1 {
    /// Checked before anything else. An unknown version has to be reported as an
    /// unknown version, not as forty complaints about keys that moved.
    pub version: u32,

    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub modules: ModulesConfig,
    #[serde(default)]
    pub outputs: OutputsConfig,
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(default = "default_interval")]
    pub interval: ConfigDuration,
    #[serde(default = "default_interval")]
    pub flush_interval: ConfigDuration,
    /// Empty means "ask the operating system", which inside a container answers
    /// with the container ID. Validation warns about that; it cannot refuse it,
    /// because running muninn directly on a host is a legitimate case where the
    /// OS hostname is exactly right.
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub omit_hostname: bool,
}

fn default_interval() -> ConfigDuration {
    ConfigDuration::from_secs(30)
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            flush_interval: default_interval(),
            hostname: String::new(),
            omit_hostname: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "default_grace")]
    pub shutdown_grace_period: ConfigDuration,
    #[serde(default = "default_start_timeout")]
    pub telegraf_start_timeout: ConfigDuration,
    #[serde(default = "default_generated_config_path")]
    pub generated_config_path: String,
    /// Where the host filesystem is mounted. `""` means "running directly on the
    /// host, no prefix applies".
    #[serde(default = "default_host_mount_prefix")]
    pub host_mount_prefix: String,
}

fn default_grace() -> ConfigDuration {
    ConfigDuration::from_secs(20)
}
fn default_start_timeout() -> ConfigDuration {
    ConfigDuration::from_secs(15)
}
fn default_generated_config_path() -> String {
    "/run/muninn/telegraf.conf".to_string()
}
fn default_host_mount_prefix() -> String {
    "/hostfs".to_string()
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_period: default_grace(),
            telegraf_start_timeout: default_start_timeout(),
            generated_config_path: default_generated_config_path(),
            host_mount_prefix: default_host_mount_prefix(),
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum LogFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Telegraf has two verbosity knobs, not five, so the mapping is coarse and
    /// stated once here rather than guessed at the call site.
    pub fn telegraf_flags(&self) -> (bool, bool) {
        match self {
            LogLevel::Trace | LogLevel::Debug => (true, false), // debug
            LogLevel::Info => (false, false),
            LogLevel::Warn | LogLevel::Error => (false, true), // quiet
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default)]
    pub level: LogLevel,
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    #[serde(default = "default_health_listen")]
    pub listen: String,
}

fn default_health_listen() -> String {
    "0.0.0.0:8080".to_string()
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            listen: default_health_listen(),
        }
    }
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

/// A module with nothing to configure beyond being on or off.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimpleModule {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisksModule {
    #[serde(default)]
    pub enabled: bool,
    /// Filesystem types to skip → `inputs.disk.ignore_fs`.
    #[serde(default)]
    pub exclude_filesystems: Vec<String>,
    /// Mount-point globs to drop. `inputs.disk` has no path exclusion, so this
    /// becomes a `tagdrop` sub-table — see ADR-0007.
    #[serde(default)]
    pub exclude_mountpoints: Vec<String>,
    /// Restrict collection → `inputs.disk.mount_points`. Setting this makes the
    /// module an allow-list.
    #[serde(default)]
    pub include_mountpoints: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskIoModule {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub include_devices: Vec<String>,
    #[serde(default)]
    pub exclude_devices: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkModule {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub include_interfaces: Vec<String>,
    #[serde(default)]
    pub exclude_interfaces: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerModule {
    /// Off by default, and that is a security decision: socket access is
    /// root-equivalent on the host. See ADR-0010.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_docker_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub container_include: Vec<String>,
    #[serde(default)]
    pub container_exclude: Vec<String>,
    #[serde(default = "default_docker_timeout")]
    pub timeout: ConfigDuration,
    /// Which container states to collect.
    ///
    /// Defaults to running only, which is the honest representation: a stopped
    /// container that disappears from the metrics is unambiguous, where one
    /// reporting zero CPU is indistinguishable from an idle container.
    ///
    /// Add `exited` when the interesting event is a container that stopped —
    /// alerting on a crashed container needs it to still be reported.
    #[serde(default = "default_container_states")]
    pub container_states: Vec<String>,
}

fn default_docker_endpoint() -> String {
    "unix:///var/run/docker.sock".to_string()
}
fn default_docker_timeout() -> ConfigDuration {
    ConfigDuration::from_secs(5)
}
fn default_container_states() -> Vec<String> {
    vec!["running".to_string()]
}

impl Default for DockerModule {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_docker_endpoint(),
            container_include: Vec::new(),
            container_exclude: Vec::new(),
            timeout: default_docker_timeout(),
            container_states: default_container_states(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesModule {
    #[serde(default)]
    pub enabled: bool,
    /// Package state changes slowly and the check is comparatively expensive, so
    /// it runs on its own schedule rather than at `agent.interval`.
    #[serde(default = "default_updates_interval")]
    pub interval: ConfigDuration,
    #[serde(default = "default_true")]
    pub security_only_metric: bool,
}

fn default_updates_interval() -> ConfigDuration {
    ConfigDuration::from_secs(3600)
}
fn default_true() -> bool {
    true
}

impl Default for UpdatesModule {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: default_updates_interval(),
            security_only_metric: true,
        }
    }
}

/// Whether a newer image is available, under the same tag, for each running
/// container.
///
/// Telegraf has no plugin for this either, so like [`UpdatesModule`] it runs
/// through `inputs.exec` and muninn does the work itself — here by asking the
/// Docker daemon to resolve the tag against the registry
/// (`GET /distribution/{name}/json`), rather than muninn talking TLS to a
/// registry directly. See ADR-0013.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageUpdatesModule {
    /// Off by default: it needs the same Docker socket access as `docker`,
    /// which is root-equivalent on the host. See ADR-0010.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_docker_endpoint")]
    pub endpoint: String,
    /// Also the timeout for the startup reachability probe and for each
    /// individual Docker API call the check makes.
    #[serde(default = "default_docker_timeout")]
    pub timeout: ConfigDuration,
    /// Registry lookups are rate-limited and comparatively expensive, so this
    /// runs on its own schedule rather than at `agent.interval` — the same
    /// reasoning as [`UpdatesModule::interval`].
    #[serde(default = "default_updates_interval")]
    pub interval: ConfigDuration,
    #[serde(default)]
    pub container_include: Vec<String>,
    #[serde(default)]
    pub container_exclude: Vec<String>,
}

impl Default for ImageUpdatesModule {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_docker_endpoint(),
            timeout: default_docker_timeout(),
            interval: default_updates_interval(),
            container_include: Vec::new(),
            container_exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulesConfig {
    #[serde(default)]
    pub cpu: SimpleModule,
    #[serde(default)]
    pub memory: SimpleModule,
    #[serde(default)]
    pub load: SimpleModule,
    #[serde(default)]
    pub system: SimpleModule,
    #[serde(default)]
    pub swap: SimpleModule,
    #[serde(default)]
    pub processes: SimpleModule,
    #[serde(default)]
    pub disks: DisksModule,
    #[serde(default)]
    pub disk_io: DiskIoModule,
    #[serde(default)]
    pub network: NetworkModule,
    #[serde(default)]
    pub docker: DockerModule,
    #[serde(default)]
    pub updates: UpdatesModule,
    #[serde(default)]
    pub image_updates: ImageUpdatesModule,
}

impl ModulesConfig {
    /// Names of the enabled modules, in a fixed order.
    ///
    /// Fixed rather than derived from the YAML, so `/status` and the generated
    /// configuration do not change shape because someone reordered their file.
    pub fn enabled_names(&self) -> Vec<&'static str> {
        let flags = [
            ("cpu", self.cpu.enabled),
            ("memory", self.memory.enabled),
            ("load", self.load.enabled),
            ("system", self.system.enabled),
            ("swap", self.swap.enabled),
            ("processes", self.processes.enabled),
            ("disks", self.disks.enabled),
            ("disk_io", self.disk_io.enabled),
            ("network", self.network.enabled),
            ("docker", self.docker.enabled),
            ("updates", self.updates.enabled),
            ("image_updates", self.image_updates.enabled),
        ];
        flags
            .into_iter()
            .filter_map(|(name, on)| on.then_some(name))
            .collect()
    }

    pub fn any_enabled(&self) -> bool {
        !self.enabled_names().is_empty()
    }

    /// Whether any enabled module reads host state through the mount prefix.
    /// Used by validation to decide whether an empty prefix is worth warning
    /// about, and later by `check-runtime`.
    pub fn any_needs_host_mount(&self) -> bool {
        self.cpu.enabled
            || self.memory.enabled
            || self.load.enabled
            || self.system.enabled
            || self.swap.enabled
            || self.processes.enabled
            || self.disks.enabled
            || self.disk_io.enabled
            || self.network.enabled
            || self.updates.enabled
    }
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    #[serde(default)]
    pub ca_file: Option<String>,
    #[serde(default)]
    pub cert_file: Option<String>,
    #[serde(default)]
    pub key_file: Option<String>,
    /// Disables certificate verification entirely. Validation logs a prominent
    /// warning; it does not refuse, because a lab with a self-signed CA is a
    /// legitimate if unfortunate case.
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfluxdbOutput {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub bucket: String,
    /// A path, never a token. There is no key anywhere that accepts the value.
    #[serde(default)]
    pub token_file: String,
    #[serde(default = "default_influx_timeout")]
    pub timeout: ConfigDuration,
    #[serde(default)]
    pub tls: TlsConfig,
}

fn default_influx_timeout() -> ConfigDuration {
    ConfigDuration::from_secs(5)
}

impl Default for InfluxdbOutput {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            organization: String::new(),
            bucket: String::new(),
            token_file: String::new(),
            timeout: default_influx_timeout(),
            tls: TlsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicAuthConfig {
    #[serde(default)]
    pub username: Option<String>,
    /// A path, never a password.
    #[serde(default)]
    pub password_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusOutput {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_prometheus_listen")]
    pub listen: String,
    #[serde(default = "default_prometheus_path")]
    pub path: String,
    #[serde(default = "default_expiration")]
    pub expiration_interval: ConfigDuration,
    #[serde(default)]
    pub basic_auth: BasicAuthConfig,
}

fn default_prometheus_listen() -> String {
    "0.0.0.0:9273".to_string()
}
fn default_prometheus_path() -> String {
    "/metrics".to_string()
}
fn default_expiration() -> ConfigDuration {
    ConfigDuration::from_secs(60)
}

impl Default for PrometheusOutput {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_prometheus_listen(),
            path: default_prometheus_path(),
            expiration_interval: default_expiration(),
            basic_auth: BasicAuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputsConfig {
    #[serde(default)]
    pub influxdb: InfluxdbOutput,
    #[serde(default)]
    pub prometheus: PrometheusOutput,
}

impl OutputsConfig {
    pub fn enabled_names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.influxdb.enabled {
            v.push("influxdb");
        }
        if self.prometheus.enabled {
            v.push("prometheus");
        }
        v
    }

    pub fn any_enabled(&self) -> bool {
        self.influxdb.enabled || self.prometheus.enabled
    }
}
