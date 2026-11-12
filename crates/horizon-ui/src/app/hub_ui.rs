//! Shared claude-hub UI primitives (spec: `docs/design/claude-hub-ui.md` §0).
//!
//! Every hub surface (usage chips, settings tabs, workspace sidebar, modals)
//! builds on these helpers so sizes, colors and states stay uniform. Colors
//! come from `theme.rs` functions only. Pure logic (accent assignment,
//! severity thresholds, bar geometry) lives in small `#[must_use]` functions
//! that are unit tested; painting wrappers stay thin.
//!
//! Naming for consumers:
//! - accents: [`accent_index`], [`assign_accent_indices`], [`accent_color`], [`profile_accent`]
//! - bars: [`mini_bar`], [`mini_bar_fill_rect`], [`severity_color`], [`Severity`]
//! - chips: [`pill`], [`pill_width`], [`key_badge`], [`kbd_hint`], [`swatch`]
//! - buttons: [`primary_button`], [`secondary_button`], [`icon_button`], [`IconKind`]
//! - focus: [`paint_focus_ring`]
//! - modals: [`modal_frame`], [`show_modal`]

#![expect(dead_code, reason = "used by claude-hub UI tasks")]

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Modifiers, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, WidgetType,
};

use crate::theme;

/// Number of distinct profile accents in `theme::PROFILE_ACCENTS`.
const ACCENT_COUNT: usize = 6;

const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

const BAR_CORNER_RADIUS: u8 = 2;
const BAR_MIN_FILL_WIDTH: f32 = 2.0;

const WARNING_PCT: f32 = 75.0;
const CRITICAL_PCT: f32 = 90.0;

// ---------------------------------------------------------------------------
// Profile accents
// ---------------------------------------------------------------------------

fn fnv1a(name: &str) -> u32 {
    name.bytes().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Accent slot for `name`: `FNV-1a(name) % 6`, moved forward (wrapping) to the
/// next slot not in `taken`. When all six slots are taken the hashed slot is
/// reused.
#[must_use]
pub(crate) fn accent_index(name: &str, taken: &[usize]) -> usize {
    let start = fnv1a(name) as usize % ACCENT_COUNT;
    (0..ACCENT_COUNT)
        .map(|offset| (start + offset) % ACCENT_COUNT)
        .find(|slot| !taken.contains(slot))
        .unwrap_or(start)
}

/// Slots for every visible profile, in list order; earlier names win ties.
/// Cache the result in `HubState` keyed by the name list.
#[must_use]
pub(crate) fn assign_accent_indices(names: &[&str]) -> Vec<usize> {
    let mut taken = Vec::with_capacity(names.len());
    for name in names {
        let slot = accent_index(name, &taken);
        taken.push(slot);
    }
    taken
}

/// Theme color for an accent slot (wraps for out-of-range slots).
#[must_use]
pub(crate) fn accent_color(slot: usize) -> Color32 {
    theme::PROFILE_ACCENTS()[slot % ACCENT_COUNT]
}

/// Stable per-profile accent given the slots already `taken` by other visible profiles.
#[must_use]
pub(crate) fn profile_accent(name: &str, taken: &[usize]) -> Color32 {
    accent_color(accent_index(name, taken))
}

// ---------------------------------------------------------------------------
// Usage bars and severity
// ---------------------------------------------------------------------------

/// Bar/value severity; also the shape of the API-provided override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Normal,
    Warning,
    Critical,
}

/// Severity for a usage percentage (0–100); `api` (the server's own verdict)
/// always wins. Non-finite percentages count as normal.
#[must_use]
pub(crate) fn severity_for(pct: f32, api: Option<Severity>) -> Severity {
    if let Some(severity) = api {
        return severity;
    }
    if !pct.is_finite() || pct < WARNING_PCT {
        Severity::Normal
    } else if pct < CRITICAL_PCT {
        Severity::Warning
    } else {
        Severity::Critical
    }
}

