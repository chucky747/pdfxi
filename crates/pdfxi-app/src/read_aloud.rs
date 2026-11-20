//! Read-aloud plan model: ordered speech chunks and a play/pause/step machine.
//!
//! This module turns caller-supplied page text into an ordered plan of
//! [`SpeechChunk`]s with word and sentence boundaries, then drives that plan
//! with a deterministic [`PlaybackState`] machine. It performs **no OS speech
//! calls**: a host implements [`SpeechSink`] to speak the [`SpeechRequest`]s the
//! plan exposes. Keeping chunking and playback window-free makes both fully
//! unit-testable and leaves the audio backend as an explicit boundary.
//!
//! Spans are [`crate::selection::OwnedSpan`] values in reading order, so the
//! same text the canvas uses for selection is the text that is spoken.

use crate::selection::OwnedSpan;

// ---------------------------------------------------------------------------
// Voice settings
// ---------------------------------------------------------------------------

/// Rate, pitch, and voice selection for a read-aloud plan.
///
/// Numeric fields are clamped by [`Self::new`] so a plan is always valid;
/// [`Self::try_new`] is the strict variant used by callers that want to reject
/// out-of-range or non-finite input instead of silently clamping it.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceSettings {
    /// Speaking rate in words per minute.
    pub rate_wpm: u16,
    /// Pitch multiplier, where `1.0` is the voice's natural pitch.
    pub pitch: f32,
    /// Opaque host voice identifier (for example a SAPI voice name).
    pub voice: String,
}

impl VoiceSettings {
    /// Slowest supported rate.
    pub const MIN_RATE_WPM: u16 = 40;
    /// Fastest supported rate.
    pub const MAX_RATE_WPM: u16 = 400;
    /// Default speaking rate.
    pub const DEFAULT_RATE_WPM: u16 = 180;
    /// Lowest supported pitch multiplier.
    pub const MIN_PITCH: f32 = 0.5;
    /// Highest supported pitch multiplier.
    pub const MAX_PITCH: f32 = 2.0;
    /// Default pitch multiplier.
    pub const DEFAULT_PITCH: f32 = 1.0;
    /// Fallback voice identifier when the caller supplies an empty name.
    pub const DEFAULT_VOICE: &'static str = "default";

    /// Create settings, clamping numeric fields to their supported bounds and
    /// substituting [`Self::DEFAULT_VOICE`] for an empty voice name.
    pub fn new(rate_wpm: u16, pitch: f32, voice: impl Into<String>) -> Self {
        let voice = voice.into();
        Self {
            rate_wpm: rate_wpm.clamp(Self::MIN_RATE_WPM, Self::MAX_RATE_WPM),
            pitch: if pitch.is_finite() {
                pitch.clamp(Self::MIN_PITCH, Self::MAX_PITCH)
            } else {
                Self::DEFAULT_PITCH
            },
            voice: if voice.is_empty() {
                Self::DEFAULT_VOICE.to_string()
            } else {
                voice
            },
        }
    }

    /// Strictly validate settings, returning `None` for a non-finite pitch, a
    /// rate or pitch outside the supported range, or an empty voice name.
    pub fn try_new(rate_wpm: u16, pitch: f32, voice: impl Into<String>) -> Option<Self> {
        let voice = voice.into();
        if !(Self::MIN_RATE_WPM..=Self::MAX_RATE_WPM).contains(&rate_wpm)
            || !pitch.is_finite()
            || !(Self::MIN_PITCH..=Self::MAX_PITCH).contains(&pitch)
            || voice.is_empty()
        {
            return None;
        }
        Some(Self {
            rate_wpm,
            pitch,
            voice,
        })
    }
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            rate_wpm: Self::DEFAULT_RATE_WPM,
            pitch: Self::DEFAULT_PITCH,
            voice: Self::DEFAULT_VOICE.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Chunking
// ---------------------------------------------------------------------------

/// How a chunk ends, which tells a speech engine where to pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkBoundary {
    /// The chunk is a word inside a sentence.
    Word,
    /// The chunk ends a sentence (`.`, `!`, or `?`).
    Sentence,
    /// The chunk is the last one on a page; the next chunk starts a new page.
    PageBreak,
    /// The final chunk in the plan.
    End,
}

/// Granularity of the generated speech plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMode {
    /// One chunk per sentence (coarser, more natural prosody).
    Sentence,
    /// One chunk per word (fine-grained, supports word highlighting).
    Word,
}

/// One ordered unit of speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechChunk {
    /// Text to speak.
    pub text: String,
    /// Zero-based source page number.
    pub page: usize,
    /// Zero-based sentence index within the whole plan.
    pub sentence_index: usize,
    /// Zero-based word index within the whole plan.
    pub word_index: usize,
    /// How this chunk ends.
    pub boundary: ChunkBoundary,
}

