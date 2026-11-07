#!/usr/bin/env python3
"""Accuracy regression: run every *.cir in this folder in ngspice and in SwiftSim
and compare all signals both report (node voltages, voltage-source and inductor
currents).

    cargo build --release
    python3 -I tests/reference/compare.py [-v] [name ...]

Needs `ngspice` on PATH. Standard library only. Exit status 1 if a circuit fails.

How it compares:
  .op         every value, |a-b| <= OP_RTOL*max(|a|,|b|) + OP_ATOL (V or A)
  .dc         as .op at every sweep point
  .tran       every sample of each run against the other run's samples around that
              instant, max error <= WAVE_RTOL * signal range (ngspice's)
  .ac         magnitude (dB) and phase (deg) at points within AC_FLOOR_DB of the peak
  .noise      V(onoise)/V(inoise) densities as .ac magnitudes (ngspice's onoise_spectrum /
              inoise_spectrum), and the integrated totals (SwiftSim's log) within AC_DB: output
              against ngspice's onoise_total, input against the power-law integral of ngspice's
              inoise spectrum (ngspice's inoise_total takes each interval's slope from the output
              density, 0.5 dB high on rc_lowpass_noise where the gain rolls off)
  oscillators mean period from mid-level crossings (SPECIAL "period")
ngspice has no .step: each .step combination becomes its own ngspice deck
(.param / .options temp=), matched to SwiftSim's stepped runs in order.

Known differences, kept out of the decks or covered by a tolerance below:
  - gmin: SwiftSim also puts it from every node to ground and across FET channels
    (TIGHT sets 1e-18 for .op/.dc); its AC junction stamps leave gmin out (AC_ATOL_I).
  - Inductors get LTspice's 1 mOhm series resistance in SwiftSim, none in ngspice; the
    decks keep >= 10 Ohm in series, so the effect stays below 1e-4.
  - Level-1 MOSFET capacitances are constant in SwiftSim (no Meyer/junction-voltage
    dependence): the models use CGSO/CGDO only, no TOX/CBD/CJ.
  - VDMOS: SwiftSim's Cgd is constant (vdmos_switch_tran uses CGDMAX = CGDMIN), and its
    KSUBTHRES defaults to 0 like LTspice while ngspice defaults to 0.1 (models.inc sets it).
  - BJT: XTF/VTF/ITF are not implemented (the models leave them out), and SwiftSim does
    not divide the TF charge by qb as SPICE3 does: ~0.3% longer storage time in
    bjt_switch_tran (within tolerance).
  - Noise: ngspice's VDMOS gate resistance (model RG) is noiseless; SwiftSim gives it
    thermal noise like any resistor (vdmos_cs_noise leaves RG out).
  - ngspice's own error: switch flips land up to a step early (SPECIAL).
"""

import itertools
import math
import os
import re
import subprocess
import sys
import tempfile
import time
from array import array
from bisect import bisect_left, bisect_right
from pathlib import Path

HERE = Path(__file__).resolve().parent
EXE = HERE.parent.parent / "target" / "release" / ("swiftsim.exe" if os.name == "nt" else "swiftsim")

OP_RTOL, OP_ATOL_V, OP_ATOL_I = 1e-6, 1e-9, 1e-12
WAVE_RTOL = 0.01
AC_DB, AC_DEG, AC_FLOOR_DB = 0.1, 1.0, 80.0
# AC currents below this are gmin-level (SwiftSim's AC junction stamps leave out gmin)
AC_ATOL_I = 1e-9
# Injected into .op/.dc decks. gmin: SwiftSim ties every node to ground with gmin (so floating
# nodes still solve) and puts it across MOSFET/JFET channels, SPICE only across junctions; at the
# default 1e-12 S that moves near-zero currents by ~1e-12 A per node-volt and low nodes by ~1e-7 V.
TIGHT = ".options reltol=1e-9 vntol=1e-9 abstol=1e-15 gmin=1e-18"

