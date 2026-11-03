#!/usr/bin/env bash
# The JS throughput suite and the scaling probes of docs/electron/perf-js.md, as a Markdown
# table each. Every measurement is a fresh process, repeated --runs times (default 5); the table
# shows the median and the best. A Verve binary built with the `jit` feature is measured twice,
# compiled and with `VERVE_JIT=0`; one without it has an interpreter column only.
#
#   scripts/js-bench/run.sh <verve.exe> [node]
#   scripts/js-bench/run.sh --runs 7 --only typed <verve.exe> node
#   scripts/js-bench/run.sh --scaling <verve.exe> [node]
#
# The columns are the ones docs/electron/perf-js.md's tables use, so a row pastes straight in.
# The test names are bench.js's fourteen, the probe names scaling.js's; a probe is timed at n and
# at 4n, and a ratio above 8 is super-linear, above 5 suspicious.
#
# A process the wrapper kills after 300 s counts as a timeout: a throughput cell reads "timeout",
# a scaling row is marked super-linear (which is what a run past 20 s means), and stderr says how
# many runs were lost either way.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
runs=5
only=""
scaling=0
verve=""
node_bin=""

usage() {
    echo "usage: run.sh [--runs N] [--only test] [--scaling] <verve.exe> [node]" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --runs|--only)
            [ $# -ge 2 ] || { echo "$1 needs a value" >&2; usage; }
            if [ "$1" = "--runs" ]; then runs="$2"; else only="$2"; fi
            shift 2
            ;;
        --scaling) scaling=1; shift ;;
        -h|--help) usage ;;
        -*) echo "unknown argument: $1" >&2; usage ;;
        *)
            if [ -z "$verve" ]; then verve="$1"; else node_bin="$1"; fi
            shift
            ;;
    esac
done
[ -n "$verve" ] || usage
[ -x "$verve" ] || { echo "not an executable: $verve" >&2; exit 2; }
if [ -n "$node_bin" ] && ! "$node_bin" -e 0 > /dev/null 2>&1; then
    echo "not a Node: $node_bin" >&2
    exit 2
fi

# coreutils' timeout under either name, checked by its own --version: Git Bash puts System32 on
# PATH, where timeout.exe is Windows' wait-for-N-seconds command and would silently succeed.
# macOS ships the real one as gtimeout (brew install coreutils).
if command -v timeout > /dev/null 2>&1 && timeout --version 2> /dev/null | grep -q coreutils; then
    timeout_cmd=timeout
elif command -v gtimeout > /dev/null 2>&1 && gtimeout --version 2> /dev/null | grep -q coreutils; then
    timeout_cmd=gtimeout
else
    echo "run.sh needs coreutils' timeout or gtimeout on PATH (macOS: brew install coreutils)" >&2
    exit 2
fi

# A build with the `jit` feature carries the name of its `VERVE_JIT_THRESHOLD`, and one without
# the feature does not: the variable is read only where that code compiled in.
jit=0
if grep -aq VERVE_JIT_THRESHOLD "$verve" 2> /dev/null; then jit=1; fi

# The code cache is off for every run, wherever the user's own folder is: a second run would
# otherwise measure code read back from disk instead of code compiled, and the numbers would stop
# being comparable between runs and between machines. `--no-code-cache` is Verve's flag; Node has
# nothing to turn off.
cache_flag=--no-code-cache
"$verve" run "$cache_flag" -e 0 > /dev/null 2>&1 || cache_flag=
if [ -z "$cache_flag" ]; then
    echo "run.sh: this verve has no --no-code-cache; the code cache is whatever VERVE_CODE_CACHE says" >&2
    export VERVE_CODE_CACHE=0
fi

script=bench.js
[ "$scaling" = 1 ] && script=scaling.js

# One measurement in a process of its own, and its stderr kept for the report if it fails.
timeout_s=300
stderr_log="$(mktemp "${TMPDIR:-/tmp}/js-bench-stderr.XXXXXX")"
trap 'rm -f "$stderr_log"' EXIT

run_once() {
    local engine="$1" test="$2"
    case "$engine" in
        jit) "$timeout_cmd" "$timeout_s" env VERVE_JIT= "$verve" run $cache_flag "$here/$script" "$test" ;;
        interp) "$timeout_cmd" "$timeout_s" env VERVE_JIT=0 "$verve" run $cache_flag "$here/$script" "$test" ;;
        node) "$timeout_cmd" "$timeout_s" "$node_bin" "$here/$script" "$test" ;;
    esac
}

