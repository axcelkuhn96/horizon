//! Sidebar context-menu rendering helpers.
//!
//! The workspace sidebar is painted in an [`egui::Area`] at
//! [`egui::Order::Tooltip`] so it stays above the canvas panels (which render in
//! [`egui::Order::Foreground`]). egui's built-in [`egui::Response::context_menu`]
//! opens its popup in [`egui::Order::Foreground`], which is *below* `Tooltip`, so
//! a context menu opened from a sidebar row paints behind the sidebar and its
//! options are invisible/unclickable.
//!
//! [`context_menu_above_sidebar`] reproduces `Response::context_menu` using the
//! [`egui::Popup`] API but forces the popup into [`egui::PopupKind::Tooltip`] so
//! it renders in the same `Tooltip` layer as the sidebar. Because the popup
//! `Area` is created during the sidebar render (after the sidebar frame), it is
//! ordered on top within that layer and is therefore fully visible and
//! clickable.

use egui::{Popup, PopupKind, Response, Ui};

/// Show a context menu (on secondary click of `response`) that paints *above*
/// the `Order::Tooltip` sidebar, instead of behind it.
///
/// Behaviorally equivalent to [`Response::context_menu`] — opens at the pointer
/// on right-click, closes on click outside or `Esc` — but rendered in the
/// `Tooltip` layer so it is not occluded by the sidebar.
pub(super) fn context_menu_above_sidebar(response: &Response, add_contents: impl FnOnce(&mut Ui)) {
    Popup::context_menu(response)
        .kind(PopupKind::Tooltip)
        .show(add_contents);
}
