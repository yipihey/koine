//! Consent decisions (docs/plan-koine-trust.md TR-D6, TR-D7, TR-D9) over the
//! review loop: the consent policy holds an agent's destructive call, the
//! person approves a subset of its targets or an alternative verb instead,
//! and `remember` writes a standing rule that lets (or refuses) the next
//! call without asking — until it is revoked.

use std::sync::{Arc, Mutex, OnceLock};

use impress_item::store::AttributedStore;
use impress_service_core::call;
use impress_service_core::pipeline::policy::{self, ConsentPolicy};
use impress_service_core::pipeline::CallerIdentity;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What the test verbs did, in order.
fn done() -> &'static Mutex<Vec<String>> {
    static DONE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    DONE.get_or_init(Default::default)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Did {
    pub ok: bool,
}

#[impress_service]
pub trait NoteService: Send + Sync + 'static {
    /// Delete notes by id.
    #[impress_method(
        safety = destructive,
        targets = "ids",
        alternatives = ["note-service_archive"]
    )]
    async fn delete(&self, ids: Vec<String>) -> Did;

    /// Archive notes by id.
    #[impress_method(safety = mutating)]
    async fn archive(&self, ids: Vec<String>) -> Did;

    /// Read the person's notes.
    #[impress_method(risk(private))]
    async fn peek(&self) -> Did;

    /// Read a web page.
    #[impress_method(risk(untrusted))]
    async fn browse(&self) -> Did;

    /// Send a note out.
    #[impress_method(safety = external, impact = "low", effects(reach = [network]))]
    async fn send(&self) -> Did;

    /// Touch a note.
    #[impress_method(safety = mutating, impact = "low")]
    async fn touch(&self) -> Did;
}

pub struct Notes;

#[async_trait::async_trait]
impl NoteService for Notes {
    async fn delete(&self, ids: Vec<String>) -> Did {
        done()
            .lock()
            .unwrap()
            .push(format!("delete {}", ids.join(",")));
        Did { ok: true }
    }
    async fn archive(&self, ids: Vec<String>) -> Did {
        done()
            .lock()
            .unwrap()
            .push(format!("archive {}", ids.join(",")));
        Did { ok: true }
    }
    async fn peek(&self) -> Did {
        done().lock().unwrap().push("peek".into());
        Did { ok: true }
    }
    async fn browse(&self) -> Did {
        done().lock().unwrap().push("browse".into());
        Did { ok: true }
    }
    async fn send(&self) -> Did {
        done().lock().unwrap().push("send".into());
        Did { ok: true }
    }
    async fn touch(&self) -> Did {
        done().lock().unwrap().push("touch".into());
        Did { ok: true }
    }
}

impress_service_impl! {
    service = NoteService,
    safety = read_only,
    since = "0.1.0",
    effects = {
        reads: [],
        writes: [],
        reach: [],
    },
    impl = Notes,
    instance = || Notes,
    methods = [
        /// Delete notes.
        delete(
            /// The notes.
            ids: Vec<String>
        ) -> Did,
        /// Archive notes.
        archive(
            /// The notes.
            ids: Vec<String>
        ) -> Did,
        /// Read the person's notes.
        peek() -> Did,
        /// Read a web page.
        browse() -> Did,
        /// Send a note out.
        send() -> Did,
        /// Touch a note.
        touch() -> Did,
    ],
}

fn process_store() -> Arc<dyn AttributedStore> {
    static STORE: OnceLock<Arc<dyn AttributedStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            koine_tasks::ensure_linked();
            let store: Arc<dyn AttributedStore> = Arc::new(koine_store_memory::MemoryStore::new());
            koine_store::process::install(store.clone());
            policy::install(Arc::new(ConsentPolicy::DEFAULT));
            koine_tasks::review::install_queue();
            store
        })
        .clone()
}

