#!/usr/bin/env bash
# What a release binary weighs, against the baseline recorded in docs/electron/size.md, and
# the crates it carries. Builds exactly one configuration; CI fails past --max-bytes.
#
#   scripts/size-report.sh                                   # default features
#   scripts/size-report.sh --features wasm                   # the Electron feature set
#   scripts/size-report.sh --profile dist --max-bytes 25165824
set -euo pipefail

bin=verve
features=""
profile=release
max_bytes=0
bloat=1

while [ $# -gt 0 ]; do
    case "$1" in
        --bin) bin="$2"; shift 2 ;;
        --features) features="$2"; shift 2 ;;
        --profile) profile="$2"; shift 2 ;;
        --max-bytes) max_bytes="$2"; shift 2 ;;
        --no-bloat) bloat=0; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

feature_args=()
if [ -n "$features" ] && [ "$features" != none ]; then
    feature_args=(--no-default-features --features "$features")
elif [ "$features" = none ]; then
    feature_args=(--no-default-features)
fi

echo "building $bin ($profile) with ${features:-default features}"
cargo build --profile "$profile" --bin "$bin" "${feature_args[@]}"

binary="target/$profile/$bin"
if [ ! -f "$binary" ]; then
    binary="$binary.exe"
fi
bytes=$(wc -c < "$binary" | tr -d ' ')
mib=$(awk -v b="$bytes" 'BEGIN { printf "%.2f", b / 1048576 }')

baseline=""
if [ -f docs/electron/size.md ]; then
    baseline=$(sed -n "s/.*size-baseline $bin \([0-9][0-9]*\).*/\1/p" docs/electron/size.md | head -n 1)
fi

line="$bin ($mib MiB, $bytes bytes)"
if [ -n "$baseline" ]; then
    delta=$((bytes - baseline))
    percent=$(awk -v d="$delta" -v b="$baseline" 'BEGIN { printf "%.1f", 100 * d / b }')
    line="$line; baseline $baseline bytes, change $delta bytes ($percent%)"
fi
echo "$line"

if [ "$bloat" = 1 ]; then
    cargo bloat --profile "$profile" --bin "$bin" --crates -n 20 "${feature_args[@]}"
fi

if [ "$max_bytes" -gt 0 ] && [ "$bytes" -gt "$max_bytes" ]; then
    echo "$bin is $bytes bytes, over the $max_bytes-byte budget" >&2
    exit 1
fi
