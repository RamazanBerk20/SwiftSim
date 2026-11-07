//! Engine checks against analytic results.

use swiftsim::{Control, Plot, SimResult, run_netlist};

fn sim(net: &str) -> SimResult {
    run_netlist(net, None, &Control::default()).unwrap_or_else(|e| panic!("{e}\n{net}"))
}

fn sig<'a>(p: &'a Plot, n: &str) -> &'a [f64] {
    &p.re[p.index(n).unwrap_or_else(|| panic!("no {n} in {:?}", p.names))]
}

fn at(p: &Plot, n: &str, t: f64) -> f64 {
    let y = sig(p, n);
    let i = p.x.partition_point(|&x| x < t).min(p.x.len() - 1).max(1);
    let (x0, x1) = (p.x[i - 1], p.x[i]);
    y[i - 1] + (y[i] - y[i - 1]) * (t - x0) / (x1 - x0)
}

fn close(a: f64, b: f64, rel: f64) {
    assert!((a - b).abs() <= rel * b.abs().max(1e-12), "{a} != {b} (rel {rel})");
}

#[test]
fn divider_op() {
    let r = sim("div\nV1 1 0 10\nR1 1 2 1k\nR2 2 0 1k\n.op\n");
    let p = &r.plots[0];
    close(sig(p, "v(2)")[0], 5.0, 1e-9);
    close(sig(p, "i(v1)")[0], -5e-3, 1e-6);
    close(sig(p, "i(r1)")[0], 5e-3, 1e-9);
}

#[test]
fn rc_step_transient() {
    let r = sim("rc\nV1 in 0 PULSE(0 1 0 1n 1n 1 2)\nR1 in out 1k\nC1 out 0 1u\n.tran 5m\n");
    let p = &r.plots[0];
    for t in [0.2e-3, 1e-3, 2e-3, 4e-3] {
        close(at(p, "v(out)", t), 1.0 - (-t / 1e-3f64).exp(), 5e-3);
    }
    assert!(p.x.len() < 2000, "too many steps: {}", p.x.len());
}

#[test]
fn rc_lowpass_ac() {
    let r = sim("ac\nV1 in 0 AC 1\nR1 in out 1k\nC1 out 0 1u\n.ac dec 50 1 100k\n");
    let p = &r.plots[0];
    let fc = 1.0 / (2.0 * std::f64::consts::PI * 1e-3);
    let i = p.x.iter().position(|&f| f >= fc).unwrap();
    let k = p.index("v(out)").unwrap();
    let (re, im) = (p.re[k][i], p.im[k][i]);
    let f = p.x[i];
    let h = 1.0 / (1.0 + (f / fc).powi(2)).sqrt();
    close(re.hypot(im), h, 1e-6);
    close(im.atan2(re).to_degrees(), -(f / fc).atan().to_degrees(), 1e-6);
}

#[test]
fn rl_step_transient() {
    let r = sim("rl\nV1 in 0 PULSE(0 1 0 1n 1n 1 2)\nR1 in a 1k\nL1 a 0 1m Rser=0\n.tran 5u\n");
    let p = &r.plots[0];
    for t in [0.5e-6, 1e-6, 3e-6] {
        close(at(p, "i(l1)", t), 1e-3 * (1.0 - (-t / 1e-6f64).exp()), 5e-3);
    }
}

#[test]
fn lc_tank_keeps_frequency_and_amplitude() {
    // 1 µH / 1 µF, f0 = 159.15 kHz; trap must not damp noticeably
    let r = sim("lc\nC1 a 0 1u IC=1\nL1 a 0 1u Rser=0\n.tran 20u uic\n");
    let p = &r.plots[0];
    let v = sig(p, "v(a)");
    let late = p.x.iter().zip(v).filter(|(t, _)| **t > 15e-6).map(|(_, v)| v.abs()).fold(0.0, f64::max);
    close(late, 1.0, 0.03);
    let w = 1.0 / (1e-6f64 * 1e-6).sqrt();
    close(at(p, "v(a)", 3e-6), (w * 3e-6).cos(), 0.03);
}

#[test]
fn diode_forward_bias() {
    let r = sim("d\nV1 1 0 5\nR1 1 2 1k\nD1 2 0 1N4148\n.op\n");
    let p = &r.plots[0];
    let vd = sig(p, "v(2)")[0];
    assert!((0.55..0.8).contains(&vd), "vd = {vd}");
    close(sig(p, "i(d1)")[0], sig(p, "i(r1)")[0], 1e-4);
}

