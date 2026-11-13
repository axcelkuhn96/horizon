use alacritty_terminal::term::TermMode;

use crate::input;

pub(crate) fn pty_mouse_reporting_enabled(terminal_mode: TermMode, modifiers: egui::Modifiers) -> bool {
    !modifiers.shift && terminal_mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)
}

pub(super) fn pointer_button_checks_clickable_target(
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> bool {
    (modifiers.ctrl || modifiers.command) && button == egui::PointerButton::Primary && pressed
}

pub(super) fn pointer_button_uses_local_selection(
    terminal_mode: TermMode,
    button: egui::PointerButton,
    modifiers: egui::Modifiers,
) -> bool {
    // With mouse tracking on, the app owns the left button; Shift is the escape hatch for
    // selecting text locally (as in other terminals).
    button == egui::PointerButton::Primary && !pty_mouse_reporting_enabled(terminal_mode, modifiers)
}

pub(super) fn pointer_button_starts_local_selection(
    terminal_mode: TermMode,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> bool {
    pressed && pointer_button_uses_local_selection(terminal_mode, button, modifiers)
}

pub(super) fn pointer_drag_updates_local_selection(
    terminal_mode: TermMode,
    buttons: input::PointerButtons,
    modifiers: egui::Modifiers,
) -> bool {
    buttons.primary && !pty_mouse_reporting_enabled(terminal_mode, modifiers)
}

pub(super) fn pointer_button_routes_to_pty_mouse(
    terminal_mode: TermMode,
    button: egui::PointerButton,
    modifiers: egui::Modifiers,
) -> bool {
    pty_mouse_reporting_enabled(terminal_mode, modifiers)
        && !pointer_button_uses_local_selection(terminal_mode, button, modifiers)
}

pub(super) fn pointer_button_event_needs_handling(
    terminal_mode: TermMode,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> bool {
    pointer_button_checks_clickable_target(button, pressed, modifiers)
        || pointer_button_routes_to_pty_mouse(terminal_mode, button, modifiers)
}

pub(super) fn pointer_motion_routes_to_pty_mouse(
    terminal_mode: TermMode,
    buttons: input::PointerButtons,
    modifiers: egui::Modifiers,
) -> bool {
    pty_mouse_reporting_enabled(terminal_mode, modifiers)
        && !pointer_drag_updates_local_selection(terminal_mode, buttons, modifiers)
}
