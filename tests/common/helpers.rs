//! Async test helpers and macros
//!
//! Provides polling-based wait utilities similar to libtransmission's waitFor()

use std::future::Future;
use std::time::{Duration, Instant};

/// Poll a condition until it becomes true or timeout is reached
///
/// Similar to libtransmission's waitFor() function
///
/// # Arguments
/// * `condition` - Closure that returns true when condition is met
/// * `timeout_ms` - Maximum time to wait in milliseconds
///
/// # Returns
/// `true` if condition was met, `false` if timeout occurred
///
/// # Example
///
/// ```rust
/// use common::helpers::wait_for;
/// use std::sync::atomic::{AtomicUsize, Ordering};
///
/// let counter = AtomicUsize::new(0);
/// let result = wait_for(|| counter.load(Ordering::Relaxed) > 5, 1000);
/// ```
pub fn wait_for<F>(condition: F, timeout_ms: u64) -> bool
where
    F: Fn() -> bool,
{
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    
    loop {
        if condition() {
            return true;
        }
        
        if Instant::now() > deadline {
            return false;
        }
        
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Async version of wait_for for use in async tests
///
/// # Example
///
/// ```rust
/// use common::helpers::wait_for_async;
/// use std::sync::atomic::{AtomicUsize, Ordering};
/// use std::sync::Arc;
///
/// # tokio_test::block_on(async {
/// let counter = Arc::new(AtomicUsize::new(0));
/// let result = wait_for_async(
///     || counter.load(Ordering::Relaxed) > 5,
///     1000
/// ).await;
/// # });
/// ```
pub async fn wait_for_async<F>(condition: F, timeout_ms: u64) -> bool
where
    F: Fn() -> bool,
{
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    
    loop {
        if condition() {
            return true;
        }
        
        if Instant::now() > deadline {
            return false;
        }
        
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait for a condition with async polling
///
/// This version accepts an async condition closure
pub async fn wait_for_async_fn<F, Fut>(condition: F, timeout_ms: u64) -> bool
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    
    loop {
        if condition().await {
            return true;
        }
        
        if Instant::now() > deadline {
            return false;
        }
        
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Placeholder for wait_for_torrent_state
/// Will be implemented once Session and Torrent types are properly imported
pub async fn wait_for_torrent_state(
    _session: &(),
    _torrent_id: (), 
    _target_state: (),
    _timeout: Duration,
) -> bool {
    // TODO: Implement once session types are available
    false
}

/// Macro version of wait_for for convenience
///
/// # Example
///
/// ```rust
/// use common::wait_for;
///
/// let mut value = 0;
/// wait_for!(value > 10, 1000);
/// ```
#[macro_export]
macro_rules! wait_for {
    ($condition:expr, $timeout_ms:expr) => {{
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis($timeout_ms);
        let mut result = false;
        
        loop {
            if $condition {
                result = true;
                break;
            }
            
            if std::time::Instant::now() > deadline {
                break;
            }
            
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        
        result
    }};
}

/// Async version of wait_for! macro
#[macro_export]
macro_rules! wait_for_async {
    ($condition:expr, $timeout_ms:expr) => {{
        async {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis($timeout_ms);
            let mut result = false;
            
            loop {
                if $condition {
                    result = true;
                    break;
                }
                
                if std::time::Instant::now() > deadline {
                    break;
                }
                
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            
            result
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn wait_for_returns_true_when_condition_met() {
        let counter = AtomicUsize::new(5);
        let result = wait_for(|| counter.load(Ordering::Relaxed) >= 5, 100);
        assert!(result);
    }

    #[test]
    fn wait_for_returns_false_on_timeout() {
        let counter = AtomicUsize::new(0);
        let result = wait_for(|| counter.load(Ordering::Relaxed) > 100, 50);
        assert!(!result);
    }

    #[tokio::test]
    async fn wait_for_async_works() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        
        // Spawn a task to increment counter
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            counter_clone.fetch_add(10, Ordering::Relaxed);
        });
        
        let result = wait_for_async(
            || counter.load(Ordering::Relaxed) >= 10,
            500
        ).await;
        
        assert!(result);
    }

    #[test]
    fn wait_for_macro_works() {
        let mut value = 0;
        // Simulate some work
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
        });
        value = 5;
        
        let result = wait_for!(value == 5, 100);
        assert!(result);
    }
}
