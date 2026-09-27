//! Per-session resource lifecycle shared by browser-use and computer-use.
//!
//! This is a port of DSH's `SessionResources`, which is the part of its
//! browser-use subsystem worth copying rather than reinventing. It carries
//! three guarantees that are easy to get subtly wrong:
//!
//! * **Lazy acquisition.** A session that never calls a tool never starts a
//!   browser or a desktop driver. Nothing is spawned at session creation.
//! * **Serialized execution.** Operations on one session run in arrival order,
//!   so a click always observes the page state the previous call left behind.
//!   Two concurrent navigations racing on one page produce states nobody can
//!   reproduce.
//! * **Deterministic disposal.** Disposal aborts the session's cancellation
//!   token, releases the resource, and waits for in-flight work to settle, so
//!   a closed session cannot leave a subprocess or a driver behind.
//!
//! Resources belong to one live session activation. A resumed session acquires
//! a fresh resource; nothing carries over, which is why the registry is keyed
//! by a session id that the caller rotates rather than by durable state.
//!
//! [`SessionResourceRegistry::run`] is the main entry point: it acquires if
//! needed, holds the session's operation lock for the duration of the closure,
//! and therefore provides serialization as a side effect of normal use.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};

/// Lifecycle state of one session's resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceStatus {
    /// No resource acquired yet. A tool call will acquire one.
    Idle,
    /// Acquired and serving operations.
    Active,
    /// Disposal in progress; new operations are refused.
    Closing,
    /// Disposed.
    Closed,
    /// Acquisition or an operation failed.
    Failed,
}

impl ResourceStatus {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => ResourceStatus::Active,
            2 => ResourceStatus::Closing,
            3 => ResourceStatus::Closed,
            4 => ResourceStatus::Failed,
            _ => ResourceStatus::Idle,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            ResourceStatus::Idle => 0,
            ResourceStatus::Active => 1,
            ResourceStatus::Closing => 2,
            ResourceStatus::Closed => 3,
            ResourceStatus::Failed => 4,
        }
    }
}

/// Errors the registry itself raises, before and around the resource's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The registry is shutting down; no new work is admitted.
    Disposing,
    /// This session's resource is being closed.
    SessionClosing,
    /// The session had no entry and one was required.
    UnknownSession,
    /// An exclusive resource is already held by another session.
    ExclusiveConflict { holder: String },
    /// The caller's cancellation fired before the operation could run.
    Cancelled,
    /// The resource could not be acquired.
    Acquire(String),
    /// The resource could not be released during disposal.
    Release(String),
    /// The closure returned an error.
    Operation(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Disposing => write!(formatter, "registry is disposing"),
            RegistryError::SessionClosing => write!(formatter, "session resource is closing"),
            RegistryError::UnknownSession => write!(formatter, "unknown session"),
            RegistryError::ExclusiveConflict { holder } => {
                write!(formatter, "resource is already held by session {holder}")
            }
            RegistryError::Cancelled => write!(formatter, "operation cancelled"),
            RegistryError::Acquire(detail) => write!(formatter, "acquire failed: {detail}"),
            RegistryError::Release(detail) => write!(formatter, "release failed: {detail}"),
            RegistryError::Operation(detail) => write!(formatter, "operation failed: {detail}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// A minimal cancellation token, avoiding a dependency for what is one flag
/// and one waker list.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        // Store true before waking so a woken waiter never observes a stale
        // flag and proceeds to run an operation that was already cancelled.
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once cancelled, or immediately if already cancelled.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// Produces and releases the per-session resource.
pub trait ResourceFactory: Send + Sync + 'static {
    type Resource: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Human-readable name used in diagnostics.
    fn label(&self) -> &'static str;

    /// Acquire a resource owned by `session_id`. Must honour cancellation.
    async fn acquire(&self, session_id: &str) -> Result<Self::Resource, Self::Error>;

    /// Release a resource. Must tolerate a partially constructed resource.
    async fn release(&self, session_id: &str, resource: Self::Resource) -> Result<(), Self::Error>;
}

struct Entry<R: Send + Sync + 'static> {
    cancel: CancelToken,
    /// Holds the acquired resource, or `None` before the first operation.
    /// The guard is held across acquisition so two concurrent first calls
    /// cannot both acquire; the stored `Arc` is cloned cheaply per operation.
    resource: Mutex<Option<Arc<R>>>,
    /// Held for the duration of an operation; this is what serializes calls.
    op_lock: Mutex<()>,
    state: AtomicU8,
    last_operation_ms: AtomicU64,
}