# Per-circuit exceptions. Keys: rtol (.tran), op_rtol, ac_db, ac_deg, tshift (s: .tran
# error is measured against the other run within ±tshift, for edges at slightly
# different instants), skip (signal regexes), period (only these signals, compared by period).
# Every entry says why it is needed.
SPECIAL = {
    "tl072_relaxation_osc_tran.cir": dict(
        period=["v(out)", "v(m)", "v(p)"],
        why="free-running oscillator: the phase drifts over many cycles, so only the period is compared",
    ),
    "switch_hysteresis_tran.cir": dict(
        tshift=1e-6,
        why="ngspice applies a switch flip to the whole 1 us step in which the control crosses the "
        "threshold (~0.3 us early); SwiftSim cuts the step at the crossing",
    ),
}


def num(s):
    """SPICE number with SI suffix ('4.7k', '10u', '1meg')."""
    m = re.match(r"([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)(meg|mil|[tgkmunpf])?", s.strip().lower())
    if not m:
        raise ValueError(f"not a number: {s}")
    scale = {"t": 1e12, "g": 1e9, "meg": 1e6, "k": 1e3, "m": 1e-3, "u": 1e-6, "n": 1e-9, "p": 1e-12, "f": 1e-15, "mil": 25.4e-6}
    return float(m.group(1)) * scale.get(m.group(2), 1.0)


def parse_step(line):
    t = line.split()[1:]
    if t[0] == "lin":
        t.pop(0)
    if t[0] == "param":
        t.pop(0)
    name = t.pop(0)
    if t[0] == "list":
        return name, [num(v) for v in t[1:]]
    if t[0] in ("dec", "oct"):
        raise ValueError(f"compare.py only expands lin/list .step: {line}")
    a, b, c = (num(v) for v in t[:3])
    return name, [a + i * c for i in range(int(math.floor((b - a) / c + 1e-9)) + 1)]


def prepare(path):
    """Netlist lines with absolute include paths (both simulators run it from a temp
    folder) and, for .op/.dc, TIGHT: at the default reltol=1e-3 a DC sweep point may stop
    iterating 1e-3 short of the solution; at 1e-9 the two agree to ~1e-9."""
    lines = []
    for ln in path.read_text().splitlines():
        if re.match(r"\s*\.(include|inc|lib)\s", ln, re.I):
            w = ln.split(None, 2)
            ln = f"{w[0]} {(path.parent / w[1]).resolve()}" + (f" {w[2]}" if len(w) > 2 else "")
        lines.append(ln)
    if any(re.match(r"\s*\.(op|dc)\b", ln, re.I) for ln in lines):
        lines.insert(1, TIGHT)
    return lines


def ngspice_decks(lines):
    """The netlist as ngspice decks, one per .step combination."""
    keep, steps, temps = [], [], []
    for ln in lines:
        low = ln.strip().lower()
        if low.startswith(".step"):
            steps.append(parse_step(low))
        elif low.startswith(".temp"):
            temps = [num(v) for v in low.split()[1:]]
        else:
            keep.append(ln)
    if len(temps) > 1:
        steps.append(("temp", temps))  # SwiftSim steps several .temp values innermost
    elif temps:
        keep.insert(1, f".options temp={temps[0]!r}")
    for combo in itertools.product(*[v for _, v in steps]):
        out = [keep[0]]
        for (name, _), v in zip(steps, combo):
            out.append(f".options temp={v!r}" if name == "temp" else f".param {name}={v!r}")
        for ln in keep[1:]:
            if ln.strip().lower().startswith(".param"):
                for name, _ in steps:
                    ln = re.sub(rf"\b{re.escape(name)}\s*=\s*(\{{[^}}]*\}}|\S+)", "", ln, flags=re.I)
                if not ln.split()[1:]:
                    continue
            out.append(ln)
        yield "\n".join(out) + "\n"


