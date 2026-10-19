//! The adapter against its fixtures (recorded from a real GADGET-4 run, see
//! ../README.md) and a fake binary (fixtures/fake-gadget4.sh).

use run_convention::{check_dir, check_parameter_schema};
use run_gadget4::error::Error;
use run_gadget4::exitmap::{classify, Ending};
use run_gadget4::{
    adopt, echo, h5, info, launch, post, render, schema, AdoptOptions, LaunchOptions,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fx(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn text(name: &str) -> String {
    fs::read_to_string(fx(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn json_of(name: &str) -> Value {
    serde_json::from_str(&text(name)).unwrap()
}

/// Whitespace-normalised lines: the `modulo whitespace` of the round trip.
fn squash(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn events(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn kinds(evs: &[Value]) -> Vec<&str> {
    evs.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

fn assert_conforms(dir: &Path) {
    let r = check_dir(dir);
    assert!(r.ok(), "check_dir findings:\n{r}");
    assert!(r.findings.is_empty());
}

// ---- schema harvest -------------------------------------------------------

#[test]
fn harvested_schema_passes_check_parameter_schema() {
    for src in [
        "startup-echo.txt",
        "parameters-usedvalues",
        "param.real.txt",
    ] {
        let e = echo::parse(&text(src));
        let s = schema::harvest(&e, src).unwrap_or_else(|p| panic!("{src}: {p:?}"));
        let bad = check_parameter_schema(&s);
        assert!(bad.is_empty(), "{src}: {bad:?}");
        assert_eq!(schema::names(&s).len(), 60, "{src}");
    }
}

#[test]
fn harvest_reads_the_startup_echo_not_the_banner() {
    let e = echo::parse(&text("startup-echo.txt"));
    assert!(e.from_startup_echo);
    assert_eq!(e.params.len(), 60);
    assert_eq!(e.version.as_deref(), Some("4.0"));
    assert_eq!(
        e.git_commit.as_deref(),
        Some("6e4f1128ac696260d3f2aff68cb6fa7b335e4c1c")
    );
    assert!(e.config.contains(&"PMGRID=256".to_string()));
    assert!(e.config.contains(&"PERIODIC".to_string()));
    assert!(e.duplicates.is_empty());
}

#[test]
fn harvest_types_groups_and_mutability_come_from_the_table() {
    let s = schema::harvest(&echo::parse(&text("startup-echo.txt")), "echo").unwrap();
    let p = &s["properties"];
    // The echo prints `Hubble 100`, which looks like an int; the code reads a double.
    assert_eq!(p["cosmology.Hubble"]["type"], "number");
    assert_eq!(p["io.OutputDir"]["format"], "directory");
    assert_eq!(p["io.InitCondFile"]["format"], "file");
    assert_eq!(p["cosmology.Omega0"]["x-run"]["mutable"], "never");
    assert_eq!(p["time.TimeMax"]["x-run"]["mutable"], "restart");
    assert_eq!(
        p["restart.CpuTimeBetRestartFile"]["x-run"]["mutable"],
        "restart"
    );
    assert_eq!(p["memory.MaxMemSize"]["x-run"]["mutable"], "restart");
    assert_eq!(p["output.TimeBetSnapshot"]["x-run"]["mutable"], "restart");
    assert_eq!(
        p["softening.SofteningComovingClass0"]["x-run"]["mutable"],
        "never"
    );
    assert_eq!(p["units.UnitLength_in_cm"]["x-run"]["unit"], "cm");
    assert_eq!(p["memory.MaxMemSize"]["x-run"]["unit"], "MBy");
    // GADGET-4 has no live set: nothing is `always`.
    for (name, v) in p.as_object().unwrap() {
        let m = v["x-run"]["mutable"].as_str().unwrap();
        assert!(m == "never" || m == "restart", "{name}: {m}");
        assert!(v["x-run"].get("env").is_none(), "{name}");
        assert!(v["description"].as_str().unwrap().len() > 3);
    }
    // Every tag of the real run is one the table knows.
    assert!(p
        .as_object()
        .unwrap()
        .keys()
        .all(|k| !k.starts_with("other.")));
    assert_eq!(s["x-gadget4"]["version"], "4.0");
}

#[test]
fn harvest_keeps_an_unknown_tag_as_other_and_says_so() {
    let e = echo::parse("Omega0 0.3\nShinyNewKnob 7\nMyFile ./x/y.dat\n");
    let s = schema::harvest(&e, "t").unwrap();
    assert_eq!(s["properties"]["other.ShinyNewKnob"]["type"], "integer");
    assert_eq!(s["properties"]["other.MyFile"]["format"], "file");
    assert!(s["description"].as_str().unwrap().contains("ShinyNewKnob"));
    assert!(check_parameter_schema(&s).is_empty());
}

#[test]
fn harvest_refuses_a_value_of_the_wrong_type() {
    let e = echo::parse("Omega0 notanumber\n");
    let err = schema::harvest(&e, "t").unwrap_err();
    assert_eq!(err[0].code, 202);
}

// ---- render ---------------------------------------------------------------

#[test]
fn render_round_trips_modulo_whitespace() {
    let param_txt = text("param.txt");
    let e = echo::parse(&param_txt);
    let params = render::params_from_echo(&e, "file:param.txt").unwrap();
    let back = render::render(&params, None).unwrap();
    assert_eq!(squash(&back), squash(&param_txt));
    // and through the harvested schema
    let s = json_of("schema.json");
    let back2 = render::render(&params, Some(&s)).unwrap();
    assert_eq!(squash(&back2), squash(&param_txt));
    // the shipped params.json renders to the shipped param.txt
    assert_eq!(
        squash(&render::render(&json_of("params.json"), Some(&s)).unwrap()),
        squash(&param_txt)
    );
}

#[test]
fn render_agrees_with_the_param_file_that_was_actually_run() {
    // param.real.txt is the file the recorded run used (free-form spacing,
    // `100.0`, `1e5`); the rendering must hold the same numbers.
    let real = echo::parse(&text("param.real.txt"));
    let mine = echo::parse(&text("param.txt"));
    let r: BTreeMap<_, _> = real.params.iter().cloned().collect();
    let m: BTreeMap<_, _> = mine.params.iter().cloned().collect();
    assert_eq!(r.keys().collect::<Vec<_>>(), m.keys().collect::<Vec<_>>());
    for (k, rv) in &r {
        let mv = &m[k];
        match (rv.parse::<f64>(), mv.parse::<f64>()) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{k}: {rv} vs {mv}"),
            _ => assert_eq!(rv, mv, "{k}"),
        }
    }
}

#[test]
fn render_refuses_an_unknown_name() {
    let mut p = json_of("params.json");
    p["cosmology.Bogus"] = json!({"value": 1, "source": "cli"});
    for schema in [None, Some(json_of("schema.json"))] {
        let err = render::render(&p, schema.as_ref()).unwrap_err();
        assert!(
            err.iter()
                .any(|x| x.code == 201 && x.message.contains("Bogus")),
            "{err:?}"
        );
    }
}

#[test]
fn render_refuses_a_name_in_the_wrong_group_or_with_no_group() {
    let mut p = json_of("params.json");
    p["gravity.BoxSize"] = json!({"value": 1.0, "source": "cli"});
    p["BoxSize"] = json!({"value": 1.0, "source": "cli"});
    let err = render::render(&p, None).unwrap_err();
    assert_eq!(err.iter().filter(|x| x.code == 201).count(), 2, "{err:?}");
}

#[test]
fn render_refuses_a_type_mismatch() {
    let s = json_of("schema.json");
    for (name, bad) in [
        ("cosmology.Omega0", json!("0.3")),
        ("memory.MaxMemSize", json!(1800.5)),
        ("io.OutputDir", json!(7)),
    ] {
        let mut p = json_of("params.json");
        p[name]["value"] = bad;
        let err = render::render(&p, Some(&s)).unwrap_err();
        assert!(
            err.iter()
                .any(|x| x.code == 202 && x.message.contains(name)),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn render_refuses_values_gadget4_would_misread() {
    let s = json_of("schema.json");
    let mut p = json_of("params.json");
    p["io.SnapshotFileBase"]["value"] = json!("two words");
    let err = render::render(&p, Some(&s)).unwrap_err();
    assert!(err.iter().any(|x| x.code == 204), "{err:?}");
    let mut p = json_of("params.json");
    p["time.ComovingIntegrationOn"]["value"] = json!(2);
    let err = render::render(&p, Some(&s)).unwrap_err();
    assert!(err.iter().any(|x| x.code == 204), "{err:?}");
}

#[test]
fn render_refuses_a_missing_required_tag() {
    let s = json_of("schema.json");
    let mut p = json_of("params.json");
    p.as_object_mut().unwrap().remove("cosmology.Omega0");
    let err = render::render(&p, Some(&s)).unwrap_err();
    assert!(
        err.iter()
            .any(|x| x.code == 203 && x.message.contains("Omega0")),
        "{err:?}"
    );
}

#[test]
fn render_takes_a_bare_params_object_too() {
    let p = json!({"cosmology.Omega0": 0.3, "memory.MaxMemSize": 1800});
    assert_eq!(
        squash(&render::render(&p, None).unwrap()),
        vec!["Omega0 0.3", "MaxMemSize 1800"]
    );
}

#[test]
fn floats_print_so_atof_reads_them_back() {
    use run_gadget4::value::fmt_float;
    assert_eq!(fmt_float(3.085678e24), "3.085678e24");
    assert_eq!(fmt_float(100000.0), "100000");
    assert_eq!(fmt_float(0.015625), "0.015625");
    assert_eq!(fmt_float(0.0), "0");
    assert_eq!(fmt_float(1.5e-7), "1.5e-7");
    for f in [3.085678e24, 1.989e43, 0.1, 1e-9, 12345.678] {
        assert_eq!(fmt_float(f).parse::<f64>().unwrap(), f);
    }
}

// ---- readers --------------------------------------------------------------

#[test]
fn sync_points_and_memory_lines_parse() {
    let sp = info::parse_sync_point(
        "Sync-Point 3, Time: 0.100898, Redshift: 8.91104, Systemstep: 0.00030007, Dloga: 0.00297844, Nsync-grv:       4096, Nsync-hyd:          0",
    )
    .unwrap();
    assert_eq!(sp.step, 3);
    assert_eq!(sp.time, 0.100898);
    assert_eq!(sp.dt, 0.00030007);
    assert_eq!(sp.redshift, Some(8.91104));
    assert_eq!(sp.nsync_grv, Some(4096));
    assert!(info::parse_sync_point("").is_none());
    assert!(info::parse_sync_point("DOMAIN: Begin domain decomposition (sync-point 0).").is_none());
    assert_eq!(
        info::parse_memory_line("MEMORY:  Largest Allocation = 365.554 Mbyte  |  Largest Allocation Without Generic = 365.554 Mbyte"),
        Some(365.554)
    );
    // comoving runs are integrated in ln a
    let f = info::fraction(0.105, 0.1, 0.11, true).unwrap();
    assert!(f > 0.5 && f < 0.52, "{f}");
    assert_eq!(info::fraction(2.0, 0.0, 4.0, false), Some(0.5));
    // every line of the recorded info.txt is a sync point or blank
    let all = text("adopt-dir/info.txt");
    let n = all.lines().filter_map(info::parse_sync_point).count();
    assert_eq!(n, 33);
}

#[test]
fn h5dump_groups_parse() {
    let cfg = h5::parse_group(&text("h5dump-config.txt"));
    assert_eq!(cfg["PERIODIC"], json!(true));
    assert_eq!(
        cfg["PMGRID"].as_f64(),
        Some(256.0),
        "GADGET-4 writes Config numbers as doubles"
    );
    assert_eq!(cfg["ASMTH"].as_f64(), Some(2.0));
    assert!(cfg.len() >= 10, "{}", cfg.len());
    let par = h5::parse_group(&text("h5dump-parameters.txt"));
    assert_eq!(par["BoxSize"], json!(50.0));
    assert_eq!(par["OutputDir"], json!("./output/"));
    assert_eq!(par["Seed"], json!(181170));
    assert_eq!(par.len(), 60);
    let hdr = h5::parse_group(&text("h5dump-header.txt"));
    assert_eq!(
        hdr["Git_commit"],
        json!("6e4f1128ac696260d3f2aff68cb6fa7b335e4c1c")
    );
}

#[test]
fn h5dump_reads_the_real_snapshot_groups() {
    assert!(
        h5::h5dump_available(),
        "h5dump is required to read snapshot headers"
    );
    let p = fx("small.hdf5");
    let cfg = h5::read_group(&p, "Config").unwrap();
    assert_eq!(cfg["PMGRID"].as_f64(), Some(256.0));
    assert_eq!(
        post::read_snapshot(&p).unwrap().config["PMGRID"],
        json!(256)
    );
    let par = h5::read_group(&p, "Parameters").unwrap();
    assert_eq!(par.len(), 60);
    // the snapshot's Parameters agree with the run's params.json
    let diff = post::diff_native(
        &json_of("params.json"),
        &par.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        post::SNAPSHOT_REL_TOL,
        "snapshot Parameters",
    );
    assert!(diff.is_empty(), "{diff:?}");
}

#[test]
fn used_values_differ_only_by_printf_precision() {
    let used = post::used_values(&text("parameters-usedvalues"));
    // %g prints 3.085678e24 as 3.08568e+24: a difference of print, not of value
    assert_eq!(used["UnitLength_in_cm"], json!(3.08568e24));
    let d = post::diff_native(
        &json_of("params.json"),
        &used,
        post::USEDVALUES_REL_TOL,
        "parameters-usedvalues",
    );
    assert!(d.is_empty(), "{d:?}");
    let mut p = json_of("params.json");
    p["cosmology.Omega0"]["value"] = json!(0.31);
    p["io.InitCondFile"]["value"] = json!("./other.dat");
    let d = post::diff_native(&p, &used, post::USEDVALUES_REL_TOL, "parameters-usedvalues");
    assert_eq!(d.len(), 2, "{d:?}");
}

// ---- exit bands -----------------------------------------------------------

fn e(log: &str, status: Option<i32>, stop: bool) -> (u16, &'static str, &'static str) {
    let x = classify(&Ending {
        log,
        status,
        stop_requested: stop,
        cont_file: false,
    });
    (x.code, x.label, x.state)
}

#[test]
fn exit_bands() {
    assert_eq!(
        e(
            "Final time=0.11 reached. Simulation ends.\nbye!\n",
            Some(0),
            false
        ),
        (0, "ok", "done")
    );
    // GADGET-4 exits 0 when it refuses the parameter file
    assert_eq!(
        e("Error in file p.txt:   Tag 'X' not allowed\nWe stop because of an error in the parameterfile\n", Some(0), false).0,
        201
    );
    assert_eq!(
        e(
            "Error. I miss a value for tag 'Seed' in parameter file 'p'.\n",
            Some(0),
            false
        )
        .0,
        201
    );
    assert_eq!(
        e("Parameter file p.txt not found.\n", Some(0), false).0,
        201
    );
    assert_eq!(
        e(
            "stop-file detected. stopping.\nendrun called\n",
            Some(0),
            true
        ),
        (130, "stopped", "suspended")
    );
    assert_eq!(e("reaching time-limit. stopping.\n", Some(0), false).0, 110);
    let t =
        "TERMINATE: ******!!!!!******  Code termination on task=0, function f, file x, line 2:\n";
    assert_eq!(
        e(
            &format!("{t}Not enough memory in mymalloc_fullinfo() to allocate 64.5 MB\n"),
            Some(1),
            false
        )
        .0,
        120
    );
    assert_eq!(
        e(&format!("{t}found NaN in position\n"), Some(1), false).0,
        310
    );
    assert_eq!(
        e(&format!("{t}timestep too small\n"), Some(1), false).0,
        320
    );
    assert_eq!(
        e(
            &format!("{t}integer timestep reached zero\n"),
            Some(1),
            false
        )
        .0,
        320
    );
    assert_eq!(
        e(&format!("{t}Can't open file 'x'\n"), Some(1), false).0,
        150
    );
    assert_eq!(e(&format!("{t}something else\n"), Some(1), false).0, 400);
    assert_eq!(
        e("MPI_ABORT was invoked on rank 0\n", Some(1), false).0,
        140
    );
    assert_eq!(e("", None, false).0, 140);
    assert_eq!(e("", Some(3), false), (410, "nonzero-exit", "failed"));
    // the word NaN inside another word is not a NaN
    assert_eq!(e(&format!("{t}Banana split\n"), Some(1), false).0, 400);
}

// ---- launch ---------------------------------------------------------------

fn opts(run: &Path, extra: &[&str]) -> LaunchOptions {
    let mut cmd = vec![fx("fake-gadget4.sh").to_string_lossy().into_owned()];
    cmd.extend(extra.iter().map(|s| s.to_string()));
    let mut o = LaunchOptions::new(run.to_path_buf(), json_of("params.json"), cmd);
    o.schema = Some(json_of("schema.json"));
    o.poll = Duration::from_millis(40);
    o.name = Some("fake".into());
    o
}

fn snap_arg() -> String {
    format!("snapshot={}", fx("small.hdf5").display())
}

#[test]
fn launch_writes_a_conforming_run() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let out = launch(&opts(&run, &["steps=3", "sleep=0.25", &snap_arg()])).unwrap();
    assert_eq!(out.exit.code, 0, "{:?}", out.exit);
    assert_conforms(&run);

    let rj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(rj["status"]["state"], "done");
    assert_eq!(rj["status"]["exit"]["code"], 0);
    assert_eq!(rj["name"], "fake");
    assert_eq!(rj["code"]["name"], "GADGET-4");
    assert_eq!(rj["code"]["version"], "4.0");
    assert_eq!(
        rj["code"]["commit"],
        "6e4f1128ac696260d3f2aff68cb6fa7b335e4c1c"
    );
    assert!(rj["code"]["build_flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "PMGRID=256"));
    // the code block comes from the snapshot's Config group (a real HDF5 file)
    assert_eq!(rj["code"]["build"]["PMGRID"], json!(256));
    assert!(rj["code"]["build"].as_object().unwrap().len() >= 10);
    assert!(rj["host"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "param.txt"));
    assert_eq!(rj["problem"]["name"], "gadget4");

    // params.json: every value with a source, the key checks out
    let pj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("params.json")).unwrap()).unwrap();
    assert_eq!(pj.as_object().unwrap().len(), 60);
    assert_eq!(
        pj["cosmology.Omega0"],
        json!({"value": 0.308, "source": "file:param.txt"})
    );

    // param.txt is what render gives
    assert_eq!(
        squash(&fs::read_to_string(run.join("param.txt")).unwrap()),
        squash(&text("param.txt"))
    );

    let evs = events(&run);
    let k = kinds(&evs);
    assert_eq!(k.first(), Some(&"started"));
    assert_eq!(k.last(), Some(&"finished"));
    assert_eq!(k.iter().filter(|x| **x == "progress").count(), 3, "{k:?}");
    assert_eq!(k.iter().filter(|x| **x == "metric").count(), 3);
    assert!(k.contains(&"output"));
    assert!(k.contains(&"checkpoint"));
    let prog: Vec<&Value> = evs.iter().filter(|e| e["kind"] == "progress").collect();
    assert_eq!(prog[0]["step"], 0);
    assert_eq!(prog[2]["step"], 2);
    assert_eq!(prog[2]["time"], 0.102);
    assert!(prog[2]["fraction"].as_f64().unwrap() > 0.0);
    let metric = evs.iter().find(|e| e["kind"] == "metric").unwrap();
    assert!(metric["values"]["wall_s"].as_f64().is_some());
    assert!(
        evs.iter()
            .any(|e| e["kind"] == "metric" && e["values"]["memory_largest_mb"].as_f64().is_some()),
        "memory.txt's MEMORY line reaches a metric"
    );
    let snap = evs.iter().find(|e| e["kind"] == "output").unwrap();
    assert_eq!(snap["path"], "output/snapshot_000.hdf5");
    assert_eq!(snap["kind_of"], "snapshot");

    // outputs.json indexes snapshot, restart file, diagnostics, log
    let oj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("outputs.json")).unwrap()).unwrap();
    let paths: Vec<&str> = oj["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["path"].as_str().unwrap())
        .collect();
    for want in [
        "output/snapshot_000.hdf5",
        "output/restartfiles/restart.0",
        "output/info.txt",
        "output/parameters-usedvalues",
        "stdout.log",
        "param.txt",
    ] {
        assert!(paths.contains(&want), "{want} not in {paths:?}");
    }
    // used values and the snapshot's Parameters group agree with params.json: no warnings
    assert!(
        !k.contains(&"warning"),
        "{:?}",
        evs.iter()
            .filter(|e| e["kind"] == "warning")
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_bare_params_file_gets_the_file_name_as_its_source() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let mut o = opts(&run, &["steps=1", "sleep=0.05"]);
    let bare: serde_json::Map<String, Value> = render::values(&o.params).into_iter().collect();
    o.params = Value::Object(bare);
    o.params_name = "my-params.json".into();
    launch(&o).unwrap();
    assert_conforms(&run);
    let pj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("params.json")).unwrap()).unwrap();
    assert_eq!(pj["cosmology.Omega0"]["source"], "file:my-params.json");
}

#[test]
fn a_disagreement_between_echo_and_snapshot_is_a_warning_not_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let out = launch(&opts(
        &run,
        &["steps=3", "sleep=0.25", &snap_arg(), "config=short"],
    ))
    .unwrap();
    assert_eq!(out.exit.code, 0);
    assert_conforms(&run);
    let evs = events(&run);
    let msgs: Vec<&str> = evs
        .iter()
        .filter(|e| e["kind"] == "warning")
        .map(|e| e["message"].as_str().unwrap())
        .collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("PMGRID") && m.contains("512")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.contains("FOF") && m.contains("not the startup echo")),
        "{msgs:?}"
    );
    // the record holds the snapshot's Config, the code's own statement
    let rj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(rj["code"]["build"]["PMGRID"], json!(256));
}

#[test]
fn launch_refuses_before_creating_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let mut o = opts(&run, &[]);
    o.params["cosmology.Bogus"] = json!({"value": 1, "source": "cli"});
    match launch(&o) {
        Err(Error::Invalid(ps)) => assert!(ps.iter().any(|p| p.code == 201), "{ps:?}"),
        other => panic!("expected a 2xx refusal, got {other:?}"),
    }
    assert!(!run.exists(), "a refused launch leaves nothing behind");

    // an output directory outside the run is refused too
    let mut o = opts(&run, &[]);
    o.params["io.OutputDir"]["value"] = json!("../elsewhere");
    match launch(&o) {
        Err(Error::Invalid(ps)) => assert!(ps.iter().any(|p| p.code == 205), "{ps:?}"),
        other => panic!("{other:?}"),
    }
    assert!(!run.exists());
}

#[test]
fn launch_a_binary_that_does_not_exist_is_a_failed_run_not_a_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let mut o = opts(&run, &[]);
    o.command = vec!["/nonexistent/Gadget4".into()];
    let out = launch(&o).unwrap();
    assert_eq!(out.exit.code, 160);
    assert_conforms(&run);
}

#[test]
fn stop_control_ends_the_run_in_the_1xx_band() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let o = opts(&run, &["steps=400", "sleep=0.1", &snap_arg()]);
    let run2 = run.clone();
    let h = std::thread::spawn(move || launch(&o));

    // wait for the run to make progress, then ask it to stop
    let t0 = std::time::Instant::now();
    loop {
        let seen = fs::read_to_string(run2.join("events.jsonl"))
            .map(|t| t.contains("\"progress\""))
            .unwrap_or(false);
        if seen {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "no progress event");
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::create_dir_all(run2.join("control")).unwrap();
    fs::write(
        run2.join("control/req-stop.json"),
        r#"{"request": "stop", "after": "checkpoint"}"#,
    )
    .unwrap();

    let out = h.join().unwrap().unwrap();
    assert_eq!(out.exit.code, 130, "{:?}", out.exit);
    assert_eq!(out.exit.label, "stopped");
    assert!((100..200).contains(&out.exit.code));
    assert_conforms(&run);

    let rj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(rj["status"]["state"], "suspended");
    assert_eq!(rj["status"]["exit"]["label"], "stopped");
    assert!(run.join("control/done/req-stop.json").exists());
    assert!(!run.join("control/req-stop.json").exists());
    let evs = events(&run);
    assert!(
        kinds(&evs).contains(&"checkpoint"),
        "stop writes restart files"
    );
    let last = evs.last().unwrap();
    assert_eq!(last["kind"], "finished");
    assert_eq!(last["exit"]["code"], 130);
    // the run did not run to the end
    assert!(evs.iter().filter(|e| e["kind"] == "progress").count() < 400);
}

#[test]
fn checkpoint_control_touches_restart_and_the_event_names_the_request() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let o = opts(&run, &["steps=12", "sleep=0.15"]);
    let run2 = run.clone();
    let h = std::thread::spawn(move || launch(&o));
    let t0 = std::time::Instant::now();
    while !fs::read_to_string(run2.join("events.jsonl"))
        .map(|t| t.contains("\"progress\""))
        .unwrap_or(false)
    {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(40));
    }
    fs::create_dir_all(run2.join("control")).unwrap();
    fs::write(
        run2.join("control/cp1.json"),
        r#"{"request": "checkpoint"}"#,
    )
    .unwrap();
    let out = h.join().unwrap().unwrap();
    assert_eq!(out.exit.code, 0);
    assert_conforms(&run);
    let evs = events(&run);
    let cp = evs.iter().find(|e| e["kind"] == "checkpoint").unwrap();
    assert_eq!(cp["request"], "cp1");
    assert!(run.join("control/done/cp1.json").exists());
}

