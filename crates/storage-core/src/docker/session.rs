//! In-memory, single-use review capabilities. These can only compare; they cannot clean.
use super::{Engine, Inventory, Object, Review};
use serde::Serialize;
use std::io::Read;
use std::time::{Duration, Instant};

const SNAPSHOT_LIFETIME: Duration = Duration::from_secs(10 * 60);
const REVIEW_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotView {
    pub snapshot_id: String,
    pub collected_at: i64,
    pub engine: Engine,
    pub objects: Vec<Object>,
    pub cache_status: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewView {
    pub token: String,
    pub snapshot_id: String,
    pub expires_at: i64,
    pub objects: Vec<Object>,
    pub cleanup_enabled: bool,
}

struct Snapshot {
    id: String,
    inventory: Inventory,
    expires: Instant,
}

struct PendingReview {
    token: String,
    review: Review,
    expires: Instant,
}

#[derive(Default)]
pub struct Session {
    snapshot: Option<Snapshot>,
    pending: Option<PendingReview>,
}

impl Session {
    pub fn invalidate(&mut self) {
        self.pending = None;
        self.snapshot = None;
    }

    pub fn install(&mut self, inventory: Inventory) -> Result<SnapshotView, String> {
        self.install_at(inventory, Instant::now())
    }

    fn install_at(&mut self, inventory: Inventory, now: Instant) -> Result<SnapshotView, String> {
        self.invalidate();
        let id = nonce()?;
        let view = SnapshotView {
            snapshot_id: id.clone(),
            collected_at: crate::sys::now(),
            engine: inventory.engine().clone(),
            objects: inventory.objects().cloned().collect(),
            cache_status: inventory.cache_status.into(),
        };
        self.snapshot = Some(Snapshot { id, inventory, expires: now + SNAPSHOT_LIFETIME });
        Ok(view)
    }

    pub fn prepare(&mut self, snapshot_id: &str, keys: &[String]) -> Result<ReviewView, String> {
        self.prepare_at(snapshot_id, keys, Instant::now())
    }

    fn prepare_at(&mut self, snapshot_id: &str, keys: &[String], now: Instant) -> Result<ReviewView, String> {
        // A new preparation attempt supersedes any older confirmation, including invalid attempts.
        self.pending = None;
        let snapshot = self.snapshot.as_ref().ok_or("Load the Docker inventory first")?;
        if snapshot.id != snapshot_id || now >= snapshot.expires {
            return Err("Docker inventory expired or was replaced; load it again".into());
        }
        let review = snapshot.inventory.review(keys)?;
        let expires = snapshot.expires.min(now + REVIEW_LIFETIME);
        let token = nonce()?;
        let view = ReviewView {
            token: token.clone(),
            snapshot_id: snapshot.id.clone(),
            expires_at: crate::sys::now() + expires.duration_since(now).as_secs() as i64,
            objects: review.objects.values().cloned().collect(),
            cleanup_enabled: false,
        };
        self.pending = Some(PendingReview { token, review, expires });
        Ok(view)
    }

    /// Consume BEFORE collecting current state, so failure, cancellation and retries never reuse
    /// a confirmation. No IDs or keys from a second IPC request can alter the stored selection.
    pub fn take(&mut self, token: &str) -> Result<Review, String> {
        self.take_at(token, Instant::now())
    }

    fn take_at(&mut self, token: &str, now: Instant) -> Result<Review, String> {
        let pending = self.pending.as_ref().ok_or("Docker review is no longer available; review again")?;
        if pending.token != token {
            return Err("Invalid Docker review token".into());
        }
        let pending = self.pending.take().unwrap();
        if now >= pending.expires {
            return Err("Docker review expired; review again".into());
        }
        Ok(pending.review)
    }

    pub fn discard(&mut self, token: &str) {
        if self.pending.as_ref().is_some_and(|p| p.token == token) {
            self.pending = None;
        }
    }
}

fn nonce() -> Result<String, String> {
    let mut bytes = [0; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "Could not create a Docker review token")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::super::{Kind, object};
    use super::*;
    use serde_json::json;

    fn inventory() -> Inventory {
        let id = format!("sha256:{}", "a".repeat(64));
        let object =
            object(Kind::Image, &id, json!({"Id":id,"Created":"today","RepoTags":[],"RepoDigests":[],"Size":10}))
                .unwrap();
        Inventory {
            engine: Engine {
                context: "desktop-linux".into(),
                endpoint: "unix:///mock/docker.sock".into(),
                id: "test".into(),
            },
            objects: [(object.key.clone(), object)].into_iter().collect(),
            cache_status: "unavailable",
        }
    }

    fn prepare(session: &mut Session, now: Instant) -> ReviewView {
        let snapshot = session.install_at(inventory(), now).unwrap();
        session.prepare_at(&snapshot.snapshot_id, &[snapshot.objects[0].key.clone()], now).unwrap()
    }

    #[test]
    fn token_is_single_use_and_cannot_change_the_selected_targets() {
        let now = Instant::now();
        let mut session = Session::default();
        let view = prepare(&mut session, now);
        assert_eq!(view.token.len(), 64);
        assert!(!view.cleanup_enabled);
        assert!(session.take_at("forged", now).is_err());
        inventory().compare_review(&session.take_at(&view.token, now).unwrap()).unwrap();
        assert!(session.take_at(&view.token, now).is_err());
    }

    #[test]
    fn expiry_uses_monotonic_time_and_consumes_expired_review() {
        let now = Instant::now();
        let mut session = Session::default();
        let view = prepare(&mut session, now);
        assert!(session.take_at(&view.token, now + REVIEW_LIFETIME).unwrap_err().contains("expired"));
        assert!(session.take_at(&view.token, now).is_err());
        let snapshot = session.install_at(inventory(), now).unwrap();
        assert!(
            session
                .prepare_at(&snapshot.snapshot_id, &[snapshot.objects[0].key.clone()], now + SNAPSHOT_LIFETIME)
                .is_err()
        );
    }

    #[test]
    fn refresh_scan_cancel_restart_and_replacement_invalidate_reviews() {
        let now = Instant::now();
        let mut session = Session::default();
        let old = prepare(&mut session, now);
        session.invalidate(); // invoked before scan/cleanup/refresh, even if the operation fails
        assert!(session.take_at(&old.token, now).is_err());
        assert!(session.prepare_at(&old.snapshot_id, &[old.objects[0].key.clone()], now).is_err());
        let replacement = prepare(&mut session, now);
        assert_ne!(replacement.token, old.token);
        assert_ne!(replacement.snapshot_id, old.snapshot_id);
        assert!(Session::default().take(&replacement.token).is_err());
    }

    #[test]
    fn new_preparation_and_dismissal_revoke_only_the_relevant_review() {
        let now = Instant::now();
        let mut session = Session::default();
        let first = prepare(&mut session, now);
        let second = session.prepare_at(&first.snapshot_id, &[first.objects[0].key.clone()], now).unwrap();
        assert!(session.take_at(&first.token, now).is_err());
        session.discard(&first.token);
        assert!(session.take_at(&second.token, now).is_ok());
        let third = prepare(&mut session, now);
        session.discard(&third.token);
        assert!(session.take_at(&third.token, now).is_err());
    }

    #[test]
    fn unknown_duplicate_and_stale_keys_never_issue_a_review() {
        let now = Instant::now();
        let mut session = Session::default();
        let view = prepare(&mut session, now);
        let key = view.objects[0].key.clone();
        for keys in [vec![], vec!["unknown".into()], vec![key.clone(), key]] {
            assert!(session.prepare_at(&view.snapshot_id, &keys, now).is_err());
        }
        assert!(session.take_at(&view.token, now).is_err());
        assert!(session.prepare_at("other snapshot", &[view.objects[0].key.clone()], now).is_err());
    }
}
