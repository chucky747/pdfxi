//! Workspace automation for PDFXI.
//!
//! `xtask` is the single entry point for verification, claims, the task board,
//! and the goal gate. Agents call it instead of inventing ad-hoc scripts.

mod bench;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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

const ALLOWED_STATUS: &[&str] = &[
    "planned",
    "in-progress",
    "verified",
    "library",
    "model",
    "replaced",
    "deferred",
];
const ALLOWED_HOSTS: &[&str] = &["gui", "cli"];

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
    /// List queued tasks whose dependencies are verified and paths are unclaimed.
    Ready,
    /// Regenerate orchestration/BOARD.md from task files.
    Board,
    /// Check documentation links and required pages.
    Docs,
    /// Run the deterministic performance benchmarks and budget gate.
    Bench {
        /// Emit an informational report; never exit non-zero on a breach.
        #[arg(long)]
        report: bool,
        /// Gate on regression thresholds (default behavior; kept for scripts).
        #[arg(long)]
        check: bool,
        /// Write the current report as the committed baseline, then exit.
        #[arg(long)]
        update_baseline: bool,
    },
    /// Run the smoke corpus. M0 is a no-op.
    Corpus {
        #[command(subcommand)]
        command: CorpusCommand,
    },
    /// Build, install, exercise and uninstall the smoke-identity installer.
    InstallerTest,
    /// Run the complete local CI gate and write `target/ci/ci-report.md`.
    Ci,
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
    host: String,
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
        Command::Ready => ready(),
        Command::Board => board(),
        Command::Docs => docs(),
        Command::Bench {
            report,
            check: _,
            update_baseline,
        } => bench_command(report, update_baseline),
        Command::Corpus { command } => match command {
            CorpusCommand::Smoke => corpus_smoke(),
        },
        Command::InstallerTest => installer_test(),
        Command::Ci => ci(),
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
    run_feature_tests()?;
    if !no_docs {
        // Single-threaded rustdoc: parallel workspace docs deadlock on this
        // host (T-0369 evidence), and the local gate must be reproducible.
        run("cargo", &["doc", "--workspace", "--no-deps", "-j", "1"])?;
    }
    features_verify()?;
    println!("VERIFY OK");
    Ok(())
}

/// Run the complete local gate that replaces GitHub CI.
///
/// The report is written even when a step fails, so the last local run is
/// inspectable without scraping the terminal.
fn ci() -> Result<()> {
    let report_dir = root().join("target").join("ci");
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("create local CI report directory {}", report_dir.display()))?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    println!("LOCAL CI start unix={started}");

    let result = (|| -> Result<()> {
        verify(false)?;
        docs()?;
        bench_command(false, false)?;
        installer_test()?;
        screen_reader_smoke()?;
        signature_check()?;
        Ok(())
    })();

    let status = if result.is_ok() { "PASS" } else { "FAIL" };
    let report = format!(
        "# PDFXI local CI\n\n\
         - started_unix: {started}\n\
         - status: {status}\n\
         - host: Windows (the only verified target)\n\n\
         Steps: `cargo xtask verify` (fmt, clippy, tests, doc -j 1, feature \
         matrix), `cargo xtask docs` (mdBook when MDBOOK is available), \
         `cargo xtask bench`, `cargo xtask installer-test`.\n\n\
         GitHub CI is not used. This local run is the verified gate.\n"
    );
    let path = report_dir.join("ci-report.md");
    fs::write(&path, report).with_context(|| format!("write {}", path.display()))?;
    if result.is_ok() {
        println!("LOCAL CI OK: {}", path.display());
    } else {
        println!("LOCAL CI FAILED: {}", path.display());
    }
    result
}

