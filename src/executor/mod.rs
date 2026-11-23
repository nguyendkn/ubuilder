//! Safe structured process execution with streaming, timeouts and cancellation.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::security;
use crate::telemetry::Ui;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

const MAX_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// `None` uses the executor-wide allowlist.
    pub env_allowlist: Option<Vec<String>>,
    pub inherit_env: bool,
    pub timeout_secs: Option<u64>,
    pub retryable: bool,
    pub description: String,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_allowlist: None,
            inherit_env: true,
            timeout_secs: None,
            retryable: false,
            description: description.into(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    pub fn retryable(mut self, yes: bool) -> Self {
        self.retryable = yes;
        self
    }

    pub fn display(&self) -> String {
        let mut text = self.program.clone();
        for arg in &self.args {
            if arg.contains(' ') {
                text.push_str(&format!(" \"{arg}\""));
            } else {
                text.push(' ');
                text.push_str(arg);
            }
        }
        text
    }
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub allowlist: Vec<String>,
    pub log_dir: Option<PathBuf>,
    pub verbose: bool,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            allowlist: crate::config::default_env_allowlist(),
            log_dir: None,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionOutput {
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub duration_ms: u128,
    pub timed_out: bool,
    pub cancelled: bool,
    pub log_path: Option<PathBuf>,
}

impl ExecutionOutput {
    pub fn success(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == Some(0)
    }
}

#[derive(Clone)]
pub struct Executor {
    config: ExecutorConfig,
    secrets: Vec<String>,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Self {
        let secrets = security::secrets_from_env(&std::env::vars().collect::<BTreeMap<_, _>>());
        Self { config, secrets }
    }

    pub fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets.extend(secrets);
        self
    }

    pub fn config(&self) -> &ExecutorConfig {
        &self.config
    }

    pub async fn run(
        &self,
        spec: &CommandSpec,
        cancel: &CancellationToken,
        ui: &Ui,
        step_id: &str,
    ) -> Result<ExecutionOutput> {
        let started = Instant::now();
        let log_path = self.step_log_path(step_id);
        let mut log_file = self.open_log(step_id)?;
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        let env = self.build_env(spec);
        cmd.env_clear();
        cmd.envs(&env);
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        if self.config.verbose && !ui.json() {
            ui.info(format!("  $ {}", spec.display()));
        }
        let mut child = cmd.spawn().map_err(|e| {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::ToolchainMissing
            } else {
                ErrorCode::ExecutionFailed
            };
            UbuilderError::new(code, format!("cannot launch `{}`: {e}", spec.program))
                .remediation("Run `ubuilder doctor` to inspect installed toolchains")
                .details(serde_json::json!({ "step": step_id, "command": spec.display() }))
        })?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamLine>();
        if let Some(stdout) = stdout {
            spawn_reader(stdout, StreamKind::Stdout, tx.clone());
        }
        if let Some(stderr) = stderr {
            spawn_reader(stderr, StreamKind::Stderr, tx.clone());
        }
        drop(tx);

        let mut stdout_tail = Tail::default();
        let mut stderr_tail = Tail::default();
        let timeout = spec
            .timeout_secs
            .or(self.config_timeout())
            .map(Duration::from_secs);
        let sleep = timeout.map(tokio::time::sleep);
        tokio::pin!(sleep);
        let mut status: Option<std::process::ExitStatus> = None;
        let mut timed_out = false;
        let mut was_cancelled = false;
        let mut channel_closed = false;

        loop {
            if status.is_some() && channel_closed {
                break;
            }
            tokio::select! {
                line = rx.recv() => {
                    match line {
                        Some(line) => {
                            self.consume_line(&line, &mut stdout_tail, &mut stderr_tail, &mut log_file, ui);
                        }
                        None => channel_closed = true,
                    }
                }
                wait = child.wait(), if status.is_none() => {
                    status = Some(wait.map_err(|e| UbuilderError::new(
                        ErrorCode::ExecutionFailed,
                        format!("cannot wait for `{}`: {e}", spec.program),
                    ))?);
                }
                _ = cancel.cancelled(), if status.is_none() => {
                    was_cancelled = true;
                    terminate_tree(pid, &mut child).await;
                    status = Some(child.wait().await.map_err(|e| UbuilderError::new(
                        ErrorCode::ExecutionFailed,
                        format!("cannot wait for `{}`: {e}", spec.program),
                    ))?);
                }
                _ = async { if let Some(s) = sleep.as_mut().as_pin_mut() { s.await } }, if status.is_none() && timeout.is_some() => {
                    timed_out = true;
                    terminate_tree(pid, &mut child).await;
                    status = Some(child.wait().await.map_err(|e| UbuilderError::new(
                        ErrorCode::ExecutionFailed,
                        format!("cannot wait for `{}`: {e}", spec.program),
                    ))?);
                }
            }
        }

        // Give the reader tasks a brief window to flush after the child exits.
        if !channel_closed {
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(line) = rx.recv().await {
                    self.consume_line(&line, &mut stdout_tail, &mut stderr_tail, &mut log_file, ui);
                }
            })
            .await;
        }

        let exit_code = status.as_ref().and_then(|s| s.code());
        let output = ExecutionOutput {
            exit_code,
            stdout_tail: security::redact(&stdout_tail.finish(), &self.secrets),
            stderr_tail: security::redact(&stderr_tail.finish(), &self.secrets),
            duration_ms: started.elapsed().as_millis(),
            timed_out,
            cancelled: was_cancelled,
            log_path,
        };
        if let Some(file) = log_file.as_mut() {
            use std::io::Write;
            let _ = writeln!(
                file,
                "--- exit={:?} timed_out={} cancelled={} ---",
                output.exit_code, timed_out, was_cancelled
            );
        }
        Ok(output)
    }

    fn config_timeout(&self) -> Option<u64> {
        None
    }

    fn build_env(&self, spec: &CommandSpec) -> BTreeMap<String, String> {
        let allowlist = spec
            .env_allowlist
            .as_ref()
            .unwrap_or(&self.config.allowlist);
        let mut env = if spec.inherit_env {
            security::filtered_env(allowlist)
        } else {
            BTreeMap::new()
        };
        for (key, value) in &spec.env {
            env.insert(key.clone(), value.clone());
        }
        // Ensure PATH exists even with an over-narrow allowlist.
        if !env.contains_key("PATH") {
            if let Ok(path) = std::env::var("PATH") {
                env.insert("PATH".into(), path);
            }
        }
        env
    }

    fn consume_line(
        &self,
        line: &StreamLine,
        stdout_tail: &mut Tail,
        stderr_tail: &mut Tail,
        log_file: &mut Option<std::fs::File>,
        ui: &Ui,
    ) {
        let redacted = security::redact(&line.text, &self.secrets);
        match line.kind {
            StreamKind::Stdout => stdout_tail.push(&redacted),
            StreamKind::Stderr => stderr_tail.push(&redacted),
        }
        if let Some(file) = log_file.as_mut() {
            use std::io::Write;
            let _ = writeln!(file, "{}", redacted);
        }
        if self.config.verbose && !ui.json() {
            match line.kind {
                StreamKind::Stdout => eprintln!("    {redacted}"),
                StreamKind::Stderr => eprintln!("    {redacted}"),
            }
        }
    }

    fn open_log(&self, step_id: &str) -> Result<Option<std::fs::File>> {
        let Some(path) = self.step_log_path(step_id) else {
            return Ok(None);
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| UbuilderError::io(format!("cannot create {}", dir.display()), e))?;
        }
        let file = std::fs::File::create(&path)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", path.display()), e))?;
        Ok(Some(file))
    }

    fn step_log_path(&self, step_id: &str) -> Option<PathBuf> {
        let dir = self.config.log_dir.as_ref()?;
        let name: String = step_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Some(dir.join(format!("{name}.log")))
    }
}

