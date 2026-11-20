//! Service discovery and endpoint validation.
//!
//! Two sources are supported:
//!
//! 1. the local service registration written by OpenCode at
//!    `<home>/.local/state/opencode/service.json`, which must be loopback; and
//! 2. an explicit override supplied by the user for a remote service, which
//!    must use `https` unless it is loopback.
//!
//! The password is read from the registration or the override and is never
//! printed. [`ServiceEndpoint`] implements [`Debug`] manually so a stray
//! `dbg!` or log line cannot leak it.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::{Host, Url};

use crate::error::AssistantError;
use crate::secret::Secret;

/// Where an endpoint came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSource {
    /// A local service registration file.
    Registration {
        /// File the registration was read from.
        path: PathBuf,
        /// Registration id, used later to key Credential Manager storage.
        id: String,
        /// Service version recorded in the registration.
        version: String,
        /// Process id recorded in the registration.
        pid: Option<u32>,
    },
    /// A URL and password supplied by the user for a remote service.
    Explicit,
}

/// A validated OpenCode service endpoint with its credential.
#[derive(Clone)]
pub struct ServiceEndpoint {
    url: Url,
    password: Secret,
    source: EndpointSource,
    loopback: bool,
}

impl ServiceEndpoint {
    /// Discovers the local service from the default registration path.
    pub fn discover() -> Result<Self, AssistantError> {
        let path = default_registration_path()?;
        Self::discover_from_file(&path)
    }

    /// Discovers the local service from a specific registration file.
    pub fn discover_from_file(path: &Path) -> Result<Self, AssistantError> {
        let text = fs::read_to_string(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AssistantError::ServiceNotFound {
                path: path.display().to_string(),
            },
            _ => AssistantError::Io {
                message: format!("cannot read {}: {error}", path.display()),
            },
        })?;
        let registration: RegistrationFile =
            serde_json::from_str(&text).map_err(|error| AssistantError::RegistrationInvalid {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        registration.into_endpoint(path)
    }

    /// Builds a user-supplied remote endpoint.
    ///
    /// A non-loopback endpoint must use `https`: the Basic password is the
    /// service's full credential and must not cross a network in clear text.
    pub fn explicit(url: &str, password: impl Into<String>) -> Result<Self, AssistantError> {
        let password: String = password.into();
        if password.is_empty() {
            return Err(AssistantError::InvalidArgument {
                message: "the service password is empty".to_owned(),
            });
        }
        let url = parse_base_url(url)?;
        let loopback = url_is_loopback(&url);
        if !loopback && url.scheme() != "https" {
            return Err(AssistantError::InsecureRemoteEndpoint {
                url: url.as_str().to_owned(),
            });
        }
        Ok(Self {
            url,
            password: Secret::new(password),
            source: EndpointSource::Explicit,
            loopback,
        })
    }

    /// The base URL, without a trailing slash.
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Where this endpoint came from.
    pub fn source(&self) -> &EndpointSource {
        &self.source
    }

    /// Whether the endpoint is on the local machine.
    pub fn is_loopback(&self) -> bool {
        self.loopback
    }

    /// The registration id, when the endpoint came from a registration file.
    pub fn registration_id(&self) -> Option<&str> {
        match &self.source {
            EndpointSource::Registration { id, .. } => Some(id.as_str()),
            EndpointSource::Explicit => None,
        }
    }

    /// The registration version, when the endpoint came from a file.
    pub fn registration_version(&self) -> Option<&str> {
        match &self.source {
            EndpointSource::Registration { version, .. } => Some(version.as_str()),
            EndpointSource::Explicit => None,
        }
    }

    /// The credential. Only the transport reads it, to build the Basic header.
    pub(crate) fn password(&self) -> &Secret {
        &self.password
    }
}

impl fmt::Debug for ServiceEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceEndpoint")
            .field("url", &self.url.as_str())
            .field("source", &self.source)
            .field("loopback", &self.loopback)
            .field("password", &self.password)
            .finish()
    }
}

