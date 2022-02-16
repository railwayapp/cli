#!/usr/bin/env node
import { execFileSync } from "child_process";
import path from "path";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

execFileSync(path.resolve(`${__dirname}/railway`), process.argv.slice(2), {
  stdio: "inherit",
});