/// Run a PowerShell script from the repository root with explicit arguments.
fn run_powershell_script(script: &str, args: &[String]) -> Result<()> {
    let path = root().join(script);
    if !path.is_file() {
        bail!("missing local CI script: {}", path.display());
    }
    let mut command = vec![
        "-NoProfile".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-File".to_owned(),
        path.to_string_lossy().into_owned(),
    ];
    command.extend_from_slice(args);
    println!("$ powershell {}", command.join(" "));
    let status = ProcessCommand::new("powershell")
        .args(&command)
        .current_dir(root())
        .status()
        .with_context(|| format!("failed to start PowerShell for {script}"))?;
    ensure_status("powershell", status)
}

/// Locate the per-user installed viewer. The local gate runs against the
/// installed build, not a developer target directory.
fn installed_viewer() -> Result<PathBuf> {
    let local = env::var_os("LOCALAPPDATA")
        .context("LOCALAPPDATA is not set; the installed viewer path is unknown")?;
    let path = PathBuf::from(local)
        .join("Programs")
        .join("PDFXI")
        .join("pdfxi-ui.exe");
    if !path.is_file() {
        bail!(
            "installed viewer not found: {} (build and install the package first)",
            path.display()
        );
    }
    Ok(path)
}

/// Run the real NVDA screen-reader smoke against the installed viewer.
fn screen_reader_smoke() -> Result<()> {
    let viewer = installed_viewer()?;
    let out = root().join("target").join("tmp").join("screen-reader-ci");
    run_powershell_script(
        "scripts/screen-reader-smoke.ps1",
        &[
            "-Exe".to_owned(),
            viewer.to_string_lossy().into_owned(),
            "-OutDir".to_owned(),
            out.to_string_lossy().into_owned(),
        ],
    )
}

/// Verify Authenticode signatures when a signing certificate is configured.
/// An unsigned build is reported, never inferred as signed.
fn signature_check() -> Result<()> {
    if env::var_os("PDFXI_SIGN_THUMBPRINT").is_none() {
        println!("SIGNATURE CHECK skipped: PDFXI_SIGN_THUMBPRINT not set (unsigned build)");
        return Ok(());
    }
    let viewer = installed_viewer()?;
    let cli = viewer.with_file_name("pdfxi.exe");
    run_powershell_script(
        "scripts/verify-signatures.ps1",
        &[
            "-Viewer".to_owned(),
            viewer.to_string_lossy().into_owned(),
            "-Cli".to_owned(),
            cli.to_string_lossy().into_owned(),
            "-Require".to_owned(),
        ],
    )
}

