use ratatui::buffer::Buffer;

use super::super::App;
use super::{make_app, make_key, render_to_buffer, TEST_TIMEOUT};
use crossterm::event::KeyCode;

fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area();
    let mut lines = Vec::with_capacity(area.height as usize);
    for y in area.top()..area.bottom() {
        let mut line = String::with_capacity(area.width as usize * 3);
        for x in area.left()..area.right() {
            line.push_str(buf[(x, y)].symbol());
        }
        line.truncate(line.trim_end().len());
        lines.push(line);
    }
    lines.join("\n")
}

fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    buffer_to_string(&render_to_buffer(app, width, height))
}

#[test]
fn snapshot_empty_kanban_board() {
    let mut app = App::new(vec![], TEST_TIMEOUT);
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
fn snapshot_help_overlay() {
    let mut app = make_app();
    app.handle_key(make_key(KeyCode::Char('?')));
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

fn make_feed_epic(id: i64, title: &str, sort_order: i64) -> crate::models::Epic {
    let now = chrono::Utc::now();
    crate::models::Epic {
        id: crate::models::EpicId(id),
        title: title.to_string(),
        description: String::new(),
        repo_path: "/repo".to_string(),
        status: crate::models::TaskStatus::Backlog,
        plan_path: None,
        sort_order: Some(sort_order),
        auto_dispatch: false,
        parent_epic_id: None,
        feed_command: Some(format!("feed-{title}")),
        feed_interval_secs: Some(30),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn snapshot_tab_bar_with_feed_epics_board_active() {
    let mut app = App::new(vec![], super::TEST_TIMEOUT);
    app.board.epics = vec![
        make_feed_epic(1, "My Feed", -2),
        make_feed_epic(2, "Another Feed", -1),
    ];
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}

#[test]
fn snapshot_tab_bar_with_feed_epics_feed_active() {
    use super::super::types::Message;
    let mut app = App::new(vec![], super::TEST_TIMEOUT);
    app.board.epics = vec![
        make_feed_epic(1, "My Feed", -2),
        make_feed_epic(2, "Another Feed", -1),
    ];
    // Enter the first feed epic view to make its tab active
    let feed_epic_id = app
        .epics()
        .iter()
        .find(|e| e.feed_command.is_some())
        .unwrap()
        .id;
    app.update(Message::EnterEpic(feed_epic_id));
    let rendered = render_to_string(&mut app, 120, 40);
    insta::assert_snapshot!(rendered);
}
