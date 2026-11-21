//! Local notifications (A-02): a bounded on-disk list of real app events.
//!
//! Every entry is written by an action that actually happened (open, save,
//! export, signature request, returned signed copy, print completion). The
//! list is a tab-separated file under the PDFXI configuration directory; a
//! missing or corrupt file loads empty. Notification failures are swallowed so
//! they can never break the action that raised them.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ui::PdfxiApp;

/// Maximum notifications kept on disk; the oldest are dropped first.
pub(crate) const MAX_NOTIFICATIONS: usize = 100;

/// Shown by the window when the store has no entries.
pub(crate) const EMPTY_MESSAGE: &str = "No notifications yet.";

/// One recorded local event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notification {
    /// Monotonic id within the store file.
    pub(crate) id: u64,
    /// Short event title.
    pub(crate) title: String,
    /// Human-readable detail.
    pub(crate) detail: String,
    /// Unix seconds when the event was recorded.
    pub(crate) unix_time: u64,
    /// Whether the user has seen it.
    pub(crate) read: bool,
}

impl Notification {
    /// Recorded time formatted as `YYYY-MM-DD HH:MM UTC`.
    #[must_use]
    pub(crate) fn time_label(&self) -> String {
        format_unix_time(self.unix_time)
    }
}

/// The on-disk notification list plus its file path.
pub(crate) struct NotificationStore {
    /// File the list is saved to and loaded from.
    path: PathBuf,
    /// Notifications in insertion order.
    notifications: Vec<Notification>,
}

impl NotificationStore {
    /// Load the store at `path`.
    ///
    /// A missing, unreadable or corrupt file yields an empty store; this is a
    /// convenience list and never fails an app action. Files holding more than
    /// [`MAX_NOTIFICATIONS`] entries are truncated to the newest ones.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut notifications = std::fs::read_to_string(&path)
            .map(|text| parse_notifications(&text))
            .unwrap_or_default();
        if notifications.len() > MAX_NOTIFICATIONS {
            notifications.drain(..notifications.len() - MAX_NOTIFICATIONS);
        }
        Self {
            path,
            notifications,
        }
    }

    /// Write the store, creating the parent directory when needed.
    pub(crate) fn save(&self) -> io::Result<()> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for notification in &self.notifications {
            text.push_str(&notification.id.to_string());
            text.push('\t');
            text.push_str(&escape_field(&notification.title));
            text.push('\t');
            text.push_str(&escape_field(&notification.detail));
            text.push('\t');
            text.push_str(&notification.unix_time.to_string());
            text.push('\t');
            text.push_str(if notification.read { "read" } else { "unread" });
            text.push('\n');
        }
        std::fs::write(&self.path, text)
    }

    /// Record one unread notification and return its id.
    ///
    /// The store is not written until [`Self::save`] is called.
    pub(crate) fn record(&mut self, title: &str, detail: &str) -> u64 {
        let id = self
            .notifications
            .iter()
            .map(|notification| notification.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.notifications.push(Notification {
            id,
            title: title.to_owned(),
            detail: detail.to_owned(),
            unix_time: unix_now(),
            read: false,
        });
        if self.notifications.len() > MAX_NOTIFICATIONS {
            let excess = self.notifications.len() - MAX_NOTIFICATIONS;
            self.notifications.drain(..excess);
        }
        id
    }

    /// Mark every entry read.
    pub(crate) fn mark_all_read(&mut self) {
        for notification in &mut self.notifications {
            notification.read = true;
        }
    }

    /// Number of unread entries.
    #[must_use]
    pub(crate) fn unread_count(&self) -> usize {
        self.notifications
            .iter()
            .filter(|notification| !notification.read)
            .count()
    }

    /// Every entry, oldest first.
    #[must_use]
    pub(crate) fn all(&self) -> &[Notification] {
        &self.notifications
    }

    /// The per-user store file.
    ///
    /// Windows uses `%APPDATA%\PDFXI\notifications.tsv` (falling back to the
    /// home directory's roaming profile), other systems use
    /// `$XDG_CONFIG_HOME/PDFXI/notifications.tsv` or
    /// `~/.config/PDFXI/notifications.tsv`. The OS temporary directory is the
    /// last resort.
    pub(crate) fn platform_path() -> PathBuf {
        config_root().map_or_else(
            || std::env::temp_dir().join("PDFXI").join("notifications.tsv"),
            |root| root.join("PDFXI").join("notifications.tsv"),
        )
    }
}

/// The platform configuration root, or `None` when the environment exposes none.
#[cfg(windows)]
fn config_root() -> Option<PathBuf> {
    non_empty_env("APPDATA")
        .map(PathBuf::from)
        .or_else(|| crate::settings::home_dir().map(|home| home.join("AppData").join("Roaming")))
}

