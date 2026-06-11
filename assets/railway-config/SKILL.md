---
name: railway-config
description: Edit this project's Railway infrastructure-as-code configuration. Use this skill whenever the user asks to create, change, import, review, or troubleshoot Railway project infrastructure for the current repository, including services, databases, buckets, custom domains, replicas/regions, groups, environment variables, `railway config *`, or `.railway/railway.ts`.
---

# Railway configuration skill

Use this skill when editing this repository's Railway configuration.

The source of desired Railway project state is:

```txt
.railway/railway.ts
```

## Core rules

1. Express Railway product intent, not internal API details.
2. Do not write Railway UUIDs into `.railway/railway.ts`.
3. Do not write `EnvironmentConfigPatch`, `ServiceInstance`, Backboard internals, or generated Railway domains into source.
4. Prefer Railway configuration helpers like `service()`, `postgres()`, `redis()`, `mysql()`, `mongo()`, `bucket()`, `group()`, `github()`, and `image()`.
5. Use `service.env.VARIABLE` and `database.env.VARIABLE` for references.
6. Keep secrets out of source. Imported unknown secret values should use `preserve()` or be omitted when the user wants a smaller import.
7. Prefer product DSL names such as `domains`, `replicas`, and `group`; avoid internal names like `customDomains` and `multiRegionConfig`.
8. Do not add platform defaults unless the user explicitly wants them.
9. Do not manage a service from both `.railway/railway.ts` and `railway.json` / `railway.toml`; migrate the repo config first.
10. After editing `.railway/railway.ts`, run `railway config plan`.
11. Do not run `railway config apply` unless the user explicitly asks.
12. Never use `railway config apply --yes` or `railway config apply --confirm-destructive` from an agent session without explicit user approval for the exact plan.

## Commands

Initialize configuration files:

```bash
railway config init
```

Import current Railway state:

```bash
railway config pull
```

Import current Railway state with a smaller generated file that omits unknown variable values:

```bash
railway config pull --omit-preserved-variables
```

Preview changes:

```bash
railway config plan
```

Apply changes:

```bash
railway config apply
```

Machine-readable preview:

```bash
railway config plan --json
```

## Authoring

Use Railway configuration helpers:

```ts
import {
  bucket,
  defineRailway,
  github,
  group,
  image,
  mongo,
  mysql,
  postgres,
  preserve,
  project,
  redis,
  service,
} from "railway/iac";
```

Minimal local service:

```ts
const web = service("web", {
  build: "bun run build",
  start: "NODE_ENV=production bun src/index.ts",
});
```

GitHub service:

```ts
const web = service("web", {
  source: github("owner/repo", { branch: "main" }),
  build: "pnpm run build",
  start: "pnpm start",
});
```

Docker image service:

```ts
const worker = service("worker", {
  source: image("ghcr.io/acme/worker:latest"),
});
```

Database reference:

```ts
const db = postgres("postgres");

const web = service("web", {
  env: {
    DATABASE_URL: db.env.DATABASE_URL,
  },
});
```

Service-to-service reference:

```ts
const api = service("api", {
  env: {
    INTERNAL_TOKEN: preserve(),
  },
});

const web = service("web", {
  env: {
    API_TOKEN: api.env.INTERNAL_TOKEN,
    API_HOST: api.env.RAILWAY_PRIVATE_DOMAIN,
  },
});
```

Custom domains:

```ts
const web = service("web", {
  domains: ["app.example.com"],
});
```

Replicas:

```ts
const web = service("web", {
  replicas: 3,
});
```

Advanced placement:

```ts
const web = service("web", {
  replicas: {
    "us-west2": 2,
    "europe-west4": 1,
  },
});
```

Groups:

```ts
const api = service("api");
const worker = service("worker");
const backend = group("Backend", [api, worker]);
```

Bucket:

```ts
const media = bucket("media", { region: "iad" });
```

Project shape:

```ts
export default defineRailway(() => {
  const db = postgres("postgres");
  const web = service("web", {
    env: {
      DATABASE_URL: db.env.DATABASE_URL,
    },
  });

  return project("my-app", {
    resources: [db, web],
  });
});
```

## Review checklist

Before applying changes, confirm:

- The user has reviewed the latest `railway config plan` output.
- `railway config plan` shows only expected changes.
- Secrets are not replaced with literal placeholder values.
- Existing Railway-managed variables are omitted or use `preserve()` when the value should remain untouched.
- Custom domains are declared with `domains`, not networking internals.
- Scaling is declared with `replicas`, not `multiRegionConfig`.
- No generated Railway service domains are committed.
