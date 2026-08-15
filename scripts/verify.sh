#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features

npm --prefix frontend ci
npm --prefix frontend audit --audit-level=moderate
npm --prefix frontend run check
npm --prefix frontend run build

docker_args=(--tag stelle:verify)
if [[ -n "${STELLE_DOCKER_NETWORK:-}" ]]; then
  docker_args+=(--network "$STELLE_DOCKER_NETWORK")
fi
docker build "${docker_args[@]}" .
scripts/container-smoke.sh stelle:verify
