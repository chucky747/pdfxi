//! Headless launch and configuration model for the desktop host (PLAT-001).
//!
//! This module models the command-line surface, the single-instance policy,
//! and the window/DPI preferences of the Windows desktop application. It
//! parses an argument slice into a validated [`LaunchConfig`] and resolves a
//! deterministic [`StartupPlan`]. It contains **no GUI code**, performs no
//! process or window management, and does no I/O: a host is expected to
//! translate the resolved plan into a real window and process lifecycle.
//!
//! Everything is value-based and deterministic so the same arguments always
//! resolve to the same plan and the same [`StartupPlan::summary`] string.

use thiserror::Error;

/// The largest accepted window dimension, in device-independent pixels.
pub const MAX_WINDOW_DIMENSION: u32 = 16_384;
/// The largest accepted number of file arguments in one launch.
pub const MAX_LAUNCH_FILES: usize = 256;
/// The largest accepted byte length of a single argument value.
pub const MAX_ARG_BYTES: usize = 4_096;

/// A DPI-awareness preference for the host window.
///
/// These are the standard Windows DPI-awareness modes, modelled as plain data.
/// No operating-system call is made from this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiAwareness {
    /// The process does not participate in DPI virtualization.
    Unaware,
    /// One DPI value is applied to the whole process.
    System,
    /// Each monitor may have its own DPI.
    PerMonitor,
    /// Per-monitor awareness with the version-2 behavior (mixed-mode scaling).
    PerMonitorV2,
}

impl DpiAwareness {
    /// Return the stable, lowercase token used on the command line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unaware => "unaware",
            Self::System => "system",
            Self::PerMonitor => "per-monitor",
            Self::PerMonitorV2 => "per-monitor-v2",
        }
    }

    /// Parse a command-line DPI token.
    ///
    /// Accepted tokens are `unaware`, `system`, `per-monitor` and
    /// `per-monitor-v2` (case-insensitive). Any other token is rejected with
    /// [`LaunchError::InvalidDpi`].
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::InvalidDpi`] for an unrecognized token.
    pub fn parse(token: &str) -> Result<Self, LaunchError> {
        match token.to_ascii_lowercase().as_str() {
            "unaware" => Ok(Self::Unaware),
            "system" => Ok(Self::System),
            "per-monitor" => Ok(Self::PerMonitor),
            "per-monitor-v2" => Ok(Self::PerMonitorV2),
            other => Err(LaunchError::InvalidDpi(other.to_owned())),
        }
    }
}

/// Whether a second launch should reuse the running instance or start a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceMode {
    /// Route file arguments to the already-running instance.
    Single,
    /// Always start an independent instance.
    Multi,
}

impl SingleInstanceMode {
    /// Return the stable, lowercase token for this policy.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
        }
    }
}

/// The initial window geometry preference.
///
/// Width and height are mandatory and validated on construction. The two
/// mutually exclusive presentation flags are set with the builder methods and
/// checked when a plan is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPreference {
    width: u32,
    height: u32,
    maximized: bool,
    fullscreen: bool,
}

impl WindowPreference {
    /// Create a window preference with the given size in device-independent pixels.
    ///
    /// Both dimensions must be in `1..=MAX_WINDOW_DIMENSION`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::InvalidDimension`] when a dimension is zero or
    /// exceeds [`MAX_WINDOW_DIMENSION`].
    pub fn new(width: u32, height: u32) -> Result<Self, LaunchError> {
        if width == 0 || width > MAX_WINDOW_DIMENSION {
            return Err(LaunchError::InvalidDimension("width", width));
        }
        if height == 0 || height > MAX_WINDOW_DIMENSION {
            return Err(LaunchError::InvalidDimension("height", height));
        }
        Ok(Self {
            width,
            height,
            maximized: false,
            fullscreen: false,
        })
    }

    /// Return a copy that requests a maximized window.
    pub const fn maximized(mut self) -> Self {
        self.maximized = true;
        self
    }

    /// Return a copy that requests a borderless full-screen window.
    pub const fn fullscreen(mut self) -> Self {
        self.fullscreen = true;
        self
    }

    /// Return the requested width in device-independent pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Return the requested height in device-independent pixels.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Return whether a maximized window was requested.
    pub const fn is_maximized(self) -> bool {
        self.maximized
    }

    /// Return whether a full-screen window was requested.
    pub const fn is_fullscreen(self) -> bool {
        self.fullscreen
    }
}