/// Split text into sentences at `.`, `!`, and `?` followed by whitespace or
/// end-of-text. Punctuation is kept with the sentence; surrounding whitespace
/// is trimmed; empty sentences are dropped.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (index, &c) in chars.iter().enumerate() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            let next_is_break = chars.get(index + 1).is_none_or(|next| next.is_whitespace());
            if next_is_break {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                current.clear();
            }
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

/// Split a sentence into whitespace-delimited words.
fn split_words(sentence: &str) -> Vec<String> {
    sentence
        .split_whitespace()
        .map(str::to_string)
        .filter(|word| !word.is_empty())
        .collect()
}

/// Join a page's spans into reading-order text, separated by single spaces.
fn page_text(spans: &[OwnedSpan]) -> String {
    spans
        .iter()
        .map(|span| span.text.as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build an ordered speech plan from pages of spans.
///
/// Each `(page, spans)` pair is expected in ascending page order; pages with no
/// non-empty text are skipped. `word_index` and `sentence_index` increase
/// continuously across the whole plan. The last chunk of every non-final page
/// is tagged [`ChunkBoundary::PageBreak`] and the very last chunk is tagged
/// [`ChunkBoundary::End`]. An empty input yields an empty plan.
pub fn build_chunks(pages: &[(usize, &[OwnedSpan])], mode: ChunkMode) -> Vec<SpeechChunk> {
    let non_empty: Vec<(usize, String)> = pages
        .iter()
        .map(|(page, spans)| (*page, page_text(spans)))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    let page_count = non_empty.len();
    let mut chunks = Vec::new();
    let mut sentence_index = 0usize;
    let mut word_index = 0usize;

    for (page_number, (page, text)) in non_empty.iter().enumerate() {
        let is_last_page = page_number + 1 == page_count;
        let sentences = split_sentences(text);
        for (sentence_number, sentence) in sentences.iter().enumerate() {
            let is_last_sentence = sentence_number + 1 == sentences.len();
            match mode {
                ChunkMode::Sentence => {
                    let words = split_words(sentence);
                    let start_word = word_index;
                    word_index += words.len();
                    let boundary = if is_last_sentence {
                        if is_last_page {
                            ChunkBoundary::End
                        } else {
                            ChunkBoundary::PageBreak
                        }
                    } else {
                        ChunkBoundary::Sentence
                    };
                    chunks.push(SpeechChunk {
                        text: sentence.clone(),
                        page: *page,
                        sentence_index,
                        word_index: start_word,
                        boundary,
                    });
                    sentence_index += 1;
                }
                ChunkMode::Word => {
                    let words = split_words(sentence);
                    for (word_number, word) in words.iter().enumerate() {
                        let is_last_word = word_number + 1 == words.len();
                        let boundary = if is_last_word && is_last_sentence {
                            if is_last_page {
                                ChunkBoundary::End
                            } else {
                                ChunkBoundary::PageBreak
                            }
                        } else if is_last_word {
                            ChunkBoundary::Sentence
                        } else {
                            ChunkBoundary::Word
                        };
                        chunks.push(SpeechChunk {
                            text: word.clone(),
                            page: *page,
                            sentence_index,
                            word_index,
                            boundary,
                        });
                        word_index += 1;
                    }
                    sentence_index += 1;
                }
            }
        }
    }
    chunks
}

// ---------------------------------------------------------------------------
// Playback state machine
// ---------------------------------------------------------------------------

/// State of a read-aloud playback session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Stopped with the cursor at the beginning.
    Stopped,
    /// Currently advancing through chunks.
    Playing,
    /// Paused on the current chunk.
    Paused,
    /// All chunks have been spoken.
    Finished,
}

/// A deterministic read-aloud plan and its playback cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadAloudPlan {
    chunks: Vec<SpeechChunk>,
    cursor: usize,
    state: PlaybackState,
    voice: VoiceSettings,
}

impl ReadAloudPlan {
    /// Create a plan from already-built chunks and voice settings.
    ///
    /// An empty chunk list produces a stopped, empty plan whose playback calls
    /// are all no-ops.
    pub fn new(chunks: Vec<SpeechChunk>, voice: VoiceSettings) -> Self {
        Self {
            chunks,
            cursor: 0,
            state: PlaybackState::Stopped,
            voice,
        }
    }

    /// Convenience constructor that chunks pages before building the plan.
    pub fn from_pages(
        pages: &[(usize, &[OwnedSpan])],
        mode: ChunkMode,
        voice: VoiceSettings,
    ) -> Self {
        Self::new(build_chunks(pages, mode), voice)
    }

    /// All chunks in speaking order.
    pub fn chunks(&self) -> &[SpeechChunk] {
        &self.chunks
    }

    /// Number of chunks.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether the plan has no chunks.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Current voice settings.
    pub fn voice(&self) -> &VoiceSettings {
        &self.voice
    }

