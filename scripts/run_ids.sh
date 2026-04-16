#!/bin/bash
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <id_file> [--research-agent claude|codex] [extra flags...]"
  echo "  id_file: text file with one Met object ID per line"
  exit 1
fi

ID_FILE="$1"
shift

cd "$(dirname "$0")/.."

while IFS= read -r id; do
  id=$(echo "$id" | tr -d '[:space:]')
  [ -z "$id" ] && continue
  echo "=== START $id $(date) ==="
  python3 scripts/add_met_artwork_by_object_id.py --object-id "$id" --force "$@"
  echo "=== END $id $(date) ==="
done < "$ID_FILE"
