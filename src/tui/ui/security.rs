use super::palette::{BORDER, CYAN, DIM_META, FG, MUTED, MUTED_LIGHT, YELLOW};
use super::shared::{
    push_hint_spans, refresh_status, render_substatus_header, render_tab_bar, staleness_color,
    truncate,
};

use crate::models::{
    format_age, AlertKind, AlertSeverity, SecurityAlert, SecurityWorkflowState,
    SecurityWorkflowSubState, Staleness,
};
use crate::tui::types::{FixDispatchKey, WorkflowKey};
use crate::tui::{App, InputMode};
use chrono::Utc;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// Column color helpers — keyed on SecurityWorkflowState (4-column)
// ---------------------------------------------------------------------------

fn security_column_color(state: SecurityWorkflowState) -> Color {
    match state {
        SecurityWorkflowState::Backlog => MUTED,
        SecurityWorkflowState::Ongoing => CYAN,
        SecurityWorkflowState::ActionRequired => YELLOW,
        SecurityWorkflowState::Done => Color::Rgb(80, 160, 80),
    }
}

fn security_column_bg_color(state: SecurityWorkflowState) -> Color {
    match state {
        SecurityWorkflowState::Backlog => Color::Rgb(26, 26, 36),
        SecurityWorkflowState::Ongoing => Color::Rgb(24, 32, 48),
        SecurityWorkflowState::ActionRequired => Color::Rgb(36, 34, 26),
        SecurityWorkflowState::Done => Color::Rgb(27, 36, 30),
    }
}

fn severity_stripe_color(severity: AlertSeverity) -> Color {
    match severity {
        AlertSeverity::Critical => Color::Red,
        AlertSeverity::High => YELLOW,
        AlertSeverity::Medium => Color::Rgb(86, 152, 194),
        AlertSeverity::Low => Color::DarkGray,
    }
}

fn severity_cursor_bg_color(severity: AlertSeverity) -> Color {
    match severity {
        AlertSeverity::Critical => Color::Rgb(56, 28, 28),
        AlertSeverity::High => Color::Rgb(52, 44, 20),
        AlertSeverity::Medium => Color::Rgb(24, 40, 52),
        AlertSeverity::Low => Color::Rgb(34, 34, 40),
    }
}

// ---------------------------------------------------------------------------
// Sub-state sort key for security workflow
// ---------------------------------------------------------------------------

fn security_sub_state_sort_key(sub: Option<SecurityWorkflowSubState>) -> u8 {
    match sub {
        Some(SecurityWorkflowSubState::Investigating) => 0,
        Some(SecurityWorkflowSubState::Idle) => 1,
        Some(SecurityWorkflowSubState::Stale) => 2,
        Some(SecurityWorkflowSubState::FindingsReady) => 3,
        Some(SecurityWorkflowSubState::NeedsManualFix) => 4,
        Some(SecurityWorkflowSubState::PrOpen) => 5,
        Some(SecurityWorkflowSubState::ChangesRequested) => 6,
        Some(SecurityWorkflowSubState::CiFailing) => 7,
        Some(SecurityWorkflowSubState::ReadyToMerge) => 8,
        None => 9,
    }
}

// ---------------------------------------------------------------------------
// Workflow key for an alert
// ---------------------------------------------------------------------------

fn workflow_key_for_alert(alert: &SecurityAlert) -> WorkflowKey {
    let kind = match alert.kind {
        AlertKind::Dependabot => crate::models::WorkflowItemKind::DependabotAlert,
        AlertKind::CodeScanning => crate::models::WorkflowItemKind::CodeScanAlert,
    };
    WorkflowKey::new(alert.repo.clone(), alert.number, kind)
}

// ---------------------------------------------------------------------------
// Security board rendering — unified 4-column view
// ---------------------------------------------------------------------------