#[test]
fn set_is_refused_unless_restart_class_and_then_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let o = opts(&run, &["steps=14", "sleep=0.15"]);
    let run2 = run.clone();
    let h = std::thread::spawn(move || launch(&o));
    let t0 = std::time::Instant::now();
    while !fs::read_to_string(run2.join("events.jsonl"))
        .map(|t| t.contains("\"progress\""))
        .unwrap_or(false)
    {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(40));
    }
    fs::create_dir_all(run2.join("control")).unwrap();
    fs::write(
        run2.join("control/s1.json"),
        r#"{"request": "set", "param": "cosmology.Omega0", "value": 0.3}"#,
    )
    .unwrap();
    fs::write(
        run2.join("control/s2.json"),
        r#"{"request": "set", "param": "time.TimeMax", "value": 0.2}"#,
    )
    .unwrap();
    fs::write(run2.join("control/s3.json"), r#"{"request": "pause"}"#).unwrap();
    fs::write(
        run2.join("control/s4.json"),
        r#"{"request": "set", "param": "io.Nope", "value": 1}"#,
    )
    .unwrap();
    let out = h.join().unwrap().unwrap();
    assert_eq!(out.exit.code, 0);
    assert_conforms(&run);
    for refused in ["s1", "s3", "s4"] {
        let b: Value = serde_json::from_str(
            &fs::read_to_string(run.join(format!("control/refused/{refused}.json"))).unwrap(),
        )
        .unwrap();
        assert!(b["reason"].as_str().unwrap().len() > 10, "{refused}");
    }
    let s1: Value =
        serde_json::from_str(&fs::read_to_string(run.join("control/refused/s1.json")).unwrap())
            .unwrap();
    assert!(s1["reason"].as_str().unwrap().contains("never"));
    assert!(run.join("control/done/s2.json").exists());
    let pend: Value =
        serde_json::from_str(&fs::read_to_string(run.join("restart-pending.json")).unwrap())
            .unwrap();
    assert_eq!(pend["time.TimeMax"]["value"], json!(0.2));
    // params.json is untouched: the key still checks (assert_conforms) and the value is the original
    let pj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("params.json")).unwrap()).unwrap();
    assert_eq!(pj["time.TimeMax"]["value"], json!(0.11));
}

