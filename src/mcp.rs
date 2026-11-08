//! `swiftsim --mcp`: a Model Context Protocol server on stdin/stdout, so coding agents can run
//! simulations and read the results. JSON-RPC 2.0, one message per line.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use swiftsim::{Control, Plot, PlotKind};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "SwiftSim is a SPICE circuit simulator (LTspice dialect). Write a netlist, call `simulate` with the signals you want in `traces`, and read the values back. \
Netlists start with a title line and need an analysis card: .op .tran .ac .dc .tf or .noise. Traces are expressions like V(out), V(a)-V(b), I(R1), Ic(Q1). \
.meas results and the operating point appear in `log`. Node 0 or gnd is ground.";

fn tools() -> Value {
    let file = json!({"type": "string", "description": "Path of a .sws, .asc, .cir or .net file"});
    json!([
        {
            "name": "simulate",
            "description": "Run a SPICE simulation from netlist text or a circuit file. Returns the log (operating point, .meas, warnings), the signals available, and sampled values of the requested traces. .step runs give one plot per step.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "netlist": {"type": "string", "description": "SPICE netlist text (LTspice dialect). Give this or `file`."},
                    "file": file,
                    "traces": {"type": "array", "items": {"type": "string"}, "description": "Expressions to return, e.g. V(out), V(a)-V(b), I(R1). AC traces come back as dB and degrees."},
                    "max_points": {"type": "integer", "minimum": 2, "description": "Most samples returned per trace, evenly spaced (default 200). min and max always cover every point."},
                    "csv": {"type": "string", "description": "Also write every signal at every point to this CSV path."}
                }
            }
        },
        {
            "name": "schematic_netlist",
            "description": "Get the SPICE netlist of a SwiftSim schematic (.sws) or LTspice schematic (.asc).",
            "inputSchema": {"type": "object", "properties": {"file": file}, "required": ["file"]}
        },
        {
            "name": "open_in_gui",
            "description": "Open a circuit file in a new SwiftSim window for the user.",
            "inputSchema": {"type": "object", "properties": {"file": file}, "required": ["file"]}
        }
    ])
}

/// Serves requests until stdin closes.
pub fn serve() {
    let running: Arc<Mutex<HashMap<String, Arc<Control>>>> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "invalid JSON"}}));
            continue;
        };
        match (msg["method"].as_str().unwrap_or(""), msg.get("id").cloned()) {
            ("notifications/cancelled", _) => {
                if let Some(c) = running.lock().unwrap().remove(&msg["params"]["requestId"].to_string()) {
                    c.cancel.store(true, Ordering::Relaxed);
                }
            }
            // simulations run on their own thread so a long one can still be cancelled
            ("tools/call", Some(id)) => {
                let ctl = Arc::new(Control::default());
                running.lock().unwrap().insert(id.to_string(), ctl.clone());
                let running = running.clone();
                std::thread::spawn(move || {
                    let reply = respond(&msg, &ctl);
                    // a cancelled request gets no response
                    if running.lock().unwrap().remove(&id.to_string()).is_some() {
                        send(&envelope(id, reply));
                    }
                });
            }
            (_, Some(id)) => send(&envelope(id.clone(), respond(&msg, &Control::default()))),
            _ => {} // other notifications need no answer
        }
    }
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}").and_then(|()| out.flush());
}

fn envelope(id: Value, reply: Result<Value, (i32, String)>) -> Value {
    match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    }
}

fn respond(msg: &Value, ctl: &Control) -> Result<Value, (i32, String)> {
    let p = &msg["params"];
    match msg["method"].as_str().unwrap_or("") {
        "initialize" => {
            let asked = p["protocolVersion"].as_str().unwrap_or("");
            Ok(json!({
                "protocolVersion": VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "swiftsim", "version": crate::gui::update::VERSION},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let (name, args) = (p["name"].as_str().unwrap_or(""), &p["arguments"]);
            if !matches!(name, "simulate" | "schematic_netlist" | "open_in_gui") {
                return Err((-32602, format!("unknown tool '{name}'")));
            }
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(name, args, ctl))).unwrap_or_else(|_| Err("internal simulator error (panic)".into()));
            Ok(match r {
                Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            })
        }
        m => Err((-32601, format!("method '{m}' not found"))),
    }
}

fn call(name: &str, a: &Value, ctl: &Control) -> Result<String, String> {
    let file = a["file"].as_str();
    match name {
        "simulate" => simulate(a, ctl).map(|v| v.to_string()),
        "schematic_netlist" => {
            let (text, warnings) = crate::read_circuit(file.ok_or("`file` is required")?)?;
            Ok(warnings.iter().map(|w| format!("* {w}\n")).collect::<String>() + &text)
        }
        _ => {
            let f = file.ok_or("`file` is required")?;
            if !Path::new(f).is_file() {
                return Err(format!("no such file: {f}"));
            }
            Command::new(crate::gui::update::self_path()).arg(f).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("couldn't start SwiftSim: {e}"))?;
            Ok(format!("opened {f}"))
        }
    }
}

