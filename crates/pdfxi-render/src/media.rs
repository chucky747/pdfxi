//! Audio and video annotation model (feature MEDIA-001).
//!
//! This module models the *data* of PDF multimedia annotations — `/Sound`,
//! `/Movie` and `/Screen` — without decoding any codec or asking the operating
//! system to play anything. It parses a small, owned intermediate dictionary
//! that mirrors the annotation's media source, poster/rendition references,
//! playback-control flags, activation trigger and volume. Every variable-length
//! input is bounded and validated with typed errors so a hostile PDF cannot make
//! this crate allocate without limit.
//!
//! # Backend limits (honest delegation)
//!
//! [`MediaAnnotation`] is a plan, not a player. It never decodes audio or video,
//! never opens an OS media sink, and never contacts a network. A screen
//! annotation may advertise several *renditions* (alternative encodings of the
//! same clip); this module validates and selects them deterministically but does
//! not inspect their payloads. [`MediaAnnotation::resolve_plan`] produces a
//! [`PlaybackPlan`] whose [`PlaybackPlan::requirements`] state exactly which
//! capabilities a native/host media player must supply.
//!
//! All operations here are pure and deterministic, add no dependency, and need
//! no cancellation token because a single parse or plan resolution is short.

/// Maximum byte length of a media source, rendition or poster name.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum number of renditions in a single annotation.
pub const MAX_RENDITIONS: usize = 32;
/// Maximum byte length of a MIME type string.
pub const MAX_MIME_BYTES: usize = 64;

/// Structured errors produced while parsing a media annotation dictionary.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MediaError {
    /// The annotation kind was not one of `Sound`, `Movie` or `Screen`.
    #[error("unknown media annotation kind '{0}'")]
    UnknownKind(String),
    /// A media source name was empty.
    #[error("media source must not be empty")]
    EmptySource,
    /// A media source name exceeded the byte-length cap.
    #[error("media source length {len} exceeds the {limit}-byte limit")]
    SourceTooLong {
        /// Actual byte length of the name.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// The annotation carried no renditions.
    #[error("media annotation must declare at least one rendition")]
    NoRenditions,
    /// The annotation declared more renditions than the cap allows.
    #[error("rendition count {count} exceeds the {limit}-rendition limit")]
    TooManyRenditions {
        /// Number of renditions supplied.
        count: usize,
        /// Enforced rendition limit.
        limit: usize,
    },
    /// A rendition name was empty.
    #[error("rendition name must not be empty")]
    EmptyRenditionName,
    /// A rendition name exceeded the byte-length cap.
    #[error("rendition name length {len} exceeds the {limit}-byte limit")]
    RenditionNameTooLong {
        /// Actual byte length of the name.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// Two renditions shared the same name.
    #[error("duplicate rendition name '{0}'")]
    DuplicateRenditionName(String),
    /// The selected rendition was not among the declared renditions.
    #[error("selected rendition '{0}' is not declared")]
    RenditionNotFound(String),
    /// A MIME type was empty.
    #[error("media MIME type must not be empty")]
    EmptyMimeType,
    /// A MIME type exceeded the byte-length cap.
    #[error("media MIME type length {len} exceeds the {limit}-byte limit")]
    MimeTypeTooLong {
        /// Actual byte length of the MIME type.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// A poster reference was empty.
    #[error("poster reference must not be empty")]
    EmptyPoster,
    /// A poster reference exceeded the byte-length cap.
    #[error("poster reference length {len} exceeds the {limit}-byte limit")]
    PosterTooLong {
        /// Actual byte length of the reference.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// The activation-event code was not recognised.
    #[error("unknown activation trigger code '{0}'")]
    UnknownActivationTrigger(String),
    /// A volume was not finite or outside `0.0..=1.0`.
    #[error("volume {value} must be finite and in 0.0..=1.0")]
    InvalidVolume {
        /// The rejected volume.
        value: f32,
    },
}

/// The PDF annotation subtype that carries the media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    /// A `/Sound` annotation (audio only).
    Sound,
    /// A `/Movie` annotation (a movie with an activation policy).
    Movie,
    /// A `/Screen` annotation (renditions and rich actions).
    Screen,
}

impl MediaKind {
    /// Parse a PDF annotation subtype name.
    ///
    /// Returns [`MediaError::UnknownKind`] for any other name.
    pub fn parse(name: &str) -> Result<Self, MediaError> {
        match name {
            "Sound" => Ok(Self::Sound),
            "Movie" => Ok(Self::Movie),
            "Screen" => Ok(Self::Screen),
            other => Err(MediaError::UnknownKind(other.to_owned())),
        }
    }

    /// Return the canonical PDF subtype name for this kind.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Sound => "Sound",
            Self::Movie => "Movie",
            Self::Screen => "Screen",
        }
    }
}

