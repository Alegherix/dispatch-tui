use crate::models::{CiStatus, ReviewDecision, ReviewPr, Reviewer};
use crate::process::ProcessRunner;
use chrono::{DateTime, Utc};

/// Determine the effective review decision for a PR node.
///
/// Uses the overall `reviewDecision` for APPROVED and CHANGES_REQUESTED.
/// For REVIEW_REQUIRED, checks whether the viewer has left comments (plain PR
/// comments or COMMENTED-state reviews) and whether the PR author has responded
/// since (via a new comment or a new commit).
fn classify_review_decision(node: &serde_json::Value, viewer_login: &str) -> ReviewDecision {
    let decision_str = node["reviewDecision"].as_str().unwrap_or("REVIEW_REQUIRED");
    match decision_str {
        "APPROVED" => return ReviewDecision::Approved,
        "CHANGES_REQUESTED" => return ReviewDecision::ChangesRequested,
        _ => {}
    }

    // Re-request is the strongest signal: if the viewer is in the current
    // reviewRequests list, the author explicitly asked for another look.
    let viewer_re_requested = node["reviewRequests"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|req| req["requestedReviewer"]["login"].as_str() == Some(viewer_login));

    if viewer_re_requested {
        return ReviewDecision::ReviewRequired;
    }

    let pr_author = node["author"]["login"].as_str().unwrap_or("");

    // Viewer's last plain comment
    let viewer_last_comment = node["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["author"]["login"].as_str() == Some(viewer_login))
        .filter_map(|c| c["createdAt"].as_str()?.parse::<DateTime<Utc>>().ok())
        .max();

    // Viewer's last review (any state: APPROVED, CHANGES_REQUESTED, COMMENTED)
    let viewer_last_review = node["reviews"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["author"]["login"].as_str() == Some(viewer_login))
        .filter_map(|r| r["submittedAt"].as_str()?.parse::<DateTime<Utc>>().ok())
        .max();

    let viewer_last_interaction = viewer_last_comment.max(viewer_last_review);

    let Some(interaction_at) = viewer_last_interaction else {
        return ReviewDecision::ReviewRequired;
    };

    // Check if author has responded since the viewer's last interaction
    let author_last_comment = node["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["author"]["login"].as_str() == Some(pr_author))
        .filter_map(|c| c["createdAt"].as_str()?.parse::<DateTime<Utc>>().ok())
        .max();

    let last_commit_date = node["commits"]["nodes"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|n| n["commit"]["committedDate"].as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    let author_responded = author_last_comment.is_some_and(|t| t > interaction_at)
        || last_commit_date.is_some_and(|t| t > interaction_at);

    if author_responded {
        ReviewDecision::ReviewRequired
    } else {
        ReviewDecision::WaitingForResponse
    }
}

/// Extract reviewers from a PR node by merging completed reviews and pending
/// review requests. A reviewer who has left an APPROVED or CHANGES_REQUESTED
/// review gets that decision; a pending request (not yet reviewed) gets `None`.
fn parse_reviewers(node: &serde_json::Value) -> Vec<Reviewer> {
    let mut by_login: std::collections::HashMap<String, Option<ReviewDecision>> =
        std::collections::HashMap::new();

    // Completed reviews — latest state per reviewer
    if let Some(reviews) = node["reviews"]["nodes"].as_array() {
        for review in reviews {
            if let Some(login) = review["author"]["login"].as_str() {
                let decision = match review["state"].as_str() {
                    Some("APPROVED") => Some(ReviewDecision::Approved),
                    Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
                    _ => continue,
                };
                by_login.insert(login.to_string(), decision);
            }
        }
    }

    // Pending review requests — only add if not already reviewed
    if let Some(requests) = node["reviewRequests"]["nodes"].as_array() {
        for req in requests {
            if let Some(login) = req["requestedReviewer"]["login"].as_str() {
                by_login.entry(login.to_string()).or_insert(None);
            }
        }
    }

    by_login
        .into_iter()
        .map(|(login, decision)| Reviewer { login, decision })
        .collect()
}

/// Build a single `ReviewPr` from a JSON node. Returns `None` for drafts.
fn build_review_pr(node: &serde_json::Value, viewer_login: &str) -> Option<ReviewPr> {
    if node["isDraft"].as_bool() == Some(true) {
        return None;
    }

    let review_decision = classify_review_decision(node, viewer_login);

    let labels = node["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let created_at = node["createdAt"]
        .as_str()
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);
    let updated_at = node["updatedAt"]
        .as_str()
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);

    let body = node["body"].as_str().unwrap_or("").to_string();
    let head_ref = node["headRefName"].as_str().unwrap_or("").to_string();

    let ci_state = node["commits"]["nodes"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|n| n["commit"]["statusCheckRollup"]["state"].as_str());
    let ci_status = CiStatus::from_github(ci_state);

    let reviewers = parse_reviewers(node);

    Some(ReviewPr {
        number: node["number"].as_i64().unwrap_or(0),
        title: node["title"].as_str().unwrap_or("").to_string(),
        author: node["author"]["login"].as_str().unwrap_or("").to_string(),
        repo: node["repository"]["nameWithOwner"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        url: node["url"].as_str().unwrap_or("").to_string(),
        is_draft: false,
        created_at,
        updated_at,
        additions: node["additions"].as_i64().unwrap_or(0),
        deletions: node["deletions"].as_i64().unwrap_or(0),
        review_decision,
        labels,
        body,
        head_ref,
        ci_status,
        reviewers,
    })
}

/// The three review-PR search aliases and their GitHub search queries.
const REVIEW_ALIASES: [(&str, &str); 3] = [
    (
        "requestedReview",
        "is:pr is:open review-requested:@me -is:draft -author:app/dependabot -author:app/renovate archived:false",
    ),
    (
        "alreadyReviewed",
        "is:pr is:open reviewed-by:@me -author:@me -is:draft -author:app/dependabot -author:app/renovate archived:false",
    ),
    (
        "commented",
        "is:pr is:open commenter:@me -author:@me -is:draft -author:app/dependabot -author:app/renovate archived:false",
    ),
];

/// Pagination state for a single search alias.
struct AliasPageInfo {
    has_next: bool,
    end_cursor: Option<String>,
}

/// Result of extracting one page of the review-PR GraphQL response.
struct ReviewPage {
    viewer_login: String,
    nodes: Vec<serde_json::Value>,
    page_infos: Vec<AliasPageInfo>,
}

/// Extract unique PR nodes and per-alias page info from one page of the
/// review-PR GraphQL response. Nodes whose URL is already in `seen_urls`
/// are skipped (cross-page and cross-alias deduplication).
fn extract_review_page(
    json: &str,
    aliases: &[&str],
    seen_urls: &mut std::collections::HashSet<String>,
) -> Result<ReviewPage, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let viewer_login = root
        .pointer("/data/viewer/login")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut nodes = Vec::new();
    let mut page_infos = Vec::new();

    for alias in aliases {
        let nodes_path = format!("/data/{alias}/nodes");
        if let Some(alias_nodes) = root.pointer(&nodes_path).and_then(|v| v.as_array()) {
            for node in alias_nodes {
                let url = node["url"].as_str().unwrap_or("").to_string();
                if !url.is_empty() && seen_urls.insert(url) {
                    nodes.push(node.clone());
                }
            }
        }

        let pi_path = format!("/data/{alias}/pageInfo");
        let has_next = root
            .pointer(&pi_path)
            .and_then(|p| p["hasNextPage"].as_bool())
            .unwrap_or(false);
        let end_cursor = root
            .pointer(&pi_path)
            .and_then(|p| p["endCursor"].as_str())
            .map(|s| s.to_string());
        page_infos.push(AliasPageInfo {
            has_next,
            end_cursor,
        });
    }

    Ok(ReviewPage {
        viewer_login,
        nodes,
        page_infos,
    })
}

/// Parse a single-page review PR response (convenience wrapper for tests).
#[cfg(test)]
fn parse_review_prs(json: &str) -> Result<Vec<ReviewPr>, String> {
    let aliases: Vec<&str> = REVIEW_ALIASES.iter().map(|(name, _)| *name).collect();
    let mut seen = std::collections::HashSet::new();
    let page = extract_review_page(json, &aliases, &mut seen)?;
    Ok(page
        .nodes
        .iter()
        .filter_map(|n| build_review_pr(n, &page.viewer_login))
        .collect())
}

/// Parse a single-page my-PRs response (convenience wrapper for tests).
#[cfg(test)]
fn parse_my_prs(json: &str) -> Result<Vec<ReviewPr>, String> {
    let mut seen = std::collections::HashSet::new();
    let page = extract_review_page(json, &["myPrs"], &mut seen)?;
    Ok(page
        .nodes
        .iter()
        .filter_map(|n| build_review_pr(n, &page.viewer_login))
        .collect())
}

/// The PR fields fragment used in both search aliases.
const PR_FIELDS: &str = r#"... on PullRequest {
        number
        title
        url
        isDraft
        createdAt
        updatedAt
        additions
        deletions
        reviewDecision
        body
        headRefName
        author { login }
        repository { nameWithOwner }
        labels(first: 10) { nodes { name } }
        comments(last: 50) { nodes { author { login } createdAt } }
        reviews(last: 20) { nodes { state author { login } submittedAt } }
        reviewRequests(first: 10) { nodes { requestedReviewer { ... on User { login } } } }
        commits(last: 1) { nodes { commit { committedDate statusCheckRollup { state } } } }
      }"#;

/// Build a GraphQL search alias fragment with optional cursor-based pagination.
fn build_search_alias(
    name: &str,
    search_query: &str,
    page_size: usize,
    cursor: &Option<String>,
) -> String {
    let after = cursor
        .as_ref()
        .map(|c| format!(", after: \"{c}\""))
        .unwrap_or_default();
    format!(
        r#"  {name}: search(query: "{search_query}", type: ISSUE, first: {page_size}{after}) {{
    pageInfo {{ hasNextPage endCursor }}
    nodes {{
      {PR_FIELDS}
    }}
  }}"#
    )
}