fn simulate(a: &Value, ctl: &Control) -> Result<Value, String> {
    let (text, mut log, base) = match (a["netlist"].as_str(), a["file"].as_str()) {
        (Some(n), None) => (n.to_string(), vec![], None),
        (None, Some(f)) => {
            let (t, w) = crate::read_circuit(f)?;
            (t, w, Path::new(f).parent().map(PathBuf::from))
        }
        _ => return Err("give exactly one of `netlist` and `file`".into()),
    };
    let r = swiftsim::run_netlist(&text, base.as_deref(), ctl)?;
    if let Some(p) = a["csv"].as_str() {
        std::fs::write(p, swiftsim::to_csv(&r.plots)).map_err(|e| format!("{p}: {e}"))?;
    }
    log.extend(r.log);
    let max = a["max_points"].as_u64().unwrap_or(200).max(2) as usize;
    let exprs: Vec<&str> = a["traces"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let plots: Vec<Value> = r
        .plots
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut v = json!({
                "analysis": format!("{:?}", p.kind).to_lowercase(),
                "step": p.step,
                "x_name": p.xname,
                "points": p.x.len(),
                "traces": exprs.iter().map(|e| trace(p, e, max)).collect::<Vec<_>>(),
            });
            if i == 0 {
                v["signals"] = json!(p.names); // the same in every step
            }
            v
        })
        .collect();
    let meas: Vec<Value> = r.meas.iter().map(|(step, name, value)| json!({"step": step, "name": name, "value": value})).collect();
    Ok(json!({"log": log, "measurements": meas, "plots": plots}))
}

fn trace(p: &Plot, expr: &str, max: usize) -> Value {
    let n = p.x.len();
    let picks: Vec<usize> = if n <= max { (0..n).collect() } else { (0..max).map(|i| i * (n - 1) / (max - 1)).collect() };
    let x: Vec<f64> = picks.iter().map(|&i| p.x[i]).collect();
    if p.kind == PlotKind::Ac {
        return match swiftsim::measure::eval_trace_c(p, expr) {
            Ok(v) => json!({
                "expr": expr,
                "x": x,
                "db": picks.iter().map(|&i| 20.0 * v[i].norm().log10()).collect::<Vec<_>>(),
                "deg": picks.iter().map(|&i| v[i].arg_deg()).collect::<Vec<_>>(),
            }),
            Err(e) => json!({"expr": expr, "error": e}),
        };
    }
    match swiftsim::measure::eval_trace(p, expr) {
        Ok(v) => json!({
            "expr": expr,
            "x": x,
            "y": picks.iter().map(|&i| v[i]).collect::<Vec<_>>(),
            "min": v.iter().copied().fold(f64::INFINITY, f64::min),
            "max": v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }),
        Err(e) => json!({"expr": expr, "error": e}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Result<Value, (i32, String)> {
        respond(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}), &Control::default())
    }

    fn simulate_ok(args: Value) -> Value {
        let r = request("tools/call", json!({"name": "simulate", "arguments": args})).unwrap();
        assert_eq!(r["isError"], false, "{r}");
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn handshake_and_tool_list() {
        assert_eq!(request("initialize", json!({"protocolVersion": "2025-03-26"})).unwrap()["protocolVersion"], "2025-03-26");
        assert_eq!(request("initialize", json!({"protocolVersion": "1999-01-01"})).unwrap()["protocolVersion"], VERSIONS[0]);
        assert_eq!(request("tools/list", json!({})).unwrap()["tools"].as_array().unwrap().len(), 3);
        assert_eq!(request("no/such", json!({})).unwrap_err().0, -32601);
        assert_eq!(request("tools/call", json!({"name": "nope"})).unwrap_err().0, -32602);
    }

    #[test]
    fn divider_operating_point_and_rc_step() {
        // 10 V across two equal resistors
        let r = simulate_ok(json!({"netlist": "d\nV1 1 0 10\nR1 1 2 1k\nR2 2 0 1k\n.op\n", "traces": ["V(2)", "V(nope)"]}));
        let t = &r["plots"][0]["traces"];
        assert!((t[0]["y"][0].as_f64().unwrap() - 5.0).abs() < 1e-8, "{t}");
        assert!(t[1]["error"].is_string());
        // RC charging: the final sample (t = 5 RC) is 1 - exp(-5)
        let r = simulate_ok(json!({"netlist": "rc\nV1 in 0 1\nR1 in out 1k\nC1 out 0 1u ic=0\n.tran 5m uic\n", "traces": ["V(out)"], "max_points": 5}));
        let t = &r["plots"][0]["traces"][0];
        assert_eq!(t["y"].as_array().unwrap().len(), 5);
        assert!((t["y"][4].as_f64().unwrap() - (1.0 - (-5f64).exp())).abs() < 2e-3, "{t}");
        assert!((t["max"].as_f64().unwrap() - (1.0 - (-5f64).exp())).abs() < 2e-3, "{t}");
    }

    #[test]
    fn ac_traces_are_decibels_and_degrees() {
        // at the corner frequency of an RC low-pass: -3.01 dB, -45 degrees
        let f = 1.0 / (2.0 * std::f64::consts::PI * 1e3 * 1e-6);
        let r = simulate_ok(json!({"netlist": format!("rc\nV1 in 0 AC 1\nR1 in out 1k\nC1 out 0 1u\n.ac lin 1 {f} {f}\n"), "traces": ["V(out)"]}));
        let t = &r["plots"][0]["traces"][0];
        assert!((t["db"][0].as_f64().unwrap() + 10.0 * 2f64.log10()).abs() < 1e-6, "{t}");
        assert!((t["deg"][0].as_f64().unwrap() + 45.0).abs() < 1e-6, "{t}");
    }

    #[test]
    fn bad_input_is_a_tool_error() {
        let bad = |args| request("tools/call", json!({"name": "simulate", "arguments": args})).unwrap();
        assert_eq!(bad(json!({}))["isError"], true);
        assert_eq!(bad(json!({"netlist": "x\nQ\n.op\n", "file": "a"}))["isError"], true);
        assert_eq!(bad(json!({"file": "/no/such/file.cir"}))["isError"], true);
    }
}
