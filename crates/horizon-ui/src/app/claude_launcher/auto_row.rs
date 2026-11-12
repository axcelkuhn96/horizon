//! The "Auto" row at the top of the launcher: same geometry as a profile row, a star instead of
//! the accent dot, `0` as quick-pick key. It is not part of `rows`, so profiles keep `1`..`9`
//! and their indices; the selection is `auto_selected` on top of `selected`.
//! Visual spec: `docs/design/claude-hub-ui.md` §12.

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType};

use super::{
    CachedGalley, ClaudeLauncherState, ROW_HEIGHT, ROW_PADDING_X, ROW_RADIUS, cached_elided, layout_key, move_selection,
};
use crate::app::claude_auto::{AUTO_NAME, AutoChoice};
use crate::app::hub_ui;
use crate::theme;

/// Fades a disabled row (spec: alpha 0.55).
const DISABLED_OPACITY: f32 = 0.55;
const STAR_RADIUS: f32 = 5.0;

/// Text of the Auto row, built with the other rows and rebuilt with them.
pub(super) struct AutoRow {
    pub(super) choice: AutoChoice,
    detail_galley: Option<CachedGalley>,
}

impl AutoRow {
    pub(super) fn new(choice: AutoChoice) -> Self {
        Self {
            choice,
            detail_galley: None,
        }
    }

    pub(super) fn available(&self) -> bool {
        self.choice.available()
    }
}

impl ClaudeLauncherState {
    /// Whether the Auto row is the current selection (and exists).
    pub(super) fn auto_row_selected(&self) -> bool {
        self.auto.is_some() && self.auto_selected
    }

    /// Arrow-key step over `[Auto] + profiles`, wrapping at both ends.
    pub(super) fn step_selection(&mut self, delta: i32) {
        let offset = usize::from(self.auto.is_some());
        let at = if self.auto_row_selected() {
            0
        } else {
            self.selected + offset
        };
        let next = move_selection(at, delta, self.rows.len() + offset);
        self.auto_selected = offset == 1 && next == 0;
        if !self.auto_selected {
            self.selected = next - offset;
        }
    }

    /// Home: the first entry (Auto when listed).
    pub(super) fn select_first(&mut self) {
        self.auto_selected = self.auto.is_some();
        self.selected = 0;
    }

    /// End: the last profile.
    pub(super) fn select_last(&mut self) {
        self.auto_selected = false;
        self.selected = self.rows.len().saturating_sub(1);
    }
}

fn paint_star(painter: &egui::Painter, center: Pos2, color: egui::Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    for angle_deg in [0.0_f32, 60.0, 120.0] {
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let offset = Vec2::new(cos, sin) * STAR_RADIUS;
        painter.line_segment([center - offset, center + offset], stroke);
    }
}

/// Draws and senses the Auto row; returns whether it was clicked (never while disabled).
pub(super) fn show_auto_row(
    ui: &mut Ui,
    auto: &mut AutoRow,
    selected: &mut bool,
    reveal_selected: &mut bool,
    pointer_moved: bool,
) -> bool {
    let available = auto.available();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            available,
            format!("Open Claude session with {AUTO_NAME}, {}", auto.choice.detail),
        )
    });
    if response.hovered() && available {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if pointer_moved {
            *selected = true;
        }
    }
    if *reveal_selected && *selected {
        ui.scroll_to_rect(rect, None);
        *reveal_selected = false;
    }
    let clicked = available && response.clicked();
    if ui.is_rect_visible(rect) {
        let pressed = available && response.is_pointer_button_down_on();
        paint_auto_row(ui.painter(), rect, auto, *selected, pressed);
        hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
    }
    if response.hovered() {
        let _ = response.on_hover_text(auto.choice.tooltip.as_str());
    }
    clicked
}

fn paint_auto_row(painter: &egui::Painter, rect: Rect, auto: &mut AutoRow, selected: bool, pressed: bool) {
    let available = auto.available();
    let mut painter = painter.clone();
    if !available {
        painter.multiply_opacity(DISABLED_OPACITY);
    }
    let accent = theme::ACCENT();
    if selected {
        let tint = if pressed { 0.30 } else { 0.22 };
        painter.rect(
            rect,
            CornerRadius::same(ROW_RADIUS),
            theme::blend(theme::PANEL_BG_ALT(), accent, tint),
            Stroke::new(1.0_f32, theme::alpha(accent, 150)),
            StrokeKind::Inside,
        );
        let bar = Rect::from_center_size(Pos2::new(rect.left() + 4.5, rect.center().y), Vec2::new(3.0, 28.0));
        painter.rect_filled(bar, CornerRadius::same(2), accent);
    }
    let left = rect.left() + ROW_PADDING_X;
    if available {
        let badge = Rect::from_min_size(Pos2::new(left, rect.center().y - 10.0), Vec2::splat(20.0));
        hub_ui::key_badge(&painter, badge, 0, selected, accent);
    }
    let star_x = left + 20.0 + 10.0 + 4.0;
    paint_star(&painter, Pos2::new(star_x, rect.center().y), accent);

    let text_left = star_x + 4.0 + 8.0;
    let text_width = (rect.right() - ROW_PADDING_X - text_left).max(0.0);
    painter.text(
        Pos2::new(text_left, rect.top() + 10.0),
        Align2::LEFT_TOP,
        AUTO_NAME,
        FontId::proportional(13.0),
        theme::FG(),
    );
    let detail = cached_elided(
        &mut auto.detail_galley,
        layout_key(&painter, text_width),
        &painter,
        &auto.choice.detail,
        FontId::proportional(10.5),
        text_width,
    );
    let color = if auto.choice.low {
        theme::PALETTE_YELLOW()
    } else {
        theme::FG_DIM()
    };
    painter.galley(Pos2::new(text_left, rect.top() + 29.0), detail, color);
}
