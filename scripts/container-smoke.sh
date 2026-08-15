#!/usr/bin/env bash
set -euo pipefail

image="${1:-stelle:verify}"
port="${STELLE_SMOKE_PORT:-18080}"
container="stelle-smoke-$$-$RANDOM"
temp_dir="$(mktemp -d)"

cleanup() {
  docker rm --force "$container" >/dev/null 2>&1 || true
  rm -rf "$temp_dir"
}
trap cleanup EXIT

run_args=(--detach --name "$container")
if [[ "${STELLE_SMOKE_USE_HOST_NETWORK:-0}" == "1" ]]; then
  run_args+=(--network host --env "STELLE_PORT=$port")
else
  run_args+=(--publish "127.0.0.1:$port:8080")
fi

docker run "${run_args[@]}" "$image" >/dev/null

health_url="http://127.0.0.1:$port/healthz"
for _ in {1..40}; do
  if curl --fail --silent --show-error "$health_url" >"$temp_dir/health.json"; then
    break
  fi
  sleep 0.25
done

if ! grep --fixed-strings --quiet '"status":"ok"' "$temp_dir/health.json"; then
  docker logs "$container" >&2
  echo "health smoke check failed" >&2
  exit 1
fi

curl --fail --silent --show-error \
  "http://127.0.0.1:$port/api/dashboard" >"$temp_dir/dashboard.json"
grep --fixed-strings --quiet '"widgets"' "$temp_dir/dashboard.json"

status="$({ curl --silent --show-error \
  --request POST \
  --output "$temp_dir/error.json" \
  --write-out '%{http_code}' \
  "http://127.0.0.1:$port/api/widgets/not-found/refresh"; } 2>"$temp_dir/curl-error.log")"
if [[ "$status" != "404" ]] ||
  ! grep --fixed-strings --quiet '"code":"widget_not_found"' "$temp_dir/error.json"; then
  cat "$temp_dir/curl-error.log" >&2
  docker logs "$container" >&2
  echo "widget error smoke check failed" >&2
  exit 1
fi

echo "Container smoke checks passed for $image"
