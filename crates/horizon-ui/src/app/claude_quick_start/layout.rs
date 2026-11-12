//! Width planning of the inline quick-start strip. Pure: text widths come in as numbers, so the
//! rules (drop e-mail ids first, then elide names, never exceed the room) are testable without
//! fonts.

use crate::app::util::usize_to_f32;

pub(super) const CHIP_PAD: f32 = 7.0;
pub(super) const DOT: f32 = 6.0;
pub(super) const DOT_GAP: f32 = 5.0;
pub(super) const ID_GAP: f32 = 6.0;
/// Space between the strip's items.
pub(super) const ITEM_GAP: f32 = 4.0;
/// Leading star that collapses the profile chips.
pub(super) const MARK_WIDTH: f32 = 22.0;
pub(super) const CLOSE_WIDTH: f32 = 20.0;
/// Width of the `+N` marker for profiles that do not fit.
pub(super) const MORE_WIDTH: f32 = 26.0;
pub(super) const MODE_PAD: f32 = 9.0;
/// A mode button never gets narrower than this (an initial and an ellipsis).
pub(super) const MODE_MIN: f32 = 28.0;
/// Below this room a chip shows its dot only.
pub(super) const NAME_MIN_VISIBLE: f32 = 14.0;
/// Star drawn in front of the profile name on the mode page.
pub(super) const STAR_WIDTH: f32 = 10.0;
/// Fixed chrome of a profile chip: padding, dot and the gap after it.
pub(super) const CHIP_CHROME: f32 = CHIP_PAD * 2.0 + DOT + DOT_GAP;
/// A chip with no text at all.
pub(super) const CHIP_MIN: f32 = CHIP_PAD * 2.0 + DOT;
/// Fixed chrome of the profile lead on the mode page.
pub(super) const LEAD_CHROME: f32 = CHIP_PAD * 2.0 + STAR_WIDTH + DOT_GAP;
/// The lead with only its star.
pub(super) const LEAD_MIN: f32 = CHIP_PAD * 2.0 + STAR_WIDTH;

/// An item that wants `natural` pixels and can shrink to `min`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FitItem {
    pub(super) natural: f32,
    pub(super) min: f32,
}

/// Widths for `items` laid out with `gap` between them inside `avail`: natural when everything
/// fits, otherwise the widest ones shrink first (one shared ceiling), none below its minimum.
/// When even the minimums overflow, the minimums are returned.
#[must_use]
pub(super) fn fit_widths(items: &[FitItem], gap: f32, avail: f32) -> Vec<f32> {
    let gaps = gap * usize_to_f32(items.len().saturating_sub(1));
    let budget = avail - gaps;
    let floor = |item: &FitItem| item.min.min(item.natural);
    let natural_total: f32 = items.iter().map(|item| item.natural).sum();
    if natural_total <= budget {
        return items.iter().map(|item| item.natural).collect();
    }
    let min_total: f32 = items.iter().map(floor).sum();
    if min_total >= budget {
        return items.iter().map(floor).collect();
    }
    let at = |level: f32| -> f32 { items.iter().map(|item| level.clamp(floor(item), item.natural)).sum() };
    let mut lo = 0.0_f32;
    let mut hi = items.iter().map(|item| item.natural).fold(0.0_f32, f32::max);
    for _ in 0..32 {
        let mid = (lo + hi) * 0.5;
        if at(mid) > budget {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    items.iter().map(|item| lo.clamp(floor(item), item.natural)).collect()
}

/// Measured text of one profile chip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChipSize {
    pub(super) name: f32,
    /// Account identifier width; `None` when the account is unknown.
    pub(super) id: Option<f32>,
}

/// How the profile chips fill the room.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChipsPlan {
    /// Account identifiers are drawn (only when everything fits with them).
    pub(super) show_ids: bool,
    /// Width of each shown chip.
    pub(super) widths: Vec<f32>,
    /// How many chips are shown; the rest collapse into a `+N` marker.
    pub(super) shown: usize,
}

