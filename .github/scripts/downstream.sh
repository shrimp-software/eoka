#!/usr/bin/env bash
# Build and test eoka-tools against this checkout of eoka instead of the
# crates.io release, so breaking changes surface before eoka is published.
# Usage: downstream.sh <eoka-tools checkout>
set -euo pipefail

eoka_dir=$(pwd)
tools_dir=$1
version=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "eoka") | .version')

cd "$tools_dir"
# Point every eoka requirement at this version so the patch applies even
# across a semver-incompatible bump.
grep -rl --include=Cargo.toml -E '^eoka = ' crates | while read -r manifest; do
  sed -i -E \
    -e "s/^eoka = \"[^\"]+\"/eoka = \"=${version}\"/" \
    -e "s/^eoka = \{ version = \"[^\"]+\"/eoka = { version = \"=${version}\"/" \
    "$manifest"
done
mkdir -p .cargo
cat > .cargo/config.toml <<TOML
[patch.crates-io]
eoka = { path = "${eoka_dir}" }
TOML

cargo update -p eoka
resolved=$(cargo tree -i eoka -e normal --depth 0 --all-features)
echo "Resolved: ${resolved}"
if [[ "$resolved" != *"(${eoka_dir})"* ]]; then
  echo "::error::eoka-tools did not build against this eoka checkout"
  exit 1
fi

cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
