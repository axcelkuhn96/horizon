//! Items of the quick-start strip with their text measured and widths planned (see
//! [`super::layout`] for the pure width rules). Built per (stage, room, pixel density) and cached
//! by [`super::state::QuickStartUi`], never per frame.

use std::sync::Arc;

use egui::{Color32, FontId, Galley, Painter};

use crate::app::hub_ui;

use super::layout::{
    CHIP_CHROME, CLOSE_WIDTH, ChipSize, ITEM_GAP, LEAD_CHROME, MARK_WIDTH, MODE_PAD, MORE_WIDTH, NAME_MIN_VISIBLE,
    modes_total, plan_chips, plan_modes, profiles_total,
};
use super::state::Row;
use super::titlebar::BUTTON_WIDTH;
use super::{QuickStartMode, Stage, StripEvent};

const NAME_SIZE: f32 = 11.0;
const ID_SIZE: f32 = 10.0;
const MODE_SIZE: f32 = 10.5;
const LAUNCH_SIZE: f32 = 10.5;
pub(super) const LAUNCH_CAPTION: &str = "Claude";

/// What a strip item is, and so what a click on it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemKind {
    /// The collapsed "Claude" button.
    Launch,
    /// The star that collapses the profile chips.
    Mark,
    Chip(usize),
    /// The profile name on the mode page.
    Back,
    Mode(QuickStartMode),
    Close,
    /// `+N` for profiles that did not fit.
    More(usize),
}

impl ItemKind {
    pub(super) fn interactive(self) -> bool {
        !matches!(self, Self::More(_))
    }

    pub(super) fn event(self) -> Option<StripEvent> {
        match self {
            Self::Launch | Self::Mark => Some(StripEvent::Toggle),
            Self::Chip(index) => Some(StripEvent::Pick(index)),
            Self::Back => Some(StripEvent::Back),
            Self::Mode(mode) => Some(StripEvent::Mode(mode)),
            Self::Close => Some(StripEvent::Dismiss),
            Self::More(_) => None,
        }
    }
}

/// One item of the strip, positioned relative to the strip's left edge.
pub(super) struct Placed {
    pub(super) kind: ItemKind,
    pub(super) x: f32,
    pub(super) w: f32,
    /// Profile name or button caption, already elided to the item's width.
    pub(super) text: Option<Arc<Galley>>,
    /// Account identifier, only when there was room for it.
    pub(super) id: Option<Arc<Galley>>,
}

pub(super) struct StripLayout {
    pub(super) items: Vec<Placed>,
    pub(super) width: f32,
}

impl StripLayout {
    /// The collapsed strip: just the button, with its caption measured once by the caller.
    pub(super) fn launch(caption: Arc<Galley>) -> Self {
        Self {
            items: vec![Placed {
                kind: ItemKind::Launch,
                x: 0.0,
                w: BUTTON_WIDTH,
                text: Some(caption),
                id: None,
            }],
            width: BUTTON_WIDTH,
        }
    }
}

pub(super) fn measure(painter: &Painter, text: &str, size: f32) -> Arc<Galley> {
    painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::PLACEHOLDER)
}

/// The collapsed button's caption.
pub(super) fn launch_caption(painter: &Painter) -> Arc<Galley> {
    measure(painter, LAUNCH_CAPTION, LAUNCH_SIZE)
}

/// `galley` when it fits `room`, else `text` elided to it; `None` when `room` is too small to
/// show any text.
fn fitted(painter: &Painter, galley: &Arc<Galley>, text: &str, size: f32, room: f32) -> Option<Arc<Galley>> {
    if room < NAME_MIN_VISIBLE {
        return None;
    }
    if galley.size().x <= room {
        Some(Arc::clone(galley))
    } else {
        Some(hub_ui::elided(painter, text, FontId::proportional(size), room))
    }
}

/// Layout of an open strip (`Collapsed` is the caller's cached launch layout).
pub(super) fn build_layout(painter: &Painter, stage: Stage, rows: &[Row], room: f32) -> StripLayout {
    match stage {
        Stage::Collapsed => StripLayout::launch(launch_caption(painter)),
        Stage::Modes { profile } => rows.get(profile).map_or_else(
            || profiles_layout(painter, rows, room),
            |row| modes_layout(painter, row, room),
        ),
        Stage::Profiles => profiles_layout(painter, rows, room),
    }
}

