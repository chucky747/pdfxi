//! Workspace automation for PDFXI.
//!
//! `xtask` is the single entry point for verification, claims, the task board,
//! and the goal gate. Agents call it instead of inventing ad-hoc scripts.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const REQUIRED_M0_PATHS: &[&str] = &[
    "AGENTS.md",
    "RULES.md",
    ".gitignore",
    "Cargo.toml",
    "rust-toolchain.toml",
    "docs/GOAL.md",
    "docs/PLAN.md",
    "docs/ARCHITECTURE.md",
    "docs/FEATURES.md",
    "docs/features.toml",
    "docs/ORCHESTRATION.md",
    "docs/TESTING.md",
    "docs/MEMORY.md",
    "opencode.json",
    ".opencode/agents/orchestrator.md",
    "orchestration/BOARD.md",
    "scripts/goal.ps1",
];

const ALLOWED_STATUS: &[&str] = &["planned", "in-progress", "verified", "replaced", "deferred"];

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "PDFXI workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Format, lint, test, and document the workspace.
    Verify {
        /// Skip the slower documentation build.
        #[arg(long)]
        no_docs: bool,
    },
    /// Verify a milestone or the whole goal contract.
    GoalVerify {
        /// Milestone to verify, for example M0.
        #[arg(long)]
        milestone: Option<String>,
        /// Verify the complete goal.
        #[arg(long)]
        all: bool,
        /// Skip the clean-worktree check (development only).
        #[arg(long)]
        allow_dirty: bool,
    },
    /// Validate the feature matrix.
    Features {
        #[command(subcommand)]
        command: FeatureCommand,
    },
    /// Claim a task for a lane.
    Claim {
        task: String,
        #[arg(long)]
        lane: String,
    },
    /// Release a task claim.
    Release { task: String },
    /// Regenerate orchestration/BOARD.md from task files.
    Board,
    /// Check documentation links and required pages.
    Docs,
    /// Run fast benchmarks. M0 is a no-op.
    Bench {
        /// Gate on regression thresholds.
        #[arg(long)]
        check: bool,
    },
    /// Run the smoke corpus. M0 is a no-op.
    Corpus {
        #[command(subcommand)]
        command: CorpusCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FeatureCommand {
    /// Validate every feature row.
    Verify,
    /// Merge lane fragments into docs/features.toml.
    Apply {
        /// Remove successfully applied fragments.
        #[arg(long)]
        clean: bool,
        /// Apply only the fragment for this task ID.
        #[arg(long)]
        task: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum CorpusCommand {
    /// Run the smoke corpus.
    Smoke,
}

#[derive(Debug, Deserialize, Serialize)]
struct Task {
    id: String,
    title: String,
    lane: String,
    status: String,
    priority: String,
    #[serde(default)]
    owns: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    acceptance: String,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Claim {
    task: String,
    lane: String,
    owns: Vec<String>,
    created_unix: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct FeatureRow {
    id: String,
    domain: String,
    title: String,
    priority: String,
    status: String,
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default)]
    docs: Option<toml::Value>,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Deserialize)]
struct FeatureFile {
    #[serde(default)]
    feature: Vec<FeatureRow>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify { no_docs } => verify(no_docs),
        Command::GoalVerify {
            milestone,
            all,
            allow_dirty,
        } => goal_verify(milestone.as_deref(), all, allow_dirty),
        Command::Features { command } => match command {
            FeatureCommand::Verify => features_verify(),
            FeatureCommand::Apply { clean, task } => features_apply(clean, task.as_deref()),
        },
        Command::Claim { task, lane } => claim(&task, &lane),
        Command::Release { task } => release(&task),
        Command::Board => board(),
        Command::Docs => docs(),
        Command::Bench { check } => bench(check),
        Command::Corpus { command } => match command {
            CorpusCommand::Smoke => corpus_smoke(),
        },
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    println!("$ {} {}", program, args.join(" "));
    let status = ProcessCommand::new(program)
        .args(args)
        .current_dir(root())
        .status()
        .with_context(|| format!("failed to start {program}"))?;
    ensure_status(program, status)
}

fn ensure_status(program: &str, status: ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{program} failed with {status}")
    }
}

fn verify(no_docs: bool) -> Result<()> {
    run("cargo", &["fmt", "--all", "--check"])?;
    run(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run("cargo", &["test", "--workspace"])?;
    if !no_docs {
        run("cargo", &["doc", "--workspace", "--no-deps"])?;
    }
    features_verify()?;
    println!("VERIFY OK");
    Ok(())
}

fn goal_verify(milestone: Option<&str>, all: bool, allow_dirty: bool) -> Result<()> {
    if !all && milestone.is_none() {
        bail!("pass --milestone M0 or --all");
    }
    if all && milestone.is_some() {
        bail!("--all and --milestone are mutually exclusive");
    }

    for path in REQUIRED_M0_PATHS {
        if !root().join(path).exists() {
            bail!("required path missing: {path}");
        }
    }
    features_verify()?;

    if let Some(milestone) = milestone {
        if !matches!(milestone, "M0" | "M1") {
            bail!(
                "milestone {milestone} is not implemented yet; only M0 and M1 are available in this build"
            );
        }
        if milestone == "M1" {
            let rows = load_features()?;
            let required = [
                "STD-011", "STD-012", "AUTO-009", "AUTO-010", "VIEW-002", "VIEW-003", "VIEW-016",
            ];
            let missing: Vec<&str> = required
                .iter()
                .copied()
                .filter(|id| {
                    !rows
                        .iter()
                        .any(|row| row.id == *id && row.status == "verified")
                })
                .collect();
            if !missing.is_empty() {
                bail!(
                    "M1 requires these feature rows to be verified: {}",
                    missing.join(", ")
                );
            }
        }
        run("cargo", &["fmt", "--all", "--check"])?;
        run("cargo", &["test", "--workspace"])?;
        run("cargo", &["doc", "--workspace", "--no-deps"])?;
        if !allow_dirty {
            require_clean_worktree()?;
        }
        let sha = git_sha()?;
        println!("MILESTONE {milestone} VERIFIED: {sha} unix={}", now_unix());
        return Ok(());
    }

    // Full goal: every feature must be verified or have a documented replacement.
    let rows = load_features()?;
    let unfinished: Vec<&FeatureRow> = rows
        .iter()
        .filter(|row| !matches!(row.status.as_str(), "verified" | "replaced" | "deferred"))
        .collect();
    if !unfinished.is_empty() {
        for row in unfinished.iter().take(20) {
            eprintln!(
                "unfinished feature: {} ({}) status={}",
                row.id, row.title, row.status
            );
        }
        bail!(
            "{} feature rows are not verified or replaced",
            unfinished.len()
        );
    }
    for row in &rows {
        if row.priority != "P3" && row.status == "deferred" {
            bail!(
                "P0/P1/P2 feature {} cannot be deferred without an ADR",
                row.id
            );
        }
        if row.status == "deferred" && row.notes.trim().is_empty() {
            bail!("deferred feature {} needs a note and ADR reference", row.id);
        }
    }

    verify(false)?;
    if !allow_dirty {
        require_clean_worktree()?;
    }
    let sha = git_sha()?;
    println!("GOAL VERIFIED: {sha} unix={}", now_unix());
    Ok(())
}

fn require_clean_worktree() -> Result<()> {
    let output = ProcessCommand::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root())
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        bail!("git status failed");
    }
    let dirty = String::from_utf8_lossy(&output.stdout);
    if !dirty.trim().is_empty() {
        bail!("worktree is not clean:\n{dirty}");
    }
    Ok(())
}

