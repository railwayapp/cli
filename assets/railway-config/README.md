# Railway configuration

This project contains a Railway configuration file:

```txt
.railway/railway.ts
```

Use it to describe the desired shape of this Railway project: services, databases, buckets, volumes, domains, and environment variables.

## Commands

Preview changes:

```bash
railway config plan
```

Stage changes for review:

```bash
railway config stage
```

Apply changes:

```bash
railway config apply
```

Deploy code:

```bash
railway up
```

If `.railway/railway.ts` has pending changes, `railway up` may ask to apply them before deploying.
