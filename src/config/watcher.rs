use anyhow::Result;
use arc_swap::ArcSwap;
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::types::AppConfig;

/// Start watching the config file for changes.
/// Returns `Arc<ArcSwap<AppConfig>>` for lock-free reading of the current config.
pub async fn watch_config(config_path: PathBuf) -> Result<Arc<ArcSwap<AppConfig>>> {
    let initial = super::loader::load_config(&config_path)?;
    let config: Arc<ArcSwap<AppConfig>> = Arc::new(ArcSwap::from_pointee(initial));

    let config_clone = config.clone();
    let watcher_path = config_path.clone();
    let reloader_path = config_path.clone();

    let (tx, mut rx) = mpsc::channel::<()>(32);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let relevant = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Data(_)) | EventKind::Create(_)
                );
                if relevant
                    && event.paths.iter().any(|p| {
                        p.ends_with(&watcher_path)
                            || p.ends_with(watcher_path.file_name().unwrap_or_default())
                    })
                {
                    let _ = tx.blocking_send(());
                }
            }
        },
        notify::Config::default(),
    )?;

    watcher.watch(
        config_path.parent().unwrap_or(Path::new(".")),
        RecursiveMode::NonRecursive,
    )?;

    // Background task: reload config when the file changes
    tokio::spawn(async move {
        // watcher must live as long as this task
        let _watcher = watcher;

        while rx.recv().await.is_some() {
            // Small delay — let the OS finish writing the file
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            match super::loader::load_config(&reloader_path) {
                Ok(new_config) => {
                    tracing::info!(
                        models = new_config.models.len(),
                        tokens = new_config.tokens.len(),
                        teams = new_config.teams.len(),
                        "Config reloaded successfully"
                    );
                    config_clone.store(Arc::new(new_config));
                }
                Err(e) => {
                    tracing::error!(error = %e, path = %reloader_path.display(), "Failed to reload config — keeping previous version");
                }
            }
        }
    });

    Ok(config)
}
