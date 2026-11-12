//! Drawing of the quick-start strip: chips in the family of the history meter (tinted fill,
//! subtle stroke), the profile accent on the dot or star, dimmed when a login is unusable.

use std::sync::Arc;

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::theme;

use super::layout::{CHIP_PAD, DOT, DOT_GAP, ID_GAP, LEAD_CHROME, MODE_PAD, STAR_WIDTH};
use super::placement::ItemKind;
use super::strip::{NOTICE_PAD, NoticeView, StripView, ViewItem};
use super::titlebar::paint_asterisk;

const RADIUS: u8 = 7;
const STAR_RADIUS: f32 = 4.5;
/// Opacity of a strip that cannot be used right now.
const DISABLED_OPACITY: f32 = 0.5;
/// Opacity of a chip whose login is expired or missing.
const DIM_OPACITY: f32 = 0.55;

/// Paints the whole strip, on top of the titlebar.
pub(in crate::app) fn paint_strip(painter: &Painter, view: &StripView) {
    let mut faded = painter.clone();
    if !view.enabled {
        faded.multiply_opacity(DISABLED_OPACITY);
    }
    for item in view.items.as_slice() {
        paint_item(&faded, item);
    }
    if let Some(notice) = &view.notice {
        paint_notice(painter, notice);
    }
}

/// Why the last start did not run: a small chip left of the strip, over the title.
fn paint_notice(painter: &Painter, notice: &NoticeView) {
    painter.rect_filled(
        notice.rect,
        CornerRadius::same(RADIUS),
        theme::alpha(theme::BG_ELEVATED(), 245),
    );
    painter.rect_stroke(
        notice.rect,
        CornerRadius::same(RADIUS),
        Stroke::new(1.0_f32, theme::alpha(theme::PALETTE_YELLOW(), 200)),
        StrokeKind::Outside,
    );
    painter.galley(
        Pos2::new(
            notice.rect.left() + NOTICE_PAD,
            notice.rect.center().y - notice.text.size().y * 0.5,
        ),
        Arc::clone(&notice.text),
        theme::FG(),
    );
}

fn paint_item(painter: &Painter, item: &ViewItem) {
    let mut painter = painter.clone();
    if item.dim {
        painter.multiply_opacity(DIM_OPACITY);
    }
    match item.kind {
        ItemKind::Launch => paint_launch(&painter, item),
        ItemKind::Mark => {
            paint_chip_background(&painter, item, theme::ACCENT());
            paint_asterisk(&painter, item.rect.center(), theme::ACCENT(), STAR_RADIUS);
        }
        ItemKind::Chip(_) => paint_chip(&painter, item),
        ItemKind::Back => paint_back(&painter, item),
        ItemKind::Mode(_) => paint_mode(&painter, item),
        ItemKind::Close => paint_close(&painter, item),
        ItemKind::More(_) => paint_more(&painter, item),
    }
    if item.focused {
        painter.rect_stroke(
            item.rect,
            CornerRadius::same(RADIUS),
            Stroke::new(2.0_f32, theme::ACCENT()),
            StrokeKind::Outside,
        );
    }
}

/// Tinted fill and stroke of a chip; stronger while hovered or pressed.
fn paint_chip_background(painter: &Painter, item: &ViewItem, accent: Color32) {
    let tint = if item.pressed {
        0.30
    } else if item.hovered {
        0.20
    } else {
        0.10
    };
    let fill_alpha = if item.hovered { 224 } else { 190 };
    painter.rect_filled(
        item.rect,
        CornerRadius::same(RADIUS),
        theme::alpha(theme::blend(theme::BG_ELEVATED(), accent, tint), fill_alpha),
    );
    let stroke_tint = if item.hovered { 0.45 } else { 0.34 };
    painter.rect_stroke(
        item.rect,
        CornerRadius::same(RADIUS),
        Stroke::new(
            1.0_f32,
            theme::alpha(theme::blend(theme::BORDER_SUBTLE(), accent, stroke_tint), 190),
        ),
        StrokeKind::Outside,
    );
}

fn text_color(item: &ViewItem) -> Color32 {
    if item.hovered { theme::FG() } else { theme::FG_SOFT() }
}