# The runs' JSON lines, one per line; ">TIMEOUT" for a process the wrapper killed at 300 s and
# ">FAILED" for any other failure. Sampling stops at the first failure and says how many runs it
# had, so a median over fewer runs is never passed off as a median over `runs`.
samples() {
    local engine="$1" test="$2" line rc
    local -i i taken=0
    for ((i = 0; i < runs; i++)); do
        rc=0
        line=$(run_once "$engine" "$test" 2> "$stderr_log") || rc=$?
        if [ "$rc" -eq 0 ] && [ -n "$line" ]; then
            taken+=1
            echo "$line"
            continue
        fi
        if [ "$rc" -eq 124 ]; then
            echo ">TIMEOUT"
            echo "run.sh: $test ($engine) was killed after ${timeout_s}s" >&2
        else
            echo ">FAILED"
            echo "run.sh: $test ($engine) exited $rc:" >&2
            sed 's/^/  /' "$stderr_log" >&2
        fi
        echo "run.sh: note: $test ($engine) stopped after $taken of $runs runs" >&2
        return
    done
}

# A number out of the JSON line: the named field, or the test's only value.
field_of() {
    if [ -n "$2" ]; then
        sed -nE "s/.*\"$2\":\"?([0-9.]+)\"?.*/\1/p" <<< "$1" | head -n 1
    else
        sed -nE 's/.*:([0-9.]+).*/\1/p' <<< "$1" | head -n 1
    fi
}

# Median and best of the numbers given, one per line; "- -" for none.
stats() {
    awk '
        /^[0-9.]+$/ { v[n++] = $1 }
        END {
            if (n == 0) { print "- -"; exit }
            for (i = 1; i < n; i++) {
                x = v[i]
                for (j = i - 1; j >= 0 && v[j] > x; j--) v[j + 1] = v[j]
                v[j + 1] = x
            }
            median = (n % 2) ? v[(n - 1) / 2] : (v[n / 2 - 1] + v[n / 2]) / 2
            printf "%.1f %.1f\n", median, v[0]
        }'
}

# "median best" of one engine's runs of one test, from a field of each JSON line. A test whose
# every run was killed has no number: "timeout timeout", which `cell` prints as `timeout`.
measure() {
    local engine="$1" test="$2" field="$3" line
    local -a values=()
    local timed_out=no
    while IFS= read -r line; do
        case "$line" in
            ">TIMEOUT") timed_out=yes; continue ;;
            ">FAILED") continue ;;
        esac
        values+=("$(field_of "$line" "$field")")
    done < <(samples "$engine" "$test")
    if [ "${#values[@]}" -eq 0 ]; then
        if [ "$timed_out" = yes ]; then echo "timeout timeout"; else echo "- -"; fi
    else
        printf '%s\n' "${values[@]}" | stats
    fi
}

# A probe's two timings and its size, from one set of runs: every run answers both t(n) and t(4n).
# The last field is "yes" when the probe itself reported a run past its 20 s limit, or when its
# process was killed — the package counts both as super-linear whatever the ratio would have been.
probe_of() {
    local engine="$1" probe="$2" line
    local -a small=() wide=()
    local over=no
    while IFS= read -r line; do
        case "$line" in
            ">TIMEOUT") over=yes; continue ;;
            ">FAILED") continue ;;
            *'"superLinear":true'*) over=yes ;;
        esac
        n=$(field_of "$line" n)
        small+=("$(field_of "$line" t1)")
        wide+=("$(field_of "$line" t4)")
    done < <(samples "$engine" "$probe")
    if [ "${#small[@]}" -eq 0 ]; then
        echo "- - - - - $over"
        return
    fi
    echo "${n:--} $(printf '%s\n' "${small[@]}" | stats) $(printf '%s\n' "${wide[@]}" | stats) $over"
}

# A ratio of two numbers as the document writes it, "56×" or "3.97×"; "-" when either is missing.
ratio() {
    case "$1$2" in *-*) echo "-"; return ;; esac
    awk -v a="$1" -v b="$2" 'BEGIN {
        if (b == 0) { print "-"; exit }
        r = a / b
        if (r >= 10) printf "%.0f×\n", r
        else printf "%.1f×\n", r
    }'
}

cell() {
    local median="${1%% *}"
    case "$median" in
        timeout) echo "timeout" ;;
        -) echo "-" ;;
        *) echo "$1" ;;
    esac
}