fn milestone_required_rows(milestone: &str) -> Option<&'static [&'static str]> {
    // Milestone gates mirror docs/PLAN.md. They are a subset of the full
    // matrix: `goal-verify --all` still requires every row.
    match milestone {
        "M0" => Some(&[]),
        "M1" => Some(&[
            "STD-011", "STD-012", "AUTO-009", "AUTO-010", "VIEW-002", "VIEW-003", "VIEW-016",
        ]),
        "M2" => Some(&[
            "CREATE-003",
            "CREATE-004",
            "CREATE-005",
            "CREATE-009",
            "VIEW-004",
            "VIEW-005",
            "VIEW-006",
            "VIEW-007",
            "EDIT-008",
            "EDIT-009",
            "EDIT-011",
            "EXPORT-006",
            "EXPORT-007",
            "PRINT-001",
            "PRINT-007",
        ]),
        "M3" => Some(&[
            "EDIT-001",
            "EDIT-002",
            "EDIT-003",
            "EDIT-004",
            "EDIT-012",
            "EDIT-013",
            "EDIT-015",
            "FORMS-001",
            "FORMS-002",
            "FORMS-003",
            "FORMS-004",
            "FORMS-005",
            "FORMS-008",
            "FORMS-009",
            "FORMS-010",
            "SEC-001",
            "SEC-002",
            "SEC-004",
            "SEC-005",
            "SEC-006",
            "SEC-008",
            "SEC-009",
            "AUTO-004",
        ]),
        "M4" => Some(&[
            "SEC-003",
            "SEC-007",
            "STD-001",
            "STD-002",
            "STD-004",
            "STD-005",
            "STD-006",
            "STD-007",
            "STD-008",
            "STD-009",
            "STD-010",
            "PRINT-002",
            "PRINT-003",
            "PRINT-004",
            "PRINT-005",
            "PRINT-006",
            "PRINT-008",
            "EXPORT-008",
        ]),
        "M5" => Some(&[
            "CREATE-001",
            "CREATE-002",
            "CREATE-006",
            "CREATE-007",
            "CREATE-008",
            "CREATE-010",
            "CREATE-011",
            "CREATE-012",
            "CREATE-013",
            "CREATE-014",
            "EXPORT-001",
            "EXPORT-002",
            "EXPORT-003",
            "EXPORT-004",
            "EXPORT-005",
            "EXPORT-009",
            "PLAT-003",
            "PLAT-004",
        ]),
        "M6" => Some(&[
            "AUTO-001",
            "AUTO-002",
            "AUTO-003",
            "AUTO-005",
            "AUTO-006",
            "AUTO-007",
            "AUTO-008",
            "REVIEW-001",
            "REVIEW-002",
            "REVIEW-003",
            "REVIEW-004",
            "REVIEW-005",
            "REVIEW-006",
            "REVIEW-007",
            "REVIEW-008",
            "REVIEW-009",
            "ACCESS-001",
            "ACCESS-002",
            "ACCESS-003",
            "ACCESS-004",
            "ACCESS-005",
            "ACCESS-006",
            "VIEW-012",
            "VIEW-013",
            "VIEW-014",
            "VIEW-015",
        ]),
        "M7" => Some(&[
            "PLAT-001",
            "PLAT-002",
            "PLAT-005",
            "PLAT-006",
            "PLAT-007",
            "MANAGE-002",
            "MANAGE-003",
            "MANAGE-004",
            "MANAGE-005",
            "MANAGE-006",
            "MEDIA-001",
            "MEDIA-002",
            "MEDIA-003",
            "MEDIA-004",
            "MEDIA-005",
            "VIEW-018",
        ]),
        _ => None,
    }
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
        let required = milestone_required_rows(milestone).ok_or_else(|| {
            anyhow::anyhow!("unknown milestone {milestone}; expected M0 through M7")
        })?;
        let rows = load_features()?;
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|id| {
                !rows.iter().any(|row| {
                    row.id == *id && (row.status == "verified" || row.status == "replaced")
                })
            })
            .collect();
        if !missing.is_empty() {
            bail!(
                "{} requires these feature rows to be verified: {}",
                milestone,
                missing.join(", ")
            );
        }
        run("cargo", &["fmt", "--all", "--check"])?;
        run("cargo", &["test", "--workspace"])?;
        run_feature_tests()?;
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

/// Extract the final path segment from a `docs/features.toml` test citation.
///
/// The matrix uses module paths (`ui::tests::name`), crate-qualified paths
/// (`pdfxi-edit::annotation::tests::name`), file-qualified paths
/// (`cli.rs::name`), and bare symbols (`name`). In every shape the symbol that
/// must resolve to a `#[test]` function is the final `::`-separated segment.
fn citation_symbol(citation: &str) -> Option<&str> {
    let trimmed = citation.trim();
    if trimmed.is_empty() {
        return None;
    }
    let symbol = trimmed.rsplit("::").next().unwrap_or(trimmed);
    let mut chars = symbol.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return None,
    }
    if chars.all(|character| character.is_ascii_alphanumeric() || character == '_') {
        Some(symbol)
    } else {
        None
    }
}

