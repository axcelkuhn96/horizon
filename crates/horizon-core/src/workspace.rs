use std::path::PathBuf;

use crate::board::WorkspaceLayout;
use crate::panel::PanelId;
use crate::runtime_state::{WorkspaceTemplateRef, new_local_id};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WorkspaceId(pub u64);

/// A visual workspace cluster of terminal panels on the canvas.
/// Workspaces are always visible — no tabs, no hidden state.
pub struct Workspace {
    pub id: WorkspaceId,
    pub local_id: String,
    pub name: String,
    pub color_idx: usize,
    pub panels: Vec<PanelId>,
    pub collapsed: bool,
    /// Whether this workspace's terminal list is folded in the sidebar.
    /// Sidebar-only affordance: it does NOT collapse the canvas panels
    /// (that is `Panel.collapsed`) nor affect the terminals themselves.
    pub sidebar_collapsed: bool,
    /// Canvas position (top-left) of the workspace badge.
    pub position: [f32; 2],
    /// Default working directory for new terminals in this workspace.
    pub cwd: Option<PathBuf>,
    pub template: Option<WorkspaceTemplateRef>,
    pub layout: Option<WorkspaceLayout>,
}

/// Predefined accent colors for workspace clusters.
pub const WORKSPACE_COLORS: &[(u8, u8, u8)] = &[
    (137, 180, 250), // blue
    (166, 227, 161), // green
    (249, 226, 175), // yellow
    (243, 139, 168), // red
    (245, 194, 231), // pink
    (148, 226, 213), // teal
    (203, 166, 247), // mauve
    (250, 179, 135), // peach
];

/// Saved color format for workspace templates: lowercase `#rrggbb` of the
/// [`WORKSPACE_COLORS`] entry at `index` (wrapping).
#[must_use]
pub fn workspace_color_hex(index: usize) -> String {
    let (r, g, b) = WORKSPACE_COLORS[index % WORKSPACE_COLORS.len()];
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Palette slot of a saved `#rrggbb` color (case-insensitive), if it is an
/// entry of [`WORKSPACE_COLORS`].
#[must_use]
pub fn workspace_color_index(color: &str) -> Option<usize> {
    let wanted = color.trim().to_ascii_lowercase();
    (0..WORKSPACE_COLORS.len()).find(|index| workspace_color_hex(*index) == wanted)
}

impl Workspace {
    #[must_use]
    pub fn new(id: WorkspaceId, name: String, color_idx: usize) -> Self {
        Self {
            id,
            local_id: new_local_id(),
            name,
            color_idx,
            panels: Vec::new(),
            collapsed: false,
            sidebar_collapsed: false,
            position: [0.0, 0.0],
            cwd: None,
            template: None,
            layout: Some(WorkspaceLayout::default()),
        }
    }

    #[must_use]
    pub fn accent(&self) -> (u8, u8, u8) {
        WORKSPACE_COLORS[self.color_idx % WORKSPACE_COLORS.len()]
    }

    pub fn add_panel(&mut self, panel_id: PanelId) {
        if !self.panels.contains(&panel_id) {
            self.panels.push(panel_id);
        }
    }

    #[must_use]
    pub fn panel_index(&self, panel_id: PanelId) -> Option<usize> {
        self.panels.iter().position(|id| *id == panel_id)
    }

    pub fn remove_panel(&mut self, panel_id: PanelId) {
        self.panels.retain(|&id| id != panel_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_color_hex_round_trips_through_palette() {
        for index in 0..WORKSPACE_COLORS.len() {
            let hex = workspace_color_hex(index);
            assert_eq!(workspace_color_index(&hex), Some(index));
            assert_eq!(workspace_color_index(&hex.to_uppercase()), Some(index));
            assert_eq!(workspace_color_index(&format!("  {hex} ")), Some(index));
        }
        assert_eq!(workspace_color_hex(0), "#89b4fa");
        assert_eq!(workspace_color_index("#123456"), None);
        assert_eq!(workspace_color_index(""), None);
    }
}