/// Owns one resource per live session activation.
pub struct SessionResourceRegistry<F: ResourceFactory> {
    factory: Arc<F>,
    /// When set, only one session may hold the resource at a time. Used for
    /// browser attach mode, where the browser is a single shared process.
    exclusive: bool,
    /// Whether operations from different sessions can collide. True for the
    /// desktop, false for a per-session browser.
    shared_host: bool,
    /// How recently another session must have operated to count as overlap.
    overlap_window: Duration,
    entries: std::sync::Mutex<HashMap<String, Arc<Entry<F::Resource>>>>,
    /// Sessions explicitly disposed. Kept so `status` can still report Closed
    /// after the entry is removed, instead of falling back to Idle.
    closed: std::sync::Mutex<std::collections::HashSet<String>>,
    disposing: AtomicBool,
}

impl<F: ResourceFactory> SessionResourceRegistry<F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory: Arc::new(factory),
            exclusive: false,
            shared_host: false,
            overlap_window: Duration::from_secs(2),
            entries: std::sync::Mutex::new(HashMap::new()),
            closed: std::sync::Mutex::new(std::collections::HashSet::new()),
            disposing: AtomicBool::new(false),
        }
    }

    pub fn exclusive(mut self) -> Self {
        self.exclusive = true;
        self
    }

    /// Mark the underlying host as shared between sessions, enabling overlap
    /// reporting. The desktop is shared; an isolated browser is not.
    pub fn shared_host(mut self) -> Self {
        self.shared_host = true;
        self
    }

    pub fn overlap_window(mut self, window: Duration) -> Self {
        self.overlap_window = window;
        self
    }

    pub fn label(&self) -> &'static str {
        self.factory.label()
    }

    /// Session ids the registry knows about.
    ///
    /// Used by shutdown to dispose every resource, so it must come from the
    /// registry itself rather than from a caller-side table: a session that
    /// acquired a resource is owned here whether or not whoever started it is
    /// still holding a handle.
    pub fn session_ids(&self) -> Vec<String> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = entries.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn status(&self, session_id: &str) -> ResourceStatus {
        if self.disposing.load(Ordering::SeqCst) {
            return ResourceStatus::Closed;
        }
        if let Some(entry) = self.entry_for(session_id) {
            return ResourceStatus::from_u8(entry.state.load(Ordering::SeqCst));
        }
        if self
            .closed
            .lock()
            .is_ok_and(|closed| closed.contains(session_id))
        {
            return ResourceStatus::Closed;
        }
        ResourceStatus::Idle
    }

    /// Sessions that operated within the overlap window, excluding `session_id`.
    ///
    /// Only populated for a shared host, because overlap is only meaningful
    /// where two sessions can touch the same thing.
    pub fn overlapping_sessions(&self, session_id: &str) -> Vec<String> {
        if !self.shared_host {
            return Vec::new();
        }
        let now = now_ms();
        let window = self.overlap_window.as_millis() as u64;
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        let mut names: Vec<(u64, String)> = entries
            .iter()
            .filter(|(id, _)| id.as_str() != session_id)
            .filter_map(|(id, entry)| {
                let last = entry.last_operation_ms.load(Ordering::SeqCst);
                (last != NEVER_OPERATED && now.saturating_sub(last) <= window)
                    .then(|| (last, id.clone()))
            })
            .collect();
        names.sort_by(|a, b| b.0.cmp(&a.0));
        names.into_iter().map(|(_, id)| id).collect()
    }

    /// Acquire the session's resource if needed, run `operation` under the
    /// session's operation lock, and release the lock afterwards.
    ///
    /// Holding the lock across the closure is what serializes calls. Dropping
    /// it on every exit path, including cancellation and panic unwind, is what
    /// keeps a failed call from wedging the session.
    pub async fn run<T, Op, Fut>(
        &self,
        session_id: &str,
        cancel: &CancelToken,
        operation: Op,
    ) -> Result<T, RegistryError>
    where
        Op: FnOnce(Arc<F::Resource>) -> Fut,
        Fut: Future<Output = Result<T, F::Error>>,
    {
        let entry = self.admit(session_id)?;

        if cancel.is_cancelled() {
            return Err(RegistryError::Cancelled);
        }

        // Acquisition is serialized against other acquisitions but not
        // against operations: the guard is dropped before the operation lock
        // is taken, so a slow acquire never blocks another session.
        {
            let mut slot = entry.resource.lock().await;
            if slot.is_none() {
                if cancel.is_cancelled() {
                    return Err(RegistryError::Cancelled);
                }
                let acquired = self
                    .factory
                    .acquire(session_id)
                    .await
                    .map(Arc::new)
                    .map_err(|error| RegistryError::Acquire(error.to_string()))?;
                *slot = Some(acquired);
                entry
                    .state
                    .store(ResourceStatus::Active.as_u8(), Ordering::SeqCst);
            }
        }

        let _guard = entry.op_lock.lock().await;

        if cancel.is_cancelled() {
            return Err(RegistryError::Cancelled);
        }

        entry.last_operation_ms.store(now_ms(), Ordering::SeqCst);

        let resource = entry
            .resource
            .lock()
            .await
            .clone()
            .ok_or(RegistryError::SessionClosing)?;

        operation(resource)
            .await
            .map_err(|error| RegistryError::Operation(error.to_string()))
    }

    /// Dispose one session: refuse new work, cancel in-flight waits, release
    /// the resource, then wait for the operation lock so no call is still
    /// using the resource when `release` runs.
    ///
    /// A session that never acquired anything is still marked closed, because
    /// the caller asked to tear it down and reporting it as idle would suggest
    /// it is still available.
    pub async fn close(&self, session_id: &str) -> Result<(), RegistryError> {
        if let Ok(mut closed) = self.closed.lock() {
            closed.insert(session_id.to_string());
        }

        let Some(entry) = self.take_entry(session_id) else {
            // Already closed, or never acquired. Either way there is nothing
            // to release.
            return Ok(());
        };

        entry
            .state
            .store(ResourceStatus::Closing.as_u8(), Ordering::SeqCst);
        entry.cancel.cancel();

        // Wait for any operation in flight to finish before releasing.
        let _guard = entry.op_lock.lock().await;

        let release = async {
            let taken = entry.resource.lock().await.take();
            if let Some(resource) = taken {
                // The operation lock is held, so no call is still using the
                // resource and the refcount should be exactly one. If it is
                // not, something retained a handle past its operation, and
                // unwrapping would run a destructor while that handle lives.
                let owned = Arc::try_unwrap(resource).map_err(|_| {
                    RegistryError::Release(
                        "resource still referenced after its last operation".to_string(),
                    )
                })?;
                self.factory
                    .release(session_id, owned)
                    .await
                    .map_err(|error| RegistryError::Release(error.to_string()))
            } else {
                Ok(())
            }
        }
        .await;

        entry
            .state
            .store(ResourceStatus::Closed.as_u8(), Ordering::SeqCst);
        release
    }

    /// Dispose every session. Reports the first failure but always attempts
    /// all of them, so one bad session cannot leak the others.
    pub async fn close_all(&self) -> Result<(), RegistryError> {
        self.disposing.store(true, Ordering::SeqCst);

        // Snapshot the ids, then close each through the normal path so the
        // per-entry release ordering is identical to a single close.
        let sessions: Vec<String> = {
            let Ok(entries) = self.entries.lock() else {
                return Ok(());
            };
            entries.keys().cloned().collect()
        };

        let mut first_error = None;
        for session_id in sessions {
            if let Err(error) = self.close(&session_id).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn take_entry(&self, session_id: &str) -> Option<Arc<Entry<F::Resource>>> {
        self.entries
            .lock()
            .ok()
            .and_then(|mut entries| entries.remove(session_id))
    }

    fn entry_for(&self, session_id: &str) -> Option<Arc<Entry<F::Resource>>> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(session_id).cloned())
    }

    /// Clear a disposed session's marker so a resumed activation can acquire a
    /// fresh resource.
    ///
    /// A closed session is deliberately *not* re-acquirable: a tool call that
    /// arrives after the user closed the session would otherwise spawn a new
    /// browser nobody asked for. Resuming is an explicit act, so it is an
    /// explicit `forget`.
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut closed) = self.closed.lock() {
            closed.remove(session_id);
        }
    }

    /// Admission: refuse work during disposal, create the session's entry on
    /// first use, and enforce exclusivity.
    fn admit(&self, session_id: &str) -> Result<Arc<Entry<F::Resource>>, RegistryError> {
        if self.disposing.load(Ordering::SeqCst) {
            return Err(RegistryError::Disposing);
        }

        if self
            .closed
            .lock()
            .is_ok_and(|closed| closed.contains(session_id))
        {
            return Err(RegistryError::SessionClosing);
        }

        let mut entries = self
            .entries
            .lock()
            .map_err(|_| RegistryError::UnknownSession)?;

        if let Some(entry) = entries.get(session_id) {
            if ResourceStatus::from_u8(entry.state.load(Ordering::SeqCst))
                == ResourceStatus::Closing
            {
                return Err(RegistryError::SessionClosing);
            }
            return Ok(entry.clone());
        }

        if self.exclusive
            && let Some((holder, _)) = entries.iter().next()
        {
            return Err(RegistryError::ExclusiveConflict {
                holder: holder.clone(),
            });
        }

        let entry = Arc::new(Entry {
            cancel: CancelToken::new(),
            resource: Mutex::new(None),
            op_lock: Mutex::new(()),
            state: AtomicU8::new(ResourceStatus::Idle.as_u8()),
            last_operation_ms: AtomicU64::new(NEVER_OPERATED),
        });
        entries.insert(session_id.to_string(), entry.clone());
        Ok(entry)
    }
}

