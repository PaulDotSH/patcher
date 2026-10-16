# GUI installer for standalone patchers

Date: 2026-10-16. Status: design approved, spec under review.

## Goal

Mod authors can ship a standalone patcher that looks and behaves like an installer: a
themed window with a welcome page, game-folder selection, a progress bar and a result
page, instead of a console window. The default look is loosely based on trap.foarte.top,
and authors can restyle anything, from a single accent colour to a complete replacement
of the layout, through a TOML file plus optional Slint sources.

Success means:

- `patcher create` without UI flags produces GUI patchers for Windows (and for Linux when
  the creator was built on Linux).
- An author can change the accent colour with one line of TOML, replace one screen with
  one `.slint` file, or replace the whole window, and `create` rejects a broken theme
  before anything ships.
- A player double-clicks the patcher, sees the folder detected (or picks it), presses
  Patch, watches a progress bar and can launch the game at the end.

## Settled decisions

| topic | decision |
|---|---|
| stubs | four: Windows CLI, Windows GUI, Linux CLI (static musl), Linux GUI (glibc) |
| UI choice | `create --ui gui\|cli`, default `gui`; `--gui theme.toml` implies `gui`; one choice for every target |
| Linux GUI | built natively on Linux only (static musl cannot dlopen X11/Wayland). No CI for now; verified by the user on a real Linux machine |
| dist | Linux host builds all four stubs (Windows ones via `x86_64-pc-windows-gnu` + mingw-w64) and is the release creator. Windows host builds Windows CLI + GUI and Linux CLI |
| missing Linux GUI stub | warn and fall back to the Linux CLI stub |
| GUI toolkit | Slint, software renderer, winit backend |
| window | frameless, custom title bar, default 760×480, size set by the theme |
| layout engine | always `slint-interpreter`; the built-in theme is embedded `.slint` source |
| customisation | TOML tokens and assets, plus optional author `.slint` that replaces single parts or the whole window |
| theme file | separate `--gui theme.toml`, bundled into the stub; no runtime sidecar |
| default theme | dark palette with lime accent, Anton / Inter / JetBrains Mono, optional tilted-tag wordmark, hard offset shadows on buttons |
| colour tiers | `accent` alone derives ink, soft and glow; `base = dark\|light` derives neutrals; any token can be overridden |
| language | English built in, every string overridable |
| background | `glow` (default), `grid`, `image`, `none`, plus progress shimmer; 30 fps cap, paused when unfocused or minimised |
| credit | "Made with patcher" link to `https://github.com/PaulDotSH/patcher`, always visible, restylable, not removable (it is the AGPL source offer) |
| exe icon | creator rewrites the Windows stub's icon and version info; fallback is a runtime window icon only |
| folder | per-OS default path candidates, editable text field, native picker via `rfd` (xdg-portal on Linux) |
| admin | Windows runs as invoker; an unwritable folder turns the button into "Patch (as administrator)", which relaunches elevated |
| cancel | allowed during verify and decode, disabled during commit |
| preview | `patcher gui-preview` with live reload |
| out of scope (v1) | code signing, Markdown, audio, localisation beyond string overrides, Steam / registry detection, macOS |

## Crates

```text
patcher-core       + footer v2, Phase enum, Progress::file, cancel flag, phase_totals
patcher-stub       becomes lib + bin; lib exposes the CLI flow (`run_cli`) for reuse
patcher-gui (new)  theme parsing, colour derivation, bundle pack/unpack, built-in .slint
                   library, interpreter compile, GUI runtime (screens, worker, folder,
                   picker, elevation, launch)
patcher-stub-gui   (new) thin binary: windows_subsystem = "windows", headless mode or
                   patcher_gui::run
patcher-create     + PE resource rewrite, footer v2 writer
patcher-cli        + --ui, --gui, --detached, gui-preview; four stub variants
xtask              dist builds the host-dependent stub set
```

`patcher-gui` is linked by both the GUI stub and the creator. The creator needs it to
validate themes and to run `gui-preview`.

