//! Painted widgets for the Workspaces tab (spec: `docs/design/claude-hub-ui.md` §4).

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Modifiers, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2, text::LayoutJob,
};
use horizon_core::{PanelKind, TerminalConfig};

use crate::app::hub_ui::{self, IconKind};
use crate::theme;

const ROW_RADIUS: u8 = 8;
const ROW_HEIGHT: f32 = 36.0;
const ACCENT_BAR_WIDTH: f32 = 3.0;
const KIND_ICON_SIZE: f32 = 16.0;
const MINI_ARROW_SIZE: f32 = 16.0;
/// Below this width the cwd field moves to its own line inside a terminal row.
const MIN_INLINE_CWD_WIDTH: f32 = 90.0;

fn ellipsized(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, max_width.max(1.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(job)
}

fn paint_mini_arrow(painter: &Painter, rect: Rect, up: bool, enabled: bool, hovered: bool) {
    let color = if !enabled {
        theme::alpha(theme::FG_DIM(), 90)
    } else if hovered {
        theme::FG()
    } else {
        theme::FG_SOFT()
    };
    if hovered && enabled {
        painter.rect_filled(rect, CornerRadius::same(4), theme::PANEL_BG_ALT());
    }
    let c = rect.center();
    let points = if up {
        vec![
            c + Vec2::new(0.0, -2.5),
            c + Vec2::new(4.0, 2.5),
            c + Vec2::new(-4.0, 2.5),
        ]
    } else {
        vec![
            c + Vec2::new(0.0, 2.5),
            c + Vec2::new(-4.0, -2.5),
            c + Vec2::new(4.0, -2.5),
        ]
    };
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// What a click on a template list row asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListRowAction {
    None,
    Select,
    MoveUp,
    MoveDown,
}

/// Template list row: swatch + ellipsized name, mini reorder arrows while hot or selected.
pub(super) fn template_list_row(
    ui: &mut Ui,
    name: &str,
    color: Color32,
    selected: bool,
    can_up: bool,
    can_down: bool,
) -> ListRowAction {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, name));
    let hot = ui.rect_contains_pointer(rect);

    let arrow_up = Rect::from_center_size(
        Pos2::new(rect.right() - 8.0 - MINI_ARROW_SIZE * 1.5 - 2.0, rect.center().y),
        Vec2::splat(MINI_ARROW_SIZE),
    );
    let arrow_down = arrow_up.translate(Vec2::new(MINI_ARROW_SIZE + 4.0, 0.0));
    let show_arrows = hot || selected;
    let mut action = if response.clicked() {
        ListRowAction::Select
    } else {
        ListRowAction::None
    };
    let mut arrow_hover = (false, false);
    if show_arrows {
        let up = ui.interact(arrow_up, response.id.with("up"), Sense::click());
        let down = ui.interact(arrow_down, response.id.with("down"), Sense::click());
        arrow_hover = (up.hovered(), down.hovered());
        if up.clicked() && can_up {
            action = ListRowAction::MoveUp;
        } else if down.clicked() && can_down {
            action = ListRowAction::MoveDown;
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            theme::blend(theme::PANEL_BG_ALT(), color, 0.22)
        } else if hot {
            theme::blend(theme::PANEL_BG_ALT(), color, 0.10)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, CornerRadius::same(ROW_RADIUS), fill);
        if selected {
            painter.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(ACCENT_BAR_WIDTH, rect.height())),
                CornerRadius {
                    nw: ROW_RADIUS,
                    sw: ROW_RADIUS,
                    ne: 0,
                    se: 0,
                },
                color,
            );
        }
        let swatch = Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.center().y), Vec2::splat(10.0));
        painter.rect_filled(swatch, CornerRadius::same(3), color);

        let text_left = rect.left() + 32.0;
        let text_right = if show_arrows {
            arrow_up.left() - 6.0
        } else {
            rect.right() - 10.0
        };
        let text_color = if selected { theme::FG() } else { theme::FG_SOFT() };
        let galley = ellipsized(
            painter,
            name,
            FontId::proportional(12.0),
            text_color,
            text_right - text_left,
        );
        let pos = Pos2::new(text_left, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley, text_color);

        if show_arrows {
            paint_mini_arrow(painter, arrow_up, true, can_up, arrow_hover.0);
            paint_mini_arrow(painter, arrow_down, false, can_down, arrow_hover.1);
        }
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    action
}

