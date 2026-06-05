# Railway configuration

This project defines its Railway infrastructure in code.

```txt
.railway/railway.ts
```

Use this file to describe the Railway project you want: services, databases, buckets, custom domains, replicas, groups, and environment variables.

## Common commands

Create the configuration files:

```bash
railway config init
```

Import an existing Railway project into code:

```bash
railway config pull
```

Preview what Railway would change:

```bash
railway config plan
```

Apply the planned changes:

```bash
railway config apply
```

Deploy this directory:

```bash
railway up
```

If `.railway/railway.ts` has pending project changes, `railway up` previews them and asks before applying them.

## Notes

- `railway config plan` is safe and does not change Railway.
- `railway config apply` asks before applying unless you pass `--yes`.
- `railway up` deploys this directory when the service has no GitHub or image source.
- Use `replicas` for scaling; advanced placement can still specify region names.
- Use `group("Name", [resources])` to keep large projects organized on the Railway canvas.
- Secrets imported from Railway may be omitted or represented with `preserve()` so they are not overwritten.
