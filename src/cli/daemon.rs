//! Local trigger daemon: poll the git/workflow fingerprint and serve a
//! loopback webhook. Scheduling (cron) is delegated to the OS scheduler.

use super::{fail, host, ok, Cli};
use crate::artifacts::sha256_bytes;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::telemetry::Ui;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start a detached daemon.
    Start(StartArgs),
    /// Stop the detached daemon.
    Stop,
    /// Show daemon liveness and the last trigger.
    Status,
    /// Run the daemon loop in the foreground (used by `start`).
    Run(StartArgs),
    /// Request one run from a running daemon.
    Trigger,
}

#[derive(Debug, Clone, Args)]
pub struct StartArgs {
    /// Workflow file; defaults to the only workflow in .ubuilder/workflows.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Poll interval in seconds.
    #[arg(long, default_value_t = 5)]
    pub interval_secs: u64,
    /// Loopback webhook port; POST /hook triggers a run.
    #[arg(long)]
    pub webhook: Option<u16>,
    /// Bearer token required by the webhook (or UBUILDER_WEBHOOK_TOKEN).
    #[arg(long, env = "UBUILDER_WEBHOOK_TOKEN")]
    pub token: Option<String>,
    /// Run once and exit instead of waiting for changes.
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DaemonState {
    last_digest: Option<String>,
    last_run: Option<u128>,
    last_status: Option<String>,
    last_error: Option<String>,
    total_runs: u64,
}

pub fn run(cli: &Cli, command: &DaemonCommand, ui: &Ui) -> Result<i32> {
    match command {
        DaemonCommand::Start(args) => start(cli, args, ui),
        DaemonCommand::Stop => stop(cli, ui),
        DaemonCommand::Status => status(cli, ui),
        DaemonCommand::Run(args) => run_loop(cli, args, ui),
        DaemonCommand::Trigger => trigger(cli, ui),
    }
}

fn start(cli: &Cli, args: &StartArgs, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, args.file.as_deref())?;
    if !paths.workflow.is_file() {
        let error = UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("workflow not found: {}", paths.workflow.display()),
        );
        return Ok(fail(ui, "daemon start", &error, None));
    }
    if let Some(pid) = read_pid(&paths.pid)? {
        if process_alive(pid) {
            return Ok(ok(
                ui,
                "daemon start",
                json!({ "running": true, "pid": pid, "already_running": true }),
            ));
        }
    }
    std::fs::create_dir_all(&paths.dir)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", paths.dir.display()), e))?;
    let exe = std::env::current_exe()
        .map_err(|e| UbuilderError::io("cannot locate ubuilder executable", e))?;
    let mut child = Command::new(exe);
    child
        .arg("--path")
        .arg(&paths.root)
        .arg("--json")
        .arg("daemon")
        .arg("run")
        .arg("--interval-secs")
        .arg(args.interval_secs.to_string());
    if let Some(file) = &args.file {
        child.arg("--file").arg(file);
    }
    if let Some(port) = args.webhook {
        child.arg("--webhook").arg(port.to_string());
    }
    if let Some(token) = &args.token {
        child.arg("--token").arg(token);
    }
    if args.once {
        child.arg("--once");
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", paths.log.display()), e))?;
    let err = log
        .try_clone()
        .map_err(|e| UbuilderError::io("cannot clone daemon log", e))?;
    child.stdout(Stdio::from(log)).stderr(Stdio::from(err));
    let child = child
        .spawn()
        .map_err(|e| UbuilderError::io("cannot start daemon", e))?;
    let pid = child.id();
    write_pid(&paths.pid, pid)?;
    // Give the child a moment to bind the webhook and write its pid.
    std::thread::sleep(Duration::from_millis(250));
    if !process_alive(pid) {
        let error = UbuilderError::new(ErrorCode::Io, "daemon exited during startup")
            .details(json!({ "log": paths.log }));
        return Ok(fail(ui, "daemon start", &error, None));
    }
    if !ui.json() {
        ui.success(format!("daemon started (pid {pid})"));
        ui.info(format!("log: {}", paths.log.display()));
    }
    Ok(ok(
        ui,
        "daemon start",
        json!({ "running": true, "pid": pid, "log": paths.log, "webhook": args.webhook }),
    ))
}

