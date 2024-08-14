FROM alpine:3.20

COPY ./railway /usr/bin/railway
RUN test -f /usr/bin/railway
