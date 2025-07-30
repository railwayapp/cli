use anyhow::Result;
use futures::Future;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::time::Duration;
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
        let mut debounce_timer: Option<tokio::time::Instant> = None;
        let debounce_duration = Duration::from_millis(100);

        loop {
            tokio::select! {
                event = file_change.recv() => {
                    if let Some(Ok(event)) = event {
                        // Filter for meaningful events (ignore temp files, focus on final writes)
                        if self.should_handle_event(&event) {
                            debounce_timer = Some(tokio::time::Instant::now() + debounce_duration);
                        }
                    }
                }
                _ = async {
                    if let Some(timer) = debounce_timer {
                        tokio::time::sleep_until(timer).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if debounce_timer.is_some() => {
                    debounce_timer = None;
                    if let Some(token) = current_cancel_token.take() {
                        token.cancel();
                    }
                    current_cancel_token = Some(CancellationToken::new());
                    // Create a dummy event for the handler
                    let dummy_event = Event::new(EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)));
                    handler(current_cancel_token.clone().unwrap(), dummy_event).await?;
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

    fn should_handle_event(&self, event: &Event) -> bool {
        // Filter out temporary files and focus on actual content changes
        match &event.kind {
            EventKind::Create(_) | EventKind::Remove(_) => {
                // Only care about creates/removes of the actual target file
                event.paths.iter().any(|p| p == &self.path)
            }
            EventKind::Modify(_) => true,
            _ => false,
        }
    }

    fn setup_file_watcher(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<Result<notify::Event, notify::Error>>,
    ) -> Result<notify::RecommendedWatcher> {
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