fn stop(cli: &Cli, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, None)?;
    let Some(pid) = read_pid(&paths.pid)? else {
        if !ui.json() {
            ui.info("daemon is not running");
        }
        return Ok(ok(
            ui,
            "daemon stop",
            json!({ "running": false, "stopped": false }),
        ));
    };
    let alive = process_alive(pid);
    if alive {
        kill_process(pid)?;
    }
    let _ = std::fs::remove_file(&paths.pid);
    if !ui.json() {
        if alive {
            ui.success(format!("daemon stopped (pid {pid})"));
        } else {
            ui.info("removed stale daemon pid file");
        }
    }
    Ok(ok(
        ui,
        "daemon stop",
        json!({ "running": false, "stopped": alive, "pid": pid }),
    ))
}

fn status(cli: &Cli, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, None)?;
    let pid = read_pid(&paths.pid)?;
    let running = pid.map(process_alive).unwrap_or(false);
    let state = read_state(&paths.state).unwrap_or_default();
    let data = json!({
        "running": running,
        "pid": pid.filter(|_| running),
        "log": paths.log,
        "state": state,
    });
    if !ui.json() {
        ui.info(if running {
            format!("daemon running (pid {})", pid.unwrap_or_default())
        } else {
            "daemon is not running".into()
        });
        if let Some(last) = state.last_run {
            ui.info(format!(
                "last run: {} ({})",
                last,
                state.last_status.as_deref().unwrap_or("unknown")
            ));
        }
    }
    Ok(ok(ui, "daemon status", data))
}

fn trigger(cli: &Cli, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, None)?;
    let Some(pid) = read_pid(&paths.pid)? else {
        let error = UbuilderError::new(ErrorCode::Usage, "daemon is not running");
        return Ok(fail(ui, "daemon trigger", &error, None));
    };
    if !process_alive(pid) {
        let error = UbuilderError::new(ErrorCode::Usage, "daemon is not running");
        return Ok(fail(ui, "daemon trigger", &error, None));
    }
    std::fs::write(&paths.trigger, b"run")
        .map_err(|e| UbuilderError::io("cannot write daemon trigger", e))?;
    if !ui.json() {
        ui.success("trigger queued");
    }
    Ok(ok(ui, "daemon trigger", json!({ "queued": true })))
}

fn run_loop(cli: &Cli, args: &StartArgs, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, args.file.as_deref())?;
    std::fs::create_dir_all(&paths.dir)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", paths.dir.display()), e))?;
    write_pid(&paths.pid, std::process::id())?;
    let mut state = read_state(&paths.state).unwrap_or_default();
    let token = args.token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(port) = args.webhook {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            UbuilderError::io(format!("cannot bind webhook 127.0.0.1:{port}"), e)
                .remediation("Choose another --webhook port or stop the process using it")
        })?;
        let tx = tx.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if serve(stream, token.as_deref()) {
                    let _ = tx.send(());
                }
            }
        });
        eprintln!("webhook listening on http://127.0.0.1:{port}/hook");
    }
    let interval = Duration::from_secs(args.interval_secs.max(1));
    let mut force = args.once;
    loop {
        if !args.once {
            if args.webhook.is_some() {
                match rx.recv_timeout(interval) {
                    Ok(()) => force = true,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                std::thread::sleep(interval);
            }
        }
        if paths.trigger.is_file() {
            let _ = std::fs::remove_file(&paths.trigger);
            force = true;
        }
        let digest = trigger_digest(&paths.root, &paths.workflow);
        let changed = digest.as_deref() != state.last_digest.as_deref();
        if force || changed {
            force = false;
            let started = crate::telemetry::now_ms();
            let result = run_workflow(&paths);
            state.last_run = Some(started);
            state.total_runs += 1;
            state.last_digest = digest;
            state.last_status = Some(match &result {
                Ok(()) => "success".into(),
                Err(error) => format!("failure: {}", error.message),
            });
            state.last_error = result.err().map(|error| error.message);
            write_state(&paths.state, &state)?;
            if !ui.json() {
                eprintln!(
                    "run {}: {}",
                    started,
                    state.last_status.as_deref().unwrap_or("unknown")
                );
            }
        }
        if args.once {
            break;
        }
    }
    let _ = std::fs::remove_file(&paths.pid);
    Ok(ok(ui, "daemon run", json!({ "runs": state.total_runs })))
}