pub fn render_security_board(frame: &mut Frame, app: &mut App, area: Rect) {
    let detail_height = if app.security_detail_visible() { 8 } else { 0 };
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

    if app.security.unconfigured {
        let prompt = "No repositories configured — press [e] to set up security alert queries";
        frame.render_widget(
            Paragraph::new(prompt)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );
        if let Some(msg) = app.status.message.as_deref() {
            frame.render_widget(
                Paragraph::new(msg.to_string()).style(Style::default().fg(Color::Yellow)),
                chunks[5],
            );
        } else {
            let key_color = Color::Cyan;
            let label_style = Style::default().fg(MUTED);
            let mut hints: Vec<Span<'static>> = Vec::new();
            push_hint_spans(&mut hints, "e", "edit queries", key_color, label_style);
            push_hint_spans(&mut hints, "Tab", "tasks", key_color, label_style);
            push_hint_spans(&mut hints, "q", "quit", key_color, label_style);
            frame.render_widget(Paragraph::new(Line::from(hints)), chunks[5]);
        }
        if matches!(app.mode(), InputMode::SecurityRepoFilter) {
            render_security_repo_filter_overlay(frame, app, area);
        }
        return;
    }

    render_security_summary_row(frame, app, chunks[1]);

    let (status_text, status_color) = refresh_status(
        app.security_last_fetch(),
        app.security_loading(),
        crate::tui::SECURITY_POLL_INTERVAL,
    );
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().fg(status_color)),
        chunks[2],
    );

    let filtered = app.filtered_security_alerts();
    if filtered.is_empty() {
        let msg = if app.security.alerts.is_empty() {
            "No security alerts found"
        } else {
            "All alerts filtered out."
        };
        let p = Paragraph::new(msg)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, chunks[3]);
    } else {
        render_security_columns(frame, app, chunks[3]);
    }

    render_security_detail(frame, app, chunks[4]);

    // Status bar
    if let Some(msg) = app.status.message.as_deref() {
        let status = Paragraph::new(msg.to_string()).style(Style::default().fg(Color::Yellow));
        frame.render_widget(status, chunks[5]);
    } else if let Some(err) = app.last_security_error() {
        let status =
            Paragraph::new(format!("Error: {err}")).style(Style::default().fg(Color::Red));
        frame.render_widget(status, chunks[5]);
    } else {
        let has_alert = app.selected_security_alert().is_some();
        let agent_status = app
            .selected_security_alert()
            .and_then(|a| app.alert_agent(a).map(|h| h.status));
        let hints =
            Paragraph::new(Line::from(security_action_hints(app, has_alert, agent_status)));
        frame.render_widget(hints, chunks[5]);
    }

    if matches!(app.mode(), InputMode::SecurityRepoFilter) {
        render_security_repo_filter_overlay(frame, app, area);
    }
}

pub(in crate::tui) fn security_action_hints(
    _app: &App,
    has_alert: bool,
    agent_status: Option<crate::models::ReviewAgentStatus>,
) -> Vec<Span<'static>> {
    use crate::models::ReviewAgentStatus;
    let key_color = Color::Cyan;
    let label_style = Style::default().fg(MUTED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push_hint = |spans: &mut Vec<Span<'static>>, key: &'static str, label: &'static str| {
        push_hint_spans(spans, key, label, key_color, label_style);
    };
    if has_alert {
        push_hint(&mut spans, "Enter", "detail");
        match agent_status {
            Some(ReviewAgentStatus::Idle) => {
                push_hint(&mut spans, "g", "go to");
                push_hint(&mut spans, "d", "resume");
                push_hint(&mut spans, "T", "detach");
            }
            Some(_) => {
                push_hint(&mut spans, "g", "go to");
                push_hint(&mut spans, "T", "detach");
            }
            None => {
                push_hint(&mut spans, "d", "dispatch");
            }
        }
        push_hint(&mut spans, "p", "open");
        push_hint(&mut spans, "m", "forward");
        push_hint(&mut spans, "M", "back");
    }
    push_hint(&mut spans, "f", "filter");
    push_hint(&mut spans, "Tab", "tasks");
    push_hint(&mut spans, "?", "help");
    push_hint(&mut spans, "q", "quit");
    spans
}

