mod config;
mod mover;
mod scanner;
mod syncthing;

use anyhow::Result;
use config::AppConfig;
use mover::{Job, MoveOptions, Outcome, WitnessMissing};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, SystemTime};
use syncthing::{Client, Event, FolderMap, ItemFinishedData};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

/// Pause before re-checking a missing mount witness.
const WITNESS_RETRY: Duration = Duration::from_secs(60);
/// A file still in the source after this long is reported on every scan.
const STALE_WARNING: Duration = Duration::from_secs(24 * 3600);
/// Next scan delay while Syncthing's folder list is unavailable (e.g. at startup).
const FOLDER_RETRY: Duration = Duration::from_secs(10);
/// Consecutive failures of one file before it is reported as an error instead of a warning.
const FAILURES_BEFORE_ERROR: u32 = 3;

type Mover = Arc<dyn Fn(&Job) -> Result<Outcome> + Send + Sync>;

// --- Shared state (in memory only: the source folder is the durable queue) ---

#[derive(Default)]
struct QueueState {
    items: VecDeque<Job>,
    queued: HashSet<PathBuf>,
    in_progress: Option<Job>,
    /// The in-progress file was announced again (e.g. a newer version arrived): queue it again
    /// once the current attempt ends, whatever its outcome.
    rerun: bool,
}

struct Shared {
    queue: Mutex<QueueState>,
    /// Wakes the worker when a job is queued (or on shutdown).
    work: Notify,
    /// Asks the scan task for a full rescan.
    rescan: Notify,
    folders: RwLock<FolderMap>,
    shutdown: AtomicBool,
}

