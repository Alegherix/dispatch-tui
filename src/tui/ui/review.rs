use super::palette::{BLUE, BORDER, CYAN, DIM_META, GREEN, MUTED, MUTED_LIGHT, RED_DIM, YELLOW};
use super::shared::{
    push_hint_spans, refresh_status, render_substatus_header, render_tab_bar, staleness_color,
    truncate,
};

use crate::models::{
    format_age, CiStatus, ReviewDecision, ReviewPr, ReviewWorkflowState, ReviewWorkflowSubState,
    Staleness,
};
use crate::tui::types::{WorkflowKey};
use crate::tui::{App, InputMode, ReviewBoardMode, ViewMode};
use chrono::Utc;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

fn render_review_repo_filter_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let repos = app.active_review_repos();
    let repo_count = repos.len();
    let popup_height = (repo_count as u16 + 5).clamp(7, area.height.saturating_sub(4));
    let popup_width = (area.width * 70 / 100).clamp(30, 60);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    let review_mode_label = app.review_repo_filter_mode().as_str();
    let block = Block::default()
        .title(format!(" Review Repo Filter ({review_mode_label}) "))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Cyan))
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Gray);
    let note_style = Style::default().fg(Color::DarkGray);

    let mut lines = vec![Line::from("")];

    for (i, repo) in repos.iter().enumerate() {
        let num = i + 1;
        let checked = if app.review_repo_filter().contains(repo) {
            "x"
        } else {
            " "
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {num}"), key_style),
            Span::styled(format!(". [{checked}] {repo}"), desc_style),
        ]));
    }

    lines.push(Line::from(""));

    let all_selected = !repos.is_empty() && app.review_repo_filter().len() == repos.len();
    let a_label = if all_selected {
        "clear all"
    } else {
        "select all"
    };
    lines.push(Line::from(vec![
        Span::styled("  [a]", key_style),
        Span::styled(format!(" {a_label}  "), note_style),
        Span::styled("[Tab]", key_style),
        Span::styled(" incl/excl  ", note_style),
        Span::styled("[Enter/Esc]", key_style),
        Span::styled(" close", note_style),
    ]));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup_area);
}

// ---------------------------------------------------------------------------
// Review board rendering
// ---------------------------------------------------------------------------

pub(in crate::tui) fn review_action_hints(
    has_pr: bool,
    agent_status: Option<crate::models::ReviewAgentStatus>,
    ready_to_merge: bool,
) -> Vec<Span<'static>> {
    use crate::models::ReviewAgentStatus;
    let key_color = Color::Cyan;
    let label_style = Style::default().fg(MUTED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut push_hint = |key: &'static str, label: &'static str| {
        push_hint_spans(&mut spans, key, label, key_color, label_style);
    };
    if has_pr {
        push_hint("Enter", "open PR");
    }
    match agent_status {
        Some(ReviewAgentStatus::Idle) => {
            push_hint("g", "go to");
            push_hint("d", "resume");
            push_hint("T", "detach");
        }
        Some(_) => {
            push_hint("g", "go to");
            push_hint("T", "detach");
        }
        None => {
            if has_pr {
                push_hint("d", "dispatch");
            }
        }
    }
    if has_pr {
        push_hint("m", "forward");
        push_hint("M", "back");
        if ready_to_merge {
            push_hint("ctrl+m", "merge");
        }
    }
    push_hint("f", "filter");
    push_hint("e", "edit queries");
    push_hint("1/2", "mode");
    push_hint("Tab", "task board");
    push_hint("?", "help");
    push_hint("q", "quit");
    spans
}

pub(in crate::tui) fn review_column_color(state: ReviewWorkflowState) -> Color {
    match state {
        ReviewWorkflowState::Backlog => MUTED,
        ReviewWorkflowState::Ongoing => BLUE,
        ReviewWorkflowState::ActionRequired => YELLOW,
        ReviewWorkflowState::Done => GREEN,
    }
}