fn render_security_summary_row(frame: &mut Frame, app: &App, area: Rect) {
    let sel_col = app.security_selection().map(|s| s.column()).unwrap_or(0);
    let col_count = 4usize;
    let workflow_states = [
        SecurityWorkflowState::Backlog,
        SecurityWorkflowState::Ongoing,
        SecurityWorkflowState::ActionRequired,
        SecurityWorkflowState::Done,
    ];

    let segments = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, col_count as u32); col_count])
        .split(area);

    let filtered = app.filtered_security_alerts();

    for (i, wf_state) in workflow_states.iter().enumerate() {
        let count = filtered
            .iter()
            .filter(|a| {
                let key = workflow_key_for_alert(a);
                let (state, _) = app
                    .security
                    .security_workflow_states
                    .get(&key)
                    .copied()
                    .unwrap_or((SecurityWorkflowState::Backlog, None));
                state == *wf_state
            })
            .count();
        let is_focused = i == sel_col;
        let prefix = if is_focused { "\u{25b8} " } else { "\u{25e6} " };
        let label = format!("{prefix}{} ({count})", wf_state.column_label());

        let color = security_column_color(*wf_state);
        let style = if is_focused {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        frame.render_widget(Paragraph::new(label).style(style), segments[i]);
    }
}

fn render_security_columns(frame: &mut Frame, app: &mut App, area: Rect) {
    let sel_col = app.security_selection().map(|s| s.column()).unwrap_or(0);
    let col_count = 4usize;
    let workflow_states = [
        SecurityWorkflowState::Backlog,
        SecurityWorkflowState::Ongoing,
        SecurityWorkflowState::ActionRequired,
        SecurityWorkflowState::Done,
    ];

    let col_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, col_count as u32); col_count])
        .split(area);

    for i in 0..col_count {
        let is_focused = i == sel_col;
        let wf_state = workflow_states[i];

        // Collect alerts for this workflow column, with sub-states
        let mut col_alerts: Vec<(&SecurityAlert, Option<SecurityWorkflowSubState>)> = app
            .filtered_security_alerts()
            .into_iter()
            .filter_map(|a| {
                let key = workflow_key_for_alert(a);
                let (state, sub) = app
                    .security
                    .security_workflow_states
                    .get(&key)
                    .copied()
                    .unwrap_or((SecurityWorkflowState::Backlog, None));
                if state == wf_state {
                    Some((a, sub))
                } else {
                    None
                }
            })
            .collect();

        // Sort by sub-state, then repo, then number
        col_alerts.sort_by(|(a, a_sub), (b, b_sub)| {
            security_sub_state_sort_key(*a_sub)
                .cmp(&security_sub_state_sort_key(*b_sub))
                .then(a.repo.cmp(&b.repo))
                .then(a.number.cmp(&b.number))
        });

        let selected_row = app.security_selection().map(|s| s.row(i)).unwrap_or(0);
        let mut list_items: Vec<ListItem> = Vec::new();
        let mut list_selection_idx: Option<usize> = None;
        let mut current_sub: Option<Option<SecurityWorkflowSubState>> = None;

        for (item_idx, (alert, sub)) in col_alerts.iter().enumerate() {
            // Section header when sub-state changes within column
            if current_sub != Some(*sub) {
                current_sub = Some(*sub);
                if let Some(sub_state) = sub {
                    let label = sub_state.section_label();
                    list_items.push(render_substatus_header(label, list_items.is_empty()));
                } else if !list_items.is_empty() {
                    list_items.push(render_substatus_header("other", false));
                }
            }

            if item_idx == selected_row {
                list_selection_idx = Some(list_items.len());
            }

            // Tmux circle: filled if a fix agent has an active tmux window
            let fix_key = FixDispatchKey::new(alert.repo.clone(), alert.number, alert.kind);
            let tmux_alive = app
                .security
                .fix_agents
                .get(&fix_key)
                .map(|h| !h.tmux_window.is_empty())
                .unwrap_or(false);

            list_items.push(build_security_alert_item(
                alert,
                wf_state,
                *sub,
                is_focused && item_idx == selected_row,
                tmux_alive,
                col_areas[i].width,
            ));
        }

        let bg = if is_focused {
            security_column_bg_color(wf_state)
        } else {
            Color::Reset
        };

        let list = List::new(list_items).block(Block::default().style(Style::default().bg(bg)));

        let mut list_state = ListState::default();
        if is_focused {
            list_state.select(list_selection_idx);
        }

        frame.render_stateful_widget(list, col_areas[i], &mut list_state);

        if let Some(sel) = app.security_selection_mut() {
            sel.list_states[i] = list_state;
        }
    }
}

