#!/bin/sh
# Signs the JIT probe the ways an embedder might ship it and checks each one:
#
#   unsigned (the linker's ad-hoc signature)  -> the JIT compiles
#   hardened runtime + allow-jit               -> the JIT compiles (MAP_JIT pages)
#   hardened runtime, no entitlement           -> no executable memory: interpreted, same result
#
# Usage: ci/macos/jit-probe.sh <path to jit_probe>
set -eu

probe=$1
here=$(dirname "$0")
expected="result=-1297967296"
work=$(mktemp -d)

check() {
    label=$1 want=$2 bin=$3
    out=$("$bin")
    echo "$label: $out"
    case $out in
        *"$expected"*) ;;
        *) echo "::error::$label: wrong result"; exit 1 ;;
    esac
    compiled=$(printf '%s\n' "$out" | sed -n 's/.*compiled=\([0-9]*\).*/\1/p')
    if [ "$want" = compiles ] && [ "$compiled" -eq 0 ]; then
        echo "::error::$label: the JIT compiled nothing"
        exit 1
    fi
    if [ "$want" = interprets ] && [ "$compiled" -ne 0 ]; then
        echo "::error::$label: compiled without the entitlement"
        exit 1
    fi
}

check "unsigned" compiles "$probe"

cp "$probe" "$work/allow-jit"
codesign --force --sign - --options runtime --entitlements "$here/allow-jit.entitlements" "$work/allow-jit"
check "hardened, allow-jit" compiles "$work/allow-jit"

cp "$probe" "$work/no-entitlement"
codesign --force --sign - --options runtime "$work/no-entitlement"
check "hardened, no entitlement" interprets "$work/no-entitlement"
