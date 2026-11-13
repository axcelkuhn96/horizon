//! Painting of the usage chips, their hover card and the overflow rows.

use std::sync::Arc;
use std::time::Instant;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::claude_usage::{ProfileUsage, UsageState, UsageWindow, remaining_percent, used_percent_label};

use super::{
    BAR_WIDTH_COMPACT, BAR_WIDTH_FULL, CHIP_GAP, CHIP_HEIGHT, CHIP_PAD, ChipEntry, ChipLayout, ChipStatus, ChipsPlan,
    ClaudeUsageState, DOT_DIAMETER, DOT_GAP, GROUP_WIDTH_FULL, LABEL_GAP, LABEL_WIDTH, PCT_WIDTH, SECTION_GAP,
    ShownWindows, api_severity, chip_label, chip_label_parts, chip_status, severity_for, time,
};
use crate::app::hub_ui::{self, Severity};
use crate::theme;

const CHIP_RADIUS: u8 = 10;
const BAR_HEIGHT: f32 = 4.0;
/// Content alpha factor of an expired or missing login.
const UNUSABLE_FADE: f32 = 0.55;
/// Bars of an unusable login still show the last known value, fainter.
const UNUSABLE_BAR_FADE: f32 = 0.4;
const TOOLTIP_WIDTH: f32 = 280.0;
const TOOLTIP_MARGIN: i8 = 12;
/// Margin the tooltip's own popup frame already adds (`Style::spacing.menu_margin`).
const POPUP_FRAME_MARGIN: i8 = 6;
const TOOLTIP_BAR: Vec2 = Vec2::new(120.0, 5.0);
const OVERFLOW_ROW_HEIGHT: f32 = 26.0;
const OVERFLOW_ROW_MIN_WIDTH: f32 = 260.0;

/// What the user asked for through the chips this frame.
#[derive(Default)]
pub(in crate::app) struct ChipsOutput {
    /// Profile whose session to open (left click).
    pub(in crate::app) launch: Option<String>,
    /// "Refresh now" was chosen in a chip's context menu.
    pub(in crate::app) refresh: bool,
    /// A hover card was open: the "Tokens today" stats poll should be running.
    pub(in crate::app) wants_tokens: bool,
}

/// How one chip is drawn this frame.
#[derive(Clone, Copy)]
struct ChipLook {
    accent: Color32,
    status: ChipStatus,
    compact: bool,
    hovered: bool,
    pressed: bool,
}

impl ChipLook {
    fn content_fade(self) -> f32 {
        if matches!(self.status, ChipStatus::Expired | ChipStatus::Missing) {
            UNUSABLE_FADE
        } else {
            1.0
        }
    }

    fn bar_fade(self) -> f32 {
        if matches!(self.status, ChipStatus::Expired | ChipStatus::Missing) {
            UNUSABLE_BAR_FADE
        } else {
            1.0
        }
    }
}

/// Draws the first `plan.visible` chips side by side and reports clicks.
pub(in crate::app) fn show_chips(
    ui: &mut Ui,
    usage: &ClaudeUsageState,
    plan: ChipsPlan,
    now: Instant,
    now_ms: i64,
) -> ChipsOutput {
    let cache = &usage.cache;
    let widths = if plan.compact {
        &cache.widths.compact
    } else {
        &cache.widths.full
    };
    let total = cache.widths.total(plan.compact, plan.visible);
    let (group, _) = ui.allocate_exact_size(Vec2::new(total, CHIP_HEIGHT), Sense::hover());
    let refreshing = usage.is_refreshing(now);
    let tokens_today = usage.tokens_today();
    let tokens_pending = usage.tokens_scan_pending(now);
    let mut output = ChipsOutput::default();
    let mut x = group.min.x;
    for index in 0..plan.visible {
        let (Some(entry), Some(layout), Some(&width)) =
            (usage.entries().get(index), cache.chips.get(index), widths.get(index))
        else {
            break;
        };
        let rect = Rect::from_min_size(
            Pos2::new(x, group.center().y - CHIP_HEIGHT / 2.0),
            Vec2::new(width, CHIP_HEIGHT),
        );
        x += width + CHIP_GAP;
        let frame = ChipFrame {
            compact: plan.compact,
            refreshing,
            now_ms,
            tokens_today,
            tokens_pending,
        };
        show_chip(ui, rect, entry, layout, frame, &mut output);
    }
    output
}

