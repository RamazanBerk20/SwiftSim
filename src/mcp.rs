//! `swiftsim --mcp`: a Model Context Protocol server on stdin/stdout, so coding agents can run
//! simulations and read the results, and drive an open window (`gui_*`, through the window's
//! private request folder; see `gui::session::ipc_dir`). JSON-RPC 2.0, one message per line.

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
.meas results and the operating point appear in `log`. Node 0 or gnd is ground. \
The gui_* tools work on the SwiftSim window the user has open: read its netlist, run it, read its results, and set its .param values as its sliders do; they never save or change the user's file.";

fn tools() -> Value {
    let file = json!({"type": "string", "description": "Path of a .sws, .asc, .cir or .net file"});
    let pid = json!({"type": "integer", "description": "Process id of the window (from gui_status); may be left out when one window is open"});
    let generation = json!({"type": "integer", "description": "The document generation you last saw (gui_netlist or gui_status `document.generation`); refused if the window has another document now"});
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
                    "csv": {"type": "string", "description": "Also write every signal at every point to this CSV path."},
                    "raw": {"type": "string", "description": "Also write the results to this path as an LTspice-compatible binary .raw file."}
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
        },
        {
            "name": "gui_status",
            "description": "State of the open SwiftSim windows: each window's document (path, kind, dirty, generation) and simulation (state idle|running|cancelling|finished|failed|cancelled, run number, progress, seconds, error). With `wait_for`, waits until a window (of `file`, if given) reaches one of those simulation states, instead of polling output files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "Only windows with this document open"},
                    "wait_for": {"type": "array", "items": {"type": "string", "enum": ["idle", "running", "cancelling", "finished", "failed", "cancelled"]}, "description": "Simulation states to wait for, e.g. [\"finished\", \"failed\"]"},
                    "after_run": {"type": "integer", "description": "With wait_for: only a run numbered above this counts (the `run` of an earlier reply)"},
                    "timeout": {"type": "number", "description": "Seconds to wait at most (default 60, at most 600)"},
                    "pid": {"type": "integer", "description": "Only the window of this process id"}
                }
            }
        },
        {
            "name": "gui_netlist",
            "description": "The netlist of the document open in a SwiftSim window, its file, generation and unsaved state, and its live .param values (the Parameters panel: each with the schematic's value). Read-only.",
            "inputSchema": {"type": "object", "properties": {"pid": pid}}
        },
        {
            "name": "gui_run",
            "description": "Run the open document's simulation in a SwiftSim window (with its live .param values) and, unless wait is false, wait until it finishes, fails or is stopped. Refused while a simulation runs, a dialog is open or the user is using the mouse. Read the results with gui_results.",
            "inputSchema": {"type": "object", "properties": {
                "pid": pid,
                "generation": generation,
                "wait": {"type": "boolean", "description": "Wait for the run to end (default true)"},
                "timeout": {"type": "number", "description": "Seconds to wait at most (default 60, at most 600)"}
            }, "required": ["generation"]}
        },
        {
            "name": "gui_results",
            "description": "The results shown in a SwiftSim window, in the same shape as `simulate`: log, measurements, and per plot (one per .step) the signals and the requested traces.",
            "inputSchema": {"type": "object", "properties": {
                "pid": pid,
                "traces": {"type": "array", "items": {"type": "string"}, "description": "Expressions to return, e.g. V(out), I(R1)"},
                "max_points": {"type": "integer", "minimum": 2, "description": "Most samples per trace (default 200)"}
            }}
        },
        {
            "name": "gui_set_params",
            "description": "Set live .param values in a SwiftSim window, as its Parameters sliders do: the last simulation re-runs with them (to wait for it, pass the reply's `after_run` to gui_status with wait_for). The schematic itself is not changed; the user applies or resets the values.",
            "inputSchema": {"type": "object", "properties": {
                "pid": pid,
                "generation": generation,
                "values": {"type": "object", "additionalProperties": {"type": "number"}, "description": "Parameter name to value, e.g. {\"rload\": 4700}"}
            }, "required": ["values", "generation"]}
        },
        {
            "name": "gui_last_error",
            "description": "The last simulation error in a SwiftSim window and the error lines of its log.",
            "inputSchema": {"type": "object", "properties": {"pid": pid}}
        }
    ])
}

/// The MCP request lifecycle: a `tools/call` is running until it answers or is cancelled.
/// Each start gets its own sequence number, so a reused id never gets an older call's reply.
#[derive(Default)]
struct Requests {
    running: HashMap<String, (u64, Arc<Control>)>,
    next: u64,
}