#[test]
fn a_parameter_refusal_that_exits_zero_is_still_a_2xx_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let out = launch(&opts(&run, &["fail=param"])).unwrap();
    assert_eq!(out.exit.code, 201);
    assert_eq!(out.exit.state, "failed");
    assert_conforms(&run);
    let rj: Value =
        serde_json::from_str(&fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
    assert_eq!(rj["status"]["state"], "failed");
    assert_eq!(rj["status"]["exit"]["invalidates_reuse"], true);
}

#[test]
fn a_nan_terminate_is_a_3xx_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let out = launch(&opts(&run, &["fail=nan", "sleep=0.1"])).unwrap();
    assert_eq!(out.exit.code, 310);
    assert_eq!(out.exit.label, "nan-in-state");
    assert_conforms(&run);
}

#[test]
fn launch_does_not_overwrite_an_existing_run() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    launch(&opts(&run, &["steps=1", "sleep=0.05"])).unwrap();
    match launch(&opts(&run, &["steps=1", "sleep=0.05"])) {
        Err(Error::Other(m)) => assert!(m.contains("run.json"), "{m}"),
        other => panic!("{other:?}"),
    }
}

// ---- adopt ----------------------------------------------------------------

#[test]
fn adopt_a_finished_gadget4_output_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("output");
    copy_tree(&fx("adopt-dir"), &dir);
    let before: Vec<_> = {
        let mut v: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        v.sort();
        v
    };
    let exit = adopt(
        &dir,
        &AdoptOptions {
            name: Some("recorded".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(exit.code, 0);
    assert_conforms(&dir);

    let rj: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(rj["provenance"], "imported");
    assert_eq!(rj["convention"], "run@1");
    assert!(
        rj["key"].as_str().unwrap().len() == 64,
        "the full parameter set was recovered"
    );
    assert_eq!(rj["status"]["state"], "done");
    assert_eq!(
        rj["code"]["commit"],
        "6e4f1128ac696260d3f2aff68cb6fa7b335e4c1c"
    );
    assert_eq!(rj["code"]["build"]["PMGRID"], json!(256));
    let pj: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("params.json")).unwrap()).unwrap();
    assert_eq!(pj.as_object().unwrap().len(), 60);
    assert_eq!(
        pj["cosmology.Omega0"]["source"],
        "usedvalues:parameters-usedvalues"
    );

    let evs = events(&dir);
    assert_eq!(kinds(&evs).iter().filter(|k| **k == "progress").count(), 33);
    assert_eq!(evs.first().unwrap()["kind"], "started");
    assert_eq!(evs.last().unwrap()["kind"], "finished");
    assert!(evs
        .iter()
        .any(|e| e["kind"] == "output" && e["path"] == "snapshot_000.hdf5"));
    assert!(evs
        .iter()
        .any(|e| e["kind"] == "checkpoint" && e["path"] == "restartfiles/restart.0"));
    let oj: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("outputs.json")).unwrap()).unwrap();
    let snap = oj["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["path"] == "snapshot_000.hdf5")
        .unwrap();
    assert_eq!(snap["kind"], "snapshot");
    assert!(snap["time"].as_f64().is_some(), "Header Time via h5dump");

    // written beside, nothing moved or changed
    for n in before {
        assert!(dir.join(&n).exists(), "{n:?} still there");
    }
    assert_eq!(
        fs::read_to_string(dir.join("parameters-usedvalues")).unwrap(),
        text("parameters-usedvalues")
    );
    // and it refuses to adopt twice
    assert!(adopt(&dir, &AdoptOptions::default()).is_err());
}