/// Page size for review PR fetches — smaller pages yield faster individual
/// responses because each PR node carries heavy nested fields (reviews,
/// comments, commits, labels).
const REVIEW_PAGE_SIZE: usize = 25;

/// Maximum number of pagination requests per fetch cycle.
const REVIEW_MAX_PAGES: usize = 3;

/// Fetch open PRs where the current user is a requested or past reviewer.
///
/// Uses three aliased GraphQL searches per request:
/// - `requestedReview`: PRs where `review-requested:@me` (pending review)
/// - `alreadyReviewed`: PRs where `reviewed-by:@me` (already reviewed, may need re-review)
/// - `commented`: PRs where `commenter:@me` (left comments but no formal review)
///
/// Paginates with `first: 25` and up to 3 pages (75 PRs per alias).
/// Aliases that are exhausted are excluded from subsequent requests.
/// The three result sets are merged and deduplicated by URL client-side.
/// Own PRs (`-author:@me`), bot authors, and archived repos are excluded server-side.
pub fn fetch_review_prs(runner: &dyn ProcessRunner) -> Result<Vec<ReviewPr>, String> {
    let mut cursors: [Option<String>; 3] = [None, None, None];
    let mut has_next = [true, true, true];
    let mut seen_urls = std::collections::HashSet::new();
    let mut all_nodes: Vec<serde_json::Value> = Vec::new();
    let mut viewer_login = String::new();

    for _ in 0..REVIEW_MAX_PAGES {
        // Build query with only aliases that still have pages.
        let mut alias_fragments = Vec::new();
        let mut active_indices = Vec::new();
        for (i, (name, search_query)) in REVIEW_ALIASES.iter().enumerate() {
            if has_next[i] {
                alias_fragments.push(build_search_alias(
                    name,
                    search_query,
                    REVIEW_PAGE_SIZE,
                    &cursors[i],
                ));
                active_indices.push(i);
            }
        }

        if alias_fragments.is_empty() {
            break;
        }

        let fragments = alias_fragments.join("\n");
        let query = format!(
            "{{\n  viewer {{ login }}\n{fragments}\n}}"
        );

        let output = runner
            .run("gh", &["api", "graphql", "-f", &format!("query={query}")])
            .map_err(|e| format!("Failed to run gh: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("gh api graphql failed: {stderr}"));
        }

        let json = String::from_utf8_lossy(&output.stdout);
        let active_aliases: Vec<&str> = active_indices
            .iter()
            .map(|&i| REVIEW_ALIASES[i].0)
            .collect();
        let page = extract_review_page(&json, &active_aliases, &mut seen_urls)?;

        if viewer_login.is_empty() {
            viewer_login = page.viewer_login;
        }
        all_nodes.extend(page.nodes);

        // Update cursors for active aliases.
        let mut any_has_next = false;
        for (pi_idx, &orig_idx) in active_indices.iter().enumerate() {
            let pi = &page.page_infos[pi_idx];
            has_next[orig_idx] = pi.has_next;
            cursors[orig_idx] = pi.end_cursor.clone();
            if pi.has_next {
                any_has_next = true;
            }
        }
        if !any_has_next {
            break;
        }
    }

    Ok(all_nodes
        .iter()
        .filter_map(|n| build_review_pr(n, &viewer_login))
        .collect())
}

/// Fetch the current user's own open PRs (non-draft).
///
/// Uses `author:@me` in a single GraphQL search alias (`myPrs`).
/// Paginates with the same page size and max pages as review PRs.
pub fn fetch_my_prs(runner: &dyn ProcessRunner) -> Result<Vec<ReviewPr>, String> {
    let search_query = "is:pr is:open author:@me -is:draft archived:false";
    let mut cursor: Option<String> = None;
    let mut seen_urls = std::collections::HashSet::new();
    let mut all_nodes: Vec<serde_json::Value> = Vec::new();
    let mut viewer_login = String::new();

    for _ in 0..REVIEW_MAX_PAGES {
        let fragment = build_search_alias("myPrs", search_query, REVIEW_PAGE_SIZE, &cursor);
        let query = format!("{{\n  viewer {{ login }}\n{fragment}\n}}");

        let output = runner
            .run("gh", &["api", "graphql", "-f", &format!("query={query}")])
            .map_err(|e| format!("Failed to run gh: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("gh api graphql failed: {stderr}"));
        }

        let json = String::from_utf8_lossy(&output.stdout);
        let page = extract_review_page(&json, &["myPrs"], &mut seen_urls)?;

        if viewer_login.is_empty() {
            viewer_login = page.viewer_login;
        }
        all_nodes.extend(page.nodes);

        let pi = &page.page_infos[0];
        if !pi.has_next {
            break;
        }
        cursor = pi.end_cursor.clone();
    }

    Ok(all_nodes
        .iter()
        .filter_map(|n| build_review_pr(n, &viewer_login))
        .collect())
}

/// Fetch open dependency-bot PRs (dependabot + renovate), non-draft.
///
/// Uses two GraphQL calls:
/// 1. Fetch the viewer's login and organization memberships.
/// 2. Run two aliased searches scoped with `user:` and `org:` qualifiers,
///    so only PRs from the viewer's own repos and org repos are returned.
///
/// The two result sets are merged and deduplicated by URL client-side.
pub fn fetch_bot_prs(runner: &dyn ProcessRunner) -> Result<Vec<ReviewPr>, String> {
    // Call 1: fetch viewer login + org logins to build owner scope.
    let identity_query = r#"{ viewer { login organizations(first: 100) { nodes { login } } } }"#;
    let output = runner
        .run(
            "gh",
            &["api", "graphql", "-f", &format!("query={identity_query}")],
        )
        .map_err(|e| format!("Failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api graphql failed: {stderr}"));
    }

    let identity_json = String::from_utf8_lossy(&output.stdout);
    let identity: serde_json::Value =
        serde_json::from_str(&identity_json).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let viewer_login = identity
        .pointer("/data/viewer/login")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut owner_scope = String::new();
    if !viewer_login.is_empty() {
        owner_scope.push_str(&format!("user:{viewer_login}"));
    }
    if let Some(org_nodes) = identity
        .pointer("/data/viewer/organizations/nodes")
        .and_then(|v| v.as_array())
    {
        for org in org_nodes {
            if let Some(login) = org["login"].as_str() {
                owner_scope.push_str(&format!(" org:{login}"));
            }
        }
    }

    // Call 2: search with owner scope injected into the query.
    let query = format!(
        r#"{{
  viewer {{ login }}
  dependabot: search(query: "is:pr is:open author:app/dependabot -is:draft {owner_scope}", type: ISSUE, first: 10) {{
    nodes {{
      {PR_FIELDS}
    }}
  }}
  renovate: search(query: "is:pr is:open author:app/renovate -is:draft {owner_scope}", type: ISSUE, first: 10) {{
    nodes {{
      {PR_FIELDS}
    }}
  }}
}}"#
    );

    let output = runner
        .run("gh", &["api", "graphql", "-f", &format!("query={query}")])
        .map_err(|e| format!("Failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api graphql failed: {stderr}"));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    parse_bot_prs(&json)
}

