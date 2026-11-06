//! Opt-in: exercises ProviderStore -> adapter -> RunCoordinator against a running
//! OpenAI-compatible server (e.g. `node scripts/dev/mock-openai-server.mjs`).
//! Set OPENGENT_TEST_OPENAI_BASE_URL (and optionally OPENGENT_TEST_OPENAI_MODEL) and run with --ignored.
//! The Gemini test needs OPENGENT_TEST_GEMINI_API_KEY (Google AI Studio); the OpenAI
//! (Responses API) test needs OPENGENT_TEST_OPENAI_API_KEY.

use std::sync::Arc;

use agent_core::agent::{context, RunCoordinator, RunRequest};
use agent_core::config::resolve_data_root;
use agent_core::domain::{ContentBlock, Message, Role, RunEventPayload, StopReason};
use agent_core::providers::capabilities::ImageSupport;
use agent_core::providers::registry::{ProviderInput, ProviderKind, ProviderStore, TlsMode};
use agent_core::providers::{CompletionRequest, ProviderEvent, ToolSpec};
use agent_core::secrets::MemorySecretStore;
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "requires a live OpenAI-compatible server"]
async fn store_builds_provider_and_run_completes() {
    let Ok(base_url) = std::env::var("OPENGENT_TEST_OPENAI_BASE_URL") else {
        eprintln!("OPENGENT_TEST_OPENAI_BASE_URL not set; skipping");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("opengent-live-{}", uuid::Uuid::new_v4()));
    let root = resolve_data_root(Some(&tmp)).unwrap();
    let store = ProviderStore::load(
        &root,
        Arc::new(MemorySecretStore::default()),
        agent_core::http::client().unwrap(),
    )
    .unwrap();
    let saved = store
        .save(ProviderInput {
            id: None,
            name: "live".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url,
            default_model: std::env::var("OPENGENT_TEST_OPENAI_MODEL").ok(),
            api_key: std::env::var("OPENGENT_TEST_OPENAI_KEY").ok(),
            include_usage: true,
            context_window: None,
            model_context_windows: std::collections::HashMap::new(),
            tls_mode: TlsMode::System,
            tls_ca_pem: None,
            image_support: ImageSupport::Auto,
            hidden_models: Vec::new(),
        })
        .unwrap();

    let models = store.discover_models(&saved.id).await.unwrap();
    let model = saved
        .default_model
        .clone()
        .or_else(|| models.first().cloned())
        .expect("a model");
    eprintln!("models: {models:?}; using {model}");

    let provider = store.build(&saved.id).unwrap();
    let history = vec![Message::text(
        Role::User,
        "Say hello in one short sentence.",
    )];
    let compiled = context::compile(
        &model,
        &history,
        None,
        &Default::default(),
        false,
        &Default::default(),
        &Default::default(),
    );
    let coord = RunCoordinator::new(1, tokio::runtime::Handle::current());
    let (tx, mut rx) = mpsc::unbounded_channel();
    coord
        .start(
            RunRequest {
                chat_id: Default::default(),
                provider,
                completion: compiled.request,
                tools: None,
                notices: vec![],
                fallback: None,
            },
            tx,
        )
        .unwrap();

    let mut text = String::new();
    let mut last = None;
    while let Some(ev) = rx.recv().await {
        if let RunEventPayload::TextDelta { text: t } = &ev.payload {
            text.push_str(t);
        }
        last = Some(ev.payload);
    }
    eprintln!("reply: {text}");
    assert!(
        matches!(last, Some(RunEventPayload::Completed { .. })),
        "last event: {last:?}"
    );
    assert!(!text.trim().is_empty());
    std::fs::remove_dir_all(tmp).ok();
}

/// Drains one stream into the blocks the coordinator would store, in stream order.
async fn stream_blocks(
    provider: &dyn agent_core::providers::ModelProvider,
    request: &agent_core::providers::CompletionRequest,
) -> (Vec<ContentBlock>, Option<StopReason>) {
    let mut rx = provider
        .stream(request, tokio_util::sync::CancellationToken::new())
        .unwrap();
    let (mut blocks, mut done) = (Vec::new(), None);
    while let Some(ev) = rx.recv().await {
        match ev {
            ProviderEvent::TextDelta(t) => match blocks.last_mut() {
                Some(ContentBlock::Text { text }) => text.push_str(&t),
                _ => blocks.push(ContentBlock::Text { text: t }),
            },
            ProviderEvent::Opaque { provider, data } => blocks.push(ContentBlock::ProviderOpaque {
                provider: provider.into(),
                data,
            }),
            ProviderEvent::ToolCall {
                id,
                name,
                arguments,
            } => blocks.push(ContentBlock::ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&arguments).unwrap_or_default(),
                server: None,
                tool: None,
            }),
            ProviderEvent::Done(r) => done = Some(r),
            ProviderEvent::Error(e) => panic!("provider error: {e}"),
            _ => {}
        }
    }
    (blocks, done)
}

