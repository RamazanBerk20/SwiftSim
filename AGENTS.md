# SwiftSim: notes for coding agents

SwiftSim is an open-source LTspice alternative: a SPICE engine (Rust library) plus a native
desktop app (eframe/egui 0.37) for Linux and Windows. Single binary, MIT OR Apache-2.0.
Repo: https://github.com/RamazanBerk20/SwiftSim (public). Read `README.md` for the feature
list and the "Not yet supported" section (the roadmap is whatever is listed there).

## Map

| Path | What lives there |
|---|---|
| `src/expr.rs` | SI numbers (`4k7`, `1meg`), expression parser/evaluator, `.func`, Monte Carlo functions |
| `src/solver.rs` | MNA matrix + LU: dense ≤ 32 unknowns, sparse Gilbert-Peierls LU above (pattern learned from stamps, refactor reuses pivots); complex for AC; transpose for noise |
| `src/devices.rs` | `Device` trait: `load` (DC/tran Newton stamp), `load_ac`, `noise`, `currents`; all device models |
| `src/netlist.rs` | LTspice-dialect parser, `.include/.lib` (incl. LTspice's installed library), `.subckt` flattening, models, built-in model library, `.step`/`.param` |
| `src/sim.rs` | `.op .dc .ac .tran .tf .noise`, `.step` orchestration, OP/noise/.four reports |
| `src/measure.rs` | `.meas`, `.four`, plot-trace expressions |
| `src/main.rs` | CLI: GUI by default, `-b file` batch mode |
| `src/gui/mod.rs` | app shell, menus, panels, simulation thread, examples, file I/O |
| `src/gui/schematic.rs` | schematic model, symbols/pins, netlister, file format + `upgrade()` |
| `src/gui/editor.rs` | canvas: tools, selection, wiring, undo, probing, drawing |
| `src/gui/waveform.rs` | plots (V/A panes, Bode, noise, FFT), cursors, CSV |
| `src/gui/asc.rs` | LTspice `.asc`/`.asy` import |
| `src/gui/session.rs` | autosave/crash recovery, recent files |
| `src/gui/update.rs` | update check against GitHub releases |
| `tests/engine.rs` | engine tests against analytic or hand-calculated results |
| `tests/reference/` | ngspice comparison suite (`compare.py` + netlists) |
| `packaging/` | Linux desktop/MIME/AppImage, Windows Inno Setup, AUR PKGBUILDs |

## Build, run, test

```bash
cargo build --release
cargo test --release                       # all unit + engine + GUI-logic tests
python3 -I tests/reference/compare.py      # needs ngspice on PATH; must stay "N/N circuits match"
./target/release/swiftsim -b file.cir      # batch: .cir/.net, .sws or .asc; -o out.csv, --netlist
cargo test --release --bin swiftsim update:: -- --ignored   # live GitHub request (network)
```

Rust 1.89+ (edition 2024; `File::try_lock`). The release profile uses thin LTO.

## Rules that keep the project correct

- **Engine changes need a test with an independent answer.** Add it to `tests/engine.rs`
  (analytic formula or hand calculation, not a value copied from SwiftSim's own output).
  Never loosen an existing tolerance to make a test pass. Run `compare.py` after any change to
  devices, solver, or analyses; it must still report all circuits matching.
- **SPICE conventions:** node `0`/`gnd` is ground (unknown index 0, dropped by the stamping
  helpers). Device currents are positive into the first terminal (`I(V1)` into +, `Ic` into the
  collector). Netlists are lowercased on parse. LTspice dialect details matter: `;` comments,
  1 mΩ default inductor `Rser`, `.param a 1` without `=`, `ako:` models, `.step` variables usable
  inside `.param` and the analysis line.
- **Schematic file format is versioned.** `Schematic::FORMAT` and `Schematic::upgrade()` in
  `schematic.rs`. If you change a symbol's pin layout or anything that changes how a saved file
  wires up, bump `FORMAT` and add an upgrade step that keeps every pin position, plus a test
  (see `old_files_keep_op_amp_pins_when_upgraded`). Op-amps have + on top since format 1.
- **egui panels adopt their content's size every frame.** Content a few pixels taller or shorter
  than the panel makes it creep. The results panel pins its content to the panel rect; keep
  that pattern for any resizable panel whose content fills it.
- **Never block the UI thread:** simulations and the update check run on threads and report
  over channels; the app calls `request_repaint` when they finish.
- **The update check** (`update.rs`) only reads the public release list. Errors from the
  automatic check stay silent. Release candidates are offered newer candidates; stable builds
  only stable releases.

## Testing the GUI on a developer machine

The maintainer may be running SwiftSim on the same machine. Never `pkill swiftsim` by name;
start your instance with `& PID=$!` and kill that PID. Point test runs at a scratch config
(`XDG_CONFIG_HOME=/some/tmp/dir`) so you don't write the user's recent files, settings or
recovery copies in `~/.config/swiftsim`. On KDE Wayland, run the app under XWayland
(`env -u WAYLAND_DISPLAY`) to drive it with `xdotool` and capture with `spectacle -b -n -a -o`.

## Git and privacy

Commits must use the GitHub noreply address `119219364+RamazanBerk20@users.noreply.github.com`
(set in this repo's git config); never commit with a personal email. End commit messages with
the `Co-Authored-By:` line your harness asks for. Commit or push only when the maintainer asks.

## Releases

1. Make sure `master` is pushed and CI (`.github/workflows/ci.yml`, Linux + Windows) is green.
2. Tag the CI-green commit: `git tag -a vX.Y.Z -m "SwiftSim X.Y.Z"` and push the tag. A tag with
   a hyphen (`v0.2.0-rc1`) becomes a pre-release.
3. `release.yml` builds the Windows installer + zip and the Linux AppImage + tarball, stamps the
   tag into the binary (`SWIFTSIM_VERSION`) and publishes the GitHub Release.
4. Bump `version` in `Cargo.toml` when the release number changes.
5. AUR (`packaging/aur/`): for `swiftsim` and `swiftsim-bin` set `pkgver`, reset `pkgrel=1`, run
   `updpkgsums` and `makepkg --printsrcinfo > .SRCINFO`, build-test with `makepkg -f`, then
   commit and push each to `ssh://aur@aur.archlinux.org/<name>.git` (key `~/.ssh/aur`,
   passphrase-protected: the maintainer unlocks it). `swiftsim-git` only changes when its
   build recipe does.

## Where the app keeps data

Config folder: `%APPDATA%\SwiftSim` or `$XDG_CONFIG_HOME`/`~/.config/swiftsim`: `settings.ron`
(eframe persistence), `recent.txt`, `ltspice-lib.txt`, `recovery-<pid>.json/.lock`.
LTspice's model library is read at run time from an installed LTspice (auto-detected, or the
folder chosen under Simulate) and is never copied into the repo.
