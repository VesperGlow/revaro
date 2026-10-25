#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  echo "Usage: $0 <siglus_rs checkout> <output directory>" >&2
  exit 2
fi
siglus_script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
siglus_source_dir="$(cd -- "$1" && pwd)"
mkdir -p -- "$2"
siglus_output_dir="$(cd -- "$2" && pwd)"
siglus_revision=5f358ef1360b0439e92aed2b749e8f25d8721954
siglus_bindgen="${SIGLUS_WASM_BINDGEN:-wasm-bindgen}"

[[ "$(git -C "$siglus_source_dir" rev-parse HEAD)" == "$siglus_revision" ]] || {
  echo "Expected siglus_rs revision $siglus_revision" >&2; exit 1;
}
[[ "$("$siglus_bindgen" --version)" == "wasm-bindgen 0.2.111" ]] || {
  echo "Set SIGLUS_WASM_BINDGEN to wasm-bindgen 0.2.111" >&2; exit 1;
}
if ! git -C "$siglus_source_dir" apply --reverse --check "$siglus_script_dir/web-compat.patch" 2>/dev/null; then
  git -C "$siglus_source_dir" apply --check "$siglus_script_dir/web-compat.patch"
  git -C "$siglus_source_dir" apply "$siglus_script_dir/web-compat.patch"
fi
python3 "$siglus_script_dir/prepare-cpal.py" "$siglus_source_dir"
cd -- "$siglus_source_dir"
CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_BUILD_JOBS=2 cargo +1.98.0 build --locked --release -p game_launcher --target wasm32-unknown-unknown
"$siglus_bindgen" target/wasm32-unknown-unknown/release/game_launcher.wasm --target web --out-dir "$siglus_output_dir"
python3 "$siglus_script_dir/adapt-glue.py" "$siglus_output_dir/game_launcher.js" > "$siglus_output_dir/glue-adaptation.json"
cp LICENSE-MPL-2.0 "$siglus_output_dir/ENGINE-LICENSE.txt"
cp vendor/cpal/LICENSE "$siglus_output_dir/CPAL-LICENSE.txt"
printf '%s\n' "$siglus_revision" > "$siglus_output_dir/SOURCE_REVISION"
cd -- "$siglus_output_dir"
sha256sum game_launcher.js game_launcher_bg.wasm > SHA256SUMS
