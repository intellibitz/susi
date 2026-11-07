//! Reuse verified bytes only within a synchronous reasoning call on this thread.
use std::{fs::File, marker::PhantomData, rc::Rc};

#[must_use]
pub struct ModelReadWindow(PhantomData<Rc<()>>);

impl ModelReadWindow {
    pub fn enter() -> Self {
        #[cfg(windows)]
        window::enter();
        Self(PhantomData)
    }
}

impl Drop for ModelReadWindow {
    fn drop(&mut self) {
        #[cfg(windows)]
        window::leave();
    }
}

pub(super) fn open(path: &std::path::Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Deny concurrent writes and writable mappings, including pre-existing
        // ones. Permit atomic rename/replacement while readers retain old bytes.
        options.share_mode(1 | 4);
    }
    options.open(path)
}

#[cfg(windows)]
pub(super) mod window {
    use std::{
        cell::RefCell,
        fs::File,
        sync::{Arc, Mutex, Weak},
    };
    use susi_config::windows_model_identity::{model_file_identity, ModelFileIdentity};
    struct Pin {
        identity: ModelFileIdentity,
        digest: String,
        _file: File,
    }
    pub(super) struct ResidentPin {
        pin: Pin,
        published: std::sync::atomic::AtomicBool,
    }
    impl ResidentPin {
        pub(super) fn publish(&self) {
            self.published
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
    // Weak references cannot prolong a load. Pending and live entries share
    // this bound; a failed or refused cold load releases its slot and handle.
    static RESIDENT: Mutex<Vec<Weak<ResidentPin>>> = Mutex::new(Vec::new());
    pub(super) fn reserve(file: &File, digest: &str) -> Option<Arc<ResidentPin>> {
        let identity = model_file_identity(file)?;
        let mut pins = RESIDENT.lock().unwrap_or_else(|e| e.into_inner());
        pins.retain(|p| p.strong_count() != 0);
        if pins.len() >= 4 {
            return None;
        }
        let pin = Arc::new(ResidentPin {
            pin: Pin {
                identity,
                digest: digest.to_owned(),
                _file: file.try_clone().ok()?,
            },
            published: std::sync::atomic::AtomicBool::new(false),
        });
        pins.push(Arc::downgrade(&pin));
        Some(pin)
    }
    #[derive(Default)]
    struct Window {
        depth: usize,
        pins: Vec<Pin>,
    }
    thread_local! { static STATE: RefCell<Window> = RefCell::default(); }
    pub(super) fn enter() {
        STATE.with(|s| s.borrow_mut().depth += 1);
    }
    pub(super) fn leave() {
        STATE.with(|s| {
            let mut window = s.borrow_mut();
            window.depth -= 1;
            if window.depth == 0 {
                window.pins.clear();
            }
        });
    }
    pub(in crate::model_license) fn identity(file: &File) -> Option<ModelFileIdentity> {
        if STATE.with(|s| s.borrow().depth == 0) {
            return None;
        }
        model_file_identity(file)
    }
    pub(in crate::model_license) fn matches(identity: &ModelFileIdentity, digest: &str) -> bool {
        STATE.with(|s| {
            s.borrow()
                .pins
                .iter()
                .any(|p| p.identity == *identity && p.digest == digest)
        })
    }
    pub(in crate::model_license) fn resident_matches(
        identity: &ModelFileIdentity,
        digest: &str,
    ) -> bool {
        RESIDENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(Weak::upgrade)
            .any(|pin| {
                pin.published.load(std::sync::atomic::Ordering::Acquire)
                    && pin.pin.identity == *identity
                    && pin.pin.digest == digest
            })
    }
    pub(in crate::model_license) fn retain(identity: ModelFileIdentity, digest: &str, file: &File) {
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            if state.depth == 0 {
                return;
            }
            let Ok(held) = file.try_clone() else {
                return;
            };
            // Enough for a target, a speculative draft and their tokenizers.
            // A longer candidate list cannot grow handles.
            if state.pins.len() == 4 {
                state.pins.remove(0);
            }
            state.pins.push(Pin {
                identity,
                digest: digest.to_owned(),
                _file: held,
            });
        });
    }
}

// Only a successfully loaded model entry owns this lease. On eviction or load
// failure its handle closes. Weak registry entries cannot prolong that lifetime.
pub struct ResidentLease {
    #[cfg(windows)]
    pin: std::sync::Arc<window::ResidentPin>,
}
impl ResidentLease {
    pub(super) fn reserve(file: &File, digest: &str) -> Option<Self> {
        #[cfg(windows)]
        {
            Some(Self {
                pin: window::reserve(file, digest)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (file, digest);
            None
        }
    }
    /// Publish only after installing successfully decoded and rechecked cache data.
    pub fn publish(self) -> Self {
        #[cfg(windows)]
        self.pin.publish();
        self
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn enter_and_drop_manage_nested_read_windows_cleanly() {
        let outer = ModelReadWindow::enter();
        {
            let _inner = ModelReadWindow::enter();
        }
        drop(outer);
    }

    #[test]
    fn open_reads_existing_file_bytes_with_shared_read_permissions() {
        let path = std::env::temp_dir().join(format!("susi-model-read-win-{}", std::process::id()));
        std::fs::write(&path, b"gguf-header-bytes").unwrap();

        let mut file = open(&path).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"gguf-header-bytes");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_returns_error_when_target_file_does_not_exist() {
        let missing = std::env::temp_dir().join(format!(
            "susi-model-read-missing-{}-none.bin",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert!(open(&missing).is_err());
    }

    #[test]
    fn concurrent_open_handles_can_read_same_model_file_simultaneously() {
        let path =
            std::env::temp_dir().join(format!("susi-model-read-shared-{}", std::process::id()));
        std::fs::write(&path, b"weights-payload").unwrap();

        let _win = ModelReadWindow::enter();
        let f1 = open(&path).unwrap();
        let f2 = open(&path).unwrap();
        assert_eq!(f1.metadata().unwrap().len(), 15);
        assert_eq!(f2.metadata().unwrap().len(), 15);

        drop(f1);
        drop(f2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sequential_read_windows_can_be_entered_and_dropped_repeatedly() {
        for _ in 0..3 {
            let win = ModelReadWindow::enter();
            let nested = ModelReadWindow::enter();
            drop(nested);
            drop(win);
        }
    }
}
