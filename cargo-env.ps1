# Optional cargo wrapper for the windows-gnu toolchain. Not needed with the default
# MSVC toolchain (rustup + Visual Studio Build Tools): plain `cargo build` works there.
#   .\cargo-env.ps1 build --release
#   .\cargo-env.ps1 test
#
# windows-gnu needs two things the rustup toolchain does not ship:
# - A GNU dlltool together with `as` on PATH, for the `windows` crate's raw-dylib imports
#   (rustup's bundled dlltool has no assembler). MSYS2: `pacman -S mingw-w64-x86_64-binutils`,
#   then point NOTCH_DLLTOOL_DIR at C:\msys64\mingw64\bin.
# - A target dir without spaces, because dlltool fails on paths with spaces.
#
# Settings (environment variables, or set them in cargo-env.local.ps1, which is gitignored):
#   NOTCH_DLLTOOL_DIR  folder containing dlltool.exe and as.exe, prepended to PATH
#   CARGO_TARGET_DIR   build output; defaults to %LOCALAPPDATA%\notch-target when the repo path has a space
$local = Join-Path $PSScriptRoot 'cargo-env.local.ps1'
if (Test-Path $local) { . $local }
if ($env:NOTCH_DLLTOOL_DIR) { $env:Path = "$env:NOTCH_DLLTOOL_DIR;$env:Path" }
if (-not $env:CARGO_TARGET_DIR -and $PSScriptRoot.Contains(' ')) {
    $env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'notch-target'
}
Push-Location $PSScriptRoot
try { & cargo @args; exit $LASTEXITCODE } finally { Pop-Location }
