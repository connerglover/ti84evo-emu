# Build the armv8m end-to-end test programs into tools/testprogs/out with the Arm GNU Toolchain
# (arm-none-eabi-* on PATH). The images are committed so that `cargo test` needs no toolchain;
# CI rebuilds them (with build.sh) and fails if they differ from the committed ones.
#
# Each program yields <name>.bin (Secure image, loaded at 0x1000_0000) and, if it has
# Non-secure code, <name>_ns.bin (loaded at 0x0020_0000).
$ErrorActionPreference = 'Stop'

$programs = @(
    @{ Name = 'exc_basic'; NonSecure = $false },
    @{ Name = 'tz_roundtrip'; NonSecure = $true },
    @{ Name = 'systick'; NonSecure = $true }
)

function Invoke-Tool {
    param([string]$Tool, [string[]]$Arguments)
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Tool $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

Push-Location $PSScriptRoot
try {
    New-Item -ItemType Directory -Force build, out | Out-Null
    foreach ($p in $programs) {
        $name = $p.Name
        Invoke-Tool arm-none-eabi-gcc @('-mcpu=cortex-m33', '-mthumb', '-mcmse', '-c', '-o', "build/$name.o", "$name.S")
        Invoke-Tool arm-none-eabi-ld @('-T', 'link.ld', '--no-warn-rwx-segments', '-o', "build/$name.elf", "build/$name.o")
        Invoke-Tool arm-none-eabi-objcopy @('-O', 'binary', '-j', '.s_text', '-j', '.s_nsc', "build/$name.elf", "out/$name.bin")
        if ($p.NonSecure) {
            Invoke-Tool arm-none-eabi-objcopy @('-O', 'binary', '-j', '.ns_text', "build/$name.elf", "out/${name}_ns.bin")
        }
        Write-Host "built $name"
    }
}
finally {
    Pop-Location
}
