//! Drives `agent::launcher::launch` with no desktop host in the process: a temp data root,
//! an in-memory secret store, the simulated provider and a collecting sink.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::agent::launcher::{launch, RunParams, RunServices, RunStarted, MAX_PROMPT_CHARS};
use agent_core::agent::{RunCoordinator, RunSink};
use agent_core::agents::{AgentInput, ModelRef};
use agent_core::config::{product, resolve_data_root};
use agent_core::domain::{ChatId, ContentBlock, Role, RunEvent, RunEventPayload};
use agent_core::mcp::{McpManager, McpStore};
use agent_core::providers::capabilities::ImageSupport;
use agent_core::providers::registry::{
    ProviderInput, ProviderKind, ProviderStore, TlsMode, SIMULATED_PROVIDER_ID,
};
use agent_core::secrets::{MemorySecretStore, SecretStore};
use agent_core::settings::SettingsStore;
use agent_core::storage::agents as agents_repo;
use agent_core::storage::chats as repo;
use agent_core::storage::{AttachmentStore, Db, MessageStatus, StoredMessage};
use agent_core::Error;
use tokio::sync::Notify;

const SIMULATED_MODEL: &str = "simulated-1";

struct Harness {
    services: RunServices,
    root: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn harness() -> Harness {
    let root = std::env::temp_dir().join(format!(
        "{}-launcher-{}",
        product().slug,
        uuid::Uuid::new_v4()
    ));
    let data_root = resolve_data_root(Some(&root)).unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let runtime = tokio::runtime::Handle::current();
    let settings = SettingsStore::load(&data_root).unwrap();
    // Keeps the compiled request independent of the machine running the test.
    settings
        .update(&serde_json::json!({ "general": { "share_environment": false } }))
        .unwrap();
    let services = RunServices {
        db: Db::open(&data_root.database_path()).unwrap(),
        providers: Arc::new(
            ProviderStore::load(
                &data_root,
                secrets.clone(),
                agent_core::http::client().unwrap(),
            )
            .unwrap(),
        ),
        attachments: Arc::new(AttachmentStore::new(data_root.attachments_dir())),
        mcp: McpManager::new(
            McpStore::load(&data_root, secrets).unwrap(),
            agent_core::http::client().unwrap(),
            runtime.clone(),
            || {},
        ),
        coordinator: RunCoordinator::new(4, runtime),
        settings: Arc::new(settings),
    };
    Harness { services, root }
}

#[derive(Debug)]
enum Entry {
    Prepared(Box<RunStarted>),
    Event(RunEventPayload),
}

/// Records `on_prepared` and every event in arrival order.
#[derive(Clone, Default)]
struct Collector {
    log: Arc<Mutex<Vec<Entry>>>,
    done: Arc<Notify>,
}

impl RunSink for Collector {
    fn send(&self, event: RunEvent) -> bool {
        let terminal = event.payload.is_terminal();
        self.log.lock().unwrap().push(Entry::Event(event.payload));
        if terminal {
            self.done.notify_one();
        }
        true
    }
}

impl Collector {
    fn on_prepared(&self) -> impl FnOnce(&RunStarted) + Send + 'static {
        let log = self.log.clone();
        move |started| {
            log.lock()
                .unwrap()
                .push(Entry::Prepared(Box::new(started.clone())))
        }
    }

    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(30), self.done.notified())
            .await
            .expect("run did not finish");
    }
}

fn simulated() -> Option<ModelRef> {
    Some(ModelRef {
        provider_id: SIMULATED_PROVIDER_ID.into(),
        model: SIMULATED_MODEL.into(),
    })
}

fn params(chat_id: ChatId, model: Option<ModelRef>, prompt: &str) -> RunParams {
    RunParams {
        chat_id,
        model,
        prompt: prompt.into(),
        attachment_ids: Vec::new(),
        replace_from: None,
        unattended: false,
    }
}

async fn messages(db: &Db, chat_id: ChatId) -> Vec<StoredMessage> {
    db.call(move |c| repo::list_messages(c, chat_id))
        .await
        .unwrap()
}

