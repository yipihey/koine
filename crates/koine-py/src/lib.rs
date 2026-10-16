//! The Python module builder (docs/plan-koine-kit-moves.md KM1; moved from
//! impress-apps' `impress-py`, plan-verb-pipeline-and-transport.md § P6).
//!
//! One generic binding over whatever inventory is linked into the binary that
//! embeds it: `list_verbs()` and `call(verb, args)`, instead of a shim per
//! `#[impress_method]`. It has **no path of its own**:
//!
//! * `list_verbs()` reads [`impress_service_core::call::descriptors`], the
//!   linked inventory the CLI and MCP servers read.
//! * `call(verb, args)` with no `app` goes through
//!   [`impress_service_core::dispatch::dispatch`] as
//!   `CallerIdentity::App("python")`: this module speaks for the process it
//!   is imported into, so `App` is the honest identity, not `Agent`.
//! * `call(verb, args, app="name")` goes over [`impress_app_transport::call`]
//!   to a running app's automation server.
//!
//! The binary crate (a `cdylib`) owns what this crate cannot know: which
//! service crates to keep linked, and which provider registry to refresh.
//! It supplies them as [`Hooks`], and its `#[pymodule]` calls
//! [`register`]:
//!
//! ```ignore
//! #[pymodule]
//! fn mymodule(m: &Bound<'_, PyModule>) -> PyResult<()> {
//!     koine_py::register(m, koine_py::Hooks { force_link, reload_providers })
//! }
//! ```
//!
//! The logic ([`list_verb_infos`], [`call_local`], [`call_remote`]) is plain
//! Rust with no `pyo3` in its signature and is tested without an interpreter.
//! The pyo3 wrappers are behind the `python` feature.

use std::sync::OnceLock;

use impress_service_core::descriptor_handle::VerbHandle;
use serde_json::Value;

/// What the embedding binary supplies.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Reference the service crates whose verbs the module should expose, so
    /// the linker cannot strip them from a `cdylib` (nothing else names them).
    /// Called before every listing and local call.
    pub force_link: fn(),
    /// Refresh the process's runtime providers from its workspace. Called once
    /// at import and by `reload_providers()`. `Ok(true)` means providers were
    /// installed, and their health is refreshed.
    pub reload_providers: fn() -> Result<bool, String>,
}

impl Hooks {
    /// No extra linking, no provider registry beyond the transport's own.
    pub const NONE: Hooks = Hooks {
        force_link: || {},
        reload_providers: || Ok(false),
    };
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Install the embedding binary's hooks (first call wins). [`register`] does
/// this; a Rust test or a non-Python embedder may call it directly.
pub fn set_hooks(hooks: Hooks) {
    let _ = HOOKS.set(hooks);
}

fn hooks() -> Hooks {
    HOOKS.get().copied().unwrap_or(Hooks::NONE)
}

/// One verb's public shape, as the catalogue and `docs/verbs/` show it:
/// enough for a caller to build a call without reading Rust source.
#[derive(Debug, Clone, PartialEq)]
pub struct VerbInfo {
    pub name: String,
    pub service: String,
    pub description: String,
    pub safety: String,
    pub input_schema: Value,
}

fn verb_info(verb: VerbHandle) -> VerbInfo {
    VerbInfo {
        name: verb.name().to_string(),
        service: verb.service().to_string(),
        description: verb.description().to_string(),
        safety: verb.safety().class.to_string(),
        input_schema: verb.input_schema().into_owned(),
    }
}

/// Every verb in the linked inventory: name, service, description, safety
/// class and input schema. Runs the [`Hooks::force_link`] hook first.
pub fn list_verb_infos() -> Vec<VerbInfo> {
    (hooks().force_link)();
    impress_service_core::call::descriptors()
        .map(verb_info)
        .collect()
}

/// `call(verb, args)` with no `app`: in-process, through
/// [`impress_service_core::dispatch::dispatch`] with
/// `CallerIdentity::App("python")`. `Err` carries the pipeline's own message
/// (a refusal's `message` field, or a JSON-parse error).
pub fn call_local(verb: &str, args: Value) -> Result<Value, String> {
    (hooks().force_link)();
    let args_json = args.to_string();
    let caller_json = r#"{"kind": "app", "name": "python"}"#;
    let outcome = impress_service_core::dispatch::dispatch(verb, &args_json, caller_json);
    let body: Value = serde_json::from_str(&outcome.body_json).map_err(|e| e.to_string())?;
    if outcome.status >= 400 {
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("verb call failed")
            .to_string();
        return Err(message);
    }
    Ok(body)
}

/// `call(verb, args, app=...)`: over the P5 transport to a running app's
/// automation server, unchanged: [`impress_app_transport::call`].
pub fn call_remote(app: &str, verb: &str, args: Value) -> Result<Value, String> {
    impress_service_core::runtime::block_on(impress_app_transport::call(app, verb, args))
        .map_err(|refusal| refusal.to_string())
}

/// Refresh this process's runtime providers: the transport's own provider
/// installer, then the embedding binary's [`Hooks::reload_providers`]; when
/// that installed providers, their health is refreshed. Python may call this
/// again after another host registers a provider.
pub fn reload_provider_registry() -> Result<(), String> {
    impress_app_transport::provider::install();
    if (hooks().reload_providers)()? {
        impress_service_core::runtime::block_on(
            impress_service_core::registry_runtime::refresh_health(),
        );
    }
    Ok(())
}

#[cfg(feature = "python")]
pub use py::register;

#[cfg(feature = "python")]
mod py {
    use super::{call_local, call_remote, list_verb_infos, Hooks, VerbInfo};
    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    use pyo3::prelude::*;
    use pyo3::types::PyDict;
    use pythonize::{depythonize, pythonize};
    use serde_json::Value;

