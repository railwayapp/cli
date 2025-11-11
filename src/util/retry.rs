use anyhow::Result;
use std::future::Future;
use std::time::Duration;
use tokio::time::sleep;

pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
    pub on_retry: Option<Box<dyn Fn(u32, u32, &anyhow::Error, u64) + Send + Sync>>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_delay_ms: 500, // 500ms for fast first retry
            max_delay_ms: 10000,   // 10 seconds max
            backoff_multiplier: 2.0,
            on_retry: None, // Silent by default
        }
    }
}

pub async fn retry_with_backoff<F, Fut, T>(config: RetryConfig, mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut delay_ms = config.initial_delay_ms;

    for attempt in 1..=config.max_attempts {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) if attempt == config.max_attempts => {
                return Err(e);
            }
            Err(e) => {
                if let Some(ref on_retry) = config.on_retry {
                    on_retry(attempt, config.max_attempts, &e, delay_ms);
                }

                sleep(Duration::from_millis(delay_ms)).await;

                // Calculate next delay with exponential backoff, capped at max_delay_ms
                delay_ms =
                    ((delay_ms as f64 * config.backoff_multiplier) as u64).min(config.max_delay_ms);
            }
        }
    }

    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_succeeds_after_retries() {
        let count = Arc::new(AtomicU32::new(0));
        let count_clone = count.clone();

        let result = retry_with_backoff(
            RetryConfig {
                max_attempts: 3,
                initial_delay_ms: 1,
                ..Default::default()
            },
            move || {
                let c = count_clone.clone();
                async move {
                    let attempt = c.fetch_add(1, Ordering::SeqCst) + 1;
                    if attempt < 3 {
                        anyhow::bail!("Not ready yet");
                    }
                    Ok::<i32, anyhow::Error>(42)
                }
            },
        )
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_fails_after_max_attempts() {
        let count = Arc::new(AtomicU32::new(0));
        let count_clone = count.clone();

        let result = retry_with_backoff(
            RetryConfig {
                max_attempts: 2,
                initial_delay_ms: 1,
                ..Default::default()
            },
            move || {
                let c = count_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("Always fails");
                    #[allow(unreachable_code)]
                    Ok::<i32, anyhow::Error>(0)
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_succeeds_first_try() {
        let result = retry_with_backoff(RetryConfig::default(), || async {
            Ok::<i32, anyhow::Error>(100)
        })
        .await;

        assert_eq!(result.unwrap(), 100);
    }

    #[tokio::test]
    async fn test_custom_logger() {
        let log_count = Arc::new(AtomicU32::new(0));
        let log_count_clone = log_count.clone();

        let result = retry_with_backoff(
            RetryConfig {
                max_attempts: 2,
                initial_delay_ms: 1,
                on_retry: Some(Box::new(move |attempt, max_attempts, _error, delay_ms| {
                    log_count_clone.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(attempt, 1);
                    assert_eq!(max_attempts, 2);
                    assert_eq!(delay_ms, 1);
                })),
                ..Default::default()
            },
            || async {
                anyhow::bail!("Always fails");
                #[allow(unreachable_code)]
                Ok::<i32, anyhow::Error>(0)
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(log_count.load(Ordering::SeqCst), 1); // Called once for the retry
    }
}