/// A parsed, validated launch configuration.
///
/// Obtain one through [`parse_launch_args`]. Defaults are: [`DpiAwareness::PerMonitorV2`],
/// [`SingleInstanceMode::Single`], a `1024x768` window, and no files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    dpi: DpiAwareness,
    single_instance: SingleInstanceMode,
    window: WindowPreference,
    files: Vec<String>,
}

impl LaunchConfig {
    /// Return the configured DPI-awareness preference.
    pub const fn dpi(&self) -> DpiAwareness {
        self.dpi
    }

    /// Return the configured single-instance policy.
    pub const fn single_instance(&self) -> SingleInstanceMode {
        self.single_instance
    }

    /// Return the configured window preference.
    pub const fn window(&self) -> &WindowPreference {
        &self.window
    }

    /// Return the file arguments in the order they were supplied.
    pub fn files(&self) -> &[String] {
        &self.files
    }
}

/// A fully resolved, validated startup plan.
///
/// The plan is the deterministic output of [`resolve_startup_plan`] and is what
/// a host consumes. It never touches an operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupPlan {
    dpi: DpiAwareness,
    single_instance: SingleInstanceMode,
    width: u32,
    height: u32,
    maximized: bool,
    fullscreen: bool,
    files: Vec<String>,
}

impl StartupPlan {
    /// Return the resolved DPI-awareness preference.
    pub const fn dpi(&self) -> DpiAwareness {
        self.dpi
    }

    /// Return the resolved single-instance policy.
    pub const fn single_instance(&self) -> SingleInstanceMode {
        self.single_instance
    }

    /// Return the resolved window width.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Return the resolved window height.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Return whether the window should be maximized.
    pub const fn is_maximized(&self) -> bool {
        self.maximized
    }

    /// Return whether the window should be full-screen.
    pub const fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Return the resolved file arguments in input order.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// Return a deterministic, human-readable summary of this plan.
    ///
    /// The same plan always produces the same string. The format is a stable
    /// `key=value` sequence separated by `; ` with files listed after `files=`.
    pub fn summary(&self) -> String {
        format!(
            "dpi={}; single_instance={}; window={}x{}; maximized={}; fullscreen={}; files={}",
            self.dpi.as_str(),
            self.single_instance.as_str(),
            self.width,
            self.height,
            self.maximized,
            self.fullscreen,
            self.files.join(",")
        )
    }
}

