#!/usr/bin/env bash
set -euo pipefail
# Publish the compose runtime bundle + manifest to aleph.im STORE.
# Requires: nix, jq, and the aleph CLI (aleph-rs) with a funded identity.
# Usage: scripts/publish-compose-runtime.sh [extra aleph flags, e.g. --channel X]

cd "$(dirname "$0")/.."

# mktemp'd below, once we know the manifest's final name; declare here so the
# trap can safely no-op if we exit before that point.
manifest=""
cleanup() {
    [[ -n "$manifest" ]] && rm -f "$manifest"
}
trap cleanup EXIT

out=$(nix build ./nix#vprogram-compose-bundle --print-out-paths --no-link)
bundle="$out/bundle.tar.gz"
template="$out/manifest.template.json"

# The aleph CLI auto-selects the storage engine by file size (native
# `storage` up to 100 MiB, `ipfs` above). fetch_bundle_artifacts (aleph-rs
# SDK) always downloads the bundle by its `bundle.sha256` against native
# storage, so an IPFS-engine upload would publish a runtime manifest that
# points at content the CLI can never fetch. Force native storage
# explicitly: an oversized bundle then fails loudly here at publish time
# instead of silently publishing an unfetchable runtime.
echo "uploading bundle ($(stat -c%s "$bundle") bytes)..." >&2
bundle_upload=$(aleph file upload "$bundle" --storage-engine storage --json "$@")
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
manifest_msg=$(aleph file upload "$manifest" --storage-engine storage --json "$@" | jq -r '.item_hash')

echo "runtime manifest published. Use with:" >&2
echo "$manifest_msg"