#[derive(Debug, Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct StreamLine {
    kind: StreamKind,
    text: String,
}

fn spawn_reader<R>(reader: R, kind: StreamKind, tx: mpsc::UnboundedSender<StreamLine>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(text)) => {
                    if tx.send(StreamLine { kind, text }).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });
}

#[derive(Default)]
struct Tail {
    lines: Vec<String>,
    bytes: usize,
}

impl Tail {
    fn push(&mut self, line: &str) {
        self.bytes += line.len() + 1;
        self.lines.push(line.to_string());
        while self.bytes > MAX_TAIL_BYTES && !self.lines.is_empty() {
            let removed = self.lines.remove(0);
            self.bytes = self.bytes.saturating_sub(removed.len() + 1);
        }
    }

    fn finish(self) -> String {
        self.lines.join("\n")
    }
}

async fn terminate_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = pid {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            // The child is the leader of its own process group.
            unsafe {
                libc::killpg(pid as i32, libc::SIGTERM);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            unsafe {
                libc::killpg(pid as i32, libc::SIGKILL);
            }
        }
    }
    let _ = child.start_kill();
}

/// Shared log writer used by the scheduler to keep step logs in one directory.
pub fn prepare_log_dir(base: &Path) -> Result<PathBuf> {
    let dir = base.join("logs");
    std::fs::create_dir_all(&dir)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", dir.display()), e))?;
    Ok(dir)
}

