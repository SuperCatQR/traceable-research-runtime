#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Conversation schema v2, Clarification schema v5, and trace schema v6 do not
# replay prior wire formats. Keep the old runtime directory and data volume
# intact for historical audit while this release starts with new storage.
runtime_dir="${TRACEABLE_SERVER_RUNTIME_DIR:-$HOME/.config/traceable-search-server-demo-v5}"
runtime_env="$runtime_dir/demo-host.env"
credential_key_file="$runtime_dir/demo-credential-encryption.key"
image="localhost/traceable-search-server-demo:latest"
container="traceable-search-server-demo"
demo_data_volume="${DEMO_DATA_VOLUME:-traceable-search-server-demo-data-v5}"

demo_bind="${DEMO_BIND:-0.0.0.0:8090}"
demo_public_origin="${DEMO_PUBLIC_ORIGIN:-http://192.168.1.71:8090}"
demo_trusted_hosts="${DEMO_TRUSTED_HOSTS:-192.168.1.71}"
demo_trusted_origins="${DEMO_TRUSTED_ORIGINS:-$demo_public_origin}"
demo_network="${DEMO_CONTAINER_NETWORK:-apps-net}"
demo_publish_address="${DEMO_PUBLISH_ADDRESS:-192.168.1.71}"
search_base_url="${SEARCH_BASE_URL:-http://searxng:8080/}"
crawl4ai_base_url="${CRAWL4AI_BASE_URL:-http://crawl4ai:11235/}"

for dependency in searxng crawl4ai; do
  if ! podman container exists "$dependency"; then
    echo "missing Podman container: $dependency" >&2
    exit 1
  fi
  if [[ "$(podman inspect --format '{{.State.Running}}' "$dependency")" != "true" ]]; then
    podman start "$dependency" >/dev/null
  fi
done

if ! podman network exists "$demo_network"; then
  echo "missing Podman network: $demo_network" >&2
  exit 1
fi

# A running prior release owns this port until the replacement image is built.
if ss -ltn "sport = :${demo_bind##*:}" | grep -q LISTEN; then
  if ! podman container exists "$container" \
    || [[ "$(podman inspect --format '{{.State.Running}}' "$container")" != "true" ]] \
    || ! podman port "$container" | grep -Fq "$demo_publish_address:${demo_bind##*:}"; then
    echo "DEMO_BIND port is already listening: $demo_bind" >&2
    exit 1
  fi
fi

install -d -m 700 "$runtime_dir"
umask 077
if [[ ! -s "$credential_key_file" ]]; then
  python3 -c 'import base64, secrets; print(base64.b64encode(secrets.token_bytes(32)).decode())' \
    > "$credential_key_file"
fi
chmod 600 "$credential_key_file"

# Do not print the existing crawl service token. An empty token is valid for
# deployments where crawl4ai does not require authentication.
crawl4ai_token="${CRAWL4AI_TOKEN:-}"
if [[ -z "$crawl4ai_token" ]]; then
  crawl4ai_token="$(podman inspect --format '{{range .Config.Env}}{{println .}}{{end}}' crawl4ai \
    | sed -n 's/^CRAWL4AI_API_TOKEN=//p' | head -n 1)"
fi

{
  printf 'SEARCH_BASE_URL=%s\n' "$search_base_url"
  printf 'CRAWL4AI_BASE_URL=%s\n' "$crawl4ai_base_url"
  printf 'CRAWL4AI_TOKEN=%s\n' "$crawl4ai_token"
  printf 'TRACEABLE_SEARCH_DATA_DIR=/data\n'
  printf 'DEMO_CATALOG_PATH=/data/demo-catalog.sqlite\n'
  printf 'DEMO_CREDENTIAL_ENCRYPTION_KEY=%s\n' "$(<"$credential_key_file")"
  printf 'DEMO_MAX_CONCURRENT_RESEARCH=%s\n' "${DEMO_MAX_CONCURRENT_RESEARCH:-2}"
  printf 'DEMO_BIND=%s\n' "$demo_bind"
  printf 'DEMO_ALLOW_NETWORK_BIND=true\n'
  printf 'DEMO_TRUSTED_HOSTS=%s\n' "$demo_trusted_hosts"
  printf 'DEMO_TRUSTED_ORIGINS=%s\n' "$demo_trusted_origins"
  printf 'DEMO_SECURE_COOKIES=%s\n' "${DEMO_SECURE_COOKIES:-false}"
  printf 'DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS=true\n'
  # Workspace users provide their own Model Profile. These placeholders keep
  # process-level configuration valid without copying a local API credential.
  printf 'STRONG_MODEL_BASE_URL=%s\n' "${STRONG_MODEL_BASE_URL:-http://127.0.0.1:3000/v1/}"
  printf 'STRONG_MODEL_API_KEY=%s\n' "${STRONG_MODEL_API_KEY:-workspace-profile-required}"
  printf 'STRONG_MODEL_ID=%s\n' "${STRONG_MODEL_ID:-workspace-profile-required}"
} > "$runtime_env"
chmod 600 "$runtime_env"

podman build --tag "$image" --file "$root_dir/demo-host/Containerfile" "$root_dir"
podman volume exists "$demo_data_volume" \
  || podman volume create "$demo_data_volume" >/dev/null
podman rm --force "$container" >/dev/null 2>&1 || true
podman run --detach \
  --name "$container" \
  --restart unless-stopped \
  --network "$demo_network" \
  --publish "$demo_publish_address:${demo_bind##*:}:${demo_bind##*:}" \
  --env-file "$runtime_env" \
  --volume "$demo_data_volume:/data:Z" \
  "$image" >/dev/null

echo "Demo: $demo_public_origin"
echo "Health: $demo_public_origin/api/health"
echo "Trace summary: /api/conversations/{conversation_id}/turns/{turn_id}/trace/summary"
echo "Trace audit: /api/conversations/{conversation_id}/turns/{turn_id}/trace/audit?stage=&cursor=&limit="