## File format

### Footer v2

A standalone patcher is `stub ++ [ui bundle] ++ [patch] ++ footer`.

```text
Footer v2 (40 bytes)
  ui_offset     u64   0 when there is no bundle
  ui_len        u64
  patch_offset  u64   0 when there is no embedded patch
  patch_len     u64
  magic         8     "PTCHEND2"
```

`PTCHEND1` footers (24 bytes) stay readable as "patch only". `Footer::find` returns both
ranges, and `Patch::open_embedded` returns `None` when `patch_len` is 0, so a detached GUI
patcher falls back to the `.patch` next to it, exactly as the CLI stub does today. Both
ranges are bounds-checked against the file and must not overlap.

The `.patch` format itself is unchanged.

### UI bundle

A zstd frame holding a bitcode-encoded `UiBundle`:

```rust
struct UiBundle {
    version: u32,                    // 1
    files: Vec<(String, Vec<u8>)>,   // bundle-relative path -> bytes
}
```

Paths obey the same rules as patch paths (relative, `/`, no `..`), checked with
`paths::validate`. `theme.toml` at the root is the normalised theme: every asset path is
rewritten to its bundle path, and font family names are filled in (read from the TTFs
with `ttf-parser` at create time). Limits: 64 MiB decoded and 1024 files, checked before
decoding.

## Theme file

Every key is optional. Asset paths are relative to the TOML file.

```toml
[window]
width = 760
height = 480
title = "My Mod"            # default: patch name
icon = "icon.png"           # window icon; also the Windows exe icon

[theme]
base = "dark"               # dark | light
accent = "#c6ff3d"
wordmark = "Trap Foarte Top" # default: patch name
tag = "Top"                 # word drawn in the tilted tag; default last word; "" disables
logo = "logo.png"           # replaces the wordmark when set
background = "glow"         # glow | grid | image | none
background_image = "bg.png" # used with background = "image"
shadows = true              # hard offset shadows on buttons

[theme.colors]              # overrides any derived token
# bg panel raised border border-hover text dim faint accent accent-ink accent-soft
# accent-glow ok bad warn

[theme.fonts]               # TTF/OTF files; defaults are the bundled Anton / Inter / JetBrains Mono
display = "fonts/MyDisplay.ttf"
body = "fonts/MyBody.ttf"
mono = "fonts/MyMono.ttf"

[layout]
installer = "ui/installer.slint"  # replaces the whole window content

[layout.parts]              # replaces single parts; see "Layout API"
progress = "ui/progress.slint"

[screens]
welcome = true
options = true              # backup toggle
license = "LICENSE.txt"     # adds the license page
require_accept = false      # gate Next on an "I agree" checkbox

[paths]                     # default game folder candidates, first valid one wins
windows = ['%ProgramFiles(x86)%\Steam\steamapps\common\Game']
linux = ["~/.local/share/Steam/steamapps/common/Game"]

[launch]
windows = "Game.exe"        # relative to the game folder
linux = "game.sh"
steam_app_id = 0            # when set, Launch opens steam://rungameid/<id> instead

[credit]
position = "bottom-right"   # bottom-right | bottom-left | top-right | top-left

[strings]                   # any built-in string by key, see docs/gui.md
patch_button = "Install"
```

Unknown keys are an error (`deny_unknown_fields`), so typos fail at create time.

### Colour derivation

1. `base` picks the neutral set. `dark` is the trap.foarte.top palette (`bg #0b0b0c`,
   `panel #141416`, `raised #1b1b1f`, `border #26262b`, `border-hover #3a3a42`,
   `text #f3f3ee`, `dim #a6a69e`, `faint #6e6e68`). `light` is its mirror.
2. `accent` (default `#c6ff3d`) derives `accent-ink` (`bg` or `text`, whichever has the
   higher WCAG contrast against the accent), `accent-soft` (accent at 10 % alpha) and
   `accent-glow` (accent at 35 % alpha).
