//! Deterministic workflow graph rendering (Mermaid, Graphviz DOT, JSON).

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::ir::WorkflowIr;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    Mermaid,
    Dot,
    Json,
}

impl GraphFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "mermaid" => Ok(Self::Mermaid),
            "dot" | "graphviz" => Ok(Self::Dot),
            "json" => Ok(Self::Json),
            other => Err(UbuilderError::new(
                ErrorCode::Usage,
                format!("unknown graph format '{other}'"),
            )
            .remediation("Use mermaid, dot or json")),
        }
    }
}

pub fn render(ir: &WorkflowIr, format: GraphFormat) -> Result<String> {
    match format {
        GraphFormat::Mermaid => Ok(mermaid(ir)),
        GraphFormat::Dot => Ok(dot(ir)),
        GraphFormat::Json => serde_json::to_string_pretty(&json!({
            "workflow": ir.name,
            "digest": ir.source_digest,
            "waves": ir.waves,
            "jobs": ir.jobs.iter().map(|job| json!({
                "id": job.id,
                "base_id": job.base_id,
                "name": job.name,
                "needs": job.needs,
                "matrix": job.matrix,
                "runner": {
                    "os": job.runner.os,
                    "arch": job.runner.arch,
                    "native": job.runner.native,
                },
            })).collect::<Vec<_>>(),
        }))
        .map_err(|error| UbuilderError::new(ErrorCode::Internal, error.to_string())),
    }
}

fn node_id(index: usize) -> String {
    format!("job{index}")
}

fn label(job: &crate::pipeline::ir::IrJob) -> String {
    let mut text = job.name.replace('"', "'");
    if !job.matrix.is_empty() {
        let values = job
            .matrix
            .iter()
            .map(|(key, value)| {
                let rendered = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                format!("{key}={}", rendered.replace('"', "'"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str(&format!(" [{values}]"));
    }
    text
}

fn mermaid(ir: &WorkflowIr) -> String {
    let mut out = String::from("graph TD\n");
    for (index, job) in ir.jobs.iter().enumerate() {
        out.push_str(&format!("  {}([\"{}\"])\n", node_id(index), label(job)));
    }
    for (index, job) in ir.jobs.iter().enumerate() {
        for need in &job.needs {
            if let Some(dep) = ir.jobs.iter().position(|candidate| candidate.id == *need) {
                out.push_str(&format!("  {} --> {}\n", node_id(dep), node_id(index)));
            }
        }
    }
    out
}

fn dot(ir: &WorkflowIr) -> String {
    let mut out = String::from("digraph pipeline {\n  rankdir=LR;\n");
    for (index, job) in ir.jobs.iter().enumerate() {
        out.push_str(&format!(
            "  {} [label=\"{}\"];\n",
            node_id(index),
            label(job).replace('\\', "\\\\")
        ));
    }
    for (index, job) in ir.jobs.iter().enumerate() {
        for need in &job.needs {
            if let Some(dep) = ir.jobs.iter().position(|candidate| candidate.id == *need) {
                out.push_str(&format!("  {} -> {};\n", node_id(dep), node_id(index)));
            }
        }
    }
    out.push_str("}\n");
    out
}