/// Destructive button: transparent fill, `BTN_CLOSE` text and a 120-alpha stroke.
pub(super) fn danger_button(ui: &mut Ui, label: &str) -> Response {
    let font = FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new((text_width + 32.0).max(64.0), 28.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let hot = response.hovered() || response.is_pointer_button_down_on();
        let painter = ui.painter();
        let fill = if hot {
            theme::alpha(theme::BTN_CLOSE(), 28)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect(
            rect,
            CornerRadius::same(ROW_RADIUS),
            fill,
            Stroke::new(1.0_f32, theme::alpha(theme::BTN_CLOSE(), 120)),
            StrokeKind::Inside,
        );
        painter.text(rect.center(), Align2::CENTER_CENTER, label, font, theme::BTN_CLOSE());
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    response
}

/// Kind icon, 16x16: shell = chevron + underscore, claude = big dot, ssh = two bars,
/// other agents = diamond, remaining panel kinds = rounded square.
pub(super) fn paint_kind_icon(painter: &Painter, center: Pos2, kind: PanelKind, accent: Color32) {
    let stroke = Stroke::new(1.5_f32, theme::FG_SOFT());
    match kind {
        PanelKind::Shell | PanelKind::Command => {
            painter.line_segment([center + Vec2::new(-5.0, -3.5), center + Vec2::new(-1.0, 0.0)], stroke);
            painter.line_segment([center + Vec2::new(-1.0, 0.0), center + Vec2::new(-5.0, 3.5)], stroke);
            painter.line_segment([center + Vec2::new(1.5, 4.0), center + Vec2::new(6.0, 4.0)], stroke);
        }
        PanelKind::Claude => {
            painter.circle_filled(center, 5.0, accent);
        }
        PanelKind::Ssh => {
            painter.line_segment([center + Vec2::new(-5.0, -3.0), center + Vec2::new(5.0, -3.0)], stroke);
            painter.line_segment([center + Vec2::new(-5.0, 3.0), center + Vec2::new(5.0, 3.0)], stroke);
        }
        PanelKind::Codex | PanelKind::OpenCode | PanelKind::Gemini | PanelKind::KiloCode | PanelKind::Pi => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + Vec2::new(0.0, -6.0),
                    center + Vec2::new(6.0, 0.0),
                    center + Vec2::new(0.0, 6.0),
                    center + Vec2::new(-6.0, 0.0),
                ],
                accent,
                Stroke::NONE,
            ));
        }
        PanelKind::Editor | PanelKind::GitChanges | PanelKind::FileExplorer | PanelKind::Usage => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::splat(11.0)),
                CornerRadius::same(2),
                stroke,
                StrokeKind::Inside,
            );
        }
    }
}

/// Accent bar color for a terminal row: the profile accent for Claude, dim for
/// shells, `ACCENT` for everything else.
pub(super) fn kind_bar_color(kind: PanelKind, profile_accent: Option<Color32>) -> Color32 {
    match kind {
        PanelKind::Claude => profile_accent.unwrap_or_else(theme::ACCENT),
        PanelKind::Shell => theme::FG_DIM(),
        _ => theme::ACCENT(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalRowAction {
    None,
    MoveUp,
    MoveDown,
    Remove,
}

pub(super) struct TerminalRowOutcome {
    pub(super) changed: bool,
    pub(super) action: TerminalRowAction,
}

/// One terminal row: kind icon, inline name, read-only kind, optional cwd and ▲▼✕.
/// When the row is too narrow for an inline cwd field it wraps onto a second line.
pub(super) fn terminal_row(
    ui: &mut Ui,
    terminal: &mut TerminalConfig,
    bar_color: Color32,
    icon_accent: Color32,
    is_first: bool,
    is_last: bool,
) -> TerminalRowOutcome {
    let mut outcome = TerminalRowOutcome {
        changed: false,
        action: TerminalRowAction::None,
    };
    let kind_label = terminal.kind.display_name();
    let kind_width = ui
        .painter()
        .layout_no_wrap(kind_label.to_owned(), FontId::proportional(10.5), Color32::WHITE)
        .size()
        .x;
    let spacing = 6.0;
    let buttons_width = 3.0 * 26.0 + 2.0 * 4.0;
    let margin = Margin {
        left: 14,
        right: 6,
        top: 5,
        bottom: 5,
    };
    let inner_width = ui.available_width() - f32::from(margin.left) - f32::from(margin.right);
    let fixed = KIND_ICON_SIZE + kind_width + buttons_width + spacing * 4.0;
    let flexible = (inner_width - fixed).max(0.0);
    let inline_cwd = flexible * 0.55 >= MIN_INLINE_CWD_WIDTH;
    let name_width = if inline_cwd {
        (flexible * 0.45).max(80.0)
    } else {
        flexible.max(80.0)
    };
    let cwd_width = if inline_cwd {
        (flexible - name_width - spacing).max(MIN_INLINE_CWD_WIDTH)
    } else {
        inner_width
    };

    let frame = Frame::new()
        .fill(theme::blend(theme::PANEL_BG_ALT(), theme::PANEL_BG(), 0.5))
        .corner_radius(CornerRadius::same(ROW_RADIUS))
        .inner_margin(margin);
    let shown = frame.show(ui, |ui| {
        ui.set_min_width(inner_width.max(0.0));
        ui.spacing_mut().item_spacing = Vec2::new(spacing, 4.0);
        ui.horizontal(|ui| {
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(KIND_ICON_SIZE), Sense::hover());
            paint_kind_icon(ui.painter(), icon_rect.center(), terminal.kind, icon_accent);

            let mut name = terminal.name.clone();
            let name_response = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .desired_width(name_width)
                    .hint_text("Name")
                    .font(FontId::proportional(12.0))
                    .margin(Margin::symmetric(6, 4)),
            );
            if name_response.changed() {
                terminal.name = name;
                outcome.changed = true;
            }

            ui.label(egui::RichText::new(kind_label).size(10.5).color(theme::FG_DIM()));

            if inline_cwd {
                outcome.changed |= cwd_field(ui, terminal, cwd_width);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if hub_ui::icon_button(ui, IconKind::Remove, "Remove terminal").clicked() {
                    outcome.action = TerminalRowAction::Remove;
                }
                if hub_ui::icon_button_enabled(ui, IconKind::Down, "Move down", !is_last).clicked() {
                    outcome.action = TerminalRowAction::MoveDown;
                }
                if hub_ui::icon_button_enabled(ui, IconKind::Up, "Move up", !is_first).clicked() {
                    outcome.action = TerminalRowAction::MoveUp;
                }
            });
        });
        if !inline_cwd {
            outcome.changed |= cwd_field(ui, terminal, cwd_width);
        }
    });

    let rect = shown.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(ACCENT_BAR_WIDTH, rect.height())),
        CornerRadius {
            nw: ROW_RADIUS,
            sw: ROW_RADIUS,
            ne: 0,
            se: 0,
        },
        bar_color,
    );
    outcome
}

