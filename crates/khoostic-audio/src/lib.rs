//! Khoostic's audio engine: decoding, format conditioning, gapless output and analysis.
//!
//! ```text
//!  AudioSource ──read──► Conditioner ──► SPSC ring ──► cpal callback ──► speakers
//!  (symphonia,           (stereo +       (lock-free)        │
//!   librespot, ...)       resample)                         └──► analysis ring ──► FFT thread
//!                                                                       └► ArcSwap<AnalysisFrame>
//! ```

pub mod analysis;
pub mod condition;
pub mod decode;
pub mod engine;
pub mod output;
pub mod source;

pub use analysis::{AnalysisFrame, BANDS, WAVEFORM_LEN};
pub use decode::{SymphoniaSource, is_supported_path};
pub use engine::{
    Command, EngineConfig, EngineEvent, EngineHandle, EngineStatus, PlaybackState, Token,
};
pub use source::{AudioSource, ReadStatus, SourceError, SourceSpec};