fn profiles_layout(painter: &Painter, rows: &[Row], room: f32) -> StripLayout {
    let names: Vec<Arc<Galley>> = rows.iter().map(|row| measure(painter, &row.name, NAME_SIZE)).collect();
    let ids: Vec<Option<Arc<Galley>>> = rows
        .iter()
        .map(|row| row.account.as_deref().map(|account| measure(painter, account, ID_SIZE)))
        .collect();
    let sizes: Vec<ChipSize> = names
        .iter()
        .zip(&ids)
        .map(|(name, id)| ChipSize {
            name: name.size().x,
            id: id.as_ref().map(|id| id.size().x),
        })
        .collect();
    let plan = plan_chips(room - MARK_WIDTH - ITEM_GAP, &sizes);
    let mut items = vec![Placed {
        kind: ItemKind::Mark,
        x: 0.0,
        w: MARK_WIDTH,
        text: None,
        id: None,
    }];
    let mut x = MARK_WIDTH + ITEM_GAP;
    for (index, (((row, name), id), width)) in rows
        .iter()
        .zip(&names)
        .zip(&ids)
        .zip(&plan.widths)
        .take(plan.shown)
        .enumerate()
    {
        let (text, id) = if plan.show_ids {
            (Some(Arc::clone(name)), id.clone())
        } else {
            (fitted(painter, name, &row.name, NAME_SIZE, width - CHIP_CHROME), None)
        };
        items.push(Placed {
            kind: ItemKind::Chip(index),
            x,
            w: *width,
            text,
            id,
        });
        x += width + ITEM_GAP;
    }
    if plan.shown < rows.len() {
        let hidden = rows.len() - plan.shown;
        items.push(Placed {
            kind: ItemKind::More(hidden),
            x,
            w: MORE_WIDTH,
            text: Some(measure(painter, &format!("+{hidden}"), ID_SIZE)),
            id: None,
        });
    }
    StripLayout {
        items,
        width: profiles_total(&plan),
    }
}

fn modes_layout(painter: &Painter, row: &Row, room: f32) -> StripLayout {
    let name = measure(painter, &row.name, NAME_SIZE);
    let captions: Vec<Arc<Galley>> = QuickStartMode::ALL
        .iter()
        .map(|mode| measure(painter, mode.inline_label(), MODE_SIZE))
        .collect();
    let natural: Vec<f32> = captions
        .iter()
        .map(|caption| caption.size().x + MODE_PAD * 2.0)
        .collect();
    let (lead, widths) = plan_modes(room, LEAD_CHROME + name.size().x, &natural);
    let mut items = vec![Placed {
        kind: ItemKind::Back,
        x: 0.0,
        w: lead,
        text: fitted(painter, &name, &row.name, NAME_SIZE, lead - LEAD_CHROME),
        id: None,
    }];
    let mut x = lead + ITEM_GAP;
    for ((mode, caption), width) in QuickStartMode::ALL.iter().zip(&captions).zip(&widths) {
        let inner = width - MODE_PAD * 2.0;
        let text = if caption.size().x <= inner {
            Arc::clone(caption)
        } else {
            hub_ui::elided(painter, mode.inline_label(), FontId::proportional(MODE_SIZE), inner)
        };
        items.push(Placed {
            kind: ItemKind::Mode(*mode),
            x,
            w: *width,
            text: Some(text),
            id: None,
        });
        x += width + ITEM_GAP;
    }
    items.push(Placed {
        kind: ItemKind::Close,
        x,
        w: CLOSE_WIDTH,
        text: None,
        id: None,
    });
    StripLayout {
        items,
        width: modes_total(lead, &widths),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_marker_is_inert() {
        assert!(ItemKind::Launch.interactive());
        assert!(ItemKind::Chip(3).interactive());
        assert!(!ItemKind::More(2).interactive());
        assert_eq!(ItemKind::More(2).event(), None);
        assert_eq!(ItemKind::Close.event(), Some(StripEvent::Dismiss));
        assert_eq!(ItemKind::Chip(3).event(), Some(StripEvent::Pick(3)));
    }
}