/// The event that activates a multimedia annotation.
///
/// The variants correspond to the PDF activation-event codes `PV`, `PI`, `PO`
/// and `PC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationTrigger {
    /// The containing page becomes visible (`PV`).
    PageVisible,
    /// The containing page becomes invisible (`PI`).
    PageInvisible,
    /// The document is opened (`PO`).
    DocumentOpen,
    /// The document is closed (`PC`).
    DocumentClose,
}

impl ActivationTrigger {
    /// Parse a PDF activation-event code.
    ///
    /// Returns [`MediaError::UnknownActivationTrigger`] for any other code.
    pub fn parse(code: &str) -> Result<Self, MediaError> {
        match code {
            "PV" => Ok(Self::PageVisible),
            "PI" => Ok(Self::PageInvisible),
            "PO" => Ok(Self::DocumentOpen),
            "PC" => Ok(Self::DocumentClose),
            other => Err(MediaError::UnknownActivationTrigger(other.to_owned())),
        }
    }

    /// Return the canonical PDF code for this trigger.
    pub const fn code(self) -> &'static str {
        match self {
            Self::PageVisible => "PV",
            Self::PageInvisible => "PI",
            Self::DocumentOpen => "PO",
            Self::DocumentClose => "PC",
        }
    }
}

/// A control that the host player should expose for a media clip.
///
/// Controls form a bounded, typed whitelist; nothing here executes code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlaybackControl {
    /// Start playback.
    Play,
    /// Pause playback.
    Pause,
    /// Stop playback and reset the position.
    Stop,
    /// Toggle muting.
    Mute,
    /// Expose a volume slider.
    Volume,
    /// Allow seeking within the clip.
    Seek,
    /// Allow full-screen playback.
    FullScreen,
}

/// A typed reference to the media payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaSource {
    /// An embedded stream, referenced by its bounded name.
    Embedded(String),
    /// A local media file, referenced by its bounded name.
    File(String),
    /// An indirect object reference (object number and generation).
    Object {
        /// The indirect object number.
        number: u32,
        /// The indirect object generation.
        generation: u16,
    },
}

impl MediaSource {
    /// Validate the bounded inputs of this reference.
    fn validate(&self) -> Result<(), MediaError> {
        if let Self::Embedded(name) | Self::File(name) = self {
            if name.is_empty() {
                return Err(MediaError::EmptySource);
            }
            if name.len() > MAX_NAME_BYTES {
                return Err(MediaError::SourceTooLong {
                    len: name.len(),
                    limit: MAX_NAME_BYTES,
                });
            }
        }
        Ok(())
    }
}

/// How the active rendition is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenditionSelection {
    /// Use the first declared rendition.
    Default,
    /// Use the rendition with this name.
    Named(String),
    /// Use the rendition at this zero-based index (clamped when out of range).
    Indexed(usize),
}

/// Owned intermediate form of one rendition in a media annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenditionInput {
    /// The rendition's bounded name.
    pub name: String,
    /// Where this rendition's payload lives.
    pub source: MediaSource,
    /// The MIME type advertised for this rendition.
    pub mime_type: String,
}