/// Bar color: neutral below 75, yellow 75..<90, red from 90; `api` overrides.
#[must_use]
pub(crate) fn severity_color(pct: f32, api: Option<Severity>) -> Color32 {
    match severity_for(pct, api) {
        Severity::Normal => theme::bar_normal(),
        Severity::Warning => theme::PALETTE_YELLOW(),
        Severity::Critical => theme::PALETTE_RED(),
    }
}

/// Fill rectangle of a bar inside `track`: `frac` is clamped to 0..=1 (NaN is
/// 0); any non-zero fraction is at least 2 px wide, capped at the track width.
#[must_use]
pub(crate) fn mini_bar_fill_rect(track: Rect, frac: f32) -> Rect {
    let frac = if frac.is_nan() { 0.0 } else { frac.clamp(0.0, 1.0) };
    if frac <= 0.0 {
        return Rect::from_min_size(track.min, Vec2::new(0.0, track.height()));
    }
    let width = (track.width() * frac).max(BAR_MIN_FILL_WIDTH).min(track.width());
    Rect::from_min_size(track.min, Vec2::new(width, track.height()))
}

/// Usage bar: track in `usage_track()`, fill in `fill`, both radius 2, no animation.
pub(crate) fn mini_bar(painter: &Painter, rect: Rect, frac: f32, fill: Color32) {
    let radius = CornerRadius::same(BAR_CORNER_RADIUS);
    painter.rect_filled(rect, radius, theme::usage_track());
    let fill_rect = mini_bar_fill_rect(rect, frac);
    if fill_rect.width() > 0.0 {
        painter.rect_filled(fill_rect, radius, fill);
    }
}

// ---------------------------------------------------------------------------
// Pills, badges, hints, swatches
// ---------------------------------------------------------------------------

const PILL_HEIGHT: f32 = 18.0;
const PILL_RADIUS: u8 = 9;
const PILL_PADDING_X: f32 = 8.0;
const PILL_FONT_SIZE: f32 = 10.5;

/// Width a pill needs for `text` (height is always [`PILL_HEIGHT`]).
#[must_use]
pub(crate) fn pill_width(painter: &Painter, text: &str) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(PILL_FONT_SIZE), Color32::WHITE);
    galley.size().x + PILL_PADDING_X * 2.0
}

/// Rect of a pill whose top-left is `min` and whose text is `text_width` wide
/// (horizontal padding is added; height is always [`PILL_HEIGHT`]).
#[must_use]
pub(crate) fn pill_rect(min: Pos2, text_width: f32) -> Rect {
    Rect::from_min_size(min, Vec2::new(text_width.max(0.0) + PILL_PADDING_X * 2.0, PILL_HEIGHT))
}

/// Status pill (ok / expired / missing / danger) painted into `rect`; build
/// `rect` with [`pill_rect`] so the height stays [`PILL_HEIGHT`].
pub(crate) fn pill(painter: &Painter, rect: Rect, text: &str, color: Color32) {
    painter.rect(
        rect,
        CornerRadius::same(PILL_RADIUS),
        theme::alpha(color, 36),
        Stroke::new(1.0_f32, theme::alpha(color, 150)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(PILL_FONT_SIZE),
        color,
    );
}

/// Numbered quick-switch badge (20x20 expected). Inactive: neutral chrome;
/// active: filled with the profile `accent`.
pub(crate) fn key_badge(painter: &Painter, rect: Rect, n: usize, active: bool, accent: Color32) {
    let (fill, text_color) = if active {
        (accent, theme::BG())
    } else {
        (theme::PANEL_BG_ALT(), theme::FG_DIM())
    };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        n.to_string(),
        FontId::monospace(11.0),
        text_color,
    );
}

