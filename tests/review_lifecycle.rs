//! Integration tests: review board lifecycle through App::update() with a real (in-memory) DB.
#![allow(dead_code, unused_imports)]

use std::time::Duration;

use dispatch_tui::models::{CiStatus, ReviewDecision, ReviewPr};
use dispatch_tui::tui::{App, Command, Message, PrListKind, ReviewAgentRequest};
use chrono::Utc;

fn make_app() -> App {
    App::new(vec![], Duration::from_secs(300))
}

fn make_pr(number: i64, repo: &str) -> ReviewPr {
    ReviewPr {
        number,
        title: format!("PR {number}"),
        author: "alice".to_string(),
        repo: repo.to_string(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        is_draft: false,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        additions: 10,
        deletions: 5,
        review_decision: ReviewDecision::ReviewRequired,
        labels: vec![],
        body: String::new(),
        head_ref: "feat/thing".to_string(),
        ci_status: CiStatus::None,
        reviewers: vec![],
    }
}

// ---------------------------------------------------------------------------
// Tick wiring: App emits FetchPrs when review lists are stale
// ---------------------------------------------------------------------------

#[test]
fn tick_triggers_fetch_when_review_list_stale() {
    let mut app = make_app();
    // Both lists have last_fetch = None (never fetched) — needs_fetch returns true
    let cmds = app.update(Message::Tick);
    assert!(
        cmds.iter().any(|c| matches!(c, Command::FetchPrs(PrListKind::Review))),
        "Tick should emit FetchPrs(Review) when list is stale"
    );
    assert!(
        cmds.iter().any(|c| matches!(c, Command::FetchPrs(PrListKind::Authored))),
        "Tick should emit FetchPrs(Authored) when list is stale"
    );
}
