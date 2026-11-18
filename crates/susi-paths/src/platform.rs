//! Platform services core does not implement on non-Linux targets (Mandate 84).
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

/// Platform service hooks for non-Linux distributions.
#[derive(Clone, Copy)]
pub struct Services {
    /// Create-or-open for appending, owner-only from birth.
    pub open_owner_only_append: fn(&Path) -> io::Result<File>,
}

static SERVICES: OnceLock<Services> = OnceLock::new();

/// Install the services; the first installation wins and later ones are ignored.
pub fn install(services: Services) {
    let _ = SERVICES.set(services);
}

/// The installed services, if any.
pub fn services() -> Option<&'static Services> {
    SERVICES.get()
}

/// The installed services, or the refusal a protected operation returns.
pub fn required(operation: &str) -> io::Result<&'static Services> {
    services().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{operation} on this OS needs the platform services Susi Commercial Apps installs; core implements it for Linux only"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protected_operation_without_services_refuses() {
        let error = required("owner-only create").err().map(|e| e.kind());
        assert_eq!(error, Some(io::ErrorKind::Unsupported));
    }
}