#[test]
fn zener_breakdown() {
    let r = sim("z\nV1 1 0 12\nR1 1 2 1k\nD1 0 2 1N4733\n.op\n");
    let v = sig(&r.plots[0], "v(2)")[0];
    assert!((4.9..5.4).contains(&v), "zener {v}");
}

#[test]
fn bjt_common_emitter() {
    let r = sim("ce\nVcc vcc 0 12\nRb vcc b 1meg\nRc vcc c 2k\nQ1 c b 0 2N3904\n.op\n");
    let p = &r.plots[0];
    let (ic, ib, ie) = (sig(p, "ic(q1)")[0], sig(p, "ib(q1)")[0], sig(p, "ie(q1)")[0]);
    close(ic + ib, -ie, 1e-9);
    let beta = ic / ib;
    assert!((150.0..500.0).contains(&beta), "beta {beta}");
    let vc = sig(p, "v(c)")[0];
    assert!(vc > 1.0 && vc < 11.0, "vc {vc}");
}

#[test]
fn pnp_mirrors_npn() {
    let r = sim("pnp\nVcc 0 vcc 12\nRb vcc b 1meg\nRc vcc c 2k\nQ1 c b 0 2N3906\n.op\n");
    let p = &r.plots[0];
    assert!(sig(p, "ic(q1)")[0] < 0.0 && sig(p, "v(c)")[0] < 0.0);
}

#[test]
fn mosfet_square_law() {
    let r = sim("m\n.model n1 nmos(vto=1 kp=1m)\nVd d 0 5\nVg g 0 2\nM1 d g 0 0 n1 W=10u L=10u\n.op\n");
    close(sig(&r.plots[0], "id(m1)")[0], 0.5e-3, 1e-6);
    let r = sim("m\n.model p1 pmos(vto=-1 kp=1m)\nVd d 0 -5\nVg g 0 -2\nM1 d g 0 0 p1\n.op\n");
    close(sig(&r.plots[0], "id(m1)")[0], -0.5e-3, 1e-6);
}

#[test]
fn cmos_inverter_dc_sweep() {
    let r = sim(
        "inv\nVdd vdd 0 5\nVin in 0 0\nM1 out in vdd vdd PMOS_GENERIC W=20u L=1u\nM2 out in 0 0 NMOS_GENERIC W=10u L=1u\n.dc Vin 0 5 0.05\n",
    );
    let p = &r.plots[0];
    let v = sig(p, "v(out)");
    assert!(v[0] > 4.99 && *v.last().unwrap() < 0.01);
    assert_eq!(p.x.len(), 101);
}

#[test]
fn vdmos_switch() {
    let r = sim("sw\nV1 vdd 0 12\nR1 vdd d 10\nVg g 0 10\nM1 d g 0 IRF540N\n.op\n");
    let vd = sig(&r.plots[0], "v(d)")[0];
    assert!(vd < 0.1, "vds on = {vd}");
}

#[test]
fn controlled_sources() {
    let r = sim(
        "ctl\nV1 a 0 2\nR1 a 0 1k\nE1 e 0 a 0 3\nRe e 0 1k\nG1 0 g a 0 1m\nRg g 0 1k\nVs a s 0\nRs s 0 1k\nF1 0 f Vs 2\nRf f 0 1k\nH1 h 0 Vs 500\nRh h 0 1k\n.op\n",
    );
    let p = &r.plots[0];
    close(sig(p, "v(e)")[0], 6.0, 1e-9);
    close(sig(p, "v(g)")[0], 2.0, 1e-9);
    close(sig(p, "v(f)")[0], 4.0, 1e-9);
    close(sig(p, "v(h)")[0], 1.0, 1e-6);
}

#[test]
fn behavioral_source_and_params() {
    let r = sim(".param vin=3 k={vin*2}\nV1 in 0 {vin}\nB1 out 0 V=V(in)**2 + k\nR1 out 0 1k\nB2 0 o2 I=if(V(in)>1, 1m, 0)\nR2 o2 0 1k\n.op\n");
    let p = &r.plots[0];
    close(sig(p, "v(out)")[0], 15.0, 1e-6);
    close(sig(p, "v(o2)")[0], 1.0, 1e-6);
}

