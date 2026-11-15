//! Read-only Docker inventory and review. Independent of filesystem scan entries and cleanup.
//!
//! A successful comparison is NOT permission to delete: Docker has no general conditional
//! delete against the inspected revision. In particular, a container's writable layer and an
//! image's references can change after inspection. The existing cleanup hold must stay.

use crate::{rules, sys};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub mod session;

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OBJECTS: usize = 10_000;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_INVENTORY_BYTES: usize = 32 * 1024 * 1024;
const OVERRIDES: &[&str] = &[
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_API_VERSION",
    "DOCKER_TLS",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CERT_PATH",
    "BUILDX_BUILDER",
    "BUILDX_CONFIG",
    "BUILDKIT_HOST",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Container,
    Image,
}

/// Docker-reported bytes, not allocated host bytes or a promise of recovered space.
#[derive(Clone, Debug, Serialize)]
pub struct Object {
    pub key: String,
    pub id: String,
    pub kind: Kind,
    pub label: String,
    pub bytes: Option<u64>,
    pub references: Vec<String>,
    pub state: String,
    /// No row in this prototype is executable, even if it has no known references.
    pub hold: String,
    pub created: String,
    pub image: Option<String>,
    pub project: Option<String>,
    pub mounts: Vec<Mount>,
    /// Retain the entire inspect response privately, including unknown fields. Comparing only
    /// the UI fields would miss restart history, mounts, configuration and platform changes.
    #[serde(skip)]
    inspect: Value,
}

/// Only mount type/name/destination are sent to the UI, never host paths or mount options.
#[derive(Clone, Debug, Serialize)]
pub struct Mount {
    pub kind: String,
    pub name: Option<String>,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Engine {
    pub context: String,
    pub endpoint: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    engine: Engine,
    objects: BTreeMap<String, Object>,
    /// Engine inventory does not include caches of arbitrary Buildx builders.
    pub cache_status: &'static str,
}

/// A backend-owned subset of one inventory. Deliberately not deserialisable from webview data.
#[derive(Clone, Debug)]
pub struct Review {
    engine: Engine,
    objects: BTreeMap<String, Object>,
}

impl Inventory {
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.objects.values()
    }

    /// Confirm keys, never CLI arguments, paths, names, tags or caller-supplied inspect data.
    pub fn review(&self, keys: &[String]) -> Result<Review, String> {
        if keys.is_empty() {
            return Err("No Docker objects selected".into());
        }
        let mut objects = BTreeMap::new();
        for key in keys {
            let object = self.objects.get(key).ok_or("Object absent from the reviewed Docker inventory")?;
            if objects.insert(key.clone(), object.clone()).is_some() {
                return Err("Duplicate Docker selection".into());
            }
        }
        Ok(Review { engine: self.engine.clone(), objects })
    }

    /// Compare only reviewed objects. New objects never enlarge the selection, but a new
    /// container referring to a reviewed image changes that image's references and refuses it.
    /// This is a diagnostic preflight, not an atomic validation-and-delete operation.
    pub fn compare_review(&self, review: &Review) -> Result<(), String> {
        if self.engine != review.engine {
            return Err("Docker endpoint or engine changed; review again".into());
        }
        for (key, seen) in &review.objects {
            let now = self.objects.get(key).ok_or("Reviewed Docker object disappeared; review again")?;
            if seen.inspect != now.inspect || seen.references != now.references {
                return Err(format!("{} changed since review; review again", seen.id));
            }
        }
        Ok(())
    }
}

/// Only these read operations can be constructed. There is intentionally no arbitrary argv
/// runner and no removal method. An absolute executable is injected for isolated CLI tests.
pub struct DockerCli {
    executable: PathBuf,
    home: PathBuf,
}

impl DockerCli {
    pub fn new(executable: PathBuf, home: PathBuf) -> Result<Self, String> {
        if !executable.is_absolute() || !home.is_absolute() {
            return Err("Docker prototype requires absolute executable and home paths".into());
        }
        Ok(Self { executable, home })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.executable);
        for name in OVERRIDES {
            cmd.env_remove(name);
        }
        // Do not let a launching shell redirect the engine, configuration directory, API
        // version or a future Buildx adapter. Never mutate this process's environment in tests.
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if ["DOCKER_", "BUILDX_", "BUILDKIT_"].iter().any(|p| name_text.starts_with(p)) {
                cmd.env_remove(name);
            }
        }
        cmd.env("HOME", &self.home)
            .arg("--config")
            .arg(self.home.join(".docker"))
            .args(["--context", rules::DOCKER_CONTEXT])
            .args(args);
        cmd
    }

    pub fn inventory(&self) -> Result<Inventory, String> {
        self.inventory_with_cancel(&AtomicBool::new(false))
    }

    pub fn inventory_with_cancel(&self, cancel: &AtomicBool) -> Result<Inventory, String> {
        Collection { cli: self, cancel, deadline: Instant::now() + COLLECTION_TIMEOUT, bytes: Cell::new(0) }.inventory()
    }
}

