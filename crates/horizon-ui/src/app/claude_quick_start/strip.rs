//! Per-frame interaction of the quick-start strip inside the titlebar row: it senses every item,
//! decodes clicks / Esc / outside presses into [`StripEvent`]s and hands [`paint`](super::paint) a
//! [`StripView`]. State is in [`super::state`], item geometry in [`super::placement`], the actual
//! start in [`super::actions`].

use std::sync::Arc;
use std::time::Instant;

use egui::{
    Color32, Context, FontId, Galley, Key, Modifiers, Painter, Pos2, Rect, Response, Sense, Ui, Vec2, WidgetInfo,
    WidgetType,
};
use horizon_core::PanelId;

use crate::app::HorizonApp;
use crate::app::hub_ui;
use crate::theme;

use super::placement::{ItemKind, Placed, StripLayout};
use super::state::Row;
use super::titlebar::{self, STRIP_GAP, STRIP_HEIGHT};
use super::{Availability, Stage, StripEvent};

const NOTICE_SIZE: f32 = 10.5;
pub(super) const NOTICE_PAD: f32 = 7.0;
/// Narrower than this the notice is not worth drawing.
const NOTICE_MIN_WIDTH: f32 = 48.0;
/// Room kept free at the titlebar's left edge for the workspace accent.
const NOTICE_LEFT_MARGIN: f32 = 8.0;

/// One item as drawn this frame.
pub(super) struct ViewItem {
    pub(super) rect: Rect,
    pub(super) kind: ItemKind,
    pub(super) accent: Color32,
    pub(super) text: Option<Arc<Galley>>,
    pub(super) id: Option<Arc<Galley>>,
    pub(super) dim: bool,
    pub(super) hovered: bool,
    pub(super) pressed: bool,
    pub(super) focused: bool,
}

/// The items of a strip. The collapsed strip is a single button that every shell panel draws
/// every frame, so it is kept out of a `Vec`.
pub(super) enum ViewItems {
    Launch(ViewItem),
    Open(Vec<ViewItem>),
}

impl ViewItems {
    pub(super) fn as_slice(&self) -> &[ViewItem] {
        match self {
            Self::Launch(item) => std::slice::from_ref(item),
            Self::Open(items) => items,
        }
    }
}

/// The transient "why nothing happened" label next to the strip.
pub(super) struct NoticeView {
    pub(super) rect: Rect,
    pub(super) text: Arc<Galley>,
}

/// Everything the painter needs for one panel's strip.
pub(in crate::app) struct StripView {
    /// Screen-independent (panel layer) rect of the whole strip.
    pub(in crate::app) rect: Rect,
    pub(super) items: ViewItems,
    pub(super) notice: Option<NoticeView>,
    /// `false` while the button cannot be used: drawn faded and inert.
    pub(in crate::app) enabled: bool,
    /// An item was clicked this frame.
    pub(in crate::app) clicked: bool,
}

/// What the panel draw code tells the strip about the titlebar this frame.
pub(in crate::app) struct StripArgs {
    pub(in crate::app) panel_id: PanelId,
    pub(in crate::app) panel_width: f32,
    /// Collapsed or renaming panel: no strip.
    pub(in crate::app) hidden: bool,
    pub(in crate::app) titlebar: Rect,
    /// X where the strip ends (left of the history meter or the controls).
    pub(in crate::app) right: f32,
    pub(in crate::app) interactive: bool,
    /// The pointer is over the panel's titlebar: only then is the shell probed for a busy job.
    pub(in crate::app) hovered: bool,
}

/// What is the same for every item of one panel's strip this frame.
struct Frame<'a> {
    ui: &'a Ui,
    panel_id: PanelId,
    stage: Stage,
    rect: Rect,
    availability: Availability,
    clickable: bool,
}

/// Profile an item belongs to, for its colour and dimming.
fn row_of(rows: &[Row], stage: Stage, kind: ItemKind) -> Option<&Row> {
    match (kind, stage) {
        (ItemKind::Chip(index), _) => rows.get(index),
        (ItemKind::Back | ItemKind::Mode(_), Stage::Modes { profile }) => rows.get(profile),
        (ItemKind::Back | ItemKind::Insert { .. }, Stage::PromptModes { prompt }) => rows.get(prompt),
        _ => None,
    }
}