/// The platform configuration root, or `None` when the environment exposes none.
#[cfg(not(windows))]
fn config_root() -> Option<PathBuf> {
    non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::settings::home_dir().map(|home| home.join(".config")))
}

/// Read an environment variable, treating an empty value as unset.
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

/// State of the Notifications window and its store.
#[derive(Default)]
pub(crate) struct NotificationsUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// Loaded store.
    pub(crate) store: Option<NotificationStore>,
    /// Store path; tests and captures inject a temporary path.
    pub(crate) path: Option<PathBuf>,
    /// Typed load error, if any.
    pub(crate) error: Option<String>,
}

impl PdfxiApp {
    /// Point the notification store at `path`.
    ///
    /// Capture and test isolation use this; the next use reloads from the new
    /// path. The shipped shell keeps the platform path.
    pub fn set_notifications_path(&mut self, path: PathBuf) {
        self.notifications.path = Some(path);
        self.notifications.store = None;
        self.notifications.error = None;
    }

    /// Load the store from the active path (platform path when none is set).
    pub(crate) fn load_notifications(&mut self) {
        let path = self
            .notifications
            .path
            .clone()
            .unwrap_or_else(NotificationStore::platform_path);
        self.notifications.path = Some(path.clone());
        self.notifications.store = Some(NotificationStore::load(path));
        self.notifications.error = None;
    }

    /// Toggle the Notifications window. Loading happens on first draw.
    pub(crate) fn toggle_notifications(&mut self) {
        self.notifications.open = !self.notifications.open;
        if self.notifications.open {
            self.load_notifications();
            self.status = "Notifications".to_owned();
        } else {
            self.status = "Notifications closed".to_owned();
        }
    }

    /// Whether the Notifications window is open (harness accessor).
    #[must_use]
    pub fn notifications_open(&self) -> bool {
        self.notifications.open
    }

    /// Number of stored notifications (0 when nothing is loaded).
    #[must_use]
    pub fn notification_count(&self) -> usize {
        self.notifications
            .store
            .as_ref()
            .map_or(0, |store| store.all().len())
    }

    /// Number of unread notifications in the loaded store.
    #[must_use]
    pub fn unread_notification_count(&self) -> usize {
        self.notifications
            .store
            .as_ref()
            .map_or(0, NotificationStore::unread_count)
    }

    /// The empty-list message, when the open window has nothing to show.
    #[must_use]
    pub fn notifications_empty_label(&self) -> Option<&'static str> {
        let empty = self
            .notifications
            .store
            .as_ref()
            .is_none_or(|store| store.all().is_empty());
        (self.notifications.open && empty).then_some(EMPTY_MESSAGE)
    }

    /// Unread count for the title-bar badge, loading the store on first use.
    pub(crate) fn notification_badge_count(&mut self) -> usize {
        if self.notifications.store.is_none() {
            self.load_notifications();
        }
        self.unread_notification_count()
    }

    /// Record a real event and persist it. Failures are deliberately ignored.
    pub(crate) fn notify_real_event(&mut self, title: &str, detail: &str) {
        if self.notifications.store.is_none() {
            self.load_notifications();
        }
        if let Some(store) = self.notifications.store.as_mut() {
            store.record(title, detail);
            if store.save().is_err() {
                // A notification must never break the action that raised it.
                self.notifications.error = Some("Could not save notifications".to_owned());
            }
        }
    }

    /// Draw the Notifications window.
    pub(crate) fn notifications_ui(&mut self, ctx: &egui::Context) {
        if !self.notifications.open {
            return;
        }
        if self.notifications.store.is_none() {
            self.load_notifications();
        }
        // Opening the window clears the unread state once.
        if let Some(store) = self.notifications.store.as_mut()
            && store.unread_count() > 0
        {
            store.mark_all_read();
            let _ = store.save();
        }
        let mut open = true;
        egui::Window::new("Notifications")
            .open(&mut open)
            .resizable(true)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.heading("Notifications");
                if let Some(error) = &self.notifications.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let rows: Vec<(String, String, String, bool)> = self
                    .notifications
                    .store
                    .as_ref()
                    .map(|store| {
                        store
                            .all()
                            .iter()
                            .map(|notification| {
                                (
                                    notification.title.clone(),
                                    notification.detail.clone(),
                                    notification.time_label(),
                                    notification.read,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if rows.is_empty() {
                    ui.label(EMPTY_MESSAGE);
                }
                for (title, detail, time, read) in &rows {
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(detail);
                    ui.label(egui::RichText::new(time).size(10.0).weak());
                    if !read {
                        ui.label("unread");
                    }
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    if ui.button("Mark all read").clicked()
                        && let Some(store) = self.notifications.store.as_mut()
                    {
                        store.mark_all_read();
                        let _ = store.save();
                    }
                });
            });
        if !open {
            self.notifications.open = false;
        }
    }

    /// A path unique to this test app, used by [`Self::new`] so headless runs
    /// never touch the user's real notification file.
    pub(crate) fn default_test_notifications_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join("PDFXI")
            .join("notifications-tests")
            .join(format!("{}-{unique}.tsv", std::process::id()))
    }
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

/// Parse a store file, skipping blank and corrupt rows.
fn parse_notifications(text: &str) -> Vec<Notification> {
    let mut notifications = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 5 {
            continue;
        }
        let (Ok(id), Ok(unix_time)) = (fields[0].parse::<u64>(), fields[3].parse::<u64>()) else {
            continue;
        };
        let read = match fields[4] {
            "read" => true,
            "unread" => false,
            _ => continue,
        };
        notifications.push(Notification {
            id,
            title: unescape_field(fields[1]),
            detail: unescape_field(fields[2]),
            unix_time,
            read,
        });
    }
    notifications
}

