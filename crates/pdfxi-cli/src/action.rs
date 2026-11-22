//! Action-file commands: `create`, `list`, `show`, `run`, and the `droplet` entry
//! point.
//!
//! ## Staging and real execution
//!
//! [`EchoOperations`] implements `pdfxi_automation::DocumentOperations` as a
//! side-effect-free adapter. The `import` step copies the staged input bytes into
//! memory and the other steps pass the current artifact along while recording a
//! short, deterministic message. Export resolves the target path against
//! `--output-dir` but writes nothing. This is the default for `action run` and
//! `droplet`, so scripts and tests can assert outcomes without touching content.
//!
//! `action run --execute` selects [`RealOperations`] instead: import validates
//! that the input parses as a PDF, a watermark step stamps real page content,
//! and an export step writes the artifact beneath `--output-dir`. Security and
//! custom-command steps are refused with a typed message rather than silently
//! staged, because their writers remain separate features. Both modes use the
//! same `DocumentExecutor` / `BatchQueue` pipeline (validation, ordering,
//! failure isolation, cancellation, per-step results) and output shapes.
//!
//! `action create` writes one validated tab-delimited action file from
//! `--step ID:KIND` and `--param ID:KEY=VALUE` flags, so the create/edit path is
//! reachable without hand-writing the format.
//!
//! ## Explorer drop handlers
//!
//! `pdfxi droplet <ACTION_FILE> <NAME> <INPUT>...` is `action run` with default
//! text output and no `--output-dir`. Because it takes a variadic input list, a
//! Windows Explorer "drop onto target" verb can pass the dropped paths straight
//! through. For example, a registry verb under
//! `HKEY_CLASSES_ROOT\*\shell\PDFXI Stage\command` may use a default value like:
//!
//! ```text
//! "C:\Program Files\PDFXI\pdfxi.exe" droplet "C:\Program Files\PDFXI\staging.act" nightly "%1"
//! ```
//!
//! Use `%*` instead of `%1` to forward several dropped files at once. The
//! command prints one deterministic line per input in drop order and exits
//! non-zero if any input failed, so it composes with `.bat` wrappers and task
//! scheduler jobs.

use anyhow::{Context, bail};
use pdfxi_automation::{
    Action, ActionLibrary, ActionStep, Artifact, BatchJob, BatchQueue, DocumentExecutor,
    DocumentOperations, JobState, OperationError, OperationOutcome, StepKind,
};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// A deterministic, no-mutation implementation of the document operations.
///
/// See the [module documentation](self) for why this executor stages inputs and
/// records outcomes instead of writing PDFs. The only failure conditions are
/// deterministic and content-independent: a non-path artifact, a directory, an
/// empty or oversized file, an unreadable file, an export target that is
/// absolute or contains a traversal component, or an action step whose required
/// parameter is absent (reported by `DocumentExecutor` before this adapter is
/// called).
#[derive(Debug)]
pub struct EchoOperations {
    output_dir: Option<PathBuf>,
}

impl EchoOperations {
    /// Creates an adapter that resolves relative export targets against
    /// `output_dir` when one is supplied.
    pub fn new(output_dir: Option<PathBuf>) -> Self {
        Self { output_dir }
    }
}

/// Largest input accepted by the staging `import` step: 512 MiB.
///
/// A single staged input is held in memory until its job finishes, so this
/// bound stops a hostile or accidentally huge dropped file from exhausting
/// memory. The limit is checked from file metadata before any bytes are read.
pub const MAX_STAGED_INPUT_BYTES: u64 = 512 * 1024 * 1024;

/// Checks a staged input's length before it is read into memory.
///
/// Kept separate from the file I/O so the boundary can be unit tested without
/// creating a multi-gigabyte file. `display` is the path shown in the error.
fn check_input_size(size: u64, display: &str) -> Result<(), OperationError> {
    if size > MAX_STAGED_INPUT_BYTES {
        return Err(OperationError::new(format!(
            "staging failed for '{display}': input is {size} bytes, which exceeds the {MAX_STAGED_INPUT_BYTES}-byte limit"
        )));
    }
    Ok(())
}