/// Plans the chips inside `avail`. Order of sacrifice: identifiers, then names (elided), then
/// whole chips behind a `+N` marker.
#[must_use]
pub(super) fn plan_chips(avail: f32, chips: &[ChipSize]) -> ChipsPlan {
    let with_ids: Vec<FitItem> = chips
        .iter()
        .map(|chip| FitItem {
            natural: CHIP_CHROME + chip.name + chip.id.map_or(0.0, |id| ID_GAP + id),
            min: CHIP_MIN,
        })
        .collect();
    let gaps = ITEM_GAP * usize_to_f32(chips.len().saturating_sub(1));
    let show_ids = chips.iter().any(|chip| chip.id.is_some())
        && with_ids.iter().map(|item| item.natural).sum::<f32>() + gaps <= avail;
    let items: Vec<FitItem> = if show_ids {
        with_ids
    } else {
        chips
            .iter()
            .map(|chip| FitItem {
                natural: CHIP_CHROME + chip.name,
                min: CHIP_MIN,
            })
            .collect()
    };
    let widths = fit_widths(&items, ITEM_GAP, avail);
    let total = |count: usize| -> f32 {
        let used: f32 = widths.iter().take(count).sum();
        let mut width = used + ITEM_GAP * usize_to_f32(count.saturating_sub(1));
        if count < widths.len() {
            width += ITEM_GAP + MORE_WIDTH;
        }
        width
    };
    let mut shown = widths.len();
    while shown > 1 && total(shown) > avail {
        shown -= 1;
    }
    ChipsPlan {
        show_ids,
        widths,
        shown,
    }
}

/// Widths of the mode page: `(lead, one per label)`. The close button keeps [`CLOSE_WIDTH`]. The
/// profile name in the lead is elided first, then the labels shrink together.
#[must_use]
pub(super) fn plan_modes(avail: f32, lead_natural: f32, labels: &[f32]) -> (f32, Vec<f32>) {
    let gaps = ITEM_GAP * usize_to_f32(labels.len() + 1);
    let labels_natural: f32 = labels.iter().sum();
    let fixed = CLOSE_WIDTH + gaps + labels_natural;
    let lead = (avail - fixed).clamp(LEAD_MIN.min(lead_natural), lead_natural.max(LEAD_MIN));
    let budget = avail - lead - CLOSE_WIDTH - ITEM_GAP * 2.0;
    let items: Vec<FitItem> = labels
        .iter()
        .map(|width| FitItem {
            natural: *width,
            min: MODE_MIN,
        })
        .collect();
    (lead, fit_widths(&items, ITEM_GAP, budget))
}

/// Total width of the mode page for the planned `lead` and `labels`.
#[must_use]
pub(super) fn modes_total(lead: f32, labels: &[f32]) -> f32 {
    lead + labels.iter().sum::<f32>() + CLOSE_WIDTH + ITEM_GAP * usize_to_f32(labels.len() + 1)
}

