use super::{
    input, require_input, resolve_workspace_path, ActionContext, ActionOutcome, ActionPlanContext,
    InputSpec, PendingCacheSave, PipelineAction,
};
use crate::error::Result;
use crate::planner::{BuildStep, InternalAction};
use async_trait::async_trait;
use indexmap::IndexMap;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "key",
        required: true,
        default: None,
        description: "Cache key; include toolchain and dependency lock information",
    },
    InputSpec {
        name: "path",
        required: true,
        default: None,
        description: "Workspace-relative directory to restore and save",
    },
    InputSpec {
        name: "restore",
        required: false,
        default: Some("true"),
        description: "Restore the directory before the step",
    },
    InputSpec {
        name: "save",
        required: false,
        default: Some("true"),
        description: "Save the directory at the end of the job when missing",
    },
];

pub struct CacheAction;

#[async_trait]
impl PipelineAction for CacheAction {
    fn id(&self) -> &'static str {
        "builtin/cache@v1"
    }

    fn description(&self) -> &'static str {
        "Restore and save a workspace directory in the local action cache"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), INPUTS, inputs)?;
        require_input(inputs, "key")?;
        require_input(inputs, "path")?;
        Ok(())
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "cache",
            "restore cache directory",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let key = require_input(inputs, "key")?;
        // The same action key can be evaluated by several jobs, including
        // matrix siblings whose job id encodes the target. Their restored
        // bytes are not interchangeable.
        let cache_key = format!("{}|job={}", key, context.job_id);
        let relative = require_input(inputs, "path")?;
        let _ = resolve_workspace_path(context.workspace, relative)?;
        let restore = parse_bool(input(inputs, "restore"), true);
        let save = parse_bool(input(inputs, "save"), true);
        let hit = if restore {
            context
                .cache
                .restore(&cache_key, context.workspace, relative)?
        } else {
            false
        };
        if !hit && save {
            context.pending_cache.push(PendingCacheSave {
                key: cache_key,
                path: relative.to_string(),
            });
        }
        Ok(ActionOutcome::plain(if hit {
            format!("cache hit for {key}")
        } else {
            format!("cache miss for {key}")
        })
        .with_output("cache-hit", hit.to_string())
        .with_output("key", key))
    }
}

fn parse_bool(raw: Option<&str>, default: bool) -> bool {
    raw.map(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(default)
}
