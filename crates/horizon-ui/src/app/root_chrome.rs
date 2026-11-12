use egui::{Pos2, Rect, Vec2};

use super::usage_chips::{ChipWidths, ChipsPlan};
use super::{SIDEBAR_WIDTH, TOOLBAR_HEIGHT};

pub(super) const ROOT_TOOLBAR_BUTTON_HEIGHT: f32 = 30.0;
pub(super) const ROOT_TOOLBAR_BUTTON_GAP: f32 = 8.0;

const ROOT_TOOLBAR_HORIZONTAL_PAD: f32 = 14.0;
const ROOT_TOOLBAR_VERTICAL_PAD: f32 = 8.0;
const ROOT_TOOLBAR_CLUSTER_GAP: f32 = 12.0;
const ROOT_TOOLBAR_SEARCH_MIN_WIDTH: f32 = 180.0;
const ROOT_TOOLBAR_SEARCH_MAX_WIDTH: f32 = 420.0;
pub(super) const ROOT_TOOLBAR_FPS_WIDTH: f32 = 72.0;
const ROOT_TOOLBAR_MORE_WIDTH: f32 = 72.0;
const ROOT_TOOLBAR_NAME_WIDTH: f32 = 72.0;
const ROOT_TOOLBAR_TAGLINE_WIDTH: f32 = 324.0;
const SIDEBAR_WIDTH_RATIO: f32 = 0.18;

pub(super) const SIDEBAR_MIN_WIDTH: f32 = 168.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolbarAction {
    QuickNav,
    RemoteHosts,
    Sessions,
    Update,
    Settings,
}

