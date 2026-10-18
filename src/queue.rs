use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub source: PathBuf,
    pub filename: String,
    pub digest: String,
    pub phase: Phase,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Polling { task_id: Uuid },
    Complete { task_id: Uuid },
    Failed { reason: String },
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    endpoint: String,
    folder: PathBuf,
    jobs: Vec<Job>,
}
pub struct Queue {
    dir: PathBuf,
    journal: Journal,
    _lock: File,
}
impl Queue {
    pub fn open(config_path: &Path, endpoint: String, folder: PathBuf) -> Result<Self> {
        let dir = config_path.with_file_name(format!(
            "{}.queue",
            config_path
                .file_name()
                .context("Missing config filename")?
                .to_string_lossy()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&dir)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(dir.join("lock"))?;
        lock.try_lock()
            .context("Another ngxd process is using this config queue")?;
        let path = dir.join("jobs.json");
        let journal = match fs::read(&path) {
            Ok(data) => {
                let journal: Journal = serde_json::from_slice(&data)
                    .context("Invalid queue journal; refusing to discard it")?;
                ensure!(journal.version == 1, "Unsupported queue journal version");
                if !journal.jobs.is_empty() {
                    ensure!(
                        journal.endpoint == endpoint && journal.folder == folder,
                        "Queued jobs belong to another endpoint/folder; use a separate config for the new destination"
                    );
                }
                Journal {
                    endpoint,
                    folder,
                    ..journal
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal {
                version: 1,
                endpoint,
                folder,
                jobs: vec![],
            },
            Err(error) => return Err(error.into()),
        };
        let queue = Self {
            dir,
            journal,
            _lock: lock,
        };
        queue.save()?;
        Ok(queue)
    }
    pub fn jobs(&self) -> &[Job] {
        &self.journal.jobs
    }
    pub fn snapshot(&self, id: Uuid) -> PathBuf {
        self.dir.join(format!("{id}.document"))
    }
    fn save(&self) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)?;
        serde_json::to_writer_pretty(&mut file, &self.journal)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(self.dir.join("jobs.json"))?;
        sync_dir(&self.dir)
    }
    pub fn enqueue(&mut self, source: &Path) -> Result<bool> {
        let Some(mut original) = open_source(source)? else {
            return Ok(false);
        };
        let before = original.metadata()?;
        let filename = source
            .file_name()
            .and_then(|s| s.to_str())
            .context("Document filename must be UTF-8")?
            .to_owned();
        let id = Uuid::new_v4();
        let mut snapshot = tempfile::NamedTempFile::new_in(&self.dir)?;
        std::io::copy(&mut original, &mut snapshot)?;
        snapshot.as_file().sync_all()?;
        let digest = hash(snapshot.path())?;
        let Some(current) = open_source(source)? else {
            return Ok(false);
        };
        let after = current.metadata()?;
        if before.len() != after.len()
            || before.modified()? != after.modified()?
            || digest != hash_reader(current)?
        {
            return Ok(false);
        }
        snapshot.persist(self.snapshot(id))?;
        sync_dir(&self.dir)?;
        self.journal.jobs.push(Job {
            id,
            source: source.into(),
            filename,
            digest,
            phase: Phase::Queued,
        });
        self.save()?;
        Ok(true)
    }
    pub fn phase(&mut self, index: usize, phase: Phase) -> Result<()> {
        self.journal.jobs[index].phase = phase;
        self.save()
    }
    pub fn remove(&mut self, index: usize) -> Result<()> {
        let id = self.journal.jobs.remove(index).id;
        self.save()?;
        match fs::remove_file(self.snapshot(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("Cannot remove completed snapshot {id}: {error}"),
        }
        Ok(())
    }
    pub fn retry_failed(&mut self) -> Result<usize> {
        let mut count = 0;
        for job in &mut self.journal.jobs {
            if matches!(job.phase, Phase::Failed { .. }) {
                job.phase = Phase::Queued;
                count += 1;
            }
        }
        self.save()?;
        Ok(count)
    }
}
// Source races and Windows sharing violations should not stop unrelated jobs.
fn open_source(path: &Path) -> Result<Option<File>> {
    let result = (|| {
        if !fs::symlink_metadata(path)?.file_type().is_file() {
            return Ok(None);
        }
        File::open(path).map(Some)
    })();
    match result {
        Ok(file) => Ok(file),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) || (cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))) =>
        {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!("Cannot read {}; will retry: {error}", path.display());
            }
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}
pub fn hash(path: &Path) -> Result<String> {
    hash_reader(File::open(path)?)
}
fn hash_reader(mut file: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let size = file.read(&mut buf)?;
        if size == 0 {
            break;
        }
        digest.update(&buf[..size]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> Result<()> {
    Ok(())
}

/// Claim the pathname first, then compare the exact file that will be removed.
/// A replacement created at the original pathname is never removed by cleanup.
pub fn delete_confirmed(job: &Job) -> Result<()> {
    ensure!(
        matches!(job.phase, Phase::Complete { .. }),
        "Refusing cleanup without confirmed success"
    );
    let claimed = job
        .source
        .with_file_name(format!(".ngxd-{}-cleanup", job.id));
    if !claimed.try_exists()? {
        match fs::symlink_metadata(&job.source) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(metadata) => ensure!(
                metadata.file_type().is_file(),
                "Source became a symlink or non-file; retaining it"
            ),
        }
        fs::rename(&job.source, &claimed).context("Claiming confirmed file for cleanup")?;
    }
    let regular = fs::symlink_metadata(&claimed)?.file_type().is_file();
    if !regular || hash(&claimed)? != job.digest {
        // hard_link is a no-clobber restore: never overwrite a newly created source.
        if regular && fs::hard_link(&claimed, &job.source).is_ok() {
            fs::remove_file(&claimed)?;
            eprintln!(
                "Source changed since upload; restored {} for a new upload",
                job.source.display()
            );
            return Ok(());
        }
        anyhow::bail!(
            "Source changed; retained {}. Restore it manually; no local data deleted",
            claimed.display()
        );
    }
    fs::remove_file(&claimed).context("Removing confirmed local document")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(source: PathBuf, digest: String) -> Job {
        Job {
            id: Uuid::new_v4(),
            source,
            filename: "a.pdf".into(),
            digest,
            phase: Phase::Complete {
                task_id: Uuid::new_v4(),
            },
        }
    }
    #[test]
    fn missing_source_is_deferred_without_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        let mut queue =
            Queue::open(&config, "https://example.com/".into(), dir.path().into()).unwrap();
        assert!(!queue.enqueue(&dir.path().join("gone.pdf")).unwrap());
        assert!(queue.jobs().is_empty());
    }
    #[test]
    fn queue_persists_tasks_and_rejects_a_different_destination() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        let source = dir.path().join("a.pdf");
        fs::write(&source, b"document").unwrap();
        let task_id = Uuid::new_v4();
        {
            let mut queue =
                Queue::open(&config, "https://example.com/".into(), dir.path().into()).unwrap();
            assert!(queue.enqueue(&source).unwrap());
            queue.phase(0, Phase::Polling { task_id }).unwrap();
        }
        assert!(Queue::open(&config, "https://other.example/".into(), dir.path().into()).is_err());
        let queue = Queue::open(&config, "https://example.com/".into(), dir.path().into()).unwrap();
        assert!(
            matches!(queue.jobs()[0].phase, Phase::Polling { task_id: saved } if saved == task_id)
        );
        assert_eq!(
            fs::read(queue.snapshot(queue.jobs()[0].id)).unwrap(),
            b"document"
        );
    }
    #[test]
    fn cleanup_preserves_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        fs::write(&path, b"original").unwrap();
        let job = job(path.clone(), hash(&path).unwrap());
        fs::write(&path, b"replacement").unwrap();
        delete_confirmed(&job).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
    }
    #[test]
    fn cleanup_requires_success_and_supports_restart_after_claim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        fs::write(&path, b"original").unwrap();
        let mut job = job(path.clone(), hash(&path).unwrap());
        job.phase = Phase::Queued;
        assert!(delete_confirmed(&job).is_err());
        job.phase = Phase::Complete {
            task_id: Uuid::new_v4(),
        };
        let claimed = path.with_file_name(format!(".ngxd-{}-cleanup", job.id));
        fs::rename(&path, &claimed).unwrap();
        fs::write(&path, b"new document").unwrap();
        delete_confirmed(&job).unwrap();
        assert!(!claimed.exists());
        assert_eq!(fs::read(&path).unwrap(), b"new document");
    }
}