/// Resolves a relative export target so it cannot escape `--output-dir`.
///
/// Absolute targets and any `..`, root, or drive/UNC prefix component are
/// rejected with a typed [`OperationError`] before a path is recorded. Nested
/// relative names such as `sub/out.pdf` are allowed and resolve underneath the
/// output directory. The same validation runs when no output directory is set,
/// so an action file never records an escaping destination.
fn resolve_export_target(
    output_dir: Option<&Path>,
    target: &str,
) -> Result<String, OperationError> {
    let candidate = Path::new(target);
    if candidate.is_absolute() {
        return Err(OperationError::new(format!(
            "export target '{target}' must be a relative path inside the output directory"
        )));
    }
    for component in candidate.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(OperationError::new(format!(
                "export target '{target}' contains a forbidden path component ('..', root, or drive prefix); it must stay inside the output directory"
            )));
        }
    }
    let resolved = match output_dir {
        Some(dir) => dir.join(candidate),
        None => candidate.to_path_buf(),
    };
    Ok(resolved.to_string_lossy().into_owned())
}

/// Carries the current artifact into the next step with a new message.
fn passthrough(input: &Artifact, message: String) -> OperationOutcome {
    match input {
        Artifact::Bytes(bytes) => OperationOutcome::bytes(bytes.clone(), message),
        Artifact::Path(path) => OperationOutcome::path(path.clone(), message),
        Artifact::Empty => OperationOutcome::empty(message),
    }
}

impl DocumentOperations for EchoOperations {
    fn import(&self, input: &Artifact) -> Result<OperationOutcome, OperationError> {
        let display = match input.as_path() {
            Some(path) => path.to_owned(),
            None => {
                return Err(OperationError::new(
                    "import step requires a path artifact to stage",
                ));
            }
        };
        let path = Path::new(&display);
        if path.is_dir() {
            return Err(OperationError::new(format!(
                "staging failed for '{display}': input is a directory"
            )));
        }
        let metadata = std::fs::metadata(path).map_err(|error| {
            OperationError::new(format!("staging failed for '{display}': {error}"))
        })?;
        check_input_size(metadata.len(), &display)?;
        let bytes = std::fs::read(path).map_err(|error| {
            OperationError::new(format!("staging failed for '{display}': {error}"))
        })?;
        if bytes.is_empty() {
            return Err(OperationError::new(format!(
                "staging failed for '{display}': input is empty"
            )));
        }
        let length = bytes.len();
        let name = path
            .file_name()
            .and_then(|part| part.to_str())
            .unwrap_or(display.as_str());
        Ok(OperationOutcome::bytes(
            bytes,
            format!("staged {name} ({length} bytes)"),
        ))
    }

    fn convert(&self, input: &Artifact, format: &str) -> Result<OperationOutcome, OperationError> {
        Ok(passthrough(
            input,
            format!("convert to '{format}' staged (no mutation)"),
        ))
    }

    fn watermark(&self, input: &Artifact, text: &str) -> Result<OperationOutcome, OperationError> {
        Ok(passthrough(
            input,
            format!("watermark '{text}' staged (no mutation)"),
        ))
    }

    fn apply_security(
        &self,
        input: &Artifact,
        _password: &str,
        _permissions: &str,
    ) -> Result<OperationOutcome, OperationError> {
        Ok(passthrough(
            input,
            "security settings staged (no mutation)".to_owned(),
        ))
    }

    fn export(&self, _input: &Artifact, target: &str) -> Result<OperationOutcome, OperationError> {
        let resolved = resolve_export_target(self.output_dir.as_deref(), target)?;
        Ok(OperationOutcome::path(
            resolved.clone(),
            format!("export staged to '{resolved}' (no write)"),
        ))
    }

    fn run_command(
        &self,
        input: &Artifact,
        name: &str,
        _arguments: &[String],
    ) -> Result<OperationOutcome, OperationError> {
        Ok(passthrough(
            input,
            format!("custom command '{name}' staged (no execution)"),
        ))
    }
}