/// [`accessible_label`], except that the chips, star and name of the prompt pages speak of
/// prompts rather than profiles.
fn stage_label(stage: Stage, kind: ItemKind, row: Option<&Row>) -> std::borrow::Cow<'static, str> {
    let name = row.map_or("", |row| row.name.as_str());
    match (stage, kind) {
        (Stage::Prompts, ItemKind::Chip(_)) => format!("Insert the prompt {name}").into(),
        (Stage::Prompts, ItemKind::Mark) => "Collapse the saved prompts".into(),
        (Stage::PromptModes { .. }, ItemKind::Back) => format!("{name}: back to the prompts").into(),
        _ => accessible_label(kind, row),
    }
}

fn accessible_label(kind: ItemKind, row: Option<&Row>) -> std::borrow::Cow<'static, str> {
    let name = row.map_or("", |row| row.name.as_str());
    match kind {
        ItemKind::Launch => "Start Claude in this terminal".into(),
        ItemKind::Mark => "Collapse the Claude profiles".into(),
        ItemKind::Chip(_) => format!("Start Claude with {name}").into(),
        ItemKind::Back => format!("{name}: back to profiles").into(),
        ItemKind::Mode(mode) => format!("{} Claude session with {name}", mode.label()).into(),
        ItemKind::Close => "Close the Claude quick start".into(),
        ItemKind::More(count) => format!("{count} more profiles").into(),
        ItemKind::PromptsEntry => "Insert a saved prompt".into(),
        ItemKind::Insert { send: false } => format!("Insert the prompt {name}").into(),
        ItemKind::Insert { send: true } => format!("Insert the prompt {name} and press Enter").into(),
    }
}

fn with_hover_text(
    response: Response,
    stage: Stage,
    kind: ItemKind,
    row: Option<&Row>,
    availability: Availability,
) -> Response {
    match kind {
        ItemKind::Launch => response.on_hover_text(availability.tooltip()),
        ItemKind::Mark => response.on_hover_text("Collapse"),
        ItemKind::PromptsEntry => response.on_hover_text("Insert a saved prompt into this terminal"),
        ItemKind::Insert { send: false } => response.on_hover_text("Type the prompt without pressing Enter"),
        ItemKind::Insert { send: true } => response.on_hover_text("Type the prompt and press Enter"),
        ItemKind::Chip(_) => response.on_hover_ui(|ui| {
            if let Some(row) = row {
                ui.label(&row.tooltip);
            }
        }),
        ItemKind::Back if matches!(stage, Stage::PromptModes { .. }) => response.on_hover_text("Back to prompts"),
        ItemKind::Back => response.on_hover_text("Back to profiles"),
        ItemKind::Mode(mode) => response.on_hover_text(mode.hint()),
        ItemKind::Close => response.on_hover_text("Close (Esc)"),
        ItemKind::More(count) => response.on_hover_ui(|ui| {
            ui.label(format!("{count} more profiles: widen the panel to pick them"));
        }),
    }
}

/// Senses one item and describes how to draw it, plus the event a click on it means.
fn sense_item(frame: &Frame<'_>, rows: &[Row], index: usize, placed: &Placed) -> (ViewItem, Option<StripEvent>) {
    let ui = frame.ui;
    let item_rect = Rect::from_min_size(
        Pos2::new(frame.rect.min.x + placed.x, frame.rect.min.y),
        Vec2::new(placed.w, frame.rect.height()),
    );
    let live = frame.clickable && placed.kind.interactive();
    let response = ui.interact(
        item_rect.expand2(Vec2::new(0.0, 2.0)),
        ui.make_persistent_id(("panel_quick_start", frame.panel_id.0, index)),
        if live { Sense::click() } else { Sense::hover() },
    );
    let row = row_of(rows, frame.stage, placed.kind);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, live, stage_label(frame.stage, placed.kind, row)));
    let response = with_hover_text(response, frame.stage, placed.kind, row, frame.availability);
    let event = if live && response.clicked() {
        placed.kind.event()
    } else {
        None
    };
    let item = ViewItem {
        rect: item_rect,
        kind: placed.kind,
        accent: row.map_or_else(theme::ACCENT, |row| row.accent),
        text: placed.text.clone(),
        id: placed.id.clone(),
        dim: row.is_some_and(|row| row.dim),
        hovered: live && response.hovered(),
        pressed: live && response.is_pointer_button_down_on(),
        focused: response.has_focus(),
    };
    (item, event)
}

