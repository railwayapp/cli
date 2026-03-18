use rmcp::{ErrorData as McpError, model::*};

use super::super::handler::RailwayMcp;
use super::super::params::{DocsFetchParams, DocsSearchParams};

impl RailwayMcp {
    pub(crate) async fn do_docs_search(
        &self,
        params: DocsSearchParams,
    ) -> Result<CallToolResult, McpError> {
        let http = reqwest::Client::new();

        let resp = http
            .get("https://docs.railway.com/sitemap-0.xml")
            .header("User-Agent", "railway-cli")
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to fetch docs sitemap: {e}"), None)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Docs search is temporarily unavailable (status {status})."
            ))]));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to read sitemap: {e}"), None))?;

        let doc_paths: Vec<String> = body
            .split("<loc>")
            .skip(1)
            .filter_map(|s| {
                let url = s.split("</loc>").next()?;
                let path = url.strip_prefix("https://docs.railway.com/")?;
                if path.is_empty() {
                    None
                } else {
                    Some(path.to_string())
                }
            })
            .collect();

        let query_lower = params.query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(&str, f64)> = doc_paths
            .iter()
            .filter_map(|path| {
                let path_lower = path.to_lowercase();
                let segments: Vec<&str> = path_lower.split('/').collect();
                let mut score: f64 = 0.0;

                for word in &query_words {
                    // Exact segment match (e.g. "cli" matches the "cli" segment)
                    if segments.iter().any(|seg| seg == word) {
                        score += 2.0;
                    // Substring match (e.g. "deploy" matches "deployments")
                    } else if path_lower.contains(*word) {
                        score += 1.0;
                    }
                }

                // Bonus for shorter paths (prefer "cli" over "cli/add")
                if score > 0.0 {
                    score += 0.1 / segments.len() as f64;
                    Some((path.as_str(), score))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if scored.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No documentation found for '{}'. Try a different search term.",
                params.query
            ))]));
        }

        let results: Vec<String> = scored
            .iter()
            .take(5)
            .map(|(path, _)| format!("- https://docs.railway.com/{path}"))
            .collect();

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Found {} result(s) for '{}':\n{}\n\nUse docs_fetch with a URL to read the full page.",
            results.len(),
            params.query,
            results.join("\n")
        ))]))
    }

    pub(crate) async fn do_docs_fetch(
        &self,
        params: DocsFetchParams,
    ) -> Result<CallToolResult, McpError> {
        let slug = extract_slug(&params.url);

        let mut slug = slug.trim_matches('/').to_string();
        while slug.contains("..") {
            slug = slug.replace("..", "");
        }

        if slug.is_empty() {
            return Err(McpError::invalid_params(
                "Invalid documentation URL: no path found.",
                None,
            ));
        }

        let base = "https://raw.githubusercontent.com/railwayapp/docs/refs/heads/main/content";
        let url = if slug.starts_with("guides/") {
            format!("{base}/{slug}.md")
        } else {
            format!("{base}/docs/{slug}.md")
        };

        let http = reqwest::Client::new();
        let resp = http
            .get(&url)
            .header("User-Agent", "railway-cli")
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to fetch doc: {e}"), None))?;

        if !resp.status().is_success() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Documentation page '{slug}' not found. Try docs_search to find the right page."
            ))]));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to read response: {e}"), None))?;

        const MAX_BYTES: usize = 8 * 1024;
        if text.len() > MAX_BYTES {
            let mut end = MAX_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            Ok(CallToolResult::success(vec![Content::text(format!(
                "{}\n\n[Content truncated at 8KB]",
                &text[..end]
            ))]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(text)]))
        }
    }
}

/// Extract slug from a docs URL or treat as a raw slug.
fn extract_slug(input: &str) -> &str {
    input
        .strip_prefix("https://docs.railway.com/")
        .or_else(|| input.strip_prefix("http://docs.railway.com/"))
        .unwrap_or(input)
}
