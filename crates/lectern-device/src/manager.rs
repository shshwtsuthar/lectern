use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    thread,
    time::Duration,
};

use crate::{
    BatchTransferOutcome, DeviceBook, DeviceConnectionState, DeviceError, DeviceId, DeviceInfo,
    DeviceTransferBook, DuplicatePolicy, FormatPriority, KoboDriver, RemovableStorageProvider,
    RemovalOutcome, SystemRemovableStorageProvider, TransferControl, TransferPlan,
    TransferProgress,
    history::TransferHistoryStore,
    kobo::is_candidate_volume,
    transfer::{build_plan, execute_plan, list_device_books, remove_device_book},
};

const EJECT_CONFIRMATION_ATTEMPTS: usize = 40;
const EJECT_CONFIRMATION_INTERVAL: Duration = Duration::from_millis(250);

/// Connection changes and the authoritative connected-device snapshot after reconciliation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconcileResult {
    /// Newly connected devices.
    pub connected: Vec<DeviceInfo>,
    /// Stable identities no longer mounted.
    pub disconnected: Vec<DeviceId>,
    /// All currently connected devices.
    pub devices: Vec<DeviceInfo>,
}

struct DeviceSession {
    info: Mutex<DeviceInfo>,
    operation: Mutex<()>,
}

struct ManagerInner<P> {
    provider: P,
    sessions: Mutex<HashMap<DeviceId, Arc<DeviceSession>>>,
    history: Mutex<TransferHistoryStore>,
}

/// Thread-safe connected-device registry and operation dispatcher.
pub struct DeviceManager<P = SystemRemovableStorageProvider> {
    inner: Arc<ManagerInner<P>>,
}

impl<P> Clone for DeviceManager<P> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl DeviceManager<SystemRemovableStorageProvider> {
    /// Creates a manager backed by the current operating system.
    ///
    /// # Errors
    ///
    /// Returns an error when local transfer history cannot be loaded safely.
    pub fn system(history_path: PathBuf) -> Result<Self, DeviceError> {
        Self::new(SystemRemovableStorageProvider, history_path)
    }
}

impl<P: RemovableStorageProvider> DeviceManager<P> {
    /// Creates a manager with an injectable mounted-storage boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when local transfer history cannot be loaded safely.
    pub fn new(provider: P, history_path: PathBuf) -> Result<Self, DeviceError> {
        Ok(Self {
            inner: Arc::new(ManagerInner {
                provider,
                sessions: Mutex::new(HashMap::new()),
                history: Mutex::new(TransferHistoryStore::load(history_path)?),
            }),
        })
    }

