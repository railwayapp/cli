# Railway CLI Template Search Plan

Railway now has a public `templateSearch` GraphQL query backed by the unified Meilisearch template index. The CLI should call this API directly and preserve the backend-ranked result order. It should not fetch `templates(first: 200)` and filter or rerank locally.

## Proposed CLI

```text
Command:
  railway templates search [query]

Arguments:
  query                         Search term. Seeds the picker in TTY mode.

Flags:
  --json                        Print the GraphQL response shape as JSON.
  --limit <n>                   Number of results to request via `first`. Defaults to 20.
  --after <cursor>              Fetch the next page using `pageInfo.endCursor`.
  --category <category>         Filter by template category.
  --verified <true|false>       Filter by verification state.
```

## Behavior

- `railway templates search` in a TTY opens a command-palette style template picker.
- `railway templates search postgres` in a TTY opens the same picker with `postgres` prefilled.
- Empty input is valid and means browse/top-template results, matching `templateSearch(query: "")`.
- The picker should visually resemble the Railway command palette: a bordered panel, a prominent search input at the top, and a scrollable list of template rows below.
- The picker calls `templateSearch` as the user types, preserves backend order, and loads more results with the API cursor.
- Each row should show the template image/icon when available, name, description, deploy count, health, creator, and verification state.
- The selected row should have a clear highlighted background while non-selected rows stay quiet and scannable.
- Keyboard behavior should match command-palette expectations: type to search, up/down to move, enter to select, esc to cancel, and load more when the cursor approaches the end of the list.
- Selecting a template should exit the picker and print a concise read-only result: template name, code, description, and suggested next command such as `railway deploy --template <code>`.
- Selection should not deploy, create, link, or otherwise mutate anything. This command is discovery only.
- Outside a TTY, the command never opens the picker. It prints results directly, or prints the GraphQL response shape when `--json` is passed.
- `--json` should match the regular GraphQL API response shape rather than introduce a custom wrapper.
- In non-TTY text output, if more results are available, print the next cursor and the command to fetch the next page.
- The CLI should use `GQLClient::new_public()` so template search works without login.

## API Mapping

- `query` maps to `templateSearch(query:)`.
- `--limit` maps to `first`.
- `--after` maps to `after`.
- `--category` maps to `category`.
- `--verified` maps to `verified`.
- JSON output should preserve the API connection structure:
  - `edges[].cursor`
  - `edges[].node`
  - `pageInfo.hasNextPage`
  - `pageInfo.endCursor`

## Implementation Notes

1. Regenerate `src/gql/schema.json`.
2. Add `src/gql/queries/strings/TemplateSearch.graphql`.
3. Add `TemplateSearch` to `src/gql/queries/mod.rs`.
4. Build the TTY picker as the default human experience using ratatui, including loading, empty, error, selected, and pagination states.
5. Update the MCP `search_templates` tool to use `templateSearch` instead of local filtering.
