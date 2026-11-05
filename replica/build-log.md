# Build log

## Stack

Rust with the `windows` crate, Direct2D and DirectWrite on a layered window. The
release binary is about 1.9 MB. The MSI is about 1 MB, built with WiX 5.

## Bugs found by running the app

| bug | cause | fix |
| --- | --- | --- |
| Wheel vanished during a drag | Fully transparent first frames let the drag fall through the window | Clear to alpha 1/255 while animating |
| Modal Right + Enter sometimes did nothing | A repaint sends a mouse-move to a still cursor, which cleared the keyboard selection | Only real cursor movement changes the selection |
| Share showed a "no app associated" error | The `Windows.ModernShare` verb does not work through ShellExecute | Moved to the share API; see open issues |
| Edit photo labels were cut off | Label column too narrow | Widened |
| Edit photo at neutral settings shifted colours | `eq` filter goes through YUV | Left out at neutral settings |
| Timeline ignored Enter | No key handler | Added |
| Metadata strip left EXIF in JPEGs | `-c copy` keeps it | Lossless stripper |
| csv to xlsx garbled non-ASCII text | Excel guessed the encoding | BOM-marked temp CSV |
| Compress threw away an explicit resize | Size check ran on resized output | Skip the check after a real resize |
| Settings asked for ffmpeg even when found | Fixed text | Shown only when missing |

## Test results

- Unit tests: 57 pass, plus 1 manual startup test that also passes.
- Output content checks: 61 of 61 (pixels, durations, page order, metadata, encoding, archives).
- Modal wheel: 6 of 6 runs in a row after the fix.
- Tool windows driven through to a saved file: compress, metadata, edit, annotate,
  background, crop, redact, video trim, video crop, video snapshot.
- MSI, no elevation: install, upgrade (startup setting kept), downgrade refused,
  uninstall (running app stopped, files, shortcut, startup entry and install key removed).

## Open issues and untested paths

- Share: the Windows share host starts, but no visible sheet was captured and focus
  leaves at once. Needs a test by hand.
- Move: only cancel was run through the wheel. A real move was not.
- Real drag from File Explorer: not machine-tested; the automation was not allowed
  to drive Explorer.
- CI workflow: YAML parses, but it has never run on GitHub.
- Auto-update: parsing and version comparison are unit tested. Download, checksum
  and silent install have never run against a real release.
- Tray icon click, Send To link, dragging out of the shelf, using a rebound key.
- A trim that keeps the full length re-encodes and can come out larger than the source.

## Known limits

- ffmpeg is not bundled, so audio, video, WebP and AVIF stay dimmed until it is on PATH or beside the exe.
- HEIC output needs the Store codecs. Office formats need Office installed.
- Image metadata is strip only. Solid vs translucent look is not built.
- Out of scope: Pages files, AirDrop, purchase and trial.