/// Return the function name when `text` begins with a function item.
fn fn_item_name(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    if rest.starts_with("pub(") {
        let close = rest.find(')')?;
        rest = rest[close + 1..].trim_start();
    } else if let Some(after) = rest.strip_prefix("pub ") {
        rest = after.trim_start();
    }
    loop {
        let mut stripped = false;
        for modifier in ["async ", "unsafe ", "const "] {
            if let Some(after) = rest.strip_prefix(modifier) {
                rest = after.trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Collect the names of functions annotated with `#[test]` in one source file.
///
/// The scanner is line based so a commented-out or quoted `#[test]` does not
/// create an entry. Multi-line attributes between `#[test]` and `fn` are
/// tracked with a bracket depth so feature-gated tests still resolve.
#[cfg(test)]
fn test_fn_names(source: &str) -> Vec<String> {
    test_fn_sites(source)
        .into_iter()
        .map(|(name, _ignored)| name)
        .collect()
}

/// Collect test functions and whether they carry `#[ignore]`.
fn test_fn_sites(source: &str) -> Vec<(String, bool)> {
    let mut sites = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut attribute_depth = 0usize;
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if attribute_depth > 0 {
            attributes.push(line.to_owned());
            attribute_depth += line.matches('[').count();
            attribute_depth = attribute_depth.saturating_sub(line.matches(']').count());
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        if line.starts_with("#[") {
            attributes.push(line.to_owned());
            if let Some(stripped) = line.strip_prefix("#[test]")
                && let Some(name) = fn_item_name(stripped.trim_start())
            {
                sites.push((
                    name,
                    attributes
                        .iter()
                        .any(|attribute| attribute.contains("#[ignore")),
                ));
                attributes.clear();
            }
            let opens = line.matches('[').count();
            let closes = line.matches(']').count();
            if opens > closes {
                attribute_depth = opens - closes;
            }
            continue;
        }
        if let Some(name) = fn_item_name(line) {
            let is_test = attributes
                .iter()
                .any(|attribute| attribute.starts_with("#[test]"));
            if is_test {
                let ignored = attributes
                    .iter()
                    .any(|attribute| attribute.contains("#[ignore"));
                sites.push((name, ignored));
            }
            attributes.clear();
        } else if !line.starts_with('#') {
            attributes.clear();
        }
    }
    sites
}

/// Recursively collect `*.rs` files below `directory`.
fn collect_rust_sources(directory: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// One resolved `#[test]` site: its crate and whether it is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestSite {
    ignored: bool,
    crate_name: String,
}

/// Index every `#[test]` function under `crates/` with its crate and ignore
/// state. Duplicate names are kept as separate sites so a citation can name a
/// test in more than one crate without hiding an ignored copy.
fn workspace_test_sites() -> Result<BTreeMap<String, Vec<TestSite>>> {
    let crates = root().join("crates");
    let mut sources = Vec::new();
    collect_rust_sources(&crates, &mut sources)?;
    let mut sites: BTreeMap<String, Vec<TestSite>> = BTreeMap::new();
    for source in sources {
        let text =
            fs::read_to_string(&source).with_context(|| format!("read {}", source.display()))?;
        let crate_name = source
            .strip_prefix(&crates)
            .ok()
            .and_then(|relative| relative.components().next())
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        for (name, ignored) in test_fn_sites(&text) {
            sites.entry(name).or_default().push(TestSite {
                ignored,
                crate_name: crate_name.clone(),
            });
        }
    }
    Ok(sites)
}

/// Locate a PDFium shared library using the same candidates as the renderer.
fn pdfium_library_candidates() -> Vec<PathBuf> {
    let library = if cfg!(windows) {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    };
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PDFIUM_PATH")
        && !path.is_empty()
    {
        candidates.push(PathBuf::from(path));
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            Path::new(&local_app_data)
                .join("pdfxi")
                .join("pdfium")
                .join("bin")
                .join(library),
        );
    }
    candidates.push(root().join(library));
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join(library));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join(library)));
    }
    candidates
}

/// Fail early with an actionable message when the native PDFium library is absent.
fn require_pdfium() -> Result<()> {
    if pdfium_library_candidates()
        .iter()
        .any(|candidate| candidate.is_file())
    {
        return Ok(());
    }
    bail!(
        "the `pdfium` feature tests need the native PDFium library, but none of the \
         candidate paths exist. Run `./scripts/fetch-pdfium.ps1` (which installs \
         $LOCALAPPDATA/pdfxi/pdfium/bin/pdfium.dll) or set PDFIUM_PATH to the library file."
    )
}

