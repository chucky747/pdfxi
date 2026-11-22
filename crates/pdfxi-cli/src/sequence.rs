//! `pdfxi action sequence`: real document execution of Action Wizard sequences.
//!
//! [`pdfxi_automation::SequenceBatchQueue`] runs an [`ActionSequence`] per input
//! file through an injected [`SequenceStepRunner`], but its default runner is
//! deliberately side-effect free. This host supplies a document runner: every
//! input is parsed as a real PDF, each step mutates that in-memory document
//! (metadata writes, sandboxed scripts and the documented document commands),
//! and every succeeded item is saved to a real output PDF that is reparsed to
//! prove the result. Failures and cancellations stay isolated to their item.
//!
//! No network, process launch or ambient state is involved; script steps run in
//! the existing deterministic sandbox with the sequence's own budget.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use pdfxi_automation::{
    ActionSequence, CancelToken, SandboxedScriptExecutor, ScriptBudget, ScriptStepExecutor,
    SequenceBatchItem, SequenceBatchQueue, SequenceBatchState, SequenceStepRunner, WizardStep,
    WizardStepKind,
};
use pdfxi_core::{Document, DocumentInfo, parse_pdf_date};
use serde::Serialize;

/// Largest input accepted, matching the action importer's own limit.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

/// Document commands implemented by this host.
const DOCUMENT_COMMANDS: [&str; 2] = ["strip-metadata", "normalize-boxes"];

/// One step as written on the command line.
enum StepSpec {
    /// A sandboxed script.
    Script(String),
    /// One `/Info` metadata key/value pair.
    Metadata(String, String),
    /// One named document command.
    Command(String),
}

/// Parses one `--step` token.
///
/// Accepted forms are `script:SOURCE`, `metadata:KEY=VALUE` and
/// `command:NAME`.
fn parse_step(token: &str) -> anyhow::Result<StepSpec> {
    if let Some(source) = token.strip_prefix("script:") {
        if source.trim().is_empty() {
            bail!("script step has an empty source");
        }
        return Ok(StepSpec::Script(source.to_owned()));
    }
    if let Some(rest) = token.strip_prefix("metadata:") {
        let (key, value) = rest
            .split_once('=')
            .with_context(|| format!("metadata step {rest:?} must be KEY=VALUE"))?;
        if key.trim().is_empty() {
            bail!("metadata step has an empty key");
        }
        return Ok(StepSpec::Metadata(key.to_owned(), value.to_owned()));
    }
    if let Some(name) = token.strip_prefix("command:") {
        if name.trim().is_empty() {
            bail!("command step has an empty name");
        }
        return Ok(StepSpec::Command(name.to_owned()));
    }
    bail!("unknown step {token:?}; expected script:, metadata:KEY=VALUE or command:NAME")
}

/// Builds a validated sequence from ordered `--step` tokens.
///
/// Step ids are `step-1`, `step-2`, … in command-line order.
fn build_sequence(tokens: &[String]) -> anyhow::Result<ActionSequence> {
    if tokens.is_empty() {
        bail!("pass at least one --step");
    }
    let mut sequence = ActionSequence::new("cli-sequence");
    for (index, token) in tokens.iter().enumerate() {
        let id = format!("step-{}", index + 1);
        let step = match parse_step(token)? {
            StepSpec::Script(source) => WizardStep::script(id, source),
            StepSpec::Metadata(key, value) => WizardStep::set_metadata(id, key, value),
            StepSpec::Command(name) => WizardStep::run_command(id, name),
        };
        sequence
            .add_step(step)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    }
    sequence
        .validate()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(sequence)
}

/// One parsed document plus the `/Info` state the steps have built so far.
///
/// [`Document::info`] reads the original trailer, not pending
/// [`Document::set_info`] writes, so consecutive metadata steps must accumulate
/// against this mirror instead of re-reading the document.
#[derive(Debug)]
struct WorkingDocument {
    /// The mutable document.
    document: Document,
    /// Current `/Info` values, seeded from the source document.
    info: DocumentInfo,
}