3. `ok` defaults to the accent; `bad #ff5b4f` and `warn #ffc53d` are fixed defaults.
4. `[theme.colors]` overrides any token last.

## Layout API

The built-in library is reachable as `@patcher` (`import { Patcher, Theme } from
"@patcher";`). It is embedded in `patcher-gui` and shared by the stub and the creator, so
the version always matches.

### Globals

`Theme`: every colour token, `font-display` / `font-body` / `font-mono` family names,
`radius`, `shadows`, `background` mode and image, `wordmark`, `tag`, `logo`.

`Strings`: one `out property <string>` per string key.

`Patcher` (Rust owns all state; `.slint` only reads properties and calls callbacks):

```slint
export enum Screen { welcome, license, folder, options, progress, done, error }
export enum BaseStatus { unknown, checking, ok, already-applied, mismatch, not-found }

export global Patcher {
    in property <Screen> screen;
    in property <string> mod-name; in property <string> mod-version;
    in property <string> mod-author; in property <string> mod-description;
    in property <string> mod-url;
    in property <string> license-text;
    in property <bool> license-accepted;
    in property <string> game-path;
    in property <BaseStatus> base-status;
    in property <string> base-detail;          // e.g. "3 files differ"
    in property <bool> can-browse;             // false when no file picker is available
    in property <bool> needs-elevation;
    in property <bool> backup;
    in property <bool> can-revert;             // a backup from this patch exists
    in property <float> progress;              // 0..1, weighted over all phases
    in property <string> phase;                // "Checking files", "Patching", ...
    in property <string> current-file;
    in property <string> speed;                // "84 MB/s"
    in property <string> eta;                  // "0:12"
    in property <[string]> log;
    in property <bool> can-cancel;
    in property <bool> can-launch;
    in property <string> result;               // done-screen summary
    in property <string> error-text;
    in property <string> error-details;
    in property <duration> tick;               // drives background animation, 30 fps max

    callback next();
    callback back();
    callback set-game-path(string);
    callback browse();
    callback set-backup(bool);
    callback accept-license(bool);
    callback start();                          // also used for "Patch (as administrator)"
    callback cancel();
    callback revert();
    callback launch();
    callback open-folder();
    callback open-url(string);
    callback copy-error();
    callback minimize();
    callback close();
    callback drag-window();
}
```

### Parts

The built-in window is a fixed shell (`@patcher/shell.slint`) that imports its parts from
`@parts`, a module generated for each theme. For every part, `@parts` re-exports either
the built-in component or the author's file:

| part key | component | contents |
|---|---|---|
| `titlebar` | `TitleBar` | drag area, wordmark, minimise and close |
| `background` | `Background` | glow, grid or image |
| `welcome` | `WelcomeScreen` | name, version, author, description, url, options when merged, revert offer |
| `license` | `LicenseScreen` | scrollable text, accept checkbox |
| `folder` | `FolderScreen` | path field, browse, base status |
| `options` | `OptionsScreen` | backup toggle |
| `progress` | `ProgressScreen` | bar, phase, file, speed, ETA, log, cancel |
| `done` | `DoneScreen` | summary, open folder, launch, close |
| `error` | `ErrorScreen` | message, details, copy, close |
| `buttons` | `NavButtons` | back / next / patch / cancel row |

Every part takes no properties; it fills its parent and reads the globals. An author file
for a part must export the component under the name in the table. The built-in parts are
importable from `@patcher` so authors can wrap or reuse them.

`[layout] installer = "file.slint"` replaces the shell: the file must export
`component Installer` (any element, not a `Window`). In both cases the generated root is:

```slint
export component Root inherits Window {
    no-frame: true;
    width: <window.width>; height: <window.height>;
    title: <window.title>; icon: <window.icon>;
    Installer { }
    Credit { }      // always drawn on top at [credit].position
}
```

`Credit` is not a part and cannot be replaced; it follows `Theme` colours.

When `[screens] options` is on and there is no license page, the options are shown on the
welcome screen and `Screen.options` is skipped.

## Runtime

### Start-up