/// The default registration path for this user.
///
/// Resolved from the environment at runtime (`USERPROFILE`, then `HOME`), never
/// hard-coded, so the crate works for any user on any machine.
pub fn default_registration_path() -> Result<PathBuf, AssistantError> {
    let home = home_directory().ok_or(AssistantError::Configuration {
        message: "cannot resolve the home directory: neither USERPROFILE nor HOME is set"
            .to_owned(),
    })?;
    Ok(registration_path_for(&home))
}

fn registration_path_for(home: &Path) -> PathBuf {
    home.join(".local")
        .join("state")
        .join("opencode")
        .join("service.json")
}

fn home_directory() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"].iter().find_map(|name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
    })
}

/// Registration file shape written by OpenCode 2.3.2.
#[derive(Debug, Deserialize)]
struct RegistrationFile {
    id: String,
    version: String,
    url: String,
    #[serde(default)]
    pid: Option<u32>,
    password: String,
}

impl RegistrationFile {
    fn into_endpoint(self, path: &Path) -> Result<ServiceEndpoint, AssistantError> {
        let invalid = |reason: &str| AssistantError::RegistrationInvalid {
            path: path.display().to_string(),
            reason: reason.to_owned(),
        };
        if self.id.trim().is_empty() {
            return Err(invalid("the id field is empty"));
        }
        if self.version.trim().is_empty() {
            return Err(invalid("the version field is empty"));
        }
        if self.password.is_empty() {
            return Err(invalid("the password field is empty"));
        }
        let url =
            parse_base_url(&self.url).map_err(|error| AssistantError::RegistrationInvalid {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        if !url_is_loopback(&url) {
            return Err(AssistantError::NonLoopbackEndpoint {
                url: url.as_str().to_owned(),
            });
        }
        Ok(ServiceEndpoint {
            url,
            password: Secret::new(self.password),
            source: EndpointSource::Registration {
                path: path.to_path_buf(),
                id: self.id,
                version: self.version,
                pid: self.pid,
            },
            loopback: true,
        })
    }
}

fn parse_base_url(raw: &str) -> Result<Url, AssistantError> {
    let url = Url::parse(raw).map_err(|error| AssistantError::InvalidEndpoint {
        reason: format!("{raw:?} is not a URL: {error}"),
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AssistantError::InvalidEndpoint {
            reason: "the URL must not embed credentials".to_owned(),
        });
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AssistantError::InvalidEndpoint {
            reason: format!("scheme {:?} is not http or https", url.scheme()),
        });
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AssistantError::InvalidEndpoint {
            reason: "the URL must not carry a query or fragment".to_owned(),
        });
    }
    if !matches!(url.path(), "" | "/") {
        return Err(AssistantError::InvalidEndpoint {
            reason: format!(
                "path {:?} is not supported; use the origin only",
                url.path()
            ),
        });
    }
    if url.host().is_none() {
        return Err(AssistantError::InvalidEndpoint {
            reason: "the URL has no host".to_owned(),
        });
    }
    Ok(url)
}

