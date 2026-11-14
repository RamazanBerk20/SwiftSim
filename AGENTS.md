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
| `src/sim.rs` | `.op .dc .ac .tran .tf .noise`, `.step` orchestration (parallel points, see below), OP/noise/.four reports |
| `src/raw.rs` | LTspice-compatible binary `.raw` export (`-b --raw`, File menu, MCP `simulate` `raw`); `tests/raw.rs` reads it back with an independent reader |
| `src/measure.rs` | `.meas`, `.four`, plot-trace expressions |
| `src/main.rs` | CLI: GUI by default, `-b file` batch mode, `--mcp` server |
| `src/mcp.rs` | MCP server on stdio for coding agents: `simulate`, `schematic_netlist`, `open_in_gui`, `gui_status`, and `gui_netlist`/`gui_run`/`gui_results`/`gui_set_params`/`gui_last_error` that drive an open window |
| `src/gui/mod.rs` | app shell, menus, panels, simulation thread, examples, file I/O |
| `src/gui/schematic.rs` | schematic model, symbols/pins, netlister, file format + `upgrade()` |
| `src/gui/editor.rs` | canvas: tools, selection, wiring, undo, probing, drawing |
| `src/gui/waveform.rs` | plots (V/A panes, Bode, noise, FFT), cursors, CSV |
| `src/gui/asc.rs` | LTspice `.asc`/`.asy` import |
| `src/gui/session.rs` | autosave/crash recovery, recent files |
| `src/gui/update.rs` | update check against GitHub releases, in-place install |
| `src/gui/i18n.rs` | UI languages (`tr`, `trf`, `para`), mirrored layout helpers; texts in `src/gui/lang/<code>.json` |
| `src/gui/bidi.rs` | Unicode bidi on top of egui: `display` for labels, `line`/`code` text fields with correct caret and selection |
| `src/gui/fonts.rs` | bundled color emoji (always), system fonts, then the bundled last-resort fonts (`assets/fonts/fallback.bin`, built by `assets/fonts/build.py`) |
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
  over channels; the app calls `request_repaint` when they finish. Engine threads use
  `sim::STACK`. App state lives in explicit state machines (`gui/state.rs`: document, simulation
  job, dialogs; `update::Updater`; `editor::Tool`; `mcp::Requests`): change them through their
  transition functions, and replace the document only via `App::replace_document`. Each window
  writes `window-<pid>.json` to the config folder for the MCP `gui_status` tool.
- **`.step` points run in parallel** (`sim::run`): scoped threads (`sim::workers`: cores − 1, capped by
  `SWIFTSIM_JOBS`, or `Control::jobs`), each building its own circuit; results are taken in step
  order and moved, never copied. Output must stay bit-for-bit equal to a serial run
  (`parallel_step_matches_serial_exactly`); every failing point is reported. Anything an engine
  thread shares must be `Sync` (no `Rc`/thread-locals carrying results between points). Monte Carlo
  draws come from `expr::seed_random(point_seed(run seed, point index))` at the start of each point,
  never from the thread: `Control::seed` / `SWIFTSIM_SEED` repeat a run.
- **Live parameters** (`state::Live`, Parameters panel): slider values override numeric `.param`s
  through `netlist::set_params` on the netlist text of a run, never the schematic until *Apply*. A
  new value while a run is in flight cancels it (`SimEvent::Cancel`) and sets `Live::rerun`; the
  next run starts when the cancelled one is done, so at most one waits. `replace_document` clears it.
- **Agents driving a window**: `gui_*` MCP requests are files `req-<pid>-<id>.json` / `resp-…` in
  `session::ipc_dir` (`$XDG_RUNTIME_DIR/swiftsim` 0700, files 0600; `%APPDATA%\SwiftSim\ipc` on
  Windows, not verified there), read by a thread in the window and answered in `App::agent`. Each request
  carries a `deadline_ms` (the window drops it unexecuted after that), `gui_run`/`gui_set_params`
  carry the document `generation` the agent read (refused on mismatch), and a window removes its
  own request files at start and exit. An agent
  request never saves, opens, closes or replaces the document, and is refused while a modal is open or
  a mouse button is down; the toolbar shows "Driven by an agent" for 15 s after each request. Never
  add a network listener.
- **Untrusted input never crashes:** expressions are depth-limited (`expr::MAX_DEPTH`), point and
  harmonic counts and subcircuit expansion are capped; add a fixture to `tests/fixtures/audit/`
  for any new crash.