/// Per-frame inputs shared by every chip.
#[derive(Clone, Copy)]
struct ChipFrame<'a> {
    compact: bool,
    refreshing: bool,
    now_ms: i64,
    tokens_today: Option<&'a str>,
    /// The stats scan may still deliver `tokens_today`: keep repainting while the card is open.
    tokens_pending: bool,
}

fn show_chip(
    ui: &mut Ui,
    rect: Rect,
    entry: &ChipEntry,
    layout: &ChipLayout,
    frame: ChipFrame<'_>,
    output: &mut ChipsOutput,
) {
    let ChipFrame {
        compact,
        refreshing,
        now_ms,
        tokens_today,
        tokens_pending,
    } = frame;
    let status = chip_status(entry.usage.as_ref(), now_ms);
    let accent = hub_ui::accent_color(layout.accent_slot);
    let id = ui.id().with(("usage_chip", &entry.name));
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.widget_info(|| {
        let label = entry
            .usage
            .as_ref()
            .map_or_else(|| entry.name.clone(), |usage| chip_label(usage, compact));
        WidgetInfo::labeled(WidgetType::Button, true, format!("Open Claude session {label}"))
    });
    if ui.is_rect_visible(rect) {
        let look = ChipLook {
            accent,
            status,
            compact,
            hovered: response.hovered(),
            pressed: response.is_pointer_button_down_on(),
        };
        paint_chip(ui.painter(), rect, entry, layout, look);
        hub_ui::paint_focus_ring(ui.painter(), &response, rect, CHIP_RADIUS);
    }
    let response = response.on_hover_ui_at_pointer(|ui| {
        output.wants_tokens = true;
        show_tooltip(ui, entry, accent, status, now_ms, tokens_today, tokens_pending);
    });
    if response.clicked() {
        output.launch = Some(entry.name.clone());
    }
    response.context_menu(|ui| {
        if refreshing {
            ui.add_enabled(false, egui::Button::new("Refreshing\u{2026}"));
        } else if ui.button("Refresh now").clicked() {
            output.refresh = true;
            ui.close();
        }
    });
}

fn paint_chip(painter: &egui::Painter, rect: Rect, entry: &ChipEntry, layout: &ChipLayout, look: ChipLook) {
    let accent = look.accent;
    let (fill, stroke) = if look.pressed {
        (
            theme::blend(theme::PANEL_BG_ALT(), accent, 0.30),
            theme::blend(theme::BORDER_STRONG(), accent, 0.45),
        )
    } else if look.hovered {
        (
            theme::blend(theme::PANEL_BG_ALT(), accent, 0.20),
            theme::blend(theme::BORDER_STRONG(), accent, 0.45),
        )
    } else {
        (
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), accent, 0.10), 232),
            theme::alpha(theme::blend(theme::BORDER_SUBTLE(), accent, 0.36), 220),
        )
    };
    let radius = CornerRadius::same(CHIP_RADIUS);
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, stroke), StrokeKind::Outside);

    let fade = look.content_fade();
    let cy = rect.center().y;
    let mut x = rect.min.x + CHIP_PAD;
    let dot = Pos2::new(x + DOT_DIAMETER / 2.0, cy);
    if look.status == ChipStatus::Stale {
        painter.circle_stroke(
            dot,
            DOT_DIAMETER / 2.0,
            Stroke::new(1.2_f32, accent.gamma_multiply(fade)),
        );
    } else {
        painter.circle_filled(dot, DOT_DIAMETER / 2.0, accent.gamma_multiply(fade));
    }
    x += DOT_DIAMETER + DOT_GAP;

    let name = layout.name(look.compact);
    painter.galley(
        Pos2::new(x, cy - name.size().y / 2.0),
        Arc::clone(name),
        theme::FG().gamma_multiply(fade),
    );
    x += name.size().x;

    let usage = entry.usage.as_ref();
    let shown = ShownWindows::of(usage);
    if !look.compact && shown.five {
        x += SECTION_GAP;
        paint_group(painter, Pos2::new(x, cy), layout, usage, true, look);
        x += GROUP_WIDTH_FULL;
    }
    match (entry.pill_text(), layout.pill_width) {
        (Some(text), Some(width)) => {
            x += SECTION_GAP;
            let color = if look.status == ChipStatus::Expired {
                theme::PALETTE_YELLOW()
            } else {
                theme::FG_DIM()
            };
            let pill = hub_ui::pill_rect(Pos2::new(x, cy - 9.0), width);
            hub_ui::pill(painter, pill, text, color);
        }
        _ if shown.seven => {
            x += SECTION_GAP;
            paint_group(painter, Pos2::new(x, cy), layout, usage, false, look);
            if let Some(countdown) = layout.countdown.as_ref().filter(|_| !look.compact) {
                painter.galley(
                    Pos2::new(x + GROUP_WIDTH_FULL, cy - countdown.size().y / 2.0),
                    Arc::clone(countdown),
                    theme::FG_DIM().gamma_multiply(fade),
                );
            }
        }
        _ => {}
    }
}

