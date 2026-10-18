//! The skill an agent reads to learn this app (plan-koine-research-os S-D1).
//!
//! [`render`] writes a `SKILL.md` from what the process links: the verbs,
//! their safety and secrets, the record kinds, and the rules that make an
//! agent safe here. The same inventory renders the same bytes, so a copy
//! checked into a repository can be diffed against a new build.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use impress_service_core::{call, VerbHandle};

use crate::context::{first_sentence, safety_word};

/// The line that marks a file as koine's own: `skill install` overwrites a
/// file that has it and keeps one that does not.
pub const GENERATED_MARK: &str = "generated-by: koine";

/// What the skill is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// The app id (`--app-id`): the skill's name and the MCP server's.
    pub app_id: String,
    /// The app's version.
    pub version: String,
    /// The record kinds the host's queries compile against.
    pub kinds: Vec<String>,
}

/// The skill's name: the app id, as a skill directory name.
pub fn skill_name(app_id: &str) -> String {
    let name: String = app_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "koine-app".into()
    } else {
        name
    }
}

/// The linked verbs, sorted by name.
fn verbs() -> Vec<VerbHandle> {
    let mut verbs: Vec<VerbHandle> = call::descriptors().collect();
    verbs.sort_by(|a, b| a.name().cmp(b.name()));
    verbs.dedup_by(|a, b| a.name() == b.name());
    verbs
}

/// The `SKILL.md` for `app`.
pub fn render(app: &AppInfo) -> String {
    let name = skill_name(&app.app_id);
    let verbs = verbs();
    let mut by_service: BTreeMap<String, Vec<&VerbHandle>> = BTreeMap::new();
    for verb in &verbs {
        by_service
            .entry(verb.service().to_string())
            .or_default()
            .push(verb);
    }
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "name: {name}");
    let _ = writeln!(
        md,
        "description: Work in {} (a koine app) through its verbs: read and change its records, \
         build surfaces (views) for the person, answer agent-desk requests, and record runs in \
         the lab notebook. Use when the task involves {} or its data.",
        app.app_id, app.app_id
    );
    let _ = writeln!(md, "---");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "<!-- {GENERATED_MARK} {} {}; regenerate with `<host> skill install` -->",
        app.app_id, app.version
    );
    let _ = writeln!(md);
    let _ = writeln!(md, "# {}", app.app_id);
    let _ = writeln!(md);
    md.push_str(&format!(
        "{} is a koine app: everything it can do is a **verb** with a typed JSON schema, a \
         safety class and a description, and everything it keeps is an **item** in one store. \
         People use the same verbs through generated forms and **surfaces** (declarative \
         views), so what you do here is what they see.\n\n",
        app.app_id
    ));

    md.push_str(
        "## Reaching the host\n\n\
         - MCP: the host serves `/mcp`; every verb below is a tool of the same name.\n\
         - HTTP: `POST /api/verb/<verb>` with a JSON object of arguments and \
         `Authorization: Bearer <token>`. The token is in \
         `<workspace>/automation/loopback-<port>.token` (workspace: `$KOINE_WORKSPACE`, else \
         `~/.koine`; port 23130 unless the host says otherwise). `GET` routes need no token.\n\
         - This file: `GET /api/skill`, the MCP resource `koine://skill`, or the verb \
         `agent-service_skill`.\n\n",
    );

    md.push_str(
        "## Rules\n\n\
         1. **A refusal is an answer.** A result with `\"ok\": false` carries a `code` and a \
         `message`; read them, and its `status` when it has one. `input_required` \
         (`review-pending`): a person must approve your call; it is kept as a pending call \
         (`pending_id`) and runs once approved — tell them, do not retry, and read \
         `review-service_show` for the answer. `auth_required` (`missing-secret`) names a \
         secret the person must set; never pass a secret as an argument. `rejected`: the \
         policy refused it; do not retry.\n\
         2. **Look before you change.** Pass `\"adverbs\": {\"tentatively\": true}` to learn \
         what a destructive or external verb would do, and under which policy, without doing \
         it.\n\
         3. **Shipped surfaces have baselines.** A surface a package shipped can be edited \
         like any other; `package-service_shipped-reset` puts the shipped one back and \
         `package-service_shipped-diff` shows what changed. Never write a \
         `koine/shipped@1.0.0` item yourself.\n\
         4. **Say what you did.** When a person is recording a run \
         (`notebook-service_runs` shows one open), your calls are in their lab notebook; \
         add what you learned with `notebook-service_annotate`.\n\
         5. **Unknown keys are refused.** Arguments are checked against the schema; a \
         misspelt key is `invalid-argument` naming the keys the verb takes.\n\
         6. **Your session's risk is tracked.** A verb's `risk` says whether it reads the \
         person's data (`private`), takes in content they did not write (`untrusted`) or \
         sends data out (`egress`). Once your session has done two of these, a call that \
         would add the third waits for a person; once it has done all three, every write and \
         every send does.\n\n",
    );

    md.push_str(
        "## Adverbs\n\nAdd `\"adverbs\": {…}` beside the arguments of any call:\n\n\
         | Adverb | Value | Effect |\n|---|---|---|\n\
         | `tentatively` | `true` | answer what would happen; run nothing |\n\
         | `with_review` | `true` | queue the call for the person's confirmation |\n\
         | `once` | a key | a retry with the same key answers the first result |\n\
         | `reversibly` | `true` | the answer names the call that undoes it (verbs that support it) |\n\
         | `within` | `{\"ms\": N}` | give up after N ms (read-only verbs, or those that support it) |\n\
         | `in_background` | `true` | start a job and answer its handle (verbs that support it) |\n\
         | `every` | `\"10m\"` | repeat on an interval until cancelled (verbs that support it) |\n\n",
    );

    md.push_str(
        "## Asked by the person: the agent desk\n\n\
         People leave work for you on the agent desk. `agent-service_next` waits for an open \
         request (up to `timeout_ms`, at most 30000) and answers it; `agent-service_claim` \
         takes it; `agent-service_answer` resolves it (name a `surface_id` when you made a \
         surface); `agent-service_decline` refuses it with a reason; `agent-service_release` \
         puts a request you claimed but cannot do right now back on the desk for another \
         try (its `releases` counts them). Each request carries \
         `context`: what the person was pointing at (`agent-service_context` answers the same \
         thing now), a failed call's record for a `diagnose` request, and for a \
         `make-surface` request the kinds and verbs to build with.\n\n",
    );

    md.push_str(
        "## Packages\n\n\
         Surfaces and skills also arrive as packages from registries. \
         `package-service_search` lists what the known registries offer; \
         `package-service_fetch {name}` downloads one and answers its review (the verbs \
         it calls, their safety, what is missing, what installing would change). Show the \
         person that review: `package-service_install {path, digest}` installs exactly \
         what was reviewed and nothing else.\n\n",
    );

    md.push_str(
        "## Making a surface\n\n\
         A surface is a JSON document: `state`, `sources` (a verb call or a query, \
         re-run when the templates in its args change), and a `root` node tree (`column`, \
         `row`, `tabs`, `text`, `table`, `list`, `tree`, `plot`, `kv`, `field`, `button`, \
         `status`, `log`, `code`, …) whose actions `set` state, `call` verbs, `refresh` \
         sources and `publish` selections. Templates are `{{state.x}}`, `{{source.y.rows}}`, \
         `{{event.value}}`. Validate with `impress-surface-service_surface-validate`, store \
         with `…_surface-create`, show it to the person with `…_surface-show`. The smallest \
         one:\n\n\
         ```json\n\
         {\"surface\": \"1.0\", \"name\": \"Hello\", \"state\": {}, \"sources\": {},\n \
         \"root\": {\"column\": [{\"text\": \"Hello.\"}]}}\n\
         ```\n\n",
    );

    if !app.kinds.is_empty() {
        md.push_str("## Record kinds\n\n");
        for kind in &app.kinds {
            let _ = writeln!(md, "- `{kind}`");
        }
        md.push('\n');
    }

    let _ = writeln!(md, "## Verbs ({})\n", verbs.len());
    md.push_str(
        "Safety: `read-only` changes nothing; `mutating` writes and may be queued for review \
         when an agent calls it; `destructive` and `external` are always worth a `tentatively` \
         first.\n\n",
    );
    for (service, verbs) in &by_service {
        let _ = writeln!(md, "### {service}\n");
        for verb in verbs {
            let mut tags = vec![safety_word(verb.safety().class).to_string()];
            if let Some(kind) = verb.returns_kind() {
                tags.push(format!("rows of `{kind}`"));
            }
            if !verb.secrets().is_empty() {
                tags.push(format!("needs {}", verb.secrets().join(", ")));
            }
            let risk = verb.risk_words();
            if !risk.is_empty() {
                tags.push(format!("risk: {}", risk.join(", ")));
            }
            let _ = writeln!(
                md,
                "- `{}` ({}): {}",
                verb.name(),
                tags.join("; "),
                first_sentence(verb.description())
            );
        }
        md.push('\n');
    }
    md
}

