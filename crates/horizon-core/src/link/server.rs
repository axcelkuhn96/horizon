//! Unix-socket server for the panel link.
//!
//! Connection threads only parse a request and forward it; every `Board` access happens
//! on the UI thread, which drains [`LinkServer::try_recv`] and answers through
//! [`PendingRequest::reply`].

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::{LinkRequest, LinkResponse};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::LinkServer;

/// One parsed request waiting for the UI thread; answer through `reply`.
#[derive(Debug)]
pub struct PendingRequest {
    pub request: LinkRequest,
    pub reply: mpsc::Sender<LinkResponse>,
}

/// `<xdg>/horizon/<pid>.sock` when `XDG_RUNTIME_DIR` is set, else `<home>/.horizon/run/<pid>.sock`.
#[must_use]
pub fn socket_path(pid: u32, xdg_runtime_dir: Option<&Path>, home: &Path) -> PathBuf {
    let file = format!("{pid}.sock");
    match xdg_runtime_dir.filter(|dir| !dir.as_os_str().is_empty()) {
        Some(xdg) => xdg.join("horizon").join(file),
        None => home.join(".horizon").join("run").join(file),
    }
}

/// Non-unix stand-in: the panel link needs unix sockets.
#[cfg(not(unix))]
pub struct LinkServer {
    path: PathBuf,
}

#[cfg(not(unix))]
impl LinkServer {
    /// # Errors
    ///
    /// Always `Unsupported` on this platform.
    pub fn start(_path: PathBuf) -> Result<Self, super::LinkError> {
        Err(super::LinkError::Unsupported)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<PendingRequest> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::socket_path;

    #[test]
    fn socket_path_prefers_xdg() {
        let path = socket_path(42, Some(Path::new("/run/user/1000")), Path::new("/home/u"));
        assert_eq!(path, Path::new("/run/user/1000/horizon/42.sock"));
    }

    #[test]
    fn socket_path_falls_back_to_home() {
        let expected = Path::new("/home/u/.horizon/run/42.sock");
        assert_eq!(socket_path(42, None, Path::new("/home/u")), expected);
        assert_eq!(socket_path(42, Some(Path::new("")), Path::new("/home/u")), expected);
    }
}
