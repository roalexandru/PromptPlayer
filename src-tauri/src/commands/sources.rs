//! Remote prompt-source IPC — public GitHub repositories (§7.2).
//!
//! Adding a source is a trust decision (its prompts get typed into whatever
//! the user has focused), so the flow is explicit: add, fetch, review, enable.
//! See `crate::sources` for the posture these commands enforce.

use crate::app::context::AppContext;
use crate::config::SourceSpec;
use crate::error::{into_ipc, AppError, IpcResult};
use crate::prompts::Prompt;
use crate::sources::{self, SourceStatus};
use crate::telemetry::{self, TelemetryEvent};

#[tauri::command]
#[specta::specta]
pub fn list_sources(ctx: tauri::State<'_, AppContext>) -> Vec<SourceStatus> {
    ctx.config
        .get()
        .sources
        .iter()
        .map(sources::status)
        .collect()
}

/// Register a source and fetch it immediately.
///
/// Fetching here rather than on the next launch is deliberate: the user just
/// typed a repo name and is looking at the dialog, so this is the only moment
/// where a 404 or a rate-limit message can be acted on.
#[tauri::command]
#[specta::specta]
pub async fn add_source(
    repo: String,
    git_ref: Option<String>,
    subdir: Option<String>,
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<SourceStatus> {
    // Normalize first: the user may have pasted a browser URL.
    let (owner, name) = match sources::parse_repo(&repo) {
        Ok(v) => v,
        Err(e) => return into_ipc(Err(AppError::InvalidArg(e))),
    };
    let spec = SourceSpec {
        repo: format!("{owner}/{name}"),
        git_ref: git_ref.filter(|r| !r.trim().is_empty()),
        subdir: subdir.filter(|s| !s.trim().is_empty()),
        enabled: true,
    };
    let ctx_owned = ctx.inner().clone();
    if ctx_owned
        .config
        .get()
        .sources
        .iter()
        .any(|s| s.id() == spec.id())
    {
        return into_ipc(Err(AppError::InvalidArg(format!(
            "{} is already a source",
            spec.repo
        ))));
    }

    match sources::fetch(&spec).await {
        Ok(outcome) => {
            if let Err(e) = ctx_owned.config.update(|c| c.sources.push(spec.clone())) {
                return into_ipc(Err(AppError::Internal(e)));
            }
            telemetry::send(
                &app,
                TelemetryEvent::SourceRefreshed {
                    changed: outcome.changed(),
                    prompt_count: outcome.prompt_count().min(u32::from(u16::MAX)) as u16,
                },
            );
            reload_with_sources(&app, &ctx_owned);
            into_ipc(Ok(sources::status(&spec)))
        }
        Err(e) => into_ipc(Err(AppError::Source(e))),
    }
}

/// Forget a source, delete its cache, and drop its prompts' enablement.
#[tauri::command]
#[specta::specta]
pub fn remove_source(
    source_id: String,
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    let cfg = ctx.config.get();
    let Some(spec) = cfg.sources.iter().find(|s| s.id() == source_id).cloned() else {
        return into_ipc(Err(AppError::InvalidArg(format!(
            "no source with id {source_id}"
        ))));
    };
    let prefix = format!("{source_id}/");
    let result = ctx.config.update(|c| {
        c.sources.retain(|s| s.id() != source_id);
        // Leave no orphaned allow-list entries behind — re-adding the same
        // repo later must start from "off until reviewed" again.
        c.enabled_remote.retain(|id| !id.starts_with(&prefix));
    });
    sources::remove_cache(&spec);
    if result.is_ok() {
        reload_with_sources(&app, ctx.inner());
    }
    into_ipc(result.map(|_| ()).map_err(AppError::Internal))
}

/// Re-fetch every enabled source. Returns their post-refresh status.
#[tauri::command]
#[specta::specta]
pub async fn refresh_sources(
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<Vec<SourceStatus>> {
    let ctx_owned = ctx.inner().clone();
    let specs: Vec<SourceSpec> = ctx_owned
        .config
        .get()
        .sources
        .into_iter()
        .filter(|s| s.enabled)
        .collect();
    let mut errors = Vec::new();
    let mut any_changed = false;
    for spec in &specs {
        match sources::fetch(spec).await {
            Ok(outcome) => {
                any_changed |= outcome.changed();
                telemetry::send(
                    &app,
                    TelemetryEvent::SourceRefreshed {
                        changed: outcome.changed(),
                        prompt_count: outcome.prompt_count().min(u32::from(u16::MAX)) as u16,
                    },
                );
            }
            Err(e) => errors.push(format!("{}: {}", spec.repo, e)),
        }
    }
    if any_changed {
        reload_with_sources(&app, &ctx_owned);
    }
    if !errors.is_empty() && specs.len() == errors.len() {
        // Every source failed — surface it rather than reporting success with
        // stale caches.
        return into_ipc(Err(AppError::Source(errors.join("; "))));
    }
    for e in &errors {
        tracing::warn!("source refresh: {}", e);
    }
    into_ipc(Ok(ctx_owned
        .config
        .get()
        .sources
        .iter()
        .map(sources::status)
        .collect()))
}

/// Enable or disable one remote prompt.
///
/// Remote enablement lives in `promptplayer.yaml`, not in the prompt file:
/// the cache directory is replaced wholesale on every refresh, so a flag
/// written there would be lost the next time the repo moved.
#[tauri::command]
#[specta::specta]
pub fn set_remote_prompt_enabled(
    prompt_id: String,
    enabled: bool,
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    let Some(prompt) = ctx.prompts.find(&prompt_id) else {
        return into_ipc(Err(AppError::PromptNotFound(prompt_id)));
    };
    if !prompt.origin.is_remote() {
        return into_ipc(Err(AppError::InvalidArg(format!(
            "{prompt_id} is a local prompt — use set_prompt_enabled"
        ))));
    }
    if enabled {
        // Same rule as a local prompt: enabling is the moment its triggers
        // start competing, so a collision has to be refused here rather than
        // silently dropped by the matcher's duplicate guard later.
        let candidate = Prompt {
            enabled: true,
            ..prompt.clone()
        };
        if let Err(e) = ctx.prompts.validate_unique_triggers(&candidate) {
            return into_ipc(Err(e));
        }
    }
    let result = ctx.config.update(|c| {
        c.enabled_remote.retain(|id| id != &prompt_id);
        if enabled {
            c.enabled_remote.push(prompt_id.clone());
        }
    });
    if result.is_ok() {
        // Flip the in-memory copy too, so the change is live without a reload.
        let _ = ctx.prompts.modify(&prompt_id, |p| p.enabled = enabled);
        crate::app::setup::reindex_after_mutation(&app, ctx.inner());
    }
    into_ipc(result.map(|_| ()).map_err(AppError::Internal))
}

/// Copy a remote prompt into the local library so it can be edited.
#[tauri::command]
#[specta::specta]
pub fn fork_prompt(
    prompt_id: String,
    app: tauri::AppHandle,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<Prompt> {
    let result = ctx.prompts.fork_to_local(&prompt_id);
    if result.is_ok() {
        crate::app::setup::reindex_after_mutation(&app, ctx.inner());
    }
    into_ipc(result)
}

/// Reload the library (local files plus every source cache) and reindex.
pub fn reload_with_sources(app: &tauri::AppHandle, ctx: &AppContext) {
    crate::app::setup::reload_library(ctx);
    crate::app::setup::reindex_after_mutation(app, ctx);
}
