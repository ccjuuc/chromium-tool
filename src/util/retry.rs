use std::future::Future;
use anyhow::Result;

pub async fn retry_async<F, Fut, T>(f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut last_err = None;
    for _ in 0..3 {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Retry failed")))
}

