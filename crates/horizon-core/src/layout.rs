use crate::panel::DEFAULT_PANEL_SIZE;

pub(crate) const TILE_GAP: f32 = 20.0;
pub(crate) const WS_INNER_PAD: f32 = 20.0;
pub(crate) const WORKSPACE_GAP: f32 = 80.0;

/// Padding around workspace panel bounds for the visual frame (left, right, bottom).
pub(crate) const WS_FRAME_PAD: f32 = 16.0;
/// Extra top padding for the workspace title bar area.
pub(crate) const WS_FRAME_TOP_EXTRA: f32 = 38.0;
/// Size of an empty workspace placeholder.
pub(crate) const WS_EMPTY_FRAME_SIZE: [f32; 2] = [304.0, 154.0];
/// Minimum gap maintained between workspace frames during collision resolution.
pub(crate) const WS_COLLISION_GAP: f32 = 10.0;

pub(crate) fn workspace_slot_width() -> f32 {
    let columns = 3.0;
    let content = columns * DEFAULT_PANEL_SIZE[0] + (columns - 1.0) * TILE_GAP;
    content + 2.0 * WS_INNER_PAD + WORKSPACE_GAP
}

pub(crate) fn tiled_panel_position(origin: [f32; 2], index: usize) -> [f32; 2] {
    let column = usize_to_f32(index % 3);
    let row = usize_to_f32(index / 3);
    [
        origin[0] + WS_INNER_PAD + column * (DEFAULT_PANEL_SIZE[0] + TILE_GAP),
        origin[1] + WS_INNER_PAD + row * (DEFAULT_PANEL_SIZE[1] + TILE_GAP),
    ]
}

pub(crate) fn ceil_sqrt_usize(value: usize) -> usize {
    if value <= 1 {
        return value;
    }

    let mut root = 1usize;
    while root.saturating_mul(root) < value {
        root += 1;
    }
    root
}

pub(crate) fn usize_to_f32(value: usize) -> f32 {
    let clamped = u16::try_from(value).unwrap_or(u16::MAX);
    f32::from(clamped)
}

/// Tolerance when deciding that two panel rectangles overlap; edges that merely touch do not.
const OVERLAP_EPSILON: f32 = 1.0;

fn rects_overlap(a_pos: [f32; 2], a_size: [f32; 2], b_pos: [f32; 2], b_size: [f32; 2]) -> bool {
    a_pos[0] < b_pos[0] + b_size[0] - OVERLAP_EPSILON
        && b_pos[0] < a_pos[0] + a_size[0] - OVERLAP_EPSILON
        && a_pos[1] < b_pos[1] + b_size[1] - OVERLAP_EPSILON
        && b_pos[1] < a_pos[1] + a_size[1] - OVERLAP_EPSILON
}

/// First tile slot (row-major, three columns) where a panel of `size` overlaps none of the
/// `occupied` `(position, size)` rectangles. Panels the user resized or moved off the grid are
/// respected; when no slot of the grid is free the panel goes below everything.
pub(crate) fn first_free_tile_slot(origin: [f32; 2], size: [f32; 2], occupied: &[([f32; 2], [f32; 2])]) -> [f32; 2] {
    let search_limit = occupied.len().saturating_mul(3).saturating_add(3);
    for index in 0..=search_limit {
        let candidate = tiled_panel_position(origin, index);
        if !occupied
            .iter()
            .any(|(position, other)| rects_overlap(candidate, size, *position, *other))
        {
            return candidate;
        }
    }
    let bottom = occupied
        .iter()
        .map(|(position, other)| position[1] + other[1])
        .fold(origin[1] + WS_INNER_PAD - TILE_GAP, f32::max);
    [origin[0] + WS_INNER_PAD, bottom + TILE_GAP]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: [f32; 2] = [100.0, 50.0];
    const SIZE: [f32; 2] = [200.0, 100.0];

    #[test]
    fn empty_workspace_gets_the_first_tile() {
        assert_eq!(first_free_tile_slot(ORIGIN, SIZE, &[]), tiled_panel_position(ORIGIN, 0));
    }

    #[test]
    fn default_sized_panels_on_the_grid_fill_tiles_in_order() {
        let first = tiled_panel_position(ORIGIN, 0);

        assert_eq!(
            first_free_tile_slot(ORIGIN, DEFAULT_PANEL_SIZE, &[(first, DEFAULT_PANEL_SIZE)]),
            tiled_panel_position(ORIGIN, 1)
        );
    }

    #[test]
    fn a_wide_panel_blocks_every_tile_it_covers() {
        let first = tiled_panel_position(ORIGIN, 0);
        let wide = [DEFAULT_PANEL_SIZE[0] * 2.0 + TILE_GAP, DEFAULT_PANEL_SIZE[1]];

        let slot = first_free_tile_slot(ORIGIN, DEFAULT_PANEL_SIZE, &[(first, wide)]);

        assert_eq!(slot, tiled_panel_position(ORIGIN, 2));
    }

    #[test]
    fn a_moved_panel_off_the_grid_still_blocks_the_tile_under_it() {
        let first = tiled_panel_position(ORIGIN, 0);
        let nudged = [first[0] + 40.0, first[1] + 30.0];

        let slot = first_free_tile_slot(ORIGIN, DEFAULT_PANEL_SIZE, &[(nudged, DEFAULT_PANEL_SIZE)]);

        assert_ne!(slot, first);
        assert!(!rects_overlap(slot, DEFAULT_PANEL_SIZE, nudged, DEFAULT_PANEL_SIZE));
    }

    #[test]
    fn full_grid_places_the_panel_below_everything() {
        let tall = [DEFAULT_PANEL_SIZE[0], 5000.0];
        let occupied: Vec<_> = (0..3)
            .map(|index| (tiled_panel_position(ORIGIN, index), tall))
            .collect();

        let slot = first_free_tile_slot(ORIGIN, DEFAULT_PANEL_SIZE, &occupied);

        assert!(slot[1] >= ORIGIN[1] + WS_INNER_PAD + 5000.0);
        assert!(
            occupied
                .iter()
                .all(|(position, size)| !rects_overlap(slot, DEFAULT_PANEL_SIZE, *position, *size))
        );
    }

    #[test]
    fn touching_edges_do_not_count_as_overlap() {
        let a = [0.0, 0.0];
        let b = [SIZE[0], 0.0];

        assert!(!rects_overlap(a, SIZE, b, SIZE));
        assert!(rects_overlap(a, SIZE, [SIZE[0] - 5.0, 0.0], SIZE));
    }
}
