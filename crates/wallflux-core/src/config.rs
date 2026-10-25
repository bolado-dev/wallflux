//! User configuration, read from `config.toml`.
//!
//! Every field has a default, so a missing or partial file is valid and an
//! empty install still runs. Unknown fields are rejected rather than ignored:
//! a typo in a hand-edited config should be reported, not silently dropped.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::CONFIG_VERSION;
use crate::paths;

/// How a video is fitted to an output whose aspect ratio differs from its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ScaleMode {
    /// Fill the output, cropping the overflowing axis. The usual choice.
    #[default]
    Cover,
    /// Fit inside the output, letterboxing the remainder.
    Contain,
    /// Fill the output, distorting the aspect ratio.
    Stretch,
}

impl ScaleMode {
    /// mpv's `--panscan` only reaches "cover" when combined with
    /// `--video-unscaled=no`; these are the two knobs we set per mode.
    pub fn mpv_panscan(self) -> &'static str {
        match self {
            ScaleMode::Cover => "1.0",
            ScaleMode::Contain | ScaleMode::Stretch => "0.0",
        }
    }

    pub fn mpv_keepaspect(self) -> &'static str {
        match self {
            ScaleMode::Stretch => "no",
            _ => "yes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackConfig {
    /// Upper bound on rendered frames per second. The daemon never draws
    /// faster than this even if the video has a higher rate, which is the
    /// single most effective knob for CPU use on a software renderer.
    pub fps_cap: u32,
    /// Audio is muted by default: a wallpaper that makes noise is a bug.
    pub mute: bool,
    /// mpv hardware decoding mode. `auto-safe` falls back to software when the
    /// driver is not trustworthy, which is what we want on a VM.
    pub hwdec: String,
    pub scale_mode: ScaleMode,
    /// Render at a fraction of the output resolution and let the GPU upscale.
    /// `1.0` is native. On a 2880x1800 panel without acceleration, `0.5`
    /// cuts the decode cost to a quarter at a barely visible loss.
    pub render_scale: f32,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            fps_cap: 30,
            mute: true,
            hwdec: "auto-safe".to_string(),
            scale_mode: ScaleMode::default(),
            render_scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    /// Stop rendering while a fullscreen window is focused. The wallpaper is
    /// invisible then, so every frame drawn is pure waste.
    pub pause_on_fullscreen: bool,
    /// Stop rendering when opaque windows cover an output completely. Same
    /// reasoning as above but for a tiled layout, which is the common case
    /// under a tiling WM.
    pub pause_on_occlusion: bool,
    /// Stop rendering while on battery.
    pub pause_on_battery: bool,
    /// Applied instead of a full pause when `pause_on_battery` is off.
    pub battery_fps_cap: u32,
    /// Seconds of inactivity before the idle fps cap kicks in. `0` disables.
    pub idle_timeout_secs: u32,
    pub idle_fps_cap: u32,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            pause_on_fullscreen: true,
            pause_on_occlusion: true,
            pause_on_battery: false,
            battery_fps_cap: 15,
            idle_timeout_secs: 0,
            idle_fps_cap: 10,
        }
    }
}

/// Per-output overrides. Everything is optional; an absent field inherits the
/// global value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Wallpaper id from the library, or `None` to leave the output alone.
    pub wallpaper: Option<String>,
    pub fps_cap: Option<u32>,
    pub scale_mode: Option<ScaleMode>,
    pub render_scale: Option<f32>,
    /// Skip this output entirely, e.g. a secondary screen you want black.
    pub disabled: bool,
}

/// The online gallery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GalleryConfig {
    /// Where the curated catalogue is fetched from. A fork can point this at
    /// its own list without touching any code.
    pub catalog_url: String,
    /// A free Pexels API key, which unlocks searching their library. Empty
    /// means the gallery shows the curated catalogue only — which is why
    /// there is no key shipped here: one baked into a public repository
    /// would be everyone's key, and would be revoked the week after.
    pub pexels_api_key: String,
}

impl Default for GalleryConfig {
    fn default() -> Self {
        Self {
            catalog_url: crate::gallery::DEFAULT_CATALOG_URL.to_string(),
            pexels_api_key: String::new(),
        }
    }
}

