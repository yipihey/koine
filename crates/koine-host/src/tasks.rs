//! A host runs tasks (docs/plan-koine-ai-tasks.md Q-D5) and, with AI, the
//! desk agent (AI-D5).
//!
//! [`start`] runs `koine-tasks`' scheduler over the process store: a pass
//! every `poll`, after `start_delay`. It registers the verb executor (Q-D3),
//! so `task-service_enqueue` work runs here as `agent:tasks`. With AI it
//! also registers the model executors (a queued conversation turn, a title
//! suggestion) over the same store and `<workspace>/ai/`. It then runs the
//! desk agent: open requests on the agent desk become conversations with the
//! linked inventory as tools (`koine-verbs`), and their replies become the
//! answers.
//!
//! The scheduler's actor (`<app_id>:tasks`) is what it stamps as
//! `assigned_to`, so a restarted host resumes its own running tasks. One
//! host per store runs a scheduler. Two would resume each other's tasks.

use std::sync::Arc;
use std::time::Duration;

use impress_item::AttributedStore;
use koine_ai::{AiRegistry, AiStore, FileBlobStore, InventoryTools, ResolveTarget};
use koine_tasks::{ItemTaskStore, PassReport, Scheduler, SchedulerConfig, VerbTaskExecutor};
use tokio::task::JoinHandle;

/// The agent verb tasks run as.
pub const VERB_AGENT: &str = "tasks";
/// The agent the desk bridge and the model's tool calls run as.
pub const AI_AGENT: &str = "koine-ai";
/// How often the desk is read.
pub const DESK_POLL: Duration = Duration::from_secs(3);

/// What [`start`] runs.
#[derive(Clone, Debug)]
pub struct TaskRuntime {
    pub app_id: String,
    pub workspace: std::path::PathBuf,
    pub poll: Duration,
    pub start_delay: Duration,
    pub ai: bool,
}

/// The background loops a host stops on shutdown.
pub struct Running {
    handles: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn stop(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

fn quiet(report: &PassReport) -> bool {
    *report == PassReport::default()
}

/// Start the scheduler (and with `runtime.ai` the model executors and the
/// desk agent) over `store`.
pub fn start(runtime: TaskRuntime, store: Arc<dyn AttributedStore>) -> Running {
    let mut scheduler = Scheduler::new(
        Arc::new(ItemTaskStore::new(store.clone())),
        SchedulerConfig {
            actor: format!("{}:tasks", runtime.app_id),
            start_delay: runtime.start_delay,
            poll_interval: runtime.poll,
            ..SchedulerConfig::default()
        },
    );
    scheduler.register(Arc::new(VerbTaskExecutor::new(VERB_AGENT)));
    let mut handles = Vec::new();

    if runtime.ai {
        let ai = Arc::new(AiStore::from_port(
            store.clone(),
            format!("agent:{}", AI_AGENT),
        ));
        let registry = AiRegistry::for_daemon(runtime.workspace.clone());
        #[cfg(feature = "apple-fm")]
        {
            let ready = koine_ai_apple::install(&registry);
            tracing::info!(target: "koine-host",
                "Apple on-device model: {}",
                if ready { "available" } else { "not available" });
        }
        let blob_root = runtime.workspace.join("ai").join("blobs");
        match FileBlobStore::open(&blob_root) {
            Ok(blobs) => {
                let tools = Arc::new(
                    InventoryTools::new(AI_AGENT)
                        // The model does not configure its own provider.
                        .excluding(["impress-ai-service"]),
                );
                scheduler.register(Arc::new(
                    koine_ai::AiTaskExecutor::with_registry(
                        ai.clone(),
                        registry.clone(),
                        Arc::new(blobs),
                    )
                    .with_tool_adapter(tools),
                ));
                scheduler.register(Arc::new(koine_ai::AiTitleTaskExecutor::with_registry(
                    ai.clone(),
                    registry.clone(),
                )));
                handles.push(tokio::spawn(desk_loop(ai, registry)));
            }
            Err(error) => tracing::warn!(target: "koine-host",
                "AI off: the blob store {} did not open: {error}", blob_root.display()),
        }
    }

    let poll = runtime.poll;
    let delay = runtime.start_delay;
    handles.push(tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        loop {
            match scheduler.run_once().await {
                Ok(report) if !quiet(&report) => {
                    tracing::info!(target: "koine-host", "tasks: {report:?}")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(target: "koine-host", "tasks: {error}"),
            }
            tokio::time::sleep(poll).await;
        }
    }));
    Running { handles }
}

/// The desk agent: each pass resolves the device's model (so a choice made
/// in settings is the one used next), then answers what it can.
async fn desk_loop(ai: Arc<AiStore>, registry: Arc<AiRegistry>) {
    let mut said_off = false;
    loop {
        match registry.resolve(&ResolveTarget::default()) {
            Ok(resolved) => {
                said_off = false;
                let bridge =
                    koine_ai::DeskBridge::new(ai.clone(), resolved.provider, resolved.model)
                        .as_agent(AI_AGENT);
                match bridge.pass().await {
                    Ok(pass) if pass != koine_ai::desk::Pass::default() => {
                        tracing::info!(target: "koine-host",
                            "desk: {} started, {} answered, {} declined",
                            pass.started, pass.answered, pass.declined)
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(target: "koine-host", "desk: {error}"),
                }
            }
            Err(error) => {
                if !said_off {
                    tracing::info!(target: "koine-host",
                        "desk agent waiting: no AI provider resolved ({error})");
                    said_off = true;
                }
            }
        }
        tokio::time::sleep(DESK_POLL).await;
    }
}
