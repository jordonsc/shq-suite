#!/usr/bin/env bash
# Home Assistant API helper
# Usage:
#   ha get /api/states
#   ha get /api/states/light.living_downlights
#   ha post /api/services/light/turn_on '{"entity_id": "light.living_downlights"}'

set -euo pipefail

if [[ -z "${HA_URL:-}" || -z "${HA_TOKEN:-}" ]]; then
    echo "Error: HA_URL and HA_TOKEN must be set in environment" >&2
    exit 1
fi

method="${1:?Usage: ha <get|post> <path> [json_body]}"
path="${2:?Usage: ha <get|post> <path> [json_body]}"
body="${3:-}"

method=$(echo "$method" | tr '[:lower:]' '[:upper:]')

args=(
    -s
    -X "$method"
    -H "Authorization: Bearer $HA_TOKEN"
    -H "Content-Type: application/json"
)

if [[ -n "$body" ]]; then
    args+=(-d "$body")
fi

curl "${args[@]}" "${HA_URL}${path}"
