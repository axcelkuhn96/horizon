//! Settings "Workspaces" tab (wired in Task 11).

use super::SettingsEditor;
use crate::theme;

#[derive(Default)]
pub(super) struct WorkspacesTabState {}

pub(super) fn show(ui: &mut egui::Ui, _editor: &mut SettingsEditor) {
    ui.label(egui::RichText::new("Coming soon").color(theme::FG_DIM()).size(12.0));
}
