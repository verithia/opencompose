//! The external markets this build speaks to.
//!
//! A module either implements [`SampleProvider`](crate::provider::SampleProvider)
//! or records why it cannot: a page-only market keeps its spec and reason, so
//! the registry can report it instead of silently dropping it.

pub mod bedroomproducersblog;
pub mod commons;
pub mod cymatics;
pub mod freesound;
pub mod internet_archive;
pub mod jamendo;
pub mod looperman;
pub mod mixkit;
pub mod openverse;
pub mod pixabay;
pub mod samplefocus;
pub mod sampleswap;
pub mod soundbible;
pub mod splice;
