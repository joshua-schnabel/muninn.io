//! Monitoring modules and outputs. Each one turns a slice of validated
//! configuration into Telegraf plugin instances.
//!
//! # The module contract
//!
//! ```ignore
//! pub trait MonitoringModule {
//!     fn id(&self) -> &'static str;
//!     fn validate(&self, ctx: &ValidationContext) -> Result<()>;
//!     fn requirements(&self) -> Requirements;                 // mounts, capabilities, host OS
//!     fn render(&self, ctx: &RenderContext) -> Result<Vec<PluginInstance>>;
//!     fn merge_key(&self) -> Option<PluginMergeKey> { None }
//! }
//! ```
//!
//! Validation and rendering are separate on purpose: every reason a module can
//! refuse must be decidable *before* anything is written or started, so a bad
//! config fails at step 3 of startup with a message naming the YAML key, not at
//! step 11 as a Telegraf parse error.
//!
//! `requirements()` is what makes `muninn check-runtime` possible: modules
//! declare the host paths and capabilities they need, so the program can verify
//! only what is actually enabled instead of demanding every mount from everyone.
//!
//! # Two mappings that are not one-to-one
//!
//! **`load` and `system` render into a single plugin.** Telegraf has no
//! `inputs.load`; load averages, uptime and logged-in users all come from
//! `inputs.system` selected via `include = [...]`. Emitting two
//! `[[inputs.system]]` blocks would duplicate metrics, so both modules return
//! the same `merge_key` and the renderer unions their `include` groups into one
//! instance. See `docs/adr/0008-system-and-load-merge.md`.
//!
//! **Exclusions are not plugin options.** `inputs.disk` has no mount-point
//! exclusion, `inputs.diskio` and `inputs.net` have no exclusion at all — they
//! only offer include lists. Every `exclude_*` key in muninn's YAML therefore
//! renders into a `tagdrop` sub-table rather than a plugin option. See
//! `docs/adr/0007-tagdrop-and-render-order.md`.
//!
//! # Planned modules
//!
//! `cpu` · `memory` · `load` · `system` · `swap` · `processes` · `disks` ·
//! `disk_io` · `network` · `docker` (off by default) · `updates` (experimental,
//! off by default until the spike in WP1 concludes).
//!
//! Outputs: `influxdb` (`outputs.influxdb_v2`) and `prometheus`
//! (`outputs.prometheus_client`). At least one must be enabled; both may be.
//!
//! Implementation lands in WP4, WP5, WP9 and WP10 — see `docs/roadmap.md`.