/// API kept for symmetry: a mutex-shared cancellation token for Ctrl+C.
pub fn shared_cancel(token: CancellationToken) -> Arc<Mutex<CancellationToken>> {
    Arc::new(Mutex::new(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> CommandSpec {
        #[cfg(windows)]
        let spec = CommandSpec::new("cmd", "test").args(["/C", script]);
        #[cfg(not(windows))]
        let spec = CommandSpec::new("sh", "test").args(["-c", script]);
        spec
    }

    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let ui = Ui::new(false, false, true);
        let executor = Executor::new(ExecutorConfig::default());
        let out = executor
            .run(
                &shell("echo hello & exit 3"),
                &CancellationToken::new(),
                &ui,
                "test-echo",
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(out.stdout_tail.contains("hello"));
        assert!(!out.success());
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        let ui = Ui::new(false, false, true);
        let executor = Executor::new(ExecutorConfig::default());
        #[cfg(windows)]
        let spec = shell("ping -n 6 127.0.0.1 >nul").timeout_secs(1);
        #[cfg(not(windows))]
        let spec = shell("sleep 5").timeout_secs(1);
        let started = Instant::now();
        let out = executor
            .run(&spec, &CancellationToken::new(), &ui, "test-timeout")
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn cancellation_stops_child() {
        let ui = Ui::new(false, false, true);
        let executor = Executor::new(ExecutorConfig::default());
        let token = CancellationToken::new();
        let cloned = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cloned.cancel();
        });
        #[cfg(windows)]
        let spec = shell("ping -n 10 127.0.0.1 >nul");
        #[cfg(not(windows))]
        let spec = shell("sleep 10");
        let out = executor
            .run(&spec, &token, &ui, "test-cancel")
            .await
            .unwrap();
        assert!(out.cancelled);
    }

    #[tokio::test]
    async fn env_allowlist_is_enforced() {
        let ui = Ui::new(false, false, true);
        let config = ExecutorConfig {
            allowlist: vec!["PATH".into()],
            ..Default::default()
        };
        let executor = Executor::new(config);
        #[cfg(windows)]
        let spec = shell("echo %UBUILDER_TEST_VAR%").env("UBUILDER_TEST_VAR", "visible");
        #[cfg(not(windows))]
        let spec = shell("printf '%s' \"$UBUILDER_TEST_VAR\"").env("UBUILDER_TEST_VAR", "visible");
        let out = executor
            .run(&spec, &CancellationToken::new(), &ui, "test-env")
            .await
            .unwrap();
        assert!(out.stdout_tail.contains("visible"));
    }

    #[tokio::test]
    async fn missing_program_reports_toolchain_missing() {
        let ui = Ui::new(false, false, true);
        let executor = Executor::new(ExecutorConfig::default());
        let spec = CommandSpec::new("definitely-not-a-real-program-xyz", "missing");
        let err = executor
            .run(&spec, &CancellationToken::new(), &ui, "test-missing")
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
    }
}
