#!/usr/bin/env node
import { execFileSync } from "child_process";
import { constants } from "os";
import path from "path";
import { exit } from "process";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const binName = process.platform === "win32" ? "railway.exe" : "railway";
const binPath = path.resolve(__dirname, binName);

try {
    execFileSync(binPath, process.argv.slice(2), { stdio: "inherit" });
} catch (e) {
    // The binary is downloaded by npm-install/postinstall.js. If that step was
    // skipped (--ignore-scripts) or failed (unsupported platform, network), the
    // spawn fails with ENOENT — say so instead of exiting 1 with no output.
    if (e.code === "ENOENT") {
        console.error(
            `railway: could not find the CLI binary at ${binPath}\n` +
                `The @railway/cli install step did not complete. Try:\n` +
                `  npm install -g @railway/cli --foreground-scripts\n` +
                `or install the CLI directly:\n` +
                `  curl -fsSL https://railway.com/install.sh | sh`,
        );
        exit(127);
    }

    // Propagate the real exit status. Without this every failure collapses to 1,
    // which hides usage errors (clap exits 2), breaks `set -e` callers that
    // inspect the code, and masks crashes as ordinary failures.
    if (typeof e.status === "number") {
        exit(e.status);
    }

    // Killed by a signal: report it the way a shell does (128 + signal number)
    // so a segfault surfaces as 139 rather than a generic 1.
    if (e.signal) {
        const signum = constants.signals[e.signal];
        exit(signum ? 128 + signum : 1);
    }

    exit(1);
}
