use rmcp::{ErrorData as McpError, model::*};

use super::super::handler::RailwayMcp;
use super::super::params::DocsSearchParams;

impl RailwayMcp {
    pub(crate) async fn do_docs_search(
        &self,
        params: DocsSearchParams,
    ) -> Result<CallToolResult, McpError> {
        // Fetch the repo file tree to find matching docs
        let resp = self
            .client
            .get("https://api.github.com/repos/railwayapp/docs/git/trees/main?recursive=1")
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "railway-cli")
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to fetch doc tree: {e}"), None)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "GitHub API returned status {status}. Docs search is temporarily unavailable."
            ))]));
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Failed to parse tree response: {e}"), None)
        })?;

        let empty_vec = vec![];
        let tree = data["tree"].as_array().unwrap_or(&empty_vec);

        // Collect doc file paths (src/docs/**/*.md)
        let doc_paths: Vec<&str> = tree
            .iter()
            .filter_map(|entry| {
                let path = entry["path"].as_str()?;
                if path.starts_with("src/docs/") && path.ends_with(".md") {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        // Score paths against the query using keyword matching
        let query_lower = params.query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(&str, usize)> = doc_paths
            .iter()
            .filter_map(|path| {
                let path_lower = path.to_lowercase();
                let score = query_words
                    .iter()
                    .filter(|word| path_lower.contains(*word))
                    .count();
                if score > 0 {
                    Some((*path, score))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));

        let slug = scored.first().and_then(|(path, _)| {
            path.strip_prefix("src/docs/")
                .and_then(|p| p.strip_suffix(".md"))
        });

        let slug = match slug {
            Some(s) => s.to_string(),
            None => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "No documentation found for '{}'. Try a different search term.",
                    params.query
                ))]));
            }
        };

        let content = self.fetch_doc_content(&slug).await?;
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    async fn fetch_doc_content(&self, slug: &str) -> Result<String, McpError> {
        let mut slug = slug.trim_matches('/').to_string();
        while slug.contains("..") {
            slug = slug.replace("..", "");
        }

        let base = "https://raw.githubusercontent.com/railwayapp/docs/refs/heads/main";
        let url = format!("{base}/src/docs/{slug}.md");

        let resp = self
            .client
            .get(&url)
            .header("User-Agent", "railway-cli")
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to fetch doc: {e}"), None))?;

        if !resp.status().is_success() {
            return Ok(format!(
                "Documentation page '{slug}' not found. Try a different search term."
            ));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to read response: {e}"), None))?;

        const MAX_BYTES: usize = 8 * 1024;
        if text.len() > MAX_BYTES {
            // Truncate on a valid UTF-8 char boundary
            let mut end = MAX_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            Ok(format!("{}\n\n[Content truncated at 8KB]", &text[..end]))
        } else {
            Ok(text)
        }
    }
}
