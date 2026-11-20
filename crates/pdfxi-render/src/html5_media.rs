//! HTML5 media replacement envelope (feature MEDIA-002).
//!
//! Flash-based PDF multimedia is retired. This module models a *local, HTML5*
//! replacement envelope: a bounded set of audio/video sources plus playback
//! options, validated so that every referenced medium is local or embedded. It
//! produces a deterministic [`Html5Descriptor`] and optional HTML5 markup for a
//! host to display; it performs no network access, executes no script, and never
//! decodes a codec.
//!
//! # Backend limits (honest delegation)
//!
//! [`Html5Descriptor::to_markup`] returns an inert markup *description* of a
//! native `<audio>`/`<video>` element. The host decides whether and how to
//! render it. Remote URLs, data/blob URLs, absolute paths and traversal paths
//! are rejected with typed errors rather than silently accepted, so a hostile
//! PDF cannot turn a "local replacement" into a network fetch or a filesystem
//! escape.
//!
//! All operations are pure, bounded and deterministic, add no dependency, and
//! need no cancellation token because a single parse is short.

/// Maximum byte length of a single source/poster reference.
pub const MAX_SOURCE_BYTES: usize = 512;
/// Maximum byte length of a MIME type string.
pub const MAX_MIME_BYTES: usize = 64;
/// Maximum number of `<source>` entries in one envelope.
pub const MAX_SOURCES: usize = 16;
/// Maximum accepted element width or height, in CSS pixels.
pub const MAX_DIMENSION: u32 = 8192;

/// Structured errors produced while validating an HTML5 media envelope.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Html5MediaError {
    /// A source reference was empty.
    #[error("media source must not be empty")]
    EmptySource,
    /// A source reference exceeded the byte-length cap.
    #[error("media source length {len} exceeds the {limit}-byte limit")]
    SourceTooLong {
        /// Actual byte length of the reference.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// The reference is a remote or protocol-relative URL.
    #[error("remote media source '{0}' is not allowed")]
    RemoteSource(String),
    /// The reference uses an unsupported URL scheme.
    #[error("media source scheme '{0}' is not supported")]
    UnsupportedScheme(String),
    /// The reference is an absolute filesystem path.
    #[error("absolute media path '{0}' is not allowed")]
    AbsolutePath(String),
    /// The reference contains a parent-directory traversal segment.
    #[error("media path '{0}' contains a parent-directory traversal")]
    TraversalPath(String),
    /// The reference contains a character unsafe for markup attributes,
    /// including control characters that would break an attribute value.
    #[error("media source '{0}' contains an unsafe character")]
    UnsafeCharacter(String),
    /// A MIME type was empty.
    #[error("media MIME type must not be empty")]
    EmptyMimeType,
    /// A MIME type was not one of the supported audio/video types.
    #[error("unsupported media MIME type '{0}'")]
    UnsupportedMimeType(String),
    /// No sources were declared.
    #[error("an HTML5 media envelope needs at least one source")]
    NoSources,
    /// More sources were declared than the cap allows.
    #[error("source count {count} exceeds the {limit}-source limit")]
    TooManySources {
        /// Number of sources supplied.
        count: usize,
        /// Enforced source limit.
        limit: usize,
    },
    /// Two sources referenced the same medium.
    #[error("duplicate media source '{0}'")]
    DuplicateSource(String),
    /// A declared dimension exceeded the cap.
    #[error("media dimension {value} exceeds the {limit}-pixel limit")]
    DimensionTooLarge {
        /// The rejected dimension.
        value: u32,
        /// Enforced dimension limit.
        limit: u32,
    },
}

/// Whether a validated source is audio or video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCategory {
    /// An audio-only medium (`audio/*`).
    Audio,
    /// A video medium (`video/*`).
    Video,
}

impl MediaCategory {
    /// Return the stable HTML element tag for this category.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

/// A validated, local-or-embedded medium reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalMediaRef {
    /// An embedded stream, marked by a leading `@`.
    Embedded(String),
    /// A relative local path with no traversal segments.
    RelativePath(String),
}