#[test]
fn subcircuit_step_and_meas() {
    let net = "sub
.subckt div in out params: ra=1k rb=1k
R1 in out {ra}
R2 out 0 {rb}
.ends
.param rlow=1k
V1 a 0 10
X1 a b div rb={rlow}
.step param rlow list 1k 3k
.op
.meas op vb find V(b)
";
    let r = sim(net);
    assert_eq!(r.plots.len(), 2);
    close(sig(&r.plots[0], "v(b)")[0], 5.0, 1e-9);
    close(sig(&r.plots[1], "v(b)")[0], 7.5, 1e-9);
    assert_eq!(r.meas.len(), 2);
    close(r.meas[1].2, 7.5, 1e-9);
    assert!(r.plots[0].index("i(x1.r1)").is_some());
}

#[test]
fn transient_meas() {
    let net = "meas
V1 in 0 SIN(1 2 1k)
R1 in out 1k
C1 out 0 1n
.tran 5m
.meas tran vavg AVG V(in) FROM=1m TO=5m
.meas tran vpp PP V(in)
.meas tran vrms RMS V(in) FROM=1m TO=5m
.meas tran t1 WHEN V(in)=2 RISE=2
.meas tran dly TRIG V(in) VAL=2 RISE=1 TARG V(out) VAL=2 RISE=1
.meas tran dbl PARAM vavg*2
";
    let r = sim(net);
    let m = |n: &str| r.meas.iter().find(|t| t.1 == n).unwrap_or_else(|| panic!("{n}: {:?}", r.log)).2;
    close(m("vavg"), 1.0, 2e-3);
    close(m("vpp"), 4.0, 2e-3);
    close(m("vrms"), (1.0f64 + 2.0).sqrt(), 2e-3);
    // sin crosses 2V (offset+amp/2) rising at t = (1/12)/1k, second at +1ms
    close(m("t1"), 1e-3 / 12.0 + 1e-3, 2e-3);
    let d = m("dly");
    assert!(d > 0.0 && d < 5e-6, "delay {d}");
    close(m("dbl"), 2.0, 2e-3);
}

#[test]
fn transformer_coupling() {
    let r = sim("xfmr\nV1 p 0 AC 1\nL1 p 0 1m\nL2 s 0 4m Rser=0\nK1 L1 L2 1\nR1 s 0 1meg\n.ac lin 1 1k 1k\n");
    let p = &r.plots[0];
    let k = p.index("v(s)").unwrap();
    close(p.re[k][0].hypot(p.im[k][0]), 2.0, 1e-3);
}

#[test]
fn half_wave_rectifier() {
    let r = sim("rect\nV1 in 0 SIN(0 10 50)\nD1 in out 1N4007\nC1 out 0 100u\nR1 out 0 1k\n.tran 100m\n");
    let p = &r.plots[0];
    let v = at(p, "v(out)", 0.095);
    assert!((8.0..9.6).contains(&v), "dc out {v}");
}

#[test]
fn switch_and_ic() {
    let r = sim(
        "sw\n.model myswitch SW(Ron=1 Roff=1meg Vt=0.5)\nV1 a 0 5\nVc c 0 PULSE(0 1 1m 1u 1u 1m 2m)\nS1 a b c 0 myswitch\nR1 b 0 1k\n.tran 3m\n",
    );
    let p = &r.plots[0];
    assert!(at(p, "v(b)", 0.5e-3) < 0.01);
    close(at(p, "v(b)", 1.5e-3), 5.0 * 1000.0 / 1001.0, 1e-3);
    let r = sim("ic\nR1 a 0 1k\nC1 a 0 1u\n.ic V(a)=2\n.tran 3m\n");
    close(at(&r.plots[0], "v(a)", 1e-3), 2.0 * (-1.0f64).exp(), 5e-3);
}

#[test]
fn unsupported_mos_level_warns() {
    let r = sim("m\n.model b4 nmos(level=54 vto=0.5)\nVd d 0 1\nVg g 0 1\nM1 d g 0 0 b4\n.op\n");
    assert!(r.log.iter().any(|l| l.contains("level 54")), "{:?}", r.log);
}

#[test]
fn errors_are_reported() {
    let e = |n: &str| run_netlist(n, None, &Control::default()).err().unwrap();
    assert!(e("t\nR1 a 0 1k\n").contains("no analysis"));
    assert!(e("t\nV1 a 0 1\nV2 a 0 2\n.op\n").contains("singular"));
    assert!(e("t\nD1 a 0 nomodel\n.op\n").contains("unknown model"));
    assert!(e("t\nX1 a b nosub\n.op\n").contains("unknown subcircuit"));
}