fn text_of(m: &StoredMessage) -> String {
    m.blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn run_is_prepared_before_streaming_and_recorded_complete() {
    let h = harness();
    let db = h.services.db.clone();
    let chat = db.call(|c| repo::create_chat(c, "t")).await.unwrap();
    let sink = Collector::default();

    let started = launch(
        &h.services,
        params(chat.id, simulated(), "  hello launcher  "),
        sink.clone(),
        sink.on_prepared(),
    )
    .await
    .unwrap();
    sink.wait().await;

    {
        let log = sink.log.lock().unwrap();
        match log.first() {
            Some(Entry::Prepared(p)) => assert_eq!(p.run_id, started.run_id),
            other => panic!("on_prepared must run before the first event, got {other:?}"),
        }
        assert!(log[1..].iter().all(|e| matches!(e, Entry::Event(_))));
        assert!(matches!(
            log.last(),
            Some(Entry::Event(RunEventPayload::Completed { .. }))
        ));
    }
    assert_eq!(started.assistant_message.status, MessageStatus::Streaming);
    assert_eq!(started.assistant_message.run_id, Some(started.run_id));
    assert_eq!(started.chat.id, chat.id);

    // The final write is queued on the database worker before the terminal event, so a
    // read submitted after it sees the finished reply.
    let stored = messages(&db, chat.id).await;
    assert_eq!(stored.len(), 2);
    let (user, assistant) = (&stored[0], &stored[1]);
    assert_eq!(user.id, started.user_message.id);
    assert_eq!(user.role, Role::User);
    assert_eq!(text_of(user), "hello launcher");
    assert_eq!(assistant.id, started.assistant_message.id);
    assert_eq!(assistant.status, MessageStatus::Complete);
    assert!(text_of(assistant).contains("hello launcher"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_turns_leave_history_untouched() {
    let h = harness();
    let db = h.services.db.clone();
    let chat = db.call(|c| repo::create_chat(c, "t")).await.unwrap();
    let chat_id = chat.id;
    let first = db
        .call(move |c| {
            let user = repo::append_message(
                c,
                chat_id,
                Role::User,
                &[ContentBlock::Text { text: "q".into() }],
                &[],
            )?;
            repo::append_message(
                c,
                chat_id,
                Role::Assistant,
                &[ContentBlock::Text { text: "a".into() }],
                &[],
            )?;
            Ok(user)
        })
        .await
        .unwrap();
    let before = messages(&db, chat_id).await;

    // A text-only model; never contacted, since the attachment gate runs first.
    let text_only = h
        .services
        .providers
        .save(ProviderInput {
            id: None,
            name: "Text only".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "http://127.0.0.1:9/v1".into(),
            default_model: Some("text-model".into()),
            api_key: Some("unused".into()),
            include_usage: true,
            context_window: None,
            model_context_windows: HashMap::new(),
            tls_mode: TlsMode::System,
            tls_ca_pem: None,
            image_support: ImageSupport::Never,
            hidden_models: Vec::new(),
        })
        .unwrap();
    let image_path = h.root.join("pixel.png");
    std::fs::write(&image_path, b"\x89PNG\r\n\x1a\n\0\0\0\0").unwrap();
    let store = h.services.attachments.clone();
    let prepared = store.prepare(store.load(&image_path).unwrap()).unwrap();
    let image = db.call(move |c| store.commit(c, prepared)).await.unwrap();

    // Every case asks to retry from the first turn, so a truncation would show.
    let retry = |model: Option<ModelRef>, prompt: &str, attachments: Vec<String>| RunParams {
        attachment_ids: attachments,
        replace_from: Some(first.clone()),
        ..params(chat_id, model, prompt)
    };
    let cases = [
        ("empty prompt", retry(simulated(), "   ", Vec::new())),
        (
            "over-limit prompt",
            retry(simulated(), &"x".repeat(MAX_PROMPT_CHARS + 1), Vec::new()),
        ),
        (
            "rejected attachment",
            retry(
                Some(ModelRef {
                    provider_id: text_only.id.clone(),
                    model: "text-model".into(),
                }),
                "look",
                vec![image.id.clone()],
            ),
        ),
    ];
    for (case, p) in cases {
        let sink = Collector::default();
        let err = launch(&h.services, p, sink.clone(), sink.on_prepared())
            .await
            .expect_err(case);
        assert!(matches!(err, Error::InvalidInput(_)), "{case}: {err:?}");
        if case == "rejected attachment" {
            assert!(err.to_string().contains("does not accept image"), "{err}");
        }
        assert!(sink.log.lock().unwrap().is_empty(), "{case}");
        assert_eq!(messages(&db, chat_id).await, before, "{case}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_chat_ignores_caller_model() {
    let h = harness();
    let db = h.services.db.clone();
    let chat = db
        .call(|c| {
            let agent = agents_repo::save_agent(
                c,
                AgentInput {
                    id: None,
                    name: "Echo".into(),
                    description: String::new(),
                    default_model: ModelRef {
                        provider_id: SIMULATED_PROVIDER_ID.into(),
                        model: SIMULATED_MODEL.into(),
                    },
                    fallback_model: None,
                    system_prompt: String::new(),
                    servers: Vec::new(),
                },
            )?;
            repo::create_chat_with(c, "t", Some(agent.id), None)
        })
        .await
        .unwrap();
    let sink = Collector::default();
    let caller_choice = Some(ModelRef {
        provider_id: "no-such-provider".into(),
        model: "no-such-model".into(),
    });

    let started = launch(
        &h.services,
        params(chat.id, caller_choice, "hi"),
        sink.clone(),
        sink.on_prepared(),
    )
    .await
    .unwrap();
    sink.wait().await;

    assert_eq!(
        started.assistant_message.model.as_deref(),
        Some(SIMULATED_MODEL)
    );
    assert_eq!(
        started.chat.provider_id.as_deref(),
        Some(SIMULATED_PROVIDER_ID)
    );
    let stored = messages(&db, chat.id).await;
    assert_eq!(stored[1].status, MessageStatus::Complete);
    assert!(text_of(&stored[1]).contains(SIMULATED_MODEL));
}