impl LocalMediaRef {
    /// Return a byte-stable descriptor that distinguishes embedded streams
    /// from relative paths.
    pub fn descriptor(&self) -> String {
        match self {
            Self::Embedded(name) => {
                let mut out = String::from("embedded:");
                push_field(&mut out, name);
                out
            }
            Self::RelativePath(path) => {
                let mut out = String::from("relative:");
                push_field(&mut out, path);
                out
            }
        }
    }
}

/// Owned intermediate form of one `<source>` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Html5SourceInput {
    /// The source reference (`@embedded` or a relative local path).
    pub src: String,
    /// The MIME type advertised for this source.
    pub mime_type: String,
}

/// Owned intermediate form of an HTML5 media replacement envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Html5EnvelopeInput {
    /// The candidate sources, in preference order.
    pub sources: Vec<Html5SourceInput>,
    /// An optional local poster image reference.
    pub poster: Option<String>,
    /// Whether native playback controls should be shown.
    pub controls: bool,
    /// Whether playback should start automatically.
    pub autoplay: bool,
    /// Whether playback should loop.
    pub loop_play: bool,
    /// Whether playback should start muted.
    pub muted: bool,
    /// Element width in CSS pixels (`0` means unspecified).
    pub width: u32,
    /// Element height in CSS pixels (`0` means unspecified).
    pub height: u32,
}

/// A validated `<source>` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Html5Source {
    reference: LocalMediaRef,
    mime_type: String,
    category: MediaCategory,
}

impl Html5Source {
    /// Return the validated local reference.
    pub fn reference(&self) -> &LocalMediaRef {
        &self.reference
    }

    /// Return the MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Return whether this source is audio or video.
    pub fn category(&self) -> MediaCategory {
        self.category
    }
}

/// A deterministic descriptor for a local HTML5 media player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Html5Descriptor {
    sources: Vec<Html5Source>,
    poster: Option<LocalMediaRef>,
    controls: bool,
    autoplay: bool,
    loop_play: bool,
    muted: bool,
    width: u32,
    height: u32,
    element: MediaCategory,
}

impl Html5Descriptor {
    /// Validate and build a descriptor from an intermediate envelope.
    pub fn parse(input: Html5EnvelopeInput) -> Result<Self, Html5MediaError> {
        if input.sources.is_empty() {
            return Err(Html5MediaError::NoSources);
        }
        if input.sources.len() > MAX_SOURCES {
            return Err(Html5MediaError::TooManySources {
                count: input.sources.len(),
                limit: MAX_SOURCES,
            });
        }
        if input.width > MAX_DIMENSION {
            return Err(Html5MediaError::DimensionTooLarge {
                value: input.width,
                limit: MAX_DIMENSION,
            });
        }
        if input.height > MAX_DIMENSION {
            return Err(Html5MediaError::DimensionTooLarge {
                value: input.height,
                limit: MAX_DIMENSION,
            });
        }

        let poster = match &input.poster {
            Some(poster) => Some(validate_reference(poster)?),
            None => None,
        };

        let mut seen: Vec<&str> = Vec::with_capacity(input.sources.len());
        let mut sources = Vec::with_capacity(input.sources.len());
        let mut element = MediaCategory::Audio;
        for source in &input.sources {
            if seen.contains(&source.src.as_str()) {
                return Err(Html5MediaError::DuplicateSource(source.src.clone()));
            }
            seen.push(source.src.as_str());
            let reference = validate_reference(&source.src)?;
            let category = classify_mime(&source.mime_type)?;
            if category == MediaCategory::Video {
                element = MediaCategory::Video;
            }
            sources.push(Html5Source {
                reference,
                mime_type: source.mime_type.clone(),
                category,
            });
        }

        Ok(Self {
            sources,
            poster,
            controls: input.controls,
            autoplay: input.autoplay,
            loop_play: input.loop_play,
            muted: input.muted,
            width: input.width,
            height: input.height,
            element,
        })
    }

