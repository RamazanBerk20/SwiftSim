# Changelog

## 0.3.0

### Added
- `.step` points run in parallel (one thread per core but one). `SWIFTSIM_JOBS=1` runs them
  serially; results are identical and come in step order. `SWIFTSIM_SEED=<n>` repeats a Monte
  Carlo run exactly on any number of threads.
- Parameters panel: live sliders for numeric `.param`s. Moving one re-runs the last simulation;
  the schematic changes only with *Apply to schematic*. F6 focuses it.
- MCP tools that drive the open window: `gui_status`, `gui_netlist`, `gui_run`, `gui_results`,
  `gui_set_params`, `gui_last_error`. Requests travel through files in a private folder (no
  network port); an agent never saves, opens or closes files.
- LTspice-compatible binary `.raw` export (File menu, `-b --raw out.raw`, MCP `simulate` `raw`).
- `SHA256SUMS` is published with every release.
- A quiet GitHub Sponsors link (README, `FUNDING.yml`).

### Changed
- A failing `.step` point no longer stops the sweep; every failing point is reported.
- The in-app updater verifies the download (size and SHA-256 against the release's `SHA256SUMS`),
  checks that the new program reports the expected version, keeps the old one as `<file>.old` and
  restores it if the new one exits with an error right after restart. There is no signature:
  trust rests on the GitHub release page. Releases without `SHA256SUMS` (0.2.0 and older) cannot
  be installed by this updater.
- Release tooling is pinned to numbered releases and checked against `packaging/linux/tools.sha256`.
- The source tree was reformatted with rustfmt; CI now checks formatting.

### Fixed
- Currents through time-dependent sources were wrong.
- Untrusted netlists, `.asc` files and expressions can no longer crash the program (depth, point,
  harmonic and subcircuit-expansion caps; new fixtures).
- Saves are atomic; crash recovery, autosave and recent files hardened.
- Many parser, `.meas`, `.four`, LTspice-import, text-input and editor fixes.

### Security
- Updates are hash-verified and roll back on failure (see Changed).
- Third-party GitHub Actions are pinned by commit; downloaded build tools are hash-checked.

### Not tested
- The Windows update path and the Windows IPC folder for `gui_*` tools.
- A real AppImage self-update.
- `.raw` files have not been opened in LTspice itself.
