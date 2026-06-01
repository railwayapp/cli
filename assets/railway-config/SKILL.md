# Railway configuration skill

Use this skill when editing this repository's Railway configuration.

The source of desired Railway project state is:

```txt
.railway/railway.ts
```

## Rules

1. Express Railway product intent, not internal API details.
2. Do not write Railway UUIDs into `.railway/railway.ts`.
3. Do not write `EnvironmentConfigPatch`, `ServiceInstance`, or Backboard internals into source.
4. Prefer helpers like `service()`, `postgres()`, `redis()`, `bucket()`, and `volume()`.
5. Keep secrets out of source. Prefer references and Railway-managed variables.
6. After editing `.railway/railway.ts`, run `railway config plan`.
7. Do not run `railway config apply` unless the user asks.

## Commands

Preview:

```bash
railway config plan
```

Stage:

```bash
railway config stage
```

Apply:

```bash
railway config apply
```

Machine-readable preview:

```bash
railway config plan --json
```

## Authoring

Use the Railway configuration helpers:

```ts
import {
  bucket,
  defineRailway,
  github,
  image,
  mongo,
  mysql,
  postgres,
  project,
  redis,
  service,
  volume,
} from "railway/iac";
```

Minimal service:

```ts
const web = service("web", {
  build: "pnpm install --frozen-lockfile && pnpm build",
  start: "pnpm start",
  env: {
    NODE_ENV: "production",
  },
});
```

Database reference:

```ts
const db = postgres("postgres");

const web = service("web", {
  env: {
    DATABASE_URL: db.url(),
    PGHOST: db.env.PGHOST,
  },
});
```

Service-to-service reference:

```ts
const api = service("api", {
  env: {
    INTERNAL_TOKEN: "replace-me",
  },
});

const web = service("web", {
  env: {
    API_TOKEN: api.env.INTERNAL_TOKEN,
    API_HOST: api.env.RAILWAY_PRIVATE_DOMAIN,
  },
});
```

Custom domain:

```ts
const web = service("web", {
  domains: ["app.example.com"],
});
```

Project shape:

```ts
export default defineRailway(() => {
  const db = postgres("postgres");
  const web = service("web", {
    env: {
      DATABASE_URL: db.url(),
    },
  });

  return project("my-app", {
    environments: ["production"],
    services: [db, web],
  });
});
```