    /// Replace the voice settings (affects subsequent [`Self::speech_request`]s).
    pub fn set_voice(&mut self, voice: VoiceSettings) {
        self.voice = voice;
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Index of the current chunk.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The current chunk, or `None` for an empty plan.
    pub fn current_chunk(&self) -> Option<&SpeechChunk> {
        self.chunks.get(self.cursor)
    }

    /// Start or resume playback.
    ///
    /// From [`PlaybackState::Finished`] this restarts at the first chunk, so a
    /// single "play" control also serves as a replay control.
    pub fn play(&mut self) -> PlaybackState {
        if self.chunks.is_empty() {
            return self.state;
        }
        if self.state == PlaybackState::Finished || self.cursor >= self.chunks.len() {
            self.cursor = 0;
        }
        self.state = PlaybackState::Playing;
        self.state
    }

    /// Pause playback. Only a playing plan transitions to paused.
    pub fn pause(&mut self) -> PlaybackState {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        }
        self.state
    }

    /// Stop playback and rewind the cursor to the first chunk.
    pub fn stop(&mut self) -> PlaybackState {
        self.cursor = 0;
        self.state = PlaybackState::Stopped;
        self.state
    }

    /// Advance one chunk. Called by the host when the current chunk finishes.
    ///
    /// Reaching the end transitions to [`PlaybackState::Finished`]. Calling
    /// `advance` while stopped does not start playback but still moves the
    /// cursor, so hosts can pre-position a plan deterministically.
    pub fn advance(&mut self) -> PlaybackState {
        if self.chunks.is_empty() || self.state == PlaybackState::Finished {
            return self.state;
        }
        if self.cursor + 1 < self.chunks.len() {
            self.cursor += 1;
        } else {
            self.state = PlaybackState::Finished;
        }
        self.state
    }

    /// Step to the next chunk and pause there, for manual review.
    pub fn step_forward(&mut self) -> Option<&SpeechChunk> {
        if self.chunks.is_empty() || self.state == PlaybackState::Finished {
            return None;
        }
        if self.cursor + 1 < self.chunks.len() {
            self.cursor += 1;
        } else {
            self.state = PlaybackState::Finished;
            return None;
        }
        self.state = PlaybackState::Paused;
        self.chunks.get(self.cursor)
    }

    /// Step to the previous chunk and pause there.
    ///
    /// From [`PlaybackState::Finished`] this returns to the final chunk, which
    /// makes stepping back out of the finished state possible.
    pub fn step_backward(&mut self) -> Option<&SpeechChunk> {
        if self.chunks.is_empty() {
            return None;
        }
        if self.state == PlaybackState::Finished {
            self.cursor = self.chunks.len() - 1;
        } else if self.cursor > 0 {
            self.cursor -= 1;
        } else {
            return None;
        }
        self.state = PlaybackState::Paused;
        self.chunks.get(self.cursor)
    }

    /// The request a host should speak for the current chunk, if any.
    pub fn speech_request(&self) -> Option<SpeechRequest> {
        self.current_chunk().map(|chunk| SpeechRequest {
            chunk: chunk.clone(),
            voice: self.voice.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Host boundary
// ---------------------------------------------------------------------------

/// A chunk plus the voice settings to speak it with.
///
/// This is the only value a host needs to call an operating-system speech
/// engine; this crate never makes such a call itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechRequest {
    /// Chunk to speak.
    pub chunk: SpeechChunk,
    /// Voice settings for this request.
    pub voice: VoiceSettings,
}

/// Host speech-engine contract.
///
/// A shell implements this trait to bridge to the platform TTS API. The plan
/// never calls it automatically: the host polls [`ReadAloudPlan::speech_request`]
/// on each frame (or on playback transitions) and forwards the result.
pub trait SpeechSink {
    /// Speak one request. Implementations decide how to queue or cancel it.
    fn speak(&mut self, request: &SpeechRequest);
    /// Cancel any in-flight speech.
    fn stop(&mut self);
}

/// A no-op sink for headless hosts and tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NullSpeechSink;

impl SpeechSink for NullSpeechSink {
    fn speak(&mut self, _request: &SpeechRequest) {}
    fn stop(&mut self) {}
}

// ---------------------------------------------------------------------------
// Deterministic cadence and reading-order utterance queue
// ---------------------------------------------------------------------------

/// Typed errors produced by the deterministic read-aloud queue.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadAloudError {
    /// A queue was built from no speech chunks.
    #[error("read-aloud utterance queue is empty")]
    EmptyQueue,
    /// An utterance index was outside the queue.
    #[error("utterance index {index} out of range (length {len})")]
    UtteranceOutOfRange {
        /// Requested index.
        index: usize,
        /// Number of utterances in the queue.
        len: usize,
    },
    /// A seek time was outside the plan duration.
    #[error("seek time {ms} ms out of range (duration {duration_ms} ms)")]
    TimeOutOfRange {
        /// Requested time in milliseconds.
        ms: u64,
        /// Total plan duration in milliseconds.
        duration_ms: u64,
    },
    /// A cadence field was zero where a positive value is required.
    #[error("invalid cadence field: {field}")]
    InvalidCadence {
        /// Name of the offending field.
        field: &'static str,
    },
}

/// Fixed-point millisecond timing used to turn chunks into timed utterances.
///
/// All fields are integer milliseconds, so every duration and cumulative time
/// is byte-for-byte reproducible. [`Self::from_voice`] derives a cadence from a
/// voice's speaking rate using integer division only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// Milliseconds spent speaking one word. Must be non-zero.
    pub word_ms: u32,
    /// Extra milliseconds after a sentence boundary.
    pub sentence_pause_ms: u32,
    /// Extra milliseconds after a page break.
    pub page_pause_ms: u32,
}

impl Cadence {
    /// Validate a cadence. Only `word_ms` must be non-zero; pauses may be zero.
    pub fn new(
        word_ms: u32,
        sentence_pause_ms: u32,
        page_pause_ms: u32,
    ) -> Result<Self, ReadAloudError> {
        if word_ms == 0 {
            return Err(ReadAloudError::InvalidCadence { field: "word_ms" });
        }
        Ok(Self {
            word_ms,
            sentence_pause_ms,
            page_pause_ms,
        })
    }

