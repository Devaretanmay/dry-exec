#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${repo_root}/llms-full.txt"

{
  for file in README.md SECURITY.md CONTRIBUTING.md; do
    if [[ -f "${repo_root}/${file}" ]]; then
      printf '\n===== %s =====\n\n' "${file}"
      sed -n '1,2400p' "${repo_root}/${file}"
    fi
  done
  if [[ -d "${repo_root}/docs" ]]; then
    while IFS= read -r file; do
      printf '\n===== %s =====\n\n' "${file#"${repo_root}/"}"
      sed -n '1,2400p' "${file}"
    done < <(find "${repo_root}/docs" -type f -name '*.md' -print | sort)
  fi
} > "${output}"

echo "wrote ${output}"