def read_raw(path):
    """ngspice binary rawfile -> [(plotname, names, {name: values})]."""
    data = path.read_bytes()
    plots, pos = [], 0
    while (end := data.find(b"Binary:\n", pos)) >= 0:
        hdr = data[pos:end].decode("latin-1").splitlines()
        pos = end + len(b"Binary:\n")
        info, k = {}, 0
        while not hdr[k].startswith("Variables:"):
            key, _, val = hdr[k].partition(":")
            info[key.strip()] = val.strip()
            k += 1
        nv, npt = int(info["No. Variables"]), int(info["No. Points"])
        names = [hdr[k + 1 + j].split()[1].lower() for j in range(nv)]
        names = [f"i({n[:-7]})" if n.endswith("#branch") else n for n in names]
        names = [{"onoise_spectrum": "v(onoise)", "inoise_spectrum": "v(inoise)"}.get(n, n) for n in names]
        w = 2 if "complex" in info["Flags"] else 1
        a = array("d")
        a.frombytes(data[pos : pos + 8 * w * nv * npt])
        pos += 8 * w * nv * npt
        if sys.byteorder != "little":
            a.byteswap()
        cols = {}
        for j, n in enumerate(names):
            if w == 2:
                cols[n] = [complex(a[2 * (r * nv + j)], a[2 * (r * nv + j) + 1]) for r in range(npt)]
            else:
                cols[n] = a[j::nv].tolist()
        plots.append((info["Plotname"], names, cols))
    return plots


def ngspice_runs(lines, tmp):
    runs, notes, totals = [], [], []
    for i, deck in enumerate(ngspice_decks(lines)):
        cir, raw = tmp / f"ng{i}.cir", tmp / f"ng{i}.raw"
        cir.write_text(deck)
        p = subprocess.run(["ngspice", "-b", "-r", str(raw), str(cir)], cwd=tmp, capture_output=True, text=True, timeout=900)
        log = p.stdout + p.stderr
        notes += [ln.strip() for ln in log.splitlines() if re.search(r"error|warning", ln, re.I) and ln.strip() not in notes]
        if not raw.exists():
            raise RuntimeError("ngspice produced no data:\n" + "\n".join(log.splitlines()[-15:]))
        for name, names, cols in read_raw(raw):
            if name == "Integrated Noise":  # v(onoise_total), v(inoise_total)
                totals.append({n[2:-7]: cols[n][0] for n in names})
            elif name == "Operating Point":
                runs.append(([0.0], {n: cols[n] for n in names}))
            else:
                x = [v.real if isinstance(v, complex) else v for v in cols[names[0]]]
                runs.append((x, {n: cols[n] for n in names[1:]}))
    return runs, notes, totals


def swiftsim_runs(lines, tmp):
    """SwiftSim batch CSV -> [(x, {name: values})], AC as complex."""
    cir, out = tmp / "sw.cir", tmp / "sw.csv"
    cir.write_text("\n".join(lines) + "\n")
    p = subprocess.run([str(EXE), "-b", str(cir), "-o", str(out)], capture_output=True, text=True, timeout=900)
    if p.returncode != 0 or not out.exists():
        raise RuntimeError("swiftsim failed:\n" + (p.stdout + p.stderr)[-2000:])
    notes = [ln.strip() for ln in p.stdout.splitlines() if ln.startswith("warning")]
    si = {"T": 1e12, "G": 1e9, "M": 1e6, "k": 1e3, "m": 1e-3, "µ": 1e-6, "n": 1e-9, "p": 1e-12, "f": 1e-15}
    found = re.findall(r"Total (output|input-referred) noise: (\S+?)([TGMkmµnpf]?)V rms", p.stdout)
    vals = [float(v) * si.get(u, 1.0) for _, v, u in found]
    totals = [{"onoise": a, "inoise": b} for a, b in zip(vals[::2], vals[1::2])]
    blocks = []
    for ln in out.read_text().splitlines():
        if ln.startswith("#"):
            continue
        f = ln.split(",")
        try:
            blocks[-1][1].append([float(v) for v in f])
        except (ValueError, IndexError):
            blocks.append(([h.strip().lower() for h in f], []))
    runs, kind = [], None
    for hdr, rows in blocks:
        kind = hdr[0]
        cols = list(zip(*rows)) if rows else [[] for _ in hdr]
        sig = {}
        j = 1
        while j < len(hdr):
            if hdr[j].endswith(" db"):
                sig[hdr[j][:-3]] = [10 ** (d / 20) * complex(math.cos(math.radians(g)), math.sin(math.radians(g))) for d, g in zip(cols[j], cols[j + 1])]
                j += 2
            else:
                sig[hdr[j]] = list(cols[j])
                j += 1
        runs.append((list(cols[0]), sig))
    return kind, runs, notes, totals


