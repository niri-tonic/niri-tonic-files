pub mod associations;
pub mod config;
pub mod connector;
pub mod filen;
pub mod local;
pub mod locations;
pub mod navigation;
pub mod remote;
pub mod storage;
pub mod theme;
pub mod ui;
/// CI embeds the planned release identity before qualifying the binary.
/// Ordinary developer builds retain Cargo's package version.
pub const APP_VERSION: &str = match option_env!("NIRI_TONIC_RELEASE_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};