pub(in crate::tui) fn review_cursor_bg_color(state: ReviewWorkflowState) -> Color {
    match state {
        ReviewWorkflowState::Backlog => Color::Rgb(30, 30, 44),
        ReviewWorkflowState::Ongoing => Color::Rgb(34, 38, 66),
        ReviewWorkflowState::ActionRequired => Color::Rgb(52, 44, 20),
        ReviewWorkflowState::Done => Color::Rgb(32, 52, 36),
    }
}

pub(in crate::tui) fn review_column_bg_color(state: ReviewWorkflowState) -> Color {
    match state {
        ReviewWorkflowState::Backlog => Color::Rgb(26, 26, 36),
        ReviewWorkflowState::Ongoing => Color::Rgb(28, 30, 44),
        ReviewWorkflowState::ActionRequired => Color::Rgb(36, 34, 26),
        ReviewWorkflowState::Done => Color::Rgb(27, 36, 30),
    }
}

/// Render the review board view.
pub fn render_review_board(frame: &mut Frame, app: &mut App, area: Rect) {
    let detail_height = if app.review_detail_visible() { 8 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),             // tab bar
            Constraint::Length(1),             // summary row
            Constraint::Length(1),             // refresh status row
            Constraint::Min(1),                // board
            Constraint::Length(detail_height), // detail panel
            Constraint::Length(1),             // status bar
        ])
        .split(area);

    render_tab_bar(frame, app, chunks[0]);
    render_review_summary_row(frame, app, chunks[1]);

    // Refresh status row
    let (last_fetch, loading) = match app.view_mode() {
        ViewMode::ReviewBoard {
            mode: ReviewBoardMode::Dependabot,
            ..
        } => (app.review_bot_prs_last_fetch(), app.review_bot_prs_loading()),
        _ => (app.review_last_fetch(), app.review_board_loading()),
    };
    let (status_text, status_color) =
        refresh_status(last_fetch, loading, crate::tui::REVIEW_REFRESH_INTERVAL);
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().fg(status_color)),
        chunks[2],
    );

    let filtered = app.active_review_prs();
    if filtered.is_empty() {
        let is_empty = match app.view_mode() {
            ViewMode::ReviewBoard {
                mode: ReviewBoardMode::Dependabot,
                ..
            } => app.review_bot_prs().is_empty(),
            _ => app.review_prs().is_empty(),
        };
        let msg = if is_empty {
            "No PRs found"
        } else {
            "All PRs filtered out."
        };
        let p = Paragraph::new(msg)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, chunks[3]);
    } else {
        render_review_columns(frame, app, chunks[3]);
    }

    render_review_detail(frame, app, chunks[4]);

    // Status bar: transient message takes priority; fall back to persistent error
    if let Some(msg) = app.status.message.as_deref() {
        let status = Paragraph::new(msg.to_string()).style(Style::default().fg(Color::Yellow));
        frame.render_widget(status, chunks[5]);
    } else if let Some(err) = app.last_review_error() {
        let status = Paragraph::new(format!("Error: {err}")).style(Style::default().fg(Color::Red));
        frame.render_widget(status, chunks[5]);
    } else if app.has_bot_pr_selection() {
        let count = app.selected_bot_prs().len();
        let text = format!("{count} selected  [A] approve  [m] merge  [Esc] clear");
        let status = Paragraph::new(text).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_widget(status, chunks[5]);
    } else {
        let has_pr = app.selected_review_pr().is_some();
        let agent_status = app
            .selected_review_pr()
            .and_then(|pr| app.pr_agent(pr).map(|h| h.status));
        let ready_to_merge = app.selected_review_pr().map(|pr| {
            let kind = match app.view_mode() {
                ViewMode::ReviewBoard { mode, .. } => mode.workflow_item_kind(),
                _ => return false,
            };
            let key = WorkflowKey::new(pr.repo.clone(), pr.number, kind);
            matches!(
                app.review.review_workflow_states.get(&key),
                Some((ReviewWorkflowState::ActionRequired, Some(ReviewWorkflowSubState::ReadyToMerge)))
            )
        }).unwrap_or(false);
        let hints = Paragraph::new(Line::from(review_action_hints(
            has_pr,
            agent_status,
            ready_to_merge,
        )));
        frame.render_widget(hints, chunks[5]);
    }

    // Filter overlay (on top of everything)
    if matches!(app.mode(), InputMode::ReviewRepoFilter) {
        render_review_repo_filter_overlay(frame, app, area);
    }
}