impl WorkingDocument {
    /// Wraps one parsed document, reading its initial metadata.
    fn new(document: Document) -> Result<Self, String> {
        let info = document
            .info()
            .map_err(|error| format!("read Info: {error}"))?
            .unwrap_or_default();
        Ok(Self { document, info })
    }

    /// Writes one `/Info` field from a `metadata:KEY=VALUE` step.
    ///
    /// Keys are case-insensitive aliases of the standard [`DocumentInfo`]
    /// fields.
    fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => self.info.title = Some(value.to_owned()),
            "author" => self.info.author = Some(value.to_owned()),
            "subject" => self.info.subject = Some(value.to_owned()),
            "keywords" => self.info.keywords = Some(value.to_owned()),
            "creator" => self.info.creator = Some(value.to_owned()),
            "producer" => self.info.producer = Some(value.to_owned()),
            "creation-date" | "creationdate" | "creation_date" => {
                self.info.creation_date = Some(parse_pdf_date(value));
            }
            "mod-date" | "moddate" | "modification-date" | "modification_date" => {
                self.info.modification_date = Some(parse_pdf_date(value));
            }
            other => {
                return Err(format!(
                    "unknown metadata key {other:?}; expected title, author, subject, keywords, \
                     creator, producer, creation-date or mod-date"
                ));
            }
        }
        self.document
            .set_info(&self.info)
            .map_err(|error| format!("write Info: {error}"))
    }

    /// Applies one documented document command.
    fn run_command(&mut self, command: &str) -> Result<(), String> {
        match command.trim().to_ascii_lowercase().as_str() {
            "strip-metadata" => {
                self.info = DocumentInfo::default();
                self.document
                    .set_info(&self.info)
                    .map_err(|error| format!("strip metadata: {error}"))
            }
            "normalize-boxes" => {
                let pages = self
                    .document
                    .page_count()
                    .map_err(|error| format!("count pages: {error}"))?;
                for index in 0..pages {
                    let boxes = self
                        .document
                        .page_boxes(index)
                        .map_err(|error| format!("page {index} boxes: {error}"))?
                        .normalized()
                        .map_err(|error| format!("page {index} normalize: {error}"))?;
                    self.document
                        .set_page_boxes(index, &boxes)
                        .map_err(|error| format!("page {index} set boxes: {error}"))?;
                }
                Ok(())
            }
            other => Err(format!(
                "command {other:?} is not allowed; supported commands: {}",
                DOCUMENT_COMMANDS.join(", ")
            )),
        }
    }
}

/// Shared working state between the batch queue and its runner.
#[derive(Debug, Default)]
struct RunState {
    /// Parsed documents keyed by the input string the queue carries.
    documents: Mutex<BTreeMap<String, WorkingDocument>>,
    /// Last failure message per input, for the report.
    failures: Mutex<BTreeMap<String, String>>,
}

/// A [`SequenceStepRunner`] that mutates real parsed documents.
#[derive(Debug)]
struct DocumentRunner {
    /// Working documents and failures.
    state: Arc<RunState>,
    /// Budget applied to every script step.
    budget: ScriptBudget,
}

impl DocumentRunner {
    /// Creates a runner over already parsed documents.
    fn new(state: Arc<RunState>, budget: ScriptBudget) -> Self {
        Self { state, budget }
    }

    /// Applies one step to the item's document.
    fn apply(&self, input: &str, step: &WizardStep, cancel: &CancelToken) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("item was cancelled".to_owned());
        }
        let mut documents = self
            .state
            .documents
            .lock()
            .map_err(|_| "document runner lock is poisoned".to_owned())?;
        let item = documents
            .get_mut(input)
            .ok_or_else(|| format!("unknown input {input:?}"))?;
        match step.kind() {
            WizardStepKind::RunScript { script } => SandboxedScriptExecutor
                .run_script(step.id(), script, &self.budget, cancel)
                .map(|_| ())
                .map_err(|error| error.to_string()),
            WizardStepKind::SetMetadata { key, value } => item.set_metadata(key, value),
            WizardStepKind::RunCommand { command } => item.run_command(command),
            WizardStepKind::Unsupported { name } => Err(format!("unsupported step kind {name:?}")),
        }
    }
}