    #[pyclass(name = "VerbInfo")]
    pub struct PyVerbInfo {
        #[pyo3(get)]
        pub name: String,
        #[pyo3(get)]
        pub service: String,
        #[pyo3(get)]
        pub description: String,
        #[pyo3(get)]
        pub safety: String,
        #[pyo3(get)]
        pub input_schema: Py<PyAny>,
    }

    #[pymethods]
    impl PyVerbInfo {
        fn __repr__(&self) -> String {
            format!(
                "VerbInfo(name={:?}, service={:?}, safety={:?})",
                self.name, self.service, self.safety
            )
        }
    }

    fn to_py_verb_info(py: Python<'_>, info: VerbInfo) -> PyResult<PyVerbInfo> {
        Ok(PyVerbInfo {
            name: info.name,
            service: info.service,
            description: info.description,
            safety: info.safety,
            input_schema: pythonize(py, &info.input_schema)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
                .into(),
        })
    }

    /// Every verb in the linked inventory: name, service, description,
    /// safety class and input schema.
    #[pyfunction]
    fn list_verbs(py: Python<'_>) -> PyResult<Vec<PyVerbInfo>> {
        list_verb_infos()
            .into_iter()
            .map(|info| to_py_verb_info(py, info))
            .collect()
    }

    /// Call one verb, in-process (`app=None`, the default) or against a
    /// running app (`app="impress"`, `"imbib"`, …, over the P5 transport).
    ///
    /// `args` is a plain `dict`; the result is the wire envelope's `dict`
    /// (`{"ok": true, …}`). A refusal raises `RuntimeError` rather than
    /// making every caller check `result["ok"]`.
    #[pyfunction]
    #[pyo3(signature = (verb, args=None, app=None))]
    fn call(
        py: Python<'_>,
        verb: &str,
        args: Option<Bound<'_, PyDict>>,
        app: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let args_value: Value = match &args {
            Some(dict) => depythonize(dict).map_err(|e| PyValueError::new_err(e.to_string()))?,
            None => Value::Object(Default::default()),
        };

        let result = match app {
            Some(app_name) => {
                let app_name = app_name.to_string();
                let verb = verb.to_string();
                py.allow_threads(|| call_remote(&app_name, &verb, args_value))
                    .map_err(PyRuntimeError::new_err)?
            }
            None => {
                let verb = verb.to_string();
                py.allow_threads(|| call_local(&verb, args_value))
                    .map_err(PyRuntimeError::new_err)?
            }
        };

        pythonize(py, &result)
            .map(Into::into)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    /// Reload providers registered since this module was imported.
    #[pyfunction]
    fn reload_providers(py: Python<'_>) -> PyResult<()> {
        py.allow_threads(super::reload_provider_registry)
            .map_err(PyRuntimeError::new_err)
    }

    /// Fill a `#[pymodule]` with `VerbInfo`, `list_verbs`, `call` and
    /// `reload_providers`, over the inventory the calling binary links.
    /// Installs `hooks`, then refreshes providers once, as import does.
    pub fn register(m: &Bound<'_, PyModule>, hooks: Hooks) -> PyResult<()> {
        super::set_hooks(hooks);
        super::reload_provider_registry().map_err(PyRuntimeError::new_err)?;
        m.add_class::<PyVerbInfo>()?;
        m.add_function(wrap_pyfunction!(list_verbs, m)?)?;
        m.add_function(wrap_pyfunction!(call, m)?)?;
        m.add_function(wrap_pyfunction!(reload_providers, m)?)?;
        Ok(())
    }
}
