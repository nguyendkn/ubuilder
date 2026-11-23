//! `ubuilder secret` commands. Values are never printed.

use super::{fail, host, ok, Cli};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::secrets::{validate_name, SecretManager};
use crate::telemetry::Ui;
use clap::{Args, Subcommand};
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// List secret names, providers and lengths.
    List,
    /// Store or replace a secret in the local protected file.
    Set(SetArgs),
    /// Delete a secret from the local protected file.
    Delete { name: String },
    /// Show metadata for one secret without revealing its value.
    Inspect { name: String },
}

#[derive(Debug, Args)]
pub struct SetArgs {
    pub name: String,
    /// Read the value from this file (first line endings trimmed).
    #[arg(long, conflicts_with_all = ["value", "stdin"])]
    pub file: Option<PathBuf>,
    /// Read the value from standard input (noninteractive CI).
    #[arg(long, conflicts_with_all = ["value", "file"])]
    pub stdin: bool,
    /// Literal value. Deprecated: visible in shell history and process
    /// listings; prefer `--stdin` or interactive input.
    #[arg(long, conflicts_with_all = ["stdin", "file"])]
    pub value: Option<String>,
}

pub fn run(_cli: &Cli, command: &SecretCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let manager = SecretManager::open(&host.data_dir())?;
    match command {
        SecretCommand::List => match manager.list() {
            Ok(entries) => {
                if !ui.json() {
                    if entries.is_empty() {
                        ui.info("no secrets configured");
                    }
                    for entry in &entries {
                        ui.info(format!(
                            "{}  provider={} length={}{}",
                            entry.name,
                            entry.provider,
                            entry.length,
                            if entry.writable { "" } else { " (read-only)" }
                        ));
                    }
                }
                Ok(ok(ui, "secret list", json!({ "secrets": entries })))
            }
            Err(error) => Ok(fail(ui, "secret list", &error, None)),
        },
        SecretCommand::Set(args) => {
            if let Err(error) = validate_name(&args.name) {
                return Ok(fail(ui, "secret set", &error, None));
            }
            if args.value.is_some() && !ui.json() {
                ui.warn("--value is visible in shell history; prefer --stdin");
            }
            let value = match read_value(args) {
                Ok(value) => value,
                Err(error) => return Ok(fail(ui, "secret set", &error, None)),
            };
            if let Err(error) = manager.set_file(&args.name, &value) {
                return Ok(fail(ui, "secret set", &error, None));
            }
            if !ui.json() {
                ui.success(format!(
                    "stored secret '{}' in {}",
                    args.name,
                    manager.file_path().display()
                ));
                if let Ok(Some((_, "environment"))) = manager.resolve(&args.name) {
                    ui.warn(format!(
                        "an environment variable also defines '{}' and takes precedence at run time",
                        args.name
                    ));
                }
            }
            Ok(ok(
                ui,
                "secret set",
                json!({
                    "name": args.name,
                    "provider": "file",
                    "path": manager.file_path(),
                    "length": value.chars().count(),
                }),
            ))
        }
        SecretCommand::Delete { name } => match manager.delete_file(name) {
            Ok(true) => {
                if !ui.json() {
                    ui.success(format!("deleted secret '{name}'"));
                }
                Ok(ok(
                    ui,
                    "secret delete",
                    json!({ "name": name, "deleted": true }),
                ))
            }
            Ok(false) => {
                if let Ok(Some((_, provider))) = manager.resolve(name) {
                    if provider == "environment" {
                        let error = UbuilderError::new(
                            ErrorCode::Usage,
                            format!("secret '{name}' comes from the environment"),
                        )
                        .remediation("Unset the environment variable in the parent process");
                        return Ok(fail(ui, "secret delete", &error, None));
                    }
                }
                Ok(ok(
                    ui,
                    "secret delete",
                    json!({ "name": name, "deleted": false }),
                ))
            }
            Err(error) => Ok(fail(ui, "secret delete", &error, None)),
        },
        SecretCommand::Inspect { name } => match manager.inspect(name) {
            Ok(Some(info)) => {
                if !ui.json() {
                    ui.info(format!(
                        "{} provider={} length={}",
                        info.name, info.provider, info.length
                    ));
                }
                Ok(ok(
                    ui,
                    "secret inspect",
                    serde_json::to_value(info).unwrap_or_default(),
                ))
            }
            Ok(None) => {
                let error = UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("secret '{name}' is not configured"),
                );
                Ok(fail(ui, "secret inspect", &error, None))
            }
            Err(error) => Ok(fail(ui, "secret inspect", &error, None)),
        },
    }
}

fn read_value(args: &SetArgs) -> Result<String> {
    let value = if let Some(value) = &args.value {
        value.clone()
    } else if args.stdin {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .map_err(|error| UbuilderError::io("cannot read secret from stdin", error))?;
        trim_line_end(value)
    } else if let Some(path) = &args.file {
        let value = std::fs::read_to_string(path).map_err(|error| {
            UbuilderError::io(format!("cannot read secret file {}", path.display()), error)
        })?;
        trim_line_end(value)
    } else {
        rpassword::prompt_password(format!("Secret value for {}: ", args.name))
            .map_err(|error| UbuilderError::io("cannot read secret from terminal", error))?
    };
    if value.is_empty() {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            "secret value must not be empty",
        ));
    }
    Ok(value)
}

fn trim_line_end(mut value: String) -> String {
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    value
}
