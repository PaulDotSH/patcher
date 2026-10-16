# GUI installer: implementation plan

Spec: `docs/superpowers/specs/2026-10-16-gui-installer-design.md` (approved 2026-10-16).

Conventions for every task:

- Commit at the end of each task with a one-line imperative message. No co-author
  trailers.
- A task is done when `cargo test --release --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` pass on Windows. The Linux GUI
  stub is only built and run on the user's Linux machine.
- Push nothing.

Pinned versions: `slint` / `slint-interpreter` 1.18.1, `rfd` 0.17, `editpe` 0.2,
`ttf-parser` 0.25, `ico` 0.6, `windows-sys` 0.61, `toml` 1.1 and `serde` as already used
by `patcher-cli`.

Dependency order: 0 → (1, 2, 3, 4, 5 in any order) → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 →
14 → 15 → 16 → 17 → 18. Tasks 1–5 touch disjoint files and could run in parallel.

---

## Task 0: Spike (gate)

Throwaway crate in the session scratchpad, not committed. Each check yields yes/no plus
evidence.

1. **Fonts and library paths.** Build a `slint-interpreter` 1.18.1 binary that writes
   `main.slint` + `Anton.ttf` (from `repos/foarte-top/trap-foarte-top/fonts/card/`) into a
   temp folder, with `import "Anton.ttf";` and `import { X } from "@lib";`, and calls
   `Compiler::set_library_paths`. Compile, show, take a screenshot. Pass: the text renders
   in Anton.
2. **PE resources.** With `editpe` 0.2: replace the icon and the version info of
   `dist/stubs/patcher-stub-windows.exe`, write it out, run it with `--help`, and check
   the icon and Properties > Details in Explorer (computer-use screenshot). Repeat on a
   `x86_64-pc-windows-gnu`-style PE if one is available; otherwise note it as unchecked.
   If it fails, try `pelite` + a hand-written `.rsrc` rebuild, judge the effort, and
   report.
3. **Size.** Release build of a minimal interpreter + winit + software-renderer binary
   with `opt-level = "s"`, `lto = "fat"`, `panic = "abort"`, `strip`. Record the size.
4. **winit access.** Through `slint::winit_030::WinitWindowAccessor` on 1.18.1: call
   `drag_window()` from a `TouchArea` pointer-down, and receive `Focused` / `Occluded`
   window events. Pass: a frameless window drags, and focus changes are logged.
5. **Offscreen screenshots.** Render a compiled interpreter component to a PNG without
   showing a window (software renderer + a minimal headless `Platform`, or
   `slint::Window::take_snapshot` if that works when hidden). Pass: a correct PNG.

**Report to the user** with sizes and findings. A failed check stops the plan until the
user picks the alternative.

---

## Task 1: Core: `Phase`, `Progress::file`, `phase_totals`

Files: `crates/patcher-core/src/progress.rs`, `apply.rs`, all `Progress` callers
(`patcher-cli`, `patcher-stub`, `patcher-create` if it reports progress, tests).

1. Add `pub enum Phase { Verify, Decode, Commit, Revert }` with `label()`, matching the
   strings `Console` shows today.
2. Change `begin(&self, label: &str, total)` to `begin(&self, phase: Phase, total)`. Add
   `fn file(&self, _path: &str) {}`.
3. Call `file()` per file in verify and decode.
4. `pub fn phase_totals(patch: &Patch) -> [(Phase, u64); 3]`: verify = sum of
   `base_files` sizes, decode = sum of `writes` sizes, commit = number of commit
   operations (moves, deletes, dir ops). The commit phase must report in the same unit.
5. Test in `crates/patcher-core/tests` (or the existing roundtrip suite): a recording
   `Progress` applied to a testgen scenario sees each phase's `inc` sum equal to
   `phase_totals`.

Commit: `Core: typed progress phases, per-file hook and phase totals`.

## Task 2: Core: cancellation

Files: `apply.rs`, `lib.rs`.

1. Add `Error::Cancelled` ("cancelled; nothing was changed").
2. Add `ApplyOptions.cancel: Option<&'a AtomicBool>`. Give `inspect` a
   `cancel: Option<&AtomicBool>` parameter, updating callers.
