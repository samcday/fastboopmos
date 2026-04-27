#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"
fastboop_root="${repo_root}/fastboop"

if [[ ! -f "${fastboop_root}/Cargo.toml" ]]; then
  cargo "$@"
  exit 0
fi

temp_dir="$(mktemp -d)"
cleanup() {
  rm -rf -- "${temp_dir}"
}
trap cleanup EXIT

config_path="${temp_dir}/config.local.toml"
link_root="${temp_dir}/fastboop"
ln -s "${fastboop_root}" "${link_root}"

{
  echo "[patch.crates-io]"
  for crate_name in fastboop-bootpro fastboop-core fastboop-schema; do
    crate_dir="${link_root}/crates/${crate_name}"
    [[ -f "${crate_dir}/Cargo.toml" ]] || continue
    echo "${crate_name} = { path = \"${crate_dir}\" }"
  done
} > "${config_path}"

cargo --config "${config_path}" "$@"
