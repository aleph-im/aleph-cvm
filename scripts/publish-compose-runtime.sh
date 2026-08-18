#!/usr/bin/env bash
set -euo pipefail
# Publish the compose runtime bundle + manifest to aleph.im STORE.
# Requires: nix, jq, and the aleph CLI (aleph-rs) with a funded identity.
# Usage: scripts/publish-compose-runtime.sh [extra aleph flags, e.g. --channel X]

cd "$(dirname "$0")/.."

out=$(nix build ./nix#vprogram-compose-bundle --print-out-paths --no-link)
bundle="$out/bundle.tar.gz"
template="$out/manifest.template.json"

echo "uploading bundle ($(stat -c%s "$bundle") bytes)..." >&2
bundle_upload=$(aleph file upload "$bundle" --json "$@")
bundle_msg=$(jq -r '.item_hash' <<<"$bundle_upload")

# The manifest's sha256 doubles as the storage download key; make sure the
# local sha256 matches what nix computed (the upload is keyed by this hash).
declared_sha=$(jq -r '.bundle.sha256' "$template")
local_sha=$(sha256sum "$bundle" | cut -d' ' -f1)
if [[ "$local_sha" != "$declared_sha" ]]; then
    echo "FATAL: bundle sha256 $local_sha != manifest sha256 $declared_sha" >&2
    exit 1
fi

manifest=$(mktemp)
jq --arg ref "$bundle_msg" '.bundle.ref = $ref' "$template" > "$manifest"

echo "uploading manifest..." >&2
manifest_msg=$(aleph file upload "$manifest" --json "$@" | jq -r '.item_hash')
rm -f "$manifest"

echo "runtime manifest published. Use with:" >&2
echo "$manifest_msg"