fn render_review_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER));

    if !app.review_detail_visible() {
        let paragraph = Paragraph::new("").block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let Some(pr) = app.selected_review_pr() else {
        let paragraph = Paragraph::new("").block(block);
        frame.render_widget(paragraph, area);
        return;
    };

    // Get workflow state for color
    let kind = match app.view_mode() {
        ViewMode::ReviewBoard { mode, .. } => mode.workflow_item_kind(),
        _ => crate::models::WorkflowItemKind::ReviewerPr,
    };
    let wf_key = WorkflowKey::new(pr.repo.clone(), pr.number, kind);
    let (wf_state, _) = app
        .review
        .review_workflow_states
        .get(&wf_key)
        .copied()
        .unwrap_or((ReviewWorkflowState::Backlog, None));
    let col_color = review_column_color(wf_state);

    let now = Utc::now();
    let age = format_age(pr.created_at, now);

    // Line 1: title + CI status
    let ci_color = match pr.ci_status {
        CiStatus::Success => Color::Green,
        CiStatus::Failure => Color::Red,
        CiStatus::Pending => Color::Yellow,
        CiStatus::None => Color::DarkGray,
    };
    let ci_label = match pr.ci_status {
        CiStatus::Success => "passing",
        CiStatus::Failure => "failing",
        CiStatus::Pending => "pending",
        CiStatus::None => "no checks",
    };
    let line1 = Line::from(vec![
        Span::styled(
            format!("{}#{} {}", pr.repo, pr.number, pr.title),
            Style::default()
                .fg(col_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  CI: {} {ci_label}", pr.ci_status.symbol()),
            Style::default().fg(ci_color),
        ),
    ]);

    // Line 2: metadata
    let line2 = Line::from(Span::styled(
        format!(
            "@{} \u{00b7} {} \u{00b7} +{}/-{}",
            pr.author, age, pr.additions, pr.deletions
        ),
        Style::default().fg(MUTED),
    ));

    // Line 3: reviewer list
    let reviewer_spans: Vec<String> = pr
        .reviewers
        .iter()
        .map(|r| {
            let icon = match r.decision {
                Some(ReviewDecision::Approved) => "\u{2713}",
                Some(ReviewDecision::ChangesRequested) => "\u{2717}",
                _ => "\u{23f3}",
            };
            format!("@{} {icon}", r.login)
        })
        .collect();
    let reviewer_line = if reviewer_spans.is_empty() {
        "No reviewers".to_string()
    } else {
        format!("Reviews: {}", reviewer_spans.join(" \u{00b7} "))
    };
    let line3 = Line::from(Span::styled(
        reviewer_line,
        Style::default().fg(MUTED_LIGHT),
    ));

    // Lines 4+: PR body (truncated to fit remaining space)
    let body_lines: Vec<Line> = pr
        .body
        .lines()
        .take(5)
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::DarkGray),
            ))
        })
        .collect();

    let mut lines = vec![line1, line2, line3];
    lines.extend(body_lines);

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_review_summary_row(frame: &mut Frame, app: &App, area: Rect) {
    let sel = app.review_selection();
    let selected_col = sel.map(|s| s.column()).unwrap_or(0);
    let col_count = ReviewBoardMode::column_count();
    let workflow_states = [
        ReviewWorkflowState::Backlog,
        ReviewWorkflowState::Ongoing,
        ReviewWorkflowState::ActionRequired,
        ReviewWorkflowState::Done,
    ];

    let segments = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, col_count as u32); col_count])
        .split(area);

    let mode_kind = match app.view_mode() {
        ViewMode::ReviewBoard { mode, .. } => mode.workflow_item_kind(),
        _ => crate::models::WorkflowItemKind::ReviewerPr,
    };
    let filtered = app.active_review_prs();

    for i in 0..col_count {
        let wf_state = workflow_states[i];
        let count = filtered.iter().filter(|pr| {
            let key = WorkflowKey::new(pr.repo.clone(), pr.number, mode_kind);
            let (state, _) = app
                .review
                .review_workflow_states
                .get(&key)
                .copied()
                .unwrap_or((ReviewWorkflowState::Backlog, None));
            state == wf_state
        }).count();
        let is_focused = i == selected_col;
        let prefix = if is_focused { "\u{25b8} " } else { "\u{25e6} " };
        let col_label = ReviewBoardMode::column_label(wf_state);
        let label = format!("{prefix}{col_label} ({count})");

        let color = review_column_color(wf_state);
        let style = if is_focused {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let p = Paragraph::new(label).style(style);
        frame.render_widget(p, segments[i]);
    }
}