    /// Return the validated sources in preference order.
    pub fn sources(&self) -> &[Html5Source] {
        &self.sources
    }

    /// Return the optional local poster reference.
    pub fn poster(&self) -> Option<&LocalMediaRef> {
        self.poster.as_ref()
    }

    /// Return whether native playback controls are shown.
    pub fn controls(&self) -> bool {
        self.controls
    }

    /// Return whether playback starts automatically.
    pub fn autoplay(&self) -> bool {
        self.autoplay
    }

    /// Return whether playback loops.
    pub fn loop_play(&self) -> bool {
        self.loop_play
    }

    /// Return whether playback starts muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Return the element width in CSS pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Return the element height in CSS pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Return the HTML element kind implied by the sources.
    pub fn element(&self) -> MediaCategory {
        self.element
    }

    /// Return a byte-stable, single-line descriptor of this envelope.
    ///
    /// Sources are kept in preference order and all text is escaped, so equal
    /// envelopes give an identical string and differing ones cannot collide.
    /// This is an identity/cache key for a host, not a markup or wire format;
    /// use [`Self::to_markup`] for the inert element description.
    pub fn descriptor(&self) -> String {
        let mut out = String::from("html5-plan/v1|element=");
        out.push_str(self.element.code());
        out.push_str("|sources=");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&source.reference().descriptor());
            out.push('(');
            push_field(&mut out, &source.mime_type);
            out.push(')');
        }
        out.push_str("|poster=");
        match &self.poster {
            Some(poster) => out.push_str(&poster.descriptor()),
            None => out.push('-'),
        }
        out.push_str("|controls=");
        out.push_str(if self.controls { "1" } else { "0" });
        out.push_str("|autoplay=");
        out.push_str(if self.autoplay { "1" } else { "0" });
        out.push_str("|loop=");
        out.push_str(if self.loop_play { "1" } else { "0" });
        out.push_str("|muted=");
        out.push_str(if self.muted { "1" } else { "0" });
        out.push_str("|width=");
        out.push_str(&self.width.to_string());
        out.push_str("|height=");
        out.push_str(&self.height.to_string());
        out
    }

    /// Produce deterministic, inert HTML5 player markup.
    ///
    /// Attributes and sources are emitted in a fixed order so identical inputs
    /// always yield byte-identical markup. References were validated to contain
    /// no markup-significant characters, so no escaping is required.
    pub fn to_markup(&self) -> String {
        let tag = match self.element {
            MediaCategory::Audio => "audio",
            MediaCategory::Video => "video",
        };
        let mut out = String::new();
        out.push('<');
        out.push_str(tag);
        if self.controls {
            out.push_str(" controls");
        }
        if self.autoplay {
            out.push_str(" autoplay");
        }
        if self.loop_play {
            out.push_str(" loop");
        }
        if self.muted {
            out.push_str(" muted");
        }
        if let Some(poster) = &self.poster {
            out.push_str(" poster=\"");
            push_reference(&mut out, poster);
            out.push('"');
        }
        if self.width != 0 {
            out.push_str(" width=\"");
            push_u32(&mut out, self.width);
            out.push('"');
        }
        if self.height != 0 {
            out.push_str(" height=\"");
            push_u32(&mut out, self.height);
            out.push('"');
        }
        out.push_str(">\n");
        for source in &self.sources {
            out.push_str("  <source src=\"");
            push_reference(&mut out, source.reference());
            out.push_str("\" type=\"");
            out.push_str(&source.mime_type);
            out.push_str("\"/>\n");
        }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
        out
    }
}

/// Append `value` to `out`, escaping the descriptor's delimiter characters so
/// distinct field values can never produce the same descriptor string.
fn push_field(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            ',' => out.push_str("\\,"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            _ => out.push(ch),
        }
    }
}

fn push_reference(out: &mut String, reference: &LocalMediaRef) {
    match reference {
        LocalMediaRef::Embedded(name) => {
            out.push('@');
            out.push_str(name);
        }
        LocalMediaRef::RelativePath(path) => out.push_str(path),
    }
}

