//! Signature-request tracker (A-31, slice 1): data model and `mailto:` URL.
//!
//! This slice is the durable half of "Request signature by email": a bounded,
//! tab-separated list of requests under the per-user configuration directory,
//! the match that marks a request signed when a signed copy comes back, and a
//! pure `mailto:` builder. There is no UI and no network here; launching the
//! mail client is an explicit host boundary in [`open_mailto`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::settings;

/// Whether a signature request is still waiting or has been satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignRequestStatus {
    /// Sent to the recipient and not yet matched to a signed document.
    Pending,
    /// A signed copy of the document was matched to this request.
    Signed,
}

impl SignRequestStatus {
    /// The tab-separated field spelling.
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Signed => "signed",
        }
    }

    /// Parse a field spelling; unknown values are rejected as corrupt.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "signed" => Some(Self::Signed),
            _ => None,
        }
    }
}

/// One request that a named recipient sign a specific document.
pub(crate) struct SignRequest {
    /// Stable request number, unique within the tracker file.
    id: u64,
    /// Document that was sent out.
    document: PathBuf,
    /// Recipient address or display text as typed by the user.
    recipient: String,
    /// Mail subject.
    subject: String,
    /// Unix seconds when the request was recorded.
    sent_at_unix: u64,
    /// Current state of the request.
    status: SignRequestStatus,
}

impl SignRequest {
    /// Document that was sent out.
    pub(crate) fn document(&self) -> &Path {
        &self.document
    }

    /// Recipient address or display text as typed by the user.
    pub(crate) fn recipient(&self) -> &str {
        &self.recipient
    }

    /// Current state of the request.
    pub(crate) fn status(&self) -> SignRequestStatus {
        self.status
    }
}

/// The on-disk request list plus its file path.
pub(crate) struct SignRequestTracker {
    /// File the list is saved to and loaded from.
    path: PathBuf,
    /// Requests in insertion order.
    requests: Vec<SignRequest>,
}

/// The header line written before every request row.
const HEADER: &str = "id\tdocument\trecipient\tsubject\tsent_at_unix\tstatus";

impl SignRequestTracker {
    /// Load the tracker at `path`.
    ///
    /// A missing, unreadable or corrupt file yields an empty tracker: this is a
    /// convenience list, so it never fails an open or panics on bad bytes.
    pub(crate) fn load(path: PathBuf) -> Self {
        let requests = std::fs::read_to_string(&path)
            .map(|text| parse_requests(&text))
            .unwrap_or_default();
        Self { path, requests }
    }

