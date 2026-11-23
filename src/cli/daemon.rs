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
use std::process::Command;
#[cfg(not(windows))]
use std::process::Stdio;
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
    /// Webhook bind address. Defaults to loopback.
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: String,
    /// Bearer token required by the webhook (or UBUILDER_WEBHOOK_TOKEN).
    #[arg(long, env = "UBUILDER_WEBHOOK_TOKEN", hide_env_values = true)]
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

fn validate_webhook(args: &StartArgs) -> Result<()> {
    if args.webhook.is_some() {
        if args
            .token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .is_none()
        {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "webhook authentication is required",
            )
            .remediation(
                "Pass --token or set UBUILDER_WEBHOOK_TOKEN; unauthenticated hooks are rejected",
            ));
        }
        args.bind.parse::<std::net::IpAddr>().map_err(|_| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("invalid webhook bind address '{}'", args.bind),
            )
            .remediation("Use an IP address such as 127.0.0.1 or ::1")
        })?;
    }
    Ok(())
}

fn start(cli: &Cli, args: &StartArgs, ui: &Ui) -> Result<i32> {
    let paths = Paths::new(cli, args.file.as_deref())?;
    validate_webhook(args)?;
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
    let mut argv = vec![
        "--path".to_string(),
        paths.root.to_string_lossy().to_string(),
        "--json".to_string(),
        "daemon".to_string(),
        "run".to_string(),
        "--interval-secs".to_string(),
        args.interval_secs.to_string(),
    ];
    if let Some(file) = &args.file {
        argv.push("--file".into());
        argv.push(file.to_string_lossy().to_string());
    }
    if let Some(port) = args.webhook {
        argv.push("--webhook".into());
        argv.push(port.to_string());
        argv.push("--bind".into());
        argv.push(args.bind.clone());
    }
    if args.once {
        argv.push("--once".into());
    }
    let pid = spawn_daemon(
        &exe,
        &argv,
        args.token.as_deref(),
        &paths.log,
        &paths.err_log,
    )?;
    write_pid(&paths.pid, pid)?;
    // Give the child a moment to bind the webhook and write its pid.
    std::thread::sleep(Duration::from_millis(250));
    if !process_alive(pid) {
        let error = UbuilderError::new(ErrorCode::Io, "daemon exited during startup")
            .details(json!({ "log": paths.log, "err_log": paths.err_log }));
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

/// Spawn a process that must outlive the calling shell. Detaching matters on
/// Windows: `CreateProcess` inherits inheritable handles, so a directly
/// spawned daemon would keep a caller's `| jq` pipe open for its lifetime.
fn spawn_daemon(
    exe: &Path,
    argv: &[String],
    token: Option<&str>,
    stdout: &Path,
    stderr: &Path,
) -> Result<u32> {
    #[cfg(windows)]
    {
        // PowerShell's `-RedirectStandard*` keeps the launcher alive for the
        // child's lifetime and reintroduces the inherited-handle problem, so
        // the daemon writes run output itself and inherits no caller pipe.
        for path in [stdout, stderr] {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| UbuilderError::io(format!("cannot open {}", path.display()), e))?;
        }
        let commandline = argv
            .iter()
            .map(|arg| quote_windows_arg(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            "$p = Start-Process -WindowStyle Hidden -FilePath {} -ArgumentList {} -PassThru; $p.Id",
            ps_quote(&exe.to_string_lossy()),
            ps_quote(&commandline),
        );
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        if let Some(token) = token {
            command.env("UBUILDER_WEBHOOK_TOKEN", token);
        }
        let output = command
            .output()
            .map_err(|e| UbuilderError::io("cannot start daemon", e))?;
        if !output.status.success() {
            return Err(UbuilderError::new(
                ErrorCode::Io,
                format!(
                    "daemon launcher failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .map_err(|_| {
                UbuilderError::new(
                    ErrorCode::Io,
                    format!(
                        "daemon launcher did not return a pid: {}",
                        String::from_utf8_lossy(&output.stdout).trim()
                    ),
                )
            })
    }
    #[cfg(not(windows))]
    {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stdout)
            .map_err(|e| UbuilderError::io(format!("cannot open {}", stdout.display()), e))?;
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr)
            .map_err(|e| UbuilderError::io(format!("cannot open {}", stderr.display()), e))?;
        let mut command = Command::new(exe);
        command
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        if let Some(token) = token {
            command.env("UBUILDER_WEBHOOK_TOKEN", token);
        }
        command
            .spawn()
            .map(|child| child.id())
            .map_err(|e| UbuilderError::io("cannot start daemon", e))
    }
}

#[cfg(windows)]
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in arg.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(character);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes));
    quoted.push('"');
    quoted
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
    validate_webhook(args)?;
    std::fs::create_dir_all(&paths.dir)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", paths.dir.display()), e))?;
    write_pid(&paths.pid, std::process::id())?;
    let mut state = read_state(&paths.state).unwrap_or_default();
    let token = args.token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(port) = args.webhook {
        let address: std::net::IpAddr = args.bind.parse().map_err(|_| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("invalid webhook bind address '{}'", args.bind),
            )
        })?;
        let listener = TcpListener::bind((address, port)).map_err(|e| {
            UbuilderError::io(format!("cannot bind webhook {}:{port}", args.bind), e)
                .remediation("Choose another --webhook port or stop the process using it")
        })?;
        let tx = tx.clone();
        let token = token.clone().ok_or_else(|| {
            UbuilderError::new(ErrorCode::ConfigInvalid, "webhook token is required")
        })?;
        std::thread::spawn(move || {
            let mut limits = WebhookLimits::default();
            for stream in listener.incoming().flatten() {
                if serve(stream, &token, &mut limits) {
                    let _ = tx.send(());
                }
            }
        });
        eprintln!("webhook listening on http://{}:{port}/hook", args.bind);
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

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const RATE_LIMIT_REQUESTS: u32 = 60;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const DEDUP_CAPACITY: usize = 1024;