/// Build a 2-line list item for a security alert card.
///
/// Line 1: `<severity_stripe> [circle] #<number> <title> [DEP]/[SCAN]`
///   - Severity stripe: colored at far left
///   - Circle: omitted in Backlog; ◉ (cyan) if tmux alive, ○ (dim) if not
///   - Kind badge: `[DEP]` in yellow for Dependabot, `[SCAN]` in cyan for CodeScanning
///
/// Line 2: `[CRIT]/[HIGH]/[MED]/[LOW] <package> CVSS:<score> <age>`
pub(in crate::tui::ui) fn build_security_alert_item(
    alert: &SecurityAlert,
    state: SecurityWorkflowState,
    _sub: Option<SecurityWorkflowSubState>,
    is_cursor: bool,
    tmux_alive: bool,
    col_width: u16,
) -> ListItem<'static> {
    let stripe_color = severity_stripe_color(alert.severity);
    let now = Utc::now();
    let age = format_age(alert.created_at, now);

    // Stripe (cursor vs. not)
    let stripe = if is_cursor { "\u{258c} " } else { "\u{258e} " };

    // Circle indicator — omitted in Backlog, filled/empty otherwise
    let (circle_text, circle_color): (&'static str, Color) =
        if state == SecurityWorkflowState::Backlog {
            ("", Color::Reset)
        } else if tmux_alive {
            ("\u{25c9} ", CYAN) // ◉
        } else {
            ("\u{25cb} ", Color::DarkGray) // ○
        };

    // Kind badge
    let (kind_badge, kind_color): (&'static str, Color) = match alert.kind {
        AlertKind::Dependabot => (" [DEP]", YELLOW),
        AlertKind::CodeScanning => (" [SCAN]", CYAN),
    };

    // Calculate width for title
    let circle_w = if circle_text.is_empty() { 0usize } else { 2 };
    let number_prefix = format!("#{} ", alert.number);
    let badge_w = kind_badge.len();
    let overhead = 2 + circle_w + number_prefix.len() + badge_w;
    let max_title = (col_width as usize).saturating_sub(overhead).max(1);
    let title_truncated = truncate(&alert.title, max_title);

    let line1_style = if is_cursor {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(FG)
    };

    let mut spans1: Vec<Span> = vec![Span::styled(stripe.to_string(), Style::default().fg(stripe_color))];
    if !circle_text.is_empty() {
        spans1.push(Span::styled(circle_text.to_string(), Style::default().fg(circle_color)));
    }
    spans1.push(Span::styled(
        format!("{number_prefix}{title_truncated}"),
        line1_style,
    ));
    spans1.push(Span::styled(kind_badge.to_string(), Style::default().fg(kind_color)));
    let line1 = Line::from(spans1);

    // Line 2: severity badge + package + CVSS + age
    let (sev_badge, sev_color) = match alert.severity {
        AlertSeverity::Critical => ("[CRIT]", Color::Red),
        AlertSeverity::High => ("[HIGH]", YELLOW),
        AlertSeverity::Medium => ("[MED] ", Color::Rgb(86, 152, 194)),
        AlertSeverity::Low => ("[LOW] ", Color::DarkGray),
    };
    let pkg = alert.package.as_deref().unwrap_or("-");
    let cvss_str = alert
        .cvss_score
        .map(|s| format!(" \u{b7} CVSS:{s:.1}"))
        .unwrap_or_default();

    let staleness = Staleness::from_age(alert.created_at, now);
    let age_color = staleness_color(staleness);
    let meta_style = Style::default().fg(DIM_META);

    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled(sev_badge, Style::default().fg(sev_color)),
        Span::styled(format!(" {pkg}{cvss_str} \u{b7} "), meta_style),
        Span::styled(age, Style::default().fg(age_color)),
    ]);

    let bg = if is_cursor {
        severity_cursor_bg_color(alert.severity)
    } else {
        Color::Reset
    };

    ListItem::new(vec![line1, line2]).style(Style::default().bg(bg))
}