3. Check the flag before each file in verify and decode (inside the rayon loop, with an
   early exit). Never check it in commit.
4. On cancel, the existing failure path removes `.patcher-staging`.
5. Test: a Progress that sets the flag after the first decode `inc`. Expect
   `Err(Cancelled)`, the game tree byte-identical to before (compare hashes), and no
   staging or backup directories.

Commit: `Core: cancel flag for verify and decode`.

## Task 3: Core: footer v2

Files: `format.rs`, `patch.rs`, `patcher-create/src/lib.rs` (`write_standalone`).

1. `Footer { ui: Option<Range>, patch: Option<Range> }` with `FOOTER_V2_LEN = 40` and
   magic `PTCHEND2`. `find` reads v2, else v1, which maps to `patch` only. Reject ranges
   outside `[0, len - footer_len)` and overlapping ranges.
2. `Footer::to_bytes` always writes v2.
3. `Patch::open_embedded` returns `None` when there is no patch range.
4. Add `pub fn embedded_ui(exe_bytes: &[u8]) -> Option<&[u8]>` in core.
5. `write_standalone(stub, ui: Option<&[u8]>, patch: Option<&Path>, out)`.
6. Tests: v1 bytes still parse; v2 with ui+patch, ui-only and patch-only; out-of-range
   and overlap rejected; existing standalone round trip still passes.

Commit: `Core: footer v2 with an optional UI bundle range`.

## Task 4: `patcher-stub` lib split

Files: `crates/patcher-stub/src/lib.rs` (new), `main.rs`, `Cargo.toml`.

1. Move `USAGE`, `Args`, `parse`, `run` and `load_patch` into `lib.rs` as
   `pub fn run_cli(args: Vec<OsString>, interactive: bool) -> ExitCode`. Add `--silent`,
   which means `--yes`.
2. `main.rs` becomes three lines.
3. Behaviour is unchanged; the existing tests and a manual `--help` check confirm it.

Commit: `Stub: move the CLI flow into a library for reuse`.

## Task 5: `patcher-gui` skeleton, theme model

Files: new `crates/patcher-gui/` (`Cargo.toml`, `src/lib.rs`, `src/theme.rs`,
`src/color.rs`, `src/paths.rs`), workspace `Cargo.toml`.

1. Serde structs for every table in the spec's theme file, all `deny_unknown_fields`,
   all optional.
2. `color.rs`: hex parse (`#rgb`, `#rrggbb`, `#rrggbbaa`), WCAG relative luminance and
   contrast, `derive(base, accent, overrides) -> Palette` per the spec.
3. `paths.rs`: expand `~`, `%VAR%` and `$VAR` / `${VAR}` per OS. Missing variables
   leave the candidate unusable rather than erroring.
4. `Theme::load(path) -> Result<ResolvedTheme>`: parse, resolve asset paths relative to
   the file, check that files exist, and validate enums and colours, with errors naming
   the key.
5. Unit tests: lime accent → dark ink; dark-blue accent → light ink; overrides win; an
   unknown key fails with its name; each expansion form works; a missing asset fails.

Commit: `GUI: theme model, colour derivation, path expansion`.

## Task 6: Bundle

Files: `crates/patcher-gui/src/bundle.rs`.

1. `UiBundle { version, files }` (bitcode, zstd). Pack takes a `ResolvedTheme`, copies
   the assets under `assets/`, the fonts under `fonts/` and the `.slint` sources under
   `ui/` (keeping their relative layout so their imports still resolve), and writes the
   normalised `theme.toml` with bundle paths and font families (read with `ttf-parser`).
2. Pack rejects author `.slint` files outside the theme's folder tree; their relative
   imports must stay inside the bundle.
3. Unpack validates paths with `patcher_core::paths`, enforces 64 MiB / 1024 files, and
   writes to `<temp>/patcher-ui-<xxh3>/`. An existing folder with a `.complete` marker
   is reused.
4. Tests: round trip, a traversal path rejected, limits enforced, font family read from
   the bundled Anton.

Commit: `GUI: UI bundle pack and unpack`.