1. `patcher-stub-gui` reads its own exe and finds the footer. With no bundle it uses the
   built-in default theme.
2. Arguments decide the mode:
   - none: GUI;
   - `--elevated --game DIR [--no-backup]`: GUI, starting straight on the progress screen
     (used by the admin relaunch);
   - anything else (`--silent`, `--yes`, `--verify`, `--revert`, `--info`, `--help`,
     `--patch`, `--out`, `GAME_DIR`): headless. On Windows it calls
     `AttachConsole(ATTACH_PARENT_PROCESS)` and runs `patcher_stub::run_cli`, so the flags
     and output match the CLI stub exactly. `--silent` means `--yes` with no prompts.
3. The bundle is unpacked into `<temp>/patcher-ui-<xxh3 of bundle>/` (reused if it is
   already there and complete) and removed on exit. Compiling from real files means
   `@image-url` and font imports work with no custom loaders.
4. `patcher-gui` writes `@parts` and `main.slint` into that folder, sets the library
   paths `@patcher` (built-in sources, unpacked next to it) and `@parts`, and compiles.
   A compile error at this point means a tampered or corrupt bundle; the stub shows a
   native message box with the diagnostics and exits.
5. The theme, strings and patch metadata are written into the globals, and the window
   opens.

### Screen flow

```text
welcome -> [license] -> folder -> [options] -> progress -> done
                                                       \-> error
welcome --(revert, when a backup from this patch exists)--> progress -> done
```

`back` is available until progress starts. `next` on the folder screen is only enabled
when `base-status` is `ok`. `already-applied` shows the done screen directly with "already
applied" and the revert offer.

### Game folder