/// Applies real in-process document operations for `action run --execute`.
///
/// Import validates that the input parses as a PDF, convert keeps PDF bytes,
/// watermark stamps real page content, and export writes the artifact beneath
/// the output directory. Security and custom commands are refused rather than
/// silently staged; those paths stay with their dedicated commands.
#[derive(Debug)]
pub struct RealOperations {
    output_dir: Option<PathBuf>,
}

impl RealOperations {
    /// Creates a real adapter that resolves relative exports against `output_dir`.
    pub fn new(output_dir: Option<PathBuf>) -> Self {
        Self { output_dir }
    }
}

/// Return an artifact's bytes, reading a path artifact from disk.
fn artifact_bytes(input: &Artifact) -> Result<Vec<u8>, OperationError> {
    match input {
        Artifact::Bytes(bytes) => Ok(bytes.clone()),
        Artifact::Path(path) => std::fs::read(path)
            .map_err(|error| OperationError::new(format!("read '{path}': {error}"))),
        Artifact::Empty => Err(OperationError::new(
            "the step has no document artifact to operate on",
        )),
    }
}

impl DocumentOperations for RealOperations {
    fn import(&self, input: &Artifact) -> Result<OperationOutcome, OperationError> {
        let display = input
            .as_path()
            .ok_or_else(|| OperationError::new("import step requires a path artifact"))?;
        let path = Path::new(display);
        if path.is_dir() {
            return Err(OperationError::new(format!(
                "import failed for '{display}': input is a directory"
            )));
        }
        let metadata = std::fs::metadata(path).map_err(|error| {
            OperationError::new(format!("import failed for '{display}': {error}"))
        })?;
        check_input_size(metadata.len(), display)?;
        let bytes = std::fs::read(path).map_err(|error| {
            OperationError::new(format!("import failed for '{display}': {error}"))
        })?;
        pdfxi_core::Document::from_bytes(bytes.clone()).map_err(|error| {
            OperationError::new(format!("import failed for '{display}': not a PDF: {error}"))
        })?;
        let length = bytes.len();
        Ok(OperationOutcome::bytes(
            bytes,
            format!("imported '{display}' ({length} bytes)"),
        ))
    }

    fn convert(&self, input: &Artifact, format: &str) -> Result<OperationOutcome, OperationError> {
        if matches!(format.trim().to_ascii_lowercase().as_str(), "pdf" | "copy") {
            Ok(passthrough(input, "kept PDF bytes".to_owned()))
        } else {
            Err(OperationError::new(format!(
                "convert to '{format}' is not supported by --execute; use the create/export commands"
            )))
        }
    }

    fn watermark(&self, input: &Artifact, text: &str) -> Result<OperationOutcome, OperationError> {
        if text.trim().is_empty() {
            return Err(OperationError::new("watermark text must not be empty"));
        }
        let bytes = artifact_bytes(input)?;
        let document = pdfxi_core::Document::from_bytes(bytes).map_err(|error| {
            OperationError::new(format!("watermark input is not a PDF: {error}"))
        })?;
        let stamped = crate::decorate::apply_default_watermark(&document, text)
            .map_err(|error| OperationError::new(format!("watermark failed: {error}")))?;
        Ok(OperationOutcome::bytes(
            stamped,
            format!("watermarked every page with '{text}'"),
        ))
    }

    fn apply_security(
        &self,
        _input: &Artifact,
        _password: &str,
        _permissions: &str,
    ) -> Result<OperationOutcome, OperationError> {
        Err(OperationError::new(
            "security steps are not supported by --execute; use the pdfxi security commands",
        ))
    }

    fn export(&self, input: &Artifact, target: &str) -> Result<OperationOutcome, OperationError> {
        let resolved = resolve_export_target(self.output_dir.as_deref(), target)?;
        let bytes = artifact_bytes(input)?;
        let path = Path::new(&resolved);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                OperationError::new(format!("create '{}': {error}", parent.display()))
            })?;
        }
        std::fs::write(path, &bytes)
            .map_err(|error| OperationError::new(format!("write '{resolved}': {error}")))?;
        Ok(OperationOutcome::path(
            resolved.clone(),
            format!("exported {} bytes to '{resolved}'", bytes.len()),
        ))
    }

    fn run_command(
        &self,
        _input: &Artifact,
        name: &str,
        _arguments: &[String],
    ) -> Result<OperationOutcome, OperationError> {
        Err(OperationError::new(format!(
            "custom command '{name}' is not supported by --execute"
        )))
    }
}