fn serial() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn as_caller(caller: CallerIdentity, name: &str, args: Value) -> Value {
    process_store();
    call::call_async_as(name, caller, args)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn agent() -> CallerIdentity {
    CallerIdentity::agent("claude")
}

fn person() -> CallerIdentity {
    CallerIdentity::Person
}

fn take_done() -> Vec<String> {
    std::mem::take(&mut *done().lock().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_approves_a_subset_or_an_alternative_and_sees_why() {
    let _one = serial().lock().await;
    take_done();
    let held = as_caller(
        agent(),
        "note-service_delete",
        json!({"ids": ["a", "b", "c"]}),
    )
    .await;
    assert_eq!(held["status"], "input_required", "{held}");
    assert!(
        held["reason"].as_str().unwrap().contains("high impact"),
        "{held}"
    );
    let pending = held["pending_id"].as_str().unwrap().to_string();

    let shown = as_caller(person(), "review-service_show", json!({"id": pending})).await;
    let row = &shown["pending"];
    assert_eq!(row["impact"], "high");
    assert_eq!(row["targets"].as_array().unwrap().len(), 3, "{shown}");
    assert_eq!(row["alternatives"], json!(["note-service_archive"]));
    assert!(
        row["question"].as_str().unwrap().contains("on 3 records"),
        "{shown}"
    );

    let stray = as_caller(
        person(),
        "review-service_approve",
        json!({"id": pending, "only": ["z"]}),
    )
    .await;
    assert_eq!(stray["code"], "invalid-argument", "{stray}");
    let approved = as_caller(
        person(),
        "review-service_approve",
        json!({"id": pending, "only": ["a", "c"]}),
    )
    .await;
    assert_eq!(approved["pending"]["state"], "done", "{approved}");
    assert_eq!(take_done(), ["delete a,c"], "only the approved subset");

    let held = as_caller(agent(), "note-service_delete", json!({"ids": ["d"]})).await;
    let pending = held["pending_id"].as_str().unwrap().to_string();
    let wrong = as_caller(
        person(),
        "review-service_approve",
        json!({"id": pending, "instead": "note-service_nope"}),
    )
    .await;
    assert_eq!(wrong["code"], "invalid-argument", "{wrong}");
    let instead = as_caller(
        person(),
        "review-service_approve",
        json!({"id": pending, "instead": "note-service_archive"}),
    )
    .await;
    assert_eq!(instead["pending"]["state"], "done", "{instead}");
    assert_eq!(take_done(), ["archive d"], "the alternative ran instead");
}

#[tokio::test(flavor = "multi_thread")]
async fn remember_makes_a_rule_and_revoking_it_asks_again() {
    let _one = serial().lock().await;
    take_done();
    let held = as_caller(agent(), "note-service_archive", json!({"ids": ["x"]})).await;
    assert_eq!(held["status"], "input_required", "{held}");
    let pending = held["pending_id"].as_str().unwrap().to_string();
    as_caller(
        person(),
        "review-service_approve",
        json!({"id": pending, "remember": true}),
    )
    .await;
    assert_eq!(take_done(), ["archive x"]);

    // The next call from the same agent runs without asking.
    let ran = as_caller(agent(), "note-service_archive", json!({"ids": ["y"]})).await;
    assert_eq!(ran["ok"], true, "{ran}");
    assert_eq!(take_done(), ["archive y"]);
    // Another agent is still asked.
    let other = as_caller(
        CallerIdentity::agent("codex"),
        "note-service_archive",
        json!({"ids": ["w"]}),
    )
    .await;
    assert_eq!(other["status"], "input_required", "{other}");

    let rules = as_caller(person(), "review-service_rules", json!({})).await;
    let rule = rules["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["verb"] == "note-service_archive" && r["revoked"] == false)
        .cloned()
        .unwrap_or_else(|| panic!("{rules}"));
    assert_eq!(rule["decision"], "allow");
    assert_eq!(rule["caller"], "agent:claude");
    assert_eq!(rule["stale"], false);
    let refused = as_caller(
        agent(),
        "review-service_revoke-rule",
        json!({"id": rule["id"]}),
    )
    .await;
    assert_eq!(refused["code"], "forbidden", "{refused}");
    as_caller(
        person(),
        "review-service_revoke-rule",
        json!({"id": rule["id"]}),
    )
    .await;
    let asked = as_caller(agent(), "note-service_archive", json!({"ids": ["z"]})).await;
    assert_eq!(asked["status"], "input_required", "asked again: {asked}");
    assert!(take_done().is_empty());

    // Remembering a rejection refuses the next call outright.
    let pending = asked["pending_id"].as_str().unwrap().to_string();
    as_caller(
        person(),
        "review-service_reject",
        json!({"id": pending, "remember": true}),
    )
    .await;
    let rejected = as_caller(agent(), "note-service_archive", json!({"ids": ["q"]})).await;
    assert_eq!(rejected["code"], "rejected", "{rejected}");
    assert_eq!(rejected["status"], "rejected");
    assert!(take_done().is_empty());
    // Leave no standing rule behind for the other tests.
    let rules = as_caller(person(), "review-service_rules", json!({})).await;
    for rule in rules["rows"].as_array().unwrap() {
        if rule["revoked"] == false {
            as_caller(
                person(),
                "review-service_revoke-rule",
                json!({"id": rule["id"]}),
            )
            .await;
        }
    }
}

async fn in_session(session: &str, caller: CallerIdentity, name: &str, args: Value) -> Value {
    impress_service_core::pipeline::context::session_scope(
        Arc::new(session.to_string()),
        as_caller(caller, name, args),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_holding_two_risks_is_asked_before_the_third() {
    let _one = serial().lock().await;
    take_done();
    let none = || json!({});
    // Private data and untrusted content: two of three, nothing is held.
    assert_eq!(
        in_session("s1", agent(), "note-service_peek", none()).await["ok"],
        true
    );
    assert_eq!(
        in_session("s1", agent(), "note-service_browse", none()).await["ok"],
        true
    );
    // Sending out would make three: a person decides.
    let held = in_session("s1", agent(), "note-service_send", none()).await;
    assert_eq!(held["status"], "input_required", "{held}");
    assert!(
        held["reason"].as_str().unwrap().contains("sent data out"),
        "{held}"
    );
    assert_eq!(take_done(), ["peek", "browse"], "the send did not run");
    // Another session that only read private data may send.
    assert_eq!(
        in_session("s2", agent(), "note-service_peek", none()).await["ok"],
        true
    );
    let other = in_session("s2", agent(), "note-service_send", none()).await;
    assert_eq!(other["ok"], true, "{other}");
    // The person is never held for it.
    for verb in [
        "note-service_peek",
        "note-service_browse",
        "note-service_send",
    ] {
        assert_eq!(in_session("s3", person(), verb, none()).await["ok"], true);
    }
    // The person approves the held send; it runs in the session, which now
    // holds all three: a read is fine, a write is held.
    as_caller(
        person(),
        "review-service_approve",
        json!({"id": held["pending_id"]}),
    )
    .await;
    take_done();
    let read = in_session("s1", agent(), "note-service_peek", none()).await;
    assert_eq!(read["ok"], true, "{read}");
    let write = in_session("s1", agent(), "note-service_touch", none()).await;
    assert_eq!(write["status"], "input_required", "{write}");
    assert!(
        write["reason"].as_str().unwrap().contains("already"),
        "{write}"
    );
}
