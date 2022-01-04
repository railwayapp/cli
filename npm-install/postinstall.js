import { createWriteStream } from "fs";
import * as fs from "fs/promises";
import fetch from "node-fetch";
import { pipeline } from "stream/promises";
import tar from "tar";

import { ARCH_MAPPING, CONFIG, PLATFORM_MAPPING } from "./config.js";

async function install() {
  const packageJson = await fs.readFile("package.json").then(JSON.parse);
  let version = packageJson.version;

  if (typeof version !== "string") {
    throw new Error("Missing version in package.json");
  }

  if (version[0] === "v") version = version.slice(1);

  let { name: binName, path: binPath, url } = CONFIG;

  // Binary name on Windows has .exe suffix
  if (process.platform === "win32") {
    binName += ".exe";

    url = url.replace(/{{win_ext}}/g, ".exe");
  } else {
    url = url.replace(/{{win_ext}}/g, "");
  }

  url = url.replace(/{{arch}}/g, ARCH_MAPPING[process.arch]);
  url = url.replace(/{{platform}}/g, PLATFORM_MAPPING[process.platform]);
  url = url.replace(/{{version}}/g, version);
  url = url.replace(/{{bin_name}}/g, binName);

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error("Failed fetching the binary: " + response.statusText);
  }

  const tarFile = "downloaded.tar.gz";

  await fs.mkdir(binPath, { recursive: true });
  await pipeline(response.body, createWriteStream(tarFile));
  await tar.x({ file: tarFile, cwd: binPath });
  await fs.rm(tarFile);
}

install()
  .then(async () => {
    process.exit(0);
  })
  .catch(async (err) => {
    console.error(err);
    process.exit(1);
  });
