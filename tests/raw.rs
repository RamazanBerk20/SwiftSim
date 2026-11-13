//! `.raw` export, read back by a reader written from LTspice's published file layout (not from
//! the writer): UTF-16LE header lines ending in `Binary:`, then per point every variable as a
//! little-endian f64 (`real ... double`) or an (re, im) f64 pair (`complex`).

use std::collections::HashMap;
use swiftsim::{Control, Plot, PlotKind, run_netlist, to_raw};

struct Raw {
    head: HashMap<String, String>,
    vars: Vec<(String, String)>,
    /// [point][variable] = (re, im)
    data: Vec<Vec<(f64, f64)>>,
}

fn read(bytes: &[u8]) -> Raw {
    let marker: Vec<u8> = "Binary:\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let end = bytes
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("Binary: line")
        + marker.len();
    let units: Vec<u16> = bytes[..end]
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16(&units).unwrap();
    let (mut head, mut vars, mut in_vars) = (HashMap::new(), vec![], false);
    for l in text.lines() {
        if in_vars && l.starts_with('\t') {
            let f: Vec<&str> = l.trim().split('\t').collect();
            assert_eq!(f[0].parse::<usize>().unwrap(), vars.len(), "{l}");
            vars.push((f[1].to_string(), f[2].to_string()));
            continue;
        }
        let (k, v) = l
            .split_once(':')
            .unwrap_or_else(|| panic!("header line {l:?}"));
        in_vars = k == "Variables";
        head.insert(k.to_string(), v.trim().to_string());
    }
    let n: usize = head["No. Points"].parse().unwrap();
    assert_eq!(head["No. Variables"].parse::<usize>().unwrap(), vars.len());
    let flags = head["Flags"].clone();
    let complex = flags.split(' ').any(|f| f == "complex");
    assert!(
        complex || flags.split(' ').any(|f| f == "double"),
        "{flags}"
    );
    let width = if complex { 16 } else { 8 };
    let body = &bytes[end..];
    assert_eq!(body.len(), n * vars.len() * width, "data size");
    let f = |o: usize| f64::from_le_bytes(body[o..o + 8].try_into().unwrap());
    let data = (0..n)
        .map(|p| {
            (0..vars.len())
                .map(|v| {
                    let o = (p * vars.len() + v) * width;
                    (f(o), if complex { f(o + 8) } else { 0.0 })
                })
                .collect()
        })
        .collect();
    Raw { head, vars, data }
}

fn plots(net: &str) -> Vec<Plot> {
    run_netlist(net, None, &Control::default())
        .unwrap_or_else(|e| panic!("{e}"))
        .plots
}

/// Every value of every plot, in order, against the file.
fn check_data(raw: &Raw, ps: &[Plot], axis: bool) {
    let mut row = 0;
    for p in ps {
        for k in 0..p.x.len() {
            let r = &raw.data[row];
            if axis {
                assert_eq!(r[0], (p.x[k], 0.0));
            }
            let off = axis as usize;
            for i in 0..p.names.len() {
                let im = if p.kind == PlotKind::Ac {
                    p.im[i][k]
                } else {
                    0.0
                };
                assert_eq!(r[off + i], (p.re[i][k], im), "{} at point {k}", p.names[i]);
            }
            row += 1;
        }
    }
    assert_eq!(row, raw.data.len());
}

#[test]
fn transient_header_and_data() {
    let ps = plots("rc\nV1 in 0 PULSE(0 1 0 1u 1u 1m 2m)\nR1 in out 1k\nC1 out 0 1u\n.tran 2m\n");
    let raw = read(&to_raw(&ps, "rc").unwrap());
    assert_eq!(raw.head["Title"], "* rc");
    assert_eq!(raw.head["Plotname"], "Transient Analysis");
    assert_eq!(raw.head["Flags"], "real forward double");
    assert_eq!(raw.head["Offset"].parse::<f64>().unwrap(), 0.0);
    assert!(raw.head["Command"].contains("LTspice"));
    assert_eq!(raw.vars[0], ("time".into(), "time".into()));
    assert!(raw.vars.contains(&("V(out)".into(), "voltage".into())));
    assert!(
        raw.vars
            .contains(&("I(R1)".into(), "device_current".into()))
            || raw
                .vars
                .contains(&("I(r1)".into(), "device_current".into())),
        "{:?}",
        raw.vars
    );
    check_data(&raw, &ps, true);
}

