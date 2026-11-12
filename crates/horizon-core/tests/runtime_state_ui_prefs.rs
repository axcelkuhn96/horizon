use horizon_core::{Config, RuntimeState, UiPrefs};

#[test]
fn ui_prefs_default_when_missing_in_old_state_file() {
    let state: RuntimeState = serde_yaml::from_str("version: 2\nworkspaces: []\n").expect("old state file parses");

    assert_eq!(state.ui, UiPrefs::default());
    assert!(state.ui.minimap_visible_or_default());
    assert!(!state.ui.hud_visible_or_default());
}

#[test]
fn ui_prefs_round_trip() {
    let state = RuntimeState {
        ui: UiPrefs::from_visibility(false, true),
        ..RuntimeState::default()
    };

    let yaml = state.to_yaml().expect("serialize runtime state");
    let reloaded: RuntimeState = serde_yaml::from_str(&yaml).expect("reload runtime state");

    assert_eq!(reloaded.ui.minimap_visible, Some(false));
    assert_eq!(reloaded.ui.hud_visible, Some(true));
    assert!(!reloaded.ui.minimap_visible_or_default());
    assert!(reloaded.ui.hud_visible_or_default());
}

#[test]
fn ui_prefs_omitted_from_yaml_when_default() {
    let state = RuntimeState {
        ui: UiPrefs::from_visibility(true, false),
        ..RuntimeState::default()
    };

    let yaml = state.to_yaml().expect("serialize runtime state");

    assert!(
        !yaml.contains("ui:"),
        "pristine toggles must leave no `ui` block:\n{yaml}"
    );
}

#[test]
fn ui_prefs_keep_only_the_toggle_that_left_its_default() {
    let state = RuntimeState {
        ui: UiPrefs::from_visibility(false, false),
        ..RuntimeState::default()
    };

    let yaml = state.to_yaml().expect("serialize runtime state");

    assert!(yaml.contains("minimap_visible: false"));
    assert!(!yaml.contains("hud_visible"));
}

#[test]
fn minimap_auto_hide_config_defaults_false_and_old_yaml_loads() {
    assert!(!Config::default().overlays.minimap_auto_hide);

    let old = "overlays:\n  minimap_width: 300.0\n  sidebar_auto_hide: true\n";
    let config = Config::from_yaml(old).expect("config without the new key loads");

    assert!(!config.overlays.minimap_auto_hide);
    assert!(config.overlays.sidebar_auto_hide);
}

#[test]
fn minimap_auto_hide_round_trips_through_config_yaml() {
    let mut config = Config::default();
    config.overlays.minimap_auto_hide = true;

    let reloaded = Config::from_yaml(&config.to_yaml().expect("serialize config")).expect("reload config");

    assert!(reloaded.overlays.minimap_auto_hide);
}
