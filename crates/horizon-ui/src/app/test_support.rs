//! Headless `HorizonApp` construction shared by the app-level tests.

use std::path::Path;

use eframe::CreationContext;
use egui::{Context, Event, Key, Modifiers, RawInput};
use horizon_core::link::LinkServer;
use horizon_core::{Config, HorizonHome, PanelKind, PanelOptions, RuntimeState, SessionStore, StartupDecision};

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

/// Options of a command panel running `script` through `sh -c`, so tests get a real PTY child
/// without depending on the user's shell.
pub(super) fn command_options(name: &str, script: &str) -> PanelOptions {
    PanelOptions {
        name: Some(name.to_owned()),
        kind: PanelKind::Command,
        command: Some("sh".to_owned()),
        args: vec!["-c".to_owned(), script.to_owned()],
        ..PanelOptions::default()
    }
}

/// Runs `body` in one headless egui frame that starts with `key` pressed under `modifiers`, the
/// way the window system would deliver it.
pub(super) fn with_key_press<R>(key: Key, modifiers: Modifiers, body: impl FnOnce(&Context) -> R) -> R {
    let ctx = Context::default();
    let input = RawInput {
        events: vec![Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }],
        ..RawInput::default()
    };
    let mut body = Some(body);
    let mut result = None;
    let _ = ctx.run(input, |ctx| {
        if let Some(body) = body.take() {
            result = Some(body(ctx));
        }
    });
    result.expect("the frame ran the body")
}