/// Importar del taller de Steam.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkshopConfig {
    /// Steam account that owns Wallpaper Engine. Only the user name: the
    /// password is steamcmd's business and never passes through wallflux.
    pub steam_user: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub playback: PlaybackConfig,
    pub power: PowerConfig,
    pub gallery: GalleryConfig,
    pub workshop: WorkshopConfig,
    /// Keyed by connector name as the compositor reports it (`eDP-1`,
    /// `DP-2`, …). `BTreeMap` so a rewritten file keeps a stable order
    /// instead of shuffling the user's sections around.
    pub outputs: BTreeMap<String, OutputConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            playback: PlaybackConfig::default(),
            power: PowerConfig::default(),
            gallery: GalleryConfig::default(),
            workshop: WorkshopConfig::default(),
            outputs: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Loads the user config, falling back to defaults when the file does not
    /// exist. A malformed file is an error: silently replacing it with
    /// defaults would throw away the user's settings.
    pub fn load() -> Result<Self> {
        let path = paths::config_file()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "no config file, using defaults");
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("cannot read {}", path.display()));
            }
        };
        let config: Self =
            toml::from_str(&text).with_context(|| format!("invalid TOML in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_file()?;
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let text = toml::to_string_pretty(self).context("cannot serialise config")?;
        // Write-then-rename so a crash mid-write cannot truncate the config.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.version > CONFIG_VERSION {
            bail!(
                "config version {} is newer than this build understands ({CONFIG_VERSION}); \
                 upgrade wallflux or fix the version field",
                self.version
            );
        }
        if self.playback.fps_cap == 0 {
            bail!("playback.fps_cap must be at least 1");
        }
        if !(0.1..=1.0).contains(&self.playback.render_scale) {
            bail!(
                "playback.render_scale must be between 0.1 and 1.0 (got {})",
                self.playback.render_scale
            );
        }
        for (name, output) in &self.outputs {
            if let Some(scale) = output.render_scale
                && !(0.1..=1.0).contains(&scale)
            {
                bail!("outputs.{name}.render_scale must be between 0.1 and 1.0 (got {scale})");
            }
            if output.fps_cap == Some(0) {
                bail!("outputs.{name}.fps_cap must be at least 1");
            }
        }
        Ok(())
    }

    /// Effective playback settings for one output, after applying overrides.
    pub fn playback_for(&self, output: &str) -> PlaybackConfig {
        let mut playback = self.playback.clone();
        if let Some(over) = self.outputs.get(output) {
            if let Some(fps) = over.fps_cap {
                playback.fps_cap = fps;
            }
            if let Some(mode) = over.scale_mode {
                playback.scale_mode = mode;
            }
            if let Some(scale) = over.render_scale {
                playback.render_scale = scale;
            }
        }
        playback
    }

    pub fn wallpaper_for(&self, output: &str) -> Option<&str> {
        let over = self.outputs.get(output)?;
        if over.disabled {
            return None;
        }
        over.wallpaper.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn empty_file_is_valid() {
        let parsed: Config = toml::from_str("").unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = toml::from_str::<Config>("[playback]\nfps_capp = 30\n").unwrap_err();
        assert!(err.to_string().contains("fps_capp"), "{err}");
    }

    #[test]
    fn per_output_override_wins() {
        let config: Config = toml::from_str(
            r#"
            [playback]
            fps_cap = 60

            [outputs."DP-1"]
            wallpaper = "neon"
            fps_cap = 24
            "#,
        )
        .unwrap();
        assert_eq!(config.playback_for("DP-1").fps_cap, 24);
        assert_eq!(config.playback_for("eDP-1").fps_cap, 60);
        assert_eq!(config.wallpaper_for("DP-1"), Some("neon"));
    }

    #[test]
    fn disabled_output_reports_no_wallpaper() {
        let config: Config = toml::from_str(
            r#"
            [outputs."DP-1"]
            wallpaper = "neon"
            disabled = true
            "#,
        )
        .unwrap();
        assert_eq!(config.wallpaper_for("DP-1"), None);
    }

    #[test]
    fn the_gallery_ships_no_api_key() {
        // A key committed to a public repository is everyone's key, and gets
        // revoked. The curated catalogue is what makes the gallery work
        // without one.
        let config = Config::default();
        assert!(config.gallery.pexels_api_key.is_empty());
        assert!(config.gallery.catalog_url.starts_with("https://"));
    }

    #[test]
    fn the_config_holds_a_steam_user_but_never_a_password() {
        // The account name is needed to invoke steamcmd. Credentials are
        // steamcmd's to handle, so there is nowhere here to put one.
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(text.contains("steam_user"));
        assert!(!text.to_lowercase().contains("password"));
    }

    #[test]
    fn future_version_is_refused() {
        let config = Config {
            version: CONFIG_VERSION + 1,
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn out_of_range_render_scale_is_refused() {
        let config = Config {
            playback: PlaybackConfig {
                render_scale: 2.0,
                ..Default::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