/// Selects the staging or real document adapter.
enum Operations {
    /// Records outcomes without writing or mutating content.
    Stage(EchoOperations),
    /// Applies real in-process PDF operations.
    Real(RealOperations),
}

impl DocumentOperations for Operations {
    fn import(&self, input: &Artifact) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.import(input),
            Self::Real(operations) => operations.import(input),
        }
    }

    fn convert(&self, input: &Artifact, format: &str) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.convert(input, format),
            Self::Real(operations) => operations.convert(input, format),
        }
    }

    fn watermark(&self, input: &Artifact, text: &str) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.watermark(input, text),
            Self::Real(operations) => operations.watermark(input, text),
        }
    }

    fn apply_security(
        &self,
        input: &Artifact,
        password: &str,
        permissions: &str,
    ) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.apply_security(input, password, permissions),
            Self::Real(operations) => operations.apply_security(input, password, permissions),
        }
    }

    fn export(&self, input: &Artifact, target: &str) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.export(input, target),
            Self::Real(operations) => operations.export(input, target),
        }
    }

    fn run_command(
        &self,
        input: &Artifact,
        name: &str,
        arguments: &[String],
    ) -> Result<OperationOutcome, OperationError> {
        match self {
            Self::Stage(operations) => operations.run_command(input, name, arguments),
            Self::Real(operations) => operations.run_command(input, name, arguments),
        }
    }
}

/// Stable JSON shape of a `run` or `droplet` result.
#[derive(Debug, Serialize)]
struct RunOutput {
    /// The name of the action that was run.
    action: String,
    /// One entry per input, in input order.
    jobs: Vec<JobOutput>,
}

/// One file's recorded result.
#[derive(Debug, Serialize)]
struct JobOutput {
    /// Deterministic job id (`job-0`, `job-1`, ...).
    id: String,
    /// Input path handled by this job.
    input: String,
    /// Terminal job state, for example `succeeded` or `failed`.
    state: String,
    /// The per-file outcome details.
    outcome: OutcomeOutput,
}

/// The per-file outcome of a finished job.
#[derive(Debug, Serialize)]
struct OutcomeOutput {
    /// Terminal outcome state.
    state: String,
    /// Ids of steps that completed, in execution order.
    completed_steps: Vec<String>,
    /// The failing step id, when the job failed.
    failed_step: Option<String>,
    /// The failure message, when the job failed.
    message: Option<String>,
    /// Stable one-line summary used by the text output.
    summary: String,
    /// Per-step results recorded by the executor.
    steps: Vec<StepOutput>,
}

/// One step result recorded by the executor.
#[derive(Debug, Serialize)]
struct StepOutput {
    /// Id of the step that produced the result.
    step_id: String,
    /// Stable step-kind string.
    kind: String,
    /// Short message from the operation.
    message: String,
    /// Artifact discriminator: `bytes`, `path`, or `empty`.
    artifact_kind: String,
    /// Deterministic rendering of the produced artifact.
    artifact: String,
}

/// Loads and parses an action file with contextual errors.
fn load_library(file: &Path) -> anyhow::Result<ActionLibrary> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read action file '{}'", file.display()))?;
    ActionLibrary::from_text(&text)
        .with_context(|| format!("invalid action file '{}'", file.display()))
}

/// Renders available action names into a deterministic, readable list.
fn available_names(library: &ActionLibrary) -> String {
    let names: Vec<&str> = library.names().collect();
    if names.is_empty() {
        "<none>".to_owned()
    } else {
        names.join(", ")
    }
}

/// Collapses control whitespace so one record always stays on one output line.
fn one_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

/// Renders an artifact deterministically without exposing document bytes.
fn artifact_text(artifact: &Artifact) -> String {
    match artifact {
        Artifact::Bytes(bytes) => format!("bytes({})", bytes.len()),
        Artifact::Path(path) => path.clone(),
        Artifact::Empty => "empty".to_owned(),
    }
}