    /// Derive a deterministic cadence from voice settings.
    ///
    /// `word_ms` is `60_000 / rate_wpm`; a sentence pause is half a word and a
    /// page pause is two words. Because [`VoiceSettings`] fields are public, the
    /// rate is clamped with `max(1)` so a hostile struct literal cannot divide
    /// by zero.
    pub fn from_voice(voice: &VoiceSettings) -> Self {
        let word_ms = 60_000u32 / u32::from(voice.rate_wpm.max(1));
        Self {
            word_ms,
            sentence_pause_ms: word_ms / 2,
            page_pause_ms: word_ms * 2,
        }
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Self::from_voice(&VoiceSettings::default())
    }
}

/// One timed unit of the reading-order utterance queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    /// Speech chunk to be spoken.
    pub chunk: SpeechChunk,
    /// Time spent speaking the chunk's words, in milliseconds.
    pub duration_ms: u32,
    /// Trailing pause after the chunk, in milliseconds.
    pub pause_ms: u32,
    /// Cumulative start time in the plan, in milliseconds.
    pub start_ms: u64,
    /// Cumulative end time (including the trailing pause), in milliseconds.
    pub end_ms: u64,
}

impl Utterance {
    fn span_ms(&self) -> u64 {
        u64::from(self.duration_ms) + u64::from(self.pause_ms)
    }
}

/// The result of one deterministic playback tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    /// Number of utterances completed by this tick.
    pub completed: usize,
    /// Cursor position after the tick.
    pub cursor: usize,
    /// Playback state after the tick.
    pub state: PlaybackState,
    /// Milliseconds of the supplied tick that were not consumed.
    pub remaining_ms: u64,
}

/// A deterministic, reading-order queue of timed utterances.
///
/// The queue is built from the ordered [`SpeechChunk`]s produced by
/// [`build_chunks`], so its iteration order is the document's reading order.
/// A host calls [`Self::tick`] with wall-clock deltas; the queue converts them
/// into chunk transitions using integer arithmetic with remainder carry, so a
/// tick split into several smaller ticks produces exactly the same outcome as
/// one large tick. No thread, timer, audio device, or sleep is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtteranceQueue {
    utterances: Vec<Utterance>,
    cursor: usize,
    elapsed_ms: u64,
    state: PlaybackState,
    cadence: Cadence,
}

impl UtteranceQueue {
    /// Build a queue from ordered speech chunks and a cadence.
    ///
    /// An empty chunk list is a typed [`ReadAloudError::EmptyQueue`] rather than
    /// a silent no-op: a read-aloud session with no utterances has no meaningful
    /// duration, cursor, or seek target.
    pub fn new(chunks: Vec<SpeechChunk>, cadence: Cadence) -> Result<Self, ReadAloudError> {
        if chunks.is_empty() {
            return Err(ReadAloudError::EmptyQueue);
        }
        let mut queue = Self {
            utterances: Vec::with_capacity(chunks.len()),
            cursor: 0,
            elapsed_ms: 0,
            state: PlaybackState::Stopped,
            cadence,
        };
        queue.rebuild(chunks);
        Ok(queue)
    }

    /// Build a queue by chunking pages of spans in reading order.
    pub fn from_pages(
        pages: &[(usize, &[OwnedSpan])],
        mode: ChunkMode,
        cadence: Cadence,
    ) -> Result<Self, ReadAloudError> {
        Self::new(build_chunks(pages, mode), cadence)
    }

