use crate::{
    config,
    paperless::{Paperless, TaskState},
    queue::{self, Phase, Queue},
};
use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

const SETTLE: Duration = Duration::from_secs(2);
const SCAN: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(30);
type Observations = HashMap<PathBuf, (u64, SystemTime, Instant)>;

pub fn retry_failed(path: &Path) -> Result<()> {
    let config = config::load_or_create(path)?;
    config.validate(path)?;
    let mut queue = Queue::open(
        path,
        config.base_url()?.to_string(),
        fs::canonicalize(&config.folder)?,
    )?;
    println!("Queued {} failed jobs for retry", queue.retry_failed()?);
    Ok(())
}
pub fn run(path: &Path, running: Arc<AtomicBool>) -> Result<()> {
    let config = config::load_or_create(path)?;
    config.validate(path)?;
    let folder = fs::canonicalize(&config.folder)?;
    let client = Paperless::new(&config)?;
    let mut queue = Queue::open(path, config.base_url()?.to_string(), folder.clone())?;
    let (tx, rx) = mpsc::sync_channel(1);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => {
                let _ = tx.try_send(());
            }
            Err(error) => eprintln!("Watcher error; periodic rescans remain active: {error}"),
            _ => {}
        })?;
    let mode = if config.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher
        .watch(&folder, mode)
        .context("Starting folder watcher")?;
    eprintln!(
        "Watching {} ({} saved jobs)",
        folder.display(),
        queue.jobs().len()
    );
    let mut observations = Observations::new();
    let mut next_scan = Instant::now();
    let mut attempts = HashMap::new();
    while running.load(Ordering::Relaxed) {
        if next_scan <= Instant::now() {
            discover(&folder, config.recursive, &mut queue, &mut observations)?;
            next_scan = Instant::now() + SCAN;
        }
        let mut index = 0;
        while index < queue.jobs().len() && running.load(Ordering::Relaxed) {
            let id = queue.jobs()[index].id;
            if attempts.get(&id).is_some_and(|time| *time > Instant::now()) {
                index += 1;
                continue;
            }
            // All journal mutations propagate errors: never continue after losing task state.
            match queue.jobs()[index].phase {
                Phase::Queued => {
                    anyhow::ensure!(
                        queue::hash(&queue.snapshot(id))? == queue.jobs()[index].digest,
                        "Queued snapshot {id} was modified; refusing upload and local cleanup"
                    );
                    match client.upload(&queue.snapshot(id), &queue.jobs()[index].filename) {
                        Ok(task_id) => {
                            queue.phase(index, Phase::Polling { task_id })?;
                            eprintln!(
                                "Uploaded {}: task {task_id}",
                                queue.jobs()[index].source.display()
                            );
                            attempts.insert(id, Instant::now() + POLL);
                        }
                        Err(error) => {
                            let permanent = error
                                .downcast_ref::<reqwest::Error>()
                                .and_then(|e| e.status())
                                .is_some_and(|s| {
                                    s.is_client_error() && s.as_u16() != 408 && s.as_u16() != 429
                                });
                            eprintln!("Upload {id} failed; local file retained: {error}");
                            if permanent {
                                queue.phase(
                                    index,
                                    Phase::Failed {
                                        reason: error.to_string(),
                                    },
                                )?;
                            }
                            attempts.insert(id, Instant::now() + RETRY);
                        }
                    }
                }
                Phase::Polling { task_id } => {
                    match client.poll(task_id) {
                        Ok(TaskState::Success) => {
                            queue.phase(index, Phase::Complete { task_id })?;
                            eprintln!("Task {task_id} succeeded; local cleanup ready");
                            attempts.remove(&id);
                            continue;
                        }
                        Ok(TaskState::Failure) => {
                            queue.phase(index, Phase::Failed { reason: format!("Paperless task {task_id} failed; inspect Paperless task details") })?;
                            eprintln!("Task {task_id} failed; retaining local file and snapshot");
                        }
                        Ok(TaskState::Pending) => {}
                        Err(error) => eprintln!(
                            "Task {task_id} query failed; will retry, local file retained: {error}"
                        ),
                    }
                    attempts.insert(id, Instant::now() + POLL);
                }
                Phase::Complete { .. } => match queue::delete_confirmed(&queue.jobs()[index]) {
                    Ok(()) => {
                        eprintln!("Completed {}", queue.jobs()[index].source.display());
                        observations.remove(&queue.jobs()[index].source);
                        queue.remove(index)?;
                        attempts.remove(&id);
                        continue;
                    }
                    Err(error) => {
                        eprintln!("Cleanup {id} deferred: {error}");
                        attempts.insert(id, Instant::now() + RETRY);
                    }
                },
                Phase::Failed { .. } => {}
            }
            index += 1;
        }
        // Notifications accelerate discovery; periodic scans recover missed events/startup files.
        if rx.recv_timeout(Duration::from_millis(200)).is_ok() {
            next_scan = Instant::now();
        }
    }
    eprintln!("Stopped; pending jobs saved for the next run");
    Ok(())
}
fn discover(
    folder: &Path,
    recursive: bool,
    queue: &mut Queue,
    observations: &mut Observations,
) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    let walk = walkdir::WalkDir::new(folder).follow_links(false);
    let walk = if recursive { walk } else { walk.max_depth(1) };
    for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("Scan error: {error}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.')
            || name.ends_with(".part")
            || name.ends_with(".tmp")
            || name.ends_with(".crdownload")
        {
            continue;
        }
        let source = entry.path().to_path_buf();
        if queue.jobs().iter().any(|job| job.source == source) {
            continue;
        }
        seen.insert(source.clone());
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.len() == 0 {
            continue;
        }
        let modified = metadata.modified()?;
        let observation = observations.entry(source.clone()).or_insert((
            metadata.len(),
            modified,
            Instant::now(),
        ));
        if observation.0 != metadata.len() || observation.1 != modified {
            *observation = (metadata.len(), modified, Instant::now());
            continue;
        }
        if observation.2.elapsed() < SETTLE {
            continue;
        }
        // Stop on queue preparation/persistence errors; never silently lose work.
        if queue
            .enqueue(&source)
            .with_context(|| format!("Queueing {}", source.display()))?
        {
            eprintln!("Queued {}", source.display());
        }
    }
    observations.retain(|source, _| seen.contains(source));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_only_includes_subfolders_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("inbox");
        fs::create_dir_all(folder.join("nested")).unwrap();
        let top = folder.join("top.pdf");
        let nested = folder.join("nested/child.pdf");
        fs::write(&top, b"top level").unwrap();
        fs::write(&nested, b"nested document").unwrap();
        let mut queue = Queue::open(
            &dir.path().join("config.json"),
            "https://example.com/".into(),
            folder.clone(),
        )
        .unwrap();
        let settled = |path: &Path| {
            let metadata = fs::metadata(path).unwrap();
            (
                metadata.len(),
                metadata.modified().unwrap(),
                Instant::now() - SETTLE,
            )
        };
        let mut observations = Observations::from([(top.clone(), settled(&top))]);
        discover(&folder, false, &mut queue, &mut observations).unwrap();
        assert_eq!(queue.jobs().len(), 1);
        assert_eq!(queue.jobs()[0].source, top);

        observations.insert(nested.clone(), settled(&nested));
        discover(&folder, true, &mut queue, &mut observations).unwrap();
        assert_eq!(queue.jobs().len(), 2);
        assert_eq!(queue.jobs()[1].source, nested);
    }
}
