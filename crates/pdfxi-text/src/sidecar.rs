//! Out-of-process OCR sidecar bridge for `pdfxi-text`.
//!
//! [`SidecarOcrEngine`] implements the dependency-free [`OcrEngine`] contract by
//! running a configured helper executable as a child process. It is the first
//! concrete backend for the pure wire format defined in [`crate::ocr`].
//!
//! # Security model
//!
//! * The executable path is validated when the configuration is built: it must
//!   be absolute, name an existing regular file, contain no `..` components,
//!   and not be a symlink. Bare names are never searched on `PATH`. It is then
//!   passed to [`std::process::Command`] directly — there is **no shell**, so no
//!   argument is ever interpreted as a command, glob, or redirection.
//! * The argument vector is fixed by default ([`DEFAULT_ARGUMENTS`]) and a
//!   caller-supplied replacement is validated (non-empty, bounded, no NUL or
//!   newline) but is still passed as a literal `argv`.
//! * The child environment is cleared with [`std::process::Command::env_clear`].
//!   Only names in the documented allowlist are copied from the ambient
//!   environment, plus an explicit map supplied by the caller. No ambient
//!   secret is forwarded implicitly, and the bridge performs no network I/O.
//! * stdout is read with a bounded buffer; exceeding the limit kills the child.
//! * A supervisor thread enforces a wall-clock timeout and an external
//!   [`CancellationToken`], killing the child on either signal.
//!
//! All errors are typed as [`SidecarError`]. The trait implementation maps them
//! onto [`OcrError`] so callers holding a `&dyn OcrEngine` keep working.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use std::{env, thread};

use crate::CancellationToken;
use crate::ocr::{
    FallbackPolicy, LanguageRouter, OcrEngine, OcrError, OcrImage, OcrOptions, OcrPage,
    SUPPORTED_SIDECAR_VERSION, SidecarRequest, SidecarResponse, sidecar_response_version,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default wall-clock budget for one recognition call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default upper bound on stdout bytes accepted from the sidecar (8 MiB).
pub const DEFAULT_MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
/// Default, fixed argument vector passed to the sidecar.
///
/// The sidecar reads one JSON request from stdin and writes one JSON response
/// to stdout; the flags merely select that mode. They contain no shell
/// metacharacters and are never interpreted by a shell.
pub const DEFAULT_ARGUMENTS: [&str; 2] = ["--json", "--stdin"];

/// Maximum bytes retained from stderr for diagnostics.
const STDERR_CAP: usize = 8 * 1024;
/// How often the supervisor checks for exit, timeout, or cancellation.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Maximum accepted length of a single argument, in bytes.
const MAX_ARGUMENT_LEN: usize = 256;
/// Maximum accepted length of an environment variable name, in bytes.
const MAX_ENV_NAME_LEN: usize = 256;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Typed failures produced while running the OCR sidecar process.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// The configured executable could not be started.
    #[error("failed to spawn OCR sidecar '{path}': {source}")]
    SpawnFailed {
        /// The executable path that failed to start.
        path: String,
        /// The underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// The sidecar exited with a non-zero status.
    #[error("OCR sidecar exited with code {code}: {stderr}")]
    NonZeroExit {
        /// Process exit code, or `-1` when the platform reports a signal.
        code: i32,
        /// Bounded stderr captured from the child.
        stderr: String,
    },
    /// The sidecar exceeded its wall-clock budget and was killed.
    #[error("OCR sidecar timed out after {duration:?}")]
    Timeout {
        /// The timeout that was exceeded.
        duration: Duration,
    },
    /// Cancellation was requested and the child was killed.
    #[error("OCR sidecar operation was cancelled")]
    Cancelled,
    /// The sidecar wrote more than the configured stdout limit and was killed.
    #[error("OCR sidecar stdout exceeded the {max} byte limit")]
    OversizedOutput {
        /// The maximum number of stdout bytes that were allowed.
        max: usize,
    },
    /// stdout was not a valid [`SidecarResponse`] payload.
    #[error("OCR sidecar returned a malformed response: {0}")]
    MalformedResponse(String),
    /// The sidecar spoke a different wire-format version.
    #[error("OCR sidecar version mismatch: expected {expected}, got {found}")]
    VersionMismatch {
        /// Version this bridge speaks.
        expected: u32,
        /// Version reported by the sidecar.
        found: u32,
    },
    /// The requested language is not in the engine's supported set.
    #[error("unsupported OCR language: {0}")]
    UnsupportedLanguage(String),
    /// The configuration rejected an invalid value.
    #[error("invalid OCR sidecar configuration: {0}")]
    InvalidConfig(String),
    /// An I/O error occurred while communicating with the child.
    #[error("OCR sidecar I/O error: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How the child process's working directory is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkingDirectoryPolicy {
    /// Leave the child in the current process's working directory.
    Inherit,
    /// Set the child's working directory to a fixed, validated path.
    Fixed(PathBuf),
}

/// Immutable, validated configuration for a [`SidecarOcrEngine`].
///
/// Build one with [`SidecarConfig::new`] and the `with_*` builders. Every setter
/// validates its input so an engine can never hold an invalid path, argument,
/// environment name, timeout, or buffer limit.
#[derive(Clone, Debug)]
pub struct SidecarConfig {
    executable: PathBuf,
    arguments: Vec<String>,
    language_router: LanguageRouter,
    timeout: Duration,
    max_stdout_bytes: usize,
    working_directory: WorkingDirectoryPolicy,
    env_allowlist: Vec<String>,
    explicit_env: BTreeMap<String, String>,
}

fn reject_untrusted_path_shape(path: &Path, label: &str) -> Result<(), SidecarError> {
    if path.as_os_str().is_empty() {
        return Err(SidecarError::InvalidConfig(format!(
            "{label} path must not be empty"
        )));
    }
    let text = path
        .as_os_str()
        .to_str()
        .ok_or_else(|| SidecarError::InvalidConfig(format!("{label} path must be valid UTF-8")))?;
    if text.trim().is_empty() || text.contains('\0') {
        return Err(SidecarError::InvalidConfig(format!(
            "{label} path is blank or contains a NUL byte"
        )));
    }
    // Bare names and relative paths are rejected rather than searched for on
    // PATH: an ambient PATH entry could otherwise choose an attacker binary.
    if !path.is_absolute() {
        return Err(SidecarError::InvalidConfig(format!(
            "{label} path must be absolute (bare names are not searched on PATH)"
        )));
    }
    // Lexical `..` (and `.`) is rejected so a caller cannot escape a directory
    // by construction. Canonicalization below catches the rest.
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(SidecarError::InvalidConfig(format!(
            "{label} path must not contain '..' or '.' components"
        )));
    }
    Ok(())
}

