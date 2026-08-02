//! The input modules.
//!
//! Each is a thin, declarative mapping from muninn's options onto a Telegraf
//! plugin. They are kept in one file rather than one file each because reading
//! them side by side is how you notice an inconsistency — nine files of twenty
//! lines would hide exactly the kind of drift this layer has to avoid.
//!
//! Ranks decide the order plugins appear in the generated file. They are spaced
//! by ten so a module can be inserted between two others without renumbering.

use muninn_core::Config;
use muninn_telegraf::PluginInstance;

use crate::{MonitoringModule, RenderContext, Requirements};

const RANK_CPU: u16 = 10;
const RANK_MEM: u16 = 20;
const RANK_SYSTEM: u16 = 30;
const RANK_SWAP: u16 = 40;
const RANK_PROCESSES: u16 = 50;
const RANK_DISK: u16 = 60;
const RANK_DISKIO: u16 = 70;
const RANK_NET: u16 = 80;
const RANK_DOCKER: u16 = 90;
const RANK_UPDATES: u16 = 100;

/// The helper muninn ships for the updates module. Its behaviour is specified by
/// `spikes/updates/probe.sh`, which the WP1 spike verified against four
/// distributions.
const UPDATE_HELPER: &str = "/usr/local/bin/muninn-update-check";

// ---------------------------------------------------------------------------

pub struct Cpu;

impl MonitoringModule for Cpu {
    fn id(&self) -> &'static str {
        "cpu"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.cpu.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![
            PluginInstance::input("cpu", RANK_CPU)
                .from_module("cpu")
                .scalar("percpu", true)
                .scalar("totalcpu", true)
                .scalar("collect_cpu_time", false)
                // `usage_active` includes iowait, so it reads as CPU saturation
                // when the machine is actually waiting on disk. Off by default.
                .scalar("report_active", false),
        ]
    }
}

// ---------------------------------------------------------------------------

pub struct Memory;

impl MonitoringModule for Memory {
    fn id(&self) -> &'static str {
        "memory"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.memory.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![PluginInstance::input("mem", RANK_MEM).from_module("memory")]
    }
}

// ---------------------------------------------------------------------------

/// Load averages.
///
/// Renders into `inputs.system` with the `load` group, and shares a merge key
/// with [`System`] so the two never produce separate instances.
pub struct Load;

impl MonitoringModule for Load {
    fn id(&self) -> &'static str {
        "load"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.load.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![
            PluginInstance::input("system", RANK_SYSTEM)
                .merge_key("system")
                .from_module("load")
                .scalar("include", vec!["load"]),
        ]
    }
}

/// Uptime and logged-in users.
///
/// Note this deliberately does *not* include the `load` group, even though
/// Telegraf's own default (`include = ["legacy"]`) would. muninn is explicit
/// rather than convenient: a module you did not enable does not collect.
pub struct System;

impl MonitoringModule for System {
    fn id(&self) -> &'static str {
        "system"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.system.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc", "var"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![
            PluginInstance::input("system", RANK_SYSTEM)
                .merge_key("system")
                .from_module("system")
                .scalar("include", vec!["uptime", "users"]),
        ]
    }
}

// ---------------------------------------------------------------------------

pub struct Swap;

impl MonitoringModule for Swap {
    fn id(&self) -> &'static str {
        "swap"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.swap.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![PluginInstance::input("swap", RANK_SWAP).from_module("swap")]
    }
}

// ---------------------------------------------------------------------------

pub struct Processes;

impl MonitoringModule for Processes {
    fn id(&self) -> &'static str {
        "processes"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.processes.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, _ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        vec![PluginInstance::input("processes", RANK_PROCESSES).from_module("processes")]
    }
}

// ---------------------------------------------------------------------------

pub struct Disks;

impl MonitoringModule for Disks {
    fn id(&self) -> &'static str {
        "disks"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.disks.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.disks;
        vec![
            PluginInstance::input("disk", RANK_DISK)
                .from_module("disks")
                .list("mount_points", &m.include_mountpoints)
                .list("ignore_fs", &m.exclude_filesystems)
                // `inputs.disk` can filter by filesystem type but not by path,
                // so path exclusions become a metric filter.
                .tagdrop("path", &m.exclude_mountpoints),
        ]
    }
}

// ---------------------------------------------------------------------------

pub struct DiskIo;

impl MonitoringModule for DiskIo {
    fn id(&self) -> &'static str {
        "disk_io"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.disk_io.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc", "sys"])
    }
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.disk_io;
        vec![
            PluginInstance::input("diskio", RANK_DISKIO)
                .from_module("disk_io")
                .list("devices", &m.include_devices)
                // The device tag is `name`, not `device`.
                .tagdrop("name", &m.exclude_devices),
        ]
    }
}

// ---------------------------------------------------------------------------

pub struct Network;

impl MonitoringModule for Network {
    fn id(&self) -> &'static str {
        "network"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.network.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements::host(&["proc"])
    }
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.network;
        vec![
            PluginInstance::input("net", RANK_NET)
                .from_module("network")
                .list("interfaces", &m.include_interfaces)
                .tagdrop("interface", &m.exclude_interfaces),
        ]
    }
}

// ---------------------------------------------------------------------------

pub struct Docker;

