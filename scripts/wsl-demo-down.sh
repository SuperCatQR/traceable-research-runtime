#!/usr/bin/env bash
set -euo pipefail

podman stop traceable-search-demo-host >/dev/null 2>&1 || true
echo "traceable-search demo host stopped; shared dependencies remain running"
