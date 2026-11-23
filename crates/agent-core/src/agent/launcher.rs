//! The single run entry point for every host (desktop chats, schedules, CLI, SDK).
//!
//! `launch` validates the turn, resolves the chat's agent or the caller's model, gates and
//! loads attachments, stores the user turn and the assistant placeholder, installs the
//! run's permission scope, compiles the request and hands it to the coordinator. Hosts
//! supply only already-created services (`RunServices`) and an event sink; opening the
//! database, loading stores, connecting MCP servers and owning the runtime stay with them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use ts_rs::TS;

use super::context::{self, AgentPrompt, PromptProfile, ToolContext};
use super::environment::{EnvContext, Environment};
use super::{FallbackModel, RunCoordinator, RunRecorder, RunRequest, RunSink};
use crate::agents::ModelRef;
use crate::domain::{ChatId, ContentBlock, RunId};
use crate::mcp::{McpManager, PermissionScope, ScopePolicies};
use crate::providers::registry::ProviderStore;
use crate::settings::SettingsStore;
use crate::storage::agents as agents_repo;
use crate::storage::chats as repo;
use crate::storage::{AttachmentKind, AttachmentStore, ChatSummary, Db, StoredMessage};
use crate::{Error, Result};

/// Hard cap on a single user prompt; larger inputs go through attachments.
pub const MAX_PROMPT_CHARS: usize = 64 * 1024;
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 20;

/// Already-created handles a run needs, assembled once per process by the host. Cheap to
/// clone; every field is a shared handle.
#[derive(Clone)]
pub struct RunServices {
    pub db: Db,
    pub providers: Arc<ProviderStore>,
    pub attachments: Arc<AttachmentStore>,
    pub mcp: Arc<McpManager>,
    pub coordinator: Arc<RunCoordinator>,
    /// App-wide settings (`settings.json`), read once per launch.
    pub settings: Arc<SettingsStore>,
}

/// Inputs to `launch`. Rust-only: never exported to the webview.
pub struct RunParams {
    pub chat_id: ChatId,
    /// Provider + model for plain chats; ignored for agent chats.
    pub model: Option<ModelRef>,
    pub prompt: String,
    pub attachment_ids: Vec<String>,
    pub replace_from: Option<String>,
    /// Started by a schedule (nobody is watching).
    pub unattended: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct RunStarted {
    pub run_id: RunId,
    /// The chat after the user turn was stored (title, agent, counts).
    pub chat: ChatSummary,
    pub user_message: StoredMessage,
    pub assistant_message: StoredMessage,
}

/// Runs file- or CPU-bound work off the async runtime and off the database worker.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
}

