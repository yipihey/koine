use run_convention::import::{detect, import_dir, ImportOptions, Outcome, Style};
use run_convention::{canonical_json, check_dir, key_of, values_of};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/import")
        .join(name)
}

/// A scratch copy: the importer writes beside what is there, so tests never
/// touch the committed fixtures.
fn copy_of(name: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    for e in fs::read_dir(fixture(name)).unwrap().flatten() {
        fs::copy(e.path(), t.path().join(e.file_name())).unwrap();
    }
    t
}

fn read(dir: &Path, f: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join(f)).unwrap()).unwrap()
}

fn import(dir: &Path) -> run_convention::import::Plan {
    match import_dir(dir, &ImportOptions::default()).unwrap() {
        Outcome::Written(p) => p,
        o => panic!("expected a write, got {o:?}"),
    }
}

fn assert_conforms(dir: &Path) {
    let r = check_dir(dir);
    assert!(r.ok(), "{r}");
}

fn no_nulls(v: &Value) {
    match v {
        Value::Null => panic!("null in a written record (R-D6.5a)"),
        Value::Array(a) => a.iter().for_each(no_nulls),
        Value::Object(o) => o.values().for_each(no_nulls),
        _ => {}
    }
}

#[test]
fn command_style_has_a_key_and_conforms() {
    let t = copy_of("command");
    let p = import(t.path());
    assert_eq!(p.styles, vec![Style::Command, Style::Metadata]);
    assert_conforms(t.path());
    let run = read(t.path(), "run.json");
    for f in ["run.json", "params.json", "outputs.json"] {
        no_nulls(&read(t.path(), f));
    }
    assert_eq!(run["convention"], "run@1");
    assert_eq!(run["provenance"], "imported");
    assert_eq!(
        run["imported"]["from"],
        serde_json::json!(["command.sh", "metadata.toml"])
    );
    assert_eq!(run["problem"]["name"], "pmf_dm_lattice_bench");
    assert_eq!(run["host"]["hostname"], "host-a");
    assert_eq!(run["code"]["julia"], "1.12.6");
    // The key is the checker's own: recompute it from the written files.
    let params = read(t.path(), "params.json");
    assert_eq!(params["mhd.pmf_n"]["value"], 32);
    assert_eq!(params["mhd.pmf_n"]["source"], "imported:command.sh");
    assert_eq!(params["meta.n"]["source"], "imported:metadata.toml");
    assert_eq!(
        run["key"].as_str().unwrap(),
        key_of(&run["problem"], &values_of(&params))
    );
    // Output paths and thread counts belong to the host, not the key.
    assert!(params.get("mhd.history_tsv").is_none());
    assert!(run["host"]["env"]["MHD_HISTORY_TSV"].is_string());
    assert_eq!(run["host"]["env"]["JULIA_NUM_THREADS"], "auto");
    // run.log has no sentinel: honest "unknown", still a legal record.
    assert_eq!(run["status"]["state"], "done");
    assert_eq!(run["status"]["exit"]["label"], "unknown");
    // metadata.toml repeats `chemistry`; last wins, and it is reported.
    assert!(run["imported"]["warnings"].is_array());
    // outputs: hashes for small files, kinds guessed.
    let outs = read(t.path(), "outputs.json");
    let outs = outs["outputs"].as_array().unwrap();
    assert!(outs.iter().all(|o| o["sha256"].is_string()));
    let kind = |path: &str| {
        outs.iter().find(|o| o["path"] == path).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(kind("run.log"), "log");
    assert_eq!(kind("summary.tsv"), "series");
    // notes: campaign as a tag, metadata `note` absent in this fixture.
    let notes = read(t.path(), "notes.json");
    assert_eq!(notes["tags"][0], "pmf-metal-solver-ladder");
}

#[test]
fn log_markers_decide_the_status() {
    for (log, state, code, label) in [
        ("x\n===== EXIT 0 =====\n", "done", 0, "ok"),
        ("x\n===== EXIT 7 =====\n", "failed", 407, "exit-7"),
        (
            "ERROR: LoadError: Metal backend is not functional\n",
            "failed",
            110,
            "backend-unavailable",
        ),
        (
            "ERROR: LoadError: invalid timestep dt=NaN\n",
            "failed",
            300,
            "numerical-failure",
        ),
        ("COMPLETE z=1000\n", "done", 0, "ok"),
    ] {
        let t = copy_of("command");
        fs::write(t.path().join("run.log"), log).unwrap();
        import(t.path());
        assert_conforms(t.path());
        let s = &read(t.path(), "run.json")["status"];
        assert_eq!(s["state"], state, "{log}");
        assert_eq!(s["exit"]["code"], code, "{log}");
        assert_eq!(s["exit"]["label"], label, "{log}");
    }
}

#[test]
fn vrun_manifest_gets_a_key_only_for_a_known_complete_family() {
    // CIC_* are the whole parameter set of a cicass suite...
    let t = copy_of("manifest");
    let p = import(t.path());
    assert_eq!(p.primary(), Style::Manifest);
    assert_conforms(t.path());
    let run = read(t.path(), "run.json");
    assert!(run["key"].is_string());
    assert_eq!(run["code"]["commit_short"], true);
    assert_eq!(run["code"]["version"], "1a5587f");
    assert!(run["code"].get("commit").is_none());
    assert_eq!(run["code"]["dirty"], true);
    assert!(
        run["code"].get("julia").is_none(),
        "julia_ver = unknown is omitted"
    );
    assert_eq!(run["host"]["accelerator"], "Apple Metal");
    assert_eq!(run["status"]["state"], "done");
    assert_eq!(run["status"]["exit"]["label"], "ok");
    assert_eq!(run["status"]["ended_at"], "2026-07-02T18:39:12.000Z");
    let notes = read(t.path(), "notes.json");
    assert!(notes["notes"][0].as_str().unwrap().contains("512^3"));
    assert_eq!(notes["tags"][0], "paired-fixed-m5");

    // ...but the same manifest for a PMF run captured only CIC_*: no key.
    let t = copy_of("manifest");
    let m = fs::read_to_string(t.path().join("manifest.toml"))
        .unwrap()
        .replace("cicass-suite512", "pmf-suite");
    fs::write(t.path().join("manifest.toml"), m).unwrap();
    import(t.path());
    assert_conforms(t.path());
    let run = read(t.path(), "run.json");
    assert!(run.get("key").is_none());
    assert!(run["imported"]["key_withheld"]
        .as_str()
        .unwrap()
        .contains("MHD_"));
}

#[test]
fn bulk_manifest_with_fields_sidecar() {
    let t = copy_of("bulk");
    import(t.path());
    assert_conforms(t.path());
    let run = read(t.path(), "run.json");
    assert_eq!(run["problem"]["name"], "bamr");
    assert_eq!(run["code"]["version"], "cbe7a91");
    assert_eq!(run["code"]["julia"], "1.12.6");
    assert_eq!(run["status"]["exit"]["label"], "ok");
    assert!(run["key"].is_string());
    let params = read(t.path(), "params.json");
    assert_eq!(params["bam.ngrid"]["value"], 16);
    assert_eq!(params["cic.zstart"]["value"], 1000.0);
    let outs = read(t.path(), "outputs.json");
    let bin = outs["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["path"] == "bamr_cellcmp_bamr16m5smoke_z999.bin")
        .unwrap();
    assert_eq!(bin["format"], "bin+toml");
    assert_eq!(bin["schema"], "fields.toml");
    assert_eq!(bin["kind"], "snapshot");
    assert_eq!(bin["bytes"], 256);
}

#[test]
fn metadata_only_has_no_key_and_still_conforms() {
    let t = copy_of("metadata");
    let p = import(t.path());
    assert_eq!(p.styles, vec![Style::Metadata]);
    assert_conforms(t.path());
    let run = read(t.path(), "run.json");
    assert!(run.get("key").is_none());
    assert!(run["imported"]["key_withheld"].is_string());
    let params = read(t.path(), "params.json");
    // sections namespace their keys; bookkeeping keys are not parameters
    assert_eq!(params["precision_matrix.ngrids"]["value"], "128");
    assert!(params.get("precision_matrix.vespa_sha").is_none());
    assert_eq!(run["code"]["version"], "f17ab87");
}

#[test]
fn reimport_is_stable_and_never_overwrites_without_force() {
    let t = copy_of("command");
    import(t.path());
    let first = read(t.path(), "run.json");
    // second run: skipped, nothing changes
    match import_dir(t.path(), &ImportOptions::default()).unwrap() {
        Outcome::Skipped(_) => {}
        o => panic!("{o:?}"),
    }
    assert_eq!(read(t.path(), "run.json"), first);
    // forced: same id and key; the outputs index no longer lists record files
    let o = ImportOptions {
        force: true,
        ..Default::default()
    };
    assert!(matches!(
        import_dir(t.path(), &o).unwrap(),
        Outcome::Written(_)
    ));
    let again = read(t.path(), "run.json");
    assert_eq!(again["id"], first["id"]);
    assert_eq!(again["key"], first["key"]);
    assert_conforms(t.path());
    // the id is a function of the folder name only
    let u = copy_of("command");
    let id_other = {
        import(u.path());
        read(u.path(), "run.json")["id"].clone()
    };
    assert_ne!(
        id_other, first["id"],
        "different folder names, different ids"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let t = copy_of("command");
    let o = ImportOptions {
        dry_run: true,
        ..Default::default()
    };
    match import_dir(t.path(), &o).unwrap() {
        Outcome::DryRun(p) => assert!(p.has_key()),
        o => panic!("{o:?}"),
    }
    for f in ["run.json", "params.json", "outputs.json", "notes.json"] {
        assert!(!t.path().join(f).exists(), "{f}");
    }
}

#[test]
fn unrecognised_folders_are_reported_not_invented() {
    let t = copy_of("unrecognised");
    assert!(detect(t.path()).is_empty());
    assert!(matches!(
        import_dir(t.path(), &ImportOptions::default()).unwrap(),
        Outcome::Unrecognised
    ));
    assert!(!t.path().join("run.json").exists());
}

#[test]
fn large_files_get_bytes_only() {
    let t = copy_of("bulk");
    let big = t.path().join("big.bin");
    let f = fs::File::create(&big).unwrap();
    f.set_len(run_convention::import::HASH_LIMIT).unwrap();
    import(t.path());
    assert_conforms(t.path());
    let outs = read(t.path(), "outputs.json");
    let e = outs["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["path"] == "big.bin")
        .unwrap();
    assert!(e.get("sha256").is_none());
    assert!(e["note"].as_str().unwrap().contains("64 MiB"));
    let _ = canonical_json(&Value::Null);
}

/// Read-only walk of the real tree. `cargo test -p run-convention --test
/// import -- --ignored --nocapture`.
#[test]
#[ignore]
fn real_vespa_runs_tree_dry_run() {
    let root = Path::new("/Users/tabel/Projects/vespa-runs/runs");
    if !root.is_dir() {
        eprintln!("{} not present; nothing to do", root.display());
        return;
    }
    let o = ImportOptions {
        dry_run: true,
        ..Default::default()
    };
    let mut per_style = std::collections::BTreeMap::<&str, (usize, usize)>::new();
    let (mut total, mut keyed, mut unrec, mut skipped) = (0, 0, 0, 0);
    let mut why = std::collections::BTreeMap::<String, usize>::new();
    let mut entries: Vec<_> = fs::read_dir(root).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if !e.path().is_dir() {
            continue;
        }
        total += 1;
        match import_dir(e.path(), &o).unwrap() {
            Outcome::DryRun(p) => {
                let s = per_style.entry(p.primary().label()).or_default();
                s.0 += 1;
                if p.has_key() {
                    s.1 += 1;
                    keyed += 1;
                } else {
                    let w = p.run["imported"]["key_withheld"].as_str().unwrap();
                    let w = w.split(" (only").next().unwrap().to_string();
                    *why.entry(w).or_default() += 1;
                }
            }
            Outcome::Unrecognised => unrec += 1,
            Outcome::Skipped(_) => skipped += 1,
            Outcome::Written(_) => unreachable!(),
        }
    }
    println!(
        "folders: {total}  unrecognised: {unrec}  skipped(existing): {skipped}  keyed: {keyed}"
    );
    for (s, (n, k)) in &per_style {
        println!("  {s}: {n} folders, {k} with a key");
    }
    for (w, n) in &why {
        println!("  no key ({n}): {w}");
    }
    assert_eq!(
        total,
        per_style.values().map(|v| v.0).sum::<usize>() + unrec + skipped
    );
}
