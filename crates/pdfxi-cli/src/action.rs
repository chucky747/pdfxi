//! Action-file commands: `list`, `show`, `run`, and the `droplet` entry point.
//!
//! ## A staging-only executor (no PDF mutation yet)
//!
//! [`EchoOperations`] implements `pdfxi_automation::DocumentOperations` as a
//! deliberately side-effect-free adapter. The `import` step copies the staged
//! input bytes into memory and the other steps pass the current artifact along
//! while recording a short, deterministic message. Export resolves the target
//! path against `--output-dir` but writes nothing.
//!
//! This is intentional at this milestone. Saving, watermarking, encrypting, and
//! converting real PDFs are owned by other lanes and are not yet exposed to the
//! CLI; wiring them in here would couple automation to in-flight parsers and
//! writers and make runs depend on document content. Staging keeps `action run`
//! deterministic, safe on hostile inputs, and easy to script or assert against,
//! while still exercising the full `DocumentExecutor` / `BatchQueue` pipeline
//! (validation, ordering, failure isolation, cancellation, per-step results).
//! Replacing `EchoOperations` with a real `DocumentOperations` implementation
//! later requires no change to the commands or their output shapes.
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
    ActionLibrary, Artifact, BatchJob, BatchQueue, DocumentExecutor, DocumentOperations, JobState,
    OperationError, OperationOutcome,
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

    let operations = EchoOperations::new(output_dir.map(Path::to_path_buf));
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

    if json {
        let output = RunOutput {
            action: name.to_owned(),
            jobs,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for job in &jobs {
            println!(
                "{}\t{}\t{}\t{}",
                job.id, job.input, job.state, job.outcome.summary
            );
        }
    }

    if failed > 0 {
        bail!("{failed} of {total} job(s) failed");
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
