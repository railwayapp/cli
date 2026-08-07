import { createWriteStream } from "fs";
import * as fs from "fs/promises";
import fetch from "node-fetch";
import path from "path";
import { pipeline } from "stream/promises";
import { fileURLToPath } from "url";
import tar from "tar";

import { CONFIG } from "./config.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = path.resolve(__dirname, "..");

// process.platform/arch -> release target triple.
//
// This is deliberately hand-maintained rather than derived from a generic table
// (we previously used @napi-rs/triples): the generic answer for Linux is always
// `*-unknown-linux-gnu`, and we do not publish gnu builds for any Linux arch
// except x86_64. That silently broke `npm i -g @railway/cli` on arm64/arm/ia32
// Linux with "Failed fetching the binary: Not Found".
//
// Every entry below must correspond to an asset produced by
// .github/workflows/release.yml. Linux uses the statically linked musl builds
// for the same reason install.sh does: one binary that runs regardless of the
// host's libc.
const TARGETS = {
    linux: {
        x64: "x86_64-unknown-linux-musl",
        arm64: "aarch64-unknown-linux-musl",
        ia32: "i686-unknown-linux-musl",
        arm: "arm-unknown-linux-musleabihf",
    },
    darwin: {
        x64: "x86_64-apple-darwin",
        arm64: "aarch64-apple-darwin",
    },
    win32: {
        x64: "x86_64-pc-windows-gnu",
        ia32: "i686-pc-windows-gnu",
        arm64: "aarch64-pc-windows-msvc",
    },
};

function resolveTarget() {
    const { platform, arch } = process;
    const triple = TARGETS[platform]?.[arch];

    if (!triple) {
        const supported = Object.entries(TARGETS)
            .flatMap(([p, arches]) => Object.keys(arches).map((a) => `${p}-${a}`))
            .join(", ");
        throw new Error(
            `Railway CLI does not ship a prebuilt binary for ${platform}-${arch}.\n` +
                `Supported: ${supported}\n` +
                `Request a build at https://github.com/railwayapp/cli/issues/new, ` +
                `or build from source with \`cargo install railwayapp\`.`,
        );
    }

    return triple;
}

async function install() {
    const packageJson = await fs
        .readFile(path.join(PACKAGE_ROOT, "package.json"), "utf8")
        .then(JSON.parse);
    let version = packageJson.version;

    if (typeof version !== "string") {
        throw new Error("Missing version in package.json");
    }

    if (version[0] === "v") version = version.slice(1);

    const { name: binName, path: binPath, url } = CONFIG;
    const triple = resolveTarget();

    const downloadUrl = url
        .replace(/{{triple}}/g, triple)
        .replace(/{{version}}/g, version)
        .replace(/{{bin_name}}/g, binName);

    console.log(downloadUrl);
    const response = await fetch(downloadUrl);
    if (!response.ok) {
        throw new Error(
            `Failed fetching the binary (${response.status} ${response.statusText}): ${downloadUrl}`,
        );
    }

    const destDir = path.resolve(PACKAGE_ROOT, binPath);
    const tarFile = path.join(PACKAGE_ROOT, "downloaded.tar.gz");

    await fs.mkdir(destDir, { recursive: true });
    await pipeline(response.body, createWriteStream(tarFile));
    await tar.x({ file: tarFile, cwd: destDir });
    await fs.rm(tarFile);

    // Fail loudly here rather than leaving a package whose `railway` shim exits
    // with a confusing ENOENT on first use.
    const expected = path.join(destDir, process.platform === "win32" ? "railway.exe" : "railway");
    await fs.access(expected).catch(() => {
        throw new Error(`Archive did not contain the expected binary at ${expected}`);
    });
}

install()
    .then(async () => {
        process.exit(0);
    })
    .catch(async (err) => {
        console.error(err.message ?? err);
        process.exit(1);
    });