/// Owned intermediate form of a media annotation dictionary.
///
/// Field names mirror the relevant PDF keys; this crate does not depend on a PDF
/// object parser, so a caller extracts these values and hands them here.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaDictionary {
    /// The annotation subtype name (`Sound`, `Movie` or `Screen`).
    pub kind: String,
    /// The annotation's primary media source.
    pub source: MediaSource,
    /// The primary MIME type of the media.
    pub mime_type: String,
    /// An optional poster image stream name.
    pub poster: Option<String>,
    /// The declared renditions, in document order.
    pub renditions: Vec<RenditionInput>,
    /// How the active rendition is selected.
    pub selection: RenditionSelection,
    /// The activation-event code (`PV`, `PI`, `PO`, `PC`).
    pub activation: String,
    /// Whether playback should begin on activation.
    pub auto_play: bool,
    /// Whether playback should restart after finishing.
    pub loop_play: bool,
    /// Playback volume in `0.0..=1.0`.
    pub volume: f32,
    /// The playback controls the host should expose.
    pub controls: Vec<PlaybackControl>,
}

/// A validated rendition of a media annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendition {
    name: String,
    source: MediaSource,
    mime_type: String,
    is_default: bool,
}

impl Rendition {
    /// Return this rendition's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return this rendition's media source.
    pub fn source(&self) -> &MediaSource {
        &self.source
    }

    /// Return this rendition's MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Return `true` when this is the selected default rendition.
    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

/// A validated multimedia annotation model.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaAnnotation {
    kind: MediaKind,
    source: MediaSource,
    mime_type: String,
    poster: Option<String>,
    renditions: Vec<Rendition>,
    selection: RenditionSelection,
    activation: ActivationTrigger,
    auto_play: bool,
    loop_play: bool,
    volume: f32,
    controls: Vec<PlaybackControl>,
}

