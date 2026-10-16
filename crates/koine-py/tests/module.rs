//! The `python` feature: `register` fills a module and the functions it adds
//! list and call the linked inventory from inside an interpreter.
#![cfg(feature = "python")]

use koine_py::Hooks;
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Ignored by default: embedding needs an interpreter that can find its
/// standard library, and a uv or venv one aborts the process otherwise. Run
/// with `PYTHONHOME=<prefix> cargo test -p koine-py --features python --
/// --ignored`.
#[test]
#[ignore = "needs an embeddable Python; set PYTHONHOME"]
fn register_builds_a_module_that_lists_and_calls_verbs() {
    let _ = perf_service::DefaultPerfService::new();
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let m = PyModule::new(py, "koine_py_test").unwrap();
        koine_py::register(&m, Hooks::NONE).unwrap();
        let verbs = m.getattr("list_verbs").unwrap().call0().unwrap();
        let names: Vec<String> = verbs
            .try_iter()
            .unwrap()
            .map(|v| v.unwrap().getattr("name").unwrap().extract().unwrap())
            .collect();
        assert!(
            names.iter().any(|n| n == "perf-service_summary"),
            "{names:?}"
        );
        let out = m
            .getattr("call")
            .unwrap()
            .call1(("perf-service_summary", PyDict::new(py)))
            .unwrap();
        assert!(out.is_instance_of::<PyDict>());
        let err = m
            .getattr("call")
            .unwrap()
            .call1(("nope_nothing",))
            .unwrap_err();
        assert!(err.to_string().contains("no such verb"), "{err}");
    });
}