/// Keyboard hint such as `kbd_hint(ui, "Enter", "open")`: key capsule + label, inline.
pub(crate) fn kbd_hint(ui: &mut Ui, key: &str, label: &str) {
    let key_galley = ui
        .painter()
        .layout_no_wrap(key.to_owned(), FontId::monospace(10.0), theme::FG_SOFT());
    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(10.5), theme::FG_DIM());
    let capsule_size = Vec2::new(key_galley.size().x + 10.0, 16.0);
    let gap = 4.0;
    let size = Vec2::new(capsule_size.x + gap + label_galley.size().x, capsule_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, format!("{key} {label}")));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let capsule = Rect::from_min_size(rect.min, capsule_size);
    painter.rect_stroke(
        capsule,
        CornerRadius::same(4),
        Stroke::new(1.0_f32, theme::BORDER_SUBTLE()),
        StrokeKind::Inside,
    );
    painter.galley(capsule.center() - key_galley.size() / 2.0, key_galley, theme::FG_SOFT());
    let label_pos = Pos2::new(capsule.right() + gap, rect.center().y - label_galley.size().y / 2.0);
    painter.galley(label_pos, label_galley, theme::FG_DIM());
}

const SWATCH_SIZE: f32 = 22.0;

/// Color swatch (22x22, radius 6). The selection/hover/focus ring is painted
/// 2 px outside the box (2 px gap + stroke), so leave >= 4 px between swatches.
/// `label` (for example "Amber") is the accessible name. In a disabled `Ui`
/// the swatch is faded and ignores hover and clicks.
pub(crate) fn swatch(ui: &mut Ui, color: Color32, selected: bool, label: &str) -> Response {
    let enabled = ui.is_enabled();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SWATCH_SIZE), sense);
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, selected, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = enabled && response.hovered();
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), color);
    let ring_rect = rect.expand(2.0);
    let ring_radius = CornerRadius::same(8);
    if response.has_focus() {
        painter.rect_stroke(
            ring_rect,
            ring_radius,
            Stroke::new(2.0_f32, theme::ACCENT()),
            StrokeKind::Outside,
        );
    } else if selected {
        painter.rect_stroke(
            ring_rect,
            ring_radius,
            Stroke::new(2.0_f32, theme::FG()),
            StrokeKind::Outside,
        );
    } else if hovered {
        painter.rect_stroke(
            ring_rect,
            ring_radius,
            Stroke::new(1.0_f32, theme::FG_SOFT()),
            StrokeKind::Outside,
        );
    }
    if selected {
        let c = rect.center();
        let check = Stroke::new(1.6_f32, theme::BG());
        painter.line_segment([c + Vec2::new(-4.0, 0.5), c + Vec2::new(-1.2, 3.5)], check);
        painter.line_segment([c + Vec2::new(-1.2, 3.5), c + Vec2::new(4.5, -3.0)], check);
    }
    response
}

// ---------------------------------------------------------------------------
// Focus ring and buttons
// ---------------------------------------------------------------------------

/// Keyboard focus ring: 2 px `ACCENT()` outside `rect`, only while `response` has focus.
pub(crate) fn paint_focus_ring(painter: &Painter, response: &Response, rect: Rect, radius: u8) {
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius),
            Stroke::new(2.0_f32, theme::ACCENT()),
            StrokeKind::Outside,
        );
    }
}

const BUTTON_PADDING_X: f32 = 16.0;
const BUTTON_MIN_WIDTH: f32 = 64.0;
const BUTTON_RADIUS: u8 = 8;

/// Allocates a button. `enabled` is the caller's flag; a disabled `Ui` also
/// disables it. Returns the response, rect and the effective enabled state.
fn button_response(ui: &mut Ui, label: &str, height: f32, font: &FontId, enabled: bool) -> (Response, Rect, bool) {
    let enabled = enabled && ui.is_enabled();
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let size = Vec2::new((text_width + BUTTON_PADDING_X * 2.0).max(BUTTON_MIN_WIDTH), height);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    (response, rect, enabled)
}

