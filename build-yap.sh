#!/bin/sh
set -eu
cd "$(dirname "$0")"
target=aarch64-unknown-linux-musl
if [ -z "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-}" ]; then
    sysroot=$(rustc --print sysroot)
    host=$(rustc -vV | sed -n 's/^host: //p')
    linker="$sysroot/lib/rustlib/$host/bin/rust-lld"
    if [ ! -x "$linker" ]; then
        echo "AArch64 linker not found: $linker" >&2
        exit 1
    fi
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=$linker
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER
fi
cargo build --release --locked --target "$target"
mkdir -p package/bin
cp "target/$target/release/ya-ambilight" package/bin/ya-ambilight
chmod 755 package/bin/ya-ambilight
tar -C package -czf ya-ambilight.yap plugin.toml config.toml bin/ya-ambilight
echo "Built ya-ambilight.yap"