struct Collection<'a> {
    cli: &'a DockerCli,
    cancel: &'a AtomicBool,
    deadline: Instant,
    bytes: Cell<usize>,
}

impl Collection<'_> {
    fn read(&self, args: &[&str]) -> Result<String, String> {
        let timeout = TIMEOUT.min(self.deadline.saturating_duration_since(Instant::now()));
        if timeout.is_zero() || self.cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Docker inventory cancelled or exceeded its time limit".into());
        }
        let out = sys::run_controlled(self.cli.command(args), timeout, self.cancel, MAX_OUTPUT)
            .ok_or("Docker inventory could not be completed (cancelled, timed out or output limit exceeded)")?;
        if !out.ok {
            // Don't return raw inspect/configuration data or shell environment values in errors.
            return Err(format!("Docker {} failed; inventory is incomplete", args[0]));
        }
        let bytes = self.bytes.get() + out.stdout.len();
        if bytes > MAX_INVENTORY_BYTES {
            return Err("Docker inventory exceeded its metadata limit".into());
        }
        self.bytes.set(bytes);
        Ok(out.stdout)
    }

    fn engine(&self) -> Result<Engine, String> {
        let contexts: Vec<Value> = parse(&self.read(&["context", "inspect", rules::DOCKER_CONTEXT])?)?;
        let [context] = contexts.as_slice() else { return Err("Expected one Docker Desktop context".into()) };
        if text(context, "Name")? != rules::DOCKER_CONTEXT {
            return Err("Unexpected Docker context".into());
        }
        let endpoint =
            context.pointer("/Endpoints/docker/Host").and_then(Value::as_str).ok_or("Missing Docker endpoint")?;
        // Start with the known macOS Desktop socket only. Do not assume that a context named
        // desktop-linux actually points at Desktop; other layouts need explicit validation.
        let expected = format!("unix://{}", self.cli.home.join(".docker/run/docker.sock").display());
        if endpoint != expected {
            return Err("Docker context is not the expected local Desktop socket".into());
        }
        let id: String = parse(&self.read(&["info", "--format", "{{json .ID}}"])?)?;
        if id.trim().is_empty() {
            return Err("Missing Docker engine ID".into());
        }
        Ok(Engine { context: rules::DOCKER_CONTEXT.into(), endpoint: endpoint.into(), id })
    }

    fn ids(&self, kind: Kind) -> Result<BTreeSet<String>, String> {
        let resource = match kind {
            Kind::Container => "container",
            Kind::Image => "image",
        };
        let output = self.read(&[resource, "ls", "--all", "--quiet", "--no-trunc"])?;
        let mut ids = BTreeSet::new();
        for id in output.lines() {
            if !valid_id(kind, id) {
                return Err("Docker returned an invalid or truncated object ID".into());
            }
            // Multiple image tags can produce duplicate IDs: inspect the content only once.
            ids.insert(id.to_owned());
            if ids.len() > MAX_OBJECTS {
                return Err("Docker inventory is too large for this prototype".into());
            }
        }
        Ok(ids)
    }

    fn inventory(&self) -> Result<Inventory, String> {
        let engine = self.engine()?;
        let mut objects = BTreeMap::new();
        let containers = self.ids(Kind::Container)?;
        let images = self.ids(Kind::Image)?;
        for (kind, ids) in [(Kind::Container, &containers), (Kind::Image, &images)] {
            for id in ids {
                let args = match kind {
                    Kind::Container => vec!["container", "inspect", "--size", id],
                    Kind::Image => vec!["image", "inspect", id],
                };
                let records: Vec<Value> = parse(&self.read(&args)?)?;
                let [record] = records.as_slice() else {
                    return Err("Expected exactly one Docker inspect record".into());
                };
                let object = object(kind, id, record.clone())?;
                objects.insert(object.key.clone(), object);
            }
        }
        let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for container in objects.values().filter(|o| o.kind == Kind::Container) {
            let image = text(&container.inspect, "Image")?;
            if !valid_id(Kind::Image, image) {
                return Err("Container has no complete image ID".into());
            }
            users.entry(image.into()).or_default().push(container.id.clone());
        }
        for image in objects.values_mut().filter(|o| o.kind == Kind::Image) {
            if let Some(containers) = users.get(&image.id) {
                image.references.extend(containers.iter().map(|id| format!("container:{id}")));
            }
            image.references.sort();
        }
        // Detect membership and endpoint changes during collection. This is not an atomic
        // snapshot and cannot detect a change-and-revert between reads; never authorise deletes.
        if containers != self.ids(Kind::Container)? || images != self.ids(Kind::Image)? || engine != self.engine()? {
            return Err("Docker changed while inventory was collected; review again".into());
        }
        Ok(Inventory { engine, objects, cache_status: "unavailable: no validated cache transport/builder identity" })
    }
}

