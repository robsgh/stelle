# syntax=docker/dockerfile:1.7

FROM node:24-alpine AS frontend
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run check && npm run build

FROM rust:1.95-alpine AS backend
RUN apk add --no-cache build-base cmake
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM alpine:3.23
RUN addgroup -S stelle && adduser -S -G stelle -h /app stelle
WORKDIR /app
COPY --from=backend /build/target/release/stelle /usr/local/bin/stelle
COPY --from=frontend /build/frontend/build /app/public
COPY config /config
RUN chown -R stelle:stelle /app /config

USER stelle
EXPOSE 8080
ENV STELLE_CONFIG=/config/dashboard.yaml \
    STELLE_STATIC_DIR=/app/public \
    STELLE_LISTEN=0.0.0.0:8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/stelle"]