struct WebhookLimits {
    window_start: std::time::Instant,
    window_count: u32,
    seen: std::collections::HashSet<String>,
    seen_order: std::collections::VecDeque<String>,
}

impl Default for WebhookLimits {
    fn default() -> Self {
        Self {
            window_start: std::time::Instant::now(),
            window_count: 0,
            seen: std::collections::HashSet::new(),
            seen_order: std::collections::VecDeque::new(),
        }
    }
}

impl WebhookLimits {
    fn allow_request(&mut self) -> bool {
        if self.window_start.elapsed() >= RATE_LIMIT_WINDOW {
            self.window_start = std::time::Instant::now();
            self.window_count = 0;
        }
        self.window_count += 1;
        self.window_count <= RATE_LIMIT_REQUESTS
    }

    /// Returns `true` when the delivery id was already accepted.
    fn duplicate(&mut self, delivery: Option<&str>) -> bool {
        let Some(delivery) = delivery.filter(|value| !value.trim().is_empty()) else {
            return false;
        };
        if self.seen.contains(delivery) {
            return true;
        }
        self.seen.insert(delivery.to_string());
        self.seen_order.push_back(delivery.to_string());
        while self.seen_order.len() > DEDUP_CAPACITY {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen.remove(&old);
            }
        }
        false
    }
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    content_length: usize,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Result<HttpRequest> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        if raw.len() > MAX_HEADER_BYTES {
            return Ok(HttpRequest {
                method: String::new(),
                path: String::new(),
                headers: Vec::new(),
                content_length: MAX_BODY_BYTES + 1,
            });
        }
        let read = stream
            .read(&mut chunk)
            .map_err(|e| UbuilderError::io("cannot read webhook request", e))?;
        if read == 0 {
            break None;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(index) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break Some(index + 4);
        }
    };
    let header_end = header_end.unwrap_or(raw.len());
    let text = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    Ok(HttpRequest {
        method,
        path,
        headers,
        content_length,
    })
}

fn authorized(request: &HttpRequest, token: &str) -> bool {
    if let Some(value) = request.header("authorization") {
        if let Some(bearer) = value.strip_prefix("Bearer ") {
            if constant_time_eq(bearer.trim(), token) {
                return true;
            }
        }
    }
    request
        .header("x-ubuilder-token")
        .is_some_and(|value| constant_time_eq(value.trim(), token))
}