impl SequenceStepRunner for DocumentRunner {
    fn run_step(&self, input: &str, step: &WizardStep, cancel: &CancelToken) -> Result<(), String> {
        let result = self.apply(input, step, cancel);
        if let Err(message) = &result
            && let Ok(mut failures) = self.state.failures.lock()
        {
            failures.insert(input.to_owned(), message.clone());
        }
        result
    }
}

/// One job row in the stable report.
#[derive(Debug, Serialize)]
struct JobRow {
    /// Queue item id.
    id: String,
    /// Input path.
    input: String,
    /// Terminal state name.
    state: &'static str,
    /// Completed step ids in order.
    steps_completed: Vec<String>,
    /// Failing step id, when the item failed.
    failed_step: Option<String>,
    /// Failure message, when the item failed.
    failure: Option<String>,
    /// Written output path, when the item succeeded.
    output: Option<String>,
    /// Written output size in bytes.
    bytes: Option<usize>,
    /// Page count of the reparsed output.
    pages: Option<usize>,
}

/// Stable JSON projection of `pdfxi action sequence`.
#[derive(Debug, Serialize)]
struct SequenceJson {
    /// Sequence name.
    sequence: String,
    /// Step tokens in execution order.
    steps: Vec<String>,
    /// One row per input.
    jobs: Vec<JobRow>,
    /// Stable one-line-per-item summary.
    summary: String,
    /// Number of failed or cancelled items.
    failed: usize,
    /// Number of items.
    total: usize,
}

/// Chooses the output path for one input.
///
/// `--out-dir` wins; otherwise the file stays next to its input as
/// `<stem>.sequence.pdf`, so the input itself is never overwritten.
fn output_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_owned());
    let name = format!("{stem}.sequence.pdf");
    match out_dir {
        Some(dir) => dir.join(name),
        None => input.with_file_name(name),
    }
}

/// Validates inputs and parses each one before any job runs.
fn load_documents(inputs: &[PathBuf]) -> anyhow::Result<BTreeMap<String, Document>> {
    if inputs.is_empty() {
        bail!("at least one --input path is required; no jobs were run");
    }
    let mut documents = BTreeMap::new();
    for input in inputs {
        let display = input.display().to_string();
        let metadata = std::fs::metadata(input)
            .with_context(|| format!("input '{display}' is not readable"))?;
        if !metadata.is_file() {
            bail!("input '{display}' is not a file");
        }
        if metadata.len() > MAX_INPUT_BYTES {
            bail!(
                "input '{display}' is {} bytes; the limit is {MAX_INPUT_BYTES}",
                metadata.len()
            );
        }
        if documents.contains_key(&display) {
            bail!("input '{display}' was given more than once");
        }
        let bytes = std::fs::read(input).with_context(|| format!("read '{display}'"))?;
        let document = Document::from_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("parse '{display}': {error}"))?;
        document
            .page_count()
            .map_err(|error| anyhow::anyhow!("count '{display}' pages: {error}"))?;
        documents.insert(display, document);
    }
    Ok(documents)
}

