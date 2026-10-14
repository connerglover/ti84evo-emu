#!/bin/sh
# Build the armv8m end-to-end test programs into tools/testprogs/out with the Arm GNU Toolchain
# (arm-none-eabi-* on PATH). The images are committed so that `cargo test` needs no toolchain;
# CI rebuilds them and fails if they differ from the committed ones.
#
# Each program yields <name>.bin (Secure image, loaded at 0x1000_0000) and, if it has
# Non-secure code, <name>_ns.bin (loaded at 0x0020_0000).
set -eu
cd "$(dirname "$0")"

# name:parts, where parts is "s" (Secure only) or "s+ns".
PROGRAMS="exc_basic:s tz_roundtrip:s+ns systick:s+ns"

mkdir -p build out
for entry in $PROGRAMS; do
    name=${entry%%:*}
    parts=${entry#*:}
    arm-none-eabi-gcc -mcpu=cortex-m33 -mthumb -mcmse -c -o "build/$name.o" "$name.S"
    arm-none-eabi-ld -T link.ld --no-warn-rwx-segments -o "build/$name.elf" "build/$name.o"
    arm-none-eabi-objcopy -O binary -j .s_text -j .s_nsc "build/$name.elf" "out/$name.bin"
    if [ "$parts" = "s+ns" ]; then
        arm-none-eabi-objcopy -O binary -j .ns_text "build/$name.elf" "out/${name}_ns.bin"
    fi
    echo "built $name"
done
