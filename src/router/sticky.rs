use dashmap::DashMap;
use std::time::Instant;

/// Sticky session entry
struct StickyEntry {
    endpoint_index: usize,
    last_access: Instant,
}

/// Session-sticky affinity store
pub struct SessionStickyStore {
    sessions: DashMap<String, StickyEntry>,
    ttl_secs: u64,
}

impl SessionStickyStore {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            sessions: DashMap::new(),
            ttl_secs,
        }
    }

    /// Get endpoint index for a session (if still alive)
    pub fn get(&self, session_id: &str) -> Option<usize> {
        if let Some(entry) = self.sessions.get(session_id) {
            if entry.last_access.elapsed().as_secs() < self.ttl_secs {
                return Some(entry.endpoint_index);
            }
            // TTL expired — remove
            drop(entry);
            self.sessions.remove(session_id);
        }
        None
    }

    /// Bind a session to a specific endpoint
    pub fn set(&self, session_id: &str, endpoint_index: usize) {
        self.sessions.insert(
            session_id.to_string(),
            StickyEntry {
                endpoint_index,
                last_access: Instant::now(),
            },
        );
    }

    /// Extend session TTL (touch)
    pub fn touch(&self, session_id: &str) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.last_access = Instant::now();
        }
    }

    /// Clean up expired sessions
    #[allow(dead_code)]
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.sessions.retain(|_, v| {
            now.duration_since(v.last_access).as_secs() < self.ttl_secs
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sticky_set_and_get() {
        let store = SessionStickyStore::new(60);
        store.set("session-1", 3);
        assert_eq!(store.get("session-1"), Some(3));
    }

    #[test]
    fn test_sticky_unknown_session() {
        let store = SessionStickyStore::new(60);
        assert_eq!(store.get("nonexistent"), None);
    }

    #[test]
    fn test_sticky_touch_extends() {
        let store = SessionStickyStore::new(1); // 1 second TTL
        store.set("session-1", 0);
        std::thread::sleep(std::time::Duration::from_millis(500));
        store.touch("session-1");               // reset TTL
        std::thread::sleep(std::time::Duration::from_millis(700));
        // Without touch it would be expired (~1200ms > 1000ms TTL),
        // but touch reset at 500ms, so it's only ~700ms old
        assert_eq!(store.get("session-1"), Some(0));
    }

    #[test]
    fn test_sticky_expires() {
        let store = SessionStickyStore::new(1); // 1 second TTL
        store.set("session-1", 0);
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(store.get("session-1"), None);
    }

    #[test]
    fn test_sticky_cleanup_removes_expired() {
        let store = SessionStickyStore::new(1);
        store.set("s1", 0);
        store.set("s2", 1);
        std::thread::sleep(std::time::Duration::from_secs(2));
        store.cleanup();
        assert_eq!(store.get("s1"), None);
        assert_eq!(store.get("s2"), None);
    }
}
