//! Several processes on one data root (desktop app + CLI): who runs startup recovery and
//! migrations, schema version checks, and one run per chat across database connections.
//! Each `Db::open_root` here stands in for a process — std file locks are per open file, so
//! two opens in one test process contend like two processes would.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::agent::launcher::{launch, RunParams, RunServices};
use agent_core::agent::{RunCoordinator, RunSink};
use agent_core::agents::ModelRef;
use agent_core::config::{product, resolve_data_root, DataRoot};
use agent_core::domain::{ChatId, ContentBlock, Role, RunEvent, RunId};
use agent_core::mcp::{McpManager, McpStore};
use agent_core::providers::registry::{ProviderStore, SIMULATED_PROVIDER_ID};
use agent_core::secrets::{MemorySecretStore, SecretStore};
use agent_core::settings::SettingsStore;
use agent_core::storage::chats as repo;
use agent_core::storage::{AttachmentStore, Db, MessageStatus, StoredMessage};
use agent_core::Error;
use tokio::sync::Notify;

struct Root {
    root: DataRoot,
    path: PathBuf,
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn root() -> Root {
    let path = std::env::temp_dir().join(format!(
        "{}-shared-{}",
        product().slug,
        uuid::Uuid::new_v4()
    ));
    Root {
        root: resolve_data_root(Some(&path)).unwrap(),
        path,
    }
}

/// A chat whose reply is still `streaming`, as a live run leaves it.
fn streaming_chat(db: &Db) -> (ChatId, String) {
    db.call_blocking(|c| {
        let chat = repo::create_chat(c, "t")?;
        repo::append_message(
            c,
            chat.id,
            Role::User,
            &[ContentBlock::Text { text: "q".into() }],
            &[],
        )?;
        let id = repo::begin_assistant_message(c, chat.id, RunId::new(), "p", "m")?;
        Ok((chat.id, id))
    })
    .unwrap()
}

fn status(db: &Db, chat: ChatId, id: &str) -> MessageStatus {
    let id = id.to_owned();
    db.call_blocking(move |c| {
        Ok(repo::list_messages(c, chat)?
            .into_iter()
            .find(|m| m.id == id)
            .unwrap()
            .status)
    })
    .unwrap()
}

#[test]
fn only_a_process_that_is_alone_recovers_streaming_replies() {
    let r = root();
    // App first, CLI second: the CLI must leave the app's live reply alone.
    let (app, app_lock) = Db::open_root(&r.root).unwrap();
    assert!(app_lock.was_alone());
    let (chat, reply) = streaming_chat(&app);
    let (cli, cli_lock) = Db::open_root(&r.root).unwrap();
    assert!(!cli_lock.was_alone());
    assert_eq!(status(&cli, chat, &reply), MessageStatus::Streaming);

    // The reverse: the second process (a CLI run) streams, the first exits, and a third
    // (the app reopened) starts. It is not alone, so the CLI's reply survives.
    let (cli_chat, cli_reply) = streaming_chat(&cli);
    drop((app, app_lock));
    let (app2, app2_lock) = Db::open_root(&r.root).unwrap();
    assert!(!app2_lock.was_alone());
    assert_eq!(
        status(&app2, cli_chat, &cli_reply),
        MessageStatus::Streaming
    );
    assert_eq!(status(&app2, chat, &reply), MessageStatus::Streaming);

    // Once everyone is gone, the next process is alone and recovers both.
    drop((cli, cli_lock, app2, app2_lock));
    let (next, next_lock) = Db::open_root(&r.root).unwrap();
    assert!(next_lock.was_alone());
    assert_eq!(status(&next, chat, &reply), MessageStatus::Interrupted);
    assert_eq!(
        status(&next, cli_chat, &cli_reply),
        MessageStatus::Interrupted
    );
}

#[test]
fn a_newer_schema_is_refused_by_both_open_paths() {
    let r = root();
    let path = r.root.database_path();
    drop(Db::open(&path).unwrap());
    set_version(&path, 999);
    for (what, result) in [
        ("primary", Db::open(&path).map(|_| ())),
        ("secondary", Db::open_secondary(&path).map(|_| ())),
        ("open_root", Db::open_root(&r.root).map(|_| ())),
    ] {
        let e = result.expect_err(what).to_string();
        assert!(e.contains("newer version"), "{what}: {e}");
    }
    // A process that isn't alone may not migrate: an older schema is refused too.
    set_version(&path, 1);
    let e = Db::open_secondary(&path).map(|_| ()).unwrap_err();
    assert!(e.to_string().contains("older version"), "{e}");
}

/// Stands in for a build with a different schema.
fn set_version(path: &std::path::Path, v: i64) {
    rusqlite::Connection::open(path)
        .unwrap()
        .pragma_update(None, "user_version", v)
        .unwrap();
}

#[derive(Clone, Default)]
struct Done(Arc<(Mutex<usize>, Notify)>);

impl RunSink for Done {
    fn send(&self, event: RunEvent) -> bool {
        if event.payload.is_terminal() {
            *self.0 .0.lock().unwrap() += 1;
            self.0 .1.notify_one();
        }
        true
    }
}

fn services(r: &DataRoot, db: Db) -> RunServices {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let handle = tokio::runtime::Handle::current();
    let settings = SettingsStore::load(r).unwrap();
    settings
        .update(&serde_json::json!({ "general": { "share_environment": false } }))
        .unwrap();
    RunServices {
        db,
        providers: Arc::new(
            ProviderStore::load(r, secrets.clone(), agent_core::http::client().unwrap()).unwrap(),
        ),
        attachments: Arc::new(AttachmentStore::new(r.attachments_dir())),
        mcp: McpManager::new(
            McpStore::load(r, secrets).unwrap(),
            agent_core::http::client().unwrap(),
            handle.clone(),
            || {},
        ),
        coordinator: RunCoordinator::new(4, handle),
        settings: Arc::new(settings),
    }
}

fn params(chat_id: ChatId, prompt: &str) -> RunParams {
    RunParams {
        chat_id,
        model: Some(ModelRef {
            provider_id: SIMULATED_PROVIDER_ID.into(),
            model: "simulated-1".into(),
        }),
        prompt: prompt.into(),
        attachment_ids: Vec::new(),
        replace_from: None,
        unattended: false,
    }
}

async fn finish(db: &Db, chat: ChatId) -> Vec<StoredMessage> {
    for _ in 0..300 {
        let m = db
            .call(move |c| repo::list_messages(c, chat))
            .await
            .unwrap();
        if m.iter().all(|m| m.status != MessageStatus::Streaming) {
            return m;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("run did not finish");
}

/// Two launches for one chat yield exactly one run and leave history intact — in one
/// process (racing past the coordinator's in-memory check) and from two processes.
#[tokio::test(flavor = "multi_thread")]
async fn two_launches_for_one_chat_start_one_run() {
    let r = root();
    let (db_a, _lock_a) = Db::open_root(&r.root).unwrap();
    let (db_b, _lock_b) = Db::open_root(&r.root).unwrap();
    let (a, b) = (services(&r.root, db_a), services(&r.root, db_b.clone()));

    for (case, other) in [("one process", &a), ("two processes", &b)] {
        let chat = a.db.call(|c| repo::create_chat(c, "t")).await.unwrap().id;
        let (sa, sb) = (Done::default(), Done::default());
        let (first, second) = tokio::join!(
            launch(&a, params(chat, "first"), sa.clone(), |_| {}),
            launch(other, params(chat, "second"), sb.clone(), |_| {}),
        );
        let results = [first.map(|_| ()), second.map(|_| ())];
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{case}: {results:?}"
        );
        assert!(
            results.iter().any(|r| matches!(r, Err(Error::RunBusy))),
            "{case}: {results:?}"
        );
        let stored = finish(&a.db, chat).await;
        assert_eq!(stored.len(), 2, "{case}: one user turn and one reply");
        assert_eq!(stored[0].role, Role::User);
        assert_eq!(stored[1].status, MessageStatus::Complete, "{case}");

        // The finished chat accepts the next turn from either side; history stays intact.
        let s = Done::default();
        launch(other, params(chat, "third"), s.clone(), |_| {})
            .await
            .unwrap();
        let stored = finish(&db_b, chat).await;
        assert_eq!(stored.len(), 4, "{case}");
        assert!(stored.iter().all(|m| m.status == MessageStatus::Complete));
    }
}