/// Primary call-to-action: amber fill, height 30. `enabled == false` paints
/// the disabled look and the returned response never reports clicks.
pub(crate) fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let font = FontId::proportional(12.0);
    let (response, rect, enabled) = button_response(ui, label, 30.0, &font, enabled);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let (fill, text_color) = if !enabled {
        (theme::alpha(theme::ACCENT(), 70), theme::alpha(theme::BG(), 160))
    } else if response.is_pointer_button_down_on() {
        (theme::blend(theme::ACCENT(), theme::BG(), 0.18), theme::BG())
    } else if response.hovered() {
        (theme::blend(theme::ACCENT(), theme::FG(), 0.12), theme::BG())
    } else {
        (theme::ACCENT(), theme::BG())
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(BUTTON_RADIUS), fill);
    // The font stack has no bold face, so the label uses the regular 12 pt weight.
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, text_color);
    paint_focus_ring(painter, &response, rect, BUTTON_RADIUS);
    response
}

/// Secondary button: chrome fill with subtle border, height 28. In a disabled
/// `Ui` it is dimmed and never reports hover or clicks.
pub(crate) fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    let font = FontId::proportional(12.0);
    let (response, rect, enabled) = button_response(ui, label, 28.0, &font, true);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let widgets = &ui.visuals().widgets;
    let visuals = if response.is_pointer_button_down_on() {
        widgets.active
    } else if response.hovered() {
        widgets.hovered
    } else {
        widgets.inactive
    };
    let (fill, stroke) = if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        (visuals.weak_bg_fill, visuals.bg_stroke)
    } else {
        (theme::PANEL_BG_ALT(), Stroke::new(1.0_f32, theme::BORDER_SUBTLE()))
    };
    let text_color = if enabled { theme::FG() } else { theme::FG_DIM() };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(BUTTON_RADIUS),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, text_color);
    paint_focus_ring(painter, &response, rect, BUTTON_RADIUS);
    response
}

/// Glyph-free icons for [`icon_button`], painted as line/triangle segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IconKind {
    /// Triangle pointing up (reorder).
    Up,
    /// Triangle pointing down (reorder).
    Down,
    /// 9x9 cross (remove); hover tints it with `BTN_CLOSE()`.
    Remove,
    /// Folder outline (browse).
    Browse,
}

const ICON_BUTTON_SIZE: f32 = 26.0;

/// 26x26 icon button; `tooltip` doubles as the accessible label.
pub(crate) fn icon_button(ui: &mut Ui, icon: IconKind, tooltip: &str) -> Response {
    icon_button_enabled(ui, icon, tooltip, true)
}

/// [`icon_button`] with an explicit enabled flag (for example the first row's
/// Up and the last row's Down). Disabled buttons are dimmed, show the tooltip
/// but have no hover fill and never report clicks; a disabled `Ui` behaves the same.
pub(crate) fn icon_button_enabled(ui: &mut Ui, icon: IconKind, tooltip: &str, enabled: bool) -> Response {
    let enabled = enabled && ui.is_enabled();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON_BUTTON_SIZE), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, tooltip));
    let response = response.on_hover_text(tooltip).on_disabled_hover_text(tooltip);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = enabled && (response.hovered() || response.is_pointer_button_down_on());
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, CornerRadius::same(BUTTON_RADIUS), theme::PANEL_BG_ALT());
    }
    let color = match (icon, hovered) {
        _ if !enabled => theme::alpha(theme::FG_DIM(), 90),
        (IconKind::Remove, true) => theme::BTN_CLOSE(),
        (_, true) => theme::FG(),
        (_, false) => theme::FG_SOFT(),
    };
    paint_icon(painter, rect.center(), icon, color);
    paint_focus_ring(painter, &response, rect, BUTTON_RADIUS);
    response
}

