#!/usr/bin/env node
import { execFileSync } from "child_process";
import path from "path";
import { exit } from "process";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const binName = process.platform === "win32" ? "railway.exe" : "railway";
try {
	execFileSync(path.resolve(`${__dirname}/${binName}`), process.argv.slice(2), {
		stdio: "inherit",
	});
} catch (e) {
	exit(1);
}
