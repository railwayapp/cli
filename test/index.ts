import { RedisClient, sql } from "bun";

const REDIS_URL = process.env.REDIS_URL;
const DATABASE_URL = process.env.DATABASE_URL;
const UTILS_URL = process.env.UTILS_URL;

type UtilsData = {
  info: {
    serverStartTime: string;
  };
};

console.log("REDIS_URL", REDIS_URL);
console.log("DATABASE_URL", DATABASE_URL);
console.log("UTILS_URL", UTILS_URL);

console.log("\n---\n");

async function testUtils() {
  if (!UTILS_URL) {
    console.log("Utils: UTILS_URL not set");
    return;
  }
  try {
    const start = performance.now();
    const response = await fetch(`${UTILS_URL}/api/info`);
    const data = (await response.json()) as UtilsData;
    const ms = Math.round((performance.now() - start) * 100) / 100;
    console.log(
      `Utils: ms=${ms}. Server start time`,
      data.info.serverStartTime
    );
  } catch (error) {
    console.log("Utils: error", error);
  }
}

async function testRedis() {
  if (!REDIS_URL) {
    console.log("Redis: REDIS_URL not set");
    return;
  }
  const client = new RedisClient(REDIS_URL);
  try {
    const start = performance.now();
    const count = await client.get("test-counter");
    const ms = Math.round((performance.now() - start) * 100) / 100;
    console.log(`Redis: count=${count} ms=${ms}`);
  } finally {
    client.close();
  }
}

async function testPostgres() {
  if (!DATABASE_URL) {
    console.log("Postgres: DATABASE_URL not set");
    return;
  }
  await sql`
      CREATE TABLE IF NOT EXISTS counter (
        id TEXT PRIMARY KEY,
        value INTEGER NOT NULL DEFAULT 0
      )
    `;
  const start = performance.now();
  const result = await sql`
      SELECT value FROM counter WHERE id = 'test'
    `;
  const ms = Math.round((performance.now() - start) * 100) / 100;
  console.log(`Postgres: count=${result[0]?.value} ms=${ms}`);
  await sql.close();
}

await testUtils();
await testRedis();
await testPostgres();

const server = Bun.serve({
  port: process.env.PORT || 8888,
  fetch(req) {
    const url = new URL(req.url);
    console.log(`[${new Date().toISOString()}] ${req.method} ${url.pathname}`);
    if (url.pathname === "/") {
      return new Response("this is a bun server");
    }
    return new Response("Not Found", { status: 404 });
  },
});

console.log(`Server running at http://localhost:${server.port}`);

console.log("test hi ");
