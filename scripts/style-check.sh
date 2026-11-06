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
#   5. verve-dom's legacy helpers in src/bind/mod.rs (`text`, `list`, `array`, `arg_string`,
#      copies of verve::native with other signatures; style.md §1) gain no callers: the lines
#      that call them, counted per file, may not pass ci/style/legacy-calls.txt, and a file not
#      listed there may not call them at all.
#   6. A realm is entered only in src/world.rs (`verve::in_realm` / `in_world`, which put the
#      previous realm back); everyone else calls those.
#
# `scripts/style-check.sh --baseline` rewrites the three lists from the tree as it is: run it
# only when a refactor shrinks them, never to let a new violation in.
set -euo pipefail
cd "$(dirname "$0")/.."

MAX_LINES=1500
LARGE=ci/style/large-files.txt
COPIES=ci/style/toolkit-copies.txt
LEGACY=ci/style/legacy-calls.txt
LEGACY_HOME=crates/verve-dom/src/bind/mod.rs
LEGACY_HELPERS='text|list|array|arg_string'
# Each helper has one home: the native-function toolkit in src/native.rs, the encoders in
# src/encoding.rs. A definition of one of these names anywhere else is a copy.
NATIVE='arg|js_text|text_of|type_error|range_error|coded_error|coded_error_value|object|function|closure|hidden|methods|exports|accessors|get|invoke|list_of|array|global|class|instance'
ENCODING='hex|base64|unbase64|percent_decode|decode_base64|data_url'

# Only the folders that exist: a crate without tests/ is not an error.
existing() { for d in "$@"; do [ -d "$d" ] && echo "$d"; done; return 0; }
# The folder names hold no spaces, so the list splits into find's arguments as it is.
sources() { find $(existing src tests crates/*/src crates/*/tests) -name '*.rs' | sort; }
library_sources() { find $(existing src crates/*/src) -name '*.rs' | sort; }

# Each check reads every file with one command rather than one command per file: on Windows a
# process costs Git Bash a fork, and a few thousand of them per commit stall the machine.

# "path lines" for each file longer than MAX_LINES; `wc` also prints "N total" lines, dropped.
large_files() {
    sources | tr '\n' '\0' | xargs -0 -r wc -l |
        awk -v max="$MAX_LINES" '$2 != "total" && $1 > max { print $2, $1 }'
    return 0
}

# "path:fn name(" for each definition of one of `names` in the files named on stdin.
definitions() {
    local names=$1
    xargs -0 -r grep -oHE "^(pub(\([a-z]+\))? )?fn ($names)\b *[<(]" || true
}

toolkit_copies() {
    {
        library_sources | grep -xF src/native.rs | tr '\n' '\0' | definitions "$ENCODING"
        library_sources | grep -xF src/encoding.rs | tr '\n' '\0' | definitions "$NATIVE"
        library_sources | grep -vxE 'src/(native|encoding)\.rs' | tr '\n' '\0' |
            definitions "$NATIVE|$ENCODING"
    } | sed -E 's/^([^:]+):.*fn ([a-z_0-9]+).*/\1 \2/' | sort -u -k1,1 -k2,2
    return 0
}

# "path lines" for each verve-dom source with lines that call a legacy helper: a bare or
# `bind::` call, not a method (`.text(`) nor another module's function (`x::text(`).
legacy_calls() {
    library_sources | grep '^crates/verve-dom/src/' | grep -vxF "$LEGACY_HOME" | tr '\n' '\0' |
        { xargs -0 -r grep -HE "(^|[^A-Za-z0-9_.:]|bind::)($LEGACY_HELPERS)\(" || true; } |
        { grep -vE ":[^:]*fn ($LEGACY_HELPERS)\(" || true; } | awk -F: '{ count[$1]++ } END { for (f in count) print f, count[f] }' | sort
    return 0
}

# Appends to each "path count" line on stdin the ceiling the table $1 gives that path, or "-".
with_ceiling() {
    awk -v table="$1" '
        BEGIN { while ((getline line < table) > 0) { split(line, field, " "); ceiling[field[1]] = field[2] } }
        { print $1, $2, ($1 in ceiling ? ceiling[$1] : "-") }'
}

if [ "${1:-}" = "--baseline" ]; then
    mkdir -p ci/style
    large_files > "$LARGE"
    toolkit_copies > "$COPIES"
    legacy_calls > "$LEGACY"
    echo "wrote $LARGE ($(wc -l < "$LARGE") files), $COPIES ($(wc -l < "$COPIES") copies) and $LEGACY ($(wc -l < "$LEGACY") files)"
    exit 0
fi

failed=0
fail() { echo "style: $*" >&2; failed=1; }

# 1. File length. Each long file is joined to its ceiling in LARGE ("-" when it has none).
while read -r f n ceiling; do
    if [ "$ceiling" = "-" ]; then
        fail "$f has $n lines (new files: at most $MAX_LINES; split it by responsibility)"
    elif [ "$n" -gt "$ceiling" ]; then
        fail "$f grew to $n lines (it may not grow past $ceiling; move the new code to a module of its own)"
    fi
done < <(large_files | with_ceiling "$LARGE")

# 2. One toolkit. A copy not listed in COPIES is new.
while read -r f name; do
    fail "$f defines its own \`$name\`: use verve::native / verve::encoding, or add what is missing there"
done < <(toolkit_copies | grep -vxFf "$COPIES" || true)

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

# 5. No new callers of verve-dom's legacy helpers.
while read -r f n ceiling; do
    if [ "$ceiling" = "-" ]; then
        fail "$f calls verve-dom's legacy helpers ($LEGACY_HELPERS) on $n line(s): use verve::native (style.md §1)"
    elif [ "$n" -gt "$ceiling" ]; then
        fail "$f calls verve-dom's legacy helpers on $n line(s), more than its $ceiling: use verve::native (style.md §1)"
    fi
done < <(legacy_calls | with_ceiling "$LEGACY")

# 6. Realms are entered only through verve::in_realm / in_world.
while IFS= read -r hit; do
    fail "$hit (enter a realm with verve::in_world / in_realm, which put the previous one back)"
done < <(library_sources | grep -vxF src/world.rs | tr '\n' '\0' |
    { xargs -0 -r grep -nHE '\benter_realm\(' || true; })

if [ "$failed" -ne 0 ]; then
    echo "style: see docs/electron/style.md" >&2
    exit 1
fi
echo "style: ok"