/// Run the tests that the default workspace build would compile out.
fn run_feature_tests() -> Result<()> {
    require_pdfium()?;
    run(
        "cargo",
        &["test", "-p", "pdfxi-app", "--features", "ui,pdfium"],
    )?;
    run(
        "cargo",
        &["test", "-p", "pdfxi-render", "--features", "pdfium"],
    )?;
    Ok(())
}

fn features_verify() -> Result<()> {
    let rows = load_features()?;
    if rows.is_empty() {
        bail!("docs/features.toml has no [[feature]] rows");
    }
    let sites = workspace_test_sites()?;
    let mut ids = BTreeSet::new();
    let mut verified = 0usize;
    let mut gui = 0usize;
    let mut cli = 0usize;
    let mut unresolved = Vec::new();
    let mut ignored_tests = Vec::new();
    let mut bad_evidence = Vec::new();
    let mut bad_hosts = Vec::new();
    let mut non_test_entries = Vec::new();
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
            if !ALLOWED_HOSTS.contains(&row.host.as_str()) {
                bail!(
                    "verified feature {} has host '{}'; expected gui or cli",
                    row.id,
                    row.host
                );
            }
            if row.host == "gui" {
                gui += 1;
            } else {
                cli += 1;
            }
            if row.tests.is_empty() {
                bail!("verified feature {} has no tests", row.id);
            }
            if !docs_present(&row.docs) {
                bail!("verified feature {} has no docs path", row.id);
            }
            if row.evidence.trim().is_empty() {
                bail!("verified feature {} has no evidence", row.id);
            }
            if !evidence_has_existing_path(&row.evidence) {
                bad_evidence.push(format!("{}: {}", row.id, row.evidence.trim()));
            }
        }
        let mut app_tests = false;
        let mut cli_tests = false;
        for citation in &row.tests {
            match citation_symbol(citation) {
                Some(symbol) => match sites.get(symbol) {
                    Some(found) => {
                        if found.iter().all(|site| site.ignored) {
                            ignored_tests.push(format!("{}: {}", row.id, citation));
                        }
                        app_tests |= found.iter().any(|site| site.crate_name == "pdfxi-app");
                        cli_tests |= found.iter().any(|site| site.crate_name == "pdfxi-cli");
                    }
                    None => unresolved.push(format!(
                        "{}: {} (no `#[test] fn {symbol}` in crates/)",
                        row.id, citation
                    )),
                },
                // A path-like entry with `::` still claims to name a test, so an
                // invalid leaf is a matrix error. Entries with no `::` that are
                // not identifiers are commands or prose in the `tests` array;
                // report them separately instead of pretending they are tests.
                None if citation.contains("::") => unresolved.push(format!(
                    "{}: {} (not a resolvable test citation)",
                    row.id, citation
                )),
                None => non_test_entries.push(format!("{}: {}", row.id, citation)),
            }
        }
        if row.status == "verified" && (app_tests || cli_tests) {
            let host_matches = match row.host.as_str() {
                "gui" => app_tests,
                "cli" => cli_tests,
                _ => false,
            };
            if !host_matches {
                bad_hosts.push(format!(
                    "{}: host '{}' but cited tests are gui={app_tests} cli={cli_tests}",
                    row.id, row.host
                ));
            }
        }
    }
    if !non_test_entries.is_empty() {
        eprintln!(
            "warning: {} `tests` entr(ies) are not test citations and were skipped:",
            non_test_entries.len()
        );
        for entry in &non_test_entries {
            eprintln!("  {entry}");
        }
    }
    if !bad_evidence.is_empty() {
        bail!(
            "{} verified feature(s) have no existing repo evidence path:\n{}",
            bad_evidence.len(),
            bad_evidence.join("\n")
        );
    }
    if !ignored_tests.is_empty() {
        bail!(
            "{} cited test(s) are marked `#[ignore]`:\n{}",
            ignored_tests.len(),
            ignored_tests.join("\n")
        );
    }
    if !bad_hosts.is_empty() {
        bail!(
            "{} verified feature(s) name a host their cited tests do not cover:\n{}",
            bad_hosts.len(),
            bad_hosts.join("\n")
        );
    }
    if !unresolved.is_empty() {
        bail!(
            "{} cited test(s) do not resolve to a `#[test]` function:\n{}",
            unresolved.len(),
            unresolved.join("\n")
        );
    }
    println!(
        "FEATURES OK: {} rows, {} verified ({} gui, {} cli)",
        rows.len(),
        verified,
        gui,
        cli
    );
    Ok(())
}

