#!/usr/bin/env bash
set -euo pipefail
preview_project_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$preview_project_root"
preview_port="${PREVIEW_PORT:-8081}"
preview_root="${PREVIEW_ROOT:-$preview_project_root/data/content-preview}"
if [[ "${1:-}" != "--no-build" ]]; then
    cargo build -p revaro-web --target wasm32-unknown-unknown
    mkdir -p dist/content-preview-web
    wasm-bindgen --target web --out-dir dist/content-preview-web \
        --out-name revaro_web --no-typescript \
        target/wasm32-unknown-unknown/debug/revaro_web.wasm
    cp -a crates/revaro-web/static/. dist/content-preview-web/
    cargo build -p revaro-server
fi
mkdir -p "$preview_root"
export APP_ADDR="127.0.0.1:$preview_port"
export APP_BASE_URL="http://localhost:$preview_port"
export APP_DATA_DIR="$preview_root/database"
export APP_OBJECTS_DIR="$preview_root/objects"
export APP_CACHES_DIR="$preview_root/caches"
export APP_WEB_DIR="$preview_project_root/dist/content-preview-web"
export ADMIN_USERNAME=admin
export ADMIN_PASSWORD="${PREVIEW_PASSWORD:-revaro-preview-2026}"
exec "$preview_project_root/target/debug/revaro"
