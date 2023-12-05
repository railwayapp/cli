FROM rust:1-alpine3.18 as builder

WORKDIR /app

COPY . .

RUN apk add --no-cache musl-dev openssl-dev

RUN cargo build --release

FROM alpine:3.18

COPY --from=builder /app/target/release/railway /usr/bin/railway 
