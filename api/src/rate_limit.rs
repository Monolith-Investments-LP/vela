use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    requests: Arc<DashMap<String, Vec<Instant>>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window_secs: u64) -> Self {
        Self {
            requests: Arc::new(DashMap::new()),
            max_requests,
            window: Duration::from_secs(window_secs),
        }
    }

    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entry = self.requests.entry(key.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < self.window);
        if entry.is_empty() {
            // All prior timestamps expired — evict the slot to bound map size,
            // then fall through to insert the current request as a fresh entry.
            drop(entry);
            self.requests.remove(key);
            self.requests.entry(key.to_string()).or_default().push(now);
            return true;
        }
        if entry.len() >= self.max_requests {
            return false;
        }
        entry.push(now);
        true
    }
}