fn object(kind: Kind, id: &str, inspect: Value) -> Result<Object, String> {
    if text(&inspect, "Id")? != id {
        return Err("Docker inspect returned a different object ID".into());
    }
    let created = text(&inspect, "Created")?.to_owned();
    let mut mounts = Vec::new();
    if kind == Kind::Container {
        for mount in inspect.get("Mounts").and_then(Value::as_array).ok_or("Missing container mounts")? {
            mounts.push(Mount {
                kind: text(mount, "Type")?.into(),
                name: mount.get("Name").and_then(Value::as_str).map(String::from),
                destination: text(mount, "Destination")?.into(),
            });
        }
    }
    let image = (kind == Kind::Container).then(|| text(&inspect, "Image").map(String::from)).transpose()?;
    let project =
        inspect.pointer("/Config/Labels/com.docker.compose.project").and_then(Value::as_str).map(String::from);
    let (label, bytes, references, state, hold) = match kind {
        Kind::Container => {
            let state = inspect.get("State").ok_or("Missing container state")?;
            let status = text(state, "Status")?.to_owned();
            if !state.get("Running").is_some_and(Value::is_boolean) {
                return Err("Missing container running state".into());
            }
            // These fields must exist even for a stopped container with no mounts.
            inspect.get("Mounts").and_then(Value::as_array).ok_or("Missing container mounts")?;
            inspect.get("Config").and_then(Value::as_object).ok_or("Missing container configuration")?;
            (
                text(&inspect, "Name")?.trim_start_matches('/').to_owned(),
                inspect.get("SizeRw").and_then(Value::as_u64),
                Vec::new(),
                status,
                "container writable data has no atomic revision check",
            )
        }
        Kind::Image => {
            let mut refs = string_array(&inspect, "RepoTags")?;
            refs.extend(string_array(&inspect, "RepoDigests")?);
            refs.sort();
            refs.dedup();
            let label = refs.first().cloned().unwrap_or_else(|| id.to_owned());
            (
                label,
                inspect.get("Size").and_then(Value::as_u64),
                refs,
                "present".into(),
                "image references can change after reinspection",
            )
        }
    };
    let prefix = match kind {
        Kind::Container => "container",
        Kind::Image => "image",
    };
    Ok(Object {
        key: format!("docker:{prefix}:{id}"),
        id: id.into(),
        kind,
        label,
        bytes,
        references,
        state,
        hold: hold.into(),
        created,
        image,
        project,
        mounts,
        inspect,
    })
}

fn valid_id(kind: Kind, id: &str) -> bool {
    let hex = match kind {
        Kind::Container => Some(id),
        Kind::Image => id.strip_prefix("sha256:"),
    };
    hex.is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| format!("Missing Docker field {key}"))
}

fn string_array(v: &Value, key: &str) -> Result<Vec<String>, String> {
    // Docker serialises absent tags/digests as either null or []. A missing field is incomplete.
    match v.get(key) {
        Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => {
            values.iter().map(|v| v.as_str().map(String::from).ok_or("Invalid Docker reference".into())).collect()
        }
        _ => Err(format!("Missing Docker field {key}")),
    }
}

fn parse<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(|_| "Invalid Docker JSON; inventory is incomplete".into())
}

/// Raw Engine `/system/df` build-cache record adapter, tested with fixtures only. CLI summary
/// formatters are lossy and Buildx may address remote/multi-node builders, so neither is silently
/// substituted here. No cache collection command or deletion is enabled.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CacheRecord {
    #[serde(rename = "ID")]
    pub id: String,
    pub r#type: String,
    pub description: String,
    pub in_use: bool,
    pub shared: bool,
    pub size: u64,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub usage_count: u64,
    /// Preserve version-specific fields (e.g. parent relationships), rather than silently
    /// treating records with changed unknown metadata as unchanged.
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}

impl CacheRecord {
    pub fn parse_engine_records(json: &str) -> Result<Vec<Self>, String> {
        let records: Vec<Self> = parse(json)?;
        let mut ids = BTreeSet::new();
        for record in &records {
            if record.id.is_empty() || !ids.insert(&record.id) {
                return Err("Empty or duplicate Docker cache ID".into());
            }
        }
        Ok(records)
    }

    /// Cache metadata equality is useful for detecting changed usage, but doesn't prevent a
    /// subsequent build or GC race. An unchanged record is still held from removal.
    pub fn unchanged_since(&self, seen: &Self) -> bool {
        self == seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_pins_context_config_and_removes_overrides() {
        let cli = DockerCli::new("/mock/docker".into(), "/mock/home".into()).unwrap();
        let cmd = cli.command(&["info"]);
        let args: Vec<_> = cmd.get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, ["--config", "/mock/home/.docker", "--context", "desktop-linux", "info"]);
        for name in OVERRIDES {
            assert!(cmd.get_envs().any(|(k, v)| k == *name && v.is_none()), "{name}");
        }
    }
}