/// Sub-state sort order within a column — lower = earlier.
fn sub_state_sort_key(sub: Option<ReviewWorkflowSubState>) -> u8 {
    match sub {
        Some(ReviewWorkflowSubState::Reviewing) => 0,
        Some(ReviewWorkflowSubState::Idle) => 1,
        Some(ReviewWorkflowSubState::Stale) => 2,
        Some(ReviewWorkflowSubState::FindingsReady) => 3,
        Some(ReviewWorkflowSubState::ReadyToMerge) => 4,
        Some(ReviewWorkflowSubState::ChangesRequested) => 5,
        Some(ReviewWorkflowSubState::AwaitingResponse) => 6,
        Some(ReviewWorkflowSubState::CiFailing) => 7,
        None => 8,
    }
}

fn render_review_columns(frame: &mut Frame, app: &mut App, area: Rect) {
    let sel_col = app.review_selection().map(|s| s.column()).unwrap_or(0);
    let col_count = ReviewBoardMode::column_count();
    let workflow_states = [
        ReviewWorkflowState::Backlog,
        ReviewWorkflowState::Ongoing,
        ReviewWorkflowState::ActionRequired,
        ReviewWorkflowState::Done,
    ];

    let mode_kind = match app.view_mode() {
        ViewMode::ReviewBoard { mode, .. } => mode.workflow_item_kind(),
        _ => crate::models::WorkflowItemKind::ReviewerPr,
    };

    let col_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, col_count as u32); col_count])
        .split(area);

    // Build column PR lists with workflow state lookup
    for i in 0..col_count {
        let is_focused = i == sel_col;
        let wf_state = workflow_states[i];

        // Collect PRs for this workflow column, with their sub-states
        let mut col_prs: Vec<(&ReviewPr, Option<ReviewWorkflowSubState>)> = app
            .active_review_prs()
            .into_iter()
            .filter_map(|pr| {
                let key = WorkflowKey::new(pr.repo.clone(), pr.number, mode_kind);
                let (state, sub) = app
                    .review
                    .review_workflow_states
                    .get(&key)
                    .copied()
                    .unwrap_or((ReviewWorkflowState::Backlog, None));
                if state == wf_state {
                    Some((pr, sub))
                } else {
                    None
                }
            })
            .collect();

        // Sort by sub-state then repo then number
        col_prs.sort_by(|(a, a_sub), (b, b_sub)| {
            sub_state_sort_key(*a_sub)
                .cmp(&sub_state_sort_key(*b_sub))
                .then(a.repo.cmp(&b.repo))
                .then(a.number.cmp(&b.number))
        });

        let selected_row = app.review_selection().map(|s| s.row(i)).unwrap_or(0);
        let mut list_items: Vec<ListItem> = Vec::new();
        let mut list_selection_idx: Option<usize> = None;
        let mut current_sub: Option<Option<ReviewWorkflowSubState>> = None;

        // Build items with circle indicator
        for (item_idx, (pr, sub)) in col_prs.iter().enumerate() {
            // Section header when sub-state changes
            if current_sub != Some(*sub) {
                current_sub = Some(*sub);
                if let Some(sub_state) = sub {
                    let label = sub_state.section_label();
                    list_items.push(render_substatus_header(label, list_items.is_empty()));
                } else if !list_items.is_empty() {
                    // No sub-state but not first item — add separator
                    list_items.push(render_substatus_header("other", false));
                }
            }

            if item_idx == selected_row {
                list_selection_idx = Some(list_items.len());
            }

            let agent_status = app.pr_agent(pr).map(|h| h.status);
            // Circle reflects whether a tmux window exists for this PR, not the agent's logical status.
            // Filled (◉) if session is live, empty (○) if not.
            let tmux_alive = app.pr_agent(pr).map(|h| !h.tmux_window.is_empty()).unwrap_or(false);
            list_items.push(build_review_pr_item(
                pr,
                wf_state,
                *sub,
                is_focused && item_idx == selected_row,
                agent_status,
                tmux_alive,
                col_areas[i].width,
            ));
        }

        let bg = if is_focused {
            review_column_bg_color(wf_state)
        } else {
            Color::Reset
        };

        let list = List::new(list_items).block(Block::default().style(Style::default().bg(bg)));

        let mut list_state = ListState::default();
        if is_focused {
            list_state.select(list_selection_idx);
        }

        frame.render_stateful_widget(list, col_areas[i], &mut list_state);

        // Write back the list state for scroll tracking
        if let Some(sel) = app.review_selection_mut() {
            sel.list_states[i] = list_state;
        }
    }
}

