# Recon map: Tangerine (macOS) -> Windows

Scope: the core loop (drag + key tap + radial wheel + drop = local conversion), the tool wheel and the action wheel.
For: the user's own daily-driver Windows 10/11 utility (desktop only).
Date: 2026-11-04

App internals were NOT observed. There is no Mac here and no binary was inspected. Everything about
in-app screens below comes from the public site and press, and is marked `guess` where inferred.

## Sources

| # | source | URL | notes |
| --- | --- | --- | --- |
| 1 | marketing site | https://tangerineformac.com/ | full format and tool lists per file category, wheel visuals |
| 2 | Digital Trends | https://www.digitaltrends.com/?p=6017866 | Shift-drag, saves beside the original, $29.99, 20 free conversions |
| 3 | Borncity | https://borncity.com/news/tangerine-fuer-macos-275-konvertierungsoptionen-im-radialmenue/ | counts, tool names, trial |
| 4 | Applesfera | https://www.applesfera.com/aplicaciones-os-x-1/hay-app-para-mac-que-permite-convertir-archivos-solo-arrastrarlos-pulsar-tecla | glass look with solid option, menu-bar icon, EUR 34.99 |

No docs, changelog or public API exist.

## Core loop

Drag any file, tap a modifier, drop on a wheel entry: the result is saved beside the original, offline, with no window opened.

## Screens

| ID | screen | how to reach | purpose | key components | states seen |
| --- | --- | --- | --- | --- | --- |
| S01 | Convert wheel | drag + tap Shift | pick target format | radial menu, hub label | filled, hover per entry (site); 3-9 entries by type |
| S02 | Tools wheel | drag + tap Cmd | pick a file tool | radial menu with icon + label | filled, hover (site); 1-9 entries by type |
| S03 | Actions wheel | drag + tap Ctrl | file actions | radial menu | 6 entries, works on anything (site) |
| S04 | Progress HUD | after a drop | conversion progress | title, "n of m saved", bar, cancel X | running (site); error, done = guess |
| S05 | Menu-bar menu | status icon | settings, unlock | native menu | seen in press only |
| S06 | Settings | S05 | rebind keys, solid vs glass look | key pickers, toggles | guess at layout |
| S07 | Tool windows | S02 entry | per-tool option UI with preview | see Tool list | site thumbnails only |
| S08 | Float shelf | S03 Float | hold files, pin to edge, drag out | floating window, file list | site text only |
| S09 | Trial / unlock | after 20 uses | paywall | - | skip: not relevant to a personal clone |

## Flows

```
F01 Convert by drag (the core loop)
    Drag -> tap Shift -> S01 -> drop on entry -> S04 -> file beside the original
    happy path clicks: 0 (one drag, one key tap). NUMBER TO BEAT.
    edge: key released (wheel stays), Esc cancels, mixed selection, name collision, unsupported type
F02 Tool by drag
    Drag -> tap Cmd -> S02 -> drop -> S07 -> save
    edge: multi-select batch, tools that need several files (merge, join, collage)
F03 File action by drag
    Drag -> tap Ctrl -> S03 -> drop
    edge: folders, mixed selections, no conversion available
F04 Keyboard only
    Select file -> Enter -> S01 -> Cmd/Ctrl switches wheel -> arrows -> Enter (Esc cancels)
F05 Switch wheels mid-drag
    S01 -> tap other key -> S02
F06 Float shelf
    S03 Float -> S08 -> pin to an edge -> drag out into another app
F07 Change keys
    S05 -> S06 -> rebind
```

## Components

| component | variants | states | used on |
| --- | --- | --- | --- |
| Radial menu | formats (text), tools (icon + text) | closed, opening, hover, drop-target, disabled | S01-3 |
| Hub label | format, tool name | idle, hover | S01-3 |
| Progress HUD | single, batch | running, done, error, cancelled | S04 |
| Option form | slider, select, number, checkbox, colour | default, focus, disabled | S07 |
| Canvas editor | crop, redact, annotate | empty, selecting | S07 |
| Timeline | trim, split, bleep | idle, playing | S07 |

## Tool list per file type (from source 1)

- Image: compress, metadata, edit, annotate, add background, crop, redact. Multi: create PDF, collage.
- Video: compress, metadata, mute, trim, crop, speed, snapshot, split, redact. Multi: join.
- Audio: compress, metadata, normalize, visualize, trim, channels, bleep.
- PDF: compress, metadata, split, organize. Multi: merge.
- Archive: extract.
- Actions: AirDrop, Float, Copy, Move, Copy Path, Terminal.

## Inferred data model (no database: local config plus in-memory jobs)

```
Settings   convert_key, tools_key, actions_key, appearance, launch_at_login
           evidence: "Change the keys in Settings", Applesfera on solid look. confidence: high
WheelItem  id, label, icon, kind (format | tool | action), input_types[]
           evidence: per-category wheels on source 1. confidence: high
Job        inputs[], target, status, progress, outputs[]
           evidence: S04 "1 of 1 saved". confidence: medium
ShelfItem  path, added_at
           evidence: Float text. confidence: guess
```

Relationships: file category 1-n WheelItem, Job 1-n outputs.

## Feature matrix

See `features.csv`.

## Out of scope (cannot or should not be cloned)

- Name, logo, tangerine-slice wheel, marketing copy and imagery. The user asked for a radial menu that is NOT the sliced look.
- Apple Pages import/export: proprietary format with no Windows engine.
- AirDrop: Apple-only. The Windows Share sheet replaces it.
- Trial and App Store purchase flow: personal tool.

## Size

Screens 8, flows 7, entities 4. Hard parts:
1. Detecting an in-progress Explorer drag and becoming a drop target mid-drag, with no shell hook DLL.
2. Replacing Apple's built-in frameworks (PDF, OCR, face detection, rich documents) without a heavy runtime.
3. 26 tools each need their own option UI; the visual ones (crop, redact, annotate, trim) need a canvas or timeline.

Size: M. Wheel plus conversions is a focused build; every visual tool at Mac polish is the long tail.