/// Converts a finished [`BatchJob`] into the stable JSON/text projection.
fn build_job_output(job: &BatchJob) -> JobOutput {
    let outcome = job.outcome();
    let steps: Vec<StepOutput> = outcome
        .map(|record| {
            record
                .step_results()
                .iter()
                .map(|result| StepOutput {
                    step_id: result.step_id().to_owned(),
                    kind: result.kind().as_str().to_owned(),
                    message: result.message().to_owned(),
                    artifact_kind: result.artifact().kind_str().to_owned(),
                    artifact: artifact_text(result.artifact()),
                })
                .collect()
        })
        .unwrap_or_default();
    JobOutput {
        id: job.id().to_owned(),
        input: job.input().to_owned(),
        state: job.state().as_str().to_owned(),
        outcome: OutcomeOutput {
            state: outcome
                .map(|record| record.state().as_str().to_owned())
                .unwrap_or_else(|| job.state().as_str().to_owned()),
            completed_steps: outcome
                .map(|record| record.completed_steps().to_vec())
                .unwrap_or_default(),
            failed_step: outcome
                .and_then(|record| record.failed_step())
                .map(str::to_owned),
            message: outcome
                .and_then(|record| record.message())
                .map(str::to_owned),
            summary: outcome
                .map(|record| record.summary())
                .unwrap_or_else(|| format!("{} steps=0", job.state().as_str())),
            steps,
        },
    }
}

/// Prints every action in `file` as `name<TAB>step-count<TAB>description`.
pub fn list_actions(file: &Path) -> anyhow::Result<()> {
    let library = load_library(file)?;
    for action in library.actions() {
        println!(
            "{}\t{}\t{}",
            action.name(),
            action.len(),
            one_line(action.description())
        );
    }
    Ok(())
}

/// Prints the name, description, and ordered steps of one named action.
pub fn show_action(file: &Path, name: &str) -> anyhow::Result<()> {
    let library = load_library(file)?;
    let action = library.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "no action named '{name}' in '{}'; available actions: {}",
            file.display(),
            available_names(&library)
        )
    })?;
    println!("name: {}", action.name());
    println!("description: {}", one_line(action.description()));
    println!("steps: {}", action.len());
    for (index, step) in action.steps().enumerate() {
        println!("{} {} {}", index + 1, step.id(), step.kind().as_str());
        let parameters: Vec<String> = step
            .parameters()
            .map(|(key, value)| format!("{key}={}", one_line(value)))
            .collect();
        if !parameters.is_empty() {
            println!("    params: {}", parameters.join(" "));
        }
    }
    Ok(())
}

/// The result of one staged batch run plus its terminal counts.
struct RunReport {
    /// Stable JSON/text projection of every job.
    output: RunOutput,
    /// Number of jobs that failed or were cancelled.
    failed: usize,
    /// Total number of jobs that ran.
    total: usize,
}

/// Validates inputs and runs an already-loaded action through the staging
/// executor, returning the deterministic report without printing anything.
///
/// This is the shared implementation behind `action run`, `droplet`, and the
/// JSON API's `run-batch`/`run-toolset` commands. Failed jobs are reported in
/// the returned outcome rather than as an error so callers decide how to
/// surface them.
fn build_run_report(
    action: &Action,
    inputs: &[PathBuf],
    output_dir: Option<&Path>,
    execute: bool,
) -> anyhow::Result<RunReport> {
    if inputs.is_empty() {
        bail!("at least one --input path is required; no jobs were run");
    }
    for input in inputs {
        if !input.exists() {
            bail!(
                "input path '{}' does not exist; no jobs were run",
                input.display()
            );
        }
    }

    let operations = if execute {
        Operations::Real(RealOperations::new(output_dir.map(Path::to_path_buf)))
    } else {
        Operations::Stage(EchoOperations::new(output_dir.map(Path::to_path_buf)))
    };
    let executor = DocumentExecutor::new(operations);
    let mut queue = BatchQueue::new();
    for (index, input) in inputs.iter().enumerate() {
        queue.enqueue_job(
            format!("job-{index}"),
            input.display().to_string(),
            action.clone(),
        )?;
    }
    queue.run_with(&executor);

    let failed = queue
        .jobs()
        .filter(|job| matches!(job.state(), JobState::Failed | JobState::Cancelled))
        .count();
    let jobs: Vec<JobOutput> = queue.jobs().map(build_job_output).collect();
    let total = jobs.len();
    Ok(RunReport {
        output: RunOutput {
            action: action.name().to_owned(),
            jobs,
        },
        failed,
        total,
    })
}

