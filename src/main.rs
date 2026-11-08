#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod gui;
mod mcp;

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Release builds use the GUI subsystem (no console), so CLI modes borrow the parent shell's console to print.
    #[cfg(windows)]
    if args.iter().any(|a| matches!(a.as_str(), "-b" | "--batch" | "-h" | "--help")) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        // u32::MAX = ATTACH_PARENT_PROCESS; failure (no parent console) is harmless.
        unsafe { AttachConsole(u32::MAX) };
    }
    if let Some(i) = args.iter().position(|a| a == "-b" || a == "--batch") {
        let Some(file) = args.get(i + 1) else {
            eprintln!("usage: swiftsim -b <file> [-o out.csv] [--netlist]");
            std::process::exit(2);
        };
        let out = args.iter().position(|a| a == "-o").and_then(|j| args.get(j + 1));
        std::process::exit(batch(file, out.map(|s| s.as_str()), args.iter().any(|a| a == "--netlist")));
    }
    if args.iter().any(|a| a == "--mcp") {
        return mcp::serve();
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("SwiftSim {}\n\nusage:\n  swiftsim [file.sws|file.cir]      open the GUI\n  swiftsim -b <file> [-o out.csv]    batch simulation of a .cir/.net, .sws or .asc file\n      --netlist                      also print the netlist that is simulated\n  swiftsim --mcp                     MCP server on stdio, for coding agents", gui::update::VERSION);
        return;
    }
    if let Err(e) = gui::run(args.get(1).cloned()) {
        eprintln!("failed to start GUI: {e}");
        std::process::exit(1);
    }
}

/// Netlist text of a .cir/.net, .sws or .asc file, and any import warnings.
fn read_circuit(file: &str) -> Result<(String, Vec<String>), String> {
    let lower = file.to_ascii_lowercase();
    let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    if lower.ends_with(".sws") {
        return gui::sws_netlist(&swiftsim::netlist::decode(&bytes)).map_err(|e| format!("{file}: {e}"));
    }
    if lower.ends_with(".asc") {
        let (s, skipped) = gui::asc::import(&swiftsim::netlist::decode(&bytes), Path::new(file).parent());
        let mut warnings = s.name_conflicts();
        if !skipped.is_empty() {
            warnings.push(format!("warning: unsupported LTspice symbols left out (their nets are open): {}", skipped.join(", ")));
        }
        return Ok((s.netlist().text, warnings));
    }
    Ok((swiftsim::netlist::decode(&bytes), vec![]))
}

fn batch(file: &str, out: Option<&str>, show_netlist: bool) -> i32 {
    let (text, warnings) = match read_circuit(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    for w in &warnings {
        println!("{w}");
    }
    if show_netlist {
        println!("{text}");
    }
    let ctl = swiftsim::Control::default();
    match swiftsim::run_netlist(&text, Path::new(file).parent(), &ctl) {
        Ok(r) => {
            for l in &r.log {
                println!("{l}");
            }
            if let Some(o) = out {
                if let Err(e) = std::fs::write(o, swiftsim::to_csv(&r.plots)) {
                    eprintln!("{o}: {e}");
                    return 1;
                }
                println!("wrote {o}");
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
