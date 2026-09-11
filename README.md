# Railway CLI

The Railway CLI lets you interact with your Railway projects from the command line. Read the [CLI documentation](https://docs.railway.com/cli).

## Installation

Install the Railway CLI with agent support configured in one step (macOS, Linux, Windows via [WSL](https://learn.microsoft.com/en-us/windows/wsl/install)):

```bash
bash <(curl -fsSL railway.com/install.sh) --agents -y
```

This installs the CLI to `~/.railway/bin` and runs [`railway setup agent`](https://docs.railway.com/cli/setup) to configure detected agent tools.

To install the CLI without agent configuration:

```bash
bash <(curl -fsSL railway.com/install.sh) -y
```

Uninstall the CLI:

```bash
bash <(curl -fsSL cli.new) -r
```

Other installation methods are available in the CLI documentation: [Homebrew](https://docs.railway.com/cli#homebrew-macos), [npm](https://docs.railway.com/cli#npm-macos-linux-windows), [Scoop](https://docs.railway.com/cli#scoop-windows), [pre-built binaries](https://docs.railway.com/cli#pre-built-binaries), and [source builds](https://docs.railway.com/cli#from-source).

## Authentication

Before using the CLI, authenticate with your Railway account:

```bash
railway login
```

For environments without a browser, such as SSH sessions, use browserless login:

```bash
railway login --browserless
```

### Tokens

For CI/CD pipelines, set environment variables instead of using interactive login:

- Project token: Set `RAILWAY_TOKEN` for project-level actions.
- Account or workspace token: Set `RAILWAY_API_TOKEN` for account-level or workspace-level actions.

```bash
RAILWAY_TOKEN=xxx railway up
```

See [Tokens](https://docs.railway.com/integrations/api#creating-a-token) for more information.

## Agent Setup

Configure Railway agent support for AI coding tools:

```bash
railway setup agent -y
```

This installs Railway skills and configures the remote Railway MCP server (`mcp.railway.com` via `railway mcp proxy`, authenticated with your CLI login) for detected tools such as Claude Code, Cursor, Codex, OpenCode, GitHub Copilot, and Factory Droid.

Use the focused commands when you only need one part of the setup:

```bash
railway mcp install --agent cursor
railway skills --agent claude-code
```

With automatic updates enabled, CLI-managed skills update silently in the background,
including on the first normal command after a CLI version change (whether upgraded
with `railway upgrade` or a package manager). Locally modified or deleted skills are
skipped, and user-added files are preserved. Restart your coding tool to load updated
skills. Use `railway skills update` to review skipped updates, or add `--force` to
overwrite local changes explicitly.

`railway autoupdate disable` (or `RAILWAY_NO_AUTO_UPDATE=1`) disables automatic CLI
and skill updates and their update notices. Explicit `railway upgrade`,
`railway check-updates`, and `railway skills update` commands remain available.

Opt into the local GraphQL-backed MCP server instead:

```bash
railway setup agent -y --local
# or
railway mcp install --local --agent cursor
```

## Update progress and status

`railway upgrade` presents CLI installation and managed skill synchronization as
one flow. Stages advance as work completes, and the final summary says whether
the new version is active or will be used on the next command. This explicit
command also synchronizes managed skills when automatic updates are disabled.

Automatic discovery and downloads run quietly. After the new CLI is active and
its skill sync has finished, an interactive command shows one completion receipt
per CLI version. Preserved local edits are reported neutrally; a skill-sync
failure is reported separately from a successful CLI install. Commands using
JSON, piped output, help, and version output do not show or consume receipts.
Install methods that require manual upgrades get one availability notice per
release instead.

Run `railway autoupdate status` to inspect the running and recorded installed
versions, staged/in-progress CLI updates, skill-sync results, and unmanaged
skills. Use `railway skills update` for detailed skill results or to retry a sync.

## Cloud agent launch defaults

Each `railway code` invocation with `--codex`, `--opencode`, `--opencode2`,
`--claude`, `--grok`, or `--railway` creates a fresh VM. This also applies to
`remote`, Codex `desktop-only`, and harness arguments after `--`; `--new` is
accepted but optional. Use `connect [agent]` for an existing backend,
`--agent <name-or-id>` for explicit server setup, or `railway ca ssh <agent>`
for a shell. Bare launches and shared CA/Desktop provisioning retain their
existing targeting behavior.

## Coding backend and app endpoints

The `code-*` endpoint is optional and configured only at VM creation.
Ordinary cloud agents do not request it by default.
Managed Codex/OpenCode client setup requests it automatically when creating a VM:

```bash
railway code --codex --new
railway code --opencode --new
railway code --opencode2 --new

# Explicitly request the endpoint on a plain cloud agent:
railway ca create my-box --code-endpoint

# Choose a custom port instead of the default 4096:
railway code --opencode2 --new --code-port 5000
railway ca create my-box --code-port 5000
```

OpenCode Desktop setup also requests it. SSH-only sessions do not need the code
endpoint. The API uses the optional `CloudAgentCreateInput.codeEndpoint` object:
`{}` selects port 4096, `{ "port": 5000 }` selects a custom port, and omission
disables it. Ports must be 1024-65535, excluding the app port 8080 and gateway
port 8790. The API injects `RAILWAY_CODE_PORT` from that configuration, and
FactoryVM injects `RAILWAY_PUBLIC_DOMAIN_<port>`. Caller and bootstrap variables
cannot override `RAILWAY_CODE_PORT` or provision a route by setting it.

Cloud agents with a `code-*` domain expose the configured port for a managed Codex or
OpenCode server. Both launchers use this endpoint, leaving the `app-*` domain
and port 8080 available for your application. One managed backend can occupy
the code port at a time; setup reports a conflict if another process uses it.
OpenCode2's server URL also serves its web UI with the printed credentials.

Agents without a code endpoint continue using port 8080 for managed backends.
This is creation-time configuration: setting an environment variable inside an existing VM
or reconnecting does not attach a domain. Sleep/wake preserves the chosen routes.
Running managed servers keep their current endpoint. On restart, a server uses
the VM's explicit code-port configuration; older VMs without it retain their
saved port. A configured code endpoint never falls back to the app domain if its
domain is unavailable.

### Add or change the endpoint using a checkpoint

Capture the source with the existing `cloudAgentCheckpointCreate` mutation and
poll `cloudAgentCheckpoint` until its status is `SUCCEEDED`. Then create a new VM:

```bash
railway ca create restored-box --from-checkpoint <checkpoint-id> --code-endpoint
# Or select a custom port:
railway ca create restored-box --from-checkpoint <checkpoint-id> --code-port 5000
railway code --opencode2 --agent restored-box
```

The equivalent API request is:

```graphql
mutation {
  cloudAgentCreate(input: {
    environmentId: "<same-environment-id>"
    name: "restored-box"
    cloudAgentCheckpointId: "<checkpoint-id>"
    codeEndpoint: { port: 5000 }
  }) {
    id
    name
    domains { prefix port domain }
  }
}
```

The disk is restored into a new VM with its own identity and domains. Processes
do not carry over. Saved Codex/OpenCode credentials and project directories are
reused, while the launchers adopt the new VM's configured endpoint. Source VM
variables are not copied automatically; pass needed variables or use a bootstrap.
Checkpoint and bootstrap creates opt in explicitly. The separate fork operation
preserves the source VM's code endpoint and port.

## OpenCode clients and remote servers

Prepare an authenticated server on a cloud agent and open your local client:

```bash
railway code --opencode
railway code --opencode2
railway code --opencode2 --name my-box
```

When the matching OpenCode Desktop edition has an existing settings file or
desktop database, Railway automatically saves the server URL, credentials,
default server, and project in it. The final output confirms that configuration
was updated; open `Railway: <agent-name>` in Desktop's server picker. Both JSON
settings and SQLite renderer state are supported, with backups of previous settings.
You may need to restart OpenCode Desktop to load the updated configuration.
Configuration failures are non-fatal and reported alongside the connection
details; rerun the command to retry.

The CLI also prints the server URL, username, password, and project directory
for manual setup, plus a Railway reconnect command:
`railway code --opencode connect <agent>` (or `--opencode2` for Beta).
After successful setup in an interactive terminal, it clears the setup messages
and shows the connection details, with the Desktop update confirmation inside
the result panel. Failed setup keeps its diagnostic output visible.
The matching local terminal client launches automatically inside the Railway CA
frame. If that client is missing, Railway offers to install it first. Declining,
Esc, or Ctrl+C at the installation prompt leaves the server running and prints
the connection details. Installations initiated inside the frame run quietly.
Standard and Beta clients are detected and installed separately.

New OpenCode agents are named `oc-railg-3ed` (standard) or `oc2-railg-3ed`
(Beta); Codex uses `codex-railg-3ed`: the first five letters/digits of the project
name, lowercase, plus a random three-character suffix. When using your default
cloud agents project, the label comes from the local repository or directory
instead. Existing names are checked before creation; `--name` overrides the
generated name. The same naming applies to `remote` and `railway ca desktop`.

Reconnect to an existing server using your local client:

```bash
railway code --opencode connect
railway code --opencode2 connect my-box
```

`connect` discovers running servers of the selected edition on agents you own.
One match connects directly; multiple matches show a `workspace/project/agent`
picker. A name or ID targets an agent directly and can wake a saved server.
Connecting also refreshes the detected Desktop edition's configuration.
Connecting never creates a new agent or installs a server on an unrelated box.
In noninteractive terminals, the CLI still attempts Desktop configuration and
prints connection details instead of prompting, installing software, or
launching a terminal client; multiple matches require a name or ID.

To run both client and server inside the cloud agent, in Railway CA:

```bash
railway code --opencode remote
railway code --opencode2 remote
```

Harness launches create a fresh VM; `--agent <name-or-id>` targets an existing one.
For the local-client setup, `--dir` selects the remote project directory
(default `/app`). Beta uses its server's startup directory, so switching it
requires a fresh agent. Setup uses the same generated credentials, HTTPS
checks, provider sign-in behavior, skills, and MCP sync as Desktop. It saves
settings in detected Desktop installations and prints them for manual entry.
A running server and its password are reused. Use `railway ca sleep <name>`
when finished.

Put harness-specific arguments after `--`, for example:
`railway code --opencode2 -- run --standalone "explain this project"`.

## Cloud agent bootstraps

Save a configured, running cloud agent as a reusable starting point:

```bash
railway ca bootstrap save dev --agent configured-vm
railway ca bootstrap list
railway ca bootstrap save updated --agent another-vm --default
railway ca bootstrap default dev

railway code --codex                         # uses your local environment default
railway code --codex --bootstrap updated     # selects a named bootstrap
railway code --codex --no-bootstrap          # starts with a clean VM
railway ca create scratch --bootstrap dev
```

On the `railway ca` launcher, click the bootstrap row below **Target Project**
or press **Option+B**. If bootstraps exist, the row says **Select Bootstrap** and
opens a compact, one-line-per-bootstrap list. Choose a bootstrap to make it your local default, **Create New** to create
one, or **No Default** to start future Cloud Agents without a bootstrap. Select
a project or environment in the sidebar and press **b** to open the same list;
this opens on **Create New** at the end of the list and uses that row's environment without changing
the launcher's prompt target.

Press **n** in the tree or bootstrap list to choose an agent for a **new VM**.
The picker includes **ChatGPT Codex**, a **Use bootstrap** checkbox, and an **Option+B**
shortcut to use the project default, select another bootstrap, or start clean.
These launch choices do not change the stored default. Press **Option+N** on
a VM, its session, or inside its focused terminal to choose an agent for a
**new session on that same VM**.

Failed captures are hidden from the TUI bootstrap pickers. Ready bootstraps and
captures still saving appear before **No Default**, with **Create New** last.

App shortcuts use **Option** (**Alt** on other keyboards): **Option+B** for
bootstraps, **Option+T** for the target project, and **Option+O** for an SSH shell.
**Option+Esc** returns keyboard focus to the tree. Ctrl+B, Ctrl+T, and Ctrl+]
remain compatibility shortcuts; Ctrl+C keeps its standard interrupt behavior.

Press **Enter** on an existing VM to reopen its primary coding agent terminal.
The CLI uses that VM's configuration and session history to identify the agent;
when it cannot, it asks you to choose without using or changing your default.
Clicking the terminal pane only changes focus; connecting to an unopened thread
still requires Enter or a double-click on that thread.

Drag the edge between the sidebar and terminal to resize the sidebar. Thread
text and horizontal separators use the full available width. The sidebar has
a solid dark grey background and a right border that defines its raised edge. The
width is saved when you release the mouse and restored next time you open the
CLI. A narrower terminal temporarily limits the displayed width without
changing that preference. Press Escape during a drag to cancel it.

The **Create bootstrap** form stays centered in the terminal pane. It has bordered
fields for a name, an optional repository (`owner/repo` or an HTTPS URL), and a
coding agent, plus a **Make default** checkbox. Click fields and the **Create
bootstrap** button, or use Tab/Shift+Tab to move between fields, left/right to
edit text or choose the agent, and Enter to activate a control. The footer shows
the available shortcuts.

Creation shows the current stage in a compact progress card while the CLI
creates a temporary VM, copies the selected harness's available local sign-in,
skills, MCP configuration and settings, optionally clones the repository into
`/app`, and saves a checkpoint. Existing Railway-managed harness settings take
precedence over imported settings. Private repositories use the VM's GitHub
access. Once capture is ready, the CLI applies your default choice and deletes
the setup VM. Provisioning or capture failures attempt cleanup and preserve the
previous default. Cleanup failures name the remaining VM.

The launcher returns to your unchanged prompt after completion. Creating from
the sidebar returns to the tree. Launching a real VM also shows only its current
preparation stage, in a fixed-width panel.

Highlight an existing running VM and press **b** to open a Name form inside the
TUI. Saving captures its disk without stopping, deleting, or disconnecting the
source VM. **Make default** starts checked when no default exists, and unchecked
when one is already selected; you can change it before creating. A failed disk
capture is not a usable bootstrap even though its name has been reserved. Retry
with the same name from the same VM to reuse that failed capture record; if it
is still saving, the TUI waits for that capture instead of submitting a duplicate.
Ready bootstraps and names captured from other VMs cannot be overwritten by
this form. A server-side snapshot timeout is reported with its failure reason,
and the default changes only after a successful capture. The flat
`railway ca bootstrap save` command still supports saving a new version of an
existing name and selecting it with `--default`. Default changes wait for capture
to succeed.

Bootstraps are shared within a project/environment pair. The default selection
is stored only in your local Railway CLI config, keyed by Railway host and
environment ID (which also identifies the project). It does not change the
dashboard or teammates’ defaults. Without a local default, new VMs start clean.
Explicit `--project` and `--environment` flags take precedence, followed by the
directory's Railway link, then the saved CA project preference. Bootstrap names
are resolved only within that environment. `list`, `save`, and `default` support
`--json`; `save` accepts `--env-file` and `--variable` for stored bootstrap
variables. Launch-time variables override stored bootstrap variables.

Only new VMs use bootstraps. Connecting to an existing VM preserves its disk.
A deleted, saving, or degraded local default produces an error; `--no-bootstrap` explicitly
bypasses it. `ca create --from-checkpoint` also bypasses the default and cannot
be combined with bootstrap flags.

The snapshot includes files and installed tools. Use an idempotent,
non-blocking `/etc/railway/bootstrap/startup.sh` to restart local services on
boot/wake, and `/etc/railway/bootstrap/AGENTS.md` for setup notes.

## Codex with local terminal or Desktop clients

Run the native Codex terminal UI on your computer, connected to Codex App Server
on a persistent Railway cloud agent:

```bash
railway code --codex
railway code --codex --agent my-box --dir /app
railway code --codex connect my-box
railway code --codex connect
```

New Codex agents use the same naming rules as OpenCode, with a `codex-` prefix
(for example, `codex-railg-3ed`). The generated name works with `connect`,
`get-config`, and cloud-agent lifecycle commands; `--name` sets a custom name.

For a Desktop-only workflow:

```bash
railway code --codex desktop-only
railway code --codex desktop-only --agent my-box --dir /app
```

This prepares the VM, starts or reuses the authenticated backend App Server,
verifies its public endpoint, registers SSH, and saves the remote project for
Codex Desktop to import at its next startup. It then exits, leaving the backend
running. It never prompts to
install or launch a local terminal client. The results panel shows backend
credentials, the SSH configuration file location, the Desktop project name, and
commands to reconnect or retrieve the configuration.

Terminal setup carries your available local Codex sign-in and configured
skills/MCP sync, starts App Server on the configured code port (default 4096, or
8080 for legacy connections), verifies its public WebSocket
handshake, and automatically launches the local client inside the Railway CA
frame. Tools, files, and threads live on the VM. Use `/resume` in Codex or select
a conversation in the CA sidebar to reopen a remote thread. `connect` discovers
running Codex servers; an explicit agent also wakes and restarts a previously
configured server as needed.

The remote terminal client uses a separate, persistent `CODEX_HOME` under
`~/.railway/codex-client/<backend-id>/`. The VM owns its tools and sessions;
local-only MCP servers, plugins, and hooks must not participate in remote
startup. This also keeps the terminal's busy indicator and cancellation state
consistent with the backend. Ctrl+C interrupts active work and exits when idle;
Ctrl+D on an empty prompt detaches even during startup.

The terminal connection uses `wss://` through the agent's public domain. Its
bearer token is passed to the local client through an environment variable.
The remote server's token, PID, version, directory, and logs live under
`~/.railway/codex/`. Setup on an explicitly selected agent reuses its token and
healthy server when the version is unchanged. An occupied port or a different
running project directory requires another agent.

Setup and `connect` also register and verify the agent's SSH host in
`~/.ssh/config` as `railway-<agent-name>`, using the same registration as
`railway ca desktop --codex`. Reconnecting upgrades the previous
`railway-agent-<agent-name>` default in both SSH and Codex Desktop's saved config.
They merge the connection and remote project into
`~/.codex/codex-app/config.json` (`$CODEX_HOME/codex-app/config.json` when set)
in the background. Setup never launches or activates Codex Desktop; the app
imports the saved configuration when you next start it.
Find `Railway: <name>` in Codex Desktop's project sidebar. An existing custom SSH
alias and project label are preserved. `connect` imports the server's actual
working directory. You can use the same VM from the terminal and Desktop.

Server setup silently checks npm for the latest official Codex release and
validates its authenticated App Server before replacing a running process.
Versions are cached under `~/.railway/runtimes/codex-server/<version>/`, with
update diagnostics in `~/.railway/codex/update.log`. A failed update preserves
the existing server. `connect` keeps a running server's version; restarting a
stopped server checks for updates. Terminal mode automatically finds or installs
the exact matching local `@openai/codex` version using npm under
`~/.railway/runtimes/codex-client/<version>/`.

For scripts, `--connection-json` returns a single JSON object containing
`schemaVersion`, agent identity, and connection `url`, `token`, `directory`,
`version`, and `reused`. Progress goes to stderr.

```bash
railway code --codex --connection-json connect my-box
railway code --codex remote             # run the UI inside Railway CA
railway code --codex -- exec "run tests" # execute inside the VM
```

Closing the local client leaves the server running. Use `railway ca sleep my-box`
to stop compute, then `railway code --codex connect my-box` to wake and reconnect.
Codex currently marks its remote App Server transport experimental.

Local connections enable automatic command permissions: Codex uses
`--ask-for-approval never --sandbox danger-full-access`, standard OpenCode
configures remote permissions while preserving explicit denies, and OpenCode2
uses `--auto`. These settings also apply on reconnect. Codex records trust for
the remote project directory, including a different repository selected when
resuming a thread.

Fresh OpenCode and OpenCode2 launches open the native home/splash screen. A
conversation is created when you submit a prompt; a launch with an initial
prompt starts directly in that conversation. Connection details are shown
before launch and again after the local client exits.

## Cloud agent conversation history

In `railway ca` and the `railway code` frame, expand a cloud agent in the left
list to browse its **Claude, Grok, Codex, OpenCode, and OpenCode2 conversations**.
Select a title and press Enter to reopen that exact thread. Saved conversations
remain available after their terminal exits; opening the list does not launch
clients for them. A fresh harness pane starts as **New Thread**, then adopts the
harness's generated title. **[S]** is reserved for direct VM shells; harness
consoles, server processes, and provisioning commands do not get session rows.
Conversation metadata is cached locally across restarts, including for sleeping
VMs. Discovery runs at startup, when selecting or expanding a running machine,
when Option+F / Alt+F reveals the sidebar, and on explicit refresh. Loaded rows,
their order, expansion choices, and the selected conversation stay in place.
The machine's status icon always represents its machine state.

Press **x / X** on a saved conversation to delete it from its harness. The row
disappears immediately while native deletion runs in the background; failures
restore the row and show an error. This deletes the saved conversation, rather
than just disconnecting its terminal. Draft **New Thread** panes simply close,
and **[S]** shell rows retain their end-session action.

There is no periodic account or VM-history polling. Codex and OpenCode title and
activity changes come from their existing native-client connections. Output from
an active SSH harness pane triggers a coalesced read of Railway's stored session
reports, without opening another VM connection. An idle sidebar does not refresh
history on its own; changes made elsewhere appear on the next explicit refresh.

Claude and Grok history is discovered directly on the VM over SSH, including
conversations started outside Railway's launcher. Claude uses a pinned official
Agent SDK, cached automatically on the VM when history is first discovered;
Grok uses its saved `summary.json` metadata. Discovery respects
`CLAUDE_CONFIG_DIR` and `GROK_HOME`, and filters hidden subagents and empty
startup records. A temporary discovery failure retains previously loaded rows.
Codex and both OpenCode versions also expose their VM-local metadata indexes,
so their history is available without a locally saved backend connection.

Selecting a Claude or Grok thread reconnects to its verified live terminal when
available, or resumes its native UI from the recorded project and configuration
directory. Claude background jobs use `claude attach`. Live metadata and hooks
update thread status and associate native panes with their conversation IDs.
Native Codex and OpenCode client actions update the pane's exact conversation
identity through a per-pane authenticated bridge.
History belongs to the VM where it was saved; wake a sleeping agent before
opening one of its threads.

## SSH shells and conversation resume

```bash
railway ca ssh my-box                 # open a login shell
railway ca ssh my-box --session       # attach to or start a durable session
railway ca ssh my-box --session name  # attach to a named session
railway ca ssh my-box --resume        # resume the latest Claude conversation
```

Plain SSH bypasses harness autostart. When opening a durable session after its
terminal has ended, interactive users can choose a recent Claude conversation.
In the CA frame, Option+O / Alt+O opens a shell on the selected VM and returns
to the frame on exit. The sidebar's `c` action copies the shell command.

## Retrieve saved connection configuration

```bash
railway code get-config                  # most recently saved connection
railway code get-config my-box           # a specific agent by name
railway code get-config codex-railg-3ed   # generated names work too
railway code get-config <agent-id>        # use an ID if names are ambiguous
railway code get-config my-box --json
```

Codex terminal setup, `connect`, and `desktop-only` (including its
`railway ca desktop --codex` alias) save the verified backend connection and
Desktop configuration outcome. OpenCode/OpenCode2 setup and reconnect also save
their server details; ordinary cloud-terminal launches save SSH details.

`get-config` works from any directory, using local snapshots without login,
network access, waking a VM, launching a client, or applying Desktop configuration.
Its human-readable output uses the same concise results panel as creation and
reconnect.
`--json` includes the saved agent/SSH metadata and the harness-specific `codex`
or `opencode` object, including credentials and Desktop status. These are saved
details, rather than a live health check; rerun setup/connect to refresh them.

Railway retains the latest snapshot per agent in `~/.railway/code-configs.json`
and continues writing `~/.railway/last-code-config.json` for compatibility with
existing CLI versions. Existing latest snapshots are retained when the archive
is first written. Named lookup requires a connection previously saved on this
computer; unknown names report an error, and duplicate names list their IDs.
The files are private and atomically updated under a lock. `railway logout`
removes both saved-configuration files.

## Cloud agents in desktop apps

Prepare a cloud agent for Claude Code Desktop, Codex, or OpenCode Desktop:

```bash
railway ca desktop --claude
railway ca desktop --codex
railway ca desktop --opencode --agent my-box
railway ca desktop --opencode --new
railway ca desktop --opencode2 --new
```

With Codex selected alone, `railway ca desktop --codex` is a compatibility alias
for `railway code --codex desktop-only`. It uses the same backend startup and
Desktop import flow, forwarding `--agent`, `--new`, `--dir`, project/environment,
and its SSH alias/config/verification options. The existing `--dry-run`,
`--remove`, and multi-app setup commands remain available.

App flags can be combined to prepare the same agent for several apps. Setup
carries available local sign-ins, applies your skills/MCP sync preferences,
and writes the agent's SSH configuration. `--dry-run` previews the local files
without creating or waking an agent or starting a connection. Existing agents
are reused and woken as needed; if none exists in the target environment, setup
creates one. `--new` always creates a fresh agent, including when several app
flags are combined. It cannot be combined with `--agent` or `--remove`.

Codex setup registers the SSH host and named remote project using Codex Desktop's
version-1 app configuration, entirely in the background. It never launches or
activates the app. Codex Desktop imports the configuration on its next startup.
Existing connections and retry/timeout preferences
are retained; changed configuration is backed up to `config.json.railway-backup`.
`--dry-run` previews the merged config. `--remove` removes the SSH
block and that alias's import declaration; remove already-imported connections
and projects inside Codex, since its import mechanism does not delete them.

OpenCode Desktop connects directly to the agent's existing HTTPS address.
The CLI starts password-protected [`opencode serve`](https://opencode.ai/docs/server/)
in the background on the configured code port (default 4096, or 8080 for legacy connections), checks
its public endpoint, and saves the URL,
username, password, default server, and remote project in Desktop's settings.
`--opencode` configures only standard OpenCode (`ai.opencode.desktop`).
`--opencode2` configures only [OpenCode2 Beta](https://github.com/anomalyco/opencode-beta)
(`ai.opencode.desktop.beta`), using its compatible server runtime. These two
flags cannot be combined. JSON server settings and SQLite renderer state are
supported; a drafts-only database does not change where settings are saved.

For Beta, the CLI seeds an `opencode2` shim on the agent. Each new process checks
the latest official Beta release, downloads the Linux Desktop package for the
agent's architecture, verifies its published SHA-256, and extracts just the CLI
executable. The first start can take several minutes. A verified cached version
is reused until the release changes; running sessions keep their executable.
A failed update reports an error and preserves the previous runtime.

Use `railway ca --opencode2` for a terminal session. In the new-session picker,
highlight OpenCode and press Tab to switch to **OpenCode2 [Beta]**. Tab also
switches editions in the prompt footer; Shift+Tab cycles harnesses.

Setup also prints the connection details and a reminder that you may need to
restart OpenCode Desktop to load the updated configuration.
Private `.railway-backup` files preserve the previous settings and server state.
You can close the terminal after setup; there is no local tunnel to keep running.

Press Cmd+B (Ctrl+B on Windows/Linux) to open Home. Under Projects, find
`Railway: <agent-name>` and `/app` (or your `--dir`), then use that project's
menu → New session. Setting a default server
does not move existing chats. Standard OpenCode provider sign-ins from `auth.json` are copied
when available. OpenCode2 imports the active account for each provider from your local
Beta credential database, preserving accounts already configured on the cloud agent.
Legacy `auth.json` is used only when no Beta credential store exists. Credentials are
sent over SSH and the temporary transfer file is removed after import. If there is
no local sign-in to copy, connect the provider in the remote server's settings.

OpenCode uses the agent's code endpoint when available. Setup refuses to take
over an occupied server port; stop the other process or use `--new` for a fresh agent.
Rerunning `railway ca desktop --opencode --agent my-box` reuses the running
server and its credentials, or starts it again after a sleep/wake or restart.
The remote credential and process state are stored privately under
`~/.railway/desktop/opencode/`; startup logs are in `server.log` there.

`railway ca desktop --opencode --agent my-box --remove` (or `--opencode2`
for Beta) stops the managed
OpenCode process on a running agent and removes the shared SSH block and the
Desktop connection/project saved by setup. It does not wake a sleeping agent.
The agent remains; `railway ca sleep my-box` stops its compute bill.

## Contributing

See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repository locally.

## Feedback

Share feedback and suggestions on [Central Station](https://station.railway.com/feedback).
