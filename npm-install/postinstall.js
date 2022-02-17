import { createWriteStream } from "fs";
import { mkdir, rm } from "fs/promises";
import fetch from "node-fetch";
import { pipeline } from "stream/promises";
import tar from "tar";

import { ARCH_MAPPING, CONFIG, PLATFORM_MAPPING } from "./config.js";

async function install() {
  const latestRelease = await fetch(CONFIG.releasesUrl).then(res => res.json())
  let version = latestRelease.tag_name?.replace("v", "");

  if (typeof version !== "string") {
    throw new Error("Missing version!");
  }

  // Fetch Static Config
  let { name: binName, path: binPath, url } = CONFIG;

  url = url.replace(/{{arch}}/g, ARCH_MAPPING[process.arch]);
  url = url.replace(/{{platform}}/g, PLATFORM_MAPPING[process.platform]);
  url = url.replace(/{{version}}/g, version);
  url = url.replace(/{{bin_name}}/g, binName);

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error("Failed fetching the binary: " + response.statusText);
  }

  const tarFile = "downloaded.tar.gz";

  await mkdir(binPath, { recursive: true });
  await pipeline(response.body, createWriteStream(tarFile));
  await tar.x({ file: tarFile, cwd: binPath });
  await rm(tarFile);

  console.info(`Railway CLI v${version} installed`)
}

install()
  .then(async () => {
    process.exit(0);
  })
  .catch(async (err) => {
    console.error(err);
    process.exit(1);
  });