fn serve(mut stream: std::net::TcpStream, token: &str, limits: &mut WebhookLimits) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return respond(&mut stream, "400 Bad Request", "malformed request"),
    };
    if request.content_length > MAX_BODY_BYTES {
        return respond(
            &mut stream,
            "413 Payload Too Large",
            "request body too large",
        );
    }
    if request.method != "POST" || request.path != "/hook" {
        return respond(&mut stream, "405 Method Not Allowed", "POST /hook required");
    }
    if !limits.allow_request() {
        return respond(&mut stream, "429 Too Many Requests", "rate limit exceeded");
    }
    if !authorized(&request, token) {
        return respond(&mut stream, "401 Unauthorized", "invalid token");
    }
    let delivery = request
        .header("x-ubuilder-delivery")
        .or_else(|| request.header("x-github-delivery"));
    if limits.duplicate(delivery) {
        return respond(&mut stream, "409 Conflict", "duplicate delivery");
    }
    respond(&mut stream, "202 Accepted", "queued")
}

fn respond(stream: &mut std::net::TcpStream, status: &str, body: &str) -> bool {
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
    err_log: PathBuf,
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
            err_log: dir.join("daemon.err.log"),
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
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map_err(|e| UbuilderError::io("cannot stop daemon", e))?;
    if output.status.success() {
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
    let output = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .map_err(|e| UbuilderError::io("cannot stop daemon", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(UbuilderError::new(
            ErrorCode::Io,
            format!("kill failed for pid {pid}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    fn exchange(mut limits: WebhookLimits, request: &str) -> (WebhookLimits, String, bool) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let request = request.to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let accepted = serve(stream, "s3cret", &mut limits);
            (limits, accepted)
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        let (limits, accepted) = server.join().unwrap();
        (limits, response, accepted)
    }

    fn post(headers: &str) -> String {
        format!("POST /hook HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
    }

    #[test]
    fn rejects_missing_and_wrong_authentication() {
        let (_, response, accepted) = exchange(WebhookLimits::default(), &post(""));
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 401"));

        let (_, response, accepted) = exchange(
            WebhookLimits::default(),
            &post("Authorization: Bearer wrong\r\n"),
        );
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 401"));
    }

    #[test]
    fn accepts_bearer_and_header_tokens() {
        let (_, response, accepted) = exchange(
            WebhookLimits::default(),
            &post("Authorization: Bearer s3cret\r\n"),
        );
        assert!(accepted);
        assert!(response.starts_with("HTTP/1.1 202"));

        let (_, response, accepted) = exchange(
            WebhookLimits::default(),
            &post("X-Ubuilder-Token: s3cret\r\n"),
        );
        assert!(accepted);
        assert!(response.starts_with("HTTP/1.1 202"));
    }

    #[test]
    fn rejects_wrong_method_and_path() {
        let request =
            "GET /hook HTTP/1.1\r\nAuthorization: Bearer s3cret\r\nContent-Length: 0\r\n\r\n";
        let (_, response, accepted) = exchange(WebhookLimits::default(), request);
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 405"));
    }

    #[test]
    fn rejects_oversized_bodies_before_reading_them() {
        let request = post("Authorization: Bearer s3cret\r\nContent-Length: 70000\r\n");
        let (_, response, accepted) = exchange(WebhookLimits::default(), &request);
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 413"));
    }

    #[test]
    fn duplicate_deliveries_are_rejected() {
        let request = post("Authorization: Bearer s3cret\r\nX-Ubuilder-Delivery: delivery-1\r\n");
        let (limits, response, accepted) = exchange(WebhookLimits::default(), &request);
        assert!(accepted);
        assert!(response.starts_with("HTTP/1.1 202"));
        let (_, response, accepted) = exchange(limits, &request);
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 409"));
    }

    #[test]
    fn rate_limit_and_constant_time_comparison() {
        let limits = WebhookLimits {
            window_count: RATE_LIMIT_REQUESTS,
            ..WebhookLimits::default()
        };
        let (_, response, accepted) = exchange(limits, &post("Authorization: Bearer s3cret\r\n"));
        assert!(!accepted);
        assert!(response.starts_with("HTTP/1.1 429"));
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