fn paint_icon(painter: &Painter, c: Pos2, icon: IconKind, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    match icon {
        IconKind::Up => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + Vec2::new(0.0, -2.5),
                    c + Vec2::new(4.0, 2.5),
                    c + Vec2::new(-4.0, 2.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
        IconKind::Down => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + Vec2::new(0.0, 2.5),
                    c + Vec2::new(-4.0, -2.5),
                    c + Vec2::new(4.0, -2.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
        IconKind::Remove => {
            let h = 4.5;
            painter.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], stroke);
            painter.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], stroke);
        }
        IconKind::Browse => {
            let body = Rect::from_center_size(c + Vec2::new(0.0, 1.0), Vec2::new(11.0, 8.0));
            painter.rect_stroke(body, CornerRadius::same(1), stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    body.left_top() + Vec2::new(0.0, -1.5),
                    body.left_top() + Vec2::new(4.0, -1.5),
                ],
                stroke,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

/// Modal container: `PANEL_BG` fill, 1 px `BORDER_STRONG`, radius 16, theme
/// popup shadow, no outer margin (sections pad themselves by 16).
pub(crate) fn modal_frame(ctx: &egui::Context) -> Frame {
    Frame::new()
        .fill(theme::PANEL_BG())
        .stroke(Stroke::new(1.0_f32, theme::BORDER_STRONG()))
        .corner_radius(CornerRadius::same(16))
        .shadow(ctx.style().visuals.popup_shadow)
        .inner_margin(Margin::ZERO)
}

/// Horizontal viewport margin kept free on each side of a modal (48 px total).
const MODAL_VIEWPORT_MARGIN: f32 = 48.0;

/// Where a modal sits inside the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ModalPlacement {
    /// Centered both ways.
    Center,
    /// Horizontally centered, top edge at this fraction of the viewport
    /// height (the Claude launcher uses `0.18`).
    TopFraction(f32),
}

/// Result of one [`show_modal`] frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModalOutcome {
    /// The user asked to close (click on the scrim, or Esc with no popup open).
    pub(crate) dismissed: bool,
    /// First frame since the modal opened; callers may seed state here.
    pub(crate) first_frame: bool,
}

/// Modal width clamped so at least 48 px of viewport stays free: `min(width, viewport_width - 48)`.
#[must_use]
pub(crate) fn modal_width(width: f32, viewport_width: f32) -> f32 {
    width.min(viewport_width - MODAL_VIEWPORT_MARGIN).max(0.0)
}

/// Distance from the viewport top to the modal top for `placement`, or `None`
/// when the modal is vertically centered.
#[must_use]
pub(crate) fn modal_top_offset(placement: ModalPlacement, viewport_height: f32) -> Option<f32> {
    match placement {
        ModalPlacement::Center => None,
        ModalPlacement::TopFraction(fraction) => {
            let fraction = if fraction.is_nan() {
                0.0
            } else {
                fraction.clamp(0.0, 1.0)
            };
            Some(viewport_height * fraction)
        }
    }
}

/// `true` when the modal was not drawn in the previous frame (just opened).
#[must_use]
fn modal_is_first_frame(last_frame: Option<u64>, now: u64) -> bool {
    last_frame.is_none_or(|last| now.saturating_sub(last) > 1)
}