    /// Recompute all durations and cumulative times from chunks and cadence.
    fn rebuild(&mut self, chunks: Vec<SpeechChunk>) {
        self.utterances.clear();
        let mut start_ms = 0u64;
        for chunk in chunks {
            let words = chunk.text.split_whitespace().count().max(1) as u32;
            let duration_ms = words.saturating_mul(self.cadence.word_ms);
            let pause_ms = match chunk.boundary {
                ChunkBoundary::Word => 0,
                ChunkBoundary::Sentence => self.cadence.sentence_pause_ms,
                ChunkBoundary::PageBreak => self.cadence.page_pause_ms,
                ChunkBoundary::End => 0,
            };
            let end_ms = start_ms.saturating_add(u64::from(duration_ms) + u64::from(pause_ms));
            self.utterances.push(Utterance {
                chunk,
                duration_ms,
                pause_ms,
                start_ms,
                end_ms,
            });
            start_ms = end_ms;
        }
        self.cursor = self.cursor.min(self.utterances.len().saturating_sub(1));
        self.elapsed_ms = self
            .elapsed_ms
            .min(self.current().map(Utterance::span_ms).unwrap_or(0));
    }

    /// All utterances in reading order.
    pub fn utterances(&self) -> &[Utterance] {
        &self.utterances
    }

    /// Number of utterances in the queue.
    pub fn len(&self) -> usize {
        self.utterances.len()
    }

    /// Whether the queue has no utterances (cannot happen for a built queue).
    pub fn is_empty(&self) -> bool {
        self.utterances.is_empty()
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Index of the current utterance.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The current utterance, if any.
    pub fn current(&self) -> Option<&Utterance> {
        self.utterances.get(self.cursor)
    }

    /// Milliseconds elapsed inside the current utterance.
    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }

    /// Total plan duration including trailing pauses, in milliseconds.
    pub fn total_duration_ms(&self) -> u64 {
        self.utterances
            .last()
            .map(|utterance| utterance.end_ms)
            .unwrap_or(0)
    }

    /// Active cadence.
    pub fn cadence(&self) -> Cadence {
        self.cadence
    }

    /// Start or resume playback. From `Finished`, restart at the first chunk.
    pub fn play(&mut self) -> PlaybackState {
        if self.state == PlaybackState::Finished {
            self.cursor = 0;
            self.elapsed_ms = 0;
        }
        self.state = PlaybackState::Playing;
        self.state
    }

    /// Resume a paused queue. Other states are unchanged.
    pub fn resume(&mut self) -> PlaybackState {
        if self.state == PlaybackState::Paused {
            self.state = PlaybackState::Playing;
        }
        self.state
    }

