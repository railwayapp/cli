#!/usr/bin/env bash
# Read-only diagnostic for the Railway CLI agent-detection rewrite.
# Captures every signal the new `detect_caller()` reads (env vars,
# IDE host indicators, CI markers, TTY status, full process ancestry)
# so an offline matcher can compute what `caller` value would be
# emitted from this exact context. Safe to run anywhere — no
# mutation, no network, just `printenv`, `tty`, and `ps`.

set -u

print_kv() {
    val="$(printenv "$1" 2>/dev/null || true)"
    if [ -n "$val" ]; then
        printf '%s=%s\n' "$1" "$val"
    fi
}

echo "### RAILWAY_CALLER_DIAGNOSTIC v1"
echo "### timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "### uname: $(uname -a 2>/dev/null || echo unknown)"
echo

echo "### TTY_STATUS"
if [ -t 0 ]; then echo "stdin_tty=true"; else echo "stdin_tty=false"; fi
if [ -t 1 ]; then echo "stdout_tty=true"; else echo "stdout_tty=false"; fi
if [ -t 2 ]; then echo "stderr_tty=true"; else echo "stderr_tty=false"; fi
echo

echo "### TERM_AND_HOST_VARS"
for v in TERM TERM_PROGRAM TERM_PROGRAM_VERSION TERMINAL_EMULATOR TERM_PRODUCT \
         __CFBundleIdentifier VSCODE_PID VSCODE_INJECTION VSCODE_GIT_IPC_HANDLE \
         VSCODE_IPC_HOOK_CLI ZED_SESSION_ID XCODE_VERSION_ACTUAL POSITRON; do
    print_kv "$v"
done
echo

echo "### AGENT_ENV"
for v in CLAUDECODE CLAUDE_CODE CLAUDE_CODE_SESSION_ID CLAUDE_CODE_ENTRYPOINT \
         CLAUDE_CODE_EXECPATH AI_AGENT \
         CURSOR_AGENT CURSOR_TRACE_ID \
         CODEX_SANDBOX OPENAI_CODEX \
         OPENCODE OPENCODE_SESSION_ID \
         AMP_CURRENT_THREAD_ID AGENT \
         AIDER \
         COPILOT_AGENT_SESSION_ID COPILOT_CLI \
         FACTORY_DROID GEMINI_CLI \
         REPLIT_AGENT REPL_ID REPLIT_USER REPL_SLUG REPL_OWNER \
         PI_CODING_AGENT __COG_BASHRC_SOURCED \
         CODESPACES CLOUD_SHELL EDITOR_IN_CLOUD_SHELL MONOSPACE_ENV \
         ANTIGRAVITY_CLI_ALIAS \
         RAILWAY_CALLER RAILWAY_AGENT_SESSION RAILWAY_INSTALL_REQUEST_ID; do
    print_kv "$v"
done
echo

echo "### CI_ENV"
for v in CI GITHUB_ACTIONS GITLAB_CI CIRCLECI BUILDKITE JENKINS_URL \
         TRAVIS TEAMCITY_VERSION TF_BUILD BITBUCKET_BUILD_NUMBER \
         DRONE SEMAPHORE CODEBUILD_BUILD_ID NETLIFY VERCEL \
         RAILWAY_ENVIRONMENT_ID RAILWAY_PROJECT_ID; do
    print_kv "$v"
done
echo

echo "### PROCESS_ANCESTRY"
echo "self_pid=$$"
PID=$$
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    LINE="$(ps -o pid=,ppid=,command= -p "$PID" 2>/dev/null | head -1)"
    [ -z "$LINE" ] && break
    printf 'hop_%02d: %s\n' "$i" "$LINE"
    PARENT="$(echo "$LINE" | awk '{print $2}')"
    [ -z "$PARENT" ] && break
    [ "$PARENT" = "0" ] && break
    [ "$PARENT" = "$PID" ] && break
    PID="$PARENT"
done
echo

echo "### END"
