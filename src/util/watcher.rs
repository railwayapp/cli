use anyhow::Result;
use futures::Future;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub struct FileWatcher {
    path: PathBuf,
}

impl FileWatcher {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn watch<F, Fut>(&self, handler: F) -> Result<()>
    where
        F: Fn(CancellationToken, Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let (watch_tx, mut file_change) = tokio::sync::mpsc::unbounded_channel();
        let _watcher = self.setup_file_watcher(watch_tx)?;
        let mut current_cancel_token: Option<CancellationToken> = None;
        loop {
            tokio::select! {
                event = file_change.recv() => {
                    if let Some(Ok(event)) = event {
                        if let Some(token) = current_cancel_token.take() {
                            token.cancel();
                        }
                        current_cancel_token = Some(CancellationToken::new());
                        handler(current_cancel_token.clone().unwrap(), event).await?;
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    if let Some(token) = current_cancel_token.take() {
                        token.cancel();
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    fn setup_file_watcher(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<Result<notify::Event, notify::Error>>,
    ) -> Result<notify::FsEventWatcher> {
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                if let Err(e) = tx.send(res) {
                    eprintln!("Failed to send file event: {e}");
                }
            },
            Config::default(),
        )?;
        watcher.watch(&self.path, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    }
}