/// Where `skill install` writes by default: the skill directories of the
/// agents that exist under `home`, as `<dir>/<skill name>/SKILL.md`.
pub fn default_targets(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    [".claude", ".codex", ".agents"]
        .iter()
        .filter(|agent| home.join(agent).is_dir())
        .map(|agent| home.join(agent).join("skills"))
        .collect()
}

/// What happened to one target of [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    Written(std::path::PathBuf),
    /// A file koine did not write is there; kept (pass `force`).
    KeptForeign(std::path::PathBuf),
    Failed(std::path::PathBuf, String),
}

/// Write `skill` as `<dir>/<name>/SKILL.md` into each of `dirs`. A file
/// without [`GENERATED_MARK`] is kept unless `force`.
pub fn install(
    skill: &str,
    name: &str,
    dirs: &[std::path::PathBuf],
    force: bool,
) -> Vec<Installed> {
    dirs.iter()
        .map(|dir| {
            let folder = dir.join(name);
            let path = folder.join("SKILL.md");
            if !force {
                if let Ok(existing) = std::fs::read_to_string(&path) {
                    if !existing.contains(GENERATED_MARK) {
                        return Installed::KeptForeign(path);
                    }
                }
            }
            match std::fs::create_dir_all(&folder).and_then(|_| std::fs::write(&path, skill)) {
                Ok(()) => Installed::Written(path),
                Err(e) => Installed::Failed(path, e.to_string()),
            }
        })
        .collect()
}