/// Esc (unless a hub modal owns it) or a press outside `rect` dismisses the open strip.
fn dismissal(ctx: &Context, layer: egui::LayerId, rect: Rect, esc_free: bool) -> Option<StripEvent> {
    if esc_free && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        return Some(StripEvent::Dismiss);
    }
    if !ctx.input(|input| input.pointer.any_pressed()) {
        return None;
    }
    // The pointer is in screen space; the panel layer may be zoomed and panned.
    let screen = ctx
        .layer_transform_to_global(layer)
        .map_or(rect, |transform| transform * rect);
    let outside = ctx
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|pos| !screen.contains(pos));
    outside.then_some(StripEvent::Dismiss)
}

impl HorizonApp {
    /// Draws-and-senses the quick-start strip of one panel's titlebar. Returns what to paint, or
    /// `None` when this panel gets no strip (not a local shell, too narrow, hidden).
    pub(in crate::app) fn quick_start_strip(&mut self, ui: &Ui, args: &StripArgs) -> Option<StripView> {
        let panel_id = args.panel_id;
        let ctx = ui.ctx().clone();
        let availability = if args.hidden {
            None
        } else {
            self.quick_start_button(panel_id, args.panel_width, args.hovered)
        };
        let Some(availability) = availability else {
            self.hub.quick_start.collapse(panel_id);
            return None;
        };
        let enabled = availability.enabled();
        if !enabled {
            self.hub.quick_start.collapse(panel_id);
        }
        self.refresh_quick_start_rows(panel_id);

        let stage = self.hub.quick_start.stage_for(panel_id);
        let room = titlebar::strip_room(args.titlebar, args.right);
        let layout = self.hub.quick_start.layout_for(ui.painter(), panel_id, stage, room);
        let rect = titlebar::strip_rect(args.titlebar, args.right, layout.width);
        let frame = Frame {
            ui,
            panel_id,
            stage,
            rect,
            availability,
            clickable: args.interactive && enabled,
        };
        let (items, mut event) = self.sense_items(&frame, &layout);
        let clicked = event.is_some();

        if stage != Stage::Collapsed && event.is_none() {
            event = dismissal(&ctx, ui.layer_id(), rect, self.hub.claude_launcher.is_none());
        }
        if let Some(event) = event {
            self.apply_quick_start_event(&ctx, panel_id, stage, event);
            ctx.request_repaint();
        }
        let notice = self.notice_view(ui.painter(), args, rect);
        Some(StripView {
            rect,
            items,
            notice,
            enabled,
            clicked,
        })
    }