#[test]
fn ac_is_complex_with_a_log_frequency_axis() {
    let ps = plots("rc\nV1 in 0 AC 1\nR1 in out 1k\nC1 out 0 1u\n.ac dec 10 1 100k\n");
    let raw = read(&to_raw(&ps, "ac").unwrap());
    assert_eq!(
        (raw.head["Plotname"].as_str(), raw.head["Flags"].as_str()),
        ("AC Analysis", "complex forward log")
    );
    assert_eq!(raw.vars[0], ("frequency".into(), "frequency".into()));
    check_data(&raw, &ps, true);
    // the RC corner: |H| = 1/sqrt(1 + (f/fc)^2) at the first frequency
    let k = raw.vars.iter().position(|v| v.0 == "V(out)").unwrap();
    let (f, (re, im)) = (raw.data[0][0].0, raw.data[0][k]);
    let fc = 1.0 / (2.0 * std::f64::consts::PI * 1e-3);
    assert!(((re * re + im * im).sqrt() - 1.0 / (1.0 + (f / fc).powi(2)).sqrt()).abs() < 1e-9);
}

#[test]
fn dc_sweep_and_nested_sweep_are_stepped() {
    let ps = plots("d\nV1 a 0 1\nV2 b 0 1\nR1 a b 1k\n.dc V1 0 2 0.5 V2 0 1 1\n");
    assert_eq!(ps.len(), 2);
    let raw = read(&to_raw(&ps, "d").unwrap());
    assert_eq!(
        (raw.head["Plotname"].as_str(), raw.head["Flags"].as_str()),
        ("DC transfer characteristic", "real forward double stepped")
    );
    assert_eq!(raw.vars[0], ("v1".into(), "voltage".into()));
    assert_eq!(raw.head["No. Points"], "10");
    check_data(&raw, &ps, true);
}

#[test]
fn operating_point_has_no_axis() {
    let ps = plots("op\nV1 a 0 10\nR1 a b 1k\nR2 b 0 1k\n.op\n");
    let raw = read(&to_raw(&ps, "op").unwrap());
    assert_eq!(
        (
            raw.head["Plotname"].as_str(),
            raw.head["No. Points"].as_str()
        ),
        ("Operating Point", "1")
    );
    assert_eq!(raw.vars[0].1, "voltage");
    check_data(&raw, &ps, false);
    let k = raw.vars.iter().position(|v| v.0 == "V(b)").unwrap();
    assert!((raw.data[0][k].0 - 5.0).abs() < 1e-8); // 10 V across two equal resistors (gmin aside)
}

#[test]
fn stepped_transient_concatenates_steps() {
    let ps = plots(
        "s\n.step param c list 1u 2u 3u\nV1 in 0 1\nR1 in out 1k\nC1 out 0 {c} ic=0\n.tran 1m uic\n",
    );
    assert_eq!(ps.len(), 3);
    let raw = read(&to_raw(&ps, "s").unwrap());
    assert_eq!(raw.head["Flags"], "real forward double stepped");
    assert_eq!(
        raw.head["No. Points"].parse::<usize>().unwrap(),
        ps.iter().map(|p| p.x.len()).sum::<usize>()
    );
    // each step starts again at t = 0
    assert_eq!(raw.data.iter().filter(|r| r[0].0 == 0.0).count(), 3);
    check_data(&raw, &ps, true);
}

#[test]
fn nothing_to_write_and_mismatched_steps() {
    assert!(to_raw(&[], "x").is_err());
    let mut ps = plots("s\n.step param r list 1 2\nV1 a 0 1\nR1 a 0 {r}\n.op\n");
    ps[1].names.pop();
    ps[1].re.pop();
    assert!(to_raw(&ps, "x").unwrap_err().contains("r=2"));
}