/// Validate the sidecar executable path.
///
/// The policy is deliberately strict: an **absolute** path to an **existing
/// regular file** with **no `..` components** and **no symlink at the final
/// component**. Canonicalization is used to confirm the path resolves to a real
/// regular file and to reject any symlink escape at the leaf. Bare names are
/// never resolved through the ambient `PATH`.
fn validate_executable(path: &Path) -> Result<(), SidecarError> {
    reject_untrusted_path_shape(path, "executable")?;
    let leaf = fs::symlink_metadata(path).map_err(|error| {
        SidecarError::InvalidConfig(format!(
            "executable does not exist or is unreadable: {error}"
        ))
    })?;
    if leaf.file_type().is_symlink() {
        return Err(SidecarError::InvalidConfig(
            "symlink executable paths are not allowed (symlink escape)".to_string(),
        ));
    }
    if !leaf.is_file() {
        return Err(SidecarError::InvalidConfig(
            "executable path is not a regular file".to_string(),
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        SidecarError::InvalidConfig(format!("executable cannot be canonicalized: {error}"))
    })?;
    if !fs::metadata(&canonical).is_ok_and(|meta| meta.is_file()) {
        return Err(SidecarError::InvalidConfig(
            "canonical executable path is not a regular file".to_string(),
        ));
    }
    Ok(())
}

/// Validate a fixed working-directory path. It must be absolute, contain no
/// traversal components, and name an existing directory. Symlinked directories
/// are permitted (unlike executable leaves) because a symlinked install prefix
/// is common and holds no executable code.
fn validate_directory(path: &Path) -> Result<(), SidecarError> {
    reject_untrusted_path_shape(path, "working directory")?;
    let meta = fs::metadata(path).map_err(|error| {
        SidecarError::InvalidConfig(format!("working directory does not exist: {error}"))
    })?;
    if !meta.is_dir() {
        return Err(SidecarError::InvalidConfig(
            "working directory path is not a directory".to_string(),
        ));
    }
    Ok(())
}

/// Validate a literal argument vector.
///
/// Arguments are passed verbatim to [`Command`], never through a shell. The
/// validator only rejects values that could crash the spawn call (NUL bytes) or
/// smuggle a control character (CR/LF), and bounds length. Shell-looking
/// characters such as `$`, `*`, or `;` are allowed deliberately: because no
/// shell is involved they remain literal text, and a test proves this.
pub fn validate_arguments(args: &[String]) -> Result<(), SidecarError> {
    for arg in args {
        if arg.is_empty() {
            return Err(SidecarError::InvalidConfig(
                "argument must not be empty".to_string(),
            ));
        }
        if arg.len() > MAX_ARGUMENT_LEN {
            return Err(SidecarError::InvalidConfig(format!(
                "argument exceeds {MAX_ARGUMENT_LEN} bytes"
            )));
        }
        if arg.contains('\0') || arg.contains('\n') || arg.contains('\r') {
            return Err(SidecarError::InvalidConfig(
                "argument contains a NUL or newline byte".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_env_name(name: &str) -> Result<(), SidecarError> {
    if name.is_empty()
        || name.len() > MAX_ENV_NAME_LEN
        || !name.is_ascii()
        || name.contains('=')
        || name.contains('\0')
    {
        return Err(SidecarError::InvalidConfig(format!(
            "invalid environment variable name '{name}'"
        )));
    }
    Ok(())
}

/// Select, from `source`, only environment variables whose names are listed in
/// `allowlist`. This is the entire ambient-environment policy of the bridge;
/// keeping it pure makes the "no ambient secret is forwarded" guarantee testable
/// without mutating process-global state.
fn filter_environment<I, K, V>(allowlist: &[String], source: I) -> BTreeMap<String, OsString>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut out = BTreeMap::new();
    for (key, value) in source {
        let Some(name) = key.as_ref().to_str() else {
            continue;
        };
        if allowlist.iter().any(|allowed| allowed == name) {
            out.insert(name.to_string(), value.as_ref().to_os_string());
        }
    }
    out
}

impl SidecarConfig {
    /// Build a configuration for `executable` supporting `languages`.
    ///
    /// The executable path is validated and the language set is validated and
    /// normalized through [`LanguageRouter`]. An empty language set is rejected
    /// because routing needs a deterministic default.
    pub fn new(
        executable: impl Into<PathBuf>,
        languages: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, SidecarError> {
        let executable = executable.into();
        validate_executable(&executable)?;
        let language_router = LanguageRouter::new(languages).map_err(|error| {
            SidecarError::InvalidConfig(format!("invalid language set: {error}"))
        })?;
        if language_router.default_language().is_none() {
            return Err(SidecarError::InvalidConfig(
                "language set must not be empty".to_string(),
            ));
        }
        Ok(Self {
            executable,
            arguments: DEFAULT_ARGUMENTS
                .iter()
                .map(|arg| (*arg).to_string())
                .collect(),
            language_router,
            timeout: DEFAULT_TIMEOUT,
            max_stdout_bytes: DEFAULT_MAX_STDOUT_BYTES,
            working_directory: WorkingDirectoryPolicy::Inherit,
            env_allowlist: Vec::new(),
            explicit_env: BTreeMap::new(),
        })
    }

    /// Replace the argument vector. The replacement is validated but always
    /// passed as a literal `argv`, never through a shell.
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Result<Self, SidecarError> {
        validate_arguments(&arguments)?;
        self.arguments = arguments;
        Ok(self)
    }

    /// Set the wall-clock timeout; it must be non-zero.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, SidecarError> {
        if timeout.is_zero() {
            return Err(SidecarError::InvalidConfig(
                "timeout must be non-zero".to_string(),
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Set the maximum accepted stdout byte count; it must be non-zero.
    pub fn with_max_stdout_bytes(mut self, max: usize) -> Result<Self, SidecarError> {
        if max == 0 {
            return Err(SidecarError::InvalidConfig(
                "max stdout bytes must be non-zero".to_string(),
            ));
        }
        self.max_stdout_bytes = max;
        Ok(self)
    }

    /// Set the child working-directory policy. A fixed path is validated.
    pub fn with_working_directory(
        mut self,
        policy: WorkingDirectoryPolicy,
    ) -> Result<Self, SidecarError> {
        if let WorkingDirectoryPolicy::Fixed(path) = &policy {
            validate_directory(path)?;
        }
        self.working_directory = policy;
        Ok(self)
    }

    /// Allow one ambient environment variable to be copied to the child.
    pub fn allow_environment(mut self, name: impl Into<String>) -> Result<Self, SidecarError> {
        let name = name.into();
        validate_env_name(&name)?;
        if !self.env_allowlist.contains(&name) {
            self.env_allowlist.push(name);
        }
        Ok(self)
    }

    /// Set one explicit environment variable for the child.
    ///
    /// This value comes from the caller, not the ambient environment, so it is
    /// never a hidden secret channel; the name is still validated.
    pub fn with_env(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, SidecarError> {
        let name = name.into();
        validate_env_name(&name)?;
        self.explicit_env.insert(name, value.into());
        Ok(self)
    }

    /// Re-check every invariant. Called again when an engine is constructed.
    pub fn validate(&self) -> Result<(), SidecarError> {
        validate_executable(&self.executable)?;
        validate_arguments(&self.arguments)?;
        if self.timeout.is_zero() {
            return Err(SidecarError::InvalidConfig(
                "timeout must be non-zero".to_string(),
            ));
        }
        if self.max_stdout_bytes == 0 {
            return Err(SidecarError::InvalidConfig(
                "max stdout bytes must be non-zero".to_string(),
            ));
        }
        if let WorkingDirectoryPolicy::Fixed(path) = &self.working_directory {
            validate_directory(path)?;
        }
        for name in &self.env_allowlist {
            validate_env_name(name)?;
        }
        for name in self.explicit_env.keys() {
            validate_env_name(name)?;
        }
        Ok(())
    }

    /// Return the configured executable path.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Return the literal argument vector.
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Return the supported language tags in stable order.
    pub fn languages(&self) -> &[String] {
        self.language_router.supported()
    }

    /// Return the wall-clock timeout.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Return the maximum accepted stdout byte count.
    pub const fn max_stdout_bytes(&self) -> usize {
        self.max_stdout_bytes
    }

    /// Return the working-directory policy.
    pub fn working_directory(&self) -> &WorkingDirectoryPolicy {
        &self.working_directory
    }

    /// Return the ambient environment variable allowlist.
    pub fn env_allowlist(&self) -> &[String] {
        &self.env_allowlist
    }

    /// Return the explicit environment variables forwarded to the child.
    pub fn explicit_env(&self) -> &BTreeMap<String, String> {
        &self.explicit_env
    }
}

// ---------------------------------------------------------------------------
// Process supervision
// ---------------------------------------------------------------------------

/// Why the supervisor stopped watching the child.
#[derive(Debug)]
enum ExitReason {
    /// The child exited on its own.
    Exited(ExitStatus),
    /// The wall-clock deadline elapsed.
    TimedOut,
    /// The cancellation token was signalled.
    Cancelled,
    /// A reader asked the supervisor to abort (for example, oversized stdout).
    Aborted,
    /// The supervisor could not poll the child.
    IoError(String),
}

/// Read at most `max` bytes from `reader`.
///
/// Any pipe failure — overflow or a read error — sets `abort` so the supervisor
/// kills and reaps the child instead of leaving it running while this function
/// returns an error.
fn capture_bounded<R: Read>(
    mut reader: R,
    max: usize,
    abort: &Arc<AtomicBool>,
) -> Result<Vec<u8>, SidecarError> {
    let mut collected = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => {
                abort.store(true, Ordering::SeqCst);
                return Err(SidecarError::Io(format!("reading sidecar output: {error}")));
            }
        };
        if read == 0 {
            break;
        }
        if collected.len().saturating_add(read) > max {
            abort.store(true, Ordering::SeqCst);
            return Err(SidecarError::OversizedOutput { max });
        }
        collected.extend_from_slice(&chunk[..read]);
    }
    Ok(collected)
}

/// Spawn the configured child, feed it `request_json`, and return its bounded
/// `(stdout, stderr)` once it has exited or been killed.
fn spawn_and_collect(
    config: &SidecarConfig,
    request_json: &str,
    cancellation: &CancellationToken,
) -> Result<(Vec<u8>, Vec<u8>), SidecarError> {
    let mut command = Command::new(&config.executable);
    command
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();

    if let WorkingDirectoryPolicy::Fixed(path) = &config.working_directory {
        command.current_dir(path);
    }
    for (name, value) in filter_environment(&config.env_allowlist, env::vars_os()) {
        command.env(name, value);
    }
    for (name, value) in &config.explicit_env {
        command.env(name, value);
    }

    let mut child = command.spawn().map_err(|error| SidecarError::SpawnFailed {
        path: config.executable.to_string_lossy().into_owned(),
        source: error,
    })?;
    let started = Instant::now();

    // Take all pipes before moving `child` into the supervisor.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| SidecarError::Io("child stdin was not piped".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SidecarError::Io("child stdout was not piped".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SidecarError::Io("child stderr was not piped".to_string()))?;

    let abort = Arc::new(AtomicBool::new(false));
    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();

    let stdout_abort = Arc::clone(&abort);
    let stdout_cap = config.max_stdout_bytes;
    thread::spawn(move || {
        let _ = stdout_tx.send(capture_bounded(stdout, stdout_cap, &stdout_abort));
    });
    let stderr_abort = Arc::clone(&abort);
    thread::spawn(move || {
        let _ = stderr_tx.send(capture_bounded(stderr, STDERR_CAP, &stderr_abort));
    });

    // The supervisor starts before the stdin write. A child that never drains
    // stdin can no longer deadlock the caller: the deadline and cancellation
    // are already being enforced while the writer waits on the pipe.
    let timeout = config.timeout;
    let watch_cancellation = cancellation.clone();
    let watch_abort = Arc::clone(&abort);
    let supervisor = thread::spawn(move || {
        loop {
            if watch_cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return ExitReason::Cancelled;
            }
            if watch_abort.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return ExitReason::Aborted;
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return ExitReason::TimedOut;
            }
            // A successful `try_wait` reaps the child, so no zombie is left behind.
            match child.try_wait() {
                Ok(Some(status)) => return ExitReason::Exited(status),
                Ok(None) => {}
                Err(error) => return ExitReason::IoError(error.to_string()),
            }
            thread::sleep(POLL_INTERVAL);
        }
    });

    // Bounded in a separate thread: if the child never reads stdin, this write
    // blocks until the supervisor kills the child, then returns a broken pipe.
    let writer_abort = Arc::clone(&abort);
    let request_bytes = request_json.as_bytes().to_vec();
    let writer = thread::spawn(move || {
        if let Err(error) = stdin.write_all(&request_bytes) {
            // A child is allowed to close stdin early after producing output;
            // that BrokenPipe is diagnostic only. All other pipe failures abort
            // so the supervisor kills the child before this call returns.
            if error.kind() != std::io::ErrorKind::BrokenPipe {
                writer_abort.store(true, Ordering::SeqCst);
            }
        }
    });

    let stdout_result = stdout_rx.recv().unwrap_or_else(|_| {
        Err(SidecarError::Io(
            "stdout reader stopped unexpectedly".to_string(),
        ))
    });
    let stderr_result = stderr_rx.recv().unwrap_or_else(|_| {
        Err(SidecarError::Io(
            "stderr reader stopped unexpectedly".to_string(),
        ))
    });

    // Always let the supervisor finish (killing/reaping on abort, timeout or
    // cancellation) before returning, and never leave the writer detached.
    let reason = supervisor
        .join()
        .map_err(|_| SidecarError::Io("supervisor thread panicked".to_string()))?;
    let _ = writer.join();

    match reason {
        ExitReason::Cancelled => Err(SidecarError::Cancelled),
        ExitReason::TimedOut => Err(SidecarError::Timeout { duration: timeout }),
        ExitReason::Aborted => match (stdout_result, stderr_result) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(_), Ok(_)) => Err(SidecarError::Io(
                "sidecar aborted by a pipe I/O error".to_string(),
            )),
        },
        ExitReason::IoError(message) => Err(SidecarError::Io(message)),
        ExitReason::Exited(status) if !status.success() => Err(SidecarError::NonZeroExit {
            code: status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&stderr_result.unwrap_or_default()).into_owned(),
        }),
        ExitReason::Exited(_) => match (stdout_result, stderr_result) {
            (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// An [`OcrEngine`] backed by an out-of-process sidecar.
///
/// The engine owns a cloneable [`CancellationToken`] that the trait's
/// [`OcrEngine::recognize`] honours; callers can obtain it with
/// [`Self::cancellation`] and cancel in flight work. Callers that need a
/// different token or an explicit page number use [`Self::recognize_page`].
#[derive(Clone, Debug)]
pub struct SidecarOcrEngine {
    config: SidecarConfig,
    cancellation: CancellationToken,
}

impl SidecarOcrEngine {
    /// Build an engine from a validated configuration.
    pub fn new(config: SidecarConfig) -> Result<Self, SidecarError> {
        config.validate()?;
        Ok(Self {
            config,
            cancellation: CancellationToken::new(),
        })
    }

    /// Build an engine that uses `cancellation` for [`OcrEngine::recognize`].
    pub fn with_cancellation(
        config: SidecarConfig,
        cancellation: CancellationToken,
    ) -> Result<Self, SidecarError> {
        config.validate()?;
        Ok(Self {
            config,
            cancellation,
        })
    }

    /// Return this engine's configuration.
    pub const fn config(&self) -> &SidecarConfig {
        &self.config
    }

    /// Return the token honoured by [`OcrEngine::recognize`].
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Recognize `image` with an explicit page number and cancellation token.
    ///
    /// Requested languages are resolved against the engine's supported set with
    /// [`FallbackPolicy::Strict`], so an unsupported language is rejected before
    /// a process is spawned. An empty request language list means "all
    /// supported languages".
    pub fn recognize_page(
        &self,
        image: OcrImage,
        options: &OcrOptions,
        page: usize,
        cancellation: &CancellationToken,
    ) -> Result<OcrPage, SidecarError> {
        options.validate().map_err(|error| {
            SidecarError::InvalidConfig(format!("invalid OCR options: {error}"))
        })?;

        let requested = if options.languages.is_empty() {
            self.config.languages().to_vec()
        } else {
            options.languages.clone()
        };
        let routes = self
            .config
            .language_router
            .route(&requested, "", FallbackPolicy::Strict)
            .map_err(|error| match error {
                OcrError::UnsupportedLanguage(tag) => SidecarError::UnsupportedLanguage(tag),
                other => SidecarError::InvalidConfig(format!("language routing failed: {other}")),
            })?;
        let languages = LanguageRouter::resolved_languages(&routes);

        let request = SidecarRequest::from_image(&image, languages, options.confidence_threshold)
            .map_err(|error| {
            SidecarError::InvalidConfig(format!("invalid sidecar request: {error}"))
        })?;
        let request_json = request.to_json().map_err(|error| {
            SidecarError::InvalidConfig(format!("request serialization: {error}"))
        })?;

        let (stdout, _) = spawn_and_collect(&self.config, &request_json, cancellation)?;
        let text = std::str::from_utf8(&stdout).map_err(|_| {
            SidecarError::MalformedResponse("stdout is not valid UTF-8".to_string())
        })?;

        let found = sidecar_response_version(text)
            .map_err(|error| SidecarError::MalformedResponse(error.to_string()))?;
        if found != SUPPORTED_SIDECAR_VERSION {
            return Err(SidecarError::VersionMismatch {
                expected: SUPPORTED_SIDECAR_VERSION,
                found,
            });
        }
        let response = SidecarResponse::from_json(text)
            .map_err(|error| SidecarError::MalformedResponse(error.to_string()))?;
        response
            .to_ocr_page(page)
            .map_err(|error| SidecarError::MalformedResponse(error.to_string()))
    }
}

impl OcrEngine for SidecarOcrEngine {
    fn name(&self) -> &str {
        "tesseract-sidecar"
    }

    fn languages(&self) -> &[String] {
        self.config.languages()
    }

    fn recognize(&self, image: OcrImage, options: &OcrOptions) -> Result<OcrPage, OcrError> {
        self.recognize_page(image, options, 0, &self.cancellation)
            .map_err(|error| match error {
                SidecarError::Cancelled => OcrError::Cancelled,
                SidecarError::UnsupportedLanguage(tag) => OcrError::UnsupportedLanguage(tag),
                other => OcrError::Backend(other.to_string()),
            })
    }
}

// ---------------------------------------------------------------------------
// Tests (pure; process tests live in tests/sidecar_bridge.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> SidecarConfig {
        let exe = env::current_exe().expect("current test executable");
        SidecarConfig::new(exe, ["eng"]).expect("valid base config")
    }

    #[test]
    fn config_rejects_empty_paths_and_empty_language_sets() {
        let exe = env::current_exe().expect("current test executable");
        assert!(matches!(
            SidecarConfig::new("", ["eng"]),
            Err(SidecarError::InvalidConfig(_))
        ));
        assert!(matches!(
            SidecarConfig::new("   ", ["eng"]),
            Err(SidecarError::InvalidConfig(_))
        ));
        assert!(matches!(
            SidecarConfig::new(&exe, Vec::<String>::new()),
            Err(SidecarError::InvalidConfig(_))
        ));
        assert!(matches!(
            SidecarConfig::new(&exe, ["bad tag"]),
            Err(SidecarError::InvalidConfig(_))
        ));
    }

    #[test]
    fn executable_validation_requires_absolute_existing_regular_file() {
        let exe = env::current_exe().expect("current test executable");
        assert!(validate_executable(&exe).is_ok());

        // Bare names and relative paths are never searched on PATH.
        assert!(validate_executable(Path::new("sidecar")).is_err());
        assert!(validate_executable(Path::new(".\\sidecar")).is_err());
        // A non-existent absolute path is rejected at configuration time.
        let missing = env::temp_dir().join(format!("pdfxi-missing-{}", std::process::id()));
        assert!(validate_executable(&missing).is_err());
        // A directory is not a regular file.
        assert!(validate_executable(&env::temp_dir()).is_err());
        // Lexical traversal is rejected even when the target exists.
        let traversal = exe
            .parent()
            .expect("parent")
            .join("..")
            .join(exe.file_name().expect("file name"));
        assert!(validate_executable(&traversal).is_err());
    }

    #[test]
    fn executable_validation_rejects_symlinks_when_creation_is_permitted() {
        let exe = env::current_exe().expect("current test executable");
        let dir = env::temp_dir().join(format!("pdfxi-t0071-link-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let link = dir.join("sidecar-link");

        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(&exe, &link).is_ok();
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&exe, &link).is_ok();

        if created {
            let result = validate_executable(&link);
            let _ = fs::remove_file(&link);
            let _ = fs::remove_dir(&dir);
            assert!(result.is_err(), "symlink executable must be rejected");
        }
        // On hosts where creating symlinks needs elevation, creation fails and
        // there is nothing to assert; validate_executable still rejects them.
    }

    #[test]
    fn working_directory_validation_requires_existing_directory() {
        let dir = env::temp_dir();
        let exe = env::current_exe().expect("current test executable");
        assert!(validate_directory(&dir).is_ok());
        assert!(
            validate_directory(&exe).is_err(),
            "a file is not a directory"
        );
        assert!(validate_directory(Path::new("relative/dir")).is_err());
        assert!(validate_directory(&dir.join("..")).is_err());
        assert!(validate_directory(&dir.join(format!("missing-{}", std::process::id()))).is_err());
    }

    #[test]
    fn config_builders_validate_limits_and_environment_names() {
        let config = base_config();
        assert_eq!(config.arguments().len(), DEFAULT_ARGUMENTS.len());
        assert_eq!(config.timeout(), DEFAULT_TIMEOUT);
        assert_eq!(config.max_stdout_bytes(), DEFAULT_MAX_STDOUT_BYTES);
        assert_eq!(config.languages(), &["eng".to_string()]);

        assert!(config.clone().with_timeout(Duration::ZERO).is_err());
        assert!(config.clone().with_max_stdout_bytes(0).is_err());
        assert!(config.clone().allow_environment("BAD=NAME").is_err());
        assert!(config.clone().with_env("", "x").is_err());
        assert!(
            config
                .clone()
                .with_working_directory(WorkingDirectoryPolicy::Fixed(PathBuf::new()))
                .is_err()
        );

        let config = config
            .with_timeout(Duration::from_millis(250))
            .expect("timeout")
            .with_max_stdout_bytes(1024)
            .expect("limit")
            .allow_environment("PDFXI_TEST_OK")
            .expect("allowlist")
            .with_env("PDFXI_FAKE_MODE", "echo")
            .expect("explicit env")
            .with_working_directory(WorkingDirectoryPolicy::Inherit)
            .expect("cwd");
        assert_eq!(config.timeout(), Duration::from_millis(250));
        assert_eq!(config.max_stdout_bytes(), 1024);
        assert_eq!(config.env_allowlist(), &["PDFXI_TEST_OK".to_string()]);
        assert_eq!(
            config.explicit_env().get("PDFXI_FAKE_MODE"),
            Some(&"echo".to_string())
        );
        config.validate().expect("still valid");
    }

    #[test]
    fn arguments_are_validated_but_shell_looking_text_is_allowed() {
        assert!(
            validate_arguments(
                &DEFAULT_ARGUMENTS
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            )
            .is_ok()
        );
        assert!(
            validate_arguments(&["$(id)".to_string(), "a;b".to_string(), "*".to_string()]).is_ok()
        );
        assert!(validate_arguments(&["".to_string()]).is_err());
        assert!(validate_arguments(&["has\nnl".to_string()]).is_err());
        assert!(validate_arguments(&["nul\0byte".to_string()]).is_err());
        assert!(validate_arguments(&["x".repeat(MAX_ARGUMENT_LEN + 1)]).is_err());
        assert!(
            base_config()
                .with_arguments(vec!["ok".to_string(), "a;b".to_string()])
                .is_ok()
        );
        assert!(
            base_config()
                .with_arguments(vec!["has\nnl".to_string()])
                .is_err()
        );
    }

    #[test]
    fn environment_filter_forwards_only_allowlisted_names() {
        let source = vec![
            ("PATH", "/usr/bin"),
            ("PDFXI_ALLOWED", "yes"),
            ("AWS_SECRET_ACCESS_KEY", "top-secret"),
            ("PDFXI_TEST_SECRET", "do-not-leak"),
        ];
        let filtered = filter_environment(&["PDFXI_ALLOWED".to_string()], source.clone());
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered.get("PDFXI_ALLOWED").and_then(|v| v.to_str()),
            Some("yes")
        );
        assert!(!filtered.contains_key("PATH"));
        assert!(!filtered.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!filtered.contains_key("PDFXI_TEST_SECRET"));

        let none = filter_environment::<_, &str, &str>(&[], source);
        assert!(none.is_empty(), "an empty allowlist forwards nothing");
    }

    #[test]
    fn engine_rejects_unsupported_language_before_spawning() {
        let engine = SidecarOcrEngine::new(base_config()).expect("engine");
        let image = OcrImage::new(1, 1, 300.0, crate::ocr::OcrPixelFormat::Grayscale, vec![0])
            .expect("image");
        let options = OcrOptions::new(vec!["deu".to_string()]);
        let token = CancellationToken::new();
        let result = engine.recognize_page(image.clone(), &options, 0, &token);
        assert!(matches!(result, Err(SidecarError::UnsupportedLanguage(ref tag)) if tag == "deu"));

        // The trait impl maps the typed error onto the contract error.
        let via_trait = engine.recognize(image, &options);
        assert_eq!(
            via_trait,
            Err(OcrError::UnsupportedLanguage("deu".to_string()))
        );
    }

    #[test]
    fn engine_exposes_documented_defaults() {
        let engine = SidecarOcrEngine::new(base_config()).expect("engine");
        assert_eq!(OcrEngine::name(&engine), "tesseract-sidecar");
        assert_eq!(engine.languages(), &["eng".to_string()]);
        assert!(!engine.cancellation().is_cancelled());
        assert_eq!(engine.config().timeout(), DEFAULT_TIMEOUT);
    }
}
