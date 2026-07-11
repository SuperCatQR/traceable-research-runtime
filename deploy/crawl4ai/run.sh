#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

image=localhost/traceable-search-crawl4ai:0.9.1
name=traceable-search-crawl4ai
token_file=$PWD/.api-token

if [[ ! -s "$token_file" ]]; then
  umask 077
  python3 -c 'import secrets; print(secrets.token_hex(32))' > "$token_file"
fi

podman build -t "$image" -f Containerfile .
podman rm -f "$name" >/dev/null 2>&1 || true
podman run -d --name "$name" \
  --cap-add NET_ADMIN \
  --publish 127.0.0.1:11235:11235 \
  --mount "type=bind,src=$token_file,dst=/run/secrets/api_token,ro=true" \
  "$image"

echo "crawl4ai starting at http://127.0.0.1:11235; token: $token_file"