/// Current Unix seconds, or 0 when the clock is before the epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Format Unix seconds as `YYYY-MM-DD HH:MM UTC`.
fn format_unix_time(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let seconds = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60
    )
}

/// Convert days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// A path under `target/tmp/notifications/` for unit tests.
#[cfg(test)]
fn temp_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/notifications");
    std::fs::create_dir_all(&dir).expect("create the temporary directory");
    dir.join(format!("{}-{tag}-{unique}.tsv", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_save_load_round_trip_keeps_unread_count() {
        let path = temp_path("roundtrip");
        let mut store = NotificationStore::load(path.clone());
        let first = store.record("Document opened", "report.pdf");
        let second = store.record("Document saved", "report.pdf");
        assert_eq!((first, second), (1, 2));
        assert_eq!(store.unread_count(), 2);
        store.save().expect("save");

        let loaded = NotificationStore::load(path.clone());
        assert_eq!(loaded.all().len(), 2);
        assert_eq!(loaded.unread_count(), 2);
        assert_eq!(loaded.all()[0].title, "Document opened");
        assert_eq!(loaded.all()[0].detail, "report.pdf");
        assert!(!loaded.all()[0].read);
        assert!(loaded.all()[0].unix_time > 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mark_all_read_persists() {
        let path = temp_path("read");
        let mut store = NotificationStore::load(path.clone());
        store.record("Document opened", "a.pdf");
        store.record("Export finished", "a.txt");
        store.mark_all_read();
        store.save().expect("save");

        let loaded = NotificationStore::load(path.clone());
        assert_eq!(loaded.unread_count(), 0);
        assert!(loaded.all().iter().all(|notification| notification.read));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_cap_drops_the_oldest_entries() {
        let path = temp_path("cap");
        let mut store = NotificationStore::load(path.clone());
        for index in 0..(MAX_NOTIFICATIONS + 5) {
            store.record("Event", &format!("item {index}"));
        }
        assert_eq!(store.all().len(), MAX_NOTIFICATIONS);
        assert_eq!(store.all()[0].detail, "item 5");
        assert_eq!(
            store.all()[MAX_NOTIFICATIONS - 1].detail,
            format!("item {}", MAX_NOTIFICATIONS + 4)
        );
        store.save().expect("save");
        let loaded = NotificationStore::load(path.clone());
        assert_eq!(loaded.all().len(), MAX_NOTIFICATIONS);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn escaping_round_trips_tabs_newlines_and_backslashes() {
        let path = temp_path("escape");
        let mut store = NotificationStore::load(path.clone());
        store.record("Tab\there", "line one\nline two\\end");
        store.save().expect("save");

        let loaded = NotificationStore::load(path.clone());
        assert_eq!(loaded.all()[0].title, "Tab\there");
        assert_eq!(loaded.all()[0].detail, "line one\nline two\\end");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_loads_empty() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "not-a-number\t\t\t\t\n\u{0}\u{0}\n1\tok\tbad\n")
            .expect("write a corrupt store");
        let store = NotificationStore::load(path.clone());
        assert!(store.all().is_empty());
        assert_eq!(store.unread_count(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn opening_a_real_fixture_records_document_opened() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let path = temp_path("open");
        let mut app = PdfxiApp::new();
        app.set_notifications_path(path.clone());
        app.open_document(&fixture);

        let loaded = NotificationStore::load(path.clone());
        let opened = loaded
            .all()
            .iter()
            .find(|notification| notification.title == "Document opened")
            .expect("opening a document records a notification");
        assert!(opened.detail.contains("microsoft-excel-2019-sample"));
        assert!(!opened.read);
        let _ = std::fs::remove_file(&path);
    }
}