/// Parse the bot PRs response (dependabot + renovate aliases), deduplicate by URL.
fn parse_bot_prs(json: &str) -> Result<Vec<ReviewPr>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let viewer_login = root
        .pointer("/data/viewer/login")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut seen_urls = std::collections::HashSet::new();
    let mut all_nodes: Vec<serde_json::Value> = Vec::new();
    for alias in &["dependabot", "renovate"] {
        let path = format!("/data/{alias}/nodes");
        if let Some(nodes) = root.pointer(&path).and_then(|v| v.as_array()) {
            for node in nodes {
                let url = node["url"].as_str().unwrap_or("").to_string();
                if !url.is_empty() && seen_urls.insert(url) {
                    all_nodes.push(node.clone());
                }
            }
        }
    }

    let mut prs = Vec::with_capacity(all_nodes.len());
    for node in &all_nodes {
        if node["isDraft"].as_bool() == Some(true) {
            continue;
        }

        let review_decision = classify_review_decision(node, viewer_login);

        let labels = node["labels"]["nodes"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let created_at = node["createdAt"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);
        let updated_at = node["updatedAt"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);

        let body = node["body"].as_str().unwrap_or("").to_string();
        let head_ref = node["headRefName"].as_str().unwrap_or("").to_string();

        let ci_state = node["commits"]["nodes"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|n| n["commit"]["statusCheckRollup"]["state"].as_str());
        let ci_status = CiStatus::from_github(ci_state);

        let reviewers = parse_reviewers(node);

        prs.push(ReviewPr {
            number: node["number"].as_i64().unwrap_or(0),
            title: node["title"].as_str().unwrap_or("").to_string(),
            author: node["author"]["login"].as_str().unwrap_or("").to_string(),
            repo: node["repository"]["nameWithOwner"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            url: node["url"].as_str().unwrap_or("").to_string(),
            is_draft: node["isDraft"].as_bool().unwrap_or(false),
            created_at,
            updated_at,
            additions: node["additions"].as_i64().unwrap_or(0),
            deletions: node["deletions"].as_i64().unwrap_or(0),
            review_decision,
            labels,
            body,
            head_ref,
            ci_status,
            reviewers,
        });
    }

    prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(prs)
}

// ---------------------------------------------------------------------------
// Security alerts
// ---------------------------------------------------------------------------

use crate::models::{AlertKind, AlertSeverity, SecurityAlert};

/// The GraphQL fields fragment for vulnerability alerts.
const VULN_ALERT_FIELDS: &str = r#"nodes {
              nameWithOwner
              vulnerabilityAlerts(first: 25, states: OPEN) {
                nodes {
                  number
                  createdAt
                  securityVulnerability {
                    severity
                    package { name }
                    vulnerableVersionRange
                    firstPatchedVersion { identifier }
                  }
                  securityAdvisory {
                    summary
                    description
                    cvss { score }
                  }
                }
              }
            }"#;

