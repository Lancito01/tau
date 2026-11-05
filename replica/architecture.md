# Architecture

## Stack (why the web defaults do not apply)

No server, no account, no database: this is a local Windows utility.

| layer | choice | why |
| --- | --- | --- |
| language | Rust, `windows` crate, MSVC toolchain | no runtime, single small exe, memory safe around COM. C++ gives the same footprint with more foot-guns; C# needs .NET or a 10 MB+ NativeAOT binary |
| UI | raw Win32 plus Direct2D/DirectWrite | both ship with Windows; no framework, no WebView |
| wheel window | layered topmost window, per-pixel alpha, IDropTarget | the only way to be a real drop target during someone else's drag |
| images | WIC (built in), ffmpeg for WebP/AVIF encode | WIC covers JPG/PNG/TIFF/BMP/GIF, HEIC decode if the codec is installed |
| audio, video | ffmpeg sidecar, found on PATH or next to the exe | nothing lighter covers these formats |
| PDF render | `Windows.Data.Pdf` (built in) | 300 DPI page export with no dependency |
| PDF edit | `lopdf` crate | merge, split, rotate, metadata |
| OCR, faces | `Windows.Media.Ocr`, `Windows.Media.FaceAnalysis` (built in) | local, free |
| archives | `%SystemRoot%\System32\tar.exe` (built in) | libarchive: zip, tar, gz, RAR read |
| Office formats | Word/Excel/PowerPoint via COM if installed | high fidelity; otherwise those entries are dimmed |
| config | small INI file in `%APPDATA%\<app>` | no serde needed |

Binary target: under 2 MB, under 15 MB idle RAM, zero CPU while idle except hook callbacks.

Missing engines never fail silently: unavailable wheel entries render dimmed with the reason in the hub.

## Data

No schema. `Settings` is an INI file. `Job` and `ShelfItem` live in memory. Wheel contents are a static table: category -> entries.

## CLI surface (replaces the API table; also what tests drive)

| command | what | flow |
| --- | --- | --- |
| `app` | run in tray | all |
| `app --convert <fmt> <files>` | run one conversion | F01 |
| `app --tool <id> <files>` | open a tool | F02 |
| `app --action <id> <files>` | run an action | F03 |
| `app --wheel <files>` | open the wheel for these files, used by Send To | F04 |
| `app --render-wheel <kind> <cat> <out.png>` | render a wheel to PNG for design review | - |

## How the drag detection works

1. `WH_KEYBOARD_LL` watches for a press of a bound key (Win by default, or one key per wheel when the separate keys setting is on); `GetAsyncKeyState(VK_LBUTTON)` says whether a drag could be running.
2. On a tap with the button down, show a nearly invisible (alpha 1/255) probe window under the cursor and nudge the mouse by one pixel with `SendInput`, so OLE re-runs its hit test.
3. If `IDropTarget::DragEnter` fires with `CF_HDROP`, a real file drag is in progress. Read the paths, pick the category, draw the wheel. If nothing fires within about 150 ms, it was not a file drag: hide silently. Text selection and window dragging are unaffected.
4. While the wheel is up, a `WH_MOUSE_LL` hook turns scrolling into switching wheels, since the drag source owns the mouse. It is removed when the wheel hides, so it never adds latency to normal mouse use.
5. `DragOver` tracks the hovered entry. `Drop` runs it. `DragLeave` closes the wheel, so moving away is cancel and normal drop targets work again. Esc is handled by the drag source and arrives as `DragLeave`.
6. Always answer `DROPEFFECT_COPY`, never `MOVE`, so the source can never delete the original.

## Parts that bite

- Elevated Explorer or apps: UIPI blocks hooks and drops across integrity levels. Document it.
- Alt tap can focus a menu bar once the drag ends, when Alt is bound under separate keys.
- Win must be hidden from Windows on both press and release, or Start opens. A press Windows already saw must have its release passed on, or the key sticks.
- DPI: per-monitor v2 manifest; compute geometry from the monitor under the cursor.
- Output collisions: never overwrite; append ` (2)`.
- Long hook callbacks get unhooked by Windows: the hook only posts a message.
- ffmpeg progress: parse `-progress pipe:1` against the probed duration.
- Cancel kills the child process and removes the partial output.
- Hide child console windows with `CREATE_NO_WINDOW`.

## Build order

1. **Vertical slice**: tray, hook, probe, drop target, wheel, one image conversion, HUD. Proves the risky part.
2. **Must-haves**: all format tables, ffmpeg engine, archives, subtitles, actions, keyboard flow, settings.
3. **Should-haves**: PDF engine, Office COM, parameter-form tools.
4. **Could-haves**: canvas tools (crop, redact, annotate), timeline tools, shelf.