/// Build a 2-line list item for a review PR card.
///
/// Line 1: `[circle] #<number> <title> [CI dot] [draft badge]`
/// - circle omitted in Backlog; ◉ (cyan) if tmux alive, ○ (dim) if not
/// - CI dot: green for success, red for failure, omitted for unknown
///
/// Line 2: context based on sub-state
pub(in crate::tui::ui) fn build_review_pr_item(
    pr: &ReviewPr,
    state: ReviewWorkflowState,
    sub: Option<ReviewWorkflowSubState>,
    is_cursor: bool,
    agent_status: Option<crate::models::ReviewAgentStatus>,
    tmux_alive: bool,
    col_width: u16,
) -> ListItem<'static> {
    let col_color = review_column_color(state);
    let now = Utc::now();

    // --- Line 1 ---
    let stripe = if is_cursor { "\u{258c} " } else { "\u{258e} " };

    // Circle indicator (omitted in Backlog)
    let (circle_text, circle_color): (&'static str, Color) = if state == ReviewWorkflowState::Backlog {
        ("", Color::Reset)
    } else if tmux_alive {
        ("\u{25c9} ", CYAN) // ◉
    } else {
        ("\u{25cb} ", Color::DarkGray) // ○
    };

    // CI dot
    let ci_dot_color = match pr.ci_status {
        CiStatus::Success => Some(Color::Green),
        CiStatus::Failure => Some(Color::Red),
        _ => None,
    };

    // Draft badge
    let draft_badge = if pr.is_draft { " [drft]" } else { "" };

    // Calculate available width for title
    let circle_w = if circle_text.is_empty() { 0usize } else { 2 };
    let ci_dot_w = if ci_dot_color.is_some() { 2usize } else { 0 };
    let draft_w = draft_badge.len();
    let header_prefix = format!("#{} ", pr.number);
    // stripe(2) + circle(0 or 2) + "#N "(varies) + title + ci_dot(0/2) + draft
    let overhead = 2 + circle_w + header_prefix.len() + ci_dot_w + draft_w;
    let max_title = (col_width as usize).saturating_sub(overhead).max(1);
    let title_truncated = truncate(&pr.title, max_title);

    let line1_style = if is_cursor {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(col_color)
    };

    let _ = agent_status; // circle already encodes running state via tmux_alive
    let mut spans1: Vec<Span> = vec![Span::styled(stripe.to_string(), Style::default().fg(col_color))];
    if !circle_text.is_empty() {
        spans1.push(Span::styled(circle_text.to_string(), Style::default().fg(circle_color)));
    }
    spans1.push(Span::styled(
        format!("{header_prefix}{title_truncated}"),
        line1_style,
    ));
    if let Some(dot_color) = ci_dot_color {
        spans1.push(Span::styled(" \u{25cf}", Style::default().fg(dot_color)));
    }
    if !draft_badge.is_empty() {
        spans1.push(Span::styled(
            draft_badge.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let line1 = Line::from(spans1);

    // --- Line 2: context based on sub-state ---
    let line2 = match sub {
        Some(ReviewWorkflowSubState::ReadyToMerge) => {
            let approved_count = pr
                .reviewers
                .iter()
                .filter(|r| r.decision == Some(ReviewDecision::Approved))
                .count();
            let check_marks = "\u{2713}\u{2713}";
            Line::from(vec![
                Span::raw("  "),
                Span::styled("approved  ", Style::default().fg(GREEN)),
                Span::styled(check_marks.to_string(), Style::default().fg(GREEN)),
                if approved_count > 0 {
                    Span::styled(
                        format!(" ({approved_count})"),
                        Style::default().fg(DIM_META),
                    )
                } else {
                    Span::raw("")
                },
            ])
        }
        Some(ReviewWorkflowSubState::FindingsReady) => Line::from(vec![
            Span::raw("  "),
            Span::styled("findings", Style::default().fg(YELLOW)),
        ]),
        Some(ReviewWorkflowSubState::Stale) => {
            let age = format_age(pr.updated_at, now);
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("stale {age}"), Style::default().fg(RED_DIM)),
            ])
        }
        _ => {
            // Default: @author  +lines-lines  age
            let age = format_age(pr.created_at, now);
            let staleness = Staleness::from_age(pr.created_at, now);
            let age_color = staleness_color(staleness);
            let meta_style = Style::default().fg(DIM_META);
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("@{} \u{b7} +{}/-{} \u{b7} ", pr.author, pr.additions, pr.deletions),
                    meta_style,
                ),
                Span::styled(age, Style::default().fg(age_color)),
            ])
        }
    };

    let bg = if is_cursor {
        review_cursor_bg_color(state)
    } else {
        Color::Reset
    };

    ListItem::new(vec![line1, line2]).style(Style::default().bg(bg))
}