def integrate_psd(f, v):
    """Integral of v(f)^2 df, each interval a power law between its end points: V^2."""
    tot = 0.0
    for f0, f1, v0, v1 in zip(f, f[1:], v, v[1:]):
        s0, s1 = v0 * v0, v1 * v1
        if s0 <= 0 or s1 <= 0:
            tot += (s0 + s1) / 2 * (f1 - f0)
            continue
        r = math.log(f1 / f0)
        e = math.log(s1 / s0) / r + 1
        tot += s0 * f0 * r if abs(e) < 1e-10 else s0 * f0 / e * ((f1 / f0) ** e - 1)
    return tot


def interp(xs, ys, x):
    i = bisect_right(xs, x)
    if i == 0:
        return ys[0]
    if i >= len(xs):
        return ys[-1]
    x0, x1 = xs[i - 1], xs[i]
    return ys[i - 1] + (ys[i] - ys[i - 1]) * (x - x0) / (x1 - x0)


def sample_err(xa, ya, xb, yb, tau):
    """For each sample of run a: (t, distance of its value from the range of run b over
    [t-tau, t+tau] plus b's samples bracketing that window). With tau = 0 that is b's value
    when b has a sample at t, else the two b samples around t: nothing finer than a sample
    interval is known, and linear interpolation across a curved edge or a jump would
    report errors neither run has."""
    out = []
    for t, v in zip(xa, ya):
        if t < xb[0] or t > xb[-1]:
            continue
        i, j = bisect_left(xb, t - tau), bisect_right(xb, t + tau)
        if i > 0 and (i == len(xb) or xb[i] > t - tau):
            i -= 1
        if j < len(xb) and (j == 0 or xb[j - 1] < t + tau):
            j += 1
        seg = yb[i:j]
        out.append((t, max(min(seg) - v, v - max(seg), 0.0)))
    return out