/// Runs a loaded action and returns its stable JSON projection as one line.
///
/// Used by the `pdfxi api` host so the API reuses the exact same executor and
/// outcome shape as `action run --json`.
pub fn run_loaded_action_json(
    action: &Action,
    inputs: &[PathBuf],
    output_dir: Option<&Path>,
) -> anyhow::Result<String> {
    let report = build_run_report(action, inputs, output_dir, false)?;
    Ok(serde_json::to_string(&report.output)?)
}

/// Validates inputs, runs a named action, and prints deterministic outcomes.
///
/// The action file, action name, action structure, and every input path are
/// checked before any job is created or executed. Jobs then run in input order
/// through [`DocumentExecutor`] driven by [`BatchQueue::run_with`]. One failing
/// input is isolated to its own outcome. Text mode prints one
/// `id<TAB>input<TAB>state<TAB>summary` line per job; `--json` prints the stable
/// [`RunOutput`] shape. In both modes the command exits non-zero when any job
/// failed.
pub fn run_action(
    file: &Path,
    name: &str,
    inputs: &[PathBuf],
    output_dir: Option<&Path>,
    execute: bool,
    json: bool,
) -> anyhow::Result<()> {
    let library = load_library(file)?;
    let action = library.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "no action named '{name}' in '{}'; available actions: {}",
            file.display(),
            available_names(&library)
        )
    })?;
    action
        .validate()
        .with_context(|| format!("action '{name}' in '{}' failed validation", file.display()))?;

    let report = build_run_report(action, inputs, output_dir, execute)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report.output)?);
    } else {
        for job in &report.output.jobs {
            println!(
                "{}\t{}\t{}\t{}",
                job.id, job.input, job.state, job.outcome.summary
            );
        }
    }

    if report.failed > 0 {
        bail!("{} of {} job(s) failed", report.failed, report.total);
    }
    Ok(())
}

/// JSON shape of `pdfxi action create`.
#[derive(Debug, Serialize)]
struct CreateActionReport {
    /// Written action file.
    file: String,
    /// Action name.
    name: String,
    /// Description.
    description: String,
    /// Step ids in order.
    steps: Vec<String>,
    /// Bytes written.
    bytes: usize,
}

/// `pdfxi action create`: write one validated tab-delimited action file.
///
/// `steps` entries use `ID:KIND`; `params` entries use `ID:KEY=VALUE`. The
/// action is validated before anything is written.
pub(crate) fn create_action(
    file: &Path,
    name: &str,
    description: &str,
    steps: &[String],
    params: &[String],
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    if steps.is_empty() {
        bail!("pass at least one --step ID:KIND");
    }
    if file.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            file.display()
        );
    }
    let mut by_step: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for text in params {
        let (id, assignment) = text
            .split_once(':')
            .with_context(|| format!("parameter {text:?} must be ID:KEY=VALUE"))?;
        let (key, value) = assignment
            .split_once('=')
            .with_context(|| format!("parameter {text:?} must be ID:KEY=VALUE"))?;
        if key.is_empty() {
            bail!("parameter {text:?} has an empty key");
        }
        by_step
            .entry(id.to_owned())
            .or_default()
            .push((key.to_owned(), value.to_owned()));
    }
    let mut action = Action::new(name, description);
    for text in steps {
        let (id, kind) = text
            .split_once(':')
            .with_context(|| format!("step {text:?} must be ID:KIND"))?;
        if id.is_empty() {
            bail!("step {text:?} has an empty id");
        }
        let kind: StepKind = kind
            .parse()
            .with_context(|| format!("step {text:?} has an unknown kind"))?;
        let mut step = ActionStep::new(id, kind);
        if let Some(values) = by_step.remove(id) {
            for (key, value) in values {
                step.add_parameter(key, value);
            }
        }
        action.add_step(step)?;
    }
    if let Some((id, _)) = by_step.into_iter().next() {
        bail!("parameter names an unknown step {id:?}");
    }
    action
        .validate()
        .map_err(|error| anyhow::anyhow!("action validation failed: {error}"))?;
    let text = action.to_text();
    let bytes = text.len();
    if let Some(parent) = file.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    std::fs::write(file, text).with_context(|| format!("write {}", file.display()))?;

    let report = CreateActionReport {
        file: file.display().to_string(),
        name: action.name().to_owned(),
        description: action.description().to_owned(),
        steps: action.steps().map(|step| step.id().to_owned()).collect(),
        bytes,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("action: {}  steps: {}", report.name, report.steps.len());
        println!("bytes: {}", report.bytes);
    }
    Ok(())
}

