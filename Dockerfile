# syntax=docker/dockerfile:1

FROM node:lts-alpine AS frontend
RUN apk upgrade --no-cache && npm install --global npm@latest
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run check && npm run build

FROM rust:alpine AS backend
RUN apk upgrade --no-cache && apk add --no-cache build-base cmake
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release

FROM alpine:3
RUN apk upgrade --no-cache && apk add --no-cache ca-certificates
RUN addgroup -S stelle && adduser -S -G stelle -h /app stelle
WORKDIR /app
COPY --from=backend /build/target/release/stelle /usr/local/bin/stelle
COPY --from=frontend /build/frontend/build /app/public
COPY config-example /config
RUN chown -R stelle:stelle /app /config

USER stelle
ENV STELLE_PORT=8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${STELLE_PORT}/healthz" || exit 1
ENTRYPOINT ["/usr/local/bin/stelle"]