fn url_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_registration(contents: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "pdfxi-assistant-registration-{}-{unique}.json",
            std::process::id()
        ));
        let mut file = fs::File::create(&path).expect("create registration");
        file.write_all(contents.as_bytes())
            .expect("write registration");
        path
    }

    const VALID: &str = r#"{
        "id":"4500097a-8e06-46d1-b79c-5e5b5bc3c663",
        "version":"2.3.2",
        "url":"http://127.0.0.1:28894",
        "pid":17164,
        "password":"top-secret-password"
    }"#;

    #[test]
    fn registration_path_is_derived_from_home() {
        let path = registration_path_for(Path::new("C:/Users/example"));
        assert!(path.ends_with(Path::new(".local/state/opencode/service.json")));
        assert!(path.starts_with(Path::new("C:/Users/example")));
    }

    #[test]
    fn reads_a_valid_registration() {
        let path = temp_registration(VALID);
        let endpoint = ServiceEndpoint::discover_from_file(&path).expect("endpoint");
        assert_eq!(endpoint.url(), "http://127.0.0.1:28894/");
        assert!(endpoint.is_loopback());
        assert_eq!(
            endpoint.registration_id(),
            Some("4500097a-8e06-46d1-b79c-5e5b5bc3c663")
        );
        assert_eq!(endpoint.registration_version(), Some("2.3.2"));
        assert!(matches!(
            endpoint.source(),
            EndpointSource::Registration {
                pid: Some(17164),
                ..
            }
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn debug_redacts_the_password() {
        let path = temp_registration(VALID);
        let endpoint = ServiceEndpoint::discover_from_file(&path).expect("endpoint");
        let rendered = format!("{endpoint:?}");
        assert!(!rendered.contains("top-secret-password"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("http://127.0.0.1:28894/"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_registration_is_typed() {
        let path = std::env::temp_dir().join("pdfxi-assistant-definitely-missing.json");
        let _ = fs::remove_file(&path);
        let error = ServiceEndpoint::discover_from_file(&path).expect_err("missing");
        assert!(matches!(error, AssistantError::ServiceNotFound { .. }));
    }

    #[test]
    fn malformed_registration_is_typed() {
        let path = temp_registration("{not json");
        let error = ServiceEndpoint::discover_from_file(&path).expect_err("malformed");
        assert!(matches!(error, AssistantError::RegistrationInvalid { .. }));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn registration_without_password_is_rejected() {
        let path = temp_registration(
            r#"{"id":"x","version":"2.3.2","url":"http://127.0.0.1:1","password":""}"#,
        );
        let error = ServiceEndpoint::discover_from_file(&path).expect_err("empty password");
        assert!(matches!(error, AssistantError::RegistrationInvalid { .. }));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn discovered_registration_must_be_loopback() {
        let path = temp_registration(
            r#"{"id":"x","version":"2.3.2","url":"https://example.test","password":"p"}"#,
        );
        let error = ServiceEndpoint::discover_from_file(&path).expect_err("remote");
        assert!(matches!(error, AssistantError::NonLoopbackEndpoint { .. }));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn localhost_and_ipv6_loopback_are_accepted() {
        for url in ["http://localhost:9000", "http://[::1]:9000"] {
            let endpoint = ServiceEndpoint::explicit(url, "p").expect("loopback");
            assert!(endpoint.is_loopback());
        }
    }

    #[test]
    fn explicit_remote_endpoint_requires_https() {
        let error = ServiceEndpoint::explicit("http://opencode.example.test", "p")
            .expect_err("plaintext remote");
        assert!(matches!(
            error,
            AssistantError::InsecureRemoteEndpoint { .. }
        ));
        assert!(ServiceEndpoint::explicit("https://opencode.example.test", "p").is_ok());
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        let error = ServiceEndpoint::explicit("file:///etc/passwd", "p").expect_err("scheme");
        assert!(matches!(error, AssistantError::InvalidEndpoint { .. }));
    }

    #[test]
    fn embedded_credentials_query_and_path_are_rejected() {
        assert!(ServiceEndpoint::explicit("http://user:pass@127.0.0.1:1", "p").is_err());
        assert!(ServiceEndpoint::explicit("http://127.0.0.1:1/?x=1", "p").is_err());
        assert!(ServiceEndpoint::explicit("http://127.0.0.1:1/api", "p").is_err());
    }

    #[test]
    fn empty_explicit_password_is_rejected() {
        let error = ServiceEndpoint::explicit("http://127.0.0.1:1", "").expect_err("empty");
        assert!(matches!(error, AssistantError::InvalidArgument { .. }));
    }
}