fn git_sha() -> Result<String> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root())
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn load_features() -> Result<Vec<FeatureRow>> {
    let path = root().join("docs/features.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let file: FeatureFile = toml::from_str(&text).context("parse docs/features.toml")?;
    Ok(file.feature)
}

fn features_verify() -> Result<()> {
    let rows = load_features()?;
    if rows.is_empty() {
        bail!("docs/features.toml has no [[feature]] rows");
    }
    let mut ids = BTreeSet::new();
    let mut verified = 0usize;
    for row in &rows {
        if !ids.insert(row.id.clone()) {
            bail!("duplicate feature id: {}", row.id);
        }
        if !ALLOWED_STATUS.contains(&row.status.as_str()) {
            bail!("invalid status '{}' for {}", row.status, row.id);
        }
        if !matches!(row.priority.as_str(), "P0" | "P1" | "P2" | "P3") {
            bail!("invalid priority '{}' for {}", row.priority, row.id);
        }
        if row.status == "verified" {
            verified += 1;
            if row.tests.is_empty() {
                bail!("verified feature {} has no tests", row.id);
            }
            if !docs_present(&row.docs) {
                bail!("verified feature {} has no docs path", row.id);
            }
            if row.evidence.trim().is_empty() {
                bail!("verified feature {} has no evidence", row.id);
            }
        }
    }
    println!("FEATURES OK: {} rows, {} verified", rows.len(), verified);
    Ok(())
}

fn docs_present(docs: &Option<toml::Value>) -> bool {
    match docs {
        Some(toml::Value::String(value)) => !value.trim().is_empty(),
        Some(toml::Value::Array(values)) => !values.is_empty(),
        Some(_) => true,
        None => false,
    }
}

fn features_apply(clean: bool, task: Option<&str>) -> Result<()> {
    let path = root().join("docs/features.toml");
    let mut main: toml::Value =
        toml::from_str(&fs::read_to_string(&path).context("read docs/features.toml")?)
            .context("parse docs/features.toml")?;
    let rows = main
        .get_mut("feature")
        .and_then(toml::Value::as_array_mut)
        .context("docs/features.toml has no feature array")?;

    let directory = root().join("orchestration/feature-updates");
    if !directory.exists() {
        println!("FEATURES APPLIED: 0 rows (no fragments)");
        return Ok(());
    }

    let mut updated = 0usize;
    let mut applied = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let fragment_path = entry.path();
        if fragment_path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        if let Some(task) = task {
            let stem = fragment_path.file_stem().and_then(|value| value.to_str());
            if stem != Some(task) {
                continue;
            }
        }
        let fragment: toml::Value = toml::from_str(&fs::read_to_string(&fragment_path)?)
            .with_context(|| format!("parse {}", fragment_path.display()))?;
        let Some(fragment_rows) = fragment.get("feature").and_then(toml::Value::as_array) else {
            continue;
        };
        for source in fragment_rows {
            let id = source
                .get("id")
                .and_then(toml::Value::as_str)
                .with_context(|| {
                    format!("fragment row without id in {}", fragment_path.display())
                })?;
            let target = rows
                .iter_mut()
                .find(|row| row.get("id").and_then(toml::Value::as_str) == Some(id))
                .with_context(|| format!("feature id {id} is not in docs/features.toml"))?;
            merge_feature_row(target, source)?;
            updated += 1;
        }
        applied.push(fragment_path);
    }

    fs::write(&path, toml::to_string_pretty(&main)?)?;
    if clean {
        for fragment in applied {
            fs::remove_file(&fragment)?;
        }
    }
    println!("FEATURES APPLIED: {updated} rows");
    features_verify()
}