    /// Senses every item of `layout`; the first click wins.
    fn sense_items(&self, frame: &Frame<'_>, layout: &StripLayout) -> (ViewItems, Option<StripEvent>) {
        let rows: &[Row] = self
            .hub
            .quick_start
            .expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == frame.panel_id)
            .map_or(&[][..], |expanded| expanded.rows_for(frame.stage));
        if let [only] = layout.items.as_slice()
            && only.kind == ItemKind::Launch
        {
            let (item, event) = sense_item(frame, rows, 0, only);
            return (ViewItems::Launch(item), event);
        }
        let mut event = None;
        let mut items = Vec::with_capacity(layout.items.len());
        for (index, placed) in layout.items.iter().enumerate() {
            let (item, clicked) = sense_item(frame, rows, index, placed);
            if event.is_none() {
                event = clicked;
            }
            items.push(item);
        }
        (ViewItems::Open(items), event)
    }

    /// The failure label of a start that did not run, left of the strip, while it is fresh.
    fn notice_view(&mut self, painter: &Painter, args: &StripArgs, strip: Rect) -> Option<NoticeView> {
        let (failure, remaining) = self.hub.quick_start.notice_for(args.panel_id, Instant::now())?;
        // One repaint when it expires, so it disappears without waiting for other input.
        painter.ctx().request_repaint_after(remaining);
        let right = strip.min.x - STRIP_GAP;
        let room = right - args.titlebar.min.x - NOTICE_LEFT_MARGIN - NOTICE_PAD * 2.0;
        if room < NOTICE_MIN_WIDTH {
            return None;
        }
        let text = hub_ui::elided(painter, failure.message(), FontId::proportional(NOTICE_SIZE), room);
        let width = text.size().x + NOTICE_PAD * 2.0;
        let rect = Rect::from_min_size(
            Pos2::new(right - width, strip.center().y - STRIP_HEIGHT * 0.5),
            Vec2::new(width, STRIP_HEIGHT),
        );
        Some(NoticeView { rect, text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::claude_launcher::RowUsage;

    fn row() -> Row {
        Row::new("claude2".to_owned(), Color32::WHITE, None, &RowUsage::new(None))
    }

    #[test]
    fn items_resolve_their_profile_from_the_stage() {
        let rows = vec![row(), row()];

        assert!(row_of(&rows, Stage::Profiles, ItemKind::Chip(1)).is_some());
        assert!(row_of(&rows, Stage::Profiles, ItemKind::Chip(2)).is_none());
        assert!(row_of(&rows, Stage::Modes { profile: 1 }, ItemKind::Back).is_some());
        assert!(
            row_of(
                &rows,
                Stage::Profiles,
                ItemKind::Mode(super::super::QuickStartMode::New)
            )
            .is_none()
        );
        assert!(row_of(&rows, Stage::Modes { profile: 0 }, ItemKind::Close).is_none());
    }

    #[test]
    fn prompt_pages_resolve_their_row_and_speak_of_prompts() {
        let rows = vec![row(), row()];
        let page = Stage::PromptModes { prompt: 1 };

        assert!(row_of(&rows, page, ItemKind::Back).is_some());
        assert!(row_of(&rows, page, ItemKind::Insert { send: true }).is_some());
        assert!(row_of(&rows, Stage::PromptModes { prompt: 5 }, ItemKind::Back).is_none());
        assert!(row_of(&rows, Stage::Prompts, ItemKind::Chip(0)).is_some());
        assert_eq!(
            stage_label(Stage::Prompts, ItemKind::Chip(0), Some(&row())),
            "Insert the prompt claude2"
        );
        assert_eq!(
            stage_label(Stage::Profiles, ItemKind::Chip(0), Some(&row())),
            "Start Claude with claude2"
        );
        assert_eq!(
            stage_label(page, ItemKind::Insert { send: true }, Some(&row())),
            "Insert the prompt claude2 and press Enter"
        );
        assert_eq!(
            stage_label(Stage::Profiles, ItemKind::PromptsEntry, None),
            "Insert a saved prompt"
        );
    }

    #[test]
    fn the_collapsed_button_has_a_static_accessible_label() {
        assert!(matches!(
            accessible_label(ItemKind::Launch, None),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(
            accessible_label(ItemKind::Chip(0), Some(&row())),
            "Start Claude with claude2"
        );
    }

    #[test]
    fn a_single_item_is_viewed_without_a_vec() {
        let item = ViewItem {
            rect: Rect::ZERO,
            kind: ItemKind::Launch,
            accent: Color32::WHITE,
            text: None,
            id: None,
            dim: false,
            hovered: false,
            pressed: false,
            focused: false,
        };

        assert_eq!(ViewItems::Launch(item).as_slice().len(), 1);
        assert!(ViewItems::Open(Vec::new()).as_slice().is_empty());
    }

    /// Runs one frame with Esc pressed; returns the dismissal and whether Esc is still unread.
    fn escape_frame(esc_free: bool) -> (Option<StripEvent>, bool) {
        let ctx = Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let mut result = (None, false);
        let _ = ctx.run(input, |ctx| {
            let event = dismissal(ctx, egui::LayerId::background(), Rect::ZERO, esc_free);
            result = (event, ctx.input(|input| input.key_pressed(Key::Escape)));
        });
        result
    }

    #[test]
    fn escape_dismisses_the_strip_and_is_consumed() {
        assert_eq!(escape_frame(true), (Some(StripEvent::Dismiss), false));
    }

    #[test]
    fn escape_is_left_alone_while_a_hub_modal_is_open() {
        assert_eq!(escape_frame(false), (None, true));
    }
}
