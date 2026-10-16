//! Installer-style GUI for standalone patchers.
//!
//! - [`theme`]: the author's `theme.toml`, validated the same way at create and run time;
//! - [`bundle`]: the theme folder packed into the patcher executable;
//! - [`color`], [`strings`], [`expand`]: palette derivation, UI strings, path expansion.

pub mod bundle;
pub mod color;
pub mod expand;
pub mod strings;
pub mod theme;

pub use theme::{Theme, ThemeError};