/// Runs one sequence over every input and writes one output PDF per success.
///
/// All inputs are validated and parsed before the first job runs. The queue
/// then executes items in input order; one failing item never stops its
/// siblings. Each succeeded item is saved, reparsed and reported, and the
/// command exits non-zero when any item failed or was cancelled.
pub(crate) fn run_sequence_batch(
    inputs: &[PathBuf],
    step_tokens: &[String],
    out_dir: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let sequence = build_sequence(step_tokens)?;
    let documents = load_documents(inputs)?;
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create output directory '{}'", dir.display()))?;
    }

    let state = Arc::new(RunState::default());
    {
        let mut working = state
            .documents
            .lock()
            .map_err(|_| anyhow::anyhow!("document runner lock is poisoned"))?;
        for (input, document) in documents {
            let item = WorkingDocument::new(document)
                .map_err(|error| anyhow::anyhow!("'{input}': {error}"))?;
            working.insert(input, item);
        }
    }
    let runner = DocumentRunner::new(Arc::clone(&state), sequence.budget());
    let mut queue = SequenceBatchQueue::with_runner(Box::new(runner));
    for (index, input) in inputs.iter().enumerate() {
        queue.enqueue(SequenceBatchItem::new(
            format!("job-{index}"),
            input.display().to_string(),
            sequence.clone(),
        ))?;
    }
    queue.run_all();
    let report = queue.report();

    let failures = state
        .failures
        .lock()
        .map_err(|_| anyhow::anyhow!("failure log lock is poisoned"))?
        .clone();
    let working = state
        .documents
        .lock()
        .map_err(|_| anyhow::anyhow!("document runner lock is poisoned"))?;
    let mut jobs = Vec::with_capacity(report.entries().len());
    let mut failed = 0;
    for entry in report.entries() {
        let succeeded = entry.state() == SequenceBatchState::Succeeded;
        if !succeeded {
            failed += 1;
        }
        let mut output = None;
        let mut bytes = None;
        let mut pages = None;
        if succeeded {
            let item = working.get(entry.input()).ok_or_else(|| {
                anyhow::anyhow!("input '{}' disappeared from the working set", entry.input())
            })?;
            let saved = item
                .document
                .save()
                .with_context(|| format!("save '{}'", entry.input()))?;
            let path = output_path(Path::new(entry.input()), out_dir);
            std::fs::write(&path, &saved).with_context(|| format!("write '{}'", path.display()))?;
            let written =
                std::fs::read(&path).with_context(|| format!("read back '{}'", path.display()))?;
            let reparsed = Document::from_bytes(written)
                .map_err(|error| anyhow::anyhow!("reparse '{}': {error}", path.display()))?;
            pages =
                Some(reparsed.page_count().map_err(|error| {
                    anyhow::anyhow!("count '{}' pages: {error}", path.display())
                })?);
            bytes = Some(saved.len());
            output = Some(path.display().to_string());
        }
        jobs.push(JobRow {
            id: entry.id().to_owned(),
            input: entry.input().to_owned(),
            state: match entry.state() {
                SequenceBatchState::Queued => "queued",
                SequenceBatchState::Running => "running",
                SequenceBatchState::Succeeded => "succeeded",
                SequenceBatchState::Failed => "failed",
                SequenceBatchState::Cancelled => "cancelled",
            },
            steps_completed: entry.completed_steps().to_vec(),
            failed_step: entry.failed_step().map(str::to_owned),
            failure: failures.get(entry.input()).cloned(),
            output,
            bytes,
            pages,
        });
    }
    let total = jobs.len();
    let summary = report.summary();

    if json {
        let payload = SequenceJson {
            sequence: sequence.name().to_owned(),
            steps: step_tokens.to_vec(),
            jobs,
            summary,
            failed,
            total,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        for job in &jobs {
            let mut line = format!(
                "{}\t{}\t{}\tsteps={}",
                job.id,
                job.input,
                job.state,
                job.steps_completed.len()
            );
            if let Some(step) = &job.failed_step {
                line.push_str(&format!(" at={step}"));
            }
            if let Some(failure) = &job.failure {
                line.push_str(&format!(" error={failure}"));
            }
            println!("{line}");
            if let Some(output) = &job.output {
                println!("\tout: {output} ({} bytes)", job.bytes.unwrap_or(0));
            }
        }
        println!("{summary}");
    }
    if failed > 0 {
        bail!("{failed} of {total} job(s) failed");
    }
    Ok(())
}