fn render_security_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER));

    if !app.security_detail_visible() {
        let paragraph = Paragraph::new("").block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let Some(alert) = app.selected_security_alert() else {
        let paragraph = Paragraph::new("").block(block);
        frame.render_widget(paragraph, area);
        return;
    };

    let color = severity_stripe_color(alert.severity);
    let now = Utc::now();
    let age = format_age(alert.created_at, now);

    // Line 1: title
    let line1 = Line::from(vec![Span::styled(
        format!("{}#{} {}", alert.repo, alert.number, alert.title),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )]);

    // Line 2: kind + severity + CVSS
    let cvss_str = alert
        .cvss_score
        .map(|s| format!(" CVSS:{s:.1}"))
        .unwrap_or_default();
    let line2 = Line::from(Span::styled(
        format!(
            "{} \u{00b7} {}{} \u{00b7} {} \u{00b7} {}",
            alert.kind.as_str(),
            alert.severity.as_str(),
            cvss_str,
            alert.repo,
            age,
        ),
        Style::default().fg(MUTED),
    ));

    // Line 3: package info or location
    let pkg_line = if let Some(pkg) = &alert.package {
        let range = alert.vulnerable_range.as_deref().unwrap_or("");
        let fix = alert
            .fixed_version
            .as_ref()
            .map(|v| format!(" \u{2192} {v}"))
            .unwrap_or_default();
        format!("Package: {pkg} {range}{fix}")
    } else {
        "No package info".to_string()
    };
    let line3 = Line::from(Span::styled(pkg_line, Style::default().fg(MUTED_LIGHT)));

    // Lines 4+: description (truncated)
    let desc_lines: Vec<Line> = alert
        .description
        .lines()
        .take(4)
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::DarkGray),
            ))
        })
        .collect();

    let mut lines = vec![line1, line2, line3];
    lines.extend(desc_lines);

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Test helper: returns concatenated text content of the card for assertions.
#[cfg(test)]
pub(in crate::tui) fn build_security_alert_item_for_test(
    alert: &SecurityAlert,
    is_cursor: bool,
    col_width: u16,
    is_running: bool,
) -> String {
    use crate::models::format_age;
    let now = chrono::Utc::now();
    let age = format_age(alert.created_at, now);
    let stripe = if is_cursor { "\u{258c} " } else { "\u{258e} " };
    let state = if is_running {
        SecurityWorkflowState::Ongoing
    } else {
        SecurityWorkflowState::Backlog
    };
    let circle = if state == SecurityWorkflowState::Backlog {
        ""
    } else if is_running {
        "\u{25c9} "
    } else {
        "\u{25cb} "
    };
    let (kind_badge, _) = match alert.kind {
        AlertKind::Dependabot => (" [DEP]", YELLOW),
        AlertKind::CodeScanning => (" [SCAN]", CYAN),
    };
    let number_prefix = format!("#{} ", alert.number);
    let badge_w = kind_badge.len();
    let circle_w = if circle.is_empty() { 0usize } else { 2 };
    let overhead = 2 + circle_w + number_prefix.len() + badge_w;
    let max_title = (col_width as usize).saturating_sub(overhead).max(1);
    let title_truncated = truncate(&alert.title, max_title);
    let (sev_badge, _) = match alert.severity {
        AlertSeverity::Critical => ("[CRIT]", Color::Red),
        AlertSeverity::High => ("[HIGH]", YELLOW),
        AlertSeverity::Medium => ("[MED] ", Color::Rgb(86, 152, 194)),
        AlertSeverity::Low => ("[LOW] ", Color::DarkGray),
    };
    let pkg = alert.package.as_deref().unwrap_or("-");
    let cvss_str = alert
        .cvss_score
        .map(|s| format!(" \u{b7} CVSS:{s:.1}"))
        .unwrap_or_default();
    format!(
        "{stripe}{circle}{number_prefix}{title_truncated}{kind_badge}  {sev_badge} {pkg}{cvss_str} \u{b7} {age}",
    )
}

