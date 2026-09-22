#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${repo_root}/target/release-wheels"
python_bin="${PYTHON_BIN:-python3}"

cd "${repo_root}"
mkdir -p "${output_dir}"

if ! command -v maturin >/dev/null 2>&1; then
  "${python_bin}" -m pip install maturin
fi

maturin build --release --interpreter "${python_bin}" --out "${output_dir}"
test -n "$(find "${output_dir}" -maxdepth 1 -name '*.whl' -print -quit)"
echo "wheel-build-ok"