impl Requests {
    fn start(&mut self, id: &str) -> (u64, Arc<Control>) {
        self.next += 1;
        let ctl = Arc::new(Control::default());
        if let Some((_, old)) = self
            .running
            .insert(id.to_string(), (self.next, ctl.clone()))
        {
            old.cancel.store(true, Ordering::Relaxed); // the client reused a running id: the older call is dropped
        }
        (self.next, ctl)
    }

    fn cancel(&mut self, id: &str) {
        if let Some((_, c)) = self.running.remove(id) {
            c.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether call `seq` under `id` may answer (it wasn't cancelled or replaced).
    fn finish(&mut self, id: &str, seq: u64) -> bool {
        if self.running.get(id).is_some_and(|(s, _)| *s == seq) {
            self.running.remove(id);
            true
        } else {
            false
        }
    }
}

/// Serves requests until stdin closes.
pub fn serve() {
    let running: Arc<Mutex<Requests>> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(
                &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "invalid JSON"}}),
            );
            continue;
        };
        match (msg["method"].as_str().unwrap_or(""), msg.get("id").cloned()) {
            ("notifications/cancelled", _) => running
                .lock()
                .unwrap()
                .cancel(&msg["params"]["requestId"].to_string()),
            // simulations run on their own thread so a long one can still be cancelled
            ("tools/call", Some(id)) => {
                let (seq, ctl) = running.lock().unwrap().start(&id.to_string());
                let running = running.clone();
                let spawned = std::thread::Builder::new()
                    .stack_size(swiftsim::sim::STACK)
                    .spawn(move || {
                        let reply = respond(&msg, &ctl);
                        // a cancelled request gets no response
                        if running.lock().unwrap().finish(&id.to_string(), seq) {
                            send(&envelope(id, reply));
                        }
                    });
                if let Err(e) = spawned {
                    send(
                        &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32603, "message": format!("couldn't start a thread: {e}")}}),
                    );
                }
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
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
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
            if !matches!(
                name,
                "simulate"
                    | "schematic_netlist"
                    | "open_in_gui"
                    | "gui_status"
                    | "gui_netlist"
                    | "gui_run"
                    | "gui_results"
                    | "gui_set_params"
                    | "gui_last_error"
            ) {
                return Err((-32602, format!("unknown tool '{name}'")));
            }
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(name, args, ctl)))
                    .unwrap_or_else(|_| Err("internal simulator error (panic)".into()));
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
        "gui_status" => Ok(gui_status(a, ctl, crate::gui::windows).to_string()),
        "gui_netlist" | "gui_run" | "gui_results" | "gui_set_params" | "gui_last_error" => {
            drive(name, a, ctl).map(|v| v.to_string())
        }
        "schematic_netlist" => {
            let (text, warnings) = crate::read_circuit(file.ok_or("`file` is required")?)?;
            Ok(warnings
                .iter()
                .map(|w| format!("* {w}\n"))
                .collect::<String>()
                + &text)
        }
        _ => {
            let f = file.ok_or("`file` is required")?;
            if !Path::new(f).is_file() {
                return Err(format!("no such file: {f}"));
            }
            Command::new(crate::gui::update::self_path())
                .arg(f)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("couldn't start SwiftSim: {e}"))?;
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
    let mut r = swiftsim::run_netlist(&text, base.as_deref(), ctl)?;
    if let Some(p) = a["csv"].as_str() {
        std::fs::write(p, swiftsim::to_csv(&r.plots)).map_err(|e| format!("{p}: {e}"))?;
    }
    if let Some(p) = a["raw"].as_str() {
        crate::write_raw(p, &r.plots, &text)?;
    }
    log.extend(std::mem::take(&mut r.log));
    Ok(results_json(&r, log, a))
}

