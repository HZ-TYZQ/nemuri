//! 1.5.0 之前的 `config.toml`：只在第一次生成 `config.kdl` 时读一次，从不写入。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::track::{LegacyPlayMode, PlaybackMode, RepeatMode, SortOrder};

use super::AppConfig;

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawConfig {
    version: u32,
    library_dir: Option<PathBuf>,
    volume: u8,
    muted: bool,
    repeat: Option<RepeatMode>,
    shuffle: Option<bool>,
    play_mode: Option<LegacyPlayMode>,
    sort: SortOrder,
    visualizer_enabled: bool,
    mouse_enabled: bool,
    lyrics_enabled: bool,
}

impl Default for RawConfig {
    fn default() -> Self {
        let config = AppConfig::default();
        Self {
            version: CONFIG_VERSION,
            library_dir: None,
            volume: config.volume,
            muted: config.muted,
            repeat: None,
            shuffle: None,
            play_mode: None,
            sort: config.sort,
            visualizer_enabled: config.visualizer_enabled,
            mouse_enabled: config.mouse_enabled,
            lyrics_enabled: config.lyrics_enabled,
        }
    }
}

impl RawConfig {
    fn into_config(self) -> AppConfig {
        // 新的 repeat / shuffle 优先；都没有时才看 v1.1.0 及更早的 play_mode。
        // 旧版本没有“播完停止”，没写循环方式时一直是顺序播放，迁移后保持原样。
        let in_order = PlaybackMode {
            repeat: RepeatMode::None,
            shuffle: false,
        };
        let mode = if self.repeat.is_some() || self.shuffle.is_some() {
            PlaybackMode {
                repeat: self.repeat.unwrap_or(in_order.repeat),
                shuffle: self.shuffle.unwrap_or(in_order.shuffle),
            }
        } else {
            self.play_mode
                .map(LegacyPlayMode::into_playback_mode)
                .unwrap_or(in_order)
        };
        AppConfig {
            library_dir: self.library_dir,
            volume: self.volume.min(100),
            muted: self.muted,
            repeat: mode.repeat,
            shuffle: mode.shuffle,
            sort: self.sort,
            visualizer_enabled: self.visualizer_enabled,
            mouse_enabled: self.mouse_enabled,
            lyrics_enabled: self.lyrics_enabled,
            ..AppConfig::default()
        }
    }
}

/// 没有旧配置时返回 None；读不了或解析不了时返回说明原因的错误。
pub(super) fn load(path: &Path) -> Option<Result<AppConfig, String>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => return Some(Err(format!("无法读取旧配置 {}: {error}", path.display()))),
    };
    Some(parse(&contents).map_err(|error| format!("旧配置 {} {error}", path.display())))
}

fn parse(contents: &str) -> Result<AppConfig, String> {
    let raw =
        toml::from_str::<RawConfig>(contents).map_err(|error| format!("无法解析: {error}"))?;
    if raw.version != CONFIG_VERSION {
        return Err(format!(
            "的版本 {} 不受支持（应为 {CONFIG_VERSION}）",
            raw.version
        ));
    }
    Ok(raw.into_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_clamped() {
        assert_eq!(parse("version = 1\nvolume = 255\n").unwrap().volume, 100);
    }

    #[test]
    fn legacy_play_mode_shuffle_migrates() {
        let config =
            parse("version = 1\nvolume = 75\nmuted = false\nplay_mode = \"shuffle\"\n").unwrap();
        assert_eq!(config.repeat, RepeatMode::None);
        assert!(config.shuffle);
        assert!(config.visualizer_enabled);
    }

    #[test]
    fn new_fields_win_over_legacy_play_mode() {
        let config = parse("version = 1\nrepeat = \"all\"\nplay_mode = \"shuffle\"\n").unwrap();
        assert_eq!(config.repeat, RepeatMode::All);
        assert!(!config.shuffle);
    }

    #[test]
    fn old_config_defaults_new_switches_to_enabled() {
        let config =
            parse("version = 1\nvolume = 75\nmuted = false\nplay_mode = \"sequential\"\n").unwrap();
        assert!(config.visualizer_enabled);
        assert!(config.lyrics_enabled);
        assert!(config.mouse_enabled);
        assert_eq!(config.repeat, RepeatMode::None);
        assert!(!config.shuffle);
    }

    #[test]
    fn an_old_config_without_a_repeat_mode_keeps_playing_in_order() {
        // 新配置默认“播完停止”，从旧配置迁来的用户不应被悄悄改掉习惯。
        for contents in [
            "version = 1\nvolume = 75\n",
            "version = 1\nshuffle = true\n",
        ] {
            let config = parse(contents).unwrap();
            assert_eq!(config.repeat, RepeatMode::None, "{contents}");
        }
    }

    #[test]
    fn windows_paths_survive() {
        let config = parse("version = 1\nlibrary_dir = 'C:\\Users\\测试 用户\\Music'\n").unwrap();
        assert_eq!(
            config.library_dir,
            Some(PathBuf::from(r"C:\Users\测试 用户\Music"))
        );
    }

    #[test]
    fn unsupported_versions_and_bad_syntax_are_errors() {
        assert!(
            parse("version = 99\n")
                .unwrap_err()
                .contains("版本 99 不受支持")
        );
        assert!(parse("not valid = [").unwrap_err().contains("无法解析"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        assert!(load(&temp.path().join("config.toml")).is_none());
    }
}
