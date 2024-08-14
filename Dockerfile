# This dockerfile is meant to be used in publish-docker.yml workflow which reuses artifacts created by release.yml
# This way we can avoid rebuilding the CLI specifically for docker

FROM alpine:3.20

# binary should be retrieved by download-artifact action by this point
COPY --chmod=755 ./railway /usr/bin/railway
