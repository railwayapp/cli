//! Execution context: a single, typed view of *how* the CLI is being
//! run, and the auth decisions that follow from it.
//!
//! Commands used to re-assemble the same scattered booleans (`--json`,
//! `--ci`, TTY checks, agent-harness detection, headless detection) by
//! hand, each combining them slightly differently — which is exactly how
//! auth dead-ends crept in. This centralizes the gathering in
//! [`ExecutionContext::detect`] and makes the decisions
//! ([`auto_auth`](ExecutionContext::auto_auth),
//! [`login_transport`](ExecutionContext::login_transport)) *pure*
//! functions of the context fields, so they can be exhaustively
//! truth-tabled in tests without touching the environment.

use is_terminal::IsTerminal;

/// Which transport an interactive sign-in should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthTransport {
    /// Open a local browser (authorization-code + PKCE).
    Browser,
    /// Print a verification URL + device code (RFC 8628) for a human to
    /// use on any device.
    DeviceCode,
}

/// The decision for a command that auto-starts auth as a *side effect*
/// (e.g. an unauthenticated `railway up`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoAuth {
    /// Proceed with interactive sign-in via the given transport.
    Proceed(AuthTransport),
    /// No interactive human is reachable (CI, `--json`, piped with no
    /// agent harness, …). The caller should surface a structured
    /// `NOT_AUTHENTICATED` error rather than opening a browser nobody
    /// can see or blocking on a device code nobody can enter.
    FailFast,
}

/// A typed snapshot of the run environment. Construct via [`detect`] in
/// real code; construct directly in tests.
///
/// [`detect`]: ExecutionContext::detect
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionContext {
    /// The command was asked for machine-readable output (`--json`).
    pub json: bool,
    /// CI mode was requested explicitly (`--ci`).
    pub ci: bool,
    /// stdout is a terminal (not piped/captured).
    pub stdout_tty: bool,
    /// stdin is a terminal (we can prompt).
    pub stdin_tty: bool,
    /// A known agent harness (Claude Code, Cursor, Codex, …) is driving
    /// us — implies a human is watching the transcript even when stdio
    /// is piped.
    pub agent_harness: bool,
    /// A local browser can plausibly open (not CI/SSH/headless).
    pub browser_reachable: bool,
}

impl ExecutionContext {
    /// Gather the context from the environment. The `json`/`ci` flags
    /// come from the invoking command's args; everything else is
    /// detected. This is the only impure part — the decision methods
    /// below are pure functions of the resulting fields.
    pub fn detect(json: bool, ci: bool) -> Self {
        Self {
            json,
            ci,
            stdout_tty: std::io::stdout().is_terminal(),
            stdin_tty: std::io::stdin().is_terminal(),
            agent_harness: crate::telemetry::is_agent_harness(),
            browser_reachable: !crate::commands::login::is_likely_headless(),
        }
    }

    /// True when an agent harness is driving us with piped stdin. The
    /// agent invocation is treated as implicit consent, so commands skip
    /// the interactive "Continue?" prompt (it couldn't be answered
    /// anyway). The stdin-not-a-TTY requirement guards against a normal
    /// interactive terminal that merely carries a stale `AI_AGENT=…`
    /// export. This gates prompt-skipping only — it does not change any
    /// OAuth timeout.
    pub fn agent_implicit_consent(&self) -> bool {
        self.agent_harness && !self.stdin_tty
    }

    /// Transport for an *explicit* sign-in (`railway login`). The user
    /// asked to authenticate, so we always attempt it; only the
    /// transport varies.
    pub fn login_transport(&self, browserless: bool) -> AuthTransport {
        if browserless || !self.browser_reachable {
            AuthTransport::DeviceCode
        } else {
            AuthTransport::Browser
        }
    }

