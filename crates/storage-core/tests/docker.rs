//! All CLI calls go to this test's absolute fake executable, never PATH or a real Docker engine.
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use storage_core::docker::{CacheRecord, DockerCli, Inventory, Kind};

const FAKE: &str = include_str!("fixtures/docker-cli.sh");

struct Mock {
    root: PathBuf,
}

impl Mock {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "storage-docker-prototype-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mock = Self { root };
        mock.write("docker", FAKE);
        fs::set_permissions(mock.root.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
        mock.json("context.json", json!([{"Name":"desktop-linux", "Endpoints":{"docker":{"Host":format!("unix://{}/.docker/run/docker.sock", mock.root.display())}}}]));
        mock.json("engine.json", json!("desktop-engine-1"));
        mock.write("containers", "");
        mock.write("images", "");
        mock
    }

    fn cli(&self) -> DockerCli {
        DockerCli::new(self.root.join("docker"), self.root.clone()).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.root.join(name), text).unwrap();
    }

    fn json(&self, name: &str, value: Value) {
        self.write(name, &serde_json::to_string(&value).unwrap());
    }

    fn image(&self, id: &str, tag: Option<&str>) {
        self.json(
            &format!("{id}.json"),
            json!([{
                "Id":id, "Created":"2026-11-13T12:00:00Z", "Size":1024,
                "RepoTags":tag.map(|t| vec![t]), "RepoDigests":[],
                "RootFS":{"Type":"layers","Layers":[format!("sha256:{}", "f".repeat(64))]}
            }]),
        );
    }

    fn container(&self, id: &str, image: &str, state: &str) {
        self.json(&format!("{id}.json"), container(id, image, state));
    }

    fn fixture(&self) -> (String, String, Inventory) {
        let c = "a".repeat(64);
        let i = format!("sha256:{}", "b".repeat(64));
        self.write("containers", &c);
        self.write("images", &format!("{i}\n{i}\n")); // two tags/list rows, one content object
        self.container(&c, &i, "exited");
        self.image(&i, Some("example:latest"));
        let inventory = self.cli().inventory().unwrap();
        (c, i, inventory)
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn container(id: &str, image: &str, status: &str) -> Value {
    json!([{
        "Id":id, "Name":"/review-me", "Image":image, "Created":"2026-11-13T12:00:00Z",
        "SizeRw":512, "State":{"Status":status,"Running":status == "running","StartedAt":"2026-11-13T12:01:00Z","FinishedAt":"2026-11-13T12:02:00Z"},
        "Config":{"Image":"example:latest","Labels":{"com.docker.compose.project":"test"}},
        "Mounts":[{"Type":"volume","Name":"keep-data","Destination":"/data"}]
    }])
}

#[test]
fn inventory_has_full_ids_distinct_kinds_sizes_and_references_without_double_counting_tags() {
    let mock = Mock::new();
    let (c, i, inventory) = mock.fixture();
    let objects: Vec<_> = inventory.objects().collect();
    assert_eq!(objects.len(), 2);
    let image = objects.iter().find(|o| o.kind == Kind::Image).unwrap();
    assert_eq!(image.id, i);
    assert_eq!(image.bytes, Some(1024));
    assert_eq!(image.references, [format!("container:{c}"), "example:latest".into()]);
    let container = objects.iter().find(|o| o.kind == Kind::Container).unwrap();
    assert_eq!(container.bytes, Some(512));
    assert_eq!(container.label, "review-me");
    assert!(objects.iter().all(|o| !o.hold.is_empty()));
    assert!(inventory.cache_status.starts_with("unavailable"));
    let serialised = serde_json::to_value(&inventory).unwrap();
    assert!(!serialised.to_string().contains("Mounts"), "private inspect metadata isn't webview data");
    let calls = fs::read_to_string(mock.root.join("calls")).unwrap();
    assert_eq!(calls.lines().filter(|l| l.starts_with("image inspect")).count(), 1);
    assert!(!calls.contains("prune") && !calls.contains(" rm"));
}

#[test]
fn unknown_duplicate_and_empty_selections_are_refused() {
    let mock = Mock::new();
    let (_, _, inventory) = mock.fixture();
    let key = inventory.objects().next().unwrap().key.clone();
    assert!(inventory.review(&[]).is_err());
    assert!(inventory.review(&["docker:image:unseen".into()]).is_err());
    assert!(inventory.review(&[key.clone(), key]).is_err());
}

#[test]
fn new_objects_never_expand_a_review_and_cannot_be_selected_from_it() {
    let mock = Mock::new();
    let (_, _, seen) = mock.fixture();
    let keys: Vec<_> = seen.objects().map(|o| o.key.clone()).collect();
    let review = seen.review(&keys).unwrap();
    let new_id = format!("sha256:{}", "c".repeat(64));
    mock.image(&new_id, None);
    let old_id = seen.objects().find(|o| o.kind == Kind::Image).unwrap().id.clone();
    mock.write("images", &format!("{old_id}\n{new_id}"));
    let now = mock.cli().inventory().unwrap();
    now.compare_review(&review).unwrap();
    assert!(seen.review(&[format!("docker:image:{new_id}")]).is_err());
    assert_eq!(keys.len(), 2);
    assert_eq!(now.objects().count(), 3);
}

#[test]
fn new_container_reference_to_reviewed_image_is_refused_even_if_stopped() {
    let mock = Mock::new();
    let (c, i, seen) = mock.fixture();
    let review = seen.review(&[format!("docker:image:{i}")]).unwrap();
    let other = "d".repeat(64);
    mock.container(&other, &i, "exited");
    mock.write("containers", &format!("{c}\n{other}"));
    assert!(mock.cli().inventory().unwrap().compare_review(&review).unwrap_err().contains("changed"));
}

#[test]
fn changed_state_restart_history_mounts_and_unknown_fields_require_review_again() {
    let mock = Mock::new();
    let (c, i, seen) = mock.fixture();
    let review = seen.review(&[format!("docker:container:{c}")]).unwrap();
    let mut cases = Vec::new();
    cases.push(container(&c, &i, "running"));
    let mut restarted = container(&c, &i, "exited");
    restarted[0]["State"]["StartedAt"] = json!("2026-11-15T12:00:00Z");
    cases.push(restarted);
    let mut mounts = container(&c, &i, "exited");
    mounts[0]["Mounts"] = json!([]);
    cases.push(mounts);
    let mut unknown = container(&c, &i, "exited");
    unknown[0]["FutureField"] = json!("new state");
    cases.push(unknown);
    for value in cases {
        mock.json(&format!("{c}.json"), value);
        assert!(mock.cli().inventory().unwrap().compare_review(&review).is_err());
    }
}

#[test]
fn tag_changes_disappearance_and_engine_replacement_refuse_the_review() {
    let mock = Mock::new();
    let (_, i, seen) = mock.fixture();
    let review = seen.review(&[format!("docker:image:{i}")]).unwrap();
    mock.image(&i, Some("new:tag"));
    assert!(mock.cli().inventory().unwrap().compare_review(&review).is_err());
    mock.image(&i, Some("example:latest"));
    mock.write("images", "");
    assert!(mock.cli().inventory().unwrap().compare_review(&review).unwrap_err().contains("disappeared"));
    mock.write("images", &i);
    mock.json("engine.json", json!("replacement-engine"));
    assert!(mock.cli().inventory().unwrap().compare_review(&review).unwrap_err().contains("engine changed"));
}

#[test]
fn incomplete_inspect_wrong_identity_short_ids_and_cli_failure_are_not_empty_success() {
    let mock = Mock::new();
    let (_, i, _) = mock.fixture();
    for record in ["not json", "[]", "[{}]"] {
        mock.write(&format!("{i}.json"), record);
        assert!(mock.cli().inventory().is_err());
    }
    let other = format!("sha256:{}", "e".repeat(64));
    mock.image(&other, None);
    fs::copy(mock.root.join(format!("{other}.json")), mock.root.join(format!("{i}.json"))).unwrap();
    assert!(mock.cli().inventory().unwrap_err().contains("different object ID"));
    mock.write("images", "123abc");
    assert!(mock.cli().inventory().unwrap_err().contains("truncated"));
    mock.write("images", &i);
    fs::remove_file(mock.root.join(format!("{i}.json"))).unwrap();
    assert!(mock.cli().inventory().unwrap_err().contains("incomplete"));
}

#[test]
fn context_name_does_not_authorise_a_remote_or_other_local_engine() {
    let mock = Mock::new();
    for endpoint in ["tcp://remote:2375", "ssh://remote", "unix:///var/run/other.sock"] {
        mock.json("context.json", json!([{"Name":"desktop-linux","Endpoints":{"docker":{"Host":endpoint}}}]));
        assert!(mock.cli().inventory().unwrap_err().contains("expected local Desktop socket"));
    }
    let calls = fs::read_to_string(mock.root.join("calls")).unwrap();
    assert!(calls.lines().all(|l| l == "context inspect desktop-linux"), "no engine request after an invalid context");
}

#[test]
fn membership_changes_during_inventory_are_detected_without_timing_sleeps() {
    let mock = Mock::new();
    mock.fixture();
    mock.write("change-membership", "");
    assert!(mock.cli().inventory().unwrap_err().contains("changed while inventory"));
}

#[test]
fn unchanged_review_is_only_a_comparison_and_all_removal_holds_remain() {
    let mock = Mock::new();
    let (_, _, seen) = mock.fixture();
    let review = seen.review(&seen.objects().map(|o| o.key.clone()).collect::<Vec<_>>()).unwrap();
    let now = mock.cli().inventory().unwrap();
    now.compare_review(&review).unwrap();
    assert!(now.objects().all(|o| !o.hold.is_empty()));
    let calls = fs::read_to_string(mock.root.join("calls")).unwrap();
    assert!(!calls.lines().any(|l| l.contains("prune") || l.contains(" rm") || l.contains("remove")));
}

#[test]
fn cache_fixture_tracks_usage_and_shared_state_but_has_no_removal_path() {
    // Raw Engine /system/df BuildCache shape, not formatted system-df or Buildx text.
    let record = json!({"ID":"cache-record-1","Type":"regular","Description":"RUN cargo build",
        "InUse":false,"Shared":true,"Size":4096,"CreatedAt":"2026-11-13T12:00:00Z",
        "LastUsedAt":"2026-11-13T12:01:00Z","UsageCount":2});
    let seen = CacheRecord::parse_engine_records(&json!([record.clone()]).to_string()).unwrap().remove(0);
    assert!(seen.unchanged_since(&seen));
    for (field, value) in [
        ("InUse", json!(true)),
        ("Shared", json!(false)),
        ("UsageCount", json!(3)),
        ("LastUsedAt", json!("2026-11-15T12:00:00Z")),
        ("Size", json!(8192)),
    ] {
        let mut changed = record.clone();
        changed[field] = value;
        let now = CacheRecord::parse_engine_records(&json!([changed]).to_string()).unwrap().remove(0);
        assert!(!now.unchanged_since(&seen), "{field}");
    }
    assert!(CacheRecord::parse_engine_records(&json!([record.clone(), record]).to_string()).is_err());
    assert!(CacheRecord::parse_engine_records("[{}]").is_err());
    assert!(CacheRecord::parse_engine_records("[{\"ID\":\"partial\"}]").is_err());
}
