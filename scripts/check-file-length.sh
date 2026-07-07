#!/bin/bash

set -euo pipefail

paths=()
for path in crates src tests; do
  if [ -d "$path" ]; then
    paths+=("$path")
  fi
done

if [ ${#paths[@]} -eq 0 ]; then
  exit 0
fi

find "${paths[@]}" -name '*.rs' -print0 |
  xargs -0 wc -l |
  awk '$1 > 700 && $2 != "total" { print "file too large:", $2, $1 " lines"; bad=1 } END { exit bad }'
