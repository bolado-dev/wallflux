//! Rendering: the EGL plumbing and the mpv player, both window-system
//! agnostic. The backends in [`crate::backend`] supply the surfaces.

pub mod egl;
pub mod mpv;

pub use egl::{Egl, Gl, Platform, Surface};
pub use mpv::{Player, PlayerOptions};