    /// Reconciles mounted volumes with the connected-device registry.
    ///
    /// Only local/removable-looking mount roots receive the cheap `.kobo` marker check. Stable
    /// platform identity lookup occurs only after that strong marker is present.
    ///
    /// # Errors
    ///
    /// Returns an error when volume discovery or a marked mount inspection fails.
    pub fn reconcile(&self) -> Result<ReconcileResult, DeviceError> {
        let volumes = self.inner.provider.list_mounted_volumes()?;
        let mut detected = HashMap::<DeviceId, DeviceInfo>::new();
        for volume in volumes.iter().filter(|volume| is_candidate_volume(volume)) {
            if !KoboDriver::has_kobo_marker(&volume.mount_path) {
                continue;
            }
            let stable_volume_id = self.inner.provider.stable_volume_id(volume);
            if let Some(device) = KoboDriver::detect(volume, stable_volume_id.as_deref())? {
                detected.insert(device.id.clone(), device);
            }
        }

        let mut sessions = lock(&self.inner.sessions)?;
        let previous = sessions.keys().cloned().collect::<HashSet<_>>();
        let current = detected.keys().cloned().collect::<HashSet<_>>();
        let mut connected = Vec::new();
        for (id, mut info) in detected {
            if let Some(session) = sessions.get(&id) {
                let mut existing = lock(&session.info)?;
                info.state = existing.state;
                *existing = info;
            } else {
                tracing::info!(device_id = %id, "device connected");
                connected.push(info.clone());
                sessions.insert(
                    id,
                    Arc::new(DeviceSession {
                        info: Mutex::new(info),
                        operation: Mutex::new(()),
                    }),
                );
            }
        }
        let mut disconnected = previous.difference(&current).cloned().collect::<Vec<_>>();
        disconnected.sort();
        for id in &disconnected {
            sessions.remove(id);
            tracing::info!(device_id = %id, "device disconnected");
        }
        connected.sort_by(|left, right| left.id.cmp(&right.id));
        let mut devices = sessions
            .values()
            .map(|session| lock(&session.info).map(|info| info.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(ReconcileResult {
            connected,
            disconnected,
            devices,
        })
    }

    /// Returns a stable snapshot of every currently connected reader.
    ///
    /// # Errors
    ///
    /// Returns an error when synchronized device state is unavailable.
    pub fn connected_devices(&self) -> Result<Vec<DeviceInfo>, DeviceError> {
        let sessions = lock(&self.inner.sessions)?;
        let mut devices = sessions
            .values()
            .map(|session| lock(&session.info).map(|info| info.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(devices)
    }

    /// Refreshes storage and marker state for one reader without racing its other operations.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader is busy, disconnected, or cannot be inspected.
    pub fn refresh(&self, id: &DeviceId) -> Result<DeviceInfo, DeviceError> {
        let session = self.session(id)?;
        let _operation = try_operation(&session)?;
        self.refresh_session(id, &session)
    }

    /// Builds a deterministic, no-write transfer plan.
    ///
    /// # Errors
    ///
    /// Returns an error for a busy/disconnected reader, unsafe path, or insufficient storage.
    pub fn plan_transfer(
        &self,
        id: &DeviceId,
        books: &[DeviceTransferBook],
        priority: &FormatPriority,
    ) -> Result<TransferPlan, DeviceError> {
        let session = self.session(id)?;
        let _operation = try_operation(&session)?;
        let info = self.refresh_session(id, &session)?;
        let history = lock(&self.inner.history)?;
        build_plan(&info, &history, books, priority)
    }

    /// Executes a prepared batch using a bounded buffer and caller-controlled cancellation.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader disconnects, is busy, or the batch is cancelled.
    pub fn transfer(
        &self,
        plan: &TransferPlan,
        duplicate_policy: DuplicatePolicy,
        progress: impl FnMut(TransferProgress) -> TransferControl,
    ) -> Result<BatchTransferOutcome, DeviceError> {
        let session = self.session(&plan.device_id)?;
        let _operation = try_operation(&session)?;
        let info = self.refresh_session(&plan.device_id, &session)?;
        let mut history = lock(&self.inner.history)?;
        let result = execute_plan(&info, &mut history, plan, duplicate_policy, progress);
        drop(history);
        let _refreshed = self.refresh_session(&plan.device_id, &session);
        if let Err(error) = &result {
            tracing::warn!(
                device_id = %plan.device_id,
                error = %error,
                "device transfer failed"
            );
        }
        result
    }

    /// Lists publication files below Lectern's controlled `Books/` directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader is busy/disconnected or its controlled tree is unsafe.
    pub fn list_books(&self, id: &DeviceId) -> Result<Vec<DeviceBook>, DeviceError> {
        let session = self.session(id)?;
        let _operation = try_operation(&session)?;
        let info = self.refresh_session(id, &session)?;
        let history = lock(&self.inner.history)?;
        list_device_books(&info, &history)
    }

    /// Removes one verified reader copy while preserving its library source.
    ///
    /// # Errors
    ///
    /// Returns an error when history cannot prove ownership, content changed, or deletion fails.
    pub fn remove_book(
        &self,
        id: &DeviceId,
        relative_path: &Path,
    ) -> Result<RemovalOutcome, DeviceError> {
        let session = self.session(id)?;
        let _operation = try_operation(&session)?;
        let info = self.refresh_session(id, &session)?;
        let mut history = lock(&self.inner.history)?;
        let result = remove_device_book(&info, &mut history, relative_path);
        drop(history);
        let _refreshed = self.refresh_session(id, &session);
        result
    }

    /// Requests safe operating-system eject and waits for mount disappearance confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader is busy/disconnected or the OS does not confirm eject.
    pub fn eject(&self, id: &DeviceId) -> Result<(), DeviceError> {
        let session = self.session(id)?;
        let _operation = try_operation(&session)?;
        let device = {
            let mut info = lock(&session.info)?;
            info.state = DeviceConnectionState::Ejecting;
            info.clone()
        };
        tracing::info!(device_id = %device.id, "requesting device eject");
        if let Err(error) = self.inner.provider.eject(&device) {
            lock(&session.info)?.state = DeviceConnectionState::Connected;
            tracing::warn!(device_id = %device.id, error = %error, "device eject failed");
            return Err(error);
        }

        for _ in 0..EJECT_CONFIRMATION_ATTEMPTS {
            let still_mounted = self
                .inner
                .provider
                .list_mounted_volumes()?
                .iter()
                .any(|volume| same_marked_mount(volume, &device.mount_path));
            if !still_mounted {
                lock(&self.inner.sessions)?.remove(id);
                tracing::info!(device_id = %device.id, "device eject confirmed");
                return Ok(());
            }
            thread::sleep(EJECT_CONFIRMATION_INTERVAL);
        }
        lock(&session.info)?.state = DeviceConnectionState::Connected;
        let error = DeviceError::Platform(
            "the operating system did not confirm that the Kobo was ejected".to_owned(),
        );
        tracing::warn!(device_id = %device.id, error = %error, "device eject failed");
        Err(error)
    }

    fn session(&self, id: &DeviceId) -> Result<Arc<DeviceSession>, DeviceError> {
        lock(&self.inner.sessions)?
            .get(id)
            .cloned()
            .ok_or(DeviceError::Disconnected)
    }

    fn refresh_session(
        &self,
        id: &DeviceId,
        session: &DeviceSession,
    ) -> Result<DeviceInfo, DeviceError> {
        let current = lock(&session.info)?.clone();
        let volumes = self.inner.provider.list_mounted_volumes()?;
        let volume = volumes
            .iter()
            .find(|volume| same_marked_mount(volume, &current.mount_path))
            .ok_or(DeviceError::Disconnected)?;
        let stable_id = self.inner.provider.stable_volume_id(volume);
        let mut refreshed = KoboDriver::detect(volume, stable_id.as_deref())?
            .filter(|device| &device.id == id)
            .ok_or(DeviceError::Disconnected)?;
        refreshed.state = current.state;
        *lock(&session.info)? = refreshed.clone();
        Ok(refreshed)
    }
}

fn same_marked_mount(volume: &crate::MountedVolume, canonical_mount: &Path) -> bool {
    KoboDriver::has_kobo_marker(&volume.mount_path)
        && std::fs::canonicalize(&volume.mount_path).is_ok_and(|path| path == canonical_mount)
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DeviceError> {
    mutex.lock().map_err(|_| DeviceError::State)
}

fn try_operation(session: &DeviceSession) -> Result<MutexGuard<'_, ()>, DeviceError> {
    match session.operation.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::WouldBlock) => Err(DeviceError::Busy),
        Err(TryLockError::Poisoned(_)) => Err(DeviceError::State),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use lectern_core::{AssetId, BookFormat, BookId};
    use tempfile::tempdir;

    use super::DeviceManager;
    use crate::{
        DeviceError, DeviceTransferBook, DeviceTransferSource, DuplicatePolicy, FormatPriority,
        MountedVolume, RemovableStorageProvider, TransferControl,
    };

    #[derive(Clone)]
    struct MockProvider {
        volumes: Arc<Mutex<Vec<MountedVolume>>>,
        eject_error: Arc<Mutex<Option<String>>>,
    }

    impl MockProvider {
        fn new(volumes: Vec<MountedVolume>) -> Self {
            Self {
                volumes: Arc::new(Mutex::new(volumes)),
                eject_error: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl RemovableStorageProvider for MockProvider {
        fn list_mounted_volumes(&self) -> Result<Vec<MountedVolume>, DeviceError> {
            Ok(self.volumes.lock().unwrap().clone())
        }

        fn stable_volume_id(&self, volume: &MountedVolume) -> Option<String> {
            Some(format!("mock-{}", volume.total_bytes))
        }

        fn eject(&self, device: &crate::DeviceInfo) -> Result<(), DeviceError> {
            if let Some(error) = self.eject_error.lock().unwrap().clone() {
                return Err(DeviceError::Platform(error));
            }
            self.volumes
                .lock()
                .unwrap()
                .retain(|volume| volume.mount_path != device.mount_path);
            Ok(())
        }
    }

    fn volume(path: &Path, name: &str, total: u64) -> MountedVolume {
        MountedVolume {
            name: OsString::from(name),
            mount_path: path.to_path_buf(),
            file_system: OsString::from("vfat"),
            total_bytes: total,
            free_bytes: total / 2,
            removable: true,
        }
    }

    #[test]
    fn reconciliation_tracks_connect_disconnect_and_multiple_readers() {
        let ordinary = tempdir().unwrap();
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        fs::create_dir(first.path().join(".kobo")).unwrap();
        fs::create_dir(second.path().join(".kobo")).unwrap();
        let provider = MockProvider::new(vec![
            volume(ordinary.path(), "USB", 10),
            volume(first.path(), "KOBOeReader", 20),
            volume(second.path(), "Reader", 30),
        ]);
        let history = tempdir().unwrap();
        let manager =
            DeviceManager::new(provider.clone(), history.path().join("history.json")).unwrap();
        let first_result = manager.reconcile().unwrap();
        assert_eq!(first_result.connected.len(), 2);
        assert_eq!(first_result.devices.len(), 2);

        provider
            .volumes
            .lock()
            .unwrap()
            .retain(|mounted| mounted.mount_path != first.path());
        let second_result = manager.reconcile().unwrap();
        assert_eq!(second_result.disconnected.len(), 1);
        assert_eq!(second_result.devices.len(), 1);
    }

    #[test]
    fn eject_reports_success_failure_and_disconnect_before_request() {
        let mount = tempdir().unwrap();
        fs::create_dir(mount.path().join(".kobo")).unwrap();
        let provider = MockProvider::new(vec![volume(mount.path(), "KOBOeReader", 20)]);
        let history = tempdir().unwrap();
        let manager =
            DeviceManager::new(provider.clone(), history.path().join("history.json")).unwrap();
        let id = manager.reconcile().unwrap().devices[0].id.clone();
        *provider.eject_error.lock().unwrap() = Some("mock failure".to_owned());
        assert!(manager.eject(&id).is_err());
        *provider.eject_error.lock().unwrap() = None;
        assert!(manager.eject(&id).is_ok());
        assert!(manager.connected_devices().unwrap().is_empty());
        assert!(matches!(manager.eject(&id), Err(DeviceError::Disconnected)));
    }

    #[test]
    fn eject_does_not_race_an_active_transfer() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source.epub");
        fs::write(&source, vec![1_u8; 600_000]).unwrap();
        let mount = tempdir().unwrap();
        fs::create_dir(mount.path().join(".kobo")).unwrap();
        let provider = MockProvider::new(vec![volume(mount.path(), "KOBOeReader", 2_000_000)]);
        let manager = DeviceManager::new(provider, fixture.path().join("history.json")).unwrap();
        let id = manager.reconcile().unwrap().devices[0].id.clone();
        let plan = manager
            .plan_transfer(
                &id,
                &[DeviceTransferBook {
                    book_id: BookId::new(1),
                    title: "Title".to_owned(),
                    authors: "Author".to_owned(),
                    sources: vec![DeviceTransferSource {
                        asset_id: AssetId::new(2),
                        format: BookFormat::Epub,
                        path: source,
                    }],
                }],
                &FormatPriority::default(),
            )
            .unwrap();
        let transfer_manager = manager.clone();
        let (started, wait_for_started) = mpsc::channel();
        let (resume, wait_for_resume) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let transfer_paused = Arc::clone(&paused);
        let handle = thread::spawn(move || {
            transfer_manager.transfer(&plan, DuplicatePolicy::Skip, |_| {
                if !transfer_paused.swap(true, Ordering::Relaxed) {
                    started.send(()).unwrap();
                    wait_for_resume
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap();
                }
                TransferControl::Continue
            })
        });
        wait_for_started
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(manager.eject(&id), Err(DeviceError::Busy)));
        resume.send(()).unwrap();
        assert!(handle.join().unwrap().is_ok());
    }
}