## Task 7: Built-in Slint library and compile pipeline

Files: `crates/patcher-gui/ui/` (`api.slint`, `shell.slint`, `widgets.slint`, one file
per part, `credit.slint`), `crates/patcher-gui/fonts/` (Anton, Inter, JetBrains Mono
TTFs from `fonts/card/`, plus their OFL texts), `src/compile.rs`.

1. `api.slint`: `Screen`, `BaseStatus`, the `Patcher`, `Theme` and `Strings` globals,
   exactly as in the spec. Re-export every built-in part.
2. Default theme, loosely after trap.foarte.top (read `style.css` there for values):
   - dark panels, lime accent, uppercase Anton headings, spaced-caps Inter labels,
     mono for paths and stats;
   - wordmark with the optional tilted tag (−3°, accent fill, hard black shadow);
   - buttons with a 4 px hard offset shadow and a press shift;
   - progress bar with an accent fill, glow and a shimmer driven by `Patcher.tick`;
   - glow and grid backgrounds driven by `Patcher.tick`.
3. `compile.rs`:
   - write the built-in sources to `<dir>/patcher/` and the generated `<dir>/parts.slint`
     (one re-export per part, built-in or `ui/<author file>`);
   - write `<dir>/main.slint` with font imports and the `Root` window (spec "Parts"), and
     the `Installer` from the shell or the author;
   - compile with library paths `@patcher` and `@parts`;
   - return the `ComponentDefinition` or diagnostics formatted as `file:line:col: msg`.
4. Test (no window needed): the default theme compiles; a theme overriding `progress`
   with a tiny valid file compiles; a broken author file returns diagnostics naming that
   file and line.

Commit: `GUI: built-in Slint library and theme compile pipeline`.

## Task 8: Screen state machine

Files: `crates/patcher-gui/src/flow.rs`, `src/backend.rs`.

1. `trait Backend: Send + Sync`: `meta()`, `inspect(path, cancel) -> GameState-ish`,
   `apply(path, backup, progress, cancel)`, `revert(path, progress)`, `can_revert(path)`,
   `writable(path)`. A real implementation wraps `Arc<Patch>`; a fake one is scripted.
2. `Flow` is a pure state machine: a `State` struct mirroring the `Patcher` properties,
   and methods for every callback (`next`, `back`, `set_game_path`, `start`, ...). It
   emits `Effect`s (spawn inspect, spawn apply, spawn revert, elevate, launch, open,
   close) for the runtime to execute. It has no Slint types, so it tests without a
   window.
3. Game-folder candidate selection (spec "Game folder") lives here, using
   `ui::looks_like_game`.
4. Tests with the fake backend: the normal path; license gate (Next disabled until
   accepted); merged options; mismatch blocks Next; already-applied jumps to done with
   revert; revert path; cancel returns to the folder screen with the cancel message; apply
   error goes to the error screen; `needs-elevation` turns `start` into an `Elevate`
   effect; close during commit is refused.

Commit: `GUI: installer screen flow as a testable state machine`.

## Task 9: Runtime glue

Files: `crates/patcher-gui/src/app.rs`, `src/progress.rs`, `src/platform.rs`.

1. `pub fn run(opts: RunOptions) -> Result<()>`. Options: bundle bytes or none, patch
   source, start mode (normal / elevated with game and backup), and preview backend.
2. Wire the `Patcher` callbacks to `Flow`, push `State` into the globals after every
   change, and execute effects on worker threads. Results return through
   `slint::invoke_from_event_loop`.
3. GUI `Progress`: atomics plus a mutex for the phase and file. A 66 ms timer computes
   the weighted progress from `phase_totals`, speed and ETA from a 2 s window, and the
   log.
4. Animation timer at 33 ms on `Patcher.tick`. Pause it on unfocus or minimise using the
   approach validated in spike 4.
5. Frameless window: `drag-window`, `minimize` and `close` (close routes through `Flow`).
6. `rfd` folder picker on a worker thread. The Linux feature is `xdg-portal`. On error,
   set `can-browse = false`.