    /// Pause a playing queue. Other states are unchanged.
    pub fn pause(&mut self) -> PlaybackState {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        }
        self.state
    }

    /// Stop and rewind to the first utterance.
    pub fn stop(&mut self) -> PlaybackState {
        self.cursor = 0;
        self.elapsed_ms = 0;
        self.state = PlaybackState::Stopped;
        self.state
    }

    /// Advance playback by `delta_ms` using integer remainder carry.
    ///
    /// Only a `Playing` queue advances; other states return a zero-progress
    /// outcome with the whole delta unspent. A tick that finishes the final
    /// utterance leaves the cursor on it and transitions to `Finished`.
    pub fn tick(&mut self, delta_ms: u64) -> TickOutcome {
        if self.state != PlaybackState::Playing || self.utterances.is_empty() {
            return TickOutcome {
                completed: 0,
                cursor: self.cursor,
                state: self.state,
                remaining_ms: delta_ms,
            };
        }
        let mut remaining = delta_ms;
        let mut completed = 0usize;
        loop {
            let span = self.current().map(Utterance::span_ms).unwrap_or(0).max(1);
            let need = span.saturating_sub(self.elapsed_ms);
            if remaining < need {
                self.elapsed_ms = self.elapsed_ms.saturating_add(remaining);
                remaining = 0;
                break;
            }
            remaining -= need;
            self.elapsed_ms = 0;
            completed += 1;
            if self.cursor + 1 < self.utterances.len() {
                self.cursor += 1;
            } else {
                self.state = PlaybackState::Finished;
                self.elapsed_ms = span;
                break;
            }
        }
        TickOutcome {
            completed,
            cursor: self.cursor,
            state: self.state,
            remaining_ms: remaining,
        }
    }

    /// Move the cursor to an utterance index, pausing there.
    pub fn seek_to_utterance(&mut self, index: usize) -> Result<usize, ReadAloudError> {
        if index >= self.utterances.len() {
            return Err(ReadAloudError::UtteranceOutOfRange {
                index,
                len: self.utterances.len(),
            });
        }
        self.cursor = index;
        self.elapsed_ms = 0;
        self.state = PlaybackState::Paused;
        Ok(index)
    }

    /// Move the cursor to the utterance active at `ms`, pausing there.
    ///
    /// Seeking exactly to the total duration positions at the final utterance
    /// with its elapsed time fully consumed. Any larger time is a typed error.
    pub fn seek_to_time(&mut self, ms: u64) -> Result<usize, ReadAloudError> {
        let total = self.total_duration_ms();
        if ms > total {
            return Err(ReadAloudError::TimeOutOfRange {
                ms,
                duration_ms: total,
            });
        }
        let index = self
            .utterances
            .iter()
            .position(|utterance| ms < utterance.end_ms)
            .unwrap_or(self.utterances.len().saturating_sub(1));
        self.cursor = index;
        self.elapsed_ms = ms.saturating_sub(self.utterances[index].start_ms);
        self.state = PlaybackState::Paused;
        Ok(index)
    }

    /// Replace the cadence and recompute every duration deterministically.
    pub fn set_cadence(&mut self, cadence: Cadence) -> Result<(), ReadAloudError> {
        let chunks = self
            .utterances
            .iter()
            .map(|utterance| utterance.chunk.clone())
            .collect();
        self.cadence = cadence;
        self.rebuild(chunks);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> OwnedSpan {
        OwnedSpan::new(text, 0.0, 0.0, 10.0, 10.0)
    }

    fn voice() -> VoiceSettings {
        VoiceSettings::default()
    }

    #[test]
    fn sentence_chunking_produces_ordered_sentences() {
        let spans = [span("Hello world. This is a test! Is it working?")];
        let pages = [(0, spans.as_slice())];
        let chunks = build_chunks(&pages, ChunkMode::Sentence);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Hello world.", "This is a test!", "Is it working?"]
        );
        assert_eq!(chunks[0].sentence_index, 0);
        assert_eq!(chunks[1].sentence_index, 1);
        assert_eq!(chunks[2].sentence_index, 2);
        assert_eq!(chunks[0].boundary, ChunkBoundary::Sentence);
        assert_eq!(chunks[2].boundary, ChunkBoundary::End);
    }

    #[test]
    fn word_chunking_records_word_and_sentence_indices() {
        let spans = [span("One two. Three four.")];
        let pages = [(0, spans.as_slice())];
        let chunks = build_chunks(&pages, ChunkMode::Word);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["One", "two.", "Three", "four."]);
        let indices: Vec<usize> = chunks.iter().map(|c| c.word_index).collect();
        assert_eq!(indices, vec![0, 1, 2, 3]);
        assert_eq!(chunks[1].boundary, ChunkBoundary::Sentence);
        assert_eq!(chunks[2].sentence_index, 1);
        assert_eq!(chunks[3].boundary, ChunkBoundary::End);
    }

    #[test]
    fn chunking_marks_page_breaks_and_keeps_page_numbers() {
        let p0 = [span("First page.")];
        let p1 = [span("Second page.")];
        let pages = [(0, p0.as_slice()), (1, p1.as_slice())];
        let chunks = build_chunks(&pages, ChunkMode::Sentence);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].page, 0);
        assert_eq!(chunks[0].boundary, ChunkBoundary::PageBreak);
        assert_eq!(chunks[1].page, 1);
        assert_eq!(chunks[1].boundary, ChunkBoundary::End);
    }

    #[test]
    fn chunking_is_deterministic_and_skips_empty_pages() {
        let p0: [OwnedSpan; 0] = [];
        let p1 = [span("  "), span("Real text.")];
        let pages = [(0, p0.as_slice()), (1, p1.as_slice())];
        let a = build_chunks(&pages, ChunkMode::Sentence);
        let b = build_chunks(&pages, ChunkMode::Sentence);
        assert_eq!(a, b);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].text, "Real text.");
        assert_eq!(a[0].page, 1);
    }

    #[test]
    fn play_pause_and_resume_transition_correctly() {
        let spans = [span("A. B.")];
        let mut plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        assert_eq!(plan.state(), PlaybackState::Stopped);
        assert_eq!(plan.play(), PlaybackState::Playing);
        assert_eq!(plan.pause(), PlaybackState::Paused);
        assert_eq!(
            plan.pause(),
            PlaybackState::Paused,
            "pausing twice is a no-op"
        );
        assert_eq!(plan.play(), PlaybackState::Playing);
        assert_eq!(plan.stop(), PlaybackState::Stopped);
        assert_eq!(plan.cursor(), 0);
    }

    #[test]
    fn step_forward_and_backward_pause_and_bound_at_the_edges() {
        let spans = [span("A. B. C.")];
        let mut plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        assert_eq!(plan.play(), PlaybackState::Playing);
        assert_eq!(plan.step_forward().map(|c| c.text.as_str()), Some("B."));
        assert_eq!(plan.state(), PlaybackState::Paused);
        assert_eq!(plan.step_backward().map(|c| c.text.as_str()), Some("A."));
        assert_eq!(
            plan.step_backward(),
            None,
            "cannot step before the first chunk"
        );
        assert_eq!(plan.step_forward().map(|c| c.text.as_str()), Some("B."));
        assert_eq!(plan.step_forward().map(|c| c.text.as_str()), Some("C."));
        assert_eq!(plan.step_forward(), None);
        assert_eq!(plan.state(), PlaybackState::Finished);
    }

    #[test]
    fn step_backward_from_finished_returns_to_last_chunk() {
        let spans = [span("A. B.")];
        let mut plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        plan.play();
        assert_eq!(plan.advance(), PlaybackState::Playing);
        assert_eq!(plan.advance(), PlaybackState::Finished);
        assert_eq!(plan.cursor(), 1);
        assert_eq!(plan.step_backward().map(|c| c.text.as_str()), Some("B."));
        assert_eq!(plan.state(), PlaybackState::Paused);
    }

    #[test]
    fn advance_reaches_finished_and_play_restarts() {
        let spans = [span("A. B.")];
        let mut plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        plan.play();
        assert_eq!(plan.advance(), PlaybackState::Playing);
        assert_eq!(plan.cursor(), 1);
        assert_eq!(plan.advance(), PlaybackState::Finished);
        assert_eq!(
            plan.advance(),
            PlaybackState::Finished,
            "advance at end is a no-op"
        );
        assert_eq!(plan.play(), PlaybackState::Playing);
        assert_eq!(
            plan.cursor(),
            0,
            "play from finished restarts at the first chunk"
        );
    }

    #[test]
    fn empty_plan_is_a_no_op_for_all_playback_calls() {
        let mut plan = ReadAloudPlan::new(Vec::new(), voice());
        assert!(plan.is_empty());
        assert_eq!(plan.current_chunk(), None);
        assert_eq!(plan.speech_request(), None);
        assert_eq!(plan.play(), PlaybackState::Stopped);
        assert_eq!(plan.advance(), PlaybackState::Stopped);
        assert_eq!(plan.step_forward(), None);
        assert_eq!(plan.step_backward(), None);
        assert_eq!(plan.pause(), PlaybackState::Stopped);
    }

    #[test]
    fn voice_settings_new_clamps_bounds_and_empty_voice() {
        let v = VoiceSettings::new(1, f32::NAN, "");
        assert_eq!(v.rate_wpm, VoiceSettings::MIN_RATE_WPM);
        assert_eq!(v.pitch, VoiceSettings::DEFAULT_PITCH);
        assert_eq!(v.voice, VoiceSettings::DEFAULT_VOICE);
        let v = VoiceSettings::new(10_000, 99.0, "Zira");
        assert_eq!(v.rate_wpm, VoiceSettings::MAX_RATE_WPM);
        assert_eq!(v.pitch, VoiceSettings::MAX_PITCH);
        assert_eq!(v.voice, "Zira");
    }

    #[test]
    fn voice_settings_try_new_rejects_invalid_input() {
        assert!(VoiceSettings::try_new(180, 1.0, "Zira").is_some());
        assert!(VoiceSettings::try_new(10, 1.0, "Zira").is_none());
        assert!(VoiceSettings::try_new(180, f32::INFINITY, "Zira").is_none());
        assert!(VoiceSettings::try_new(180, 3.0, "Zira").is_none());
        assert!(VoiceSettings::try_new(180, 1.0, "").is_none());
    }

    #[test]
    fn speech_request_carries_current_chunk_and_voice_and_set_voice_updates_it() {
        let spans = [span("A.")];
        let mut plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        let request = plan.speech_request().expect("current chunk");
        assert_eq!(request.chunk.text, "A.");
        assert_eq!(request.voice.rate_wpm, VoiceSettings::DEFAULT_RATE_WPM);
        let new_voice = VoiceSettings::new(220, 1.1, "David");
        plan.set_voice(new_voice.clone());
        assert_eq!(
            plan.speech_request().expect("current chunk").voice,
            new_voice
        );
    }

    #[test]
    fn null_sink_accepts_requests_without_side_effects() {
        let spans = [span("A.")];
        let plan =
            ReadAloudPlan::from_pages(&[(0, spans.as_slice())], ChunkMode::Sentence, voice());
        let request = plan.speech_request().expect("current chunk");
        let mut sink = NullSpeechSink;
        sink.speak(&request);
        sink.stop();
        assert_eq!(sink, NullSpeechSink);
    }

    fn cadence() -> Cadence {
        Cadence::new(100, 25, 200).expect("valid test cadence")
    }

    #[test]
    fn utterance_queue_is_deterministic_and_carries_reading_order() {
        let p0 = [span("alpha beta.")];
        let p1 = [span("gamma delta.")];
        let pages = [(0, p0.as_slice()), (1, p1.as_slice())];
        let queue = UtteranceQueue::from_pages(&pages, ChunkMode::Word, cadence())
            .expect("utterance queue");
        let texts: Vec<&str> = queue
            .utterances()
            .iter()
            .map(|utterance| utterance.chunk.text.as_str())
            .collect();
        assert_eq!(texts, vec!["alpha", "beta.", "gamma", "delta."]);
        // One word per chunk: 100 ms of speech plus boundary pauses.
        assert_eq!(queue.utterances()[0].duration_ms, 100);
        assert_eq!(queue.utterances()[0].pause_ms, 0);
        assert_eq!(queue.utterances()[1].pause_ms, 200); // last word on page 0
        assert_eq!(queue.utterances()[3].pause_ms, 0); // final chunk
        assert_eq!(queue.utterances()[3].end_ms, 100 + 300 + 100 + 100);
        assert_eq!(queue.total_duration_ms(), 100 + 300 + 100 + 100);
        assert_eq!(
            queue,
            UtteranceQueue::from_pages(&pages, ChunkMode::Word, cadence()).expect("repeat")
        );
    }

    #[test]
    fn tick_remainder_carry_matches_one_large_tick() {
        let p0 = [span("one two. three four.")];
        let pages = [(0, p0.as_slice())];
        let build =
            || UtteranceQueue::from_pages(&pages, ChunkMode::Word, cadence()).expect("queue");
        let mut single = build();
        single.play();
        let one = single.tick(1_000);
        let mut chunked = build();
        chunked.play();
        let a = chunked.tick(120);
        let b = chunked.tick(330);
        let c = chunked.tick(550);
        assert_eq!(a.completed + b.completed + c.completed, one.completed);
        assert_eq!(chunked, single);
        assert_eq!(chunked.cursor(), single.cursor());
        assert_eq!(chunked.state(), single.state());
    }

    #[test]
    fn pause_resume_seek_and_stop_are_deterministic() {
        let p0 = [span("a b. c d.")];
        let pages = [(0, p0.as_slice())];
        let mut queue =
            UtteranceQueue::from_pages(&pages, ChunkMode::Word, cadence()).expect("queue");
        assert_eq!(queue.play(), PlaybackState::Playing);
        assert_eq!(queue.tick(10).completed, 0);
        assert_eq!(queue.pause(), PlaybackState::Paused);
        let paused = queue.clone();
        assert_eq!(queue.tick(10_000).completed, 0);
        assert_eq!(queue, paused);
        assert_eq!(queue.resume(), PlaybackState::Playing);
        assert_eq!(queue.seek_to_utterance(2), Ok(2));
        assert_eq!(queue.state(), PlaybackState::Paused);
        assert_eq!(queue.elapsed_ms(), 0);
        assert_eq!(
            queue.seek_to_time(queue.utterances()[1].start_ms + 5),
            Ok(1)
        );
        assert_eq!(queue.elapsed_ms(), 5);
        assert_eq!(queue.stop(), PlaybackState::Stopped);
        assert_eq!(queue.cursor(), 0);
        assert_eq!(queue.elapsed_ms(), 0);
    }

    #[test]
    fn cadence_change_recomputes_timing_deterministically() {
        let p0 = [span("one two.")];
        let pages = [(0, p0.as_slice())];
        let mut queue =
            UtteranceQueue::from_pages(&pages, ChunkMode::Word, cadence()).expect("queue");
        let original = queue.total_duration_ms();
        let faster = Cadence::new(50, 10, 100).expect("cadence");
        queue.set_cadence(faster).expect("set cadence");
        assert_eq!(queue.cadence(), faster);
        assert!(queue.total_duration_ms() < original);
        assert_eq!(
            queue,
            UtteranceQueue::from_pages(&pages, ChunkMode::Word, faster).expect("rebuilt")
        );
    }

    #[test]
    fn cadence_from_voice_is_deterministic_and_guards_zero_rate() {
        let settings = VoiceSettings::default();
        let derived = Cadence::from_voice(&settings);
        assert_eq!(derived.word_ms, 60_000 / u32::from(settings.rate_wpm));
        assert_eq!(derived.sentence_pause_ms, derived.word_ms / 2);
        assert_eq!(derived.page_pause_ms, derived.word_ms * 2);
        let hostile = VoiceSettings {
            rate_wpm: 0,
            pitch: 1.0,
            voice: "x".to_string(),
        };
        assert_eq!(Cadence::from_voice(&hostile).word_ms, 60_000);
    }

    #[test]
    fn utterance_queue_rejects_empty_and_invalid_ranges() {
        let empty: [OwnedSpan; 0] = [];
        assert_eq!(
            UtteranceQueue::from_pages(&[(0, empty.as_slice())], ChunkMode::Word, cadence()),
            Err(ReadAloudError::EmptyQueue)
        );
        assert_eq!(
            Cadence::new(0, 1, 1),
            Err(ReadAloudError::InvalidCadence { field: "word_ms" })
        );
        let spans = [span("a b.")];
        let mut queue =
            UtteranceQueue::from_pages(&[(0, spans.as_slice())], ChunkMode::Word, cadence())
                .expect("queue");
        let len = queue.len();
        assert_eq!(
            queue.seek_to_utterance(len),
            Err(ReadAloudError::UtteranceOutOfRange { index: len, len })
        );
        let total = queue.total_duration_ms();
        assert_eq!(
            queue.seek_to_time(total + 1),
            Err(ReadAloudError::TimeOutOfRange {
                ms: total + 1,
                duration_ms: total
            })
        );
        assert_eq!(queue.seek_to_time(total), Ok(len - 1));
    }
}
