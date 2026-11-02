#!/usr/bin/env bash
# Type-checks and lints the Linux build (x86_64-unknown-linux-gnu) from any system, the way the
# Linux `style` job in CI does, so code behind `cfg(unix)` / `cfg(windows)` is checked before
# it is pushed: an item used on one platform only is dead code on the other.
#
#   bash scripts/linux-check.sh                 # clippy over --workspace --all-targets
#   bash scripts/linux-check.sh -p verve --lib  # or any arguments for `cargo clippy`
#
# Needs the Rust target (`rustup target add x86_64-unknown-linux-gnu`) and, off Linux, Zig 0.15
# or newer (https://ziglang.org/download/), which is the C compiler and archiver for `ring`.
# `scripts/zig-cc/` holds the shims that make Zig look like gcc to cc-rs. The build goes to
# `target/linux-check`, apart from the host's own.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=x86_64-unknown-linux-gnu
die() { echo "linux-check: $*" >&2; exit 1; }

if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "$TARGET"; then
    die "the Rust target is missing: rustup target add $TARGET"
fi

host="$(uname -sm)"
if [ "$host" != "Linux x86_64" ]; then
    command -v zig >/dev/null 2>&1 || die "zig is not installed (0.15 or newer): https://ziglang.org/download/"
    version="$(zig version)"
    major="${version%%.*}"; rest="${version#*.}"; minor="${rest%%.*}"
    if [ "$major" -eq 0 ] && [ "$minor" -lt 15 ]; then
        die "zig $version is too old: 0.15 or newer is needed"
    fi
    shims="$PWD/scripts/zig-cc"
    case "$host" in
        MINGW*|MSYS*|CYGWIN*)
            VERVE_BASH="$(cygpath -w "$(command -v bash)")"
            export VERVE_BASH
            export CC_x86_64_unknown_linux_gnu="$(cygpath -w "$shims/cc.cmd")"
            export AR_x86_64_unknown_linux_gnu="$(cygpath -w "$shims/ar.cmd")"
            ;;
        *)
            export CC_x86_64_unknown_linux_gnu="$shims/cc"
            export AR_x86_64_unknown_linux_gnu="$shims/ar"
            ;;
    esac
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR_LINUX:-$PWD/target/linux-check}"
if [ "$#" -eq 0 ]; then
    set -- --workspace --all-targets
fi
exec cargo clippy --target "$TARGET" "$@" -- -D warnings