fn paint_group(
    painter: &egui::Painter,
    left_center: Pos2,
    layout: &ChipLayout,
    usage: Option<&ProfileUsage>,
    five_hour: bool,
    look: ChipLook,
) {
    let window = usage.map(|u| if five_hour { &u.five_hour } else { &u.seven_day });
    let percent = window.and_then(|w| w.percent);
    let severity = severity_for(percent, usage.and_then(|u| api_severity(u, five_hour)));
    let fade = look.content_fade();
    let bar_width = if look.compact {
        BAR_WIDTH_COMPACT
    } else {
        BAR_WIDTH_FULL
    };

    let (label, pct) = layout.window(five_hour);
    painter.galley(
        Pos2::new(left_center.x, left_center.y - label.size().y / 2.0),
        Arc::clone(label),
        theme::FG_DIM().gamma_multiply(fade),
    );
    let bar = Rect::from_min_size(
        Pos2::new(
            left_center.x + LABEL_WIDTH + LABEL_GAP,
            left_center.y - BAR_HEIGHT / 2.0,
        ),
        Vec2::new(bar_width, BAR_HEIGHT),
    );
    let bar_fade = look.bar_fade();
    let bar_radius = CornerRadius::same(2);
    painter.rect_filled(bar, bar_radius, theme::usage_track().gamma_multiply(bar_fade));
    if let Some(percent) = percent {
        let fill = hub_ui::mini_bar_fill_rect(bar, percent / 100.0);
        if fill.width() > 0.0 {
            let color = hub_ui::severity_color(percent, Some(severity));
            painter.rect_filled(fill, bar_radius, color.gamma_multiply(bar_fade));
        }
    }

    let color = if look.status == ChipStatus::Stale {
        theme::FG_DIM()
    } else if severity == Severity::Normal {
        theme::FG_SOFT()
    } else {
        hub_ui::severity_color(percent.unwrap_or(0.0), Some(severity))
    };
    painter.galley(
        Pos2::new(
            bar.max.x + LABEL_GAP + PCT_WIDTH - pct.size().x,
            left_center.y - pct.size().y / 2.0,
        ),
        Arc::clone(pct),
        color.gamma_multiply(fade),
    );
}

// ---------------------------------------------------------------------------
// Hover card
// ---------------------------------------------------------------------------

fn status_pill(status: ChipStatus) -> (&'static str, Color32) {
    match status {
        ChipStatus::NoData => ("no data", theme::FG_DIM()),
        ChipStatus::Fresh => ("ok", theme::PALETTE_GREEN()),
        ChipStatus::Stale => ("stale", theme::PALETTE_YELLOW()),
        ChipStatus::Expired => ("expired", theme::PALETTE_YELLOW()),
        ChipStatus::Missing => ("no login", theme::FG_DIM()),
    }
}

