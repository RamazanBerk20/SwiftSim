//! SwiftSim: a fast SPICE-compatible circuit simulator engine.

pub mod devices;
pub mod expr;
pub mod measure;
pub mod netlist;
pub mod sim;
pub mod solver;

pub use sim::{Control, Plot, PlotKind, SimResult, run_netlist};

/// CSV of all plots (AC exported as magnitude dB / phase degrees).
pub fn to_csv(plots: &[Plot]) -> String {
    let mut s = String::new();
    for p in plots {
        if !p.step.is_empty() {
            s.push_str(&format!("# step {}\n", p.step));
        }
        s.push_str(&p.xname);
        for n in &p.names {
            if p.kind == PlotKind::Ac {
                s.push_str(&format!(",{n} dB,{n} deg"));
            } else {
                s.push_str(&format!(",{n}"));
            }
        }
        s.push('\n');
        for k in 0..p.x.len() {
            s.push_str(&format!("{:e}", p.x[k]));
            for i in 0..p.names.len() {
                if p.kind == PlotKind::Ac {
                    let z = solver::C64::new(p.re[i][k], p.im[i][k]);
                    s.push_str(&format!(",{:e},{:e}", 20.0 * z.norm().log10(), z.arg_deg()));
                } else {
                    s.push_str(&format!(",{:e}", p.re[i][k]));
                }
            }
            s.push('\n');
        }
    }
    s
}