#[test]
fn adopt_infers_an_unfinished_run_and_says_it_inferred() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("output");
    copy_tree(&fx("adopt-dir"), &dir);
    // cut info.txt short of TimeMax
    let info = fs::read_to_string(dir.join("info.txt")).unwrap();
    let cut: Vec<&str> = info.lines().take(20).collect();
    fs::write(dir.join("info.txt"), cut.join("\n") + "\n").unwrap();
    let exit = adopt(&dir, &AdoptOptions::default()).unwrap();
    assert_eq!((exit.code, exit.state), (130, "suspended"));
    assert!(exit.message.unwrap().contains("unknown"));
    assert_conforms(&dir);
}

#[test]
fn adopt_refuses_a_directory_with_no_usedvalues() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(adopt(tmp.path(), &AdoptOptions::default()).is_err());
}

#[test]
fn the_table_covers_every_documented_parameter() {
    // 79 rows in src/data/allvars.cc (the per-class softening tags as 3 patterns)
    let defs = run_gadget4::table::defs();
    assert_eq!(defs.len(), 79);
    let (never, restart) = defs
        .iter()
        .fold((0, 0), |(n, r), d| match d.mutable.as_str() {
            "never" => (n + 1, r),
            "restart" => (n, r + 1),
            other => panic!("{other}"),
        });
    assert_eq!((never, restart), (42, 37));
    assert!(run_gadget4::table::lookup("SofteningComovingClass5").is_some());
    assert!(run_gadget4::table::lookup("SofteningComovingClass").is_none());
    assert!(run_gadget4::table::lookup("Nope").is_none());
}
