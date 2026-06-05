use super::super::*;
use super::editor_panel_options;
use crate::panel::PanelId;

/// Place a panel at an explicit canvas position with a known size so direction
/// math is deterministic regardless of auto-layout.
fn place(board: &mut Board, id: PanelId, x: f32, y: f32) {
    let panel = board.panel_mut(id).expect("panel exists");
    panel.move_to([x, y]);
    panel.resize_layout([100.0, 100.0]);
}

fn spawn(board: &mut Board, workspace_id: WorkspaceId) -> PanelId {
    board
        .create_panel(editor_panel_options(), workspace_id)
        .expect("panel should spawn")
}

#[test]
fn panel_in_direction_walks_a_2x2_grid() {
    let mut board = Board::new();
    let ws = board.create_workspace("grid");
    let tl = spawn(&mut board, ws);
    let tr = spawn(&mut board, ws);
    let bl = spawn(&mut board, ws);
    let br = spawn(&mut board, ws);

    place(&mut board, tl, 0.0, 0.0);
    place(&mut board, tr, 200.0, 0.0);
    place(&mut board, bl, 0.0, 200.0);
    place(&mut board, br, 200.0, 200.0);

    assert_eq!(board.panel_in_direction(tl, Direction::Right), Some(tr));
    assert_eq!(board.panel_in_direction(tl, Direction::Down), Some(bl));
    assert_eq!(board.panel_in_direction(br, Direction::Left), Some(bl));
    assert_eq!(board.panel_in_direction(br, Direction::Up), Some(tr));
}

#[test]
fn panel_in_direction_returns_none_at_grid_edge() {
    let mut board = Board::new();
    let ws = board.create_workspace("grid");
    let tl = spawn(&mut board, ws);
    let tr = spawn(&mut board, ws);

    place(&mut board, tl, 0.0, 0.0);
    place(&mut board, tr, 200.0, 0.0);

    assert_eq!(board.panel_in_direction(tl, Direction::Left), None);
    assert_eq!(board.panel_in_direction(tl, Direction::Up), None);
}

#[test]
fn panel_in_direction_ignores_other_workspaces() {
    let mut board = Board::new();
    let ws_a = board.create_workspace("a");
    let ws_b = board.create_workspace("b");

    let a = spawn(&mut board, ws_a);
    let b = spawn(&mut board, ws_b);

    place(&mut board, a, 0.0, 0.0);
    // `b` sits to the right of `a` in canvas space but lives in another
    // workspace, so it must not be reachable from `a`.
    place(&mut board, b, 200.0, 0.0);

    assert_eq!(board.panel_in_direction(a, Direction::Right), None);
}