/// A run's results as `simulate` returns them, with the `traces` and `max_points` of `a`.
pub fn results_json(r: &swiftsim::SimResult, log: Vec<String>, a: &Value) -> Value {
    let max = a["max_points"].as_u64().unwrap_or(200).max(2) as usize;
    let exprs: Vec<&str> = a["traces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
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
    let meas: Vec<Value> = r
        .meas
        .iter()
        .map(|(step, name, value)| json!({"step": step, "name": name, "value": value}))
        .collect();
    json!({"log": log, "measurements": meas, "plots": plots})
}

/// The `gui_*` tools that act on an open window: the request goes to the window of `pid` (or
/// the only one open) over its private request folder; `gui_run` then waits on its status file.
fn drive(name: &str, a: &Value, ctl: &Control) -> Result<Value, String> {
    let wins = crate::gui::windows();
    let pid = match a["pid"].as_u64() {
        Some(p) if wins.iter().any(|w| w["pid"].as_u64() == Some(p)) => p,
        Some(p) => return Err(format!("no SwiftSim window with pid {p}")),
        None if wins.len() == 1 => wins[0]["pid"].as_u64().unwrap_or(0),
        None if wins.is_empty() => {
            return Err("no SwiftSim window is open (open_in_gui opens one)".into());
        }
        None => {
            let list: Vec<String> = wins
                .iter()
                .map(|w| {
                    format!(
                        "{} ({})",
                        w["pid"],
                        w["document"]["path"].as_str().unwrap_or("untitled")
                    )
                })
                .collect();
            return Err(format!(
                "several SwiftSim windows are open; give `pid`, one of: {}",
                list.join(", ")
            ));
        }
    };
    let op = name.trim_start_matches("gui_");
    let r = crate::gui::ask_window(
        pid as u32,
        &json!({"op": op, "args": a}),
        std::time::Duration::from_secs(15),
        || ctl.cancel.load(Ordering::Relaxed),
    )?;
    if op != "run" || a["wait"].as_bool() == Some(false) {
        return Ok(r);
    }
    let run = r["run"].as_u64().unwrap_or(0);
    let wait = json!({"pid": pid, "wait_for": ["finished", "failed", "cancelled"], "after_run": run.saturating_sub(1), "timeout": a["timeout"].clone()});
    let st = gui_status(&wait, ctl, crate::gui::windows);
    let sim = st["windows"]
        .get(0)
        .map_or(Value::Null, |w| w["simulation"].clone());
    Ok(json!({"run": run, "timed_out": st["timed_out"], "simulation": sim}))
}

/// `gui_status`: the windows' states, after waiting for a simulation state if asked to.
fn gui_status(a: &Value, ctl: &Control, windows: impl Fn() -> Vec<Value>) -> Value {
    let file = a["file"]
        .as_str()
        .map(|f| std::path::absolute(f).unwrap_or_else(|_| f.into()));
    let pid = a["pid"].as_u64();
    let wait: Vec<&str> = a["wait_for"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let after = a["after_run"].as_u64().unwrap_or(0);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs_f64(
            a["timeout"].as_f64().unwrap_or(60.0).clamp(0.0, 600.0),
        );
    loop {
        let list: Vec<Value> = windows()
            .into_iter()
            .filter(|w| {
                file.as_ref().is_none_or(|f| {
                    w["document"]["path"]
                        .as_str()
                        .is_some_and(|p| Path::new(p) == f)
                })
            })
            .filter(|w| pid.is_none_or(|p| w["pid"].as_u64() == Some(p)))
            .collect();
        let reached = list.iter().any(|w| {
            wait.contains(&w["simulation"]["state"].as_str().unwrap_or(""))
                && w["simulation"]["run"].as_u64().unwrap_or(0) > after
        });
        let timed_out = std::time::Instant::now() >= deadline;
        if wait.is_empty() || reached || timed_out || ctl.cancel.load(Ordering::Relaxed) {
            return json!({"windows": list, "timed_out": !wait.is_empty() && !reached});
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn trace(p: &Plot, expr: &str, max: usize) -> Value {
    let n = p.x.len();
    let picks: Vec<usize> = if n <= max {
        (0..n).collect()
    } else {
        (0..max).map(|i| i * (n - 1) / (max - 1)).collect()
    };
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
        respond(
            &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            &Control::default(),
        )
    }

    fn simulate_ok(args: Value) -> Value {
        let r = request("tools/call", json!({"name": "simulate", "arguments": args})).unwrap();
        assert_eq!(r["isError"], false, "{r}");
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn handshake_and_tool_list() {
        assert_eq!(
            request("initialize", json!({"protocolVersion": "2025-03-26"})).unwrap()["protocolVersion"],
            "2025-03-26"
        );
        assert_eq!(
            request("initialize", json!({"protocolVersion": "1999-01-01"})).unwrap()["protocolVersion"],
            VERSIONS[0]
        );
        assert_eq!(
            request("tools/list", json!({})).unwrap()["tools"]
                .as_array()
                .unwrap()
                .len(),
            9
        );
        assert_eq!(request("no/such", json!({})).unwrap_err().0, -32601);
        assert_eq!(
            request("tools/call", json!({"name": "nope"}))
                .unwrap_err()
                .0,
            -32602
        );
    }

    #[test]
    fn divider_operating_point_and_rc_step() {
        // 10 V across two equal resistors
        let r = simulate_ok(
            json!({"netlist": "d\nV1 1 0 10\nR1 1 2 1k\nR2 2 0 1k\n.op\n", "traces": ["V(2)", "V(nope)"]}),
        );
        let t = &r["plots"][0]["traces"];
        assert!((t[0]["y"][0].as_f64().unwrap() - 5.0).abs() < 1e-8, "{t}");
        assert!(t[1]["error"].is_string());
        // RC charging: the final sample (t = 5 RC) is 1 - exp(-5)
        let r = simulate_ok(
            json!({"netlist": "rc\nV1 in 0 1\nR1 in out 1k\nC1 out 0 1u ic=0\n.tran 5m uic\n", "traces": ["V(out)"], "max_points": 5}),
        );
        let t = &r["plots"][0]["traces"][0];
        assert_eq!(t["y"].as_array().unwrap().len(), 5);
        assert!(
            (t["y"][4].as_f64().unwrap() - (1.0 - (-5f64).exp())).abs() < 2e-3,
            "{t}"
        );
        assert!(
            (t["max"].as_f64().unwrap() - (1.0 - (-5f64).exp())).abs() < 2e-3,
            "{t}"
        );
    }

    #[test]
    fn ac_traces_are_decibels_and_degrees() {
        // at the corner frequency of an RC low-pass: -3.01 dB, -45 degrees
        let f = 1.0 / (2.0 * std::f64::consts::PI * 1e3 * 1e-6);
        let r = simulate_ok(
            json!({"netlist": format!("rc\nV1 in 0 AC 1\nR1 in out 1k\nC1 out 0 1u\n.ac lin 1 {f} {f}\n"), "traces": ["V(out)"]}),
        );
        let t = &r["plots"][0]["traces"][0];
        assert!(
            (t["db"][0].as_f64().unwrap() + 10.0 * 2f64.log10()).abs() < 1e-6,
            "{t}"
        );
        assert!((t["deg"][0].as_f64().unwrap() + 45.0).abs() < 1e-6, "{t}");
    }

    #[test]
    fn bad_input_is_a_tool_error() {
        let bad =
            |args| request("tools/call", json!({"name": "simulate", "arguments": args})).unwrap();
        assert_eq!(bad(json!({}))["isError"], true);
        assert_eq!(
            bad(json!({"netlist": "x\nQ\n.op\n", "file": "a"}))["isError"],
            true
        );
        assert_eq!(bad(json!({"file": "/no/such/file.cir"}))["isError"], true);
    }

    /// The audit's case: cancel id 7, reuse 7; the old call's late answer must not take the new
    /// call's place, and the new call still answers.
    #[test]
    fn request_lifecycle() {
        let mut r = Requests::default();
        let (s1, c1) = r.start("7");
        r.cancel("7");
        assert!(c1.cancel.load(Ordering::Relaxed));
        let (s2, c2) = r.start("7");
        assert!(!r.finish("7", s1), "the cancelled call answered");
        assert!(r.finish("7", s2) && !c2.cancel.load(Ordering::Relaxed));
        assert!(!r.finish("7", s2), "answered twice");
        // an id reused while its call still runs: the older call is stopped and stays silent
        let (s3, c3) = r.start("8");
        let (s4, _) = r.start("8");
        assert!(c3.cancel.load(Ordering::Relaxed) && !r.finish("8", s3) && r.finish("8", s4));
        // unknown ids
        r.cancel("9");
        assert!(!r.finish("9", 1));
    }

    #[test]
    fn gui_status_waits_for_a_state() {
        // windows report absolute paths; "/a.sws" alone is drive-relative on Windows
        let win = |path: &str, state: &str, run: u64| json!({"pid": run, "document": {"path": std::path::absolute(path).unwrap()}, "simulation": {"state": state, "run": run}});
        let polls = std::cell::Cell::new(0);
        // the run finishes on the third look
        let windows = || {
            polls.set(polls.get() + 1);
            vec![
                win(
                    "/a.sws",
                    if polls.get() < 3 {
                        "running"
                    } else {
                        "finished"
                    },
                    2,
                ),
                win("/b.sws", "finished", 1),
            ]
        };
        let ctl = Control::default();
        let r = gui_status(
            &json!({"file": "/a.sws", "wait_for": ["finished", "failed"], "timeout": 10}),
            &ctl,
            windows,
        );
        assert_eq!(
            (
                r["timed_out"].as_bool(),
                r["windows"].as_array().unwrap().len(),
                polls.get()
            ),
            (Some(false), 1, 3),
            "{r}"
        );
        assert_eq!(r["windows"][0]["simulation"]["state"], "finished");
        // without wait_for: one look at every window; an old run doesn't count with after_run
        assert_eq!(
            gui_status(&json!({}), &ctl, windows)["windows"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            gui_status(&json!({"pid": 1}), &ctl, windows)["windows"][0]["document"]["path"],
            "/b.sws"
        );
        let r = gui_status(
            &json!({"file": "/b.sws", "wait_for": ["finished"], "after_run": 1, "timeout": 0.3}),
            &ctl,
            windows,
        );
        assert_eq!(r["timed_out"], true);
    }
}