fn run_workflow(paths: &Paths) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| UbuilderError::io("cannot locate ubuilder executable", e))?;
    let mut command = Command::new(exe);
    command
        .arg("--path")
        .arg(&paths.root)
        .arg("--no-color")
        .arg("pipeline")
        .arg("run")
        .arg("--file")
        .arg(&paths.workflow);
    let output = command
        .output()
        .map_err(|e| UbuilderError::io("cannot run workflow", e))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.run_log)
        .map_err(|e| UbuilderError::io("cannot open daemon run log", e))?;
    let mut log = log;
    let _ = writeln!(log, "===== {} =====", crate::telemetry::now_ms());
    let _ = log.write_all(&output.stdout);
    let _ = log.write_all(&output.stderr);
    if output.status.success() {
        Ok(())
    } else {
        Err(UbuilderError::new(
            ErrorCode::ExecutionFailed,
            format!("workflow exited with {}", output.status),
        ))
    }
}

fn serve(mut stream: std::net::TcpStream, token: Option<&str>) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = [0u8; 8192];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut authorized = token.is_none();
    let mut method_ok = false;
    if let Some(method) = request_line.split_whitespace().next() {
        method_ok = method == "POST";
    }
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") && token.is_some() {
                authorized = value.trim() == format!("Bearer {}", token.unwrap_or_default());
            }
            if name.eq_ignore_ascii_case("x-ubuilder-token") {
                authorized = token == Some(value.trim());
            }
        }
    }
    let (status, body) = if method_ok && authorized {
        ("202 Accepted", "queued")
    } else if !method_ok {
        ("405 Method Not Allowed", "POST /hook required")
    } else {
        ("401 Unauthorized", "invalid token")
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    status.starts_with("202")
}

fn trigger_digest(root: &Path, workflow: &Path) -> Option<String> {
    let workflow_hash = std::fs::read(workflow).ok()?;
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    Some(sha256_bytes(
        format!("{head}\n{}", sha256_bytes(&workflow_hash)).as_bytes(),
    ))
}

struct Paths {
    root: PathBuf,
    workflow: PathBuf,
    dir: PathBuf,
    pid: PathBuf,
    state: PathBuf,
    trigger: PathBuf,
    log: PathBuf,
    run_log: PathBuf,
}

impl Paths {
    fn new(cli: &Cli, file: Option<&Path>) -> Result<Self> {
        let host = host()?;
        let root = super::absolute_path(&cli.path);
        let workflow = crate::pipeline::parser::resolve_path(&root, file)
            .unwrap_or_else(|_| root.join(".ubuilder/workflows/build.yml"));
        let dir = host.data_dir().join("daemon");
        Ok(Self {
            workflow,
            root,
            pid: dir.join("daemon.pid"),
            state: dir.join("state.json"),
            trigger: dir.join("trigger"),
            log: dir.join("daemon.log"),
            run_log: dir.join("runs.log"),
            dir,
        })
    }
}

fn read_pid(path: &Path) -> Result<Option<u32>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(raw.trim().parse().ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(UbuilderError::io("cannot read daemon pid", error)),
    }
}

fn write_pid(path: &Path, pid: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    std::fs::write(path, pid.to_string())
        .map_err(|e| UbuilderError::io("cannot write daemon pid", e))
}

fn read_state(path: &Path) -> Option<DaemonState> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_state(path: &Path, state: &DaemonState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("cannot serialize state: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| UbuilderError::io("cannot write daemon state", e))?;
    std::fs::rename(&tmp, path).map_err(|e| UbuilderError::io("cannot replace daemon state", e))
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .to_ascii_lowercase()
            .contains("ubuilder"),
        _ => false,
    }
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn kill_process(pid: u32) -> Result<()> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .map_err(|e| UbuilderError::io("cannot stop daemon", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(UbuilderError::new(
            ErrorCode::Io,
            format!("taskkill failed for pid {pid}"),
        ))
    }
}

#[cfg(not(windows))]
fn kill_process(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map_err(|e| UbuilderError::io("cannot stop daemon", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(UbuilderError::new(
            ErrorCode::Io,
            format!("kill failed for pid {pid}"),
        ))
    }
}