fn render_security_repo_filter_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let repos = app.active_security_repos();
    let mode_str = app.security.repo_filter_mode.as_str();
    render_security_filter_overlay_inner(frame, area, repos, mode_str, |r| {
        app.security.repo_filter.contains(r)
    });
}

fn render_security_filter_overlay_inner(
    frame: &mut Frame,
    area: Rect,
    repos: &[String],
    mode_str: &str,
    is_selected: impl Fn(&str) -> bool,
) {
    if repos.is_empty() {
        return;
    }

    let height = (repos.len() as u16 + 4).min(area.height.saturating_sub(2));
    let width = 50.min(area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" Filter Repos ({mode_str}) "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CYAN));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" Mode: {mode_str} [Tab] toggle"),
        Style::default().fg(MUTED_LIGHT),
    )));
    lines.push(Line::from(Span::styled(
        " [a]ll toggle",
        Style::default().fg(MUTED),
    )));

    for (i, repo) in repos.iter().enumerate() {
        let selected = is_selected(repo);
        let marker = if selected { "\u{25c9}" } else { "\u{25cb}" };
        let num = i + 1;
        let line = Line::from(vec![
            Span::styled(
                format!(" {num}"),
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {marker} {repo}"),
                if selected {
                    Style::default().fg(FG)
                } else {
                    Style::default().fg(MUTED)
                },
            ),
        ]);
        lines.push(line);
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// Dependabot PR action hints — kept for use from input/mod.rs
// ---------------------------------------------------------------------------

pub(in crate::tui) fn dependabot_action_hints(
    has_selected: bool,
    selected_pr: Option<&crate::models::ReviewPr>,
    agent_status: Option<crate::models::ReviewAgentStatus>,
) -> Vec<Span<'static>> {
    use crate::models::ReviewAgentStatus;
    let key_color = Color::Cyan;
    let label_style = Style::default().fg(MUTED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push_hint = |spans: &mut Vec<Span<'static>>, key: &'static str, label: String| {
        push_hint_spans(spans, key, &label, key_color, label_style);
    };

    if has_selected {
        push_hint(&mut spans, "a", "approve".into());
        push_hint(&mut spans, "m", "merge".into());
        push_hint(&mut spans, "Esc", "clear".into());
    } else if selected_pr.is_some() {
        push_hint(&mut spans, "Space", "select".into());
        match agent_status {
            Some(ReviewAgentStatus::Idle) => {
                push_hint(&mut spans, "g", "go to".into());
                push_hint(&mut spans, "d", "resume".into());
                push_hint(&mut spans, "T", "detach".into());
            }
            Some(_) => {
                push_hint(&mut spans, "g", "go to".into());
                push_hint(&mut spans, "T", "detach".into());
            }
            None => {
                push_hint(&mut spans, "d", "dispatch".into());
            }
        }
        push_hint(&mut spans, "p", "open".into());
    }
    push_hint(&mut spans, "Tab", "tasks".into());
    push_hint(&mut spans, "?", "help".into());
    push_hint(&mut spans, "q", "quit".into());
    spans
}
