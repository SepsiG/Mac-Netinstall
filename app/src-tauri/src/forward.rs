//! Engine events → the window, across engine replacements.

use ni_engine::{EngineEvent, EngineHandle};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

/// Tauri event carrying an [`EngineEvent`] (the engine's JSON).
pub const ENGINE_EVENT: &str = "engine-event";
/// Tauri event telling the window it missed events (payload: how many;
/// 0 when the engine task was replaced): it should reload the snapshot.
pub const ENGINE_LAGGED: &str = "engine-lagged";
/// Tauri event asking the window to confirm quitting while devices are
/// being prepared or flashed (payload: how many).
pub const CLOSE_REQUESTED: &str = "close-requested";

/// What the forwarder passes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forwarded {
    Event(EngineEvent),
    Lagged(u64),
}

/// Pass every event of the current engine task to `emit`, following
/// replacements; returns when the engine watch is closed.
pub async fn forward_events(
    mut engines: watch::Receiver<EngineHandle>,
    emit: impl Fn(Forwarded) + Send,
) {
    loop {
        let mut events = engines.borrow_and_update().subscribe();
        loop {
            tokio::select! {
                r = events.recv() => match r {
                    Ok(e) => emit(Forwarded::Event(e)),
                    Err(RecvError::Lagged(n)) => emit(Forwarded::Lagged(n)),
                    Err(RecvError::Closed) => {
                        // This task ended; wait for its replacement.
                        if engines.changed().await.is_err() {
                            return;
                        }
                        emit(Forwarded::Lagged(0));
                        break;
                    }
                },
                changed = engines.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    emit(Forwarded::Lagged(0));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_assets::testutil::tempdir;
    use ni_assets::HttpConfig;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn replacements_are_followed_and_announced() {
        let dir = tempdir("forward");
        let spawn = |n: &str| EngineHandle::spawn(dir.join(n), HttpConfig::default()).unwrap();
        let (engines, rx) = watch::channel(spawn("a"));
        let (tx, mut got) = mpsc::unbounded_channel();
        let task = tokio::spawn(forward_events(rx, move |f| {
            let _ = tx.send(f);
        }));
        tokio::task::yield_now().await;
        engines.send_replace(spawn("b"));
        let first = tokio::time::timeout(Duration::from_secs(5), got.recv()).await;
        assert_eq!(first.unwrap(), Some(Forwarded::Lagged(0)));
        drop(engines);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the forwarder ends with the watch")
            .unwrap();
    }
}
