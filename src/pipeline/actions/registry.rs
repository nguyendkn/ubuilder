//! Built-in action registry. Local, offline and version-pinned.

use super::build::BuildAction;
use super::cache_action::CacheAction;
use super::checkout::CheckoutAction;
use super::checksum::ChecksumAction;
use super::package::PackageAction;
use super::setup::{SETUP_DOTNET, SETUP_NODE, SETUP_RUST};
use super::transfer::{DownloadArtifactAction, UploadArtifactAction};
use super::verify::VerifyAction;
use super::PipelineAction;

static CHECKOUT: CheckoutAction = CheckoutAction;
static BUILD: BuildAction = BuildAction;
static PACKAGE: PackageAction = PackageAction;
static VERIFY: VerifyAction = VerifyAction;
static UPLOAD: UploadArtifactAction = UploadArtifactAction;
static DOWNLOAD: DownloadArtifactAction = DownloadArtifactAction;
static CHECKSUM: ChecksumAction = ChecksumAction;
static CACHE: CacheAction = CacheAction;

static ACTIONS: &[&dyn PipelineAction] = &[
    &CHECKOUT,
    &SETUP_RUST,
    &SETUP_DOTNET,
    &SETUP_NODE,
    &BUILD,
    &PACKAGE,
    &VERIFY,
    &UPLOAD,
    &DOWNLOAD,
    &CHECKSUM,
    &CACHE,
];

pub fn all() -> &'static [&'static dyn PipelineAction] {
    ACTIONS
}

pub fn get(id: &str) -> Option<&'static dyn PipelineAction> {
    ACTIONS.iter().copied().find(|action| action.id() == id)
}

pub fn is_known(id: &str) -> bool {
    get(id).is_some()
}

pub fn ids() -> Vec<&'static str> {
    ACTIONS.iter().map(|action| action.id()).collect()
}

pub fn describe() -> serde_json::Value {
    serde_json::json!(ACTIONS
        .iter()
        .map(|action| serde_json::json!({
            "id": action.id(),
            "description": action.description(),
            "inputs": action.inputs().iter().map(|spec| serde_json::json!({
                "name": spec.name,
                "required": spec.required,
                "default": spec.default,
                "description": spec.description,
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}
