//! Settings this build keeps outside a project: the offline renderer's worker
//! count and whether track rendering is parallel at all.
//!
//! Preferences are converted to request-owned worker snapshots at render
//! boundaries. Saving preferences never changes the process environment.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Largest worker count the preferences page accepts.
pub const MAX_RENDER_THREADS: u32 = 64;

/// How outbound HTTP picks a proxy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// Whatever the operating system and the environment declare.
    #[default]
    System,
    /// One explicit proxy for every request.
    Manual,
    /// Never through a proxy.
    None,
}

/// Network settings: the proxy every outbound client should use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkSettings {
    pub proxy_mode: ProxyMode,
    /// The proxy URL configured by the user in manual mode.
    pub proxy_url: String,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            proxy_mode: ProxyMode::System,
            proxy_url: String::new(),
        }
    }
}

impl NetworkSettings {
    /// The URL a manual proxy should use, or `None` when the mode does not need
    /// one or the saved value is not an HTTP proxy URL.
    pub fn manual_url(&self) -> Option<&str> {
        if self.proxy_mode != ProxyMode::Manual {
            return None;
        }
        let url = self.proxy_url.trim();
        (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("socks5://"))
            .then_some(url)
    }
}

/// The container session audio is archived in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionAudioFormat {
    #[default]
    Wav,
    Mp3,
}

/// How full audition renders are kept beside a conversation. The bounded
/// excerpt delivered to the model retains its original WAV bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionAudioSettings {
    pub format: SessionAudioFormat,
    /// MP3 bitrate in kbps; ignored for WAV.
    pub mp3_kbps: u32,
}

impl Default for SessionAudioSettings {
    fn default() -> Self {
        Self {
            format: SessionAudioFormat::Wav,
            mp3_kbps: 192,
        }
    }
}

impl SessionAudioSettings {
    /// Clamp stored audio settings to their supported ranges.
    pub fn sanitized(mut self) -> Self {
        self.mp3_kbps = self.mp3_kbps.clamp(64, 320);
        self
    }
}

/// The persisted render-performance settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    /// Requested workers; `None` follows the machine's parallelism.
    pub render_threads: Option<u32>,
    /// Off renders every track on one worker.
    pub parallel_tracks: bool,
    pub network: NetworkSettings,
    /// Container and bitrate for audition audio kept with a conversation.
    pub session_audio: SessionAudioSettings,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            render_threads: None,
            parallel_tracks: true,
            network: NetworkSettings::default(),
            session_audio: SessionAudioSettings::default(),
        }
    }
}

impl GuiSettings {
    /// Clamp the stored count into the range the page and the engine accept.
    pub fn sanitized(mut self) -> Self {
        self.render_threads = self
            .render_threads
            .map(|count| count.clamp(1, MAX_RENDER_THREADS));
        self.network.proxy_url = self.network.proxy_url.trim().to_string();
        self.session_audio = self.session_audio.clone().sanitized();
        // A manual mode without a usable URL would silently send every request
        // straight out, which is worse than following the system.
        if self.network.proxy_mode == ProxyMode::Manual && self.network.manual_url().is_none() {
            self.network.proxy_mode = ProxyMode::System;
        }
        self
    }

    /// A fixed execution snapshot; GUI auto mode follows the machine rather
    /// than inheriting a CLI-only environment pin.
    pub fn worker_options(&self) -> oc_engine::render_parallel::RenderWorkerOptions {
        oc_engine::render_parallel::RenderWorkerOptions::new(
            std::num::NonZeroUsize::new(self.effective_workers().max(1)).expect("positive workers"),
        )
    }

    /// Workers an offline render will use with these settings.
    pub fn effective_workers(&self) -> usize {
        if !self.parallel_tracks {
            return 1;
        }
        match self.render_threads {
            Some(count) => count.clamp(1, MAX_RENDER_THREADS) as usize,
            None => machine_parallelism(),
        }
    }
}

/// Parallelism the machine reports, with the same floor the engine uses.
pub fn machine_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
}

/// The file the settings live in, inside the private configuration directory.
pub fn path() -> PathBuf {
    crate::private_fs::config_dir().join("gui-settings.yaml")
}

/// Read YAML preferences, falling back to legacy JSON if YAML has not been saved.
pub fn load() -> GuiSettings {
    load_from(&path())
}