// ---------------------------------------------------------------------------
// Helpers for the Dependabot security board (3-column ReviewDecision layout)
// ---------------------------------------------------------------------------

/// Column color for the 3-column Dependabot board (ReviewDecision-keyed).
pub(in crate::tui::ui) fn dependabot_column_color(decision: ReviewDecision) -> Color {
    match decision {
        ReviewDecision::ReviewRequired => BLUE,
        ReviewDecision::WaitingForResponse => YELLOW,
        ReviewDecision::ChangesRequested => RED_DIM,
        ReviewDecision::Approved => GREEN,
    }
}

/// Column background for the 3-column Dependabot board (ReviewDecision-keyed).
pub(in crate::tui::ui) fn dependabot_column_bg_color(decision: ReviewDecision) -> Color {
    match decision {
        ReviewDecision::ReviewRequired => Color::Rgb(28, 30, 44),
        ReviewDecision::WaitingForResponse => Color::Rgb(36, 34, 26),
        ReviewDecision::ChangesRequested => Color::Rgb(36, 28, 28),
        ReviewDecision::Approved => Color::Rgb(27, 36, 30),
    }
}

/// Cursor highlight background for the 3-column Dependabot board.
pub(in crate::tui::ui) fn dependabot_cursor_bg_color(decision: ReviewDecision) -> Color {
    match decision {
        ReviewDecision::ReviewRequired => Color::Rgb(34, 38, 66),
        ReviewDecision::WaitingForResponse => Color::Rgb(52, 44, 20),
        ReviewDecision::ChangesRequested => Color::Rgb(56, 32, 32),
        ReviewDecision::Approved => Color::Rgb(32, 52, 36),
    }
}