/// `✳ Claude ‹`
fn paint_launch(painter: &Painter, item: &ViewItem) {
    paint_chip_background(painter, item, theme::ACCENT());
    let rect = item.rect;
    let center_y = rect.center().y;
    paint_asterisk(
        painter,
        Pos2::new(rect.left() + 12.0, center_y),
        theme::ACCENT(),
        STAR_RADIUS,
    );
    if let Some(caption) = &item.text {
        painter.galley(
            Pos2::new(rect.left() + 22.0, center_y - caption.size().y * 0.5),
            Arc::clone(caption),
            text_color(item),
        );
    }
    // Left-pointing chevron: the strip opens toward the title, anchored at the history meter.
    let tip = Pos2::new(rect.right() - 11.0, center_y);
    let stroke = Stroke::new(1.3_f32, theme::FG_DIM());
    painter.line_segment([tip + Vec2::new(2.5, -3.0), tip], stroke);
    painter.line_segment([tip, tip + Vec2::new(2.5, 3.0)], stroke);
}

/// `● name  account`
fn paint_chip(painter: &Painter, item: &ViewItem) {
    paint_chip_background(painter, item, item.accent);
    let rect = item.rect;
    let center_y = rect.center().y;
    painter.circle_filled(
        Pos2::new(rect.left() + CHIP_PAD + DOT * 0.5, center_y),
        DOT * 0.5,
        item.accent,
    );
    let mut x = rect.left() + CHIP_PAD + DOT + DOT_GAP;
    if let Some(name) = &item.text {
        painter.galley(
            Pos2::new(x, center_y - name.size().y * 0.5),
            Arc::clone(name),
            text_color(item),
        );
        x += name.size().x + ID_GAP;
    }
    if let Some(id) = &item.id {
        painter.galley(
            Pos2::new(x, center_y - id.size().y * 0.5),
            Arc::clone(id),
            theme::FG_DIM(),
        );
    }
}

/// `✳ name` in the profile accent; clicking goes back to the chips.
fn paint_back(painter: &Painter, item: &ViewItem) {
    paint_chip_background(painter, item, item.accent);
    let rect = item.rect;
    let center_y = rect.center().y;
    paint_asterisk(
        painter,
        Pos2::new(rect.left() + CHIP_PAD + STAR_WIDTH * 0.5, center_y),
        item.accent,
        STAR_RADIUS,
    );
    if let Some(name) = &item.text {
        painter.galley(
            Pos2::new(rect.left() + LEAD_CHROME - CHIP_PAD, center_y - name.size().y * 0.5),
            Arc::clone(name),
            theme::FG(),
        );
    }
}

/// `New` / `Continue` / `Resume…`: a button in the profile accent.
fn paint_mode(painter: &Painter, item: &ViewItem) {
    let tint = if item.pressed {
        0.42
    } else if item.hovered {
        0.32
    } else {
        0.20
    };
    painter.rect_filled(
        item.rect,
        CornerRadius::same(RADIUS),
        theme::alpha(theme::blend(theme::BG_ELEVATED(), item.accent, tint), 230),
    );
    painter.rect_stroke(
        item.rect,
        CornerRadius::same(RADIUS),
        Stroke::new(1.0_f32, theme::alpha(item.accent, if item.hovered { 200 } else { 130 })),
        StrokeKind::Outside,
    );
    if let Some(text) = &item.text {
        let pos = Pos2::new(
            item.rect.left() + MODE_PAD.min((item.rect.width() - text.size().x) * 0.5).max(0.0),
            item.rect.center().y - text.size().y * 0.5,
        );
        painter.galley(pos, Arc::clone(text), theme::FG());
    }
}

/// `✕`
fn paint_close(painter: &Painter, item: &ViewItem) {
    let tint = if item.pressed {
        0.30
    } else if item.hovered {
        0.18
    } else {
        0.0
    };
    painter.rect_filled(
        item.rect,
        CornerRadius::same(RADIUS),
        theme::alpha(theme::blend(theme::BG_ELEVATED(), theme::FG(), tint), 190),
    );
    let color = if item.hovered { theme::FG() } else { theme::FG_DIM() };
    let stroke = Stroke::new(1.3_f32, color);
    let center = item.rect.center();
    let arm = 3.2;
    painter.line_segment([center + Vec2::new(-arm, -arm), center + Vec2::new(arm, arm)], stroke);
    painter.line_segment([center + Vec2::new(-arm, arm), center + Vec2::new(arm, -arm)], stroke);
}

/// `+N`: profiles that did not fit.
fn paint_more(painter: &Painter, item: &ViewItem) {
    if let Some(text) = &item.text {
        let rect = Rect::from_center_size(item.rect.center(), text.size());
        painter.galley(rect.min, Arc::clone(text), theme::FG_DIM());
    }
}
