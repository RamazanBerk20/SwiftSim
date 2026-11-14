# SwiftSim

A fast, open-source SPICE circuit simulator with a modern schematic editor and
waveform viewer — an alternative to LTSpice for Linux and Windows.
Single native binary, written in Rust (egui GUI, own simulation engine).

![Transient run with voltage and current panes](docs/transient.png)
![AC analysis Bode plot](docs/ac.png)

## Install

Prebuilt x86-64 packages are attached to each
[release](https://github.com/RamazanBerk20/SwiftSim/releases).

**Windows**
- `SwiftSim-setup.exe`: installer. Installs for the current user (no admin needed),
  adds a Start menu entry and opens `.sws` files with SwiftSim. It isn't code-signed, so
  SmartScreen may warn: *More info → Run anyway*.
- `swiftsim-windows-x86_64.zip`: portable. Unzip anywhere and run `swiftsim.exe`;
  double-click `register-sws.cmd` once to open `.sws` files with that copy.

**Linux**
- `SwiftSim-x86_64.AppImage`: `chmod +x SwiftSim-x86_64.AppImage`, then run it.
  Needs FUSE (like every AppImage) and glibc 2.39 or newer (Ubuntu 24.04, Fedora 40, current Arch).
- `swiftsim-linux-x86_64.tar.gz`: extract and run `./install.sh`. It installs to `~/.local`
  with a menu entry, icon and `.sws`/`.asc` file types (`sudo PREFIX=/usr/local ./install.sh`
  installs for all users). To uninstall, delete `~/.local/bin/swiftsim`,
  `~/.local/share/applications/swiftsim.desktop`, `~/.local/share/mime/packages/swiftsim.xml`
  and `~/.local/share/icons/hicolor/*/apps/swiftsim.*`.
- Arch Linux (AUR): `swiftsim-bin` (release binary), `swiftsim` (built from the release
  source) or `swiftsim-git` (latest `master`), e.g. `yay -S swiftsim-bin`.

The AppImage, the tarball install and the Windows packages update themselves: when a newer release
exists, *Install update* downloads it, replaces the program and offers to restart (a copy in Program
Files shows the administrator prompt). AUR and other package-manager installs update through the
package manager, and builds from source only link to the release.

Making a release: set `version` in `Cargo.toml`, then push a tag such as `v0.3.0`.
`.github/workflows/release.yml` builds, tests and packages on Linux and Windows and publishes a
GitHub Release with generated notes (tags containing `-`, e.g. `v0.3.0-rc1`, become pre-releases).

## Build & run

Requires Rust 1.89+ (edition 2024; crash recovery uses `File::try_lock`).

```bash
cargo build --release
```

```bash
./target/release/swiftsim                  # GUI (opens an example)
./target/release/swiftsim my.sws           # open a schematic
./target/release/swiftsim my.cir           # open a SPICE netlist in the text editor
./target/release/swiftsim -b my.cir -o out.csv   # batch run, log + CSV export
./target/release/swiftsim -b my.asc              # batch-run an LTspice schematic
./target/release/swiftsim -b my.sws --netlist    # batch-run a saved schematic, print its netlist
./target/release/swiftsim -b my.cir --raw out.raw  # also write an LTspice-compatible .raw file
```

`.step` points run in parallel, one thread per core but one; `SWIFTSIM_JOBS=n` caps the threads
(`SWIFTSIM_JOBS=1` runs them one at a time). Results are identical either way and come in step order.
Monte Carlo functions (`rand`, `mc`, `flat`, `gauss`) draw a new seed each run; each step point gets
its own stream from it, so `SWIFTSIM_SEED=<number>` repeats a run exactly, on any number of threads.

On Windows the same commands produce `swiftsim.exe` (no console window in release builds).

## What works today

**Simulator engine** (`src/`, usable as a library)
- Analyses: `.op`, `.tran` (adaptive trapezoidal with LTE control, breakpoints, `uic`),
  `.ac` (dec/oct/lin), `.dc` (one or two nested sweeps), `.tf V(out[,ref]) | I(Vx) <src>`
  (DC small-signal gain, input and output impedance),
  `.noise V(out[,ref]) <src> dec|oct|lin <n> <fstart> <fstop>`: `V(onoise)` and `V(inoise)`
  (output and input-referred density, V/√Hz) and each part's share of the output noise as
  `V(<part>)`; integrated totals and per-part shares in the Log. Noise sources: resistor
  thermal 4kT/R (also RS/RB/RC/RE/RD/RG series resistors), diode shot 2qI + flicker
  `KF·I^AF/f`, BJT collector and base shot + base flicker, MOSFET/JFET/VDMOS channel thermal
  8/3·kT·gm + flicker (`KF`, `AF`; level-1 MOSFETs follow ngspice's `NLEV` 0–3, and without
  `NLEV` use SPICE3's `KF·Id^AF/(f·W·L·Cox²)`, Cox from `TOX` or 100 nm). Solved by the adjoint
  method: one transposed small-signal solve per frequency
- `.four f0 [Nharmonics] [Nperiods] V(out) …` after `.tran`: harmonics, phases and THD in the Log
- Temperature: `.temp t1 [t2 …]` (several values step them), `.step temp …`, `.options tnom=`;
  `temp` is usable in expressions. SPICE3 scaling of diode/BJT saturation currents (`XTI`, `EG`),
  junction potentials and capacitances, BJT beta (`XTB`), level-1 MOSFET VTO/KP, thermal voltage,
  resistor `tc1=`/`tc2=` (or `tc=a,b`)
- `.step` (param / source / element / temp; lin, dec, oct, list; nested; points run in parallel), `.meas`
  (AVG, MAX, MIN, PP, RMS, INTEG, FIND…AT, FIND…WHEN, WHEN, TRIG/TARG, PARAM),
  `.param` with expressions, `.func` user functions (global or per `.subckt`), `.ic`,
  `.options`, `.model`, `.subckt` with `params:`,
  `.include` / `.lib` (incl. sections)
- Elements: R, C (Rser/Rpar, IC), L (Rser, IC), K (mutual coupling), V/I with DC, AC,
  PULSE, SIN/SINE, EXP, PWL, SFFM (V: Rser), D (junction caps, breakdown/zener),
  Q (Gummel-Poon DC, junction/diffusion charge, RB/RC/RE), M (level 1 with bulk junction
  diodes, LTSpice VDMOS with body diode and `KSUBTHRES`), J (NJF/PJF JFET: VTO, BETA, LAMBDA, IS, N, RD, RS, CGS, CGD, PB, M, FC),
  E/F/G/H incl. `POLY(n)`, E/G `value={…}` and `table=(…)` / `TABLE {expr}=(…)`,
  B (behavioral V=/I= with `V()`, `I()`, `time`, `if()`, math), S (voltage switch with
  hysteresis), X (subcircuits)
- Vendor macromodels in the classic PSpice style run unmodified, e.g. TI's TL072 and
  LM358 (checked in `tests/models/`)
- Expressions: `+ - * / ** ^`, comparisons, `&& ||`, `?:`, `if`, `min/max/limit`,
  trig/exp/log, `u()`, `uramp()`, `table(x, x1,y1, …)`, Monte Carlo `mc() gauss() flat() rand()`
- LTSpice conventions: SI suffixes (`4k7`, `1meg`, `10uF`), `;` comments, inductor
  default Rser = 1 mΩ, current sign conventions (I(V1) into +, Ic into collector…)
- Convergence aids: pn-junction and FET voltage limiting, gmin stepping, source stepping
- Built-in parts: 1N4148, 1N4007, 1N5819, zeners, LEDs, 2N3904/3906, 2N2222/2907,
  BC547B/557B, TIP31C/32C, 2N7000, BS170, BSS84, IRF540N, IRF9540N, IRLZ44N,
  generic NMOS/PMOS (approximate datasheet-level models)

**Desktop app**
- Schematic editor: part palette with search and icons, grid snapping, wires with
  L-routing and T-junction detection, net labels, ground, SPICE directive text,
  rotate/mirror, box select, rubber-band wires when dragging parts, copy/paste,
  undo/redo, zoom/pan/fit, junction dots, unconnected-pin markers
- Properties panel: values, model picker (built-in + your `.model`s), structured
  source editor (DC / Pulse / Sine / Exp / PWL + AC), simulation-command form
- Simulation runs in the background with progress (`.step` runs count their points) and Stop
- Parameters panel: every numeric top-level `.param` of the schematic gets a slider (a decade
  either side, log scale; the range is editable) and a value field. Moving one re-runs the last
  simulation and redraws the plot; a run still going is stopped and replaced by the newest value.
  The schematic changes only with *Apply to schematic* (one undoable edit); *Reset* goes back
  to the schematic's values. F6 focuses the first slider, the arrow keys move it
- File ▸ Export results as LTspice .raw: binary `.raw` (transient, AC, DC sweep, noise,
  operating point, stepped runs) for LTspice and other `.raw` readers
- Probe: click a wire → voltage, click a part → current (after `.noise`: its noise
  contribution); labeled nets auto-plot
- Waveforms: separate V and A panes on a shared axis, Bode plots (dB + phase, log
  axis), noise spectra (log-log, V/√Hz), stepped runs overlaid, arbitrary trace expressions (`V(a)-V(b)`,
  `I(R1)*V(out)`), FFT, two cursors with Δ and 1/Δ readout, CSV export
- `.op` voltages annotated on the schematic; Log shows OP tables, `.meas`, warnings
- Netlist mode: edit and run any SPICE netlist directly (LTSpice `.net/.cir` files)
- Ten interface languages, with a mirrored layout in Arabic; net labels and text in any script,
  and color emoji (see [Languages](#languages))
- *View → Reset layout* returns the panels to their default sizes and leaves the window as it is
- Updates install from inside the app on Linux and Windows
- LTspice `.asc` import (File ▸ Open): stock symbols (R, C, L, diodes, sources, BJT, MOSFET,
  JFET, E/G, switch, ideal opamp), plus subcircuit symbols (`Prefix X`: vendor opamps, your
  own `.asy`) read from the `.asy` next to the schematic or in LTspice's `lib/sym`; they
  become boxed `X` parts with the symbol's pins and a `.lib` line for its model file.
  Other symbols are listed in the Log and left out
- Dark / light theme; schematics saved as JSON (`.sws`)
- Update notice: at startup the app reads SwiftSim's public release list on GitHub and shows a
  banner when a newer release exists (release candidates see newer candidates, stable builds only
  stable releases). It sends only a `SwiftSim/<version>` user agent, downloads nothing, and can
  be turned off under Help ("Check for updates at startup"); Help also has "Check for updates now"
- Crash recovery: while a document has unsaved changes, a recovery copy is written every
  30 s and on every run (never over your own file). If SwiftSim crashes or is killed, the next
  start asks whether to restore it or discard it; saving or closing normally deletes it
- File ▸ Open recent (last 10 files); theme, window size and position, panel sizes, the
  operating-point toggle and the last folder used are remembered. All of it lives in
  `%APPDATA%\SwiftSim` or `~/.config/swiftsim` (`recovery-*.json`, `recent.txt`, `settings.ron`)

### Keyboard

| Key | Action | Key | Action |
|---|---|---|---|
| R C L D V I Q M U | place part | Ctrl+R / Ctrl+E | rotate / mirror |
| G / N | ground / net label | Ctrl+Z / Ctrl+Y | undo / redo |
| W or F3 | wire | Ctrl+C / V / X / D | copy / paste / cut / duplicate |
| T or `.` | SPICE directive | Del | delete |
| P | probe | Space | zoom to fit |
| Esc | cancel / select tool | F5 | run |
| F6 | Parameters panel (arrows move a slider) | | |

Pan with middle or right drag, zoom with the wheel.

### LTspice model library

SwiftSim does not ship LTspice's model files, but it reads them from your LTspice install
at run time. A model or subcircuit that is not in the netlist or SwiftSim's built-ins is
looked up in LTspice's `lib/cmp/standard.dio`, `standard.bjt`, `standard.mos` and
`standard.jft`; relative `.lib`/`.include` paths that don't exist next to the file are also
tried in `lib/sub/` and `lib/cmp/`. Vendor-encrypted model files can't be simulated; they
are named in the Log and in the error for the part that needs them.

The `lib` folder is found automatically in the usual places:

- Windows: `%LOCALAPPDATA%\LTspice\lib` (LTspice 24) or `Documents\LTspiceXVII\lib`
- Linux (Wine): the same folders under `$WINEPREFIX` (default `~/.wine`)`/drive_c/users/$USER/`
- macOS: `~/Library/Application Support/LTspice/lib`

To use another folder, pick it under **Simulate ▸ Choose LTspice library folder…** (saved in
`%APPDATA%\SwiftSim\ltspice-lib.txt` or `~/.config/swiftsim/ltspice-lib.txt`), or for batch runs
set `SWIFTSIM_LTSPICE_LIB`:

```bash
SWIFTSIM_LTSPICE_LIB=~/.wine/drive_c/users/$USER/AppData/Local/LTspice/lib ./target/release/swiftsim -b my.asc
```

## Languages

The interface comes in English, Turkish, Spanish, French, German, Russian, Arabic, Japanese, Chinese and
Korean: it follows the system language, or pick one under *View → Language*. In Arabic the interface is
mirrored; the schematic, plots and netlists stay left-to-right, as circuit diagrams and code do.

Net labels and text can use any script. Fonts for nearly every writing system are built in and used only
when your system has none for a script. Right-to-left text displays and edits correctly in every field.
Emoji work in net labels and text too, in color on every machine, including flags, skin tones and
family sequences.

Net names ignore case in every script, on every machine: `ÇIKIŞ` and `çıkış` are one net, and results
keep the spelling you used. An emoji is the same name with or without its invisible variation selector
(`❤` and `❤️`). Because SPICE needs `I` to equal `i` while Turkish pairs `I` with `ı`,
`ı` and `İ` count as `i`; different words that collide this way (`kır` and `kir`) stay one net and the
Log warns about them.

## Coding agents (MCP)

`swiftsim --mcp` is a [Model Context Protocol](https://modelcontextprotocol.io) server on stdio, so a
coding agent can run circuits and read the numbers. With Claude Code:

```bash
claude mcp add swiftsim -- swiftsim --mcp
```

Other clients take the same command in their MCP configuration. *Help → Copy MCP server command* copies
it with the full path to this copy, which is what you want on Windows or with the AppImage.

| Tool | What it does |
|---|---|
| `simulate` | Runs netlist text or a `.sws`/`.asc`/`.cir` file; returns the log, `.meas` results, signal names and sampled `traces` (`V(out)`, `I(R1)`, …; AC as dB and degrees); `csv` also writes every point to a file, `raw` an LTspice `.raw` file |
| `schematic_netlist` | The SPICE netlist of a `.sws` or `.asc` schematic |
| `open_in_gui` | Opens a circuit file in a new SwiftSim window |
| `gui_status` | Lists the open windows (`pid`, document, simulation state, live parameters, whether an agent drives it); can wait until a run is `finished` or `failed` |
| `gui_netlist` | The netlist of a window's document, its path and unsaved state, and its Parameters values |
| `gui_run` | Runs the window's simulation (with its Parameters values) and waits for it to end; needs the `generation` of the document you read |
| `gui_results` | The window's results, in the same shape as `simulate` |
| `gui_set_params` | Sets Parameters values, like the sliders: the last simulation re-runs; the schematic is not changed; needs `generation` |
| `gui_last_error` | The window's last simulation error and the error lines of its Log |

The `gui_*` tools work on the window the user is looking at (`pid` picks one when several are open).
Requests reach it through files in a folder only you can open: `$XDG_RUNTIME_DIR/swiftsim` (mode 0700,
files 0600; `ipc` in the config folder without `XDG_RUNTIME_DIR`), or `%APPDATA%\SwiftSim\ipc` on
Windows. Nothing listens on a network port. While an agent drives it, the window says so in its
toolbar. An agent never saves, opens or closes a file or touches unsaved edits, and its requests are
refused while a dialog is open or the mouse button is held down. A request the window can't take
before its deadline (15 s) is dropped without being carried out, and `gui_run`/`gui_set_params` are
refused when the window has another document than the `generation` the agent read.

## Not yet supported (compared with LTSpice)

Being explicit, since the goal is LTSpice parity:

- Noise: no resistor flicker noise or `noiseless` flag; JFET gate and level-1 MOSFET bulk
  junctions are noiseless (as in SPICE); level-1 `NLEV=3` keeps 8/3·kT·gm channel noise
  (ngspice's `GDSNOI` form is not implemented). LTspice's own MOSFET flicker formula is not verified
  (no LTspice here): without `NLEV` SwiftSim assumes SPICE3's, ngspice defaults to `NLEV=2`
- Temperature: no effect on C/L values, diode BV/RS, BJT RB/RC/RE, VDMOS VTO (VDMOS only scales KP)
  or JFET VTO/BETA (JFET gate junctions do scale); no per-model `TNOM`
- Devices: MESFET, transmission lines (T/O), current-controlled switch (W),
  MOSFET levels above 1 / BSIM (they run with level-1 equations and a warning in the Log),
  LTSpice digital/XSPICE `A` devices, E/G `laplace=`
- BJT: `XTF`/`VTF`/`ITF` (bias-dependent transit time) are ignored, and the `TF` charge is not
  divided by qb as in SPICE3 (storage times ~0.3% longer when `VAF`/`IKF` are set)
- Level-1 MOSFET bulk junctions take `IS` only (no `JS`·`AD`/`AS` area scaling)
- JFET extras: gate recombination current (ISR/NR), impact ionization (ALPHA/VK), the
  B (doping tail) parameter are ignored
- `.func` functions are not visible in `.meas` expressions
- `.asc` import: non-stock symbols other than subcircuits (`Prefix X`), the `.asy` drawing
  (subcircuits show as a box), hierarchical schematics
- VDMOS gate-drain capacitance is constant (LTSpice's is voltage-dependent); level-1
  MOSFETs have constant (non-Meyer) capacitances
- `.raw` export follows the published file layout but has not been opened in LTspice itself yet

## Layout

```
src/expr.rs      numbers with SI suffixes, expression parser/evaluator
src/solver.rs    MNA matrix, dense and sparse LU (real + complex)
src/devices.rs   device models and stamps
src/netlist.rs   parser, subcircuit flattening, models, built-in library
src/sim.rs       OP / DC / AC / noise / transient, .step orchestration
src/measure.rs   .meas and plot expressions
src/mcp.rs       MCP server for coding agents (swiftsim --mcp)
src/gui/         app shell, schematic model + netlister, canvas editor, waveform viewer,
                 recovery copies + recent files (session.rs), updates (update.rs),
                 UI languages (i18n.rs, lang/*.json), right-to-left text (bidi.rs),
                 fallback fonts (fonts.rs)
tests/engine.rs  analytic checks (RC/RL/LC, AC, diodes, BJT, MOS, JFET, POLY/table/.func, .meas …)
tests/models/    public vendor macromodels used by the tests
assets/          app icon (SVG source, PNGs, .ico; render.sh regenerates them), fallback
                 fonts (fonts/build.py regenerates them)
packaging/       Linux .desktop/MIME/install.sh/AppImage script, Windows installer, AUR PKGBUILDs
tests/reference/ accuracy suite against ngspice (compare.py + netlists)
```

```bash
cargo test --release
```

## Accuracy against ngspice

`tests/reference/` has 40 netlists (RC/RLC filters, rectifiers, BJT/MOSFET/JFET/VDMOS stages
and switches, CMOS logic, TI op-amp circuits, oscillators, `.dc` sweeps, `.step`, temperature,
`.noise` of resistors, diode, BJT, MOSFET for each `NLEV`, JFET and VDMOS)
and a script that runs each one in ngspice and in SwiftSim and compares every node voltage
and source current both report:

```bash
cargo build --release
python3 -I tests/reference/compare.py        # -v lists every signal; names select a subset
```

It needs `ngspice` on PATH (Arch `pacman -S ngspice`, Debian/Ubuntu `apt install ngspice`)
and only the Python standard library. Tolerances: `.op`/`.dc` 1e-6 relative at every point
(both run with reltol=1e-9), `.tran` 1% of each signal's range, `.ac` 0.1 dB and 1°,
`.noise` spectra and integrated totals 0.1 dB (they agree within 0.03 dB), oscillators by period. The remaining known differences are listed at the top of `compare.py`.

## Support

SwiftSim is free. If it is useful to you, you can
[sponsor its development](https://github.com/sponsors/RamazanBerk20) through GitHub Sponsors.

## License

SwiftSim is licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option.

The fonts compiled into the program (`assets/fonts/fallback.bin`, from the Noto project) are under the
[SIL Open Font License 1.1](LICENSE-OFL).

The model files in `tests/models/` (TI's LM358 and TL072 macromodels) are copyright Texas
Instruments and are distributed under TI's own terms, not under SwiftSim's licenses.
