//! GADGET-4's endings, mapped to the convention's labelled exit bands (R-D4.5).
//!
//! GADGET-4 cannot be judged by its exit status: `endrun()` and a parameter
//! error both exit 0 (the Gadget4Lib bridge scans the log for the same reason),
//! and `Terminate()` calls `MPI_Abort`, which `mpirun` reports as 1. So the log
//! is read first, the status second.
//!
//! | code | label | when |
//! |---|---|---|
//! | 0 | ok | ran to the end |
//! | 110 | walltime | `TimeLimitCPU` reached: "reaching time-limit. stopping." (restart files written, `cont` file) |
//! | 120 | out-of-memory | "Not enough memory in mymalloc" (`MaxMemSize`) |
//! | 130 | stopped | a `stop` request was honoured ("stop-file detected. stopping.") |
//! | 140 | mpi-failure | `MPI_ABORT`/signal without a GADGET-4 message |
//! | 150 | io-failure | cannot open/read/write a file |
//! | 201 | invalid-parameters | parameter file refused (unknown/missing/repeated tag) |
//! | 310 | nan-in-state | `Terminate` naming NaN |
//! | 320 | timestep-collapse | "timestep too small", "integer timestep reached zero" |
//! | 400 | terminated | any other `Terminate`/`endrun` with an error |
//! | 410 | nonzero-exit | non-zero status with nothing in the log to say why |

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: u16,
    pub label: &'static str,
    pub message: Option<String>,
    pub invalidates_reuse: bool,
    /// The run state to write: `done`, `suspended` or `failed`.
    pub state: &'static str,
}

impl Exit {
    fn new(
        code: u16,
        label: &'static str,
        message: Option<String>,
        state: &'static str,
        invalidates: bool,
    ) -> Self {
        Exit {
            code,
            label,
            message,
            invalidates_reuse: invalidates,
            state,
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        let mut o = serde_json::json!({"code": self.code, "label": self.label});
        if let Some(m) = &self.message {
            o["message"] = serde_json::json!(m);
        }
        if self.invalidates_reuse {
            o["invalidates_reuse"] = serde_json::json!(true);
        }
        o
    }
}

/// What the adapter saw.
#[derive(Debug, Default, Clone)]
pub struct Ending<'a> {
    /// The stdout/stderr text (head and tail is enough).
    pub log: &'a str,
    /// The child's exit status; `None` when it was killed by a signal.
    pub status: Option<i32>,
    /// The adapter wrote a `stop` file at a control request's asking.
    pub stop_requested: bool,
    /// GADGET-4 wrote the `cont` file (it hit the CPU-time limit).
    pub cont_file: bool,
}

fn first_line_with<'a>(log: &'a str, needles: &[&str]) -> Option<&'a str> {
    log.lines()
        .find(|l| needles.iter().any(|n| l.contains(n)))
        .map(str::trim)
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

pub fn classify(e: &Ending<'_>) -> Exit {
    let log = e.log;
    // 1. The parameter file was refused (GADGET-4 then exits 0).
    let refused = log.lines().map(str::trim).find(|l| {
        l.contains("We stop because of an error in the parameterfile")
            || l.contains("I miss a value for tag")
            || (l.starts_with("Error in file") && l.contains("Tag '"))
            || (l.starts_with("Parameter file") && l.contains("not found"))
    });
    if refused.is_some() {
        let detail = first_line_with(
            log,
            &["Error in file", "I miss a value for tag", "Parameter file"],
        )
        .or(refused)
        .unwrap_or("");
        return Exit::new(
            201,
            "invalid-parameters",
            Some(clip(detail)),
            "failed",
            true,
        );
    }
    // 2. A Terminate(): the message names the cause.
    let terminated = log.contains("TERMINATE:") || log.contains("Code termination on task");
    if terminated {
        let at = log
            .find("Code termination on task")
            .or_else(|| log.find("TERMINATE:"))
            .unwrap_or(0);
        let tail = &log[at..];
        let msg: String = clip(
            &tail
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        );
        let low = tail.to_ascii_lowercase();
        if low.contains("not enough memory") {
            return Exit::new(120, "out-of-memory", Some(msg), "failed", false);
        }
        if low
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "nan")
        {
            return Exit::new(310, "nan-in-state", Some(msg), "failed", true);
        }
        if low.contains("timestep too small")
            || low.contains("integer timestep reached zero")
            || low.contains("time-step of integer size 1")
        {
            return Exit::new(320, "timestep-collapse", Some(msg), "failed", true);
        }
        if low.contains("can't open")
            || low.contains("cannot open")
            || low.contains("error reading")
            || low.contains("error writing")
            || low.contains("i/o")
            || low.contains("hdf5")
        {
            return Exit::new(150, "io-failure", Some(msg), "failed", false);
        }
        return Exit::new(400, "terminated", Some(msg), "failed", true);
    }
    // 3. A deliberate stop.
    let finished = log.contains("Simulation ends");
    if e.status == Some(0)
        && (log.contains("stop-file detected") || (e.stop_requested && !finished))
    {
        return Exit::new(
            130,
            "stopped",
            Some("stop request honoured; restart files written".into()),
            "suspended",
            false,
        );
    }
    // 4. Out of CPU time: GADGET-4 stops itself and leaves `cont`.
    if e.cont_file || log.contains("reaching time-limit. stopping.") {
        return Exit::new(
            110,
            "walltime",
            Some("TimeLimitCPU reached; restart files written".into()),
            "suspended",
            false,
        );
    }
    // 5. No GADGET-4 message: the status decides.
    match e.status {
        Some(0) => Exit::new(0, "ok", None, "done", false),
        Some(n) => {
            if log.contains("MPI_ABORT") || log.contains("mpirun") || log.contains("prterun") {
                Exit::new(
                    140,
                    "mpi-failure",
                    Some(format!("MPI aborted with status {n}")),
                    "failed",
                    false,
                )
            } else {
                Exit::new(
                    410,
                    "nonzero-exit",
                    Some(format!("exit status {n}")),
                    "failed",
                    true,
                )
            }
        }
        None => Exit::new(
            140,
            "mpi-failure",
            Some("the process was killed by a signal".into()),
            "failed",
            false,
        ),
    }
}