fn merge_feature_row(target: &mut toml::Value, source: &toml::Value) -> Result<()> {
    let target = target
        .as_table_mut()
        .context("feature row is not a table")?;
    for key in ["status", "tests", "docs", "evidence", "notes"] {
        if let Some(value) = source.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
    Ok(())
}

fn task_path(task: &str) -> PathBuf {
    root()
        .join("orchestration/tasks")
        .join(format!("{task}.json"))
}

fn claims_dir() -> PathBuf {
    root().join("orchestration/claims")
}

fn load_tasks() -> Result<Vec<Task>> {
    let dir = root().join("orchestration/tasks");
    let mut tasks = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let task: Task = serde_json::from_str(&text)
            .with_context(|| format!("parse task {}", path.display()))?;
        tasks.push(task);
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tasks)
}

fn claim(task_id: &str, lane: &str) -> Result<()> {
    let path = task_path(task_id);
    let text =
        fs::read_to_string(&path).with_context(|| format!("task not found: {}", path.display()))?;
    let task: Task = serde_json::from_str(&text)?;
    if task.lane != lane {
        bail!(
            "task {} belongs to lane {}, not {}",
            task_id,
            task.lane,
            lane
        );
    }
    fs::create_dir_all(claims_dir())?;
    for entry in fs::read_dir(claims_dir())? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let existing: Claim = serde_json::from_str(&fs::read_to_string(&path)?)?;
        if existing.task == task_id {
            continue;
        }
        for owned in &task.owns {
            for other in &existing.owns {
                if paths_overlap(owned, other) {
                    bail!(
                        "claim conflict: {} owns {} which overlaps {} owned by {} ({})",
                        task_id,
                        owned,
                        other,
                        existing.lane,
                        existing.task
                    );
                }
            }
        }
    }
    let claim = Claim {
        task: task_id.to_owned(),
        lane: lane.to_owned(),
        owns: task.owns.clone(),
        created_unix: now_unix(),
    };
    let out = claims_dir().join(format!("{task_id}.json"));
    fs::write(&out, serde_json::to_string_pretty(&claim)?)?;
    println!("CLAIMED {task_id} for {lane}: {}", out.display());
    Ok(())
}

