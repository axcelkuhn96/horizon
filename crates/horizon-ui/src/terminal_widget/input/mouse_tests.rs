use super::routing::{
    pointer_button_checks_clickable_target, pointer_button_event_needs_handling, pointer_button_routes_to_pty_mouse,
    pointer_button_starts_local_selection, pointer_drag_updates_local_selection, pointer_motion_routes_to_pty_mouse,
};
use alacritty_terminal::term::TermMode;
use egui::{Modifiers, PointerButton};

use crate::input::{GridPoint, PointerButtons, mouse_button_report};

#[test]
fn plain_primary_click_and_drag_go_to_the_app_in_mouse_mode() {
    let buttons = PointerButtons {
        primary: true,
        middle: false,
        secondary: false,
    };
    let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG;

    assert!(!pointer_button_starts_local_selection(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::NONE
    ));
    assert!(!pointer_drag_updates_local_selection(mode, buttons, Modifiers::NONE));
    for pressed in [true, false] {
        assert!(pointer_button_routes_to_pty_mouse(
            mode,
            PointerButton::Primary,
            Modifiers::NONE
        ));
        assert!(pointer_button_event_needs_handling(
            mode,
            PointerButton::Primary,
            pressed,
            Modifiers::NONE
        ));
    }
    assert!(pointer_motion_routes_to_pty_mouse(mode, buttons, Modifiers::NONE));
}

#[test]
fn plain_primary_selects_locally_when_the_app_has_no_mouse_tracking() {
    let mode = TermMode::ALT_SCREEN;

    assert!(pointer_button_starts_local_selection(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::NONE
    ));
    assert!(!pointer_button_routes_to_pty_mouse(
        mode,
        PointerButton::Primary,
        Modifiers::NONE
    ));
}

#[test]
fn a_left_click_is_reported_as_sgr_press_then_release() {
    let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
    let point = GridPoint { line: 4, column: 9 };

    let press = mouse_button_report(PointerButton::Primary, true, Modifiers::NONE, mode, point);
    let release = mouse_button_report(PointerButton::Primary, false, Modifiers::NONE, mode, point);

    assert_eq!(press.as_deref(), Some(&b"\x1b[<0;10;5M"[..]));
    assert_eq!(release.as_deref(), Some(&b"\x1b[<0;10;5m"[..]));
    assert_eq!(
        mouse_button_report(PointerButton::Primary, true, Modifiers::SHIFT, mode, point),
        None,
        "Shift keeps the click for local selection"
    );
}

#[test]
fn shift_primary_drag_selects_locally_in_mouse_mode() {
    let buttons = PointerButtons {
        primary: true,
        middle: false,
        secondary: false,
    };
    let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG;

    assert!(pointer_button_starts_local_selection(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::SHIFT
    ));
    assert!(pointer_drag_updates_local_selection(mode, buttons, Modifiers::SHIFT));
    assert!(!pointer_button_routes_to_pty_mouse(
        mode,
        PointerButton::Primary,
        Modifiers::SHIFT
    ));
    assert!(!pointer_motion_routes_to_pty_mouse(mode, buttons, Modifiers::SHIFT));
}

#[test]
fn ctrl_or_cmd_primary_click_still_checks_clickable_targets() {
    let mode = TermMode::MOUSE_REPORT_CLICK;

    assert!(pointer_button_checks_clickable_target(
        PointerButton::Primary,
        true,
        Modifiers::CTRL
    ));
    assert!(pointer_button_checks_clickable_target(
        PointerButton::Primary,
        true,
        Modifiers::COMMAND
    ));
    assert!(!pointer_button_starts_local_selection(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::CTRL
    ));
    assert!(!pointer_button_starts_local_selection(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::COMMAND
    ));
    assert!(pointer_button_event_needs_handling(
        mode,
        PointerButton::Primary,
        true,
        Modifiers::CTRL
    ));
}

#[test]
fn non_selection_mouse_reporting_remains_available() {
    let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION;
    let secondary_drag = PointerButtons {
        primary: false,
        middle: false,
        secondary: true,
    };

    assert!(pointer_button_routes_to_pty_mouse(
        mode,
        PointerButton::Secondary,
        Modifiers::NONE
    ));
    assert!(pointer_button_routes_to_pty_mouse(
        mode,
        PointerButton::Primary,
        Modifiers::ALT
    ));
    assert!(pointer_motion_routes_to_pty_mouse(
        mode,
        secondary_drag,
        Modifiers::NONE
    ));
    assert!(pointer_motion_routes_to_pty_mouse(
        mode,
        PointerButtons::default(),
        Modifiers::NONE
    ));
}
