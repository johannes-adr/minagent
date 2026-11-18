//! Tools the agent can call. Each implements [`crate::harness::ToolPolicy`] and is
//! registered in [`crate::harness`].

/// Subdirectory of the harness's directory that `execute_javascript` (`fs`) and
/// `view_image` (relative paths) work in. `.env`, `skills/` and the sources stay outside.
#[cfg(any(feature = "javascript", feature = "image"))]
pub const WORKDIR: &str = "work";

/// The absolute path of [`WORKDIR`]; it may not exist yet.
#[cfg(any(feature = "javascript", feature = "image"))]
pub fn workdir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_default().join(WORKDIR)
}

pub mod agent;
pub mod execute;
#[cfg(feature = "javascript")]
pub mod execute_javascript;
mod process;
pub mod skills;
#[cfg(feature = "image")]
pub mod view_image;
