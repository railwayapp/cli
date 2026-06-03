# Proposal: stop classifying human terminal emulators as `agent_unknown`

**Status:** Draft / proposal for the CLI team · **Author:** cody@railway.com (via Claude Code)
**Related:** [Agentic Loop Telemetry RFC](https://www.notion.so/3500e4c54563809bb7a0f9fce8e5efed), `dbt-analytics` agent-adoption dashboard

## Problem

The CLI caller-detection logic classifies a meaningful slice of **interactive human
terminal sessions** as `caller_class='agent_unknown'` (an "automated subprocess we
couldn't fingerprint"), with the terminal emulator as the `caller_subkind`. This
inflates the "agent" cohort with humans and pollutes every downstream agentic metric.

### Evidence (from `railway-infra.analytics.fct_agentic_events`, 2026-05-18 → 06-01)

Distinct users by `agent_unknown` subkind:

| subkind | users | interactive human terminal? |
|---|---|---|
| `(null)` | 6,752 | mixed |
| `vscode` | 2,418 | ambiguous (IDE terminal vs agent subprocess) |
| `node` | 1,513 | ambiguous |
| **`apple_terminal`** | **1,226** | **yes — human terminal** |
| `cursor` | 766 | ambiguous |
| `shell` | 625 | ambiguous |
| **`ghostty`** | **190** | **yes — human terminal** |
| **`iterm`** | **165** | **yes — human terminal** |
| **`warp`** | **120** | **yes — human terminal** |
| `python` | 100 | likely script |
| `jetbrains` / `zed` | 82 | ambiguous |

`apple_terminal`, `iterm`, `ghostty`, `warp` are **interactive human terminal
emulators** — a user typing `railway ...` by hand — yet they are bucketed as
`agent_unknown`. That's ~1,700 clearly-human users misclassified as agents in a
2-week window, plus several thousand ambiguous IDE-hosted rows.

## Impact

- Agent-vs-human cohort counts are wrong: "agent users" is overstated and the
  agent/human crossover margin is narrower than reported.
- `agent_unknown` deploys at a much lower rate (~29%) than named agents (~60%),
  dragging aggregate agent deploy/quality metrics downward.
- Any analytics keyed on `caller_class IN ('agent_named','agent_unknown',...)`
  inherits the impurity.

## Proposal

In the caller-detection path (`src/exec_context.rs` / the telemetry detector that
emits `caller`), treat **recognized interactive terminal emulators as `tty`
(human)**, not `agent_unknown`, **unless a strong agent signal is present**
(explicit `RAILWAY_CALLER`/`RAILWAY_AGENT_SESSION`, a known agent in the process
ancestry, or a non-interactive stdin with an agent fingerprint).

Concretely, review the heuristic that currently lands `apple_terminal` / `iterm` /
`ghostty` / `warp` (and similar) hosts in `agent_unknown`: an attached TTY in a
known terminal emulator with no agent ancestry should classify as `human`.

Open questions for the CLI team:
- Where exactly in the detector does the terminal-emulator host get the
  `agent_unknown` class instead of `tty`? (process-tree vs TTY-presence ordering)
- Should ambiguous IDE hosts (`vscode`/`cursor`/`jetbrains`/`zed`) be split into
  "human-in-IDE-terminal" vs "agent subprocess" with a clearer signal?

## Not in scope here

This proposal does not include the code change — it needs the detector's domain
owners + telemetry tests (synthetic traces through each terminal/agent path). It
documents the data evidence and the requested behavior so the CLI team can scope
the fix.