fn build_pr_line1_dependabot(
    pr: &ReviewPr,
    color: Color,
    is_cursor: bool,
    agent_status: Option<crate::models::ReviewAgentStatus>,
    col_width: u16,
) -> Line<'static> {
    let stripe = if is_cursor { "\u{258c} " } else { "\u{258e} " };

    let (badge_text, badge_is_running) = match agent_status {
        Some(crate::models::ReviewAgentStatus::Reviewing) => ("\u{25c9} ", true),
        Some(crate::models::ReviewAgentStatus::FindingsReady) => ("\u{2714} ", false),
        Some(crate::models::ReviewAgentStatus::Idle) => ("\u{25cb} ", false),
        None => ("", false),
    };

    let header = format!("#{} {}", pr.number, pr.title);
    let badge_w = if badge_text.is_empty() { 0 } else { 2 };
    let max_header = (col_width as usize).saturating_sub(4 + badge_w);
    let header_truncated = truncate(&header, max_header);

    let line1_style = if is_cursor {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };
    let badge_style = if badge_is_running {
        Style::default().fg(CYAN)
    } else {
        line1_style
    };

    let mut spans = vec![Span::styled(stripe.to_string(), Style::default().fg(color))];
    if !badge_text.is_empty() {
        spans.push(Span::styled(badge_text.to_string(), badge_style));
    }
    spans.push(Span::styled(header_truncated, line1_style));
    let ci_color = match pr.ci_status {
        CiStatus::Success => Color::Green,
        CiStatus::Failure => Color::Red,
        CiStatus::Pending => Color::Yellow,
        CiStatus::None => Color::DarkGray,
    };
    spans.push(Span::styled(
        " \u{25cf}",
        Style::default().fg(ci_color),
    ));
    Line::from(spans)
}

/// Build a list item for the Dependabot security board (ReviewDecision-based coloring).
pub(in crate::tui::ui) fn build_dependabot_pr_item(
    pr: &ReviewPr,
    decision: ReviewDecision,
    is_cursor: bool,
    agent_status: Option<crate::models::ReviewAgentStatus>,
    is_selected: bool,
    col_width: u16,
) -> ListItem<'static> {
    let _ = is_selected; // selection highlight applied by List widget, not inline
    let color = dependabot_column_color(decision);
    let now = Utc::now();
    let age = format_age(pr.created_at, now);

    let line1 = build_pr_line1_dependabot(pr, color, is_cursor, agent_status, col_width);

    let staleness = Staleness::from_age(pr.created_at, now);
    let age_color = staleness_color(staleness);

    let (ci_prefix_color, ci_label) = ci_state_prefix(pr.ci_status);
    let meta_style = Style::default().fg(DIM_META);
    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled(ci_label, Style::default().fg(ci_prefix_color)),
        Span::styled(
            format!(" \u{b7} +{}/-{} \u{b7} ", pr.additions, pr.deletions),
            meta_style,
        ),
        Span::styled(age, Style::default().fg(age_color)),
    ]);

    let bg = if is_cursor {
        dependabot_cursor_bg_color(decision)
    } else {
        Color::Reset
    };

    ListItem::new(vec![line1, line2]).style(Style::default().bg(bg))
}

fn ci_state_prefix(status: CiStatus) -> (Color, &'static str) {
    match status {
        CiStatus::Success => (Color::Green, "\u{25cf} passing"),
        CiStatus::Failure => (Color::Red, "\u{25cf} failing"),
        CiStatus::Pending => (Color::Yellow, "\u{25cf} pending"),
        CiStatus::None => (Color::DarkGray, "\u{25cf} \u{2013}"),
    }
}
