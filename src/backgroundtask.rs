use std::{future::Future, mem::replace};

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

/// Wrapper for tokio tasks with cancelation and shutdown helpers.
/// 
/// Automatically triggers shutdown token if dropped before calling
/// `finish()`.
pub struct BackgroundTask<T: Send + 'static> {
    shutdown_token: CancellationToken,
    shutdown_dropguard: Option<DropGuard>,
    join_handle: Option<JoinHandle<T>>,
}

impl<T: Send + 'static> BackgroundTask<T> {
    /// Spawn a background task.
    pub fn spawn<F: FnOnce(CancellationToken) -> Fut, Fut: Future<Output = T> + Send + 'static>(func: F) -> Self {
        let shutdown_token = CancellationToken::new();
        let shutdown_dropguard = shutdown_token.clone().drop_guard();
        let join_handle = tokio::spawn(func(shutdown_token.clone()));

        Self {
            shutdown_token,
            shutdown_dropguard: Some(shutdown_dropguard),
            join_handle: Some(join_handle),
        }
    }

    /// Attempt to spawn a background task.
    pub fn try_spawn<F: FnOnce(CancellationToken) -> Result<Fut, E>, Fut: Future<Output = T> + Send + 'static, E>(func: F) -> Result<Self, E> {
        let shutdown_token = CancellationToken::new();
        let shutdown_dropguard = shutdown_token.clone().drop_guard();

        match func(shutdown_token.clone()) {
            Ok(future) => {
                let join_handle = tokio::spawn(future);

                Ok(Self {
                    shutdown_token,
                    shutdown_dropguard: Some(shutdown_dropguard),
                    join_handle: Some(join_handle),
                })
            },
            Err(e) => Err(e)
        }
    }

    /// Shutdown the task (if still running), wait for completion, and return the result.
    pub async fn finish(&mut self) -> Result<T, anyhow::Error> {
        self.shutdown_token.cancel();
        let shutdown_dropguard = replace(&mut self.shutdown_dropguard, None);
        if let Some(shutdown_dropguard) = shutdown_dropguard {
            shutdown_dropguard.disarm();
        }
        let join_handle = replace(&mut self.join_handle, None);
        if let Some(join_handle) = join_handle {
            Ok(join_handle.await?)
        } else {
            anyhow::bail!("task already finished");
        }
    }
}
