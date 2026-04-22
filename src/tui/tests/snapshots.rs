use ratatui::buffer::Buffer;

use super::super::{ui, App};
use super::{make_app, make_key, make_review_board_app, make_security_board_app};
use crossterm::event::KeyCode;

/// Convert a ratatui buffer to a plain text string, one line per row.
/// Trailing spaces on each line are trimmed so snapshots are compact.
fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area();
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buf[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, app)).unwrap();
    buffer_to_string(terminal.backend().buffer())
}

// --- Snapshot tests ---

#[test]
fn snapshot_empty_kanban_board() {
    use std::time::Duration;
    let mut app = App::new(vec![], Duration::from_secs(300));
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

#[test]
fn snapshot_kanban_with_tasks() {
    let mut app = make_app();
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

#[test]
fn snapshot_review_board_reviewer_mode() {
    let mut app = make_review_board_app();
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

#[test]
fn snapshot_security_board() {
    let mut app = make_security_board_app();
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

#[test]
fn snapshot_help_overlay() {
    let mut app = make_app();
    app.handle_key(make_key(KeyCode::Char('?')));
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}