def period(x, y):
    lo, hi = min(y), max(y)
    mid = (lo + hi) / 2
    tc = [x[k - 1] + (mid - y[k - 1]) * (x[k] - x[k - 1]) / (y[k] - y[k - 1]) for k in range(1, len(y)) if y[k - 1] < mid <= y[k]]
    tc = tc[len(tc) // 2 :]  # skip start-up
    return (tc[-1] - tc[0]) / (len(tc) - 1) if len(tc) > 2 else float("nan")


def compare(kind, sw, ng, spec):
    """[(ratio, signal, message)] for each compared signal; ratio > 1 fails."""
    res = []
    skip = [re.compile(s) for s in spec.get("skip", [])]
    for (xa, a), (xb, b) in zip(sw, ng):
        for name in sorted(set(a) & set(b)):
            if any(r.fullmatch(name) for r in skip) or name not in spec.get("period", [name]):
                continue
            ya, yb = a[name], b[name]
            if any(isinstance(v, float) and math.isnan(v) for v in ya):
                continue
            cur = name.startswith("i")
            if name in spec.get("period", []):
                pa, pb = period(xa, ya), period(xb, yb)
                e = abs(pa - pb) / pb
                res.append((e / spec.get("rtol", WAVE_RTOL), name, f"period {pa:.6g} vs {pb:.6g} s ({100 * e:.3f}%)"))
            elif kind == "frequency":
                peak = max(abs(v) for v in yb)
                if peak == 0:
                    continue
                db, dg, worst = spec.get("ac_db", AC_DB), spec.get("ac_deg", AC_DEG), (0.0, "", 0.0)
                for f, p, q in zip(xb, (interp(xa, ya, f) for f in xb), yb):
                    if abs(q) < max(peak * 10 ** (-AC_FLOOR_DB / 20), AC_ATOL_I if cur else 0) or p == 0:
                        continue
                    edb = abs(20 * math.log10(abs(p) / abs(q)))
                    edg = abs((math.degrees(math.atan2(p.imag, p.real) - math.atan2(q.imag, q.real)) + 180) % 360 - 180)
                    r = max(edb / db, edg / dg)
                    if r > worst[0]:
                        worst = (r, f"{edb:.3g} dB, {edg:.3g} deg at {f:.4g} Hz", f)
                res.append((worst[0], name, worst[1] or "exact"))
            elif kind == "time":
                tau = spec.get("tshift", 0.0)
                e = sample_err(xa, ya, xb, yb, tau) + sample_err(xb, yb, xa, ya, tau)
                scale = max(max(yb) - min(yb), 0.01 * max(abs(v) for v in yb), 1e-9 if cur else 1e-6)
                t, emax = max(e, key=lambda p: p[1])
                rms = math.sqrt(sum(p[1] ** 2 for p in e) / len(e))
                tol = spec.get("rtol", WAVE_RTOL) * scale
                res.append((emax / tol, name, f"max {emax:.3g} at t={t:.4g} (tol {tol:.3g}), rms {rms:.3g}"))
            elif len(ya) != len(yb):
                res.append((math.inf, name, f"{len(ya)} SwiftSim points vs {len(yb)} ngspice points"))
            else:  # op / dc: pointwise
                rt, at = spec.get("op_rtol", OP_RTOL), OP_ATOL_I if cur else OP_ATOL_V
                worst = (0.0, 0)
                for k, (p, q) in enumerate(zip(ya, yb)):
                    r = abs(p - q) / (rt * max(abs(p), abs(q)) + at)
                    if r > worst[0]:
                        worst = (r, k)
                k = worst[1]
                at_x = "" if kind == "op" else f" at {xb[k]:.4g}"
                res.append((worst[0], name, f"{ya[k]:.9g} vs {yb[k]:.9g}{at_x}"))
    return res


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    verbose = "-v" in sys.argv
    if not EXE.exists():
        sys.exit(f"{EXE} not found: run `cargo build --release` first")
    files = sorted(HERE.glob("*.cir"))
    if args:
        files = [f for f in files if any(a in f.name for a in args)]
    failed = 0
    for f in files:
        spec = SPECIAL.get(f.name, {})
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            try:
                lines = prepare(f)
                t0 = time.time()
                kind, sw, sw_notes, sw_tot = swiftsim_runs(lines, tmp)
                t1 = time.time()
                ng, ng_notes, ng_tot = ngspice_runs(lines, tmp)
                t2 = time.time()
            except Exception as e:
                failed += 1
                print(f"FAIL  {f.name}: {e}")
                continue
        # nested .dc: ngspice concatenates the outer sweep into one plot
        if len(ng) < len(sw) and len(sw) % len(ng) == 0:
            k = len(sw) // len(ng)
            ng = [(x[i * n : (i + 1) * n], {s: v[i * n : (i + 1) * n] for s, v in sig.items()}) for x, sig in ng for n in [len(x) // k] for i in range(k)]
        if len(sw) != len(ng):
            failed += 1
            print(f"FAIL  {f.name}: {len(sw)} SwiftSim runs vs {len(ng)} ngspice runs")
            continue
        res = compare(kind, sw, ng, spec)
        if len(sw_tot) != len(ng_tot):
            res.append((math.inf, "noise totals", f"{len(sw_tot)} SwiftSim vs {len(ng_tot)} ngspice"))
        for (x, sig), b in zip((r for r in ng if "v(inoise)" in r[1]), ng_tot):
            b["inoise"] = math.sqrt(integrate_psd(x, sig["v(inoise)"]))
        for a, b in zip(sw_tot, ng_tot):
            for key in ("onoise", "inoise"):
                e = abs(20 * math.log10(a[key] / b[key]))
                res.append((e / spec.get("ac_db", AC_DB), f"{key}_total", f"{a[key]:.5g} vs {b[key]:.5g} V rms ({e:.3g} dB)"))
        if not res:
            failed += 1
            print(f"FAIL  {f.name}: no common signals")
            continue
        worst = max(res)
        ok = worst[0] <= 1.0
        failed += not ok
        print(f"{'ok  ' if ok else 'FAIL'}  {f.name:32} {len(res):3} signals  worst {worst[0]:7.3f} {worst[1]}: {worst[2]}  [{t1 - t0:.2f}s / ngspice {t2 - t1:.2f}s]")
        if spec.get("why"):
            print(f"      note: {spec['why']}")
        if verbose:
            for r in sorted(res, reverse=True):
                print(f"      {r[0]:9.3g}  {r[1]:16} {r[2]}")
            for n in sw_notes:
                print(f"      swiftsim: {n}")
            for n in ng_notes:
                print(f"      ngspice: {n}")
    print(f"{len(files) - failed}/{len(files)} circuits match")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