    /// Write the tracker, creating the parent directory when needed.
    ///
    /// Fields are escaped with [`escape_field`], so a tab or newline in a
    /// recipient or subject stays inside its column.
    pub(crate) fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::from(HEADER);
        text.push('\n');
        for request in &self.requests {
            text.push_str(&request.id.to_string());
            text.push('\t');
            text.push_str(&escape_field(&request.document.to_string_lossy()));
            text.push('\t');
            text.push_str(&escape_field(&request.recipient));
            text.push('\t');
            text.push_str(&escape_field(&request.subject));
            text.push('\t');
            text.push_str(&request.sent_at_unix.to_string());
            text.push('\t');
            text.push_str(request.status.as_str());
            text.push('\n');
        }
        std::fs::write(&self.path, text)
    }

    /// Record a new pending request and return its id.
    ///
    /// The tracker is not written until [`Self::save`] is called.
    pub(crate) fn record(
        &mut self,
        document: PathBuf,
        recipient: &str,
        subject: &str,
        now_unix: u64,
    ) -> u64 {
        let id = self
            .requests
            .iter()
            .map(|request| request.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.requests.push(SignRequest {
            id,
            document,
            recipient: recipient.to_owned(),
            subject: subject.to_owned(),
            sent_at_unix: now_unix,
            status: SignRequestStatus::Pending,
        });
        id
    }

    /// Mark pending requests signed when a returned copy matches.
    ///
    /// A request matches when its document path equals `document`, or when its
    /// recipient text appears (case-insensitively) in `signer_subject`. Returns
    /// whether at least one request changed.
    pub(crate) fn mark_signed_for(
        &mut self,
        document: &Path,
        signer_subject: Option<&str>,
    ) -> bool {
        let signer_subject = signer_subject.map(str::to_lowercase);
        let mut changed = false;
        for request in &mut self.requests {
            if request.status != SignRequestStatus::Pending {
                continue;
            }
            let path_match = request.document == document;
            let recipient = request.recipient.trim().to_lowercase();
            let subject_match = signer_subject
                .as_ref()
                .is_some_and(|signer| !recipient.is_empty() && signer.contains(&recipient));
            if path_match || subject_match {
                request.status = SignRequestStatus::Signed;
                changed = true;
            }
        }
        changed
    }

    /// Iterate over the requests that are still pending.
    pub(crate) fn pending(&self) -> impl Iterator<Item = &SignRequest> {
        self.requests
            .iter()
            .filter(|request| request.status == SignRequestStatus::Pending)
    }

    /// Every request, in insertion order.
    pub(crate) fn all(&self) -> &[SignRequest] {
        &self.requests
    }

    /// The per-user tracker file.
    ///
    /// Windows uses `%APPDATA%\PDFXI\sign-requests.tsv` (falling back to the
    /// home directory's roaming profile), other systems use
    /// `$XDG_CONFIG_HOME/PDFXI/sign-requests.tsv` or
    /// `~/.config/PDFXI/sign-requests.tsv`. The OS temporary directory is the
    /// last resort when no home directory exists.
    pub(crate) fn tracker_path() -> PathBuf {
        config_root().map_or_else(
            || std::env::temp_dir().join("PDFXI").join("sign-requests.tsv"),
            |root| root.join("PDFXI").join("sign-requests.tsv"),
        )
    }
}

/// The platform configuration root, or `None` when the environment exposes none.
#[cfg(windows)]
fn config_root() -> Option<PathBuf> {
    non_empty_env("APPDATA")
        .map(PathBuf::from)
        .or_else(|| settings::home_dir().map(|home| home.join("AppData").join("Roaming")))
}

/// The platform configuration root, or `None` when the environment exposes none.
#[cfg(not(windows))]
fn config_root() -> Option<PathBuf> {
    non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| settings::home_dir().map(|home| home.join(".config")))
}

/// Read an environment variable, treating an empty value as unset.
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