/// Total width of the profile page: star, shown chips and the `+N` marker.
#[must_use]
pub(super) fn profiles_total(plan: &ChipsPlan) -> f32 {
    let chips: f32 = plan.widths.iter().take(plan.shown).sum();
    let mut width = MARK_WIDTH + chips + ITEM_GAP * usize_to_f32(plan.shown);
    if plan.shown < plan.widths.len() {
        width += ITEM_GAP + MORE_WIDTH;
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    fn sum(widths: &[f32]) -> f32 {
        widths.iter().sum()
    }

    fn chips(names: &[f32], id: Option<f32>) -> Vec<ChipSize> {
        names.iter().map(|name| ChipSize { name: *name, id }).collect()
    }

    #[test]
    fn items_keep_their_natural_width_when_there_is_room() {
        let items = [
            FitItem {
                natural: 50.0,
                min: 10.0,
            },
            FitItem {
                natural: 70.0,
                min: 10.0,
            },
        ];

        assert_eq!(fit_widths(&items, 4.0, 200.0), vec![50.0, 70.0]);
    }

    #[test]
    fn the_widest_items_shrink_first_and_the_total_fits() {
        let items = [
            FitItem {
                natural: 40.0,
                min: 10.0,
            },
            FitItem {
                natural: 100.0,
                min: 10.0,
            },
            FitItem {
                natural: 100.0,
                min: 10.0,
            },
        ];

        let widths = fit_widths(&items, 4.0, 150.0);

        assert!(close(widths[0], 40.0), "small item untouched: {widths:?}");
        assert!(close(widths[1], widths[2]));
        assert!(close(sum(&widths) + 8.0, 150.0), "fills the room: {widths:?}");
    }

    #[test]
    fn items_never_drop_below_their_minimum() {
        let items = [
            FitItem {
                natural: 90.0,
                min: 30.0,
            },
            FitItem {
                natural: 90.0,
                min: 30.0,
            },
        ];

        let widths = fit_widths(&items, 4.0, 20.0);

        assert_eq!(widths, vec![30.0, 30.0]);
    }

    #[test]
    fn roomy_titlebar_shows_names_and_ids_in_full() {
        let plan = plan_chips(600.0, &chips(&[40.0, 50.0], Some(30.0)));

        assert!(plan.show_ids);
        assert_eq!(plan.shown, 2);
        assert!(close(plan.widths[0], CHIP_CHROME + 40.0 + ID_GAP + 30.0));
    }

    #[test]
    fn tight_titlebar_drops_ids_before_it_elides_names() {
        let sizes = chips(&[40.0, 40.0, 40.0, 40.0], Some(50.0));
        // Names alone: 4 * (25 + 40) + 3 * 4 = 272; with ids it would be 4 * 121 + 12.
        let plan = plan_chips(280.0, &sizes);

        assert!(!plan.show_ids);
        assert!(plan.widths.iter().all(|width| close(*width, CHIP_CHROME + 40.0)));
    }

    #[test]
    fn narrower_still_elides_names_and_stays_inside_the_room() {
        let sizes = chips(&[60.0, 60.0, 60.0, 60.0], Some(50.0));
        let plan = plan_chips(200.0, &sizes);

        assert!(!plan.show_ids);
        assert_eq!(plan.shown, 4);
        assert!(plan.widths.iter().all(|width| *width < CHIP_CHROME + 60.0));
        assert!(plan.widths.iter().all(|width| *width >= CHIP_MIN));
        assert!(profiles_total(&plan) - MARK_WIDTH - ITEM_GAP <= 200.0 + 0.01);
    }

    #[test]
    fn too_many_profiles_collapse_into_a_counter_that_still_fits() {
        let sizes = chips(&[30.0; 10], None);
        let avail = 130.0;
        let plan = plan_chips(avail, &sizes);

        assert!(plan.shown >= 1 && plan.shown < 10);
        assert!(profiles_total(&plan) - MARK_WIDTH - ITEM_GAP <= avail + 0.01);
    }

    #[test]
    fn unknown_accounts_never_turn_ids_on() {
        let plan = plan_chips(900.0, &chips(&[40.0], None));

        assert!(!plan.show_ids);
    }

    #[test]
    fn mode_page_keeps_everything_when_there_is_room() {
        let (lead, labels) = plan_modes(600.0, 80.0, &[40.0, 70.0, 70.0]);

        assert!(close(lead, 80.0));
        assert_eq!(labels, vec![40.0, 70.0, 70.0]);
        assert!(modes_total(lead, &labels) <= 600.0);
    }

    #[test]
    fn mode_page_elides_the_profile_name_before_the_buttons() {
        let labels = [40.0, 70.0, 70.0];
        // Fixed part: 180 labels + 20 close + 16 gaps = 216; 260 leaves 44 for the lead.
        let (lead, planned) = plan_modes(260.0, 120.0, &labels);

        assert!(close(lead, 44.0), "{lead}");
        assert_eq!(planned, labels.to_vec());
        assert!(close(modes_total(lead, &planned), 260.0));
    }

    #[test]
    fn mode_page_shrinks_buttons_only_after_the_name_is_gone() {
        let (lead, planned) = plan_modes(190.0, 120.0, &[40.0, 70.0, 70.0]);

        assert!(close(lead, LEAD_MIN));
        assert!(planned.iter().all(|width| *width >= MODE_MIN));
        assert!(planned[1] < 70.0 && planned[2] < 70.0);
        assert!(modes_total(lead, &planned) <= 190.0 + 0.01);
    }
}