fn cwd_field(ui: &mut Ui, terminal: &mut TerminalConfig, width: f32) -> bool {
    let mut cwd = terminal.cwd.clone().unwrap_or_default();
    let response = ui.add(
        egui::TextEdit::singleline(&mut cwd)
            .desired_width(width)
            .hint_text("inherit")
            .font(FontId::monospace(11.0))
            .margin(Margin::symmetric(6, 4)),
    );
    if response.changed() {
        terminal.cwd = if cwd.trim().is_empty() { None } else { Some(cwd) };
        return true;
    }
    false
}

/// Runs `add` with the widgets' strokes tinted red when `invalid`.
pub(super) fn error_scope<R>(ui: &mut Ui, invalid: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        if invalid {
            let red = Stroke::new(1.0_f32, theme::PALETTE_RED());
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_stroke = red;
            visuals.widgets.hovered.bg_stroke = red;
            visuals.selection.stroke = red;
        }
        add(ui)
    })
    .inner
}

/// Selection keys for the add-terminal list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListKey {
    None,
    Up,
    Down,
    Confirm,
    Cancel,
}

/// Reads (and consumes) list navigation keys unless a text field has focus.
pub(super) fn read_list_key(ui: &Ui) -> ListKey {
    if ui.ctx().wants_keyboard_input() {
        return ListKey::None;
    }
    ui.input_mut(|input| {
        if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
            ListKey::Down
        } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
            ListKey::Up
        } else if input.consume_key(Modifiers::NONE, Key::Enter) {
            ListKey::Confirm
        } else if input.consume_key(Modifiers::NONE, Key::Escape) {
            ListKey::Cancel
        } else {
            ListKey::None
        }
    })
}

/// Row in the add-terminal list (30 high): kind icon or profile dot + name.
pub(super) fn preset_row(
    ui: &mut Ui,
    name: &str,
    kind: PanelKind,
    profile_accent: Option<Color32>,
    highlighted: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() || highlighted {
            painter.rect_filled(
                rect,
                CornerRadius::same(ROW_RADIUS),
                theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.18),
            );
        }
        let icon_center = Pos2::new(rect.left() + 18.0, rect.center().y);
        match profile_accent {
            Some(accent) => {
                painter.circle_filled(icon_center, 4.0, accent);
            }
            None => paint_kind_icon(painter, icon_center, kind, theme::ACCENT()),
        }
        let galley = ellipsized(
            painter,
            name,
            FontId::proportional(12.0),
            theme::FG(),
            rect.right() - rect.left() - 40.0,
        );
        let pos = Pos2::new(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley, theme::FG());
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    response
}

/// Full-width secondary button (28 high), same look as `hub_ui::secondary_button`.
pub(super) fn wide_secondary_button(ui: &mut Ui, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let hot = response.hovered() || response.is_pointer_button_down_on();
        let (fill, stroke) = if hot {
            let visuals = ui.visuals().widgets.hovered;
            (visuals.weak_bg_fill, visuals.bg_stroke)
        } else {
            (theme::PANEL_BG_ALT(), Stroke::new(1.0_f32, theme::BORDER_SUBTLE()))
        };
        let painter = ui.painter();
        painter.rect(rect, CornerRadius::same(ROW_RADIUS), fill, stroke, StrokeKind::Inside);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            theme::FG(),
        );
        hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
    }
    response
}