impl MediaAnnotation {
    /// Validate and build a media annotation from an intermediate dictionary.
    pub fn parse(dict: MediaDictionary) -> Result<Self, MediaError> {
        let kind = MediaKind::parse(&dict.kind)?;
        dict.source.validate()?;
        validate_mime(&dict.mime_type)?;
        if let Some(poster) = &dict.poster {
            validate_poster(poster)?;
        }

        if dict.renditions.is_empty() {
            return Err(MediaError::NoRenditions);
        }
        if dict.renditions.len() > MAX_RENDITIONS {
            return Err(MediaError::TooManyRenditions {
                count: dict.renditions.len(),
                limit: MAX_RENDITIONS,
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(dict.renditions.len());
        for input in &dict.renditions {
            validate_rendition(input)?;
            if seen.contains(&input.name.as_str()) {
                return Err(MediaError::DuplicateRenditionName(input.name.clone()));
            }
            seen.push(input.name.as_str());
        }

        let active_index = resolve_selection(&dict.selection, &dict.renditions)?;
        let renditions = dict
            .renditions
            .into_iter()
            .enumerate()
            .map(|(index, input)| Rendition {
                name: input.name,
                source: input.source,
                mime_type: input.mime_type,
                is_default: index == active_index,
            })
            .collect();

        if !dict.volume.is_finite() || !(0.0..=1.0).contains(&dict.volume) {
            return Err(MediaError::InvalidVolume { value: dict.volume });
        }

        // Controls are a bounded whitelist, deduplicated and sorted so the
        // resulting plan is deterministic regardless of declaration order.
        let mut controls = dict.controls;
        controls.sort_unstable();
        controls.dedup();

        Ok(Self {
            kind,
            source: dict.source,
            mime_type: dict.mime_type,
            poster: dict.poster,
            renditions,
            selection: dict.selection,
            activation: ActivationTrigger::parse(&dict.activation)?,
            auto_play: dict.auto_play,
            loop_play: dict.loop_play,
            volume: dict.volume,
            controls,
        })
    }

    /// Return the annotation subtype.
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Return the primary media source.
    pub fn source(&self) -> &MediaSource {
        &self.source
    }

    /// Return the primary MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Return the optional poster stream name.
    pub fn poster(&self) -> Option<&str> {
        self.poster.as_deref()
    }

    /// Return the declared renditions in document order.
    pub fn renditions(&self) -> &[Rendition] {
        &self.renditions
    }

    /// Return the configured rendition selection.
    pub fn selection(&self) -> &RenditionSelection {
        &self.selection
    }

    /// Return the activation trigger.
    pub fn activation(&self) -> ActivationTrigger {
        self.activation
    }

    /// Return the auto-play flag.
    pub fn auto_play(&self) -> bool {
        self.auto_play
    }

    /// Return the loop flag.
    pub fn loop_play(&self) -> bool {
        self.loop_play
    }

    /// Return the volume in `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Return the sorted, deduplicated playback-control whitelist.
    pub fn controls(&self) -> &[PlaybackControl] {
        &self.controls
    }

    /// Resolve the zero-based index of the active rendition.
    ///
    /// The result is deterministic and always in `0..renditions.len()`; a named
    /// selection was validated during parsing and an indexed one is clamped.
    pub fn active_rendition_index(&self) -> usize {
        resolve_selection(&self.selection, &self.renditions_as_inputs()).unwrap_or(0)
    }

    /// Return the active rendition, or `None` only if there are no renditions.
    pub fn active_rendition(&self) -> Option<&Rendition> {
        self.renditions.get(self.active_rendition_index())
    }

    /// Build a deterministic playback plan for a host player.
    pub fn resolve_plan(&self) -> PlaybackPlan {
        let index = self.active_rendition_index();
        let active = self.renditions.get(index);
        let source = active
            .map(Rendition::source)
            .cloned()
            .unwrap_or_else(|| self.source.clone());
        let mime_type = active
            .map(Rendition::mime_type)
            .unwrap_or(&self.mime_type)
            .to_owned();

        let mut requirements = vec![HostRequirement::MediaPlayer];
        if self.poster.is_some() {
            requirements.push(HostRequirement::PosterFrame);
        }
        if self.controls.contains(&PlaybackControl::FullScreen) {
            requirements.push(HostRequirement::FullScreen);
        }

        PlaybackPlan {
            kind: self.kind,
            source,
            mime_type,
            poster: self.poster.clone(),
            activation: self.activation,
            auto_play: self.auto_play,
            loop_play: self.loop_play,
            volume: self.volume,
            muted: self.controls.contains(&PlaybackControl::Mute),
            controls: self.controls.clone(),
            active_rendition_index: index,
            rendition_name: active.map(Rendition::name).unwrap_or("").to_owned(),
            requirements,
        }
    }

    /// Rebuild lightweight rendition inputs for selection resolution.
    fn renditions_as_inputs(&self) -> Vec<RenditionInput> {
        self.renditions
            .iter()
            .map(|r| RenditionInput {
                name: r.name.clone(),
                source: r.source.clone(),
                mime_type: r.mime_type.clone(),
            })
            .collect()
    }
}

/// A capability a host media player must provide to realise a [`PlaybackPlan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRequirement {
    /// Decode and play the selected media source.
    MediaPlayer,
    /// Display the annotation's poster frame before/during playback.
    PosterFrame,
    /// Support full-screen playback.
    FullScreen,
}

/// The deterministic plan a host player must realise.
///
/// This is data only: the crate never decodes a codec or starts playback.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackPlan {
    kind: MediaKind,
    source: MediaSource,
    mime_type: String,
    poster: Option<String>,
    activation: ActivationTrigger,
    auto_play: bool,
    loop_play: bool,
    volume: f32,
    muted: bool,
    controls: Vec<PlaybackControl>,
    active_rendition_index: usize,
    rendition_name: String,
    requirements: Vec<HostRequirement>,
}

impl PlaybackPlan {
    /// Return the annotation subtype.
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Return the source the host player should open.
    pub fn source(&self) -> &MediaSource {
        &self.source
    }