fn push_u32(out: &mut String, value: u32) {
    // A fresh stack buffer is sufficient for the ten decimal digits of u32::MAX.
    let mut buf = [0u8; 10];
    let mut index = buf.len();
    let mut remaining = value;
    loop {
        index -= 1;
        buf[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    if let Ok(text) = std::str::from_utf8(&buf[index..]) {
        out.push_str(text);
    }
}

fn validate_reference(src: &str) -> Result<LocalMediaRef, Html5MediaError> {
    if src.is_empty() {
        return Err(Html5MediaError::EmptySource);
    }
    if src.len() > MAX_SOURCE_BYTES {
        return Err(Html5MediaError::SourceTooLong {
            len: src.len(),
            limit: MAX_SOURCE_BYTES,
        });
    }
    for ch in src.chars() {
        // Control characters (including NUL, newline and tab) would break an
        // HTML attribute value or smuggle content past a host renderer, so they
        // are rejected alongside the markup-significant punctuation.
        if ch.is_control() || matches!(ch, '"' | '\'' | '<' | '>' | '&') {
            return Err(Html5MediaError::UnsafeCharacter(src.to_owned()));
        }
    }

    if let Some(embedded) = src.strip_prefix('@') {
        if embedded.is_empty() {
            return Err(Html5MediaError::EmptySource);
        }
        return Ok(LocalMediaRef::Embedded(embedded.to_owned()));
    }

    if src.starts_with("//") {
        return Err(Html5MediaError::RemoteSource(src.to_owned()));
    }
    if let Some(index) = src.find(':') {
        let scheme = &src[..index];
        if scheme.len() == 1 && scheme.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(Html5MediaError::AbsolutePath(src.to_owned()));
        }
        if !scheme.is_empty() && scheme.bytes().all(|b| b.is_ascii_alphabetic()) {
            if is_remote_scheme(scheme) {
                return Err(Html5MediaError::RemoteSource(src.to_owned()));
            }
            return Err(Html5MediaError::UnsupportedScheme(scheme.to_owned()));
        }
    }
    if src.starts_with('/') || src.starts_with('\\') {
        return Err(Html5MediaError::AbsolutePath(src.to_owned()));
    }

    for segment in src.split(['/', '\\']) {
        if segment == ".." {
            return Err(Html5MediaError::TraversalPath(src.to_owned()));
        }
    }

    Ok(LocalMediaRef::RelativePath(src.to_owned()))
}

fn is_remote_scheme(scheme: &str) -> bool {
    ["http", "https", "ftp", "ftps", "ws", "wss"]
        .iter()
        .any(|remote| scheme.eq_ignore_ascii_case(remote))
}

fn classify_mime(mime: &str) -> Result<MediaCategory, Html5MediaError> {
    if mime.is_empty() {
        return Err(Html5MediaError::EmptyMimeType);
    }
    if mime.len() > MAX_MIME_BYTES {
        return Err(Html5MediaError::UnsupportedMimeType(mime.to_owned()));
    }
    match mime.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp4" | "audio/ogg" | "audio/webm" | "audio/wav" | "audio/x-wav"
        | "audio/aac" => Ok(MediaCategory::Audio),
        "video/mp4" | "video/webm" | "video/ogg" | "video/quicktime" => Ok(MediaCategory::Video),
        _ => Err(Html5MediaError::UnsupportedMimeType(mime.to_owned())),
    }
}

