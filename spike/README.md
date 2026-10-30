# Phase 0 spike: the notch pill

Two implementations of the same pill (identical size, radius, animation curve and text) to compare RAM and CPU before choosing a stack. The Composition variant won; see [results.md](results.md).

| Folder | Stack |
|---|---|
| `pill-composition/` | Rust + Win32 + Windows.UI.Composition, text via Direct2D/DirectWrite. Animations run in DWM. The window shrinks to the pill's size when collapsed. |
| `pill-slint/` | Rust + Slint 1.18 (winit). The renderer is chosen at runtime: `SLINT_BACKEND=winit-femtovg` (OpenGL) or `winit-software`. The window stays 680×236. |

Left-click the pill to expand/collapse, right-click to quit. `--cycle <ms>` makes the pill toggle on its own.

## Build

`spike/` is a separate workspace (excluded from the main one):

```powershell
cd spike
cargo build --release
```

With the default MSVC toolchain nothing else is needed. With `x86_64-pc-windows-gnu`, follow the GNU notes in the main [README](../README.md#build-from-source) (a GNU `dlltool` with `as` on `PATH`, and a target dir without spaces); Slint's dependencies additionally need `libshlwapi.a`, which rustup's GNU toolchain does not ship. Put it in a folder and pass it with `$env:RUSTFLAGS = '-L native=<folder>'`.

## Measuring

```powershell
powershell -ExecutionPolicy Bypass -File .\measure.ps1
```

The script looks for the binaries in `$env:CARGO_TARGET_DIR\release` (or `spike\target\release`; override with `-Bin`). Results go to `results.md`: private working set, working set, private bytes, process CPU, and the extra CPU of `dwm.exe`, for an idle and an active scenario.
