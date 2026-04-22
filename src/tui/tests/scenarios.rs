use crossterm::event::KeyCode;

use super::super::{App, Command, InputMode, Message, ViewMode};
use super::{make_app, make_key};
use crate::models::{TaskId, TaskStatus};

/// Drives an `App` through a sequence of key events, collecting all `Command`s emitted.
struct Scenario {
    app: App,
    commands: Vec<Command>,
}

impl Scenario {
    fn new() -> Self {
        Self {
            app: make_app(),
            commands: vec![],
        }
    }

    fn with_app(app: App) -> Self {
        Self {
            app,
            commands: vec![],
        }
    }

    fn key(&mut self, code: KeyCode) -> &mut Self {
        let cmds = self.app.handle_key(make_key(code));
        self.commands.extend(cmds);
        self
    }

    fn char_keys(&mut self, s: &str) -> &mut Self {
        for c in s.chars() {
            self.key(KeyCode::Char(c));
        }
        self
    }
}

// --- Scenario: task creation dialog ---

#[test]
fn scenario_task_creation_dialog_enters_input_title_mode() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('n'));
    assert!(
        matches!(s.app.input.mode, InputMode::InputTitle),
        "expected InputTitle after pressing n, got {:?}",
        s.app.input.mode
    );
}

#[test]
fn scenario_task_creation_empty_title_returns_to_normal() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('n'));
    s.key(KeyCode::Enter);
    assert!(
        matches!(s.app.input.mode, InputMode::Normal),
        "empty title should return to Normal, got {:?}",
        s.app.input.mode
    );
}

#[test]
fn scenario_task_creation_typing_title_advances_to_tag_input() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('n'));
    s.char_keys("My Task");
    s.key(KeyCode::Enter);
    assert!(
        matches!(s.app.input.mode, InputMode::InputTag),
        "expected InputTag after submitting title, got {:?}",
        s.app.input.mode
    );
}

#[test]
fn scenario_task_creation_esc_cancels_from_title_input() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('n'));
    s.char_keys("partial");
    s.key(KeyCode::Esc);
    assert!(
        matches!(s.app.input.mode, InputMode::Normal),
        "Esc should cancel back to Normal, got {:?}",
        s.app.input.mode
    );
    assert!(
        s.app.input.buffer.is_empty(),
        "buffer should be cleared after cancel"
    );
}

// --- Scenario: quick dispatch ---

#[test]
fn scenario_quick_dispatch_with_repo_path_emits_command() {
    let mut app = make_app();
    app.update(Message::RepoPathsUpdated(vec!["/repo".to_string()]));

    let mut s = Scenario::with_app(app);
    s.key(KeyCode::Char('D'));

    assert!(
        s.commands
            .iter()
            .any(|c| matches!(c, Command::QuickDispatch { .. })),
        "expected QuickDispatch command, got {:?}",
        s.commands
    );
}

#[test]
fn scenario_quick_dispatch_without_repo_path_shows_no_dispatch_command() {
    // make_app() starts with repo_paths = [], so D emits a StatusInfo instead
    let mut s = Scenario::new();
    s.key(KeyCode::Char('D'));

    assert!(
        !s.commands
            .iter()
            .any(|c| matches!(c, Command::QuickDispatch { .. })),
        "should not emit QuickDispatch without a repo path"
    );
}

// --- Scenario: board tab cycling ---

#[test]
fn scenario_tab_switches_kanban_to_review_board() {
    let mut s = Scenario::new();
    assert!(
        matches!(s.app.board.view_mode, ViewMode::Board(_)),
        "should start on kanban board"
    );
    s.key(KeyCode::Tab);
    assert!(
        matches!(s.app.board.view_mode, ViewMode::ReviewBoard { .. }),
        "Tab from kanban should switch to ReviewBoard, got {:?}",
        s.app.board.view_mode
    );
}

#[test]
fn scenario_tab_cycles_through_all_three_boards() {
    let mut s = Scenario::new();
    // kanban → review
    s.key(KeyCode::Tab);
    assert!(
        matches!(s.app.board.view_mode, ViewMode::ReviewBoard { .. }),
        "first Tab: expected ReviewBoard, got {:?}",
        s.app.board.view_mode
    );
    // review → security
    s.key(KeyCode::Tab);
    assert!(
        matches!(s.app.board.view_mode, ViewMode::SecurityBoard { .. }),
        "second Tab: expected SecurityBoard, got {:?}",
        s.app.board.view_mode
    );
    // security → kanban
    s.key(KeyCode::Tab);
    assert!(
        matches!(s.app.board.view_mode, ViewMode::Board(_)),
        "third Tab: expected Board (kanban), got {:?}",
        s.app.board.view_mode
    );
}

// --- Scenario: help overlay open/close ---

#[test]
fn scenario_help_overlay_opens_on_question_mark() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('?'));
    assert!(
        matches!(s.app.input.mode, InputMode::Help),
        "expected Help mode after '?', got {:?}",
        s.app.input.mode
    );
}

#[test]
fn scenario_help_overlay_toggles_closed_on_second_question_mark() {
    let mut s = Scenario::new();
    s.key(KeyCode::Char('?'));
    s.key(KeyCode::Char('?'));
    assert!(
        matches!(s.app.input.mode, InputMode::Normal),
        "expected Normal mode after closing help, got {:?}",
        s.app.input.mode
    );
}

// --- Scenario: navigate and move task ---

#[test]
fn scenario_move_key_advances_selected_task_to_next_column() {
    // make_app() places task 1 in Backlog; the initial selection is column 0 (Backlog), row 0.
    // Press 'm' to move the selected task forward (Backlog → Running).
    let mut s = Scenario::new();
    let cmds = s.app.handle_key(make_key(KeyCode::Char('m')));
    s.commands.extend(cmds);

    let task1 = s
        .app
        .board
        .tasks
        .iter()
        .find(|t| t.id == TaskId(1))
        .expect("task 1 should exist");
    assert_eq!(
        task1.status,
        TaskStatus::Running,
        "task 1 should be moved to Running after 'm'"
    );
    assert!(
        s.commands
            .iter()
            .any(|c| matches!(c, Command::PersistTask(_))),
        "move should emit PersistTask command"
    );
}