/// Parse the GraphQL vulnerability alerts response into `SecurityAlert`s.
fn parse_graphql_security_alerts(json: &str) -> Result<Vec<SecurityAlert>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let repos = root
        .pointer("/data/viewer/repositories/nodes")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
        .clone();

    let mut alerts = Vec::new();
    for repo_node in &repos {
        let repo = repo_node["nameWithOwner"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let alert_nodes = match repo_node
            .pointer("/vulnerabilityAlerts/nodes")
            .and_then(|v| v.as_array())
        {
            Some(nodes) => nodes,
            None => continue,
        };

        for node in alert_nodes {
            let number = node["number"].as_i64().unwrap_or(0);
            let severity_str = node
                .pointer("/securityVulnerability/severity")
                .and_then(|v| v.as_str())
                .unwrap_or("MODERATE");
            let severity = AlertSeverity::parse(severity_str).unwrap_or(AlertSeverity::Medium);

            let title = node
                .pointer("/securityAdvisory/summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let package = node
                .pointer("/securityVulnerability/package/name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let vulnerable_range = node
                .pointer("/securityVulnerability/vulnerableVersionRange")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let fixed_version = node
                .pointer("/securityVulnerability/firstPatchedVersion/identifier")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let cvss_score = node
                .pointer("/securityAdvisory/cvss/score")
                .and_then(|v| v.as_f64());
            let url = format!(
                "https://github.com/{repo}/security/dependabot/{number}"
            );
            let created_at = node["createdAt"]
                .as_str()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                .unwrap_or_else(Utc::now);
            let description = node
                .pointer("/securityAdvisory/description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            alerts.push(SecurityAlert {
                number,
                repo: repo.clone(),
                severity,
                kind: AlertKind::Dependabot,
                title,
                package,
                vulnerable_range,
                fixed_version,
                cvss_score,
                url,
                created_at,
                state: "open".to_string(),
                description,
            });
        }
    }
    Ok(alerts)
}

/// Fetch security alerts from GitHub using GraphQL `vulnerabilityAlerts`.
///
/// Paginates through the viewer's repositories (ordered by most recently pushed),
/// collecting open Dependabot vulnerability alerts. Uses at most `MAX_PAGES`
/// GraphQL requests (100 repos each), which is dramatically faster than the
/// per-repo REST API approach.
///
/// Results are sorted by severity (critical first), then CVSS score descending.
pub fn fetch_security_alerts(runner: &dyn ProcessRunner) -> Result<Vec<SecurityAlert>, String> {
    const MAX_PAGES: usize = 3;

    let mut all_alerts: Vec<SecurityAlert> = Vec::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_PAGES {
        let after_clause = match &cursor {
            Some(c) => format!(", after: \"{c}\""),
            None => String::new(),
        };

        let query = format!(
            r#"{{
  viewer {{
    repositories(first: 100, affiliations: [OWNER, ORGANIZATION_MEMBER], orderBy: {{field: PUSHED_AT, direction: DESC}}{after_clause}) {{
      pageInfo {{ hasNextPage endCursor }}
      {VULN_ALERT_FIELDS}
    }}
  }}
}}"#
        );

        let output = runner
            .run("gh", &["api", "graphql", "-f", &format!("query={query}")])
            .map_err(|e| format!("Failed to run gh: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("gh api graphql failed: {stderr}"));
        }

        let json = String::from_utf8_lossy(&output.stdout);
        all_alerts.extend(parse_graphql_security_alerts(&json)?);

        let root: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse JSON: {e}"))?;
        let page_info = root.pointer("/data/viewer/repositories/pageInfo");
        let has_next = page_info
            .and_then(|p| p["hasNextPage"].as_bool())
            .unwrap_or(false);
        if !has_next {
            break;
        }
        cursor = page_info
            .and_then(|p| p["endCursor"].as_str())
            .map(|s| s.to_string());
    }

    // Sort by severity (critical first), then CVSS descending
    all_alerts.sort_by(|a, b| {
        a.severity
            .column_index()
            .cmp(&b.severity.column_index())
            .then_with(|| {
                b.cvss_score
                    .unwrap_or(0.0)
                    .partial_cmp(&a.cvss_score.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    Ok(all_alerts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::MockProcessRunner;

    // PR #42 is in requestedReview (pending review), PR #99 is a draft (filtered),
    // PR #50 is in alreadyReviewed (already reviewed, approved),
    // PR #60 is in commented (only left comments, no formal review).
    const SAMPLE_RESPONSE: &str = r#"{
        "data": {
            "viewer": {"login": "me"},
            "requestedReview": {
                "nodes": [
                    {
                        "number": 42,
                        "title": "Fix login flow",
                        "url": "https://github.com/acme/app/pull/42",
                        "isDraft": false,
                        "createdAt": "2026-03-28T10:00:00Z",
                        "updatedAt": "2026-03-29T14:00:00Z",
                        "additions": 15,
                        "deletions": 3,
                        "reviewDecision": "REVIEW_REQUIRED",
                        "author": {"login": "alice"},
                        "repository": {"nameWithOwner": "acme/app"},
                        "labels": {"nodes": [{"name": "bug"}, {"name": "urgent"}]},
                        "comments": {"nodes": []},
                        "reviews": {"nodes": []},
                        "commits": {"nodes": [{"commit": {"committedDate": "2026-03-28T10:00:00Z"}}]}
                    },
                    {
                        "number": 99,
                        "title": "Update sbt to 1.12",
                        "url": "https://github.com/acme/app/pull/99",
                        "isDraft": true,
                        "createdAt": "2026-03-27T08:00:00Z",
                        "updatedAt": "2026-03-27T08:00:00Z",
                        "additions": 1,
                        "deletions": 1,
                        "reviewDecision": "REVIEW_REQUIRED",
                        "author": {"login": "scala-steward"},
                        "repository": {"nameWithOwner": "acme/app"},
                        "labels": {"nodes": []},
                        "comments": {"nodes": []},
                        "reviews": {"nodes": []},
                        "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T08:00:00Z"}}]}
                    }
                ]
            },
            "alreadyReviewed": {
                "nodes": [
                    {
                        "number": 50,
                        "title": "Refactor auth module",
                        "url": "https://github.com/acme/backend/pull/50",
                        "isDraft": false,
                        "createdAt": "2026-03-25T12:00:00Z",
                        "updatedAt": "2026-03-29T09:00:00Z",
                        "additions": 200,
                        "deletions": 80,
                        "reviewDecision": "APPROVED",
                        "author": {"login": "bob"},
                        "repository": {"nameWithOwner": "acme/backend"},
                        "labels": {"nodes": [{"name": "refactor"}]},
                        "comments": {"nodes": []},
                        "reviews": {"nodes": []},
                        "commits": {"nodes": [{"commit": {"committedDate": "2026-03-25T12:00:00Z"}}]}
                    }
                ]
            },
            "commented": {
                "nodes": [
                    {
                        "number": 60,
                        "title": "Add logging to auth",
                        "url": "https://github.com/acme/backend/pull/60",
                        "isDraft": false,
                        "createdAt": "2026-03-26T08:00:00Z",
                        "updatedAt": "2026-03-29T10:00:00Z",
                        "additions": 30,
                        "deletions": 5,
                        "reviewDecision": "REVIEW_REQUIRED",
                        "author": {"login": "carol"},
                        "repository": {"nameWithOwner": "acme/backend"},
                        "labels": {"nodes": []},
                        "comments": {"nodes": [
                            {"author": {"login": "me"}, "createdAt": "2026-03-27T10:00:00Z"}
                        ]},
                        "reviews": {"nodes": []},
                        "commits": {"nodes": [{"commit": {"committedDate": "2026-03-26T08:00:00Z"}}]}
                    }
                ]
            }
        }
    }"#;

    #[test]
    fn parse_review_prs_extracts_all_fields() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
        // Draft PR #99 is filtered out, leaving 3
        assert_eq!(prs.len(), 3);

        let pr = &prs[0];
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Fix login flow");
        assert_eq!(pr.author, "alice");
        assert_eq!(pr.repo, "acme/app");
        assert_eq!(pr.url, "https://github.com/acme/app/pull/42");
        assert!(!pr.is_draft);
        assert_eq!(pr.additions, 15);
        assert_eq!(pr.deletions, 3);
        assert_eq!(pr.review_decision, ReviewDecision::ReviewRequired);
        assert_eq!(pr.labels, vec!["bug", "urgent"]);
    }

    #[test]
    fn parse_review_prs_filters_drafts_and_handles_approved() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
        // Draft PR #99 excluded; #50 (approved) is now index 1
        assert_eq!(prs.len(), 3);
        assert_eq!(prs[1].review_decision, ReviewDecision::Approved);
    }

    #[test]
    fn parse_review_prs_empty_nodes() {
        let json = r#"{"data":{"viewer":{"login":"me"},"requestedReview":{"nodes":[]},"alreadyReviewed":{"nodes":[]},"commented":{"nodes":[]}}}"#;
        let prs = parse_review_prs(json).unwrap();
        assert!(prs.is_empty());
    }

    #[test]
    fn parse_review_prs_invalid_json() {
        let result = parse_review_prs("not json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_review_prs_null_review_decision_defaults_to_review_required() {
        let json = r#"{"data":{"viewer":{"login":"me"},"requestedReview":{"nodes":[{
            "number": 1, "title": "T", "url": "https://github.com/o/r/pull/1", "isDraft": false,
            "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
            "additions": 0, "deletions": 0,
            "reviewDecision": null,
            "author": {"login": "a"}, "repository": {"nameWithOwner": "o/r"},
            "labels": {"nodes": []},
            "comments": {"nodes": []},
            "reviews": {"nodes": []},
            "commits": {"nodes": []}
        }]},"alreadyReviewed":{"nodes":[]},"commented":{"nodes":[]}}}"#;
        let prs = parse_review_prs(json).unwrap();
        assert_eq!(prs[0].review_decision, ReviewDecision::ReviewRequired);
    }

    #[test]
    fn fetch_review_prs_calls_gh_and_parses() {
        let runner = MockProcessRunner::new(vec![MockProcessRunner::ok_with_stdout(
            SAMPLE_RESPONSE.as_bytes(),
        )]);
        let prs = fetch_review_prs(&runner).unwrap();
        assert_eq!(prs.len(), 3); // draft filtered out

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gh");
        assert!(calls[0].1.contains(&"graphql".to_string()));
    }

    #[test]
    fn fetch_review_prs_gh_failure() {
        let runner = MockProcessRunner::new(vec![MockProcessRunner::fail("gh: not authenticated")]);
        let result = fetch_review_prs(&runner);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not authenticated"));
    }

    #[test]
    fn fetch_review_prs_query_includes_all_searches() {
        let runner = MockProcessRunner::new(vec![MockProcessRunner::ok_with_stdout(
            SAMPLE_RESPONSE.as_bytes(),
        )]);
        let _ = fetch_review_prs(&runner);
        let calls = runner.recorded_calls();
        let query_arg = calls[0].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            query_arg.contains("review-requested:@me"),
            "missing review-requested qualifier"
        );
        assert!(
            query_arg.contains("reviewed-by:@me"),
            "missing reviewed-by qualifier"
        );
        assert!(
            query_arg.contains("commenter:@me"),
            "missing commenter qualifier"
        );
        assert!(query_arg.contains("-is:draft"));
        assert!(query_arg.contains("-author:app/dependabot"));
        assert!(query_arg.contains("-author:app/renovate"));
        assert!(query_arg.contains("-author:@me"));
    }

    #[test]
    fn parse_review_prs_deduplicates_across_aliases() {
        // PR #42 appears in all three aliases — should only be counted once.
        let json = r#"{
            "data": {
                "viewer": {"login": "me"},
                "requestedReview": {"nodes": [{
                    "number": 42, "title": "Fix login flow",
                    "url": "https://github.com/acme/app/pull/42",
                    "isDraft": false,
                    "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
                    "additions": 15, "deletions": 3,
                    "reviewDecision": "REVIEW_REQUIRED",
                    "author": {"login": "alice"}, "repository": {"nameWithOwner": "acme/app"},
                    "labels": {"nodes": []}, "comments": {"nodes": []},
                    "reviews": {"nodes": []}, "commits": {"nodes": []}
                }]},
                "alreadyReviewed": {"nodes": [{
                    "number": 42, "title": "Fix login flow",
                    "url": "https://github.com/acme/app/pull/42",
                    "isDraft": false,
                    "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
                    "additions": 15, "deletions": 3,
                    "reviewDecision": "REVIEW_REQUIRED",
                    "author": {"login": "alice"}, "repository": {"nameWithOwner": "acme/app"},
                    "labels": {"nodes": []}, "comments": {"nodes": []},
                    "reviews": {"nodes": []}, "commits": {"nodes": []}
                }]},
                "commented": {"nodes": [{
                    "number": 42, "title": "Fix login flow",
                    "url": "https://github.com/acme/app/pull/42",
                    "isDraft": false,
                    "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
                    "additions": 15, "deletions": 3,
                    "reviewDecision": "REVIEW_REQUIRED",
                    "author": {"login": "alice"}, "repository": {"nameWithOwner": "acme/app"},
                    "labels": {"nodes": []}, "comments": {"nodes": []},
                    "reviews": {"nodes": []}, "commits": {"nodes": []}
                }]}
            }
        }"#;
        let prs = parse_review_prs(json).unwrap();
        assert_eq!(prs.len(), 1, "duplicate should be deduplicated");
        assert_eq!(prs[0].number, 42);
    }

    // -----------------------------------------------------------------------
    // classify_review_decision tests
    // -----------------------------------------------------------------------

    fn make_pr_node(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn classify_approved_takes_priority() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "APPROVED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": []}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::Approved
        );
    }

    #[test]
    fn classify_changes_requested_takes_priority() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "CHANGES_REQUESTED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": []}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::ChangesRequested,
        );
    }

    #[test]
    fn classify_no_viewer_interaction() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": []},
            "reviews": {"nodes": []},
            "commits": {"nodes": []}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::ReviewRequired,
        );
    }

    #[test]
    fn classify_viewer_comment_no_author_response() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T10:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::WaitingForResponse,
        );
    }

    #[test]
    fn classify_viewer_commented_review_no_response() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": []},
            "reviews": {"nodes": [
                {"state": "COMMENTED", "author": {"login": "me"}, "submittedAt": "2026-03-28T12:00:00Z"}
            ]},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T10:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::WaitingForResponse,
        );
    }

    #[test]
    fn classify_viewer_approved_review_no_response() {
        // Viewer approved but overall PR still needs other reviews.
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": []},
            "reviews": {"nodes": [
                {"state": "APPROVED", "author": {"login": "me"}, "submittedAt": "2026-03-28T12:00:00Z"}
            ]},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T10:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::WaitingForResponse,
        );
    }

    #[test]
    fn classify_author_comment_after_viewer() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"},
                {"author": {"login": "alice"}, "createdAt": "2026-03-28T13:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T10:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::ReviewRequired,
        );
    }

    #[test]
    fn classify_new_commit_after_viewer_comment() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-28T14:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::ReviewRequired,
        );
    }

    #[test]
    fn classify_author_comment_before_viewer() {
        let node = make_pr_node(
            r#"{
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "comments": {"nodes": [
                {"author": {"login": "alice"}, "createdAt": "2026-03-28T10:00:00Z"},
                {"author": {"login": "me"}, "createdAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviews": {"nodes": []},
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-27T10:00:00Z"}}]}
        }"#,
        );
        assert_eq!(
            classify_review_decision(&node, "me"),
            ReviewDecision::WaitingForResponse,
        );
    }

    #[test]
    fn parse_review_prs_extracts_ci_status_and_body() {
        let json = r#"{"data":{"viewer":{"login":"me"},"requestedReview":{"nodes":[{
            "number": 77,
            "title": "Fix auth bug",
            "url": "https://github.com/acme/app/pull/77",
            "isDraft": false,
            "createdAt": "2026-03-28T10:00:00Z",
            "updatedAt": "2026-03-29T14:00:00Z",
            "additions": 10,
            "deletions": 2,
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "alice"},
            "repository": {"nameWithOwner": "acme/app"},
            "labels": {"nodes": []},
            "comments": {"nodes": []},
            "reviews": {"nodes": []},
            "body": "This fixes the auth bug",
            "headRefName": "fix-auth-bug",
            "commits": {"nodes": [{"commit": {"committedDate": "2026-03-28T10:00:00Z", "statusCheckRollup": {"state": "SUCCESS"}}}]},
            "reviewRequests": {"nodes": []}
        }]},"alreadyReviewed":{"nodes":[]},"commented":{"nodes":[]}}}"#;
        let prs = parse_review_prs(json).unwrap();
        assert_eq!(prs.len(), 1);
        let pr = &prs[0];
        assert_eq!(pr.ci_status, CiStatus::Success);
        assert_eq!(pr.body, "This fixes the auth bug");
        assert_eq!(pr.head_ref, "fix-auth-bug");
    }

    #[test]
    fn parse_reviewers_from_reviews_and_requests() {
        let node = make_pr_node(
            r#"{
            "reviews": {"nodes": [
                {"state": "APPROVED", "author": {"login": "bob"}, "submittedAt": "2026-03-28T12:00:00Z"}
            ]},
            "reviewRequests": {"nodes": [
                {"requestedReviewer": {"login": "carol"}}
            ]}
        }"#,
        );
        let mut reviewers = parse_reviewers(&node);
        reviewers.sort_by(|a, b| a.login.cmp(&b.login));
        assert_eq!(reviewers.len(), 2);
        assert_eq!(reviewers[0].login, "bob");
        assert_eq!(reviewers[0].decision, Some(ReviewDecision::Approved));
        assert_eq!(reviewers[1].login, "carol");
        assert_eq!(reviewers[1].decision, None);
    }

    #[test]
    fn classify_rerequest_moves_to_review_required() {
        let node = serde_json::json!({
            "reviewDecision": "REVIEW_REQUIRED",
            "author": { "login": "alice" },
            "comments": { "nodes": [
                { "author": { "login": "viewer" }, "createdAt": "2026-01-01T01:00:00Z" }
            ] },
            "reviews": { "nodes": [] },
            "commits": { "nodes": [{ "commit": { "committedDate": "2026-01-01T00:00:00Z" } }] },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "viewer" } }
            ] }
        });
        let decision = classify_review_decision(&node, "viewer");
        assert_eq!(decision, ReviewDecision::ReviewRequired);
    }

    #[test]
    fn classify_no_rerequest_stays_waiting() {
        let node = serde_json::json!({
            "reviewDecision": "REVIEW_REQUIRED",
            "author": { "login": "alice" },
            "comments": { "nodes": [
                { "author": { "login": "viewer" }, "createdAt": "2026-01-01T01:00:00Z" }
            ] },
            "reviews": { "nodes": [] },
            "commits": { "nodes": [{ "commit": { "committedDate": "2026-01-01T00:00:00Z" } }] },
            "reviewRequests": { "nodes": [] }
        });
        let decision = classify_review_decision(&node, "viewer");
        assert_eq!(decision, ReviewDecision::WaitingForResponse);
    }

    #[test]
    fn classify_draft_filtered_in_parse() {
        let json = r#"{"data":{"viewer":{"login":"me"},"requestedReview":{"nodes":[{
            "number": 1, "title": "T", "url": "https://github.com/o/r/pull/1", "isDraft": true,
            "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
            "additions": 0, "deletions": 0,
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "a"}, "repository": {"nameWithOwner": "o/r"},
            "labels": {"nodes": []},
            "comments": {"nodes": []},
            "reviews": {"nodes": []},
            "commits": {"nodes": []}
        }]},"alreadyReviewed":{"nodes":[]},"commented":{"nodes":[]}}}"#;
        let prs = parse_review_prs(json).unwrap();
        assert!(prs.is_empty());
    }

    /// Integration test: calls the real `gh` CLI to verify fetch works end-to-end.
    /// Run with: cargo test fetch_review_prs_real -- --ignored
    #[test]
    #[ignore]
    fn fetch_review_prs_real() {
        let runner = crate::process::RealProcessRunner;
        let result = fetch_review_prs(&runner);
        eprintln!("result: {result:?}");
        assert!(result.is_ok(), "fetch failed: {}", result.unwrap_err());
        let prs = result.unwrap();
        eprintln!("fetched {} PRs", prs.len());
        for pr in &prs {
            eprintln!("  #{} {} [{:?}]", pr.number, pr.title, pr.review_decision);
        }
    }

    // -----------------------------------------------------------------------
    // parse_my_prs / fetch_my_prs tests
    // -----------------------------------------------------------------------

    const MY_PRS_RESPONSE: &str = r#"{
    "data": {
        "viewer": {"login": "me"},
        "myPrs": {
            "nodes": [
                {
                    "number": 101,
                    "title": "My feature PR",
                    "url": "https://github.com/acme/app/pull/101",
                    "isDraft": false,
                    "createdAt": "2026-03-28T10:00:00Z",
                    "updatedAt": "2026-03-29T14:00:00Z",
                    "additions": 50,
                    "deletions": 10,
                    "reviewDecision": "REVIEW_REQUIRED",
                    "author": {"login": "me"},
                    "repository": {"nameWithOwner": "acme/app"},
                    "labels": {"nodes": [{"name": "feature"}]},
                    "comments": {"nodes": []},
                    "reviews": {"nodes": []},
                    "body": "Adds a new feature",
                    "headRefName": "my-feature",
                    "commits": {"nodes": [{"commit": {"committedDate": "2026-03-28T10:00:00Z", "statusCheckRollup": {"state": "SUCCESS"}}}]},
                    "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "alice"}}]}
                }
            ]
        }
    }
}"#;

    #[test]
    fn parse_my_prs_extracts_fields() {
        let prs = parse_my_prs(MY_PRS_RESPONSE).unwrap();
        assert_eq!(prs.len(), 1);
        let pr = &prs[0];
        assert_eq!(pr.number, 101);
        assert_eq!(pr.title, "My feature PR");
        assert_eq!(pr.author, "me");
        assert_eq!(pr.review_decision, ReviewDecision::ReviewRequired);
        assert_eq!(pr.ci_status, CiStatus::Success);
        assert_eq!(pr.reviewers.len(), 1);
        assert_eq!(pr.reviewers[0].login, "alice");
    }

    #[test]
    fn parse_my_prs_filters_drafts() {
        let json = r#"{"data":{"viewer":{"login":"me"},"myPrs":{"nodes":[{
            "number": 1, "title": "T", "url": "https://github.com/o/r/pull/1", "isDraft": true,
            "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
            "additions": 0, "deletions": 0,
            "reviewDecision": "REVIEW_REQUIRED",
            "author": {"login": "me"}, "repository": {"nameWithOwner": "o/r"},
            "labels": {"nodes": []}, "comments": {"nodes": []},
            "reviews": {"nodes": []}, "body": "", "headRefName": "x",
            "commits": {"nodes": []}, "reviewRequests": {"nodes": []}
        }]}}}"#;
        let prs = parse_my_prs(json).unwrap();
        assert!(prs.is_empty());
    }

    #[test]
    fn fetch_my_prs_calls_gh_with_author_query() {
        let runner = MockProcessRunner::new(vec![MockProcessRunner::ok_with_stdout(
            MY_PRS_RESPONSE.as_bytes(),
        )]);
        let prs = fetch_my_prs(&runner).unwrap();
        assert_eq!(prs.len(), 1);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        let query_arg = calls[0].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            query_arg.contains("author:@me"),
            "missing author:@me qualifier"
        );
        assert!(query_arg.contains("-is:draft"));
    }

    #[test]
    fn fetch_bot_prs_query_includes_owner_qualifiers() {
        let identity_response = r#"{"data":{"viewer":{"login":"testuser","organizations":{"nodes":[{"login":"myorg"},{"login":"otherorg"}]}}}}"#;
        let search_response = r#"{"data":{"viewer":{"login":"testuser"},"dependabot":{"nodes":[]},"renovate":{"nodes":[]}}}"#;

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(identity_response.as_bytes()),
            MockProcessRunner::ok_with_stdout(search_response.as_bytes()),
        ]);

        let _ = fetch_bot_prs(&runner);
        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 2);

        // First call: identity query fetches orgs
        let identity_query = calls[0].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            identity_query.contains("organizations"),
            "first call should fetch orgs"
        );

        // Second call: search includes owner qualifiers
        let search_query = calls[1].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            search_query.contains("user:testuser"),
            "missing user: qualifier"
        );
        assert!(
            search_query.contains("org:myorg"),
            "missing org:myorg qualifier"
        );
        assert!(
            search_query.contains("org:otherorg"),
            "missing org:otherorg qualifier"
        );
        assert!(search_query.contains("author:app/dependabot"));
        assert!(search_query.contains("author:app/renovate"));
    }

    #[test]
    fn fetch_bot_prs_no_orgs_only_user_qualifier() {
        let identity_response =
            r#"{"data":{"viewer":{"login":"solo","organizations":{"nodes":[]}}}}"#;
        let search_response = r#"{"data":{"viewer":{"login":"solo"},"dependabot":{"nodes":[]},"renovate":{"nodes":[]}}}"#;

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(identity_response.as_bytes()),
            MockProcessRunner::ok_with_stdout(search_response.as_bytes()),
        ]);

        let _ = fetch_bot_prs(&runner);
        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 2);

        let search_query = calls[1].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            search_query.contains("user:solo"),
            "missing user: qualifier"
        );
        assert!(!search_query.contains("org:"), "should have no org: qualifiers");
    }

    // --- Security alert parsing tests ---

    const GRAPHQL_ALERTS_RESPONSE: &str = r#"{
        "data": {
            "viewer": {
                "repositories": {
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": [
                        {
                            "nameWithOwner": "acme/app",
                            "vulnerabilityAlerts": {
                                "nodes": [
                                    {
                                        "number": 1,
                                        "createdAt": "2026-03-01T10:00:00Z",
                                        "securityVulnerability": {
                                            "severity": "CRITICAL",
                                            "package": {"name": "lodash"},
                                            "vulnerableVersionRange": "< 4.17.21",
                                            "firstPatchedVersion": {"identifier": "4.17.21"}
                                        },
                                        "securityAdvisory": {
                                            "summary": "Prototype Pollution in lodash",
                                            "cvss": {"score": 9.8},
                                            "description": "A prototype pollution vuln."
                                        }
                                    },
                                    {
                                        "number": 5,
                                        "createdAt": "2026-03-05T10:00:00Z",
                                        "securityVulnerability": {
                                            "severity": "MODERATE",
                                            "package": {"name": "express"},
                                            "vulnerableVersionRange": "< 5.0.0",
                                            "firstPatchedVersion": null
                                        },
                                        "securityAdvisory": {
                                            "summary": "Open redirect in express",
                                            "cvss": {"score": 5.3},
                                            "description": "An open redirect."
                                        }
                                    }
                                ]
                            }
                        },
                        {
                            "nameWithOwner": "acme/lib",
                            "vulnerabilityAlerts": {
                                "nodes": []
                            }
                        }
                    ]
                }
            }
        }
    }"#;

    #[test]
    fn parse_graphql_security_alerts_basic() {
        let alerts = parse_graphql_security_alerts(GRAPHQL_ALERTS_RESPONSE).unwrap();
        assert_eq!(alerts.len(), 2);

        assert_eq!(alerts[0].number, 1);
        assert_eq!(alerts[0].repo, "acme/app");
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
        assert_eq!(alerts[0].kind, AlertKind::Dependabot);
        assert_eq!(alerts[0].package.as_deref(), Some("lodash"));
        assert_eq!(alerts[0].vulnerable_range.as_deref(), Some("< 4.17.21"));
        assert_eq!(alerts[0].fixed_version.as_deref(), Some("4.17.21"));
        assert_eq!(alerts[0].cvss_score, Some(9.8));
        assert_eq!(
            alerts[0].url,
            "https://github.com/acme/app/security/dependabot/1"
        );
        assert_eq!(alerts[0].title, "Prototype Pollution in lodash");

        assert_eq!(alerts[1].number, 5);
        assert_eq!(alerts[1].repo, "acme/app");
        assert_eq!(alerts[1].severity, AlertSeverity::Medium);
        assert_eq!(alerts[1].fixed_version, None);
    }

    #[test]
    fn parse_graphql_security_alerts_empty_repos() {
        let json = r#"{"data":{"viewer":{"repositories":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
        let alerts = parse_graphql_security_alerts(json).unwrap();
        assert!(alerts.is_empty());
    }

    #[test]
    fn parse_graphql_security_alerts_invalid_json() {
        let result = parse_graphql_security_alerts("not json");
        assert!(result.is_err());
    }

    #[test]
    fn fetch_security_alerts_uses_graphql() {
        let runner = MockProcessRunner::new(vec![MockProcessRunner::ok_with_stdout(
            GRAPHQL_ALERTS_RESPONSE.as_bytes(),
        )]);
        let alerts = fetch_security_alerts(&runner).unwrap();
        assert_eq!(alerts.len(), 2);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gh");
        assert!(
            calls[0].1.contains(&"graphql".to_string()),
            "should use graphql API"
        );
        let query_arg = calls[0].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            query_arg.contains("vulnerabilityAlerts"),
            "query should include vulnerabilityAlerts"
        );

        // Results should be sorted by severity (critical first)
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
        assert_eq!(alerts[1].severity, AlertSeverity::Medium);
    }

    // -----------------------------------------------------------------------
    // Review PR pagination tests
    // -----------------------------------------------------------------------

    /// Helper: build a review PR search response page with pageInfo per alias.
    /// `aliases` is a list of (name, nodes_json, has_next, end_cursor).
    fn make_review_page(
        aliases: &[(&str, &str, bool, Option<&str>)],
    ) -> String {
        let mut parts = Vec::new();
        for (name, nodes, has_next, cursor) in aliases {
            let cursor_json = match cursor {
                Some(c) => format!("\"{c}\""),
                None => "null".to_string(),
            };
            parts.push(format!(
                r#""{name}": {{
                    "pageInfo": {{"hasNextPage": {has_next}, "endCursor": {cursor_json}}},
                    "nodes": [{nodes}]
                }}"#
            ));
        }
        format!(
            r#"{{"data": {{"viewer": {{"login": "me"}}, {}}}}}"#,
            parts.join(", ")
        )
    }

    /// Minimal PR node JSON for pagination tests.
    fn pr_node_json(number: i64, title: &str, url: &str) -> String {
        format!(
            r#"{{
                "number": {number}, "title": "{title}",
                "url": "{url}", "isDraft": false,
                "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
                "additions": 1, "deletions": 0,
                "reviewDecision": "REVIEW_REQUIRED",
                "author": {{"login": "alice"}},
                "repository": {{"nameWithOwner": "acme/app"}},
                "labels": {{"nodes": []}},
                "comments": {{"nodes": []}},
                "reviews": {{"nodes": []}},
                "commits": {{"nodes": []}}
            }}"#
        )
    }

    #[test]
    fn fetch_review_prs_paginates_across_multiple_pages() {
        let node1 = pr_node_json(1, "PR one", "https://github.com/acme/app/pull/1");
        let node2 = pr_node_json(2, "PR two", "https://github.com/acme/app/pull/2");

        let page1 = make_review_page(&[
            ("requestedReview", &node1, true, Some("cursor1")),
            ("alreadyReviewed", "", false, None),
            ("commented", "", false, None),
        ]);
        let page2 = make_review_page(&[
            ("requestedReview", &node2, false, None),
        ]);

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
            MockProcessRunner::ok_with_stdout(page2.as_bytes()),
        ]);

        let prs = fetch_review_prs(&runner).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 1);
        assert_eq!(prs[1].number, 2);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 2, "should make 2 requests");

        // Second request should include cursor and only the active alias
        let query2 = calls[1].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(query2.contains("cursor1"), "second page should use cursor");
        assert!(
            query2.contains("requestedReview"),
            "should include requestedReview alias"
        );
        assert!(
            !query2.contains("alreadyReviewed"),
            "exhausted alias should be excluded"
        );
    }

    #[test]
    fn fetch_review_prs_stops_when_all_aliases_exhausted() {
        let node1 = pr_node_json(1, "PR one", "https://github.com/acme/app/pull/1");

        let page1 = make_review_page(&[
            ("requestedReview", &node1, false, None),
            ("alreadyReviewed", "", false, None),
            ("commented", "", false, None),
        ]);

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
        ]);

        let prs = fetch_review_prs(&runner).unwrap();
        assert_eq!(prs.len(), 1);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1, "should stop after 1 request when no hasNextPage");
    }

    #[test]
    fn fetch_review_prs_deduplicates_across_pages() {
        // Same PR appears in requestedReview page 1 and commented page 2
        let node = pr_node_json(42, "Shared PR", "https://github.com/acme/app/pull/42");

        let page1 = make_review_page(&[
            ("requestedReview", &node, false, None),
            ("alreadyReviewed", "", false, None),
            ("commented", "", true, Some("c_cursor")),
        ]);
        let page2 = make_review_page(&[
            ("commented", &node, false, None),
        ]);

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
            MockProcessRunner::ok_with_stdout(page2.as_bytes()),
        ]);

        let prs = fetch_review_prs(&runner).unwrap();
        assert_eq!(prs.len(), 1, "duplicate across pages should be deduplicated");
        assert_eq!(prs[0].number, 42);
    }

    #[test]
    fn fetch_review_prs_uses_page_size_25() {
        let page1 = make_review_page(&[
            ("requestedReview", "", false, None),
            ("alreadyReviewed", "", false, None),
            ("commented", "", false, None),
        ]);

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
        ]);

        let _ = fetch_review_prs(&runner);
        let calls = runner.recorded_calls();
        let query = calls[0].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            query.contains("first: 25"),
            "should use page size 25, got: {query}"
        );
    }

    #[test]
    fn fetch_my_prs_paginates() {
        let node1 = pr_node_json(10, "My PR 1", "https://github.com/acme/app/pull/10");
        let node2 = pr_node_json(11, "My PR 2", "https://github.com/acme/app/pull/11");

        let page1 = make_review_page(&[
            ("myPrs", &node1, true, Some("my_cursor")),
        ]);
        let page2 = make_review_page(&[
            ("myPrs", &node2, false, None),
        ]);

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
            MockProcessRunner::ok_with_stdout(page2.as_bytes()),
        ]);

        let prs = fetch_my_prs(&runner).unwrap();
        assert_eq!(prs.len(), 2);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 2);
        let query2 = calls[1].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(query2.contains("my_cursor"), "second page should use cursor");
    }

    #[test]
    fn fetch_security_alerts_paginates() {
        let page1 = r#"{"data":{"viewer":{"repositories":{"pageInfo":{"hasNextPage":true,"endCursor":"abc123"},"nodes":[{"nameWithOwner":"acme/app","vulnerabilityAlerts":{"nodes":[{"number":1,"createdAt":"2026-03-01T10:00:00Z","securityVulnerability":{"severity":"HIGH","package":{"name":"pkg1"},"vulnerableVersionRange":"< 1.0","firstPatchedVersion":{"identifier":"1.0"}},"securityAdvisory":{"summary":"Vuln 1","cvss":{"score":7.5},"description":"desc1"}}]}}]}}}}"#;
        let page2 = r#"{"data":{"viewer":{"repositories":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"nameWithOwner":"acme/lib","vulnerabilityAlerts":{"nodes":[{"number":2,"createdAt":"2026-03-02T10:00:00Z","securityVulnerability":{"severity":"LOW","package":{"name":"pkg2"},"vulnerableVersionRange":"< 2.0","firstPatchedVersion":{"identifier":"2.0"}},"securityAdvisory":{"summary":"Vuln 2","cvss":{"score":3.1},"description":"desc2"}}]}}]}}}}"#;

        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(page1.as_bytes()),
            MockProcessRunner::ok_with_stdout(page2.as_bytes()),
        ]);
        let alerts = fetch_security_alerts(&runner).unwrap();
        assert_eq!(alerts.len(), 2);

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 2, "should make 2 requests for pagination");

        // Second query should include the cursor
        let query_arg = calls[1].1.iter().find(|a| a.contains("query=")).unwrap();
        assert!(
            query_arg.contains("abc123"),
            "second page should use cursor from first page"
        );
    }
}