/// Whether `evidence` names at least one repository path that exists.
///
/// Evidence may be prose around the path, but command output alone is no
/// longer accepted for a verified row: the reviewer must be able to open the
/// cited record.
fn evidence_has_existing_path(evidence: &str) -> bool {
    let root = root();
    for raw in evidence.split_whitespace() {
        let token = raw.trim_matches(|character: char| {
            matches!(
                character,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '.' | '*'
            )
        });
        let token = token.strip_prefix("./").unwrap_or(token).replace('\\', "/");
        let token = token.trim_start_matches('/');
        if token.contains("..") {
            continue;
        }
        if ["docs/", "fixtures/", "crates/", "scripts/"]
            .iter()
            .any(|prefix| token.starts_with(prefix))
            && root.join(token).exists()
        {
            return true;
        }
    }
    false
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
    for key in ["status", "host", "tests", "docs", "evidence", "notes"] {
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
    for dependency in &task.depends_on {
        let dependency_path = task_path(dependency);
        if !dependency_path.exists() {
            bail!("task {task_id} depends on {dependency}, which has no task file");
        }
        let dependency_task: Task = serde_json::from_str(&fs::read_to_string(&dependency_path)?)?;
        if dependency_task.status != "verified" {
            bail!(
                "task {task_id} depends on {dependency}, whose status is {} (must be verified)",
                dependency_task.status
            );
        }
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

fn ready() -> Result<()> {
    let tasks = load_tasks()?;
    let mut active: Vec<Claim> = Vec::new();
    if claims_dir().exists() {
        for entry in fs::read_dir(claims_dir())? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            active.push(serde_json::from_str(&fs::read_to_string(&path)?)?);
        }
    }
    let verified: BTreeSet<&str> = tasks
        .iter()
        .filter(|task| task.status == "verified")
        .map(|task| task.id.as_str())
        .collect();
    let mut printed = 0usize;
    for task in &tasks {
        if task.status != "queued" {
            continue;
        }
        if active.iter().any(|claim| claim.task == task.id) {
            continue;
        }
        if !task
            .depends_on
            .iter()
            .all(|dependency| verified.contains(dependency.as_str()))
        {
            continue;
        }
        let conflict = active.iter().any(|claim| {
            claim.task != task.id
                && claim
                    .owns
                    .iter()
                    .any(|owned| task.owns.iter().any(|wanted| paths_overlap(owned, wanted)))
        });
        if conflict {
            continue;
        }
        println!("{} {} {} {}", task.id, task.lane, task.priority, task.title);
        printed += 1;
    }
    if printed == 0 {
        println!("no ready tasks");
    }
    Ok(())
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
    let book = root().join("docs/book");
    for path in ["book.toml", "src/SUMMARY.md"] {
        if !book.join(path).exists() {
            bail!("missing mdBook page: docs/book/{path}");
        }
    }
    let mdbook = std::env::var_os("MDBOOK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mdbook"));
    match ProcessCommand::new(&mdbook)
        .args(["build", "docs/book", "-d", "target/tmp/mdbook-book"])
        .current_dir(root())
        .status()
    {
        Ok(status) if status.success() => println!("MDBOOK OK"),
        Ok(status) => bail!("mdbook build failed with {status}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: mdbook is not installed; book build skipped");
        }
        Err(error) => return Err(error).context("run mdbook"),
    }
    println!("DOCS OK");
    Ok(())
}