# The header of a table whose columns are `names`, given as Markdown cells without the bars.
header() {
    local cell
    printf '|'
    for cell in "$@"; do printf ' %s |' "$cell"; done
    printf '\n|'
    for cell in "$@"; do printf '%s|' '---'; done
    printf '\n'
}

names="$("$verve" run $cache_flag "$here/$script" --list | sed -E 's/^\[//; s/\]$//; s/"//g; s/,/ /g')"
if [ -n "$only" ]; then
    case " $names " in
        *" $only "*) names="$only" ;;
        *) echo "unknown test: $only (one of: $names)" >&2; exit 2 ;;
    esac
fi

echo "verve: $verve (built with the JIT feature: $([ "$jit" = 1 ] && echo yes || echo no))"
echo "node: ${node_bin:-not given} · runs: $runs per test and engine · median (best), ms"
echo

if [ "$scaling" = 1 ]; then
    engines="interp"
    [ "$jit" = 1 ] && engines="jit interp"
    [ -n "$node_bin" ] && engines="$engines node"
    # The document's table, which holds the interpreter's own numbers; the detail table below
    # has every engine.
    header probe n 't(n)' 't(4n)' ratio verdict
    details=()
    for probe in $names; do
        ratios=""
        n="-"
        over=no
        cells=()
        for engine in $engines; do
            read -r size t1_median t1_best t4_median t4_best self_over <<< "$(probe_of "$engine" "$probe")"
            [ "$engine" = interp ] && n="${size:--}"
            [ "$self_over" = yes ] && over=yes
            cells+=("$t1_median ($t1_best)" "$t4_median ($t4_best)" "$(ratio "$t4_median" "$t1_median")")
            # The verdict is Verve's: n is sized for its interpreter, so Node's ratio here
            # compares runs of a few milliseconds at most.
            [ "$engine" = node ] || ratios="$ratios $(ratio "$t4_median" "$t1_median")"
        done
        if [ "$over" = yes ]; then
            verdict="**super-linear**"
        else
            verdict=$(awk -v list="$ratios" 'BEGIN {
                worst = 0
                count = split(list, r, " ")
                for (i = 1; i <= count; i++) { v = r[i]; sub("×$", "", v); if (v != "-" && v + 0 > worst) worst = v + 0 }
                if (worst == 0) print "skipped"
                else if (worst > 8) print "**super-linear**"
                else if (worst > 5) print "suspicious"
                else print "linear" }')
        fi
        # The document's row takes the interpreter's t(n), t(4n) and ratio; `interp` is the
        # second engine in the list when the build has the JIT, and the first when it does not.
        interp_at=0
        [ "$jit" = 1 ] && interp_at=3
        row="| \`$probe\` | $n | $(cell "${cells[$interp_at]}") | $(cell "${cells[$interp_at + 1]}") | ${cells[$interp_at + 2]} | $verdict |"
        echo "$row"
        details+=("| \`$probe\` | $n | $(printf '%s | ' "${cells[@]}")")
    done

    echo
    echo "detail, every engine (median (best), ms):"
    detail_columns=(probe n)
    for engine in $engines; do
        detail_columns+=("$engine t(n)" "$engine t(4n)" "$engine ratio")
    done
    header "${detail_columns[@]}"
    printf '%s\n' "${details[@]}"
    exit 0
fi

columns=('test')
[ -n "$node_bin" ] && columns+=('Node')
[ "$jit" = 1 ] && columns+=('Verve JIT')
columns+=('Verve interp')
[ "$jit" = 1 ] && [ -n "$node_bin" ] && columns+=('JIT / Node')
[ -n "$node_bin" ] && columns+=('interp / Node')
header "${columns[@]}"

for test in $names; do
    row="| \`$test\` |"
    node_median="-"
    if [ -n "$node_bin" ]; then
        read -r node_median node_best <<< "$(measure node "$test" "")"
        row="$row $(cell "$node_median ($node_best)") |"
    fi
    jit_median="-"
    if [ "$jit" = 1 ]; then
        read -r jit_median jit_best <<< "$(measure jit "$test" "")"
        row="$row $(cell "$jit_median ($jit_best)") |"
    fi
    read -r interp_median interp_best <<< "$(measure interp "$test" "")"
    row="$row $(cell "$interp_median ($interp_best)") |"
    [ "$jit" = 1 ] && [ -n "$node_bin" ] && row="$row $(ratio "$jit_median" "$node_median") |"
    [ -n "$node_bin" ] && row="$row $(ratio "$interp_median" "$node_median") |"
    echo "$row"
done