/// `OPENGENT_TEST_GEMINI_API_KEY=… cargo test -p agent-core -- --ignored gemini`
/// (optional `OPENGENT_TEST_GEMINI_MODEL`, default `gemini-flash-latest`, and
/// `OPENGENT_TEST_GEMINI_BASE_URL`, e.g. `http://localhost:8789/v1` for the mock server).
#[tokio::test]
#[ignore = "requires a Google AI Studio API key"]
async fn gemini_lists_models_and_completes_a_tool_round_trip() {
    let Ok(key) = std::env::var("OPENGENT_TEST_GEMINI_API_KEY") else {
        eprintln!("OPENGENT_TEST_GEMINI_API_KEY not set; skipping");
        return;
    };
    tool_round_trip(
        ProviderKind::GoogleGemini,
        key,
        std::env::var("OPENGENT_TEST_GEMINI_BASE_URL").unwrap_or_default(),
        std::env::var("OPENGENT_TEST_GEMINI_MODEL")
            .unwrap_or_else(|_| "gemini-flash-latest".into()),
    )
    .await;
}

/// `OPENGENT_TEST_OPENAI_API_KEY=… cargo test -p agent-core -- --ignored openai_lists`
/// (optional `OPENGENT_TEST_OPENAI_MODEL`, default `gpt-5-mini`, and
/// `OPENGENT_TEST_OPENAI_RESPONSES_BASE_URL`, e.g. `http://localhost:8787/v1` for
/// `mock-openai-server.mjs` with model `mock-reasoning`).
#[tokio::test]
#[ignore = "requires an OpenAI API key"]
async fn openai_lists_models_and_completes_a_tool_round_trip() {
    let Ok(key) = std::env::var("OPENGENT_TEST_OPENAI_API_KEY") else {
        eprintln!("OPENGENT_TEST_OPENAI_API_KEY not set; skipping");
        return;
    };
    tool_round_trip(
        ProviderKind::OpenAi,
        key,
        std::env::var("OPENGENT_TEST_OPENAI_RESPONSES_BASE_URL").unwrap_or_default(),
        std::env::var("OPENGENT_TEST_OPENAI_MODEL").unwrap_or_else(|_| "gpt-5-mini".into()),
    )
    .await;
}

/// Store → discovery → a tool call → replay of the stored turn (with its provider state)
/// → a final text answer. An empty `base_url` uses the kind's default endpoint.
async fn tool_round_trip(kind: ProviderKind, key: String, base_url: String, model: String) {
    let tmp = std::env::temp_dir().join(format!("opengent-live-{}", uuid::Uuid::new_v4()));
    let root = resolve_data_root(Some(&tmp)).unwrap();
    let store = ProviderStore::load(
        &root,
        Arc::new(MemorySecretStore::default()),
        agent_core::http::client().unwrap(),
    )
    .unwrap();
    let saved = store
        .save(ProviderInput {
            id: None,
            name: kind.label().into(),
            kind,
            base_url,
            default_model: None,
            api_key: Some(key),
            include_usage: false,
            context_window: None,
            model_context_windows: std::collections::HashMap::new(),
            tls_mode: TlsMode::System,
            tls_ca_pem: None,
            image_support: ImageSupport::Auto,
            hidden_models: Vec::new(),
        })
        .unwrap();
    let models = store.discover_models(&saved.id).await.unwrap();
    eprintln!("models: {models:?}");
    assert!(!models.is_empty());

    let provider = store.build(&saved.id).unwrap();
    let tools = vec![ToolSpec {
        name: "weather__get".into(),
        description: "Current weather for a city.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }];
    let mut history = vec![Message::text(
        Role::User,
        "What's the weather in Paris? Please use tool weather__get.",
    )];
    let request = |messages: Vec<Message>| CompletionRequest {
        model: model.clone(),
        system: Some("You are a helpful assistant.".into()),
        messages,
        max_output_tokens: None,
        tools: tools.clone(),
    };
    let (mut blocks, done) = stream_blocks(provider.as_ref(), &request(history.clone())).await;
    assert_eq!(done, Some(StopReason::ToolUse), "blocks: {blocks:?}");
    let ids: Vec<String> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolCall { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    for id in ids {
        blocks.push(ContentBlock::ToolResult {
            call_id: id,
            content: serde_json::json!([{ "type": "text", "text": "{\"temp_c\":18,\"sky\":\"clear\"}" }]),
            is_error: false,
            status: None,
        });
    }
    let stored = Message {
        role: Role::Assistant,
        blocks,
    };
    history.extend(context::render_turn(&stored, &Default::default(), false));
    let (reply, done) = stream_blocks(provider.as_ref(), &request(history)).await;
    eprintln!("reply: {reply:?}");
    assert_eq!(done, Some(StopReason::EndTurn));
    assert!(reply
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if !text.trim().is_empty())));
    std::fs::remove_dir_all(tmp).ok();
}