/// Shows a modal over a viewport scrim, above the toolbar and sidebar
/// (`Order::Debug`, like the command palette and directory picker). The scrim
/// swallows pointer input; a click on it, or Esc when no popup inside the
/// modal is open, sets [`ModalOutcome::dismissed`]. The caller owns the "open"
/// state and drops it on `dismissed`.
///
/// Keyboard focus: on the first frame `initial_focus` (a widget id inside
/// the modal, such as a search field) is focused, otherwise a focus sink
/// covering the modal. While open, focus is pulled back to the sink whenever
/// nothing is focused, so keystrokes never fall through to a terminal behind
/// it (terminals only take keys when no other widget has focus).
///
/// Note: `egui` popups (`ComboBox`) use `Order::Foreground` and would render
/// below this modal, so use inline selectors inside it.
pub(crate) fn show_modal(
    ctx: &egui::Context,
    id: egui::Id,
    width: f32,
    placement: ModalPlacement,
    initial_focus: Option<egui::Id>,
    add_contents: impl FnOnce(&mut Ui),
) -> ModalOutcome {
    let screen = ctx.content_rect();
    let frame_nr = ctx.cumulative_frame_nr();
    let last_frame_key = id.with("last_frame");
    let first_frame = modal_is_first_frame(ctx.data(|data| data.get_temp::<u64>(last_frame_key)), frame_nr);
    ctx.data_mut(|data| data.insert_temp(last_frame_key, frame_nr));
    // Read before the contents run: a popup closes itself on Esc while they draw.
    let popup_open = egui::Popup::is_any_open(ctx);
    let mut scrim_clicked = false;

    egui::Area::new(id.with("scrim"))
        .order(egui::Order::Debug)
        .fixed_pos(screen.min)
        .constrain(false)
        .interactable(true)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen, CornerRadius::ZERO, theme::scrim());
            scrim_clicked = ui.allocate_rect(screen, Sense::click()).clicked();
        });

    let (anchor, offset) = match modal_top_offset(placement, screen.height()) {
        Some(top) => (Align2::CENTER_TOP, Vec2::new(0.0, top)),
        None => (Align2::CENTER_CENTER, Vec2::ZERO),
    };
    let sink = id.with("focus_sink");
    let modal_width = modal_width(width, screen.width());
    let modal = egui::Area::new(id.with("modal"))
        .order(egui::Order::Debug)
        .anchor(anchor, offset)
        .show(ctx, |ui| {
            let framed = modal_frame(ctx).show(ui, |ui| {
                ui.set_width(modal_width);
                add_contents(ui);
            });
            ui.interact(framed.response.rect, sink, Sense::focusable_noninteractive());
        });
    ctx.move_to_top(modal.response.layer_id);

    if first_frame {
        ctx.memory_mut(|memory| memory.request_focus(initial_focus.unwrap_or(sink)));
    } else if !ctx.wants_keyboard_input() {
        ctx.memory_mut(|memory| memory.request_focus(sink));
    }

    let pointer_inside = ctx
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|pos| modal.response.rect.contains(pos));
    let mut dismissed = scrim_clicked && !first_frame && !pointer_inside;
    if !popup_open && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        dismissed = true;
    }
    ModalOutcome { dismissed, first_frame }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_width(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-4, "width {actual} != {expected}");
    }

    #[test]
    fn accent_index_is_stable_fnv1a() {
        // Reference FNV-1a 32-bit vectors.
        assert_eq!(fnv1a(""), 0x811c_9dc5);
        assert_eq!(fnv1a("a"), 0xe40c_292c);
        assert_eq!(fnv1a("foobar"), 0xbf9c_f968);
        assert_eq!(accent_index("foobar", &[]), 0xbf9c_f968_usize % ACCENT_COUNT);
        assert_eq!(accent_index("foobar", &[]), accent_index("foobar", &[]));
        assert!(accent_index("work", &[]) < ACCENT_COUNT);
    }

    #[test]
    fn accent_collision_takes_next_free() {
        let home = accent_index("home", &[]);
        assert_eq!(accent_index("home", &[home]), (home + 1) % ACCENT_COUNT);
        assert_eq!(
            accent_index("home", &[home, (home + 1) % ACCENT_COUNT]),
            (home + 2) % ACCENT_COUNT
        );

        // Wraps past the end of the table.
        let taken: Vec<usize> = (0..ACCENT_COUNT)
            .filter(|slot| *slot != (home + 5) % ACCENT_COUNT)
            .collect();
        assert_eq!(accent_index("home", &taken), (home + 5) % ACCENT_COUNT);

        // Identical hashes get distinct slots in list order; all-taken reuses the hashed slot.
        assert_eq!(
            assign_accent_indices(&["home", "home"]),
            vec![home, (home + 1) % ACCENT_COUNT]
        );
        let all: Vec<usize> = (0..ACCENT_COUNT).collect();
        assert_eq!(accent_index("home", &all), home);
        assert_eq!(
            assign_accent_indices(&["a", "b", "c", "d", "e", "f"]).len(),
            ACCENT_COUNT
        );
    }

    #[test]
    fn severity_thresholds_75_90() {
        assert_eq!(severity_for(0.0, None), Severity::Normal);
        assert_eq!(severity_for(74.9, None), Severity::Normal);
        assert_eq!(severity_for(75.0, None), Severity::Warning);
        assert_eq!(severity_for(89.9, None), Severity::Warning);
        assert_eq!(severity_for(90.0, None), Severity::Critical);
        assert_eq!(severity_for(100.0, None), Severity::Critical);
        assert_eq!(severity_for(f32::NAN, None), Severity::Normal);
        assert_eq!(severity_color(10.0, None), theme::bar_normal());
        assert_eq!(severity_color(80.0, None), theme::PALETTE_YELLOW());
        assert_eq!(severity_color(95.0, None), theme::PALETTE_RED());
    }

    #[test]
    fn severity_api_override_wins() {
        assert_eq!(severity_for(10.0, Some(Severity::Critical)), Severity::Critical);
        assert_eq!(severity_for(99.0, Some(Severity::Normal)), Severity::Normal);
        assert_eq!(severity_color(99.0, Some(Severity::Warning)), theme::PALETTE_YELLOW());
    }

    #[test]
    fn mini_bar_min_width_when_nonzero() {
        let track = Rect::from_min_size(Pos2::new(10.0, 4.0), Vec2::new(100.0, 4.0));
        assert_width(mini_bar_fill_rect(track, 0.0).width(), 0.0);
        assert_width(mini_bar_fill_rect(track, f32::NAN).width(), 0.0);
        assert_width(mini_bar_fill_rect(track, -1.0).width(), 0.0);
        assert_width(mini_bar_fill_rect(track, 0.001).width(), BAR_MIN_FILL_WIDTH);
        assert_width(mini_bar_fill_rect(track, 0.5).width(), 50.0);
        assert_width(mini_bar_fill_rect(track, 7.0).width(), 100.0);
        let fill = mini_bar_fill_rect(track, 0.5);
        assert_eq!((fill.min, fill.height()), (track.min, track.height()));
        // A track narrower than the minimum never overflows.
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(1.0, 4.0));
        assert_width(mini_bar_fill_rect(tiny, 0.5).width(), 1.0);
    }

    #[test]
    fn profile_accents_are_distinct() {
        let accents = theme::PROFILE_ACCENTS();
        for (i, a) in accents.iter().enumerate() {
            assert!(accents.iter().skip(i + 1).all(|b| a != b));
        }
    }

    #[test]
    fn modal_width_clamps_to_viewport_minus_margin() {
        assert_width(modal_width(480.0, 1280.0), 480.0);
        assert_width(modal_width(480.0, 500.0), 452.0);
        assert_width(modal_width(480.0, 528.0), 480.0);
        assert_width(modal_width(480.0, 30.0), 0.0);
    }

    #[test]
    fn modal_top_offset_is_fraction_of_viewport_height() {
        assert_eq!(modal_top_offset(ModalPlacement::Center, 800.0), None);
        assert_width(
            modal_top_offset(ModalPlacement::TopFraction(0.18), 800.0).unwrap_or(-1.0),
            144.0,
        );
        assert_width(
            modal_top_offset(ModalPlacement::TopFraction(2.0), 800.0).unwrap_or(-1.0),
            800.0,
        );
        assert_width(
            modal_top_offset(ModalPlacement::TopFraction(f32::NAN), 800.0).unwrap_or(-1.0),
            0.0,
        );
    }

    #[test]
    fn modal_first_frame_detects_gaps() {
        assert!(modal_is_first_frame(None, 5));
        assert!(!modal_is_first_frame(Some(4), 5));
        assert!(!modal_is_first_frame(Some(5), 5));
        assert!(modal_is_first_frame(Some(2), 5));
    }

    #[test]
    fn pill_rect_enforces_height_and_adds_padding() {
        let rect = pill_rect(Pos2::new(10.0, 20.0), 30.0);
        assert_width(rect.height(), PILL_HEIGHT);
        assert_width(rect.height(), 18.0);
        assert_width(rect.width(), 30.0 + PILL_PADDING_X * 2.0);
        assert_eq!(rect.min, Pos2::new(10.0, 20.0));
        assert_width(pill_rect(Pos2::ZERO, -5.0).width(), PILL_PADDING_X * 2.0);
    }
}