impl ToolbarAction {
    const SECONDARY: [Self; 1] = [Self::RemoteHosts];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::QuickNav => "Quick Nav",
            Self::RemoteHosts => "Remote Hosts",
            Self::Sessions => "Sessions",
            Self::Update => "Update",
            Self::Settings => "Settings",
        }
    }

    fn estimated_width(self) -> f32 {
        estimated_toolbar_button_width(self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolbarItem {
    /// Claude usage chips; their width depends on the plan, see [`visible_items_width`].
    UsageChips,
    FpsMeter,
    Action(ToolbarAction),
    OverflowMenu,
}

impl ToolbarItem {
    /// Width of the item; the chips item is sized by the caller (`chips_width`).
    fn estimated_width(self, chips_width: f32) -> f32 {
        match self {
            Self::UsageChips => chips_width,
            Self::FpsMeter => ROOT_TOOLBAR_FPS_WIDTH,
            Self::Action(action) => action.estimated_width(),
            Self::OverflowMenu => ROOT_TOOLBAR_MORE_WIDTH,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct RootToolbarLayout {
    pub(super) brand_rect: Rect,
    pub(super) search_rect: Rect,
    pub(super) actions_rect: Rect,
    pub(super) visible_items: Vec<ToolbarItem>,
    pub(super) overflow_actions: Vec<ToolbarAction>,
    pub(super) show_tagline: bool,
    /// Presentation of the usage chips; `visible == 0` when none are inline.
    pub(super) chips: ChipsPlan,
}

/// One combination of optional toolbar parts to try, from richest to leanest.
#[derive(Clone, Copy)]
struct ToolbarState {
    show_tagline: bool,
    secondary_visible: usize,
    show_fps: bool,
}

const TOOLBAR_STATES: [ToolbarState; 5] = [
    ToolbarState {
        show_tagline: true,
        secondary_visible: 1,
        show_fps: true,
    },
    ToolbarState {
        show_tagline: false,
        secondary_visible: 1,
        show_fps: true,
    },
    ToolbarState {
        show_tagline: false,
        secondary_visible: 0,
        show_fps: true,
    },
    ToolbarState {
        show_tagline: false,
        secondary_visible: 1,
        show_fps: false,
    },
    ToolbarState {
        show_tagline: false,
        secondary_visible: 0,
        show_fps: false,
    },
];

struct RootToolbarCandidate {
    layout: RootToolbarLayout,
    search_available: f32,
}

pub(super) fn effective_sidebar_width(viewport_width: f32) -> f32 {
    (viewport_width * SIDEBAR_WIDTH_RATIO).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_WIDTH)
}

/// Lays the toolbar out for `viewport`. The search field shrinks to its minimum
/// first, then the optional parts go (tagline, fps, secondary actions) while
/// the usage chips try full and then compact; chips that still do not fit move
/// into the overflow menu, last profile first.
pub(super) fn root_toolbar_layout(viewport: Rect, show_update: bool, chips: &ChipWidths) -> RootToolbarLayout {
    let content_rect = Rect::from_min_max(
        Pos2::new(
            viewport.min.x + ROOT_TOOLBAR_HORIZONTAL_PAD,
            viewport.min.y + ROOT_TOOLBAR_VERTICAL_PAD,
        ),
        Pos2::new(
            viewport.max.x - ROOT_TOOLBAR_HORIZONTAL_PAD,
            viewport.min.y + TOOLBAR_HEIGHT - ROOT_TOOLBAR_VERTICAL_PAD,
        ),
    );
    let total = chips.len();
    let fits = |candidate: &RootToolbarCandidate| candidate.search_available >= ROOT_TOOLBAR_SEARCH_MIN_WIDTH;
    let build = |state: ToolbarState, plan: ChipsPlan| {
        layout_candidate(viewport, content_rect, state, plan, chips, show_update)
    };

    let presentations: &[bool] = if total == 0 { &[false] } else { &[false, true] };
    for &compact in presentations {
        let plan = ChipsPlan {
            compact,
            visible: total,
            total,
        };
        for state in TOOLBAR_STATES {
            let candidate = build(state, plan);
            if fits(&candidate) {
                return candidate.layout;
            }
        }
    }

    // Leanest toolbar; drop chips from the end until the rest fits.
    let leanest = TOOLBAR_STATES[TOOLBAR_STATES.len() - 1];
    for visible in (1..total).rev() {
        let candidate = build(
            leanest,
            ChipsPlan {
                compact: true,
                visible,
                total,
            },
        );
        if fits(&candidate) {
            return candidate.layout;
        }
    }
    // Not even one compact chip fits: they all live in the overflow menu.
    build(
        leanest,
        ChipsPlan {
            compact: true,
            visible: 0,
            total,
        },
    )
    .layout
}

fn layout_candidate(
    viewport: Rect,
    content_rect: Rect,
    state: ToolbarState,
    chips_plan: ChipsPlan,
    chips: &ChipWidths,
    show_update: bool,
) -> RootToolbarCandidate {
    let ToolbarState {
        show_tagline,
        secondary_visible,
        show_fps,
    } = state;
    let brand_width = ROOT_TOOLBAR_NAME_WIDTH
        + if show_tagline {
            ROOT_TOOLBAR_BUTTON_GAP + ROOT_TOOLBAR_TAGLINE_WIDTH
        } else {
            0.0
        };
    let brand_rect = Rect::from_min_size(content_rect.min, Vec2::new(brand_width, content_rect.height()));

    let mut visible_items = Vec::with_capacity(7);
    if chips_plan.visible > 0 {
        visible_items.push(ToolbarItem::UsageChips);
    }
    if show_fps {
        visible_items.push(ToolbarItem::FpsMeter);
    }
    visible_items.push(ToolbarItem::Action(ToolbarAction::QuickNav));
    for action in ToolbarAction::SECONDARY.iter().take(secondary_visible).copied() {
        visible_items.push(ToolbarItem::Action(action));
    }

    let overflow_actions = ToolbarAction::SECONDARY
        .iter()
        .skip(secondary_visible)
        .copied()
        .collect::<Vec<_>>();
    if !overflow_actions.is_empty() || chips_plan.overflowing() {
        visible_items.push(ToolbarItem::OverflowMenu);
    }
    if show_update {
        visible_items.push(ToolbarItem::Action(ToolbarAction::Update));
    }
    visible_items.push(ToolbarItem::Action(ToolbarAction::Sessions));
    visible_items.push(ToolbarItem::Action(ToolbarAction::Settings));

    let chips_width = chips.total(chips_plan.compact, chips_plan.visible);
    let actions_width = visible_items_width(&visible_items, chips_width);
    let actions_left = content_rect.max.x - actions_width;
    let actions_rect = Rect::from_min_size(
        Pos2::new(actions_left, content_rect.min.y),
        Vec2::new(actions_width, content_rect.height()),
    );

    let search_left_bound = brand_rect.max.x + ROOT_TOOLBAR_CLUSTER_GAP;
    let search_right_bound = actions_rect.min.x - ROOT_TOOLBAR_CLUSTER_GAP;
    let search_available = (search_right_bound - search_left_bound).max(0.0);
    let search_width = search_available.min(ROOT_TOOLBAR_SEARCH_MAX_WIDTH);
    let search_left = search_left_bound + ((search_available - search_width) * 0.5);
    let search_rect = Rect::from_center_size(
        Pos2::new(search_left + search_width * 0.5, viewport.min.y + TOOLBAR_HEIGHT * 0.5),
        Vec2::new(search_width, TOOLBAR_HEIGHT - 14.0),
    );

    RootToolbarCandidate {
        layout: RootToolbarLayout {
            brand_rect,
            search_rect,
            actions_rect,
            visible_items,
            overflow_actions,
            show_tagline,
            chips: chips_plan,
        },
        search_available,
    }
}

fn visible_items_width(items: &[ToolbarItem], chips_width: f32) -> f32 {
    let gaps = items
        .len()
        .saturating_sub(1)
        .try_into()
        .map_or(0.0, |count: u16| f32::from(count) * ROOT_TOOLBAR_BUTTON_GAP);
    items.iter().map(|item| item.estimated_width(chips_width)).sum::<f32>() + gaps
}

fn estimated_toolbar_button_width(label: &str) -> f32 {
    let chars = u16::try_from(label.chars().count()).map_or(f32::from(u16::MAX), f32::from);
    34.0 + chars * 7.2
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect};

    use super::{ToolbarAction, ToolbarItem, effective_sidebar_width, root_toolbar_layout};
    use crate::app::usage_chips::ChipWidths;
    use crate::app::{SIDEBAR_WIDTH, TOOLBAR_HEIGHT};

    #[test]
    fn sidebar_width_shrinks_on_narrow_desktop_viewports() {
        assert!((effective_sidebar_width(1600.0) - SIDEBAR_WIDTH).abs() <= f32::EPSILON);
        assert!(effective_sidebar_width(1024.0) < SIDEBAR_WIDTH);
        assert!((effective_sidebar_width(800.0) - super::SIDEBAR_MIN_WIDTH).abs() <= f32::EPSILON);
    }

    #[test]
    fn toolbar_hides_tagline_before_collapsing_actions() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1024.0, 768.0));
        let layout = root_toolbar_layout(viewport, false, &ChipWidths::none());

        assert!(!layout.show_tagline);
        assert!(layout.visible_items.contains(&ToolbarItem::FpsMeter));
        assert!(
            layout
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::Sessions))
        );
        assert!(layout.search_rect.width() >= 180.0);
    }

    #[test]
    fn toolbar_moves_secondary_actions_into_overflow_on_tighter_widths() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 768.0));
        let layout = root_toolbar_layout(viewport, false, &ChipWidths::none());

        assert!(!layout.show_tagline);
        assert_eq!(layout.overflow_actions, vec![ToolbarAction::RemoteHosts]);
        assert!(layout.visible_items.contains(&ToolbarItem::FpsMeter));
        assert!(layout.visible_items.contains(&ToolbarItem::OverflowMenu));
        assert!((layout.search_rect.center().y - TOOLBAR_HEIGHT * 0.5).abs() <= f32::EPSILON);
    }

    #[test]
    fn toolbar_keeps_primary_actions_visible_at_min_window_width() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(760.0, 600.0));
        let layout = root_toolbar_layout(viewport, false, &ChipWidths::none());

        assert!(
            layout
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::QuickNav))
        );
        assert!(
            layout
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::Sessions))
        );
        assert!(
            layout
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::Settings))
        );
    }

    #[test]
    fn toolbar_keeps_update_visible_when_present() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 600.0));
        let layout = root_toolbar_layout(viewport, true, &ChipWidths::none());

        assert!(
            layout
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::Update))
        );
        assert_eq!(layout.overflow_actions, vec![ToolbarAction::RemoteHosts]);
    }

    #[test]
    fn layout_hides_chips_when_disabled_or_no_profiles() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1600.0, 768.0));
        // Disabled bar and zero profiles both reach the layout as "no chips".
        let layout = root_toolbar_layout(viewport, false, &ChipWidths::none());

        assert!(!layout.visible_items.contains(&ToolbarItem::UsageChips));
        assert_eq!(layout.chips.visible, 0);
        assert_eq!(layout.chips.total, 0);
        assert!(!layout.chips.overflowing());
        assert!(layout.overflow_actions.is_empty());
        assert!(!layout.visible_items.contains(&ToolbarItem::OverflowMenu));
    }

    #[test]
    fn layout_shows_all_chips_full_when_there_is_room() {
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(1600.0, 768.0));
        let layout = root_toolbar_layout(viewport, false, &ChipWidths::estimated(1));

        assert!(layout.visible_items.contains(&ToolbarItem::UsageChips));
        assert_eq!(layout.chips.visible, 1);
        assert!(!layout.chips.compact);
        assert!(layout.search_rect.width() >= 180.0);
    }

    #[test]
    fn layout_compacts_then_overflows_when_narrow() {
        let chips = ChipWidths::estimated(4);

        // Four full chips never fit; all four compact do, with the tagline gone.
        let wide = root_toolbar_layout(Rect::from_min_max(Pos2::ZERO, Pos2::new(1600.0, 768.0)), false, &chips);
        assert!(wide.chips.compact);
        assert_eq!(wide.chips.visible, 4);
        assert!(!wide.chips.overflowing());

        // Narrower: some chips fall into the overflow menu, search keeps its minimum.
        let tight = root_toolbar_layout(Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 768.0)), false, &chips);
        assert!(tight.chips.compact);
        assert!(tight.chips.visible >= 1 && tight.chips.visible < 4);
        assert!(tight.chips.overflowing());
        assert!(tight.visible_items.contains(&ToolbarItem::OverflowMenu));
        assert!(tight.search_rect.width() >= 180.0);

        // At the minimum window width every chip is in the overflow menu and
        // the primary actions are still there.
        let minimum = root_toolbar_layout(Rect::from_min_max(Pos2::ZERO, Pos2::new(760.0, 600.0)), false, &chips);
        assert_eq!(minimum.chips.visible, 0);
        assert_eq!(minimum.chips.total, 4);
        assert!(!minimum.visible_items.contains(&ToolbarItem::UsageChips));
        assert!(minimum.visible_items.contains(&ToolbarItem::OverflowMenu));
        assert!(
            minimum
                .visible_items
                .contains(&ToolbarItem::Action(ToolbarAction::Settings))
        );
    }

    #[test]
    fn chips_never_mix_full_and_compact_and_stay_inside_the_actions_rect() {
        let chips = ChipWidths::estimated(3);
        for width in [820.0_f32, 900.0, 1100.0, 1300.0, 1500.0, 1900.0] {
            let layout = root_toolbar_layout(Rect::from_min_max(Pos2::ZERO, Pos2::new(width, 700.0)), false, &chips);
            let plan = layout.chips;

            assert_eq!(plan.total, 3);
            assert!(plan.visible <= plan.total);
            assert_eq!(
                layout.visible_items.contains(&ToolbarItem::UsageChips),
                plan.visible > 0,
                "width {width}"
            );
            assert!(
                chips.total(plan.compact, plan.visible) <= layout.actions_rect.width(),
                "width {width}"
            );
        }
    }
}