/// Sentinel for "this session has never run an operation". A plain 0 would
/// collide with a real timestamp, because a first operation can easily land
/// within the first millisecond of process start.
const NEVER_OPERATED: u64 = u64::MAX;

fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug)]
    struct FakeError(String);

    impl std::fmt::Display for FakeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for FakeError {}

    #[derive(Default)]
    struct Counters {
        acquired: AtomicUsize,
        released: AtomicUsize,
    }

    struct FakeFactory {
        counters: Arc<Counters>,
        fail_acquire: bool,
    }

    struct FakeResource {
        id: String,
    }

    impl ResourceFactory for FakeFactory {
        type Resource = FakeResource;
        type Error = FakeError;

        fn label(&self) -> &'static str {
            "fake"
        }

        async fn acquire(&self, session_id: &str) -> Result<FakeResource, FakeError> {
            if self.fail_acquire {
                return Err(FakeError("cannot acquire".to_string()));
            }
            self.counters.acquired.fetch_add(1, Ordering::SeqCst);
            Ok(FakeResource {
                id: session_id.to_string(),
            })
        }

        async fn release(
            &self,
            _session_id: &str,
            _resource: FakeResource,
        ) -> Result<(), FakeError> {
            self.counters.released.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn factory() -> (FakeFactory, Arc<Counters>) {
        let counters = Arc::new(Counters::default());
        (
            FakeFactory {
                counters: counters.clone(),
                fail_acquire: false,
            },
            counters,
        )
    }

    fn acquired(counters: &Counters) -> usize {
        counters.acquired.load(Ordering::SeqCst)
    }

    fn released(counters: &Counters) -> usize {
        counters.released.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn nothing_is_acquired_before_the_first_operation() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);

        assert_eq!(registry.status("session-1"), ResourceStatus::Idle);
        assert_eq!(acquired(&counters), 0);
    }

    #[tokio::test]
    async fn first_operation_acquires_once_and_later_calls_reuse_it() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        for _ in 0..3 {
            let id = registry
                .run("session-1", &cancel, |resource| async move {
                    Ok(resource.id.clone())
                })
                .await
                .unwrap();
            assert_eq!(id, "session-1");
        }

        assert_eq!(acquired(&counters), 1);
        assert_eq!(registry.status("session-1"), ResourceStatus::Active);
    }

    #[tokio::test]
    async fn operations_on_one_session_run_in_arrival_order() {
        let (factory, _counters) = factory();
        let registry = Arc::new(SessionResourceRegistry::new(factory));
        let cancel = CancelToken::new();

        let order = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for index in 0..8 {
            let registry = registry.clone();
            let cancel = cancel.clone();
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                registry
                    .run("session-1", &cancel, move |_resource| async move {
                        order.lock().await.push(index);
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        Ok(())
                    })
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }

        let observed = order.lock().await.clone();
        assert_eq!(observed, (0..8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn distinct_sessions_do_not_serialize_against_each_other() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        let slow = registry.run("session-a", &cancel, |_| async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(())
        });

        // B must finish while A is still working. If sessions shared a lock,
        // B could not complete until A's 60ms elapsed.
        let start = Instant::now();
        registry
            .run("session-b", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        let b_elapsed = start.elapsed();

        slow.await.unwrap();

        assert!(
            b_elapsed < Duration::from_millis(40),
            "session-b waited {b_elapsed:?} behind session-a",
        );
        assert_eq!(acquired(&counters), 2);
    }

    #[tokio::test]
    async fn close_releases_the_resource_and_refuses_later_work() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        registry
            .run("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();

        registry.close("session-1").await.unwrap();
        assert_eq!(released(&counters), 1);
        assert_eq!(registry.status("session-1"), ResourceStatus::Closed);

        // A closed session must not silently acquire a new resource.
        assert!(matches!(
            registry
                .run("session-1", &cancel, |_| async { Ok(()) })
                .await,
            Err(RegistryError::SessionClosing)
        ));

        // Resuming is explicit, and yields a genuinely fresh resource.
        registry.forget("session-1");
        registry
            .run("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(acquired(&counters), 2);
    }

    #[tokio::test]
    async fn close_waits_for_in_flight_work_before_releasing() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        let in_flight = registry.run("session-1", &cancel, |_| async {
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok(())
        });
        // Give the operation a moment to take the lock, then close underneath.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let closing = registry.close("session-1");

        let (operation, closed) = tokio::join!(in_flight, closing);
        operation.unwrap();
        closed.unwrap();

        assert_eq!(
            released(&counters),
            1,
            "resource must not be released while an operation still holds it",
        );
    }

    #[tokio::test]
    async fn closing_an_unknown_session_is_a_no_op() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        assert!(registry.close("never-existed").await.is_ok());
        assert_eq!(released(&counters), 0);
    }

    #[tokio::test]
    async fn cancelled_calls_do_not_acquire() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();
        cancel.cancel();

        assert!(matches!(
            registry
                .run("session-1", &cancel, |_| async { Ok(()) })
                .await,
            Err(RegistryError::Cancelled)
        ));
        assert_eq!(acquired(&counters), 0);
    }

    #[tokio::test]
    async fn acquire_failure_surfaces_and_leaves_the_session_retryable() {
        let counters = Arc::new(Counters::default());
        let registry = SessionResourceRegistry::new(FakeFactory {
            counters,
            fail_acquire: true,
        });
        let cancel = CancelToken::new();

        assert!(matches!(
            registry
                .run("session-1", &cancel, |_| async { Ok(()) })
                .await,
            Err(RegistryError::Acquire(_))
        ));
    }

    #[tokio::test]
    async fn operation_errors_propagate_without_wedging_the_session() {
        let (factory, _counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        let failed = registry
            .run("session-1", &cancel, |_| async {
                Err::<(), _>(FakeError("selector not found".to_string()))
            })
            .await;
        assert!(matches!(failed, Err(RegistryError::Operation(_))));

        // The lock must have been released, so a later call still works.
        registry
            .run("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exclusive_registry_refuses_a_second_holder() {
        let (factory, _counters) = factory();
        let registry = SessionResourceRegistry::new(factory).exclusive();
        let cancel = CancelToken::new();

        registry
            .run("session-1", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();

        assert!(matches!(
            registry
                .run("session-2", &cancel, |_| async { Ok(()) })
                .await,
            Err(RegistryError::ExclusiveConflict { .. })
        ));
    }

    #[tokio::test]
    async fn shared_host_reports_overlap_and_per_host_does_not() {
        let cancel = CancelToken::new();

        let (shared_factory, _) = factory();
        let shared = SessionResourceRegistry::new(shared_factory).shared_host();
        shared
            .run("session-a", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(shared.overlapping_sessions("session-b"), vec!["session-a"]);

        let (isolated_factory, _) = factory();
        let isolated = SessionResourceRegistry::new(isolated_factory);
        isolated
            .run("session-a", &cancel, |_| async { Ok(()) })
            .await
            .unwrap();
        assert!(isolated.overlapping_sessions("session-b").is_empty());
    }

    #[tokio::test]
    async fn close_all_releases_every_session_and_stops_admission() {
        let (factory, counters) = factory();
        let registry = SessionResourceRegistry::new(factory);
        let cancel = CancelToken::new();

        for id in ["session-1", "session-2", "session-3"] {
            registry
                .run(id, &cancel, |_| async { Ok(()) })
                .await
                .unwrap();
        }
        assert_eq!(acquired(&counters), 3);

        registry.close_all().await.unwrap();
        assert_eq!(released(&counters), 3);
        assert_eq!(registry.status("session-1"), ResourceStatus::Closed);

        assert!(matches!(
            registry
                .run("session-4", &cancel, |_| async { Ok(()) })
                .await,
            Err(RegistryError::Disposing)
        ));
    }

    #[tokio::test]
    async fn cancel_token_wakes_waiters() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());

        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            true
        });

        tokio::time::sleep(Duration::from_millis(5)).await;
        token.cancel();

        assert!(handle.await.unwrap());
        assert!(token.is_cancelled());
    }
}