/// Escape backslashes, tabs and line breaks so one field stays on one row.
fn escape_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Reverse [`escape_field`]; unknown escapes are kept literally.
fn unescape_field(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => unescaped.push('\t'),
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

/// Parse a tracker file, skipping the header, blank lines and corrupt rows.
fn parse_requests(text: &str) -> Vec<SignRequest> {
    let mut requests = Vec::new();
    for line in text.lines() {
        if line == HEADER || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 6 {
            continue;
        }
        let (Ok(id), Ok(sent_at_unix), Some(status)) = (
            fields[0].parse::<u64>(),
            fields[4].parse::<u64>(),
            SignRequestStatus::parse(fields[5]),
        ) else {
            continue;
        };
        requests.push(SignRequest {
            id,
            document: PathBuf::from(unescape_field(fields[1])),
            recipient: unescape_field(fields[2]),
            subject: unescape_field(fields[3]),
            sent_at_unix,
            status,
        });
    }
    requests
}

/// Build the `mailto:` URL for a signature request.
///
/// The recipient is inserted as typed so its `@` stays readable. Subject and
/// body are percent-encoded, including space, `@`, `&`, `=`, `#`, `?` and line
/// breaks (`%0A`). `document_name` is accepted for the caller's message body;
/// it is not added to the URL here.
pub(crate) fn request_mailto_url(
    document_name: &str,
    recipient: &str,
    subject: &str,
    body: &str,
) -> String {
    let _ = document_name;
    format!(
        "mailto:{recipient}?subject={}&body={}",
        percent_encode(subject),
        percent_encode(body)
    )
}

/// Percent-encode everything except unreserved URL characters.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Hand a `mailto:` URL to the platform's mail client.
///
/// This is the only process launch in the module. It does not wait for the
/// mail client and it is never called from tests.
pub(crate) fn open_mailto(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()?;
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status()?;
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(url).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "mail client launcher exited with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique tracker path under `target/tmp/sign-requests/`.
    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/sign-requests");
        std::fs::create_dir_all(&dir).expect("create the temporary directory");
        dir.join(format!("tracker-{}-{tag}-{unique}.tsv", std::process::id()))
    }

    #[test]
    fn mailto_url_encodes_subject_body_and_keeps_recipient_at() {
        let url = request_mailto_url(
            "contract.pdf",
            "ada@example.com",
            "Please sign & return\nThanks",
            "Line #1 = ready?",
        );
        assert!(url.starts_with("mailto:ada@example.com?subject="));
        assert!(url.contains("subject=Please%20sign%20%26%20return%0AThanks"));
        assert!(url.contains("body=Line%20%231%20%3D%20ready%3F"));
    }

    #[test]
    fn records_survive_save_and_load_as_pending() {
        let path = temp_path("roundtrip");
        let mut tracker = SignRequestTracker::load(path.clone());
        let first = tracker.record(
            PathBuf::from(r"C:\docs\a.pdf"),
            "ada@example.com",
            "Sign A",
            1_700_000_000,
        );
        let second = tracker.record(
            PathBuf::from(r"C:\docs\b.pdf"),
            "grace@example.com",
            "Sign B",
            1_700_000_100,
        );
        assert_eq!((first, second), (1, 2));
        tracker.save().expect("save the tracker");

        let loaded = SignRequestTracker::load(path.clone());
        assert_eq!(loaded.all().len(), 2);
        assert_eq!(loaded.pending().count(), 2);
        assert_eq!(loaded.all()[0].document, PathBuf::from(r"C:\docs\a.pdf"));
        assert_eq!(loaded.all()[0].recipient, "ada@example.com");
        assert_eq!(loaded.all()[0].subject, "Sign A");
        assert_eq!(loaded.all()[0].sent_at_unix, 1_700_000_000);
        assert_eq!(loaded.all()[0].status, SignRequestStatus::Pending);
        assert_eq!(loaded.all()[1].document, PathBuf::from(r"C:\docs\b.pdf"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mark_signed_for_matches_document_or_signer_subject() {
        let path = temp_path("mark");
        let mut tracker = SignRequestTracker::load(path.clone());
        tracker.record(PathBuf::from("a.pdf"), "ada@example.com", "Sign A", 1);
        tracker.record(PathBuf::from("b.pdf"), "grace@example.com", "Sign B", 2);

        assert!(tracker.mark_signed_for(Path::new("a.pdf"), None));
        assert_eq!(tracker.all()[0].status, SignRequestStatus::Signed);
        assert_eq!(tracker.all()[1].status, SignRequestStatus::Pending);
        assert!(
            !tracker.mark_signed_for(Path::new("a.pdf"), None),
            "an already-signed request is not a change"
        );

        tracker.record(PathBuf::from("c.pdf"), "linus@example.com", "Sign C", 3);
        assert!(tracker.mark_signed_for(
            Path::new("elsewhere.pdf"),
            Some("CN=Linus Torvalds, E=linus@example.com"),
        ));
        assert_eq!(tracker.all()[2].status, SignRequestStatus::Signed);
        assert_eq!(tracker.pending().count(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_tracker_file_loads_as_empty() {
        let path = temp_path("corrupt");
        std::fs::write(
            &path,
            "id\tdocument\trecipient\nnot-a-number\t\t\t\t\t\n\u{0}\u{0}",
        )
        .expect("write a corrupt tracker");
        let tracker = SignRequestTracker::load(path.clone());
        assert!(tracker.all().is_empty());
        assert_eq!(tracker.pending().count(), 0);
        let _ = std::fs::remove_file(&path);
    }
}
