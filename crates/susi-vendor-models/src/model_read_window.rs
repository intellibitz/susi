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
    use std::{cell::RefCell, fs::File};
    use susi_config::windows_model_identity::{model_file_identity, ModelFileIdentity};
    struct Pin {
        identity: ModelFileIdentity,
        digest: String,
        _file: File,
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
