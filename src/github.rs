use crate::models::{ReviewDecision, ReviewPr};
use crate::process::ProcessRunner;
use chrono::{DateTime, Utc};

/// Default authors to exclude (dependency bots).
pub const DEFAULT_EXCLUDED_AUTHORS: &[&str] = &[
    "dependabot[bot]",
    "scala-steward",
    "renovate[bot]",
];

/// Parse the JSON response from `gh api graphql` into a list of ReviewPr.
fn parse_review_prs(json: &str) -> Result<Vec<ReviewPr>, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse JSON: {e}"))?;

    let nodes = root
        .pointer("/data/search/nodes")
        .and_then(|v| v.as_array())
        .ok_or("Missing data.search.nodes in response")?;

    let mut prs = Vec::with_capacity(nodes.len());
    for node in nodes {
        let review_decision_str = node["reviewDecision"].as_str().unwrap_or("REVIEW_REQUIRED");
        let review_decision =
            ReviewDecision::parse(review_decision_str).unwrap_or(ReviewDecision::ReviewRequired);

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
        });
    }

    Ok(prs)
}

/// Filter out PRs authored by excluded bot accounts.
fn filter_bots(prs: Vec<ReviewPr>, excluded: &[&str]) -> Vec<ReviewPr> {
    prs.into_iter()
        .filter(|pr| !excluded.iter().any(|bot| pr.author == *bot))
        .collect()
}

/// Fetch open PRs where the current user is a requested reviewer.
/// Uses `gh api graphql` via the provided ProcessRunner.
pub fn fetch_review_prs(
    runner: &dyn ProcessRunner,
    excluded_authors: &[&str],
) -> Result<Vec<ReviewPr>, String> {
    let query = r#"{
  search(query: "is:pr is:open review-requested:@me", type: ISSUE, first: 100) {
    nodes {
      ... on PullRequest {
        number
        title
        url
        isDraft
        createdAt
        updatedAt
        additions
        deletions
        reviewDecision
        author { login }
        repository { nameWithOwner }
        labels(first: 10) { nodes { name } }
      }
    }
  }
}"#;

    let output = runner
        .run("gh", &["api", "graphql", "-f", &format!("query={query}")])
        .map_err(|e| format!("Failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh api graphql failed: {stderr}"));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    let prs = parse_review_prs(&json)?;
    Ok(filter_bots(prs, excluded_authors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::MockProcessRunner;

    const SAMPLE_RESPONSE: &str = r#"{
        "data": {
            "search": {
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
                        "labels": {"nodes": [{"name": "bug"}, {"name": "urgent"}]}
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
                        "labels": {"nodes": []}
                    },
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
                        "labels": {"nodes": [{"name": "refactor"}]}
                    }
                ]
            }
        }
    }"#;

    #[test]
    fn parse_review_prs_extracts_all_fields() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
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
    fn parse_review_prs_handles_draft_and_approved() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
        assert!(prs[1].is_draft);
        assert_eq!(prs[2].review_decision, ReviewDecision::Approved);
    }

    #[test]
    fn parse_review_prs_empty_nodes() {
        let json = r#"{"data":{"search":{"nodes":[]}}}"#;
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
        let json = r#"{"data":{"search":{"nodes":[{
            "number": 1, "title": "T", "url": "u", "isDraft": false,
            "createdAt": "2026-03-28T10:00:00Z", "updatedAt": "2026-03-28T10:00:00Z",
            "additions": 0, "deletions": 0,
            "reviewDecision": null,
            "author": {"login": "a"}, "repository": {"nameWithOwner": "o/r"},
            "labels": {"nodes": []}
        }]}}}"#;
        let prs = parse_review_prs(json).unwrap();
        assert_eq!(prs[0].review_decision, ReviewDecision::ReviewRequired);
    }

    #[test]
    fn filter_bots_removes_excluded_authors() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
        let filtered = filter_bots(prs, DEFAULT_EXCLUDED_AUTHORS);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].author, "alice");
        assert_eq!(filtered[1].author, "bob");
    }

    #[test]
    fn filter_bots_empty_exclude_list_keeps_all() {
        let prs = parse_review_prs(SAMPLE_RESPONSE).unwrap();
        let filtered = filter_bots(prs, &[]);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn fetch_review_prs_calls_gh_and_parses() {
        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::ok_with_stdout(SAMPLE_RESPONSE.as_bytes()),
        ]);
        let prs = fetch_review_prs(&runner, DEFAULT_EXCLUDED_AUTHORS).unwrap();
        assert_eq!(prs.len(), 2); // bot filtered out

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gh");
        assert!(calls[0].1.contains(&"graphql".to_string()));
    }

    #[test]
    fn fetch_review_prs_gh_failure() {
        let runner = MockProcessRunner::new(vec![
            MockProcessRunner::fail("gh: not authenticated"),
        ]);
        let result = fetch_review_prs(&runner, DEFAULT_EXCLUDED_AUTHORS);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not authenticated"));
    }
}
