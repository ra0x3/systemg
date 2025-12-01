use std::sync::{Mutex, OnceLock};

/// Global lock for environment variable modifications in tests.
/// All tests that modify environment variables (especially HOME) should acquire this lock
/// to prevent race conditions between parallel test executions.
pub static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
