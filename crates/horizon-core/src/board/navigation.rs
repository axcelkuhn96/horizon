use crate::panel::PanelId;

use super::Board;

/// Cardinal direction for spatial panel navigation.
///
/// The canvas uses screen coordinates where `position[1]` (y) increases
/// *downward*, so [`Direction::Up`] means smaller y and [`Direction::Down`]
/// means larger y.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Axis-aligned rectangle described by its top-left position and size, both in
/// canvas coordinates. Mirrors [`crate::panel::PanelLayout`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelRect {
    pub position: [f32; 2],
    pub size: [f32; 2],
}

impl PanelRect {
    #[must_use]
    fn center(&self) -> [f32; 2] {
        [
            self.position[0] + self.size[0] * 0.5,
            self.position[1] + self.size[1] * 0.5,
        ]
    }
}

/// Pick the nearest candidate panel lying in the half-plane of `dir` relative
/// to `from`.
///
/// Geometry uses panel **centers** (simple and robust against differing panel
/// sizes). A candidate qualifies when its center is strictly on the `dir` side
/// of `from`'s center along the relevant axis:
/// - `Left`: candidate.center.x < from.center.x
/// - `Right`: candidate.center.x > from.center.x
/// - `Up`: candidate.center.y < from.center.y  (smaller y is up)
/// - `Down`: candidate.center.y > from.center.y
///
/// Among qualifying candidates the nearest is chosen by:
/// 1. primary key — absolute delta along the direction axis (how far in `dir`)
/// 2. tiebreak — absolute deviation on the orthogonal axis (how far off-line)
///
/// Returns `None` when no candidate qualifies.
#[must_use]
pub fn nearest_in_direction(from: PanelRect, candidates: &[(PanelId, PanelRect)], dir: Direction) -> Option<PanelId> {
    let from_center = from.center();
    let mut best: Option<(PanelId, f32, f32)> = None;

    for (id, rect) in candidates {
        let center = rect.center();
        let dx = center[0] - from_center[0];
        let dy = center[1] - from_center[1];

        let (primary, orthogonal) = match dir {
            Direction::Left => {
                if dx >= 0.0 {
                    continue;
                }
                (dx.abs(), dy.abs())
            }
            Direction::Right => {
                if dx <= 0.0 {
                    continue;
                }
                (dx.abs(), dy.abs())
            }
            Direction::Up => {
                if dy >= 0.0 {
                    continue;
                }
                (dy.abs(), dx.abs())
            }
            Direction::Down => {
                if dy <= 0.0 {
                    continue;
                }
                (dy.abs(), dx.abs())
            }
        };

        let is_better = best.as_ref().is_none_or(|(_, best_primary, best_orthogonal)| {
            primary < *best_primary
                || ((primary - *best_primary).abs() <= f32::EPSILON && orthogonal < *best_orthogonal)
        });
        if is_better {
            best = Some((*id, primary, orthogonal));
        }
    }

    best.map(|(id, _, _)| id)
}

impl Board {
    /// Find the panel nearest to `from` in the given `dir`, considering only
    /// panels in the same (active) workspace as `from`.
    ///
    /// Returns `None` when `from` is not a known panel, its workspace is
    /// unknown, or no panel qualifies in that direction.
    #[must_use]
    pub fn panel_in_direction(&self, from: PanelId, dir: Direction) -> Option<PanelId> {
        let workspace_id = self.panel_workspace_id(from)?;
        let from_panel = self.panel(from)?;
        let from_rect = PanelRect {
            position: from_panel.layout.position,
            size: from_panel.layout.size,
        };

        let candidates: Vec<(PanelId, PanelRect)> = self
            .panels
            .iter()
            .filter(|panel| panel.id != from && panel.workspace_id == workspace_id)
            .map(|panel| {
                (
                    panel.id,
                    PanelRect {
                        position: panel.layout.position,
                        size: panel.layout.size,
                    },
                )
            })
            .collect();

        nearest_in_direction(from_rect, &candidates, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::{Direction, PanelRect, nearest_in_direction};
    use crate::panel::PanelId;

    const SIZE: [f32; 2] = [100.0, 100.0];

    fn rect(x: f32, y: f32) -> PanelRect {
        PanelRect {
            position: [x, y],
            size: SIZE,
        }
    }

    /// A 2x2 grid of panels:
    ///   id 1 (top-left)  id 2 (top-right)
    ///   id 3 (bot-left)  id 4 (bot-right)
    fn grid() -> Vec<(PanelId, PanelRect)> {
        vec![
            (PanelId(1), rect(0.0, 0.0)),
            (PanelId(2), rect(200.0, 0.0)),
            (PanelId(3), rect(0.0, 200.0)),
            (PanelId(4), rect(200.0, 200.0)),
        ]
    }

    fn others(all: &[(PanelId, PanelRect)], skip: PanelId) -> Vec<(PanelId, PanelRect)> {
        all.iter().filter(|(id, _)| *id != skip).copied().collect()
    }

    #[test]
    fn finds_each_neighbor_from_top_left_corner() {
        let all = grid();
        let from = rect(0.0, 0.0); // id 1
        let candidates = others(&all, PanelId(1));

        assert_eq!(
            nearest_in_direction(from, &candidates, Direction::Right),
            Some(PanelId(2))
        );
        assert_eq!(
            nearest_in_direction(from, &candidates, Direction::Down),
            Some(PanelId(3))
        );
        assert_eq!(nearest_in_direction(from, &candidates, Direction::Left), None);
        assert_eq!(nearest_in_direction(from, &candidates, Direction::Up), None);
    }

    #[test]
    fn finds_each_neighbor_from_bottom_right_corner() {
        let all = grid();
        let from = rect(200.0, 200.0); // id 4
        let candidates = others(&all, PanelId(4));

        assert_eq!(
            nearest_in_direction(from, &candidates, Direction::Left),
            Some(PanelId(3))
        );
        assert_eq!(nearest_in_direction(from, &candidates, Direction::Up), Some(PanelId(2)));
        assert_eq!(nearest_in_direction(from, &candidates, Direction::Right), None);
        assert_eq!(nearest_in_direction(from, &candidates, Direction::Down), None);
    }

    #[test]
    fn no_candidate_returns_none() {
        let from = rect(0.0, 0.0);
        assert_eq!(nearest_in_direction(from, &[], Direction::Right), None);
    }

    #[test]
    fn tie_on_axis_broken_by_smaller_orthogonal_deviation() {
        // Two candidates to the right at the same x delta; the one with the
        // smaller vertical deviation wins.
        let from = rect(0.0, 0.0);
        let aligned = (PanelId(10), rect(200.0, 0.0)); // dy = 0
        let off_line = (PanelId(11), rect(200.0, 300.0)); // dy = 300
        let candidates = vec![off_line, aligned];

        assert_eq!(
            nearest_in_direction(from, &candidates, Direction::Right),
            Some(PanelId(10))
        );
    }

    #[test]
    fn nearest_along_axis_wins_over_farther() {
        let from = rect(0.0, 0.0);
        let near = (PanelId(20), rect(150.0, 0.0));
        let far = (PanelId(21), rect(600.0, 0.0));
        let candidates = vec![far, near];

        assert_eq!(
            nearest_in_direction(from, &candidates, Direction::Right),
            Some(PanelId(20))
        );
    }
}
