# Agent instructions

## Project

Patcher: a Rust tool for game-mod authors. `patcher create` diffs an original
game folder against a modded one and writes `Name.patch` plus self-contained
patchers (`Name-patcher.exe` for Windows, `Name-patcher` for Linux; stub ++ patch
++ footer). Players run the patcher next to their own game copy. It verifies the
base files, applies the patch with backups and rollback, and can revert. Players
get an installer window (Slint, themeable) or a console patcher. Mod authors
redistribute only the difference, never the game. Working MVP plus GUI; local git
repo with remote github.com/PaulDotSH/patcher.

- Design and format: `DESIGN.md`. GUI theming: `docs/gui.md`. Benchmarks:
  `BENCHMARKS.md`. GUI design/plan: `docs/superpowers/`.
- Settled: one base version per patch; ops modify/add/delete/copy/move; size
  first, then apply speed (5 % tolerance rule, fast/balanced/max profiles);
  archives such as PopCap `.pak` are opaque blobs for now (the `Transform` slot is
  reserved); file hashes are size + xxh3 always, BLAKE3 optional; backups plus
  `--revert`; Windows and Linux; license AGPL-3.0-or-later.
- GUI is settled too: 4 stubs (win/linux x cli/gui), theme via TOML plus runtime
  `.slint` files, a fixed `Patcher` global API, the unremovable "Made with
  patcher" link, demo/preview wordmark is "patcher". Re-ask only if a fact breaks
  a decision.
- Push to GitHub only when the user says so.

## Stack and layout

- Rust workspace (edition 2024, resolver 3): `crates/*` plus `xtask`.
  `patcher-core`: format, pure-Rust decoders, apply/verify/revert (linked into the
  stubs). `patcher-create`: scanning, classification, encoders, bsdiff, patch
  writer (native libzstd). `patcher-cli`: the `patcher` author binary.
  `patcher-stub`: player console patcher. `patcher-gui`: themes, UI bundles,
  built-in Slint layout, installer runtime. `patcher-stub-gui`: player installer
  window. `patcher-testgen`: deterministic synthetic games and mod scenarios.
  `xtask`: `dist`, `gen-corpus`, `gen-large`, `bench`.
- `vendor/ruzstd` is a vendored ruzstd 0.9.1 with an added `set_raw_prefix`
  (zstd `--patch-from` frames); wired in through `[patch.crates-io]`. Do not
  replace it with the crates.io version.
- `.cargo/config.toml`: musl uses `rust-lld`; the Windows-gnu target uses mingw
  (Linux hosts); `cargo xtask` is an alias.
- Examples and samples: `examples/gui/`, `gui.example.toml`, `patch.example.toml`.
- Generated or ignored: `target/`, `dist/` (stubs and `patcher.exe` from
  `cargo xtask dist`).
- Linux GUI stub is glibc and builds natively on Linux only; Linux runtime is
  verified by compile (and the checklist in `docs/linux-gui-checklist.md`).

## Commands

- Test: `cargo test --release --workspace` (as CI does).
- Dist (stubs embedded into `dist/patcher(.exe)`): `cargo xtask dist`; needs
  `rustup target add x86_64-unknown-linux-musl`.
- Benchmarks: `cargo xtask bench --scale 1 --large 3` (regenerates
  `BENCHMARKS.md`); corpora: `cargo xtask gen-corpus`, `cargo xtask gen-large`.
- Linux console stub: `cargo build --release -p patcher-stub --target x86_64-unknown-linux-musl`.
- GUI theme preview: `patcher gui-preview [--gui theme.toml] [--screenshot DIR]`.

Run the relevant checks before declaring work done.

## Coding style and rules

- Follow existing patterns and match the surrounding code.
- Do not over-rely on OOP when a simpler structure fits.
- Do not add helper functions when an equivalent already exists in a library
  that is already in use.
- Prefer the standard library and existing dependencies over new ones.
- Do not add comments unless asked.
- The stub crates ship to players: keep them small and pure Rust (no native
  libzstd in the stub), and never let a patch path escape the game folder.
- Test data is generated (`patcher-testgen`); do not commit game files.
- Commit or push only when asked. Never add AI co-author trailers (e.g.
  `Co-Authored-By: Claude`) to commits.

## Testing and verification

- Write tests before implementation where practical.
- When a test fails, fix the code — not the test — unless the test asserts the
  wrong behavior. Never weaken a test just to make it pass.
- Static types, linters, and checkers are part of "done".
- GUI checks must not take over the user's mouse or focus: use
  `gui-preview --screenshot` for offscreen renders.

## Output-heavy commands

- **Delegate output-heavy commands to a cheap model.** When a command's output
  is large but the answer you need is small (parsing logs, sifting build or CI
  output, scanning long dumps), prefer handing it to a cheap subagent and
  taking back only the conclusion: DeepSeek via the `deepseek` MCP
  (`run_task`) or Claude Haiku. Give it the exact command and what to report;
  it reads, it does not edit. This needs no explicit opt-in.
- **Test runs print failures, not passes.** Run suites in a mode that hides
  passing tests and shows only failures plus the summary: e.g. `cargo test -q`,
  `cargo nextest run --status-level fail`, `pytest -q`, `vitest --reporter=dot`,
  `go test ./...` without `-v`, `dotnet test -v q`. If the runner has no such
  mode, filter its output down to the failures. Never pull hundreds of passing
  lines into context.

## Planning

- For large tasks, plan first with a strong model, then implement with cheaper
  models to save tokens.
- State the goal and the stopping condition before starting.

## Subagent usage

These rules are always in effect, even when the `dispatching-parallel-agents`
skill is not loaded. If you do dispatch subagents, follow them; the skill has
the full workflow.

- **Explicit opt-in only.** Never dispatch, spawn, or delegate to subagents
  unless the developer explicitly asks for it in the current task (e.g. "use
  subagents", "dispatch these in parallel", "delegate this"). A task looking
  parallelizable is not authorization, and neither is a past instruction. If
  parallel work would clearly help, ask once; absent a yes, do the work yourself,
  sequentially. Exception: delegating an output-heavy command to a cheap model
  (see "Output-heavy commands") needs no opt-in.
- **Cheap models for subagents.** The coordinating session runs the expensive
  model; helpers run a cheap/fast one: ChatGPT Luna, DeepSeek v4.1 Flash, Claude
  Sonnet, or Muse-Spark. Pick the cheapest that can do the job, never silently a
  premium model, and state the chosen model in the dispatch and the summary.
- **Poll at the harness cadence.** Polling too often destroys prompt-cache
  retention. Claude Code: check every 4 minutes. Codex and opencode running
  OpenAI models: check every 25 minutes, otherwise wait for completion, and do
  not poll faster. Manual polling only — no tight auto-poll loops. If a result
  arrives earlier, use it immediately.