    /// Return the MIME type of the selected media.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Return the optional poster stream name.
    pub fn poster(&self) -> Option<&str> {
        self.poster.as_deref()
    }

    /// Return the activation trigger.
    pub fn activation(&self) -> ActivationTrigger {
        self.activation
    }

    /// Return whether playback starts on activation.
    pub fn auto_play(&self) -> bool {
        self.auto_play
    }

    /// Return whether playback loops.
    pub fn loop_play(&self) -> bool {
        self.loop_play
    }

    /// Return the playback volume.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Return whether playback starts muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Return the sorted playback-control whitelist.
    pub fn controls(&self) -> &[PlaybackControl] {
        &self.controls
    }

    /// Return the zero-based index of the selected rendition.
    pub fn active_rendition_index(&self) -> usize {
        self.active_rendition_index
    }

    /// Return the name of the selected rendition.
    pub fn rendition_name(&self) -> &str {
        &self.rendition_name
    }

    /// Return the host capabilities required to realise this plan.
    pub fn requirements(&self) -> &[HostRequirement] {
        &self.requirements
    }

    /// Return `true` — a native media player is always required.
    pub fn needs_host_player(&self) -> bool {
        true
    }
}

fn validate_mime(mime: &str) -> Result<(), MediaError> {
    if mime.is_empty() {
        return Err(MediaError::EmptyMimeType);
    }
    if mime.len() > MAX_MIME_BYTES {
        return Err(MediaError::MimeTypeTooLong {
            len: mime.len(),
            limit: MAX_MIME_BYTES,
        });
    }
    Ok(())
}

fn validate_poster(poster: &str) -> Result<(), MediaError> {
    if poster.is_empty() {
        return Err(MediaError::EmptyPoster);
    }
    if poster.len() > MAX_NAME_BYTES {
        return Err(MediaError::PosterTooLong {
            len: poster.len(),
            limit: MAX_NAME_BYTES,
        });
    }
    Ok(())
}

fn validate_rendition(input: &RenditionInput) -> Result<(), MediaError> {
    if input.name.is_empty() {
        return Err(MediaError::EmptyRenditionName);
    }
    if input.name.len() > MAX_NAME_BYTES {
        return Err(MediaError::RenditionNameTooLong {
            len: input.name.len(),
            limit: MAX_NAME_BYTES,
        });
    }
    input.source.validate()?;
    validate_mime(&input.mime_type)
}

fn resolve_selection(
    selection: &RenditionSelection,
    renditions: &[RenditionInput],
) -> Result<usize, MediaError> {
    match selection {
        RenditionSelection::Default => Ok(0),
        RenditionSelection::Named(name) => renditions
            .iter()
            .position(|r| r.name == *name)
            .ok_or_else(|| MediaError::RenditionNotFound(name.clone())),
        RenditionSelection::Indexed(index) => Ok((*index).min(renditions.len().saturating_sub(1))),
    }
}

#[cfg(test)]
fn sample_dictionary() -> MediaDictionary {
    MediaDictionary {
        kind: "Screen".to_owned(),
        source: MediaSource::Embedded("clipStream".to_owned()),
        mime_type: "video/mp4".to_owned(),
        poster: Some("posterImage".to_owned()),
        renditions: vec![
            RenditionInput {
                name: "high".to_owned(),
                source: MediaSource::Embedded("clipHigh".to_owned()),
                mime_type: "video/mp4".to_owned(),
            },
            RenditionInput {
                name: "low".to_owned(),
                source: MediaSource::File("clip_low.mp4".to_owned()),
                mime_type: "video/mp4".to_owned(),
            },
        ],
        selection: RenditionSelection::Named("low".to_owned()),
        activation: "PV".to_owned(),
        auto_play: true,
        loop_play: false,
        volume: 0.8,
        controls: vec![
            PlaybackControl::Seek,
            PlaybackControl::Play,
            PlaybackControl::Mute,
            PlaybackControl::Play,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_and_movie_annotations_parse() {
        let mut sound = sample_dictionary();
        sound.kind = "Sound".to_owned();
        sound.mime_type = "audio/mpeg".to_owned();
        let sound = MediaAnnotation::parse(sound).expect("valid sound annotation");
        assert_eq!(sound.kind(), MediaKind::Sound);
        assert_eq!(sound.mime_type(), "audio/mpeg");
        assert_eq!(
            sound.source(),
            &MediaSource::Embedded("clipStream".to_owned())
        );

        let mut movie = sample_dictionary();
        movie.kind = "Movie".to_owned();
        movie.source = MediaSource::Object {
            number: 12,
            generation: 0,
        };
        let movie = MediaAnnotation::parse(movie).expect("valid movie annotation");
        assert_eq!(movie.kind(), MediaKind::Movie);
        assert_eq!(
            movie.source(),
            &MediaSource::Object {
                number: 12,
                generation: 0
            }
        );
        assert_eq!(MediaKind::parse("Screen"), Ok(MediaKind::Screen));
        assert_eq!(MediaKind::Screen.code(), "Screen");
    }

    #[test]
    fn rendition_selection_resolves_named_default_and_indexed() {
        let named = MediaAnnotation::parse(sample_dictionary()).expect("named selection");
        assert_eq!(
            named.selection(),
            &RenditionSelection::Named("low".to_owned())
        );
        assert_eq!(named.active_rendition_index(), 1);
        assert_eq!(named.active_rendition().map(Rendition::name), Some("low"));
        assert!(named.renditions()[1].is_default());
        assert!(!named.renditions()[0].is_default());

        let mut default = sample_dictionary();
        default.selection = RenditionSelection::Default;
        let default = MediaAnnotation::parse(default).expect("default selection");
        assert_eq!(default.active_rendition_index(), 0);
        assert_eq!(
            default.active_rendition().map(Rendition::name),
            Some("high")
        );

        // Indexed selections are clamped to the available range.
        let mut indexed_dict = sample_dictionary();
        indexed_dict.selection = RenditionSelection::Indexed(99);
        let indexed = MediaAnnotation::parse(indexed_dict).expect("indexed selection");
        assert_eq!(indexed.active_rendition_index(), 1);
    }

    #[test]
    fn invalid_activation_trigger_is_a_typed_error() {
        let mut dict = sample_dictionary();
        dict.activation = "ZZ".to_owned();
        assert_eq!(
            MediaAnnotation::parse(dict),
            Err(MediaError::UnknownActivationTrigger("ZZ".to_owned()))
        );
        for (code, trigger) in [
            ("PV", ActivationTrigger::PageVisible),
            ("PI", ActivationTrigger::PageInvisible),
            ("PO", ActivationTrigger::DocumentOpen),
            ("PC", ActivationTrigger::DocumentClose),
        ] {
            assert_eq!(ActivationTrigger::parse(code), Ok(trigger));
            assert_eq!(trigger.code(), code);
        }
    }

    #[test]
    fn bounded_input_is_enforced() {
        let mut dict = sample_dictionary();
        dict.renditions = vec![];
        assert_eq!(MediaAnnotation::parse(dict), Err(MediaError::NoRenditions));

        let mut dict = sample_dictionary();
        dict.renditions = vec![
            RenditionInput {
                name: "x".to_owned(),
                source: MediaSource::File("a".to_owned()),
                mime_type: "video/mp4".to_owned(),
            };
            MAX_RENDITIONS + 1
        ];
        assert_eq!(
            MediaAnnotation::parse(dict),
            Err(MediaError::TooManyRenditions {
                count: MAX_RENDITIONS + 1,
                limit: MAX_RENDITIONS
            })
        );

        let mut dict = sample_dictionary();
        dict.renditions[1].name = "high".to_owned();
        dict.selection = RenditionSelection::Default;
        assert_eq!(
            MediaAnnotation::parse(dict),
            Err(MediaError::DuplicateRenditionName("high".to_owned()))
        );

        let mut dict = sample_dictionary();
        dict.source = MediaSource::Embedded(String::new());
        assert_eq!(MediaAnnotation::parse(dict), Err(MediaError::EmptySource));

        let mut dict = sample_dictionary();
        dict.mime_type = "x".repeat(MAX_MIME_BYTES + 1);
        assert_eq!(
            MediaAnnotation::parse(dict),
            Err(MediaError::MimeTypeTooLong {
                len: MAX_MIME_BYTES + 1,
                limit: MAX_MIME_BYTES
            })
        );

        let mut dict = sample_dictionary();
        dict.poster = Some(String::new());
        assert_eq!(MediaAnnotation::parse(dict), Err(MediaError::EmptyPoster));

        for volume in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            let mut dict = sample_dictionary();
            dict.volume = volume;
            assert!(
                matches!(
                    MediaAnnotation::parse(dict),
                    Err(MediaError::InvalidVolume { .. })
                ),
                "volume {volume} must be rejected"
            );
        }
    }

    #[test]
    fn playback_plan_reflects_selection_poster_and_controls() {
        let annotation = MediaAnnotation::parse(sample_dictionary()).expect("valid");
        let plan = annotation.resolve_plan();
        assert_eq!(plan.kind(), MediaKind::Screen);
        assert_eq!(plan.active_rendition_index(), 1);
        assert_eq!(plan.rendition_name(), "low");
        assert_eq!(plan.source(), &MediaSource::File("clip_low.mp4".to_owned()));
        assert_eq!(plan.mime_type(), "video/mp4");
        assert_eq!(plan.poster(), Some("posterImage"));
        assert!(plan.auto_play());
        assert!(!plan.loop_play());
        assert!((plan.volume() - 0.8).abs() < f32::EPSILON);
        assert!(plan.muted());
        assert!(plan.needs_host_player());
        // Controls were duplicated and unsorted on input; the plan is sorted
        // and deduplicated in the canonical enum order.
        assert_eq!(
            plan.controls(),
            &[
                PlaybackControl::Play,
                PlaybackControl::Mute,
                PlaybackControl::Seek,
            ]
        );
        // The sample declares no FullScreen control, so that requirement is
        // omitted; a FullScreen control adds it (covered in resolve_plan).
        assert_eq!(
            plan.requirements(),
            &[HostRequirement::MediaPlayer, HostRequirement::PosterFrame]
        );
    }

    #[test]
    fn plan_is_deterministic_and_omits_unneeded_requirements() {
        let annotation = MediaAnnotation::parse(sample_dictionary()).expect("valid");
        assert_eq!(annotation.resolve_plan(), annotation.resolve_plan());

        let mut quiet = sample_dictionary();
        quiet.poster = None;
        quiet.controls.clear();
        let quiet = MediaAnnotation::parse(quiet).expect("valid").resolve_plan();
        assert_eq!(quiet.controls(), &[] as &[PlaybackControl]);
        assert_eq!(quiet.requirements(), &[HostRequirement::MediaPlayer]);
        assert!(!quiet.muted());

        let mut full = sample_dictionary();
        full.controls = vec![PlaybackControl::FullScreen];
        let full = MediaAnnotation::parse(full).expect("valid").resolve_plan();
        assert_eq!(
            full.requirements(),
            &[
                HostRequirement::MediaPlayer,
                HostRequirement::PosterFrame,
                HostRequirement::FullScreen,
            ]
        );
    }
}