#[cfg(test)]
fn sample_envelope() -> Html5EnvelopeInput {
    Html5EnvelopeInput {
        sources: vec![
            Html5SourceInput {
                src: "media/lecture.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
            },
            Html5SourceInput {
                src: "@embeddedFallback".to_owned(),
                mime_type: "video/webm".to_owned(),
            },
        ],
        poster: Some("media/poster.png".to_owned()),
        controls: true,
        autoplay: false,
        loop_play: true,
        muted: true,
        width: 640,
        height: 360,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_envelope_produces_a_video_descriptor() {
        let descriptor = Html5Descriptor::parse(sample_envelope()).expect("valid envelope");
        assert_eq!(descriptor.element(), MediaCategory::Video);
        assert_eq!(descriptor.sources().len(), 2);
        assert_eq!(
            descriptor.sources()[0].reference(),
            &LocalMediaRef::RelativePath("media/lecture.mp4".to_owned())
        );
        assert_eq!(descriptor.sources()[0].category(), MediaCategory::Video);
        assert_eq!(
            descriptor.sources()[1].reference(),
            &LocalMediaRef::Embedded("embeddedFallback".to_owned())
        );
        assert_eq!(
            descriptor.poster(),
            Some(&LocalMediaRef::RelativePath("media/poster.png".to_owned()))
        );
        assert!(descriptor.controls());
        assert!(descriptor.loop_play());
        assert!(descriptor.muted());
        assert!(!descriptor.autoplay());
        assert_eq!((descriptor.width(), descriptor.height()), (640, 360));
    }

    #[test]
    fn descriptor_markup_is_deterministic() {
        let descriptor = Html5Descriptor::parse(sample_envelope()).expect("valid envelope");
        let first = descriptor.to_markup();
        assert_eq!(first, descriptor.to_markup());
        let again = Html5Descriptor::parse(sample_envelope()).expect("same envelope");
        assert_eq!(first, again.to_markup());
        assert!(first.starts_with("<video controls loop"));
        assert!(first.contains("poster=\"media/poster.png\""));
        assert!(first.contains("width=\"640\" height=\"360\""));
        assert!(first.contains("<source src=\"media/lecture.mp4\" type=\"video/mp4\"/>"));
        assert!(first.contains("<source src=\"@embeddedFallback\" type=\"video/webm\"/>"));
        assert!(first.ends_with("</video>"));
    }

    #[test]
    fn audio_only_envelope_uses_audio_element() {
        let mut envelope = sample_envelope();
        envelope.sources = vec![Html5SourceInput {
            src: "audio/theme.mp3".to_owned(),
            mime_type: "audio/mpeg".to_owned(),
        }];
        envelope.poster = None;
        envelope.width = 0;
        envelope.height = 0;
        let descriptor = Html5Descriptor::parse(envelope).expect("audio envelope");
        assert_eq!(descriptor.element(), MediaCategory::Audio);
        let markup = descriptor.to_markup();
        assert!(markup.starts_with("<audio"));
        assert!(!markup.contains("poster="));
        assert!(!markup.contains("width="));
        assert!(markup.ends_with("</audio>"));
    }

    #[test]
    fn remote_and_scheme_sources_are_rejected() {
        for src in [
            "https://example.com/clip.mp4",
            "http://example.com/clip.mp4",
            "//example.com/clip.mp4",
            "ftp://example.com/clip.mp4",
        ] {
            let mut envelope = sample_envelope();
            envelope.sources[0].src = src.to_owned();
            assert_eq!(
                Html5Descriptor::parse(envelope),
                Err(Html5MediaError::RemoteSource(src.to_owned())),
                "remote source {src} must be rejected"
            );
        }

        let mut envelope = sample_envelope();
        envelope.sources[0].src = "data:video/mp4;base64,AAAA".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::UnsupportedScheme("data".to_owned()))
        );

        let mut envelope = sample_envelope();
        envelope.sources[0].src = "javascript:alert(1)".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::UnsupportedScheme("javascript".to_owned()))
        );
    }

    #[test]
    fn absolute_and_traversal_paths_are_rejected() {
        for src in ["/etc/passwd", "C:\\media\\clip.mp4", "\\media\\clip.mp4"] {
            let mut envelope = sample_envelope();
            envelope.sources[0].src = src.to_owned();
            assert_eq!(
                Html5Descriptor::parse(envelope),
                Err(Html5MediaError::AbsolutePath(src.to_owned())),
                "absolute path {src} must be rejected"
            );
        }

        let mut envelope = sample_envelope();
        envelope.sources[0].src = "media/../secret.mp4".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::TraversalPath(
                "media/../secret.mp4".to_owned()
            ))
        );
    }

    #[test]
    fn unsupported_and_unsafe_sources_are_rejected() {
        let mut envelope = sample_envelope();
        envelope.sources[0].mime_type = "application/x-shockwave-flash".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::UnsupportedMimeType(
                "application/x-shockwave-flash".to_owned()
            ))
        );

        let mut envelope = sample_envelope();
        envelope.sources[0].src = "media/<script>.mp4".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::UnsafeCharacter(
                "media/<script>.mp4".to_owned()
            ))
        );
    }

    #[test]
    fn bounded_input_is_enforced() {
        let mut envelope = sample_envelope();
        envelope.sources.clear();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::NoSources)
        );

        let mut envelope = sample_envelope();
        envelope.sources = vec![
            Html5SourceInput {
                src: "a.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
            };
            MAX_SOURCES + 1
        ];
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::TooManySources {
                count: MAX_SOURCES + 1,
                limit: MAX_SOURCES
            })
        );

        let mut envelope = sample_envelope();
        envelope.sources[1].src = "media/lecture.mp4".to_owned();
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::DuplicateSource(
                "media/lecture.mp4".to_owned()
            ))
        );

        let mut envelope = sample_envelope();
        envelope.sources[0].src = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::SourceTooLong {
                len: MAX_SOURCE_BYTES + 1,
                limit: MAX_SOURCE_BYTES
            })
        );

        let mut envelope = sample_envelope();
        envelope.width = MAX_DIMENSION + 1;
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::DimensionTooLarge {
                value: MAX_DIMENSION + 1,
                limit: MAX_DIMENSION
            })
        );
    }

    #[test]
    fn control_characters_in_references_are_rejected() {
        for bad in [
            "media/a\nb.mp4",
            "media/a\tb.mp4",
            "media/a\rb.mp4",
            "media/a\0b.mp4",
        ] {
            let mut envelope = sample_envelope();
            envelope.sources[0].src = bad.to_owned();
            assert_eq!(
                Html5Descriptor::parse(envelope),
                Err(Html5MediaError::UnsafeCharacter(bad.to_owned())),
                "control character in {bad:?} must be rejected"
            );
        }
        // The poster reference goes through the same validator.
        let mut envelope = sample_envelope();
        envelope.poster = Some("poster\n.png".to_owned());
        assert_eq!(
            Html5Descriptor::parse(envelope),
            Err(Html5MediaError::UnsafeCharacter("poster\n.png".to_owned()))
        );
    }

    #[test]
    fn descriptor_is_byte_stable_and_reflects_the_envelope() {
        let descriptor = Html5Descriptor::parse(sample_envelope()).expect("valid envelope");
        let summary = descriptor.descriptor();
        assert_eq!(summary, descriptor.descriptor());
        assert_eq!(
            summary,
            Html5Descriptor::parse(sample_envelope())
                .expect("same envelope")
                .descriptor()
        );
        assert!(summary.starts_with("html5-plan/v1|element=video|sources="));
        assert!(summary.contains("relative:media/lecture.mp4(video/mp4)"));
        assert!(summary.contains("embedded:embeddedFallback(video/webm)"));
        assert!(summary.contains("|poster=relative:media/poster.png|"));
        assert!(summary.ends_with("|controls=1|autoplay=0|loop=1|muted=1|width=640|height=360"));

        // Changing one option changes the descriptor and the element tag is
        // recomputed from the source set.
        let mut envelope = sample_envelope();
        envelope.sources = vec![Html5SourceInput {
            src: "audio/theme.mp3".to_owned(),
            mime_type: "audio/mpeg".to_owned(),
        }];
        envelope.poster = None;
        let changed = Html5Descriptor::parse(envelope)
            .expect("audio envelope")
            .descriptor();
        assert!(changed.starts_with("html5-plan/v1|element=audio|"));
        assert!(changed.contains("|poster=-|"));
        assert_ne!(changed, summary);
    }
}
