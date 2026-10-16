# Linux GUI checklist

The Linux installer stub is only built and run on Linux, so this list is for a real
Linux machine. Report anything that differs from "expect".

## 1. Build

```sh
rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-gnu
sudo apt install build-essential mingw-w64 pkg-config \
    libxkbcommon-dev libwayland-dev libfontconfig-dev    # names differ on other distros
cargo test --release --workspace
cargo xtask dist
```

Expect `dist/stubs/` to hold all four stubs: `patcher-stub-linux`,
`patcher-stub-gui-linux`, `patcher-stub-windows.exe` and
`patcher-stub-gui-windows.exe`. If one is missing, the `warning:` line above the list says
why; send it along.

If linking `patcher-stub-gui` fails, the error names a missing `-l<library>`; install its
`-dev` package and run `cargo xtask dist` again (and say which one, so the list above can
be fixed).

## 2. A test patcher

```sh
cargo xtask gen-corpus --scale 0.3 --out /tmp/corpus
./dist/patcher create --base /tmp/corpus/base --modded /tmp/corpus/full-mod \
    --name "Neon Overhaul" --mod-version 2.1 -o /tmp/out
cp -r /tmp/corpus/base /tmp/game && cp /tmp/out/NeonOverhaul-patcher /tmp/game/
ldd /tmp/out/NeonOverhaul-patcher
```

Expect no "no Linux GUI stub" warning, and `ldd` listing only glibc libraries (libc, libm,
libgcc_s and similar). X11, Wayland and xkbcommon are loaded at run time, not linked.

## 3. The window

Run `/tmp/game/NeonOverhaul-patcher` from a file manager (double-click) and from a
terminal, once in an X11 session and once in a Wayland session if you have both.

| try | expect |
|---|---|
| start | window without a system frame, NEON in white and OVERHAUL in the lime tag, the glow drifting slowly |
| drag the empty top area | the window moves |
| minimise button, then restore | minimises; animation resumes |
| Next | folder page with `/tmp/game` found: "Game found, ready to patch" |
| edit the path to `/tmp` | "The game was not found in this folder" after a moment |
| Browse | the desktop's folder picker (xdg portal); choosing `/tmp/game` checks it |
| Patch | progress page, then Done; `diff -r /tmp/game /tmp/corpus/full-mod` shows only the patcher and `.patcher-backup` |
| Close, run again | welcome page offers "Remove mod"; it restores `/tmp/game` to `/tmp/corpus/base` |
| "Made with patcher" | opens the GitHub page in the browser |
| Copy details (force an error: `chmod -w /tmp/game/data`, then Patch) | error page; Copy details puts the text on the clipboard; `chmod +w` afterwards |

Without xdg-desktop-portal (some minimal window managers) Browse may do nothing; the path
field still works. Say if that happens.

## 4. Without a window

```sh
cd /tmp/game
./NeonOverhaul-patcher --verify     # prints the result in the terminal, exit code 0
./NeonOverhaul-patcher --silent .   # patches without a window
./NeonOverhaul-patcher --revert     # restores
```

## 5. Theme preview

```sh
./dist/patcher gui-preview --gui examples/gui/full-window/theme.toml
```

Expect the light sidebar layout. Change `accent` in that theme file and save: the window
reloads with the new colour within a second.

## What to send back

- the output of `cargo xtask dist` (the warnings and the size list);
- the `ldd` output;
- anything in the tables above that did not happen as described, with the desktop
  (GNOME, KDE...) and X11 or Wayland.
