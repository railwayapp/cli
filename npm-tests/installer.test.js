import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import * as fs from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import * as tar from "tar";

const run = promisify(execFile);
const root = fileURLToPath(new URL("../", import.meta.url));
const binary = process.platform === "win32" ? "railway.exe" : "railway";

test("npm installer with native fetch and tar 7", async (t) => {
    // Keep fixtures below the package root so the copied installer resolves tar.
    const scratch = await fs.mkdtemp(path.join(root, ".installer-test-"));
    t.after(() => fs.rm(scratch, { recursive: true, force: true }));
    const source = path.join(scratch, "source");
    await fs.mkdir(source);
    await fs.writeFile(path.join(source, binary), "fixture binary");
    const archive = path.join(scratch, "release.tar.gz");
    await tar.c({ gzip: true, file: archive, cwd: source }, [binary]);
    const archiveBytes = await fs.readFile(archive);
    await fs.writeFile(path.join(source, "unrelated"), "wrong release");
    const missing = path.join(scratch, "missing.tar.gz");
    await tar.c({ gzip: true, file: missing, cwd: source }, ["unrelated"]);
    const missingBytes = await fs.readFile(missing);
    const server = createServer((req, res) => {
        if (req.url === "/redirect") {
            res.writeHead(302, { location: "/binary" });
            res.end();
        } else if (req.url === "/binary") {
            res.end(archiveBytes);
        } else if (req.url === "/missing") {
            res.end(missingBytes);
        } else if (req.url === "/corrupt") {
            res.end("not a tar archive");
        } else {
            res.writeHead(404);
            res.end("not found");
        }
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    t.after(() => new Promise((resolve) => server.close(resolve)));
    const base = `http://127.0.0.1:${server.address().port}`;

    async function install(route) {
        const dest = await fs.mkdtemp(path.join(scratch, "package-"));
        const installer = path.join(dest, "npm-install");
        await fs.mkdir(installer);
        await fs.copyFile(path.join(root, "npm-install/postinstall.js"), path.join(installer, "postinstall.js"));
        await fs.writeFile(path.join(dest, "package.json"), JSON.stringify({ type: "module", version: "5.71.0" }));
        await fs.writeFile(path.join(installer, "config.js"),
            `export const CONFIG = ${JSON.stringify({ name: "railway", path: "./bin", url: base + route })};`);
        let failure;
        try {
            await run(process.execPath, [path.join(installer, "postinstall.js")], { timeout: 15000 });
        } catch (error) {
            failure = error;
        }
        return { dest, failure };
    }

    for (const route of ["/binary", "/redirect"]) {
        await t.test(`downloads and extracts ${route}`, async () => {
            const { dest, failure } = await install(route);
            assert.equal(failure, undefined);
            assert.equal(await fs.readFile(path.join(dest, "bin", binary), "utf8"), "fixture binary");
            await assert.rejects(fs.access(path.join(dest, "downloaded.tar.gz")), { code: "ENOENT" });
        });
    }
    for (const [route, message] of [
        ["/404", /Failed fetching the binary \(404/],
        ["/corrupt", /TAR_BAD_ARCHIVE/],
        ["/missing", /Archive did not contain the expected binary/],
    ]) {
        await t.test(`fails clearly for ${route}`, async () => {
            const { dest, failure } = await install(route);
            assert.equal(failure?.code, 1);
            assert.match(failure.stderr, message);
            await assert.rejects(fs.access(path.join(dest, "downloaded.tar.gz")), { code: "ENOENT" });
        });
    }
});