7. `launch`: Steam URL or exe with the game folder as working directory. `open-folder`
   and `open-url` through `explorer` / `xdg-open`. `copy-error` through
   `slint::platform`'s clipboard if exposed, otherwise `arboard`. Check which is cheaper
   and note it in the commit.
8. Manual check on Windows: run against a testgen scenario through a temporary
   `examples/run.rs`, and patch a generated game end to end, including cancel.

Commit: `GUI: runtime wiring, progress, window controls, picker, launch`.

## Task 10: Windows elevation

Files: `crates/patcher-gui/src/elevate.rs`.

1. `writable(dir)`: create and delete `.patcher-write-test`; map access denied to
   `false`.
2. `relaunch_elevated(exe, game, backup)` with `ShellExecuteExW` and the `runas` verb
   from `windows-sys`. Distinguish "user declined" (`ERROR_CANCELLED`) from other
   errors.
3. A no-op on Linux (`writable` is still used for an error message).
4. Manual check: patch a copy of a test game under `C:\Program Files\patcher-test\`,
   accept UAC, and confirm the elevated instance opens on the progress screen and
   finishes. Decline once and confirm the inline error.

Commit: `GUI: relaunch as administrator for protected game folders`.

## Task 11: `patcher-stub-gui`

Files: new `crates/patcher-stub-gui/` (`Cargo.toml`, `src/main.rs`), workspace profile
entry `opt-level = "s"`.

1. `#![windows_subsystem = "windows"]`.
2. Mode selection per the spec. Headless mode: `AttachConsole(ATTACH_PARENT_PROCESS)` on
   Windows, then `patcher_stub::run_cli`.
3. GUI mode: read its own exe (mmap), get the bundle range, call `patcher_gui::run`. On a
   compile error in the stub, show a native message box (`MessageBoxW`; on Linux,
   stderr plus a minimal Slint error window) and exit 1.
4. Manual checks:
   - double-click opens the GUI;
   - `--verify` from a terminal prints into that terminal;
   - `--silent GAME_DIR` patches without a window;
   - with an empty bundle, the default theme shows.
5. Record the release size in `BENCHMARKS.md`.

Commit: `GUI stub: windowed patcher with a headless CLI mode`.

## Task 12: Creator: PE resource rewrite

Files: `crates/patcher-create/src/pe.rs`.

1. `brand_windows_stub(stub: &[u8], icon_png: Option<&[u8]>, meta: &Meta) -> Vec<u8>`,
   using the method validated in spike 2. Build the icon at 16, 24, 32, 48, 64 and 256 px
   with `ico` and a PNG decoder; the creator-side `image` crate is fine.
2. If rewriting fails, print a warning and use the stub unchanged (the window icon still
   applies at runtime).
3. Test: brand the Windows CLI stub, re-parse it with `editpe`, check the version strings
   and the icon group, and confirm the branded stub still runs `--help`.

Commit: `Creator: brand Windows stubs with the mod icon and version info`.

## Task 13: CLI: `--ui`, `--gui`, `--detached`, four stub variants

Files: `crates/patcher-cli/src/main.rs`, `build.rs`, `Cargo.toml`.

1. Add the flags per the spec. `--gui` implies `gui`. `--detached` conflicts with
   `--no-standalone`.
2. `build.rs`: the env vars for four stubs. `find_stub(target, ui, dir)` with the new
   file names and the dev-build fallbacks for `patcher-stub-gui`.
3. GUI path: `Theme::load` → compile check through `patcher_gui::compile` in a temp
   folder (abort on diagnostics) → `bundle::pack` → per target, pick the stub (Linux GUI
   missing → the warning text from the spec, CLI stub) → `brand_windows_stub` →
   `write_standalone`.
4. Tests (`crates/patcher-cli/tests/gui.rs`, using testgen):
   - `create --ui gui --gui <fixture>` with a stub dir holding built stubs: footer parses,
     the bundle unpacks and compiles;
   - `--detached` yields a bundle-only exe that loads a sibling `.patch` (through
     `run_cli --verify`);
   - a stub dir without the Linux GUI stub yields the warning and a CLI Linux patcher;
   - a broken theme fails `create` with a `file:line` message.