- **Updates** (`update.rs`): errors from the automatic check stay silent. Release candidates are
  offered newer candidates; stable builds only stable releases. *Install update* only takes files
  from this repo's release page, checks the size and SHA-256 against the release's `SHA256SUMS`
  (published by `release.yml`) before running anything, then requires `--help`'s first line to be
  exactly `SwiftSim <tag>`. The replaced program stays as `<file>.old` until the new one has run for
  twice `RESTART_GRACE` (then `cleanup()`; never at startup), and "Restart now" puts it back if the
  new one exits with an error before that.
  `update::Updater` is the one state machine for check/install/restart. The tools `release.yml`
  downloads are pinned to numbered releases and checked against `packaging/linux/tools.sha256`. Only stamped release builds (`SWIFTSIM_VERSION`)
  outside `/usr` update themselves. The asset names in `plan()` must match `release.yml` and
  `package.sh`; the tarball's layout must match `unpack()`.
- **UI text goes through `i18n`.** The English text is the key: `tr("Save")`, `trf("Saved {}", &[&p])`,
  and `para(...)` for paragraphs that wrap in a side panel. A new or changed text needs an entry in
  every `src/gui/lang/*.json`; `every_language_covers_every_text` fails otherwise. Engine log lines
  stay English (the log colours lines by their `warning`/`error` prefix).
- **Node names compare by `expr::fold`** (NFC, case-folded, `ı` and `İ` count as `i`), never
  `to_ascii_lowercase`, and never by a locale. Non-ASCII names keep their first spelling in results
  (`Deck::spelling`); `Plot::names` holds that spelling, so look signals up with `Plot::index`.
- **Right-to-left UI.** Wrap a new panel or dialog's contents in `i18n::mirrored`, two-column forms
  in `i18n::form_row`, and use `i18n::para` for wrapping paragraphs. User-editable text goes through
  `bidi::line` or `bidi::code`, not a bare `TextEdit` (they also keep the caret out of multi-part
  emoji); the canvas draws names through `bidi::display`.
  The schematic, plots, netlists and the Log stay left to right.

## Testing the GUI on a developer machine

The maintainer may be running SwiftSim on the same machine. Never `pkill swiftsim` by name;
start your instance with `& PID=$!` and kill that PID. Point test runs at a scratch config
(`XDG_CONFIG_HOME=/some/tmp/dir`) so you don't write the user's recent files, settings or
recovery copies in `~/.config/swiftsim`.

Never open windows, send input or take screenshots on the maintainer's desktop. Run the GUI in a
private headless KWin instead: `dbus-run-session -- kwin_wayland --virtual --xwayland --socket wl`
with its own `XDG_RUNTIME_DIR`, `HOME` and `XDG_CONFIG_HOME`, and give the app only that session's
`WAYLAND_DISPLAY`, `DISPLAY` and `DBUS_SESSION_BUS_ADDRESS` (check they are not `wayland-0/1`,
`:0/:1` or `/run/user/<uid>/bus`). Open and run circuits through the MCP tools (`open_in_gui`,
`gui_run`); for pointer input start the window on the nested Xwayland (no `WAYLAND_DISPLAY`) and use
`xdotool` with the nested `DISPLAY` (`XwaylandEisNoPrompt=true` under `[Xwayland]` in the nested
`kwinrc` skips KWin's permission prompt). Capture with `spectacle -b -n -f -o` run in the nested
session (`KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1` in KWin's environment).
`scripts/screenshots.sh` does all of this and regenerates `docs/transient.png` and `docs/ac.png`.

## Git and privacy

Commits must use the GitHub noreply address `119219364+RamazanBerk20@users.noreply.github.com`
(set in this repo's git config); never commit with a personal email. End commit messages with
the `Co-Authored-By:` line your harness asks for. Commit or push only when the maintainer asks.

## Releases

1. Make sure `master` is pushed and CI (`.github/workflows/ci.yml`, Linux + Windows) is green.
2. Tag the CI-green commit: `git tag -a vX.Y.Z -m "SwiftSim X.Y.Z"` and push the tag. A tag with
   a hyphen (`v0.3.0-rc1`) becomes a pre-release.
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
(eframe persistence), `recent.txt`, `ltspice-lib.txt`, `recovery-<pid>.json/.lock`, `window-<pid>.json`.
LTspice's model library is read at run time from an installed LTspice (auto-detected, or the
folder chosen under Simulate) and is never copied into the repo.
