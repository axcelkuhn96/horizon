//! Directory browser modal that fills one profile's `config_dir`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use egui::{Context, CornerRadius, FontId, Pos2, Sense, Ui, Vec2};
use horizon_core::{ClaudeProfile, dir_search};

use crate::dir_picker::{PickerEmptyState, PickerModalAction, PickerModalConfig, PickerModalState, split_path_display};
use crate::theme;

const DEBOUNCE_MS: u128 = 60;
const BROWSE_ROW_HEIGHT: f32 = 34.0;

/// Local directory search modal that fills one profile's `config_dir`.
pub(super) struct BrowseState {
    target: usize,
    modal: PickerModalState,
    results: Vec<PathBuf>,
    search_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
    last_query_sent: String,
    last_query_time: Instant,
    loaded: bool,
}

enum BrowseOutcome {
    Open,
    Cancelled,
    Picked(PathBuf),
}

impl BrowseState {
    pub(super) fn new(target: usize, current_dir: &str) -> Self {
        let expanded = dir_search::expand_tilde(current_dir.trim());
        let query = if !current_dir.trim().is_empty() && expanded.is_dir() {
            format!("{}/", dir_search::abbreviate_home(&expanded).trim_end_matches('/'))
        } else {
            "~/".to_string()
        };
        Self {
            target,
            search_rx: Some(dir_search::spawn_lookup(query.clone())),
            modal: PickerModalState::new(query.clone()),
            results: Vec::new(),
            last_query_sent: query,
            last_query_time: Instant::now(),
            loaded: false,
        }
    }

    fn update_search(&mut self) {
        if let Some(rx) = &self.search_rx {
            match rx.try_recv() {
                Ok(results) => {
                    self.results = results;
                    self.modal.clamp_selected(self.results.len());
                    self.search_rx = None;
                    self.loaded = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.search_rx = None,
            }
        }
        if self.modal.query() != self.last_query_sent && self.last_query_time.elapsed().as_millis() >= DEBOUNCE_MS {
            self.last_query_sent = self.modal.query().to_string();
            self.last_query_time = Instant::now();
            self.search_rx = Some(dir_search::spawn_lookup(self.last_query_sent.clone()));
        }
    }

    fn show(&mut self, ctx: &Context) -> BrowseOutcome {
        self.update_search();
        let message = if self.loaded {
            "No directories found"
        } else {
            "Searching..."
        };
        let action = self.modal.show(
            ctx,
            &PickerModalConfig {
                id_source: "claude_profile_dir_picker",
                heading: "Select Claude config directory",
                hint_text: "Type a path or search...",
                status_text: None,
                empty_state: PickerEmptyState {
                    message,
                    color: theme::FG_DIM(),
                },
                footer_action_label: None,
            },
            &self.results,
            |ui, width, index, path, selected| browse_row(ui, width, index, path, selected),
        );
        match action {
            PickerModalAction::None | PickerModalAction::FooterAction => BrowseOutcome::Open,
            PickerModalAction::Cancelled => BrowseOutcome::Cancelled,
            PickerModalAction::ClickedRow(index) => self
                .results
                .get(index)
                .map_or(BrowseOutcome::Open, |path| BrowseOutcome::Picked(path.clone())),
            PickerModalAction::CompleteSelection => {
                if let Some(path) = self.results.get(self.modal.selected_index()) {
                    self.modal
                        .set_query(format!("{}/", dir_search::abbreviate_home(path).trim_end_matches('/')));
                    self.last_query_time = Instant::now();
                }
                BrowseOutcome::Open
            }
            PickerModalAction::Submit => self.submit(),
        }
    }

    fn submit(&self) -> BrowseOutcome {
        let typed = dir_search::expand_tilde(self.modal.query().trim());
        if !self.modal.query().trim().is_empty() && typed.is_dir() {
            return BrowseOutcome::Picked(typed);
        }
        self.results
            .get(self.modal.selected_index())
            .map_or(BrowseOutcome::Open, |path| BrowseOutcome::Picked(path.clone()))
    }
}

fn browse_row(ui: &mut Ui, width: f32, index: usize, path: &Path, selected: bool) -> bool {
    let display = dir_search::abbreviate_home(path);
    let row = ui.allocate_space(Vec2::new(width, BROWSE_ROW_HEIGHT)).1;
    let response = ui.interact(row, ui.make_persistent_id(("claude_dir_row", index)), Sense::click());
    let painter = ui.painter_at(row);
    if selected {
        painter.rect_filled(
            row,
            CornerRadius::same(8),
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.28), 200),
        );
    } else if response.hovered() {
        painter.rect_filled(row, CornerRadius::same(8), theme::alpha(theme::PANEL_BG_ALT(), 160));
    }
    let (dir_part, name_part) = split_path_display(&display);
    let font = FontId::monospace(12.5);
    let y = row.center().y;
    let name_color = if selected { theme::FG() } else { theme::FG_SOFT() };
    let mut x = row.min.x + 14.0;
    if !dir_part.is_empty() {
        x = painter
            .text(
                Pos2::new(x, y),
                egui::Align2::LEFT_CENTER,
                &dir_part,
                font.clone(),
                theme::FG_DIM(),
            )
            .max
            .x;
    }
    painter.text(Pos2::new(x, y), egui::Align2::LEFT_CENTER, &name_part, font, name_color);
    response.clicked()
}

pub(super) fn render_browse(ctx: &Context, profiles: &mut [ClaudeProfile], slot: &mut Option<BrowseState>) -> bool {
    let Some(browse) = slot.as_mut() else {
        return false;
    };
    match browse.show(ctx) {
        BrowseOutcome::Open => false,
        BrowseOutcome::Cancelled => {
            *slot = None;
            false
        }
        BrowseOutcome::Picked(path) => {
            let target = browse.target;
            *slot = None;
            profiles.get_mut(target).is_some_and(|profile| {
                profile.config_dir = dir_search::abbreviate_home(&path);
                true
            })
        }
    }
}
