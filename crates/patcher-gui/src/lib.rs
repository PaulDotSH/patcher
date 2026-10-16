//! Installer-style GUI for standalone patchers.
//!
//! - [`theme`]: the author's `theme.toml`, validated the same way at create and run time;
//! - [`bundle`]: the theme folder packed into the patcher executable;
//! - [`compile`]: the built-in Slint library plus the theme, compiled with the interpreter;
//! - [`color`], [`strings`], [`expand`]: palette derivation, UI strings, path expansion.

pub mod bundle;
pub mod color;
pub mod compile;
pub mod expand;
pub mod flow;
pub mod strings;
pub mod theme;

pub use theme::{Theme, ThemeError};
