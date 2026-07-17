#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_env="$root_dir/.env"
# Conversation schema v2, Clarification schema v5, and trace schema v6 require new storage.
# The prior generic directory and data volume remain untouched for audit.
runtime_dir="$HOME/.config/traceable-search-demo-v5"
runtime_env="$runtime_dir/demo-host.env"
credential_key_file="$runtime_dir/demo-credential-encryption.key"
image="localhost/traceable-search-demo-host:latest"
container="traceable-search-demo-host"

if [[ ! -f "$source_env" ]]; then
  echo "missing $source_env" >&2
  exit 1
fi

install -d -m 700 "$runtime_dir"
umask 077
if [[ ! -s "$credential_key_file" ]]; then
  python3 -c 'import base64, secrets; print(base64.b64encode(secrets.token_bytes(32)).decode())' \
    > "$credential_key_file"
fi
chmod 600 "$credential_key_file"
credential_encryption_key="$(<"$credential_key_file")"
{
  printf 'SEARCH_BASE_URL=http://127.0.0.1:8888/\n'
  printf 'CRAWL4AI_BASE_URL=http://127.0.0.1:11235/\n'
  printf 'TRACEABLE_SEARCH_DATA_DIR=/data\n'
  printf 'DEMO_CATALOG_PATH=/data/demo-catalog.sqlite\n'
  printf 'DEMO_CREDENTIAL_ENCRYPTION_KEY=%s\n' "$credential_encryption_key"
  printf 'DEMO_MAX_CONCURRENT_RESEARCH=2\n'
  printf 'DEMO_SECURE_COOKIES=false\n'
  printf 'DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS=true\n'
  tr -d '\r' < "$source_env" \
    | grep -E '^(CRAWL4AI_TOKEN|STRONG_MODEL_BASE_URL|STRONG_MODEL_API_KEY|STRONG_MODEL_ID)=' || true
} > "$runtime_env"
chmod 600 "$runtime_env"

for dependency in searxng traceable-search-crawl4ai; do
  if ! podman container exists "$dependency"; then
    echo "missing Podman container: $dependency; prepare the dependency using README.md first" >&2
    exit 1
  fi
  if [[ "$(podman inspect --format '{{.State.Running}}' "$dependency")" != "true" ]]; then
    podman start "$dependency" >/dev/null
  fi
done
podman build --tag "$image" --file "$root_dir/demo-host/Containerfile" "$root_dir"
podman volume exists traceable-search-demo-data-v5 \
  || podman volume create traceable-search-demo-data-v5 >/dev/null
podman rm --force "$container" >/dev/null 2>&1 || true
podman run --detach \
  --name "$container" \
  --restart unless-stopped \
  --network host \
  --env-file "$runtime_env" \
  --volume traceable-search-demo-data-v5:/data:Z \
  "$image" >/dev/null

echo "Demo: http://127.0.0.1:8080"
echo "Health: http://127.0.0.1:8080/api/health"
echo "Runtime credentials: $runtime_env (mode 0600)"