fn show_tooltip(
    ui: &mut Ui,
    entry: &ChipEntry,
    accent: Color32,
    status: ChipStatus,
    now_ms: i64,
    tokens_today: Option<&str>,
    tokens_pending: bool,
) {
    Frame::NONE
        .inner_margin(Margin::same(TOOLTIP_MARGIN - POPUP_FRAME_MARGIN))
        .show(ui, |ui| {
            tooltip_body(ui, entry, accent, status, now_ms, tokens_today);
        });
    if tokens_today.is_none() && tokens_pending {
        // The stats poll is still scanning: look again shortly, only while the card is open
        // and until the scan answers or its patience window ends.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
}

fn tooltip_body(
    ui: &mut Ui,
    entry: &ChipEntry,
    accent: Color32,
    status: ChipStatus,
    now_ms: i64,
    tokens_today: Option<&str>,
) {
    ui.set_width(TOOLTIP_WIDTH);
    ui.spacing_mut().item_spacing.y = 2.0;

    let (header, _) = ui.allocate_exact_size(Vec2::new(TOOLTIP_WIDTH, 20.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(Pos2::new(header.min.x + 4.0, header.center().y), 4.0, accent);
    painter.text(
        Pos2::new(header.min.x + 14.0, header.center().y),
        Align2::LEFT_CENTER,
        &entry.name,
        FontId::proportional(13.0),
        theme::FG(),
    );
    let (pill_label, pill_color) = status_pill(status);
    let pill_width = hub_ui::pill_width(painter, pill_label);
    let pill = hub_ui::pill_rect(
        Pos2::new(header.max.x - pill_width, header.center().y - 9.0),
        pill_width,
    );
    hub_ui::pill(painter, pill, pill_label, pill_color);
    if let Some(identity) = &entry.identity {
        ui.label(egui::RichText::new(identity).size(11.0).color(theme::FG_DIM()));
    }
    separator(ui);

    let usage = entry.usage.as_ref();
    let shown = ShownWindows::of(usage);
    for (label, five_hour) in [("5h", true), ("7d", false)] {
        if !shown.shows(five_hour) {
            continue;
        }
        let window = usage.map(|u| if five_hour { &u.five_hour } else { &u.seven_day });
        let reset = if five_hour {
            &entry.reset_five
        } else {
            &entry.reset_seven
        };
        let api = usage.and_then(|u| api_severity(u, five_hour));
        window_rows(ui, label, window, api, reset.as_ref(), now_ms);
    }
    if shown.five || shown.seven {
        separator(ui);
    }

    if let Some(tokens) = tokens_today {
        tokens_row(ui, tokens);
    }
    let age = usage.and_then(|u| u.fetched_at_ms).map_or_else(
        || "Not updated yet".to_owned(),
        |at| format!("Updated {}", time::format_age(now_ms.saturating_sub(at) / 1000)),
    );
    ui.label(egui::RichText::new(age).size(10.5).color(theme::FG_DIM()));
    match status {
        ChipStatus::Expired | ChipStatus::Missing => {
            ui.label(
                egui::RichText::new(format!("Open {} in a terminal to renew login", entry.name))
                    .size(10.5)
                    .color(theme::PALETTE_YELLOW()),
            );
        }
        ChipStatus::Stale => {
            let note = if usage.is_some_and(|u| u.state == UsageState::Stale) {
                "Last refresh failed; showing older data"
            } else {
                "Data is older than 15 minutes"
            };
            ui.label(egui::RichText::new(note).size(10.5).color(theme::PALETTE_RED()));
        }
        ChipStatus::NoData | ChipStatus::Fresh => {}
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Click: open session \u{b7} Right-click: refresh")
            .size(10.0)
            .color(theme::FG_DIM()),
    );
}

/// `Tokens today   1.2M` (all accounts).
fn tokens_row(ui: &mut Ui, tokens: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Tokens today (all accounts)")
                .size(11.0)
                .color(theme::FG_SOFT()),
        );
        ui.label(egui::RichText::new(tokens).monospace().size(11.0).color(theme::FG()));
    });
}

fn separator(ui: &mut Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(TOOLTIP_WIDTH, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SUBTLE());
    ui.add_space(6.0);
}