impl Shared {
    fn new(folders: FolderMap) -> Self {
        Self {
            queue: Mutex::new(QueueState::default()),
            work: Notify::new(),
            rescan: Notify::new(),
            folders: RwLock::new(folders),
            shutdown: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a job found by a scan, unless that source is already queued or in progress. Never
    /// blocks on I/O, so producers keep up while the worker copies a large file.
    fn push(&self, job: Job) -> bool {
        self.push_inner(job, false)
    }

    /// Queues a job announced by Syncthing. If that file is being processed, a newer version may
    /// have arrived: it is queued again when the current attempt ends. (Scans do not do this, so
    /// a failing file is only retried on the next scan, never in a loop.)
    fn push_event(&self, job: Job) -> bool {
        self.push_inner(job, true)
    }

    fn push_inner(&self, job: Job, rerun_if_in_progress: bool) -> bool {
        let mut q = self.lock();
        if q.in_progress.as_ref().is_some_and(|j| j.src == job.src) {
            q.rerun |= rerun_if_in_progress;
            return false;
        }
        if !q.queued.insert(job.src.clone()) {
            return false;
        }
        q.items.push_back(job);
        drop(q);
        self.work.notify_one();
        true
    }

    fn push_front(&self, job: Job) {
        let mut q = self.lock();
        if q.queued.insert(job.src.clone()) {
            q.items.push_front(job);
        }
    }

    fn pop(&self) -> Option<Job> {
        let mut q = self.lock();
        let job = q.items.pop_front()?;
        q.queued.remove(&job.src);
        q.in_progress = Some(job.clone());
        q.rerun = false;
        Some(job)
    }

    fn done(&self) {
        let mut q = self.lock();
        let job = q.in_progress.take();
        if std::mem::take(&mut q.rerun)
            && let Some(job) = job
            && q.queued.insert(job.src.clone())
        {
            q.items.push_back(job);
        }
    }

    fn len(&self) -> usize {
        self.lock().items.len()
    }

    fn folders(&self) -> FolderMap {
        self.folders
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_folders(&self, map: FolderMap) {
        *self.folders.write().unwrap_or_else(|e| e.into_inner()) = map;
    }

    fn stop(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.work.notify_one();
        self.rescan.notify_one();
    }

    fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

// --- Worker: the only consumer, so HDD access is strictly sequential ---

async fn run_worker(shared: Arc<Shared>, mover: Mover) {
    let mut succeeded_since_drain = 0usize;
    let mut failures: HashMap<PathBuf, u32> = HashMap::new();

    while !shared.stopping() {
        let Some(job) = shared.pop() else {
            // Drain-then-rescan (decision 0003): catches files whose events were lost during the
            // burst. Only after real progress, so a failing file cannot cause a hot loop.
            if succeeded_since_drain > 0 {
                succeeded_since_drain = 0;
                shared.rescan.notify_one();
            }
            shared.work.notified().await;
            continue;
        };

        info!("Processing {:?} ({} more queued)", job.src, shared.len());
        let m = mover.clone();
        let j = job.clone();
        let result = tokio::task::spawn_blocking(move || m(&j)).await;
        shared.done();

        match result {
            Err(e) => error!("Mover panicked on {:?}: {}", job.src, e),
            Ok(Ok(outcome)) => {
                failures.remove(&job.src);
                match outcome {
                    Outcome::Moved(to) => {
                        info!("Moved {:?} -> {:?}", job.src, to);
                        succeeded_since_drain += 1;
                    }
                    Outcome::Duplicate(at) => {
                        info!(
                            "Identical copy already at {:?}; removed source {:?}",
                            at, job.src
                        );
                        succeeded_since_drain += 1;
                    }
                    Outcome::Gone => debug!("{:?} already gone", job.src),
                    Outcome::WouldMove(to) => {
                        info!("[dry run] would move {:?} -> {:?}", job.src, to)
                    }
                    Outcome::WouldDeduplicate(at) => {
                        info!(
                            "[dry run] would remove {:?} (identical to {:?})",
                            job.src, at
                        )
                    }
                }
            }
            Ok(Err(e)) if e.downcast_ref::<WitnessMissing>().is_some() => {
                error!("{:#}. Pausing for {}s", e, WITNESS_RETRY.as_secs());
                shared.push_front(job);
                tokio::select! {
                    _ = tokio::time::sleep(WITNESS_RETRY) => {}
                    _ = shared.work.notified() => {} // shutdown or new work: re-check now
                }
            }
            Ok(Err(e)) => {
                let n = failures.entry(job.src.clone()).or_default();
                *n += 1;
                if *n >= FAILURES_BEFORE_ERROR {
                    error!(
                        "Failed {} times on {:?}: {:#}. Kept in source; retried on next scan",
                        n, job.src, e
                    );
                } else {
                    warn!(
                        "Failed on {:?}: {:#}. Kept in source; retried on next scan",
                        job.src, e
                    );
                }
            }
        }
    }
    info!("Worker stopped");
}

// --- Event poller: memory only, never touches the disk ---

/// Turns a Syncthing event into a job, if it is a successfully received file in a mapped folder.
fn job_from_event(event: &Event, folders: &FolderMap, cfg: &AppConfig) -> Option<Job> {
    if event.event_type != "ItemFinished" {
        return None;
    }
    let data: ItemFinishedData = match serde_json::from_value(event.data.clone()?) {
        Ok(d) => d,
        Err(e) => {
            warn!("Unparsable ItemFinished event {}: {}", event.id, e);
            return None;
        }
    };
    if data.item_type != "file" || data.action != "update" || data.error.is_some() {
        return None;
    }
    let Some(rel) = folders.get(&data.folder) else {
        if folders.is_empty() {
            warn!(
                "No folder mapping yet; {:?} will be picked up by the next scan",
                data.item
            );
        } else {
            debug!("Ignoring item from unmapped folder {}", data.folder);
        }
        return None;
    };
    let item = Path::new(&data.item);
    if !item.components().all(|c| matches!(c, Component::Normal(_))) {
        warn!(
            "Ignoring suspicious item path {:?} in folder {}",
            data.item, data.folder
        );
        return None;
    }
    Some(Job {
        src: cfg.src_dir.join(rel).join(item),
        dst: cfg.dst_dir.join(rel).join(item),
    })
}

async fn run_events(client: Arc<Client>, cfg: Arc<AppConfig>, shared: Arc<Shared>) {
    let mut since = 0u64;
    let mut start_time = client.start_time().await.ok();
    let mut backoff = Duration::from_secs(1);

    while !shared.stopping() {
        match client.events(since).await {
            Ok(events) => {
                backoff = Duration::from_secs(1);
                // Checked on every response: after a restart our cursor may be ahead of the new
                // IDs (empty polls) or new IDs may already have passed it (silent gap).
                if check_restart(&client, &mut start_time, &mut since, &shared).await {
                    continue;
                }
                if events.is_empty() {
                    continue;
                }
                if since > 0 && events[0].id > since + 1 {
                    warn!(
                        "Missed {} Syncthing event(s); rescanning",
                        events[0].id - since - 1
                    );
                    shared.rescan.notify_one();
                }
                let folders = shared.folders();
                for event in &events {
                    since = event.id;
                    if event.event_type == "ConfigSaved" {
                        info!("Syncthing configuration changed; refreshing folders");
                        shared.rescan.notify_one();
                    } else if let Some(job) = job_from_event(event, &folders, &cfg)
                        && shared.push_event(job.clone())
                    {
                        info!("Queued {:?} ({} pending)", job.src, shared.len());
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Syncthing events: {:#}. Retrying in {}s",
                    e,
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
                check_restart(&client, &mut start_time, &mut since, &shared).await;
            }
        }
    }
}

/// Event IDs restart from 1 when Syncthing restarts (decision 0002): reset the cursor and rescan.
/// Returns true when a restart was detected (the current batch of events must be discarded).
async fn check_restart(
    client: &Client,
    start_time: &mut Option<String>,
    since: &mut u64,
    shared: &Shared,
) -> bool {
    let Ok(now) = client.start_time().await else {
        return false;
    };
    let restarted = start_time.as_ref().is_some_and(|prev| *prev != now);
    if restarted {
        info!("Syncthing restarted; resetting event cursor and rescanning");
        *since = 0;
        shared.rescan.notify_one();
    }
    *start_time = Some(now);
    restarted
}

// --- Scanner: rebuilds the queue from the source folders ---

async fn run_scans(client: Arc<Client>, cfg: Arc<AppConfig>, shared: Arc<Shared>) {
    let mut next = Duration::ZERO; // scan at startup
    loop {
        tokio::select! {
            _ = tokio::time::sleep(next) => {}
            _ = shared.rescan.notified() => {}
        }
        if shared.stopping() {
            return;
        }

        match client.folders(&cfg).await {
            Ok(map) => {
                if map != shared.folders() {
                    info!("Folder mapping: {:?}", map);
                    shared.set_folders(map);
                }
            }
            Err(e) => warn!(
                "Cannot refresh folders, keeping the previous mapping: {:#}",
                e
            ),
        }
        let folders = shared.folders();
        if folders.is_empty() {
            warn!(
                "No Syncthing folder is mapped; check SYNCTHING_STORAGE_DIRECTORY and FOLDER_IDS"
            );
        }

        let (src, dst, min_age) = (cfg.src_dir.clone(), cfg.dst_dir.clone(), cfg.min_age);
        let scan = tokio::task::spawn_blocking(move || {
            scanner::scan(&src, &dst, &folders, min_age, SystemTime::now())
        })
        .await;
        // Retry soon only while no mapping is known (e.g. Syncthing not up yet at startup).
        next = if shared.folders().is_empty() {
            FOLDER_RETRY.min(cfg.scan_interval)
        } else {
            cfg.scan_interval
        };
        match scan {
            Err(e) => error!("Scan panicked: {}", e),
            Ok(r) => {
                let found = r.jobs.len();
                let added = r
                    .jobs
                    .into_iter()
                    .filter(|j| shared.push(j.clone()))
                    .count();
                info!(
                    "Scan: {} file(s) in source, {} newly queued, {} too recent, {} error(s), {} leftover(s)",
                    found, added, r.deferred, r.errors, r.leftovers
                );
                if let Some(oldest) = r.oldest.filter(|o| *o > STALE_WARNING) {
                    warn!(
                        "A file has been waiting in the source for {}h; check for repeated failures",
                        oldest.as_secs() / 3600
                    );
                }
                if r.deferred > 0 {
                    next = next.min(cfg.min_age.max(Duration::from_secs(5)));
                }
            }
        }
    }
}

// --- Main ---

async fn run() -> Result<()> {
    info!(
        "Starting Syncthing Backup Porter {}",
        env!("CARGO_PKG_VERSION")
    );
    let cfg = Arc::new(AppConfig::from_env()?);
    if cfg.dry_run {
        warn!("DRY_RUN: no file will be written or removed");
    }
    let witness = cfg.witness_file.as_ref().map(|w| cfg.dst_dir.join(w));
    match &witness {
        None => warn!("WITNESS_FILE not set: an unmounted destination would not be detected"),
        Some(w) if !w.exists() => error!(
            "Witness {:?} is missing; nothing will be moved until it appears",
            w
        ),
        Some(_) => {}
    }

    let client = Arc::new(Client::new(&cfg)?);
    let shared = Arc::new(Shared::new(FolderMap::new()));

    let opts = MoveOptions {
        dst_root: cfg.dst_dir.clone(),
        witness,
        verify: cfg.verify_copy,
        dry_run: cfg.dry_run,
        allow_link: true,
    };
    let mover: Mover = Arc::new(move |job: &Job| mover::move_file(job, &opts));

    tokio::spawn(run_scans(client.clone(), cfg.clone(), shared.clone()));
    tokio::spawn(run_events(client, cfg, shared.clone()));
    tokio::spawn(stop_on_signal(shared.clone()));

    // Returns after the file in progress is finished.
    run_worker(shared, mover).await;
    Ok(())
}

async fn stop_on_signal(shared: Arc<Shared>) {
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                error!("Cannot listen for SIGTERM: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = terminate => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    info!("Shutdown requested; finishing the current file");
    shared.stop();
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    if let Err(e) = run().await {
        error!("FATAL: {:?}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;

    fn cfg() -> AppConfig {
        AppConfig {
            st_url: String::new(),
            st_api_key: String::new(),
            src_dir: PathBuf::from("/st"),
            dst_dir: PathBuf::from("/hdd"),
            st_storage_dir: PathBuf::from("/"),
            witness_file: None,
            dry_run: false,
            folder_ids: None,
            scan_interval: Duration::from_secs(900),
            verify_copy: true,
            min_age: Duration::from_secs(30),
        }
    }

    fn item_event(item: &str, folder: &str, action: &str, error: serde_json::Value) -> Event {
        Event {
            id: 1,
            event_type: "ItemFinished".into(),
            data: Some(
                json!({"item": item, "folder": folder, "action": action, "type": "file", "error": error}),
            ),
        }
    }

    fn job(src: &str) -> Job {
        Job {
            src: PathBuf::from(src),
            dst: PathBuf::from(format!("/hdd{}", src)),
        }
    }

    #[test]
    fn event_to_job() {
        let folders = FolderMap::from([("photos".to_string(), PathBuf::from("phone/photos"))]);
        let c = cfg();
        let j = job_from_event(
            &item_event("DCIM/a.jpg", "photos", "update", json!(null)),
            &folders,
            &c,
        )
        .unwrap();
        assert_eq!(j.src, PathBuf::from("/st/phone/photos/DCIM/a.jpg"));
        assert_eq!(j.dst, PathBuf::from("/hdd/phone/photos/DCIM/a.jpg"));

        for ev in [
            item_event("a.jpg", "photos", "delete", json!(null)),
            item_event("a.jpg", "photos", "update", json!("pull failed")),
            item_event("a.jpg", "unknown", "update", json!(null)),
            item_event("../escape.jpg", "photos", "update", json!(null)),
            item_event("/abs.jpg", "photos", "update", json!(null)),
        ] {
            assert!(job_from_event(&ev, &folders, &c).is_none(), "{:?}", ev.data);
        }
    }

    #[test]
    fn queue_deduplicates() {
        let s = Shared::new(FolderMap::new());
        assert!(s.push(job("/a")));
        assert!(!s.push(job("/a")));
        assert_eq!(s.pop(), Some(job("/a")));
        assert!(!s.push(job("/a")), "in-progress file is not queued twice");
        s.done();
        assert_eq!(s.len(), 0, "a scan never re-queues the file in progress");

        assert!(s.push(job("/a")));
        assert_eq!(s.pop(), Some(job("/a")));
        assert!(!s.push_event(job("/a")));
        assert_eq!(s.len(), 0);
        s.done();
        let requeued = s.len();
        assert_eq!(
            requeued, 1,
            "announced by an event while in progress: re-queued"
        );
        assert_eq!(s.pop(), Some(job("/a")));
        s.done();
        assert_eq!(s.len(), 0, "no rerun requested this time");
    }

    /// The burst scenario (decision 0003): a 1 GB copy must not stop new files from being queued.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queue_grows_while_worker_blocked() {
        let shared = Arc::new(Shared::new(FolderMap::new()));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let processed = Arc::new(Mutex::new(Vec::new()));
        let p = processed.clone();
        let mover: Mover = Arc::new(move |j: &Job| {
            if j.src == Path::new("/big") {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            p.lock().unwrap().push(j.src.clone());
            Ok(Outcome::Moved(j.dst.clone()))
        });
        let worker = tokio::spawn(run_worker(shared.clone(), mover));

        shared.push(job("/big"));
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        for i in 0..100 {
            assert!(shared.push(job(&format!("/small{:03}", i))));
        }
        assert_eq!(
            shared.len(),
            100,
            "all small files queued while the big copy is blocked"
        );
        assert!(processed.lock().unwrap().is_empty());

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), shared.rescan.notified())
            .await
            .expect("drain must trigger a rescan");
        let done = processed.lock().unwrap().clone();
        assert_eq!(done.len(), 101);
        assert_eq!(done[0], PathBuf::from("/big"));
        assert_eq!(done[100], PathBuf::from("/small099"));

        shared.stop();
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn failures_do_not_trigger_rescan() {
        let shared = Arc::new(Shared::new(FolderMap::new()));
        let calls = Arc::new(Mutex::new(0));
        let c = calls.clone();
        let mover: Mover = Arc::new(move |_: &Job| {
            *c.lock().unwrap() += 1;
            anyhow::bail!("disk error")
        });
        let worker = tokio::spawn(run_worker(shared.clone(), mover));
        shared.push(job("/a"));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), shared.rescan.notified())
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 1);
        shared.stop();
        worker.await.unwrap();
    }
}
