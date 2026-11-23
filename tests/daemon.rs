//! Daemon lifecycle and authenticated webhook behaviour.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn ubuilder() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ubuilder"))
}

struct DaemonSandbox {
    temp: TempDir,
    project: PathBuf,
    workflow: PathBuf,
}

impl DaemonSandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let workflow = project.join(".ubuilder/workflows/build.yml");
        std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        std::fs::write(
            &workflow,
            "version: 1\nname: Daemon\njobs:\n  noop:\n    steps:\n      - run: echo daemon\n",
        )
        .unwrap();
        Self {
            temp,
            project,
            workflow,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = ubuilder();
        command
            .args(["--path", self.project.to_str().unwrap(), "--json"])
            .args(args)
            .env("UBUILDER_DATA_DIR", self.temp.path().join("data"))
            .env("UBUILDER_CACHE_DIR", self.temp.path().join("cache"))
            .env("NO_COLOR", "1");
        command
    }

    fn json(&self, args: &[&str]) -> (Output, Value) {
        let output = self.command(args).output().unwrap();
        let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?} did not print JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output, json)
    }

    fn start(&self, extra: &[&str]) -> (Output, Value) {
        let mut args = vec![
            "daemon",
            "start",
            "--file",
            self.workflow.to_str().unwrap(),
            "--interval-secs",
            "60",
        ];
        args.extend(extra);
        self.json(&args)
    }

    fn stop(&self) {
        let _ = self.json(&["daemon", "stop"]);
    }
}

impl Drop for DaemonSandbox {
    fn drop(&mut self) {
        self.stop();
    }
}

fn request(port: u16, raw: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn wait_for_webhook(port: u16, token: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let raw = format!(
            "POST /hook HTTP/1.1\r\nAuthorization: Bearer {token}\r\nX-Ubuilder-Delivery: startup\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(raw.as_bytes()).unwrap();
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_ok() && !response.is_empty() {
                return response;
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon webhook never became reachable"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn start_status_trigger_restart_and_stop() {
    let sandbox = DaemonSandbox::new();
    let (output, started) = sandbox.start(&[]);
    assert!(output.status.success(), "{started}");
    assert_eq!(started["data"]["running"], true);
    let pid = started["data"]["pid"].as_u64().unwrap();

    let (_, again) = sandbox.start(&[]);
    assert_eq!(again["data"]["already_running"], true);
    assert_eq!(again["data"]["pid"].as_u64().unwrap(), pid);

    let (_, status) = sandbox.json(&["daemon", "status"]);
    assert_eq!(status["data"]["running"], true);
    assert_eq!(status["data"]["pid"].as_u64().unwrap(), pid);

    let (_, triggered) = sandbox.json(&["daemon", "trigger"]);
    assert_eq!(triggered["data"]["queued"], true);

    sandbox.stop();
    let (_, stopped) = sandbox.json(&["daemon", "status"]);
    assert_eq!(stopped["data"]["running"], false);
    assert!(stopped["data"]["pid"].is_null());

    // A second start must reuse the same data directory cleanly.
    let (output, restarted) = sandbox.start(&[]);
    assert!(output.status.success(), "{restarted}");
    assert_eq!(restarted["data"]["running"], true);
}

#[test]
fn stale_pid_is_removed_and_stop_is_idempotent() {
    let sandbox = DaemonSandbox::new();
    sandbox.command(&["daemon", "status"]).output().unwrap();
    let pid_file = sandbox.temp.path().join("data/daemon/daemon.pid");
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, "999999").unwrap();
    let (_, stopped) = sandbox.json(&["daemon", "stop"]);
    assert_eq!(stopped["data"]["stopped"], false);
    assert!(!pid_file.exists());

    let (_, stopped) = sandbox.json(&["daemon", "stop"]);
    assert_eq!(stopped["data"]["stopped"], false);
}

#[test]
fn webhook_requires_configuration_and_authenticates_requests() {
    let sandbox = DaemonSandbox::new();
    let (output, missing) = sandbox.start(&["--webhook", &free_port().to_string()]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(missing["error"]["code"], "CONFIG_INVALID");

    let (output, invalid_bind) =
        sandbox.start(&["--webhook", &free_port().to_string(), "--bind", "not-an-ip"]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(invalid_bind["error"]["code"], "CONFIG_INVALID");

    let port = free_port();
    let (output, started) =
        sandbox.start(&["--webhook", &port.to_string(), "--token", "webhook-secret"]);
    assert!(output.status.success(), "{started}");

    let accepted = wait_for_webhook(port, "webhook-secret");
    assert!(accepted.starts_with("HTTP/1.1 202"), "{accepted}");

    let unauthorized = request(
        port,
        "POST /hook HTTP/1.1\r\nAuthorization: Bearer wrong\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

    let duplicate = request(
        port,
        "POST /hook HTTP/1.1\r\nAuthorization: Bearer webhook-secret\r\nX-Ubuilder-Delivery: startup\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert!(duplicate.starts_with("HTTP/1.1 409"), "{duplicate}");
}

#[test]
fn daemon_child_receives_webhook_token_only_through_the_environment() {
    let sandbox = DaemonSandbox::new();
    let port = free_port();
    let (output, started) = sandbox.start(&[
        "--webhook",
        &port.to_string(),
        "--token",
        "environment-only",
    ]);
    assert!(output.status.success(), "{started}");
    let pid = started["data"]["pid"].as_u64().unwrap();

    #[cfg(windows)]
    let command_line = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine"),
        ])
        .output()
        .unwrap();
    #[cfg(not(windows))]
    let command_line = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&command_line.stdout).to_string();
    assert!(
        !text.contains("environment-only"),
        "webhook token leaked into argv: {text}"
    );

    let response = wait_for_webhook(port, "environment-only");
    assert!(response.starts_with("HTTP/1.1 202"), "{response}");
}