/// Persists the user turn, creates the assistant placeholder, compiles context from stored
/// history and starts streaming. If `replace_from` is set (retry), that message and everything
/// after it is removed first. Agent chats ignore `params.model`: their models, tools and
/// prompt come from the stored agent. `on_prepared` runs after the turn is stored and right
/// before streaming starts (the scheduler announces the run there so the UI sees it before
/// its first event).
///
/// Must be awaited inside a Tokio runtime (blocking reads use `tokio::task::spawn_blocking`).
///
/// The sequence — validate → in-process busy check → resolve agent/provider/attachments →
/// one write transaction (busy check against the database, truncate, user turn,
/// placeholder) → compile → `set_scope` → `on_prepared` → `start_with_id`. The database
/// check makes "one run per chat" hold across processes sharing the data root and between
/// concurrent launches in one process: of two launches for a chat, the later one gets
/// `RunBusy` before anything is stored. `Completed` means the final write was queued on the
/// database worker, not that it succeeded; a host that needs the outcome reads the stored
/// status after the terminal event.
pub async fn launch<S: RunSink>(
    services: &RunServices,
    params: RunParams,
    sink: S,
    on_prepared: impl FnOnce(&RunStarted) + Send,
) -> Result<RunStarted> {
    let RunParams {
        chat_id,
        model: selection,
        prompt,
        attachment_ids,
        replace_from,
        unattended,
    } = params;
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() && attachment_ids.is_empty() {
        return Err(Error::InvalidInput("prompt is empty".into()));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(Error::InvalidInput(format!(
            "prompt exceeds {MAX_PROMPT_CHARS} characters"
        )));
    }
    if attachment_ids.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(Error::InvalidInput("too many attachments".into()));
    }
    if services.coordinator.active_run(chat_id).is_some() {
        return Err(Error::RunBusy);
    }
    // Read once: a change applies to runs started after it.
    let share_environment = services.settings.get().general.share_environment;

    // Rust owns agent configuration: an agent chat's models, tools and prompt are read from
    // the database, never taken from the caller.
    // Tool permissions come from the agent, or for plain chats from the chat's overrides.
    let (agent, chat_overrides) = services
        .db
        .call(move |c| {
            let agent = repo::get_summary(c, chat_id)?
                .agent_id
                .map(|a| agents_repo::get_agent(c, a))
                .transpose()?;
            let overrides = match agent {
                Some(_) => None,
                None => Some(repo::get_tool_overrides(c, chat_id)?),
            };
            Ok((agent, overrides))
        })
        .await?;
    let (provider_id, model) = match &agent {
        Some(a) => (
            a.default_model.provider_id.clone(),
            a.default_model.model.clone(),
        ),
        None => selection
            .map(|m| (m.provider_id, m.model.trim().to_owned()))
            .filter(|(_, m)| !m.is_empty())
            .ok_or_else(|| Error::InvalidInput("choose a model first".into()))?,
    };
    let missing_provider = |e: Error| {
        match (&agent, e) {
        (Some(a), Error::NotFound(_)) => Error::InvalidInput(format!(
            "the model provider for agent \"{}\" was removed — edit the agent to choose another model",
            a.name
        )),
        (_, e) => e,
    }
    };

    let provider = services
        .providers
        .build(&provider_id)
        .map_err(missing_provider)?;
    // capabilities_for resolves the context window per provider+model:
    // per-model override → provider default → adapter-declared.
    let caps = services
        .providers
        .capabilities_for(&provider_id, &model)
        .map_err(missing_provider)?;
    let mut context_window = caps.context_window;
    let mut notices = Vec::new();

    // Agent fallback: resolved up front so a broken fallback is reported, not discovered
    // mid-failure. The request is compiled to fit the smaller of the two windows.
    let mut fallback = None;
    if let Some(fb) = agent.as_ref().and_then(|a| a.fallback_model.clone()) {
        let resolved = services.providers.build(&fb.provider_id).and_then(|p| {
            Ok((
                p,
                services
                    .providers
                    .capabilities_for(&fb.provider_id, &fb.model)?,
            ))
        });
        match resolved {
            Ok((p, fb_caps)) => {
                context_window = Some(
                    context_window
                        .unwrap_or(context::DEFAULT_CONTEXT_TOKENS)
                        .min(
                            fb_caps
                                .context_window
                                .unwrap_or(context::DEFAULT_CONTEXT_TOKENS),
                        ),
                );
                fallback = Some(FallbackModel {
                    provider: p,
                    provider_id: fb.provider_id,
                    model: fb.model,
                });
            }
            Err(e) => notices.push(format!("The fallback model is unavailable ({e}).")),
        }
    }
    let run_id = RunId::new();

    // Attachments are resolved in two steps each: what to read comes from the database,
    // the bytes are read and base64-encoded on a blocking thread. Page images make these
    // reads far too large for the single database worker.
    // This turn's attachments first, so nothing is stored if one is unusable or unreadable.
    let store = services.attachments.clone();
    let (ids, gate) = (attachment_ids.clone(), caps.clone());
    let sources = services
        .db
        .call(move |conn| {
            ids.iter()
                .map(|id| {
                    let meta = store
                        .get(conn, id)?
                        .ok_or_else(|| Error::NotFound(format!("attachment {id}")))?;
                    // The authority on what a model can be sent; the UI only mirrors it.
                    if let Some(reason) = meta.unusable_reason(&gate) {
                        return Err(Error::InvalidInput(format!("{}: {reason}", meta.name)));
                    }
                    store.source(conn, meta, gate.images)
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    let mut contents = HashMap::new();
    let mut blocks = Vec::new();
    if !sources.is_empty() {
        let store = services.attachments.clone();
        let loaded = blocking(move || {
            sources
                .into_iter()
                .map(|s| Ok((store.load_content(&s)?, s.meta)))
                .collect::<Result<Vec<_>>>()
        })
        .await?;
        for (content, meta) in loaded {
            contents.insert(meta.id.clone(), content);
            let pages_rendered =
                (meta.kind() == AttachmentKind::Pdf).then_some(meta.pages_rendered);
            blocks.push(ContentBlock::Attachment {
                id: meta.id,
                name: meta.name,
                mime: meta.mime,
                char_count: meta.char_count,
                page_count: meta.page_count,
                pages_rendered,
            });
        }
    }
    if !prompt.is_empty() {
        blocks.push(ContentBlock::Text { text: prompt });
    }

    // One database round trip: store the turn (`start_turn`: busy check, retry truncation,
    // user turn, placeholder — one write transaction) and load everything needed to compile
    // the request.
    let store = services.attachments.clone();
    let (pid, mdl) = (provider_id.clone(), model.clone());
    let images = caps.images;
    let prepared = services
        .db
        .call(move |conn| {
            let (user_id, assistant_id) = repo::start_turn(
                conn,
                chat_id,
                replace_from.as_deref(),
                &blocks,
                &attachment_ids,
                run_id,
                &pid,
                &mdl,
            )?;
            let messages = repo::list_messages(conn, chat_id)?;
            // Attachment payloads for earlier turns too, so history stays coherent.
            let mut seen: HashSet<&str> = attachment_ids.iter().map(String::as_str).collect();
            let mut earlier = Vec::new();
            for m in &messages {
                for b in &m.blocks {
                    if let ContentBlock::Attachment { id, .. } = b {
                        if seen.insert(id) {
                            if let Some(meta) = store.get(conn, id)? {
                                earlier.push(store.source(conn, meta, images)?);
                            }
                        }
                    }
                }
            }
            let chat = repo::get_summary(conn, chat_id)?;
            Ok((user_id, assistant_id, messages, earlier, chat))
        })
        .await?;
    let (user_id, assistant_id, messages, earlier, chat) = prepared;
    // From here on the placeholder exists: a failure must close it, or the chat would
    // report busy (`start_turn`) until the next startup recovery.
    let fail = |e: Error| {
        fail_placeholder(&services.db, &assistant_id, &e);
        e
    };
    if !earlier.is_empty() {
        let store = services.attachments.clone();
        // Best effort: an earlier attachment that can no longer be read is sent as
        // "unavailable" rather than failing the new turn.
        let loaded = blocking(move || {
            Ok(earlier
                .into_iter()
                .filter_map(|s| Some((s.meta.id.clone(), store.load_content(&s).ok()?)))
                .collect::<Vec<_>>())
        })
        .await
        .map_err(fail)?;
        contents.extend(loaded);
    }

    let user_message = messages
        .iter()
        .find(|m| m.id == user_id)
        .cloned()
        .ok_or_else(|| fail(Error::Internal("user message vanished".into())))?;
    let assistant_message = messages
        .iter()
        .find(|m| m.id == assistant_id)
        .cloned()
        .ok_or_else(|| fail(Error::Internal("assistant message vanished".into())))?;

    // History = everything complete/interrupted before the placeholder, as domain messages.
    let stored: Vec<&StoredMessage> = messages
        .iter()
        .filter(|m| m.id != assistant_id && !m.blocks.is_empty())
        .collect();
    let mut history: Vec<_> = stored.iter().map(|m| m.to_message()).collect();
    // Off means nothing is collected. Stamps use the OS zone's rules at each message's
    // instant, like schedules.
    let env = if share_environment {
        let environment = blocking(|| Ok(Environment::detect())).await.map_err(fail)?;
        EnvContext::new(
            &environment,
            stored.iter().map(|m| m.created_at),
            &chrono::Local,
        )
    } else {
        EnvContext::default()
    };
    // Provider state only replays to its own protocol family; drop the rest before
    // budgeting so it costs neither tokens nor request bytes.
    let families: Vec<&str> = [
        provider.family(),
        fallback.as_ref().and_then(|f| f.provider.family()),
    ]
    .into_iter()
    .flatten()
    .collect();
    context::retain_opaque_for(&mut history, &families);

    // Tool snapshot: catalog from connected servers (skipped when the model can't call
    // tools), the system-prompt section, and a token reservation for the definitions.
    // The run's permission scope (agent or chat) decides which tools it sees and their
    // policies; it stays installed so dispatch re-checks it on every call.
    let mut tool_ctx = ToolContext::default();
    let mut run_tools = None;
    let (scope, policies) = match (&agent, chat_overrides) {
        (Some(a), _) => (PermissionScope::Agent(a.id), a.scope_policies()),
        (None, o) => (
            PermissionScope::Chat(chat_id),
            ScopePolicies::Chat(o.unwrap_or_default()),
        ),
    };
    services.mcp.set_scope(scope, policies);
    if caps.tools {
        if let Some((tools, prompt, def_tokens)) = services.mcp.prepare_tools(
            &history,
            context_window.unwrap_or(context::DEFAULT_CONTEXT_TOKENS),
            &scope,
        ) {
            tool_ctx.definition_tokens = def_tokens;
            tool_ctx.prompt = prompt;
            run_tools = Some(tools);
        }
    }

    let profile = match &agent {
        Some(a) => PromptProfile::Agent(AgentPrompt {
            name: a.name.clone(),
            description: a.description.clone(),
            instructions: a.system_prompt.clone(),
            unattended,
        }),
        None => PromptProfile::Default,
    };
    let compiled = context::compile(
        &model,
        &history,
        context_window,
        caps.max_output_tokens,
        &contents,
        caps.images,
        &tool_ctx,
        &profile,
        &env,
    );
    if compiled.trimmed_messages > 0 {
        notices.push(format!(
            "Older messages were left out to fit the model's context window ({} omitted).",
            compiled.trimmed_messages
        ));
    }

    let started = RunStarted {
        run_id,
        chat,
        user_message,
        assistant_message,
    };
    on_prepared(&started);
    let sink = RunRecorder::new(sink, services.db.clone(), assistant_id);
    let result = services.coordinator.start_with_id(
        run_id,
        RunRequest {
            chat_id,
            provider,
            completion: compiled.request,
            tools: run_tools,
            notices,
            fallback,
        },
        sink,
    );
    if let Err(e) = result {
        fail_placeholder(&services.db, &started.assistant_message.id, &e);
        return Err(e);
    }
    Ok(started)
}

/// Stores a placeholder whose run never started as failed.
fn fail_placeholder(db: &Db, message_id: &str, e: &Error) {
    let (id, msg) = (message_id.to_owned(), e.to_string());
    db.spawn_job(move |c| {
        repo::finalize_assistant_message(
            c,
            &id,
            &[],
            crate::storage::MessageStatus::Failed,
            None,
            Some(&msg),
        )
    });
}
