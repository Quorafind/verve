#!/usr/bin/env bash
# The style rules a machine can check (docs/electron/style.md §10), over the crates in this
# workspace (never boa/, which is upstream's):
#
#   1. File length: a new file has at most 1500 lines; a file already longer is listed in
#      ci/style/large-files.txt with its length then, and may not grow past it.
#   2. One toolkit: the helpers in src/native.rs and src/encoding.rs are defined there only.
#      Copies that predate the rule are listed in ci/style/toolkit-copies.txt until the refactor
#      gates remove them; a new copy fails.
#   3. No dbg!, todo! or unimplemented! in library code; a TODO comment names its package (P31: …).
#   4. Every new crate (verve-electron, verve-gpui, verve-layout) takes the workspace lints.
#
# `scripts/style-check.sh --baseline` rewrites both lists from the tree as it is: run it only
# when a refactor shrinks them, never to let a new violation in.
set -euo pipefail
cd "$(dirname "$0")/.."

MAX_LINES=1500
LARGE=ci/style/large-files.txt
COPIES=ci/style/toolkit-copies.txt
TOOLKIT='arg|js_text|text_of|type_error|range_error|coded_error|coded_error_value|object|function|closure|hidden|methods|exports|accessors|get|invoke|list_of|array|global|class|instance|base64|unbase64|percent_decode|decode_base64'

# Only the folders that exist: a crate without tests/ is not an error.
existing() { for d in "$@"; do [ -d "$d" ] && echo "$d"; done; return 0; }
sources() { existing src tests crates/*/src crates/*/tests | xargs -I{} find {} -name '*.rs' | sort; }
library_sources() { existing src crates/*/src | xargs -I{} find {} -name '*.rs' | sort; }

large_files() {
    sources | while read -r f; do
        n=$(wc -l < "$f" | tr -d ' ')
        if [ "$n" -gt "$MAX_LINES" ]; then echo "$f $n"; fi
    done
    return 0
}

toolkit_copies() {
    library_sources | grep -v -e '^src/native.rs$' -e '^src/encoding.rs$' | while read -r f; do
        { grep -oE "^(pub(\([a-z]+\))? )?fn ($TOOLKIT)\b *[<(]" "$f" || true; } |
            sed -E "s/^.*fn ([a-z_0-9]+).*/\1/" | sort -u | sed "s|^|$f |"
    done
    return 0
}

if [ "${1:-}" = "--baseline" ]; then
    mkdir -p ci/style
    large_files > "$LARGE"
    toolkit_copies > "$COPIES"
    echo "wrote $LARGE ($(wc -l < "$LARGE") files) and $COPIES ($(wc -l < "$COPIES") copies)"
    exit 0
fi

failed=0
fail() { echo "style: $*" >&2; failed=1; }

# 1. File length.
while read -r f n; do
    ceiling=$(awk -v f="$f" '$1 == f { print $2 }' "$LARGE")
    if [ -z "$ceiling" ]; then
        fail "$f has $n lines (new files: at most $MAX_LINES; split it by responsibility)"
    elif [ "$n" -gt "$ceiling" ]; then
        fail "$f grew to $n lines (it may not grow past $ceiling; move the new code to a module of its own)"
    fi
done < <(large_files)

# 2. One toolkit.
while read -r f name; do
    grep -qxF "$f $name" "$COPIES" ||
        fail "$f defines its own \`$name\`: use verve::native / verve::encoding, or add what is missing there"
done < <(toolkit_copies)

# 3. Banned macros and anonymous TODOs.
while IFS= read -r hit; do
    fail "$hit (no dbg!/todo!/unimplemented! in library code)"
done < <(library_sources | xargs grep -nE '\b(dbg|todo|unimplemented)!\(' || true)
while IFS= read -r hit; do
    fail "$hit (a TODO names the package that owns it: \`// P31: …\`)"
done < <(sources | xargs grep -nE '(//|/\*).*\b(TODO|FIXME|XXX)\b' | grep -vE '\bP[0-9]{2}\b' || true)

# 4. New crates take the workspace lints.
for crate in verve-electron verve-gpui verve-layout; do
    manifest="crates/$crate/Cargo.toml"
    [ -f "$manifest" ] || continue
    grep -qE '^\[lints\]' "$manifest" && grep -qE '^workspace *= *true' "$manifest" ||
        fail "$manifest must have \`[lints] workspace = true\`"
done

if [ "$failed" -ne 0 ]; then
    echo "style: see docs/electron/style.md" >&2
    exit 1
fi
echo "style: ok"
