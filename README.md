# SwiftSim

A fast, open-source SPICE circuit simulator with a modern schematic editor and
waveform viewer — an alternative to LTSpice for Linux and Windows.
Single native binary, written in Rust (egui GUI, own simulation engine).

![Transient run with voltage and current panes](docs/transient.png)
![AC analysis Bode plot](docs/ac.png)

## Build & run

Requires Rust 1.85+ (edition 2024).

```bash
cargo build --release
```

```bash
./target/release/swiftsim                  # GUI (opens an example)
./target/release/swiftsim my.sws           # open a schematic
./target/release/swiftsim my.cir           # open a SPICE netlist in the text editor
./target/release/swiftsim -b my.cir -o out.csv   # batch run, log + CSV export
./target/release/swiftsim -b my.asc              # batch-run an LTspice schematic
```

On Windows the same commands produce `swiftsim.exe` (no console window in release builds).

## What works today

**Simulator engine** (`src/`, usable as a library)
- Analyses: `.op`, `.tran` (adaptive trapezoidal with LTE control, breakpoints, `uic`),
  `.ac` (dec/oct/lin), `.dc` (one or two nested sweeps), `.tf V(out[,ref]) | I(Vx) <src>`
  (DC small-signal gain, input and output impedance)
- `.four f0 [Nharmonics] [Nperiods] V(out) …` after `.tran`: harmonics, phases and THD in the Log
- Temperature: `.temp t1 [t2 …]` (several values step them), `.step temp …`, `.options tnom=`;
  `temp` is usable in expressions. SPICE3 scaling of diode/BJT saturation currents (`XTI`, `EG`),
  junction potentials and capacitances, BJT beta (`XTB`), level-1 MOSFET VTO/KP, thermal voltage,
  resistor `tc1=`/`tc2=` (or `tc=a,b`)
- `.step` (param / source / element / temp; lin, dec, oct, list; nested), `.meas`
  (AVG, MAX, MIN, PP, RMS, INTEG, FIND…AT, FIND…WHEN, WHEN, TRIG/TARG, PARAM),
  `.param` with expressions, `.func` user functions (global or per `.subckt`), `.ic`,
  `.options`, `.model`, `.subckt` with `params:`,
  `.include` / `.lib` (incl. sections)
- Elements: R, C (Rser/Rpar, IC), L (Rser, IC), K (mutual coupling), V/I with DC, AC,
  PULSE, SIN/SINE, EXP, PWL, SFFM (V: Rser), D (junction caps, breakdown/zener),
  Q (Gummel-Poon DC, junction/diffusion charge, RB/RC/RE), M (level 1 and LTSpice VDMOS
  with body diode), J (NJF/PJF JFET: VTO, BETA, LAMBDA, IS, N, RD, RS, CGS, CGD, PB, M, FC),
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
- Simulation runs in the background with progress and Stop
- Probe: click a wire → voltage, click a part → current; labeled nets auto-plot
- Waveforms: separate V and A panes on a shared axis, Bode plots (dB + phase, log
  axis), stepped runs overlaid, arbitrary trace expressions (`V(a)-V(b)`,
  `I(R1)*V(out)`), FFT, two cursors with Δ and 1/Δ readout, CSV export
- `.op` voltages annotated on the schematic; Log shows OP tables, `.meas`, warnings
- Netlist mode: edit and run any SPICE netlist directly (LTSpice `.net/.cir` files)
- LTspice `.asc` import (File ▸ Open): stock symbols (R, C, L, diodes, sources, BJT, MOSFET,
  JFET, E/G, switch, ideal opamp); other symbols are listed in the Log and left out
- Dark / light theme; schematics saved as JSON (`.sws`)

### Keyboard

| Key | Action | Key | Action |
|---|---|---|---|
| R C L D V I Q M U | place part | Ctrl+R / Ctrl+E | rotate / mirror |
| G / N | ground / net label | Ctrl+Z / Ctrl+Y | undo / redo |
| W or F3 | wire | Ctrl+C / V / X / D | copy / paste / cut / duplicate |
| T or `.` | SPICE directive | Del | delete |
| P | probe | Space | zoom to fit |
| Esc | cancel / select tool | F5 | run |

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

## Not yet supported (compared with LTSpice)

Being explicit, since the goal is LTSpice parity:

- Analyses: `.noise`
- Temperature: no effect on C/L values, diode BV/RS, BJT RB/RC/RE, VDMOS VTO (VDMOS only scales KP)
  or JFET VTO/BETA (JFET gate junctions do scale); no per-model `TNOM`
- Devices: MESFET, transmission lines (T/O), current-controlled switch (W),
  MOSFET levels above 1 / BSIM (they run with level-1 equations and a warning in the Log),
  LTSpice digital/XSPICE `A` devices, E/G `laplace=`
- JFET extras: gate recombination current (ISR/NR), impact ionization (ALPHA/VK), the
  B (doping tail) parameter and noise parameters are ignored
- `.func` functions are not visible in `.meas` expressions
- `.asc` import of non-stock `.asy` symbols (vendor parts, custom symbols); hierarchical schematics
- Sparse matrix solver: the engine uses dense LU with sparse-aware elimination —
  fast up to a few hundred nodes, slow for multi-thousand-node netlists
- VDMOS gate-drain capacitance is constant (LTSpice's is voltage-dependent); level-1
  MOSFETs have constant (non-Meyer) capacitances
- Waveform-file export in `.raw` format

## Layout

```
src/expr.rs      numbers with SI suffixes, expression parser/evaluator
src/solver.rs    MNA matrix, LU (real + complex)
src/devices.rs   device models and stamps
src/netlist.rs   parser, subcircuit flattening, models, built-in library
src/sim.rs       OP / DC / AC / transient, .step orchestration
src/measure.rs   .meas and plot expressions
src/gui/         app shell, schematic model + netlister, canvas editor, waveform viewer
tests/engine.rs  analytic checks (RC/RL/LC, AC, diodes, BJT, MOS, JFET, POLY/table/.func, .meas …)
tests/models/    public vendor macromodels used by the tests
```

```bash
cargo test --release
```