    /// Decision for *implicit* sign-in triggered as a side effect (an
    /// unauthenticated `railway up`). Fails fast only when there's no
    /// human to complete a flow; otherwise proceeds with the appropriate
    /// transport — a browser when one is reachable, or a device code
    /// (which the human completes on another device) when it isn't.
    pub fn auto_auth(&self, browserless: bool) -> AutoAuth {
        // Machine contexts have no human in the loop: JSON output is
        // consumed by a tool, and --ci is non-interactive by definition.
        if self.json || self.ci {
            return AutoAuth::FailFast;
        }
        // No human reachable to complete a sign-in: stdout is captured
        // and there's no agent harness whose human could complete it.
        if !self.stdout_tty && !self.agent_implicit_consent() {
            return AutoAuth::FailFast;
        }
        // A human is present. Pick the transport: a browser if one can
        // open, otherwise device-code (SSH / no DISPLAY) — the same
        // fallback `railway login` uses here, so an unauthenticated `up`
        // can sign the user in and deploy in one shot even on a remote
        // box. (When there's genuinely no human, we already failed fast
        // above rather than print a code into the void.)
        AutoAuth::Proceed(self.login_transport(browserless))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builder for a fully-specified context so each test reads as a
    /// truth-table row.
    fn ctx(
        json: bool,
        ci: bool,
        stdout_tty: bool,
        stdin_tty: bool,
        agent_harness: bool,
        browser_reachable: bool,
    ) -> ExecutionContext {
        ExecutionContext {
            json,
            ci,
            stdout_tty,
            stdin_tty,
            agent_harness,
            browser_reachable,
        }
    }

    // --- login_transport: explicit `railway login` always attempts ---

    #[test]
    fn login_uses_browser_when_reachable_and_not_browserless() {
        let c = ctx(false, false, true, true, false, true);
        assert_eq!(c.login_transport(false), AuthTransport::Browser);
    }

    #[test]
    fn login_uses_device_code_when_browserless_requested() {
        let c = ctx(false, false, true, true, false, true);
        assert_eq!(c.login_transport(true), AuthTransport::DeviceCode);
    }

    #[test]
    fn login_uses_device_code_when_no_browser_reachable() {
        // SSH / no-DISPLAY / CI env: browser can't open, but the user
        // explicitly asked to log in, so device-code (not fail-fast).
        let c = ctx(false, false, true, true, false, false);
        assert_eq!(c.login_transport(false), AuthTransport::DeviceCode);
    }

    // --- auto_auth: implicit sign-in from an unauthed `railway up` ---

    #[test]
    fn auto_auth_proceeds_in_a_plain_interactive_terminal() {
        let c = ctx(false, false, true, true, false, true);
        assert_eq!(c.auto_auth(false), AutoAuth::Proceed(AuthTransport::Browser));
    }

    #[test]
    fn auto_auth_proceeds_under_an_agent_harness_with_piped_stdio() {
        // Agent harness with captured stdout/stdin: a human is watching
        // and can complete the browser sign-in.
        let c = ctx(false, false, false, false, true, true);
        assert_eq!(c.auto_auth(false), AutoAuth::Proceed(AuthTransport::Browser));
    }

    #[test]
    fn auto_auth_fails_fast_in_json_mode() {
        let c = ctx(true, false, true, true, false, true);
        assert_eq!(c.auto_auth(false), AutoAuth::FailFast);
    }

    #[test]
    fn auto_auth_fails_fast_in_ci_mode() {
        let c = ctx(false, true, true, true, false, true);
        assert_eq!(c.auto_auth(false), AutoAuth::FailFast);
    }

    #[test]
    fn auto_auth_uses_device_code_on_ssh_with_an_interactive_human() {
        // SSH / no-DISPLAY with an interactive TTY: no local browser, but
        // a human is present, so `up` falls back to a device code they
        // complete on another device instead of failing fast.
        let c = ctx(false, false, true, true, false, false);
        assert_eq!(
            c.auto_auth(false),
            AutoAuth::Proceed(AuthTransport::DeviceCode)
        );
    }

    #[test]
    fn auto_auth_uses_device_code_on_ssh_under_a_watching_agent() {
        // SSH with an agent harness (piped stdio): the watching human can
        // complete the printed device code.
        let c = ctx(false, false, false, false, true, false);
        assert_eq!(
            c.auto_auth(false),
            AutoAuth::Proceed(AuthTransport::DeviceCode)
        );
    }

    #[test]
    fn auto_auth_fails_fast_on_ssh_with_no_human() {
        // SSH / no-DISPLAY, captured stdout, no agent harness: nobody to
        // read the code, so fail fast rather than print it into the void.
        let c = ctx(false, false, false, false, false, false);
        assert_eq!(c.auto_auth(false), AutoAuth::FailFast);
    }

    #[test]
    fn auto_auth_fails_fast_when_stdout_piped_and_no_agent() {
        // A bare script / pipe with no agent harness: nobody to complete
        // a browser flow.
        let c = ctx(false, false, false, false, false, true);
        assert_eq!(c.auto_auth(false), AutoAuth::FailFast);
    }

    #[test]
    fn auto_auth_does_not_treat_stale_agent_export_in_a_tty_as_consent() {
        // Agent env var present but a real interactive stdin (stale
        // dotfile export). stdout is a TTY, so we still proceed — but via
        // the normal interactive path, not implicit-agent consent.
        let c = ctx(false, false, true, true, true, true);
        assert!(!c.agent_implicit_consent());
        assert_eq!(c.auto_auth(false), AutoAuth::Proceed(AuthTransport::Browser));
    }
}