/// `5h  ▰▰▱▱ 42% used · 58% left` plus the reset line under it.
fn window_rows(
    ui: &mut Ui,
    label: &str,
    window: Option<&UsageWindow>,
    api: Option<&str>,
    reset: Option<&super::ResetInfo>,
    now_ms: i64,
) {
    let (row, _) = ui.allocate_exact_size(Vec2::new(TOOLTIP_WIDTH, 16.0), Sense::hover());
    let painter = ui.painter();
    let cy = row.center().y;
    painter.text(
        Pos2::new(row.min.x, cy),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(10.5),
        theme::FG_DIM(),
    );
    let bar = Rect::from_min_size(Pos2::new(row.min.x + 24.0, cy - TOOLTIP_BAR.y / 2.0), TOOLTIP_BAR);
    let percent = window.and_then(|w| w.percent);
    let severity = severity_for(percent, api);
    painter.rect_filled(bar, CornerRadius::same(2), theme::usage_track());
    if let Some(percent) = percent {
        let fill = hub_ui::mini_bar_fill_rect(bar, percent / 100.0);
        if fill.width() > 0.0 {
            painter.rect_filled(
                fill,
                CornerRadius::same(2),
                hub_ui::severity_color(percent, Some(severity)),
            );
        }
    }
    let text_x = bar.max.x + 8.0;
    match window.and_then(|w| w.percent.map(|percent| (w, percent))) {
        Some((window, percent)) => {
            let used = format!("{}% used", used_percent_label(window));
            let used_galley = painter.layout_no_wrap(used, FontId::proportional(11.0), theme::FG());
            let used_width = used_galley.size().x;
            painter.galley(
                Pos2::new(text_x, cy - used_galley.size().y / 2.0),
                used_galley,
                theme::FG(),
            );
            painter.text(
                Pos2::new(text_x + used_width, cy),
                Align2::LEFT_CENTER,
                format!(" \u{b7} {}% left", remaining_percent(percent)),
                FontId::proportional(11.0),
                theme::FG_DIM(),
            );
        }
        None => {
            painter.text(
                Pos2::new(text_x, cy),
                Align2::LEFT_CENTER,
                "\u{2014}",
                FontId::proportional(11.0),
                theme::FG_DIM(),
            );
        }
    }
    if let Some(reset) = reset {
        let secs = reset.unix - now_ms / 1000;
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.label(
                egui::RichText::new(format!("resets {} (in {})", reset.clock, time::format_relative(secs)))
                    .size(10.5)
                    .color(theme::FG_DIM()),
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Overflow menu rows
// ---------------------------------------------------------------------------

/// Rows of the chips that did not fit (`plan.visible..plan.total`), for the
/// overflow menu. Returns the profile to open when a row is clicked.
pub(in crate::app) fn show_overflow_rows(ui: &mut Ui, usage: &ClaudeUsageState, plan: ChipsPlan) -> Option<String> {
    let mut launch = None;
    for index in plan.visible..plan.total {
        let (Some(entry), Some(layout)) = (usage.entries().get(index), usage.cache.chips.get(index)) else {
            break;
        };
        let (head, tail) = chip_label_parts(&entry.name, entry.usage.as_ref(), false);
        let response = overflow_row(ui, hub_ui::accent_color(layout.accent_slot), &head, &tail);
        if response.clicked() {
            launch = Some(entry.name.clone());
            ui.close();
        }
    }
    launch
}

fn overflow_row(ui: &mut Ui, accent: Color32, head: &str, tail: &str) -> Response {
    let head_galley = ui
        .painter()
        .layout_no_wrap(head.to_owned(), FontId::proportional(12.0), theme::FG());
    let tail_galley = ui
        .painter()
        .layout_no_wrap(tail.to_owned(), FontId::proportional(11.0), theme::FG_DIM());
    let content = 8.0 + 8.0 + 8.0 + head_galley.size().x + 10.0 + tail_galley.size().x + 8.0;
    let width = content.max(OVERFLOW_ROW_MIN_WIDTH).max(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, OVERFLOW_ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            true,
            format!("Open Claude session {head} {tail}").trim_end().to_owned(),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            painter.rect_filled(
                rect,
                CornerRadius::same(6),
                theme::blend(theme::PANEL_BG_ALT(), accent, 0.18),
            );
        }
        let cy = rect.center().y;
        painter.circle_filled(Pos2::new(rect.min.x + 12.0, cy), 4.0, accent);
        let text_x = rect.min.x + 24.0;
        let head_width = head_galley.size().x;
        painter.galley(
            Pos2::new(text_x, cy - head_galley.size().y / 2.0),
            head_galley,
            theme::FG(),
        );
        painter.galley(
            Pos2::new(text_x + head_width + 10.0, cy - tail_galley.size().y / 2.0),
            tail_galley,
            theme::FG_DIM(),
        );
        hub_ui::paint_focus_ring(painter, &response, rect, 6);
    }
    response
}