fn load_from(path: &std::path::Path) -> GuiSettings {
    let settings = match std::fs::read(path) {
        Ok(bytes) => serde_yaml::from_slice::<GuiSettings>(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let legacy = path.with_extension("json");
            match std::fs::read(&legacy) {
                Ok(bytes) => serde_json::from_slice::<GuiSettings>(&bytes)
                    .map_err(|error| format!("invalid '{}': {error}", legacy.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return GuiSettings::default();
                }
                Err(error) => Err(format!("cannot read '{}': {error}", legacy.display())),
            }
        }
        Err(error) => Err(error.to_string()),
    };
    match settings {
        Ok(settings) => settings.sanitized(),
        Err(error) => {
            log::warn!("gui settings: cannot load '{}': {error}", path.display());
            GuiSettings::default()
        }
    }
}

/// Write the YAML settings atomically; the caller reports the failure to the page.
pub fn save(settings: &GuiSettings) -> Result<(), String> {
    save_to(&path(), settings)
}

fn save_to(path: &std::path::Path, settings: &GuiSettings) -> Result<(), String> {
    let body = serde_yaml::to_string(settings)
        .map_err(|error| format!("cannot encode the settings: {error}"))?;
    crate::private_fs::write_atomic(path, body.as_bytes())
        .map_err(|error| format!("cannot write '{}': {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_preferences_produce_independent_snapshots() {
        let auto = GuiSettings::default();
        let pinned = GuiSettings {
            render_threads: Some(7),
            ..auto.clone()
        };
        let serial = GuiSettings {
            parallel_tracks: false,
            ..pinned.clone()
        };
        assert_eq!(auto.worker_options().workers(), machine_parallelism());
        assert_eq!(pinned.worker_options().workers(), 7);
        assert_eq!(serial.worker_options().workers(), 1);
        assert_eq!(pinned.worker_options().workers(), 7);
        assert_eq!(
            GuiSettings {
                render_threads: Some(0),
                ..auto.clone()
            }
            .worker_options()
            .workers(),
            1
        );
        assert_eq!(
            GuiSettings {
                render_threads: Some(u32::MAX),
                ..auto
            }
            .worker_options()
            .workers(),
            MAX_RENDER_THREADS as usize
        );
    }

    #[test]
    fn the_network_defaults_to_the_system_proxy() {
        let settings = GuiSettings::default();
        assert_eq!(settings.network.proxy_mode, ProxyMode::System);
        assert!(settings.network.manual_url().is_none());
    }

    #[test]
    fn a_manual_mode_without_a_url_falls_back_to_the_system() {
        let settings = GuiSettings {
            render_threads: None,
            parallel_tracks: true,
            network: NetworkSettings {
                proxy_mode: ProxyMode::Manual,
                proxy_url: "  ".into(),
            },
            session_audio: SessionAudioSettings::default(),
        }
        .sanitized();
        assert_eq!(settings.network.proxy_mode, ProxyMode::System);
    }

    #[test]
    fn a_manual_proxy_survives_sanitizing() {
        let settings = GuiSettings {
            render_threads: None,
            parallel_tracks: true,
            network: NetworkSettings {
                proxy_mode: ProxyMode::Manual,
                proxy_url: " http://proxy.example.invalid:8080 ".into(),
            },
            session_audio: SessionAudioSettings::default(),
        }
        .sanitized();
        assert_eq!(
            settings.network.manual_url(),
            Some("http://proxy.example.invalid:8080")
        );
    }

    #[test]
    fn defaults_are_auto_and_parallel() {
        let settings = GuiSettings::default();
        assert_eq!(settings.render_threads, None);
        assert!(settings.parallel_tracks);
        assert_eq!(settings.effective_workers(), machine_parallelism());
    }

    #[test]
    fn a_count_is_clamped_to_the_accepted_range() {
        let settings = GuiSettings {
            render_threads: Some(0),
            parallel_tracks: true,
            network: NetworkSettings::default(),
            session_audio: SessionAudioSettings::default(),
        }
        .sanitized();
        assert_eq!(settings.render_threads, Some(1));
        let settings = GuiSettings {
            render_threads: Some(9_999),
            parallel_tracks: true,
            network: NetworkSettings::default(),
            session_audio: SessionAudioSettings::default(),
        }
        .sanitized();
        assert_eq!(settings.render_threads, Some(MAX_RENDER_THREADS));
    }

    #[test]
    fn serial_rendering_wins_over_the_requested_count() {
        let settings = GuiSettings {
            render_threads: Some(8),
            parallel_tracks: false,
            network: NetworkSettings::default(),
            session_audio: SessionAudioSettings::default(),
        };
        assert_eq!(settings.effective_workers(), 1);
    }

    #[test]
    fn yaml_takes_precedence_and_legacy_json_still_loads() {
        let dir = std::env::temp_dir().join(format!(
            "opencompose-gui-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gui-settings.yaml");
        let legacy = GuiSettings {
            render_threads: Some(3),
            network: NetworkSettings {
                proxy_mode: ProxyMode::Manual,
                proxy_url: "http://proxy.example.invalid:8080".into(),
            },
            ..GuiSettings::default()
        };
        crate::private_fs::write_atomic(
            &file.with_extension("json"),
            &serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        assert_eq!(load_from(&file), legacy);
        let updated = GuiSettings {
            render_threads: Some(4),
            ..legacy
        };
        save_to(&file, &updated).unwrap();
        assert_eq!(load_from(&file), updated);
        let _ = std::fs::remove_dir_all(dir);
    }
}