Commit: `CLI: GUI patchers with themes, detached mode and stub fallback`.

## Task 14: `gui-preview`

Files: `crates/patcher-cli/src/preview.rs`, `crates/patcher-gui/src/sim.rs`.

1. `sim.rs`: a simulated `Backend` (fake meta, an always-valid folder, an 8 s scripted
   run with fake file names, and `--fail`).
2. Verify-only real backend: `apply` calls `inspect`, then reports "preview: nothing was
   written".
3. Reload loop: a 500 ms timer compares the modification times of the theme and every
   referenced file. On a change it recompiles, swaps the component and restores the
   screen. On errors it prints them and shows an overlay.
4. `--screenshot DIR`: for each `Screen`, set the state from a canned `State` and render
   off screen using the method from spike 5. Write `DIR/<screen>.png`.
5. Manual check: edit the accent in a theme while the preview runs and see it update
   within a second.

Commit: `CLI: gui-preview with live reload and screenshots`.

## Task 15: Dist

Files: `xtask/src/main.rs`, `.cargo/config.toml` (windows-gnu linker entry for Linux
hosts), `BENCHMARKS.md`.

1. Host-dependent stub set per the spec, with env vars for all four. Copy them into
   `dist/stubs/` with the new names.
2. On Linux, the `x86_64-pc-windows-gnu` linker is `x86_64-w64-mingw32-gcc`. If it is
   missing, print a warning naming the `mingw-w64` package.
3. Run `cargo xtask dist` on Windows. Check that `dist/patcher.exe` embeds three stubs
   and creates a GUI Windows patcher with no `--stub-dir`.
4. Add the stub sizes to `BENCHMARKS.md`.

Commit: `xtask: build GUI stubs and pick the stub set by host`.

## Task 16: Visual pass on the default theme

1. `gui-preview --screenshot` for every screen. Compare against trap.foarte.top's local
   `style.css` and `index.html` (palette, type scale, tag, shadows); the site is not
   copied.
2. Iterate on spacing, type sizes, contrast (WCAG AA for body text against panels) and
   the light `base` variant. Check 760×480 and a larger configured size.
3. Save the final PNGs to `docs/img/gui-*.png`.

Commit: `GUI: polish the default theme`.

## Task 17: Docs and examples

Files: `docs/gui.md`, `gui.example.toml`, `examples/gui/progress-only/`,
`examples/gui/full-window/`, `README.md`, `DESIGN.md`.

1. `docs/gui.md` covers:
   - quick start (`--gui`, `gui-preview`);
   - the theme reference (every key, generated from the serde structs' doc comments
     where practical, otherwise written out);
   - the string key table;
   - the `Patcher` / `Theme` / `Strings` API;
   - the part table;
   - the full-window contract;
   - the credit rule and its AGPL reason;
   - screenshots.
2. Two example themes. `full-window` deliberately looks different (for example a light
   theme with a sidebar) to show the range.
3. A compile test over `gui.example.toml` and both examples, added to `patcher-gui`
   tests.
4. README: a "GUI patchers" section. DESIGN.md: rows in the decisions table, footer v2,
   the new crates.

Commit: `Docs: GUI theming guide, example themes`.

## Task 18: Final verification and Linux checklist

1. `cargo test --release --workspace`, clippy, `cargo xtask dist` on Windows.
2. End to end on Windows with a testgen scenario, as a player would:
   - double-click the GUI patcher, auto-detect from `[paths]`, browse to change folder,
     patch, launch;
   - re-open it and revert;
   - repeat with cancel and with a deliberate mismatch.
   Take screenshots with computer-use.
3. Write `docs/linux-gui-checklist.md` for the user:
   - prerequisites (`rustup target add x86_64-pc-windows-gnu`, `mingw-w64`, the usual
     X11 / Wayland dev packages if linking needs them);
   - `cargo xtask dist`;
   - what to try: X11 and Wayland, the portal picker present and absent, drag, minimise,
     launch, `--silent` from a terminal;
   - what to report back.
4. Update the patcher memory notes with anything non-obvious learned.

Commit: `Docs: Linux GUI test checklist`.