/// Typed errors produced while parsing or resolving a launch configuration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LaunchError {
    /// A flag was not recognized.
    #[error("unknown launch flag: {0}")]
    UnknownFlag(String),
    /// A flag that requires a value did not receive one.
    #[error("launch flag {0} requires a value")]
    MissingValue(&'static str),
    /// The DPI token was not one of the documented values.
    #[error("invalid DPI preference: {0}")]
    InvalidDpi(String),
    /// The `WxH` window expression was malformed.
    #[error("invalid window size: {0}")]
    InvalidWindow(String),
    /// A window dimension was zero or too large.
    #[error("invalid window {0}: {1}")]
    InvalidDimension(&'static str, u32),
    /// A flag that accepts one value was supplied more than once.
    #[error("duplicate launch flag: {0}")]
    DuplicateFlag(&'static str),
    /// `--maximized` and `--fullscreen` were both requested.
    #[error("a window cannot be both maximized and full-screen")]
    ConflictingWindowModes,
    /// A file argument was empty.
    #[error("file argument must not be empty")]
    EmptyFile,
    /// Too many file arguments were supplied.
    #[error("too many file arguments: {0} (limit {1})")]
    TooManyFiles(usize, usize),
    /// A single argument exceeded the documented byte limit.
    #[error("launch argument at index {index} is {len} bytes (limit {limit})")]
    ArgTooLong {
        /// The zero-based argument index.
        index: usize,
        /// The argument byte length.
        len: usize,
        /// The accepted limit.
        limit: usize,
    },
}

/// Parse an argument slice into a validated [`LaunchConfig`].
///
/// The first element is conventionally the program name and is ignored when it
/// equals `argv[0]`; callers may instead pass only the arguments. Supported
/// flags are `--dpi <token>`, `--single-instance`, `--multi-instance`,
/// `--window <W>x<H>`, `--maximized`, `--fullscreen` and `--file <path>`. Any
/// non-flag argument is treated as a file path.
///
/// # Errors
///
/// Returns a typed [`LaunchError`] for an unknown flag, a missing or malformed
/// value, a duplicate single-value flag, an empty or oversized argument, or too
/// many files.
pub fn parse_launch_args(args: &[String]) -> Result<LaunchConfig, LaunchError> {
    let mut dpi = DpiAwareness::PerMonitorV2;
    let mut single_instance = SingleInstanceMode::Single;
    let mut window = WindowPreference::new(1024, 768)?;
    let mut maximized = false;
    let mut fullscreen = false;
    let mut files: Vec<String> = Vec::new();
    let mut seen_dpi = false;
    let mut seen_instance = false;
    let mut seen_window = false;

    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg.len() > MAX_ARG_BYTES {
            return Err(LaunchError::ArgTooLong {
                index,
                len: arg.len(),
                limit: MAX_ARG_BYTES,
            });
        }
        match arg.as_str() {
            "--dpi" => {
                if seen_dpi {
                    return Err(LaunchError::DuplicateFlag("--dpi"));
                }
                seen_dpi = true;
                let value = next_value(args, &mut index, "--dpi")?;
                dpi = DpiAwareness::parse(value)?;
            }
            "--single-instance" => {
                if seen_instance {
                    return Err(LaunchError::DuplicateFlag("instance policy"));
                }
                seen_instance = true;
                single_instance = SingleInstanceMode::Single;
            }
            "--multi-instance" => {
                if seen_instance {
                    return Err(LaunchError::DuplicateFlag("instance policy"));
                }
                seen_instance = true;
                single_instance = SingleInstanceMode::Multi;
            }
            "--window" => {
                if seen_window {
                    return Err(LaunchError::DuplicateFlag("--window"));
                }
                seen_window = true;
                let value = next_value(args, &mut index, "--window")?;
                window = parse_window(value)?;
            }
            "--maximized" => {
                if maximized {
                    return Err(LaunchError::DuplicateFlag("--maximized"));
                }
                maximized = true;
            }
            "--fullscreen" => {
                if fullscreen {
                    return Err(LaunchError::DuplicateFlag("--fullscreen"));
                }
                fullscreen = true;
            }
            "--file" => {
                let value = next_value(args, &mut index, "--file")?;
                push_file(&mut files, value)?;
            }
            other if other.starts_with("--") => {
                return Err(LaunchError::UnknownFlag(other.to_owned()));
            }
            path => push_file(&mut files, path)?,
        }
        index = index.saturating_add(1);
    }

    if maximized {
        window = window.maximized();
    }
    if fullscreen {
        window = window.fullscreen();
    }
    Ok(LaunchConfig {
        dpi,
        single_instance,
        window,
        files,
    })
}

/// Consume the value following a flag, rejecting a missing or flag-like value.
fn next_value<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &'static str,
) -> Result<&'a str, LaunchError> {
    let next = index.saturating_add(1);
    match args.get(next) {
        Some(value) if !value.starts_with("--") => {
            *index = next;
            Ok(value.as_str())
        }
        _ => Err(LaunchError::MissingValue(flag)),
    }
}

/// Parse a `WxH` window expression.
fn parse_window(value: &str) -> Result<WindowPreference, LaunchError> {
    let (width, height) = value
        .split_once('x')
        .ok_or_else(|| LaunchError::InvalidWindow(value.to_owned()))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| LaunchError::InvalidWindow(value.to_owned()))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| LaunchError::InvalidWindow(value.to_owned()))?;
    WindowPreference::new(width, height).map_err(|_| LaunchError::InvalidWindow(value.to_owned()))
}

/// Append a non-empty file argument, enforcing the documented bound.
fn push_file(files: &mut Vec<String>, value: &str) -> Result<(), LaunchError> {
    if value.is_empty() {
        return Err(LaunchError::EmptyFile);
    }
    if files.len() >= MAX_LAUNCH_FILES {
        return Err(LaunchError::TooManyFiles(
            files.len().saturating_add(1),
            MAX_LAUNCH_FILES,
        ));
    }
    files.push(value.to_owned());
    Ok(())
}

