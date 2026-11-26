pub mod display_ffi;
pub mod fan;
pub mod macos;

use std::sync::Mutex;
use std::time::{Duration, Instant};

struct ProbeCache<T>(Mutex<Option<(Instant, T)>>);

impl<T> Default for ProbeCache<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T: Clone> ProbeCache<T> {
    fn sample(&self, ttl: Duration, fresh: bool, probe: impl FnOnce() -> T) -> T {
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !fresh {
            if let Some((at, value)) = cache.as_ref() {
                if at.elapsed() < ttl {
                    return value.clone();
                }
            }
        }
        let value = probe();
        *cache = Some((Instant::now(), value.clone()));
        value
    }
}