/// Runs a named action with parameter defaults applied to steps that lack them.
///
/// Shared by `pdfxi toolset run`; the caller has already checked the action for
/// dangerous step kinds.
pub fn run_action_with_defaults(
    file: &Path,
    name: &str,
    defaults: &[(String, String)],
    inputs: &[PathBuf],
    output_dir: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let library = load_library(file)?;
    let mut action = library
        .get(name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no action named '{name}' in '{}'; available actions: {}",
                file.display(),
                available_names(&library)
            )
        })?
        .clone();
    action
        .validate()
        .with_context(|| format!("action '{name}' in '{}' failed validation", file.display()))?;
    action.apply_defaults(defaults);

    let report = build_run_report(&action, inputs, output_dir, false)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report.output)?);
    } else {
        for job in &report.output.jobs {
            println!(
                "{}\t{}\t{}\t{}",
                job.id, job.input, job.state, job.outcome.summary
            );
        }
    }
    if report.failed > 0 {
        bail!("{} of {} job(s) failed", report.failed, report.total);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_size_limit_checks_the_boundary_without_a_large_file() {
        assert!(check_input_size(0, "empty.pdf").is_ok());
        assert!(check_input_size(MAX_STAGED_INPUT_BYTES, "exact.pdf").is_ok());
        let error = check_input_size(MAX_STAGED_INPUT_BYTES + 1, "huge.pdf")
            .expect_err("one byte over the limit must be rejected");
        let message = error.message();
        assert!(message.contains("huge.pdf"), "{message}");
        assert!(message.contains("exceeds"), "{message}");
        assert!(message.contains("536870912"), "{message}");
    }

    #[test]
    fn export_target_resolver_rejects_traversal_and_absolute_targets() {
        let dir = Path::new("C:\\staging");
        let error = resolve_export_target(Some(dir), "../escape.pdf")
            .expect_err("parent traversal must be rejected");
        assert!(error.message().contains("forbidden path component"));

        let absolute = std::env::temp_dir()
            .join("pdfxi-review-absolute.pdf")
            .to_string_lossy()
            .into_owned();
        let error = resolve_export_target(Some(dir), &absolute)
            .expect_err("an absolute target must be rejected");
        assert!(
            error.message().contains("relative path"),
            "{}",
            error.message()
        );

        #[cfg(windows)]
        {
            let rooted = "\\escape.pdf";
            let error = resolve_export_target(Some(dir), rooted)
                .expect_err("a rooted target must be rejected");
            assert!(error.message().contains("forbidden path component"));
        }
    }

    #[test]
    fn export_target_resolver_keeps_relative_names_under_the_output_dir() {
        let dir = Path::new("C:\\staging");
        let plain = resolve_export_target(Some(dir), "out.pdf").expect("relative target");
        assert_eq!(plain, dir.join("out.pdf").to_string_lossy().into_owned());
        let nested = resolve_export_target(Some(dir), "sub/out.pdf").expect("nested target");
        assert_eq!(
            nested,
            dir.join("sub/out.pdf").to_string_lossy().into_owned()
        );
        let bare = resolve_export_target(None, "out.pdf").expect("bare relative target");
        assert_eq!(bare, "out.pdf");
    }
}
