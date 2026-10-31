//! Opt-in: exercises ProviderStore -> adapter -> RunCoordinator against a running
//! OpenAI-compatible server (e.g. `node scripts/dev/mock-openai-server.mjs`).
//! Set OPENGENT_TEST_OPENAI_BASE_URL (and optionally OPENGENT_TEST_OPENAI_MODEL) and run with --ignored.

use std::sync::Arc;

use agent_core::agent::{context, RunCoordinator, RunRequest};
use agent_core::config::resolve_data_root;
use agent_core::domain::{Message, Role, RunEventPayload};
use agent_core::providers::capabilities::ImageSupport;
use agent_core::providers::registry::{ProviderInput, ProviderKind, ProviderStore, TlsMode};
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