impl MonitoringModule for Docker {
    fn id(&self) -> &'static str {
        "docker"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.docker.enabled
    }
    fn requirements(&self) -> Requirements {
        let mut req = Requirements::default();
        // The socket is not under the host mount prefix — it is its own,
        // deliberate, root-equivalent grant. See ADR-0010.
        req.absolute_paths.push("/var/run/docker.sock".to_string());
        req
    }
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.docker;
        vec![
            PluginInstance::input("docker", RANK_DOCKER)
                .from_module("docker")
                .scalar("endpoint", m.endpoint.clone())
                .scalar("timeout", m.timeout.as_telegraf())
                .list("container_name_include", &m.container_include)
                .list("container_name_exclude", &m.container_exclude)
                // A stopped container disappears from the metrics rather than
                // reporting zeros, which is the honest representation.
                .scalar("container_state_include", vec!["running"])
                .scalar("perdevice_include", vec!["cpu"])
                .scalar("total_include", vec!["cpu", "blkio", "network"]),
        ]
    }
}

// ---------------------------------------------------------------------------

/// Pending package updates on the host.
///
/// Telegraf has no package input plugin, so this runs the helper muninn ships
/// and parses its influx line protocol. The approach — read-only host mounts
/// plus a simulated `apt-get -s dist-upgrade` — was settled by the WP1 spike,
/// which measured it against Debian 12/13 and Ubuntu 22.04/24.04.
pub struct Updates;

impl MonitoringModule for Updates {
    fn id(&self) -> &'static str {
        "updates"
    }
    fn enabled(&self, c: &Config) -> bool {
        c.modules.updates.enabled
    }
    fn requirements(&self) -> Requirements {
        Requirements {
            // /usr is needed because /etc/os-release is a symlink into it, and a
            // mount set without it reports "not a Debian host" for a machine
            // that plainly is. Found the hard way during the spike.
            host_paths: vec!["var", "etc", "usr"],
            debian_family_only: true,
            ..Default::default()
        }
    }
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.updates;
        let hostfs = ctx
            .config
            .runtime
            .host_mount_prefix
            .clone()
            .unwrap_or_else(|| "/".to_string());

        vec![
            PluginInstance::input("exec", RANK_UPDATES)
                .from_module("updates")
                .scalar("commands", vec![UPDATE_HELPER])
                .scalar("environment", vec![format!("HOSTFS={hostfs}")])
                // Its own schedule: package state changes on the scale of hours,
                // and a full apt resolution is expensive next to reading /proc.
                .scalar("interval", m.interval.as_telegraf())
                // Generous, because apt has to parse the host's whole package
                // index. Well under the interval either way.
                .scalar("timeout", "30s")
                .scalar("data_format", "influx")
                // The helper reports a failed check as data (check_success=0)
                // and exits 0, so a non-zero exit would be a helper bug. Telegraf
                // should surface it rather than swallow it.
                .scalar("ignore_error", false),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_unique_and_ordered() {
        let ranks = [
            RANK_CPU,
            RANK_MEM,
            RANK_SYSTEM,
            RANK_SWAP,
            RANK_PROCESSES,
            RANK_DISK,
            RANK_DISKIO,
            RANK_NET,
            RANK_DOCKER,
            RANK_UPDATES,
        ];
        let mut sorted = ranks.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ranks.len(), "ranks must be unique");
        assert_eq!(sorted, ranks.to_vec(), "ranks must already be in order");
    }

    /// `load` and `system` must agree on the merge key, or they emit two
    /// `[[inputs.system]]` blocks and every metric is collected twice.
    #[test]
    fn load_and_system_share_a_merge_key() {
        let cfg = crate::tests::config_with(|c| {
            c.modules.load.enabled = true;
            c.modules.system.enabled = true;
        });
        let ctx = RenderContext::new(&cfg);
        let load = Load.render(&ctx).remove(0);
        let system = System.render(&ctx).remove(0);
        assert_eq!(load.merge_key, system.merge_key);
        assert!(load.merge_key.is_some());
        assert_eq!(load.plugin, system.plugin);
    }

    /// No MVP module may require a Linux capability: the hardening baseline
    /// drops them all, so a module that needed one would silently not work.
    #[test]
    fn no_module_requires_a_capability() {
        for module in crate::all_modules() {
            assert!(
                module.requirements().capabilities.is_empty(),
                "{} requires a capability, which the hardening baseline drops",
                module.id()
            );
        }
    }

    /// The Docker socket must not be declared as a host path: it is a separate,
    /// deliberate grant, and folding it into the mount prefix would make it look
    /// like part of the ordinary host mount.
    #[test]
    fn only_docker_requires_an_absolute_path() {
        for module in crate::all_modules() {
            let req = module.requirements();
            if module.id() == "docker" {
                assert_eq!(req.absolute_paths, vec!["/var/run/docker.sock".to_string()]);
                assert!(req.host_paths.is_empty());
            } else {
                assert!(
                    req.absolute_paths.is_empty(),
                    "{} should not need an absolute path",
                    module.id()
                );
            }
        }
    }

    /// Found during the WP1 spike: /etc/os-release is a symlink into /usr/lib,
    /// so a mount set with /etc but not /usr reports "not a Debian host".
    #[test]
    fn the_updates_module_requires_usr_for_the_os_release_symlink() {
        let req = Updates.requirements();
        assert!(req.host_paths.contains(&"usr"), "got: {:?}", req.host_paths);
        assert!(req.debian_family_only);
    }
}