fn bench_command(report: bool, update_baseline: bool) -> Result<()> {
    println!("$ cargo xtask bench report={report} update_baseline={update_baseline}");
    bench::run(report, update_baseline).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Run the committed deterministic fixtures and the real-producer corpus.
///
/// The first suite builds or verifies the synthetic corpus; the second checks
/// `fixtures/realworld/` against its manifest (checksums, page counts, marker
/// words, and the recorded `known_failure` and encrypted limitations).
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
    run(
        "cargo",
        &["test", "-p", "pdfxi-testkit", "--test", "realworld_corpus"],
    )?;
    println!("CORPUS SMOKE OK");
    Ok(())
}

/// Build, install, exercise and uninstall the smoke-identity installer.
///
/// First runs `scripts/test-portable-install.ps1`, the hermetic regression
/// test for the portable contract: every manifest file is installed, including
/// `install.ps1`, the installed tree verifies against its manifest, an upgrade
/// merges the guide without nesting `docs\docs`, and an in-place run is safe.
/// Then delegates to `scripts/test-installer.ps1`, which prints a loud
/// `SKIPPED:` line and exits 0 when Inno Setup is not installed, so a host
/// without Inno Setup does not fail this command. It exits non-zero on a
/// failed assertion.
fn installer_test() -> Result<()> {
    run(
        "powershell",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts/test-portable-install.ps1",
        ],
    )?;
    run(
        "powershell",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "scripts/test-installer.ps1",
        ],
    )
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

    #[test]
    fn citation_parser_accepts_supported_forms() {
        assert_eq!(
            citation_symbol("ui::tests::opens_a_document"),
            Some("opens_a_document")
        );
        assert_eq!(
            citation_symbol("pdfxi-edit::annotation::tests::bounds"),
            Some("bounds")
        );
        assert_eq!(
            citation_symbol("cli.rs::repair_repairs"),
            Some("repair_repairs")
        );
        assert_eq!(citation_symbol("bare_symbol"), Some("bare_symbol"));
    }

    #[test]
    fn citation_parser_rejects_non_identifiers() {
        assert_eq!(citation_symbol(""), None);
        assert_eq!(citation_symbol("ui::tests::"), None);
        assert_eq!(citation_symbol("ui::tests::not a name"), None);
        assert_eq!(citation_symbol("9starts_with_a_digit"), None);
    }

    #[test]
    fn test_index_finds_annotated_functions() {
        let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn first() {}\n\n    #[test]\n    #[should_panic]\n    fn second() {}\n}\nfn helper() {}\n";
        assert_eq!(test_fn_names(source), vec!["first", "second"]);
    }

    #[test]
    fn test_index_ignores_unannotated_functions_and_comments() {
        let source =
            "fn plain() {}\n// #[test]\n// fn commented() {}\nconst TEXT: &str = \"#[test]\";\n";
        assert!(test_fn_names(source).is_empty());
    }

    #[test]
    fn test_sites_record_ignore_attributes() {
        let source =
            "#[test]\nfn fast() {}\n\n#[test]\n#[ignore = \"slow on purpose\"]\nfn slow() {}\n";
        assert_eq!(
            test_fn_sites(source),
            vec![("fast".to_owned(), false), ("slow".to_owned(), true)]
        );
    }

    #[test]
    fn evidence_paths_must_exist_in_the_repo() {
        assert!(evidence_has_existing_path(
            "see docs/evidence/review/2026-11-23-finish.md for the run"
        ));
        assert!(!evidence_has_existing_path(
            "cargo test --workspace: 3891 passed"
        ));
        assert!(!evidence_has_existing_path(
            "docs/evidence/does-not-exist.md"
        ));
    }
}