Candidates are tried in order: `[paths].<os>` entries with `~`, `%VAR%` (Windows) and
`$VAR` / `${VAR}` (Linux) expanded, then the exe's folder, then the current directory. The
first candidate where `ui::looks_like_game` holds is chosen. If none qualifies, the first
configured candidate is shown as text (or the exe's folder when there are none).

Every edit to the path field (debounced 300 ms) and every browse result runs `inspect` on
a worker thread and updates `base-status` and `base-detail`. Browse uses
`rfd::FileDialog::pick_folder`. On Linux `rfd` is built with its xdg-portal backend. If
the portal call fails, `can-browse` becomes false and the button hides.

### Applying

`start` spawns a worker thread holding an `Arc<Patch>`. It calls `apply` with a GUI
`Progress` that stores phase, byte counters and current file in atomics and a mutex. A
`slint::Timer` at 66 ms copies them into the globals, computes the weighted progress from
`apply::phase_totals`, and derives speed and ETA from a 2-second moving window.
`note()` lines go to `log`.

`cancel` sets the `AtomicBool` passed in `ApplyOptions.cancel`. `can-cancel` is false once
the Commit phase begins, and so is the window's close button. Closing the window during
verify or decode counts as cancel; the window closes once the worker has cleaned up.

`revert` runs `patcher_core::revert` the same way.

### Windows elevation

Before `start`, a write probe creates and deletes `.patcher-write-test` in the game
folder. If that fails with access denied, `needs-elevation` turns on and `start` calls
`ShellExecuteW("runas", exe, "--elevated --game <dir> [--no-backup]")`, then closes the
window once the new process has launched. If the user declines UAC, the GUI stays open
with an error line under the button.

### Animation

A `slint::Timer` at 33 ms advances `Patcher.tick`. Built-in animated parts use `tick`
rather than `animation-tick()`, so redraws are capped at 30 fps. The timer stops while
the window is unfocused or minimised (winit window events through Slint's winit accessor)
and when `background = "none"`, apart from the progress shimmer during progress.

### Window

`no-frame: true`. The title bar's drag area calls `drag-window`, which calls winit's
`Window::drag_window()`; minimise and close use `slint::Window`. Close confirms during
progress, as described above.

### Launch and open folder

`launch` runs `steam://rungameid/<id>` through the OS URL handler when `steam_app_id` is
set. Otherwise it starts `[launch].<os>` with the game folder as its working directory.
`can-launch` is false when neither is configured or the file does not exist.
`open-folder` and `open-url` use the OS handler (`explorer` / `xdg-open`).

## Core changes

- `progress::Phase { Verify, Decode, Commit, Revert }` with `label()`.
  `Progress::begin(&self, phase: Phase, total: u64)` replaces the label argument; the
  `Console` progress prints `phase.label()`.
- `Progress::file(&self, path: &str) {}`, a default no-op, called when a file starts in
  verify and decode.
- `apply::phase_totals(&Patch) -> [(Phase, u64); 3]`: bytes read in verify, bytes written
  in decode, and the commit operation count.
- `ApplyOptions.cancel: Option<&AtomicBool>`, also accepted by `inspect`. It is checked
  before each file in verify and decode. A set flag returns `Error::Cancelled` after the
  normal staging cleanup. It is never checked in commit.
- `format::Footer` becomes footer v2 with v1 read support. `Patch::open_embedded` handles
  `patch_len == 0`.
- `patcher-stub` moves `parse` / `run` / `load_patch` into `lib.rs` as
  `pub fn run_cli(args: impl Iterator<Item = OsString>) -> Result<(), String>`.

## Creator changes

### `create`

| flag | meaning |
|---|---|
| `--ui gui\|cli` | stub variant, default `gui` |
| `--gui FILE` | theme file; implies `--ui gui` |
| `--detached` | write the GUI patcher without the patch inside (`stub ++ bundle ++ footer`) next to `Name.patch` |

With GUI:

1. Load the theme, resolve assets, read font families, normalise, and build the bundle.
   No `--gui` means an empty bundle (default theme).
2. Generate `@parts` and `main.slint` into a temp folder and compile them with the
   interpreter, exactly as the stub will. Errors abort `create` with the file, line and
   message.
3. Pick the stub per target: GUI variant, or for Linux without a GUI stub, the CLI stub
   plus the warning `no Linux GUI stub in this build (build the creator on Linux); the
   Linux patcher will be a console app`.
4. For Windows targets, rewrite the stub's resources: the icon from `[window] icon`
   (converted to a multi-size `.ico`, 16 to 256 px) and the version info (ProductName =
   mod name, ProductVersion / FileVersion = mod version, CompanyName = author,
   FileDescription = "<name> patcher"). This happens on the stub bytes before anything is
   appended.
5. Write `stub ++ bundle ++ [patch] ++ footer v2`.

### Stub lookup

The `stubs` module gains `STUB_WINDOWS_GUI` and `STUB_LINUX_GUI`, from
`PATCHER_STUB_WINDOWS_GUI` / `PATCHER_STUB_LINUX_GUI` in `build.rs`. Runtime lookup in
`--stub-dir`, the exe folder and `stubs/` adds `patcher-stub-gui-windows.exe` and
`patcher-stub-gui-linux`.

### `gui-preview`

```text
patcher gui-preview [--gui theme.toml] [--patch FILE] [--game DIR] [--fail] [--screenshot DIR]
```

- Without `--patch`, a simulated backend: a fake mod, a game folder that always
  validates, and an 8-second scripted progress run. `--fail` makes the run end on the
  error screen instead.
- With `--patch`, the real backend restricted to `inspect`: start runs verify only and
  then shows the done screen as "preview: nothing was written".
- Every 500 ms it checks the modification times of the theme and every referenced file.
  On a change it rebuilds and replaces the window, keeping the current screen. Compile
  errors go to the terminal and an error overlay in the window, and the last good window
  stays usable.
- `--screenshot DIR` renders each screen to `DIR/<screen>.png` with the software
  renderer, off screen, at the configured window size, and exits. It is used for docs
  and automated visual checks.

## Dist

`cargo xtask dist` builds per host:

| host | stubs |
|---|---|
| Windows | `patcher-stub` (msvc), `patcher-stub-gui` (msvc), `patcher-stub` (linux-musl) |
| Linux | `patcher-stub` and `patcher-stub-gui` (linux-gnu for the GUI, musl for the CLI), `patcher-stub` and `patcher-stub-gui` (`x86_64-pc-windows-gnu`, needs mingw-w64) |

A missing target or toolchain prints the existing style of warning and is skipped. The
release profile keeps `opt-level = 3` for `patcher-stub`. `patcher-stub-gui` uses
`opt-level = "s"`; its size is measured in the spike and reported in BENCHMARKS.md, with
no cap.

## Licensing

Slint is used under GPL-3.0, which is compatible with this AGPL-3.0-or-later project. The
bundled fonts are OFL-1.1; their license texts are embedded in the stub next to the
fonts (OFL only requires that they travel with the font, not that they are shown). `rfd`, `editpe` (or its replacement),
`ttf-parser` and `ico` are MIT/Apache.

## Error handling

| situation | behaviour |
|---|---|
| theme error at create (missing file, unknown key, bad colour, slint compile error) | `create` fails, naming the file and key or line |
| bundle missing or corrupt in the stub | default theme, plus a warning line in the log |
| bundle compiles with errors in the stub (tampered) | native message box with diagnostics, exit code 1 |
| base mismatch | folder screen shows `mismatch` and the first 5 differing paths in `base-detail`; Next disabled |
| apply error | error screen with `error-text` (the core error's message) and `error-details` (full chain plus log); rollback as today |
| cancel | back to the folder screen with "Cancelled; nothing was changed" |
| UAC declined | stay on the folder screen with an inline error |
| no file picker | browse hidden, text field still works |

## Testing

- **Core:** footer v2 round trip; v1 still read; overlapping or out-of-range ranges
  rejected; a bundle-only exe yields `open_embedded == None`; cancel during decode leaves
  the game byte-identical and no staging folder behind; `phase_totals` match the bytes
  reported to `Progress`.
- **patcher-gui unit tests:** colour derivation (ink chosen by contrast for light and dark
  accents); TOML parsing, unknown keys and bad colours; path expansion (`~`, `%VAR%`,
  `$VAR`); bundle round trip and path validation; `@parts` generation.
- **Compile tests (no window needed):** the built-in theme, `gui.example.toml`, and both
  `examples/gui/` themes compile through the interpreter.
- **Screen state machine:** driven with a fake backend: normal run, license gate,
  mismatch, already applied, revert, cancel, error, merged options.
- **Creator integration:** `create --ui gui --gui examples/gui/progress-only/theme.toml`
  produces an exe whose footer and bundle parse and whose bundle compiles. `--detached`
  produces a bundle-only exe plus `.patch`. A missing Linux GUI stub produces the warning
  and a CLI Linux patcher.
- **Visual:** `gui-preview --screenshot` for the default theme and both examples,
  inspected by eye; the PNGs go in `docs/img/`.
- **Linux GUI:** compiled and tested by the user on a Linux machine.

## Docs and examples

- `docs/gui.md`: theme reference, every string key, the `Patcher` / `Theme` / `Strings`
  API, the part table, and the preview workflow.
- `gui.example.toml`: every key, commented.
- `examples/gui/progress-only/`: restyles only `ProgressScreen`.
- `examples/gui/full-window/`: replaces the whole `Installer`.
- README and DESIGN.md: sections on the GUI stub and footer v2.

## Spike first

These are checked before the main work. If one fails, the user is consulted before the
design changes.

1. `slint-interpreter` loads a `.ttf` through `import "x.ttf";` from a folder at runtime,
   and resolves `@patcher` / `@parts` library paths.
2. A pure-Rust crate (first candidate `editpe`) rewrites icon and version-info resources
   of the msvc and gnu stubs, and the result still runs.
3. GUI stub size (Windows msvc, release, `opt-level = "s"`): measured and reported.
4. winit `drag_window` and focus / minimise events are reachable through Slint's winit
   accessor on the pinned Slint version.
5. Off-screen screenshots of a compiled interpreter component with the software renderer.
