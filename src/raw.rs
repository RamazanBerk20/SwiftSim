//! LTspice-compatible binary `.raw` files: a UTF-16LE header, then little-endian point records.
//! Real analyses are written with the `double` flag (every value an f64, as LTspice does with
//! `.options numdgt=7`), AC as complex f64 pairs; `.step` runs follow one another with `stepped`.

use crate::{Plot, PlotKind};

/// The `.raw` file of a run's plots (all from one analysis, every step with the same signals).
pub fn to_raw(plots: &[Plot], title: &str) -> Result<Vec<u8>, String> {
    let first = plots.first().ok_or("no results to write")?;
    // one header describes every point, so every step must have the same columns
    if let Some(p) = plots.iter().find(|p| {
        p.kind != first.kind
            || p.names != first.names
            || p.re.len() != first.names.len()
            || (p.kind == PlotKind::Ac && p.im.len() != first.names.len())
    }) {
        return Err(format!(
            "step '{}' has other signals than the first; a .raw file can't hold both",
            p.step
        ));
    }
    let complex = first.kind == PlotKind::Ac;
    // the operating point and .tf have no sweep variable
    let axis = !matches!(first.kind, PlotKind::Op);
    let plotname = match first.kind {
        PlotKind::Tran => "Transient Analysis",
        PlotKind::Ac => "AC Analysis",
        PlotKind::Dc => "DC transfer characteristic",
        PlotKind::Noise => "Noise Spectral Density - (V/Hz½ or A/Hz½)",
        PlotKind::Op if first.xname == "tf" => "Transfer Function",
        PlotKind::Op => "Operating Point",
    };
    let mut flags = vec![if complex { "complex" } else { "real" }, "forward"];
    if matches!(first.kind, PlotKind::Ac | PlotKind::Noise) && is_log(&first.x) {
        flags.push("log");
    }
    if !complex {
        flags.push("double");
    }
    if plots.len() > 1 {
        flags.push("stepped");
    }
    let mut vars: Vec<(String, &str)> = vec![];
    if axis {
        let kind = match first.kind {
            PlotKind::Tran => "time",
            PlotKind::Ac | PlotKind::Noise => "frequency",
            _ if first.xname.starts_with('i') => "current",
            _ => "voltage",
        };
        vars.push((first.xname.clone(), kind));
    }
    for n in &first.names {
        let kind = if n.starts_with("v(") || !n.contains('(') {
            "voltage"
        } else {
            "device_current"
        };
        vars.push((ltspice_name(n), kind));
    }
    let points: usize = plots.iter().map(|p| p.x.len()).sum();
    let mut head = format!(
        "Title: * {}\nDate: {}\nPlotname: {plotname}\nFlags: {}\nNo. Variables: {}\nNo. Points: {points:>12}\nOffset:   0.0000000000000000e+000\nCommand: SwiftSim {} (LTspice compatible)\nVariables:\n",
        title.trim(),
        date(std::time::SystemTime::now()),
        flags.join(" "),
        vars.len(),
        env!("CARGO_PKG_VERSION"),
    );
    for (i, (n, k)) in vars.iter().enumerate() {
        head += &format!("\t{i}\t{n}\t{k}\n");
    }
    head += "Binary:\n";
    let width = if complex { 16 } else { 8 };
    let mut out = Vec::with_capacity(head.len() * 2 + points * vars.len() * width);
    out.extend(head.encode_utf16().flat_map(u16::to_le_bytes));
    for p in plots {
        for k in 0..p.x.len() {
            if axis {
                out.extend(p.x[k].to_le_bytes());
                if complex {
                    out.extend(0f64.to_le_bytes());
                }
            }
            for i in 0..p.names.len() {
                out.extend(p.re[i][k].to_le_bytes());
                if complex {
                    out.extend(p.im[i][k].to_le_bytes());
                }
            }
        }
    }
    Ok(out)
}

/// `v(out)` → `V(out)`, `ic(q1)` → `Ic(q1)`: LTspice's spelling of a signal.
fn ltspice_name(n: &str) -> String {
    let mut c = n.chars();
    c.next()
        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// A sweep whose points grow by a constant ratio (`dec`/`oct`).
fn is_log(x: &[f64]) -> bool {
    x.len() > 2 && x[0] > 0.0 && {
        let r = x[1] / x[0];
        r > 1.0 && x.windows(2).all(|w| ((w[1] / w[0]) / r - 1.0).abs() < 1e-6)
    }
}

/// `Thu Nov 12 16:55:02 2026` (UTC), the form LTspice writes.
fn date(t: std::time::SystemTime) -> String {
    let s = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, secs) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil date from days since 1970-01-01 (H. Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][m as usize - 1];
    format!(
        "{wd} {mon} {d:2} {:02}:{:02}:{:02} {y}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_matches_known_days() {
        let at = |s: u64| super::date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(s));
        assert_eq!(at(0), "Thu Jan  1 00:00:00 1970");
        assert_eq!(at(951_782_400), "Tue Feb 29 00:00:00 2000");
        assert_eq!(at(1_794_502_502), "Thu Nov 12 16:55:02 2026");
    }

    #[test]
    fn names_and_log_sweeps() {
        assert_eq!(super::ltspice_name("v(out)"), "V(out)");
        assert_eq!(super::ltspice_name("ic(q1)"), "Ic(q1)");
        assert!(super::is_log(&[1.0, 10.0, 100.0]) && !super::is_log(&[1.0, 2.0, 3.0]));
    }
}
