# Notch pill spike results

Measured 2026-10-22 17:11 on Intel(R) Core(TM) i3-10105F CPU @ 3.70GHz, 8 logical threads. Warmup 8 s, sample 20 s, active scenario toggles every 700 ms.

| Variant | Scenario | Private WS (MB) | Working set (MB) | Private bytes (MB) | CPU app (% 1 core) | CPU dwm (% 1 core) | Thread | Notes |
|---|---|---|---|---|---|---|---|---|
| Composition (Win32 + DWM) | idle | 11.6 | 43 | 52.9 | 0 | 27.86 | 25 |  |
| Composition (Win32 + DWM) | active | 12.1 | 44.1 | 54.1 | 0.68 | 27.33 | 26 |  |
| Slint femtovg (OpenGL) | idle | 42.6 | 75.7 | 69.4 | 0 | 51.43 | 13 |  |
| Slint femtovg (OpenGL) | active | 54.4 | 87.6 | 86.5 | 19.16 | 53.12 | 13 |  |
| Slint software | idle | 3.3 | 20.6 | 4 | 0 | 26.27 | 8 |  |
| Slint software | active | 3.5 | 21 | 4.2 | 5.41 | 44.18 | 8 |  |

Baseline dwm CPU without a pill: 28.16% 1 core

Binary size:
- `pill-composition.exe`: 0.27 MB
- `pill-slint.exe`: 8.8 MB