/// Resolve a validated [`StartupPlan`] from a configuration.
///
/// The only additional check beyond [`parse_launch_args`] is that a window
/// cannot be both maximized and full-screen.
///
/// # Errors
///
/// Returns [`LaunchError::ConflictingWindowModes`] when both presentation
/// flags are set.
pub fn resolve_startup_plan(config: &LaunchConfig) -> Result<StartupPlan, LaunchError> {
    let window = config.window();
    if window.is_maximized() && window.is_fullscreen() {
        return Err(LaunchError::ConflictingWindowModes);
    }
    Ok(StartupPlan {
        dpi: config.dpi(),
        single_instance: config.single_instance(),
        width: window.width(),
        height: window.height(),
        maximized: window.is_maximized(),
        fullscreen: window.is_fullscreen(),
        files: config.files().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn startup_plan_is_deterministic() {
        let config = parse_launch_args(&args(&[
            "--dpi",
            "per-monitor",
            "--multi-instance",
            "--window",
            "1280x720",
            "--maximized",
            "a.pdf",
            "b.pdf",
        ]))
        .expect("valid args");
        let plan = resolve_startup_plan(&config).expect("valid plan");
        let again = resolve_startup_plan(&config).expect("valid plan again");
        assert_eq!(plan, again);
        assert_eq!(plan.summary(), again.summary());
        assert_eq!(plan.dpi(), DpiAwareness::PerMonitor);
        assert_eq!(plan.single_instance(), SingleInstanceMode::Multi);
        assert_eq!((plan.width(), plan.height()), (1280, 720));
        assert!(plan.is_maximized());
        assert_eq!(plan.files(), &["a.pdf", "b.pdf"]);
    }

    #[test]
    fn defaults_are_stable() {
        let config = parse_launch_args(&[]).expect("empty args use defaults");
        assert_eq!(config.dpi(), DpiAwareness::PerMonitorV2);
        assert_eq!(config.single_instance(), SingleInstanceMode::Single);
        assert_eq!(
            (config.window().width(), config.window().height()),
            (1024, 768)
        );
        let plan = resolve_startup_plan(&config).expect("valid defaults");
        assert_eq!(
            plan.summary(),
            "dpi=per-monitor-v2; single_instance=single; window=1024x768; \
             maximized=false; fullscreen=false; files="
        );
    }

    #[test]
    fn invalid_dpi_is_rejected() {
        let err = parse_launch_args(&args(&["--dpi", "bogus"])).expect_err("bad DPI");
        assert!(matches!(&err, LaunchError::InvalidDpi(value) if value == "bogus"));
        assert!(err.to_string().contains("invalid DPI preference"));
    }

    #[test]
    fn invalid_args_are_rejected() {
        assert!(matches!(
            parse_launch_args(&args(&["--nope"])),
            Err(LaunchError::UnknownFlag(_))
        ));
        assert!(matches!(
            parse_launch_args(&args(&["--dpi"])),
            Err(LaunchError::MissingValue("--dpi"))
        ));
        assert!(matches!(
            parse_launch_args(&args(&["--window", "wide"])),
            Err(LaunchError::InvalidWindow(_))
        ));
        assert!(matches!(
            parse_launch_args(&args(&["--dpi", "system", "--dpi", "unaware"])),
            Err(LaunchError::DuplicateFlag("--dpi"))
        ));
        assert!(matches!(
            parse_launch_args(&args(&["--file", ""])),
            Err(LaunchError::EmptyFile)
        ));
    }

    #[test]
    fn conflicting_window_modes_are_rejected() {
        let config = parse_launch_args(&args(&["--maximized", "--fullscreen"]))
            .expect("parse accepts both flags");
        let err = resolve_startup_plan(&config).expect_err("conflict");
        assert!(matches!(err, LaunchError::ConflictingWindowModes));
    }

    #[test]
    fn window_dimensions_are_bounded() {
        assert!(matches!(
            WindowPreference::new(0, 100),
            Err(LaunchError::InvalidDimension("width", 0))
        ));
        assert!(matches!(
            WindowPreference::new(10, MAX_WINDOW_DIMENSION.saturating_add(1)),
            Err(LaunchError::InvalidDimension("height", _))
        ));
    }

    #[test]
    fn typed_error_variants_are_distinct() {
        let unknown = LaunchError::UnknownFlag("--x".to_owned());
        let missing = LaunchError::MissingValue("--dpi");
        assert_ne!(unknown, missing);
        assert!(unknown.to_string().contains("--x"));
        assert!(missing.to_string().contains("requires a value"));
    }
}
