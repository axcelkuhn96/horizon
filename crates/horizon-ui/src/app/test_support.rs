//! Headless `HorizonApp` construction shared by the app-level tests.

use std::path::Path;

use eframe::CreationContext;
use egui::Context;
use horizon_core::link::LinkServer;
use horizon_core::{Config, HorizonHome, RuntimeState, SessionStore, StartupDecision};

use super::HorizonApp;
use crate::input;

/// An ephemeral app on the default config with no link server, rooted in `dir`.
pub(super) fn test_app_in(dir: &Path) -> HorizonApp {
    test_app_with(dir, &Config::default(), RuntimeState::default(), None)
}

/// An ephemeral app restoring `state`, rooted in `dir`; `link_server` is injected as in
/// `HorizonApp::with_link_server`.
pub(super) fn test_app_with(
    dir: &Path,
    config: &Config,
    state: RuntimeState,
    link_server: Option<LinkServer>,
) -> HorizonApp {
    let config_path = dir.join("config.yaml");
    let home = HorizonHome::from_root(dir.join(".horizon"));
    let session_store = SessionStore::new(home, config_path.clone());
    let cc = CreationContext::_new_kittest(Context::default());

    HorizonApp::with_link_server(
        &cc,
        config,
        config_path,
        session_store,
        StartupDecision::Ephemeral {
            runtime_state: Box::new(state),
        },
        input::ObservedKeyboardInputs::default(),
        link_server,
    )
}

/// Like [`test_app_in`] with a throw-away directory that is gone once this returns.
pub(super) fn test_app() -> HorizonApp {
    let temp = tempfile::tempdir().expect("temp dir");
    test_app_in(temp.path())
}