fn release(task_id: &str) -> Result<()> {
    let path = claims_dir().join(format!("{task_id}.json"));
    if path.exists() {
        fs::remove_file(&path)?;
        println!("RELEASED {task_id}");
    } else {
        println!("NO CLAIM {task_id}");
    }
    Ok(())
}

fn normalize_glob(glob: &str) -> String {
    glob.trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .trim_end_matches('/')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let a = normalize_glob(left);
    let b = normalize_glob(right);
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

fn board() -> Result<()> {
    let tasks = load_tasks()?;
    let mut out = String::new();
    out.push_str("# PDFXI Orchestration Board\n\n");
    out.push_str("Generated by `cargo xtask board`. Do not edit by hand.\n\n");
    out.push_str("| ID | Lane | Priority | Status | Title | Acceptance |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for task in &tasks {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            task.id,
            task.lane,
            task.priority,
            task.status,
            task.title.replace('|', "\\|"),
            task.acceptance.replace('|', "\\|")
        ));
    }
    let path = root().join("orchestration/BOARD.md");
    fs::write(&path, out)?;
    println!("BOARD written: {}", path.display());
    Ok(())
}

fn docs() -> Result<()> {
    for path in [
        "docs/GOAL.md",
        "docs/PLAN.md",
        "docs/ARCHITECTURE.md",
        "docs/FEATURES.md",
        "docs/ORCHESTRATION.md",
        "docs/TESTING.md",
        "docs/MEMORY.md",
    ] {
        if !root().join(path).exists() {
            bail!("missing docs page: {path}");
        }
    }
    println!("DOCS OK");
    Ok(())
}

fn bench(check: bool) -> Result<()> {
    let mut args = vec!["run", "-p", "pdfxi-testkit", "--example", "bench_open"];
    if check {
        args.push("--release");
    }
    run("cargo", &args)?;
    if check {
        println!("BENCH OK (budget enforced)");
    } else {
        println!("BENCH OK");
    }
    Ok(())
}

fn corpus_smoke() -> Result<()> {
    run(
        "cargo",
        &[
            "test",
            "-p",
            "pdfxi-testkit",
            "--test",
            "committed_fixtures",
        ],
    )?;
    println!("CORPUS SMOKE OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_overlap_detects_prefixes() {
        assert!(paths_overlap(
            "crates/pdfxi-core/**",
            "crates/pdfxi-core/src/lib.rs"
        ));
        assert!(paths_overlap("docs/**", "docs/PLAN.md"));
        assert!(!paths_overlap(
            "crates/pdfxi-core/**",
            "crates/pdfxi-render/**"
        ));
    }

    #[test]
    fn normalize_glob_is_stable() {
        assert_eq!(normalize_glob("./crates/foo/**"), "crates/foo");
        assert_eq!(normalize_glob("docs\\PLAN.md"), "docs/plan.md");
    }

    #[test]
    fn docs_present_accepts_string_and_array_forms() {
        assert!(docs_present(&Some(toml::Value::String(
            "docs/FEATURES.md".to_owned()
        ))));
        assert!(docs_present(&Some(toml::Value::Array(vec![
            toml::Value::String("a".to_owned())
        ]))));
        assert!(!docs_present(&Some(toml::Value::String(String::new()))));
        assert!(!docs_present(&None));
    }

    #[test]
    fn merge_feature_row_updates_only_allowed_fields() {
        let mut target: toml::Value =
            toml::from_str("id = \"VIEW-001\"\nstatus = \"planned\"\ntitle = \"Open\"\n")
                .expect("target");
        let source: toml::Value =
            toml::from_str("id = \"VIEW-001\"\nstatus = \"verified\"\ntitle = \"Ignored\"\n")
                .expect("source");
        merge_feature_row(&mut target, &source).expect("merge");
        assert_eq!(
            target.get("status").and_then(toml::Value::as_str),
            Some("verified")
        );
        assert_eq!(
            target.get("title").and_then(toml::Value::as_str),
            Some("Open")
        );
    }
}
