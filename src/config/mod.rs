//! 平台标准路径和用户配置。
//!
//! 配置写在 `config.kdl` 里，归用户自己维护：程序只在第一次启动时生成它，
//! 退出时只把运行中改过的那几项写回原处，其余内容和注释保持原样。

mod document;
mod legacy;
mod template;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::theme::Theme;
use crate::track::{RepeatMode, SortOrder};

const APP_DIR: &str = "tui-music-player";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_file: PathBuf,
    /// 1.4.0 之前的 `config.toml`，只在第一次生成 `config.kdl` 时读取。
    pub legacy_config_file: PathBuf,
    pub playlists_dir: PathBuf,
    pub session_file: PathBuf,
    /// 长录音各自的收听进度。
    pub progress_file: PathBuf,
    pub cache_db: PathBuf,
    pub default_music_dir: Option<PathBuf>,
}

impl AppPaths {
    pub fn discover() -> io::Result<Self> {
        let config = dirs::config_dir().ok_or_else(|| missing_dir("配置"))?;
        let data = dirs::data_dir().ok_or_else(|| missing_dir("应用数据"))?;
        let cache = dirs::cache_dir().ok_or_else(|| missing_dir("缓存"))?;
        Ok(Self::from_roots(config, data, cache, dirs::audio_dir()))
    }

    pub fn from_roots(
        config: PathBuf,
        data: PathBuf,
        cache: PathBuf,
        music: Option<PathBuf>,
    ) -> Self {
        Self {
            config_file: config.join(APP_DIR).join("config.kdl"),
            legacy_config_file: config.join(APP_DIR).join("config.toml"),
            playlists_dir: data.join(APP_DIR).join("playlists"),
            session_file: data.join(APP_DIR).join("session.toml"),
            progress_file: data.join(APP_DIR).join("progress.json"),
            cache_db: cache.join(APP_DIR).join("library.sqlite3"),
            default_music_dir: music,
        }
    }
}

fn missing_dir(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("系统没有提供{name}用户目录"),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub library_dir: Option<PathBuf>,
    pub volume: u8,
    pub muted: bool,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// 左右平衡，-100（只有左声道）到 100（只有右声道），0 居中。
    pub balance: i8,
    /// 左右声道互换。
    pub swap_channels: bool,
    pub sort: SortOrder,
    pub visualizer_enabled: bool,
    pub mouse_enabled: bool,
    pub lyrics_enabled: bool,
    /// 隐私模式：系统媒体控制只收到“正在播放”，不知道标题、歌手、专辑和文件路径。
    pub privacy: bool,
    /// 睡眠定时到点前淡出的时长。启动时读取，运行中不会改变，也从不写回。
    pub sleep_fade: Duration,
    /// 启动时按 `theme {}` 得出，运行中不会改变，也从不写回。
    pub theme: Theme,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            library_dir: None,
            volume: 100,
            muted: false,
            repeat: RepeatMode::Stop,
            shuffle: false,
            balance: 0,
            swap_channels: false,
            sort: SortOrder::default(),
            visualizer_enabled: true,
            mouse_enabled: true,
            lyrics_enabled: true,
            privacy: false,
            sleep_fade: Duration::from_secs(30),
            theme: Theme::default(),
        }
    }
}

/// 启动时读配置的结果。
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: AppConfig,
    /// 要在底栏显示的一行提示：新建或迁移了配置，或者配置里有写错的地方。
    pub message: Option<String>,
    /// 退出时能否把改动写回。文件有语法错误时为 false，程序不会去碰它。
    pub writable: bool,
}

impl AppConfig {
    /// 读取 `config.kdl`；还没有时生成一份，有旧的 `config.toml` 就沿用其中的设置。
    pub fn load(paths: &AppPaths) -> io::Result<LoadedConfig> {
        match fs::read_to_string(&paths.config_file) {
            Ok(text) => Ok(document::read(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(create(paths)),
            Err(error) => Err(error),
        }
    }

    /// 把相对 `start` 改过的设置写回 `config.kdl`，只改动这几项所在的行。
    ///
    /// 没有改动时不碰文件；文件在运行中被删掉时也不再重建。
    pub fn save_changes(&self, start: &AppConfig, path: &Path) -> Result<(), String> {
        if !document::has_changes(start, self) {
            return Ok(());
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("无法读取配置 {}: {error}", path.display())),
        };
        let updated = document::write_changes(&text, start, self)?;
        atomic_write(path, updated.as_bytes())
            .map_err(|error| format!("无法保存配置 {}: {error}", path.display()))
    }
}

/// 把主音乐库写进 `config.kdl` 的 `library` 一项，文件其余部分保持原样。
pub fn set_library(path: &Path, library: &Path) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("无法读取配置 {}: {error}", path.display()))?;
    let updated = document::set_library(&text, library)?;
    atomic_write(path, updated.as_bytes())
        .map_err(|error| format!("无法保存配置 {}: {error}", path.display()))
}

/// 第一次启动：按旧配置或默认设置生成 `config.kdl`。
fn create(paths: &AppPaths) -> LoadedConfig {
    let path = paths.config_file.display();
    let (mut config, note) = match legacy::load(&paths.legacy_config_file) {
        Some(Ok(config)) => (config, format!("已把 config.toml 的设置迁移到 {path}")),
        Some(Err(error)) => (
            AppConfig::default(),
            format!("{error}；已按默认设置创建 {path}"),
        ),
        None => (AppConfig::default(), format!("已创建配置文件 {path}")),
    };
    if config.library_dir.is_none() {
        config.library_dir = paths.default_music_dir.clone();
    }
    let message = match atomic_write(&paths.config_file, template::render(&config).as_bytes()) {
        Ok(()) => note,
        Err(error) => format!("无法创建配置文件 {path}: {error}"),
    };
    LoadedConfig {
        config,
        message: Some(message),
        writable: true,
    }
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标文件路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标文件名不是 UTF-8"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));

    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::SortKey;

    fn paths_in(root: &Path) -> AppPaths {
        AppPaths::from_roots(
            root.join("config"),
            root.join("data"),
            root.join("cache"),
            Some(root.join("music")),
        )
    }

    #[test]
    fn app_paths_keep_config_data_cache_and_music_separate() {
        let paths = AppPaths::from_roots(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
            Some(PathBuf::from("/music")),
        );
        assert_eq!(
            paths.config_file,
            PathBuf::from("/config/tui-music-player/config.kdl")
        );
        assert_eq!(
            paths.legacy_config_file,
            PathBuf::from("/config/tui-music-player/config.toml")
        );
        assert_eq!(
            paths.playlists_dir,
            PathBuf::from("/data/tui-music-player/playlists")
        );
        assert_eq!(
            paths.cache_db,
            PathBuf::from("/cache/tui-music-player/library.sqlite3")
        );
        assert_eq!(
            paths.session_file,
            PathBuf::from("/data/tui-music-player/session.toml")
        );
        assert_eq!(
            paths.progress_file,
            PathBuf::from("/data/tui-music-player/progress.json")
        );
        assert_eq!(paths.default_music_dir, Some(PathBuf::from("/music")));
    }

    #[test]
    fn the_first_start_writes_a_template_with_the_default_music_dir() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths_in(temp.path());

        let loaded = AppConfig::load(&paths).unwrap();
        assert!(loaded.writable);
        assert!(loaded.message.unwrap().contains("已创建配置文件"));
        let expected = AppConfig {
            library_dir: paths.default_music_dir.clone(),
            ..AppConfig::default()
        };
        assert_eq!(loaded.config, expected);

        // 生成的文件读回来是同一份设置，再次启动不再提示。
        let again = AppConfig::load(&paths).unwrap();
        assert_eq!(again.message, None);
        assert_eq!(again.config, expected);
    }

    #[test]
    fn the_first_start_carries_over_the_old_config_toml() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths_in(temp.path());
        fs::create_dir_all(paths.legacy_config_file.parent().unwrap()).unwrap();
        let old = "version = 1\nlibrary_dir = \"/srv/音乐\"\nvolume = 42\nmuted = true\n\
                   repeat = \"one\"\nshuffle = true\nlyrics_enabled = false\n\n\
                   [sort]\nkey = \"album\"\ndescending = true\n";
        fs::write(&paths.legacy_config_file, old).unwrap();

        let loaded = AppConfig::load(&paths).unwrap();
        assert!(
            loaded
                .message
                .unwrap()
                .contains("已把 config.toml 的设置迁移到")
        );
        let expected = AppConfig {
            library_dir: Some(PathBuf::from("/srv/音乐")),
            volume: 42,
            muted: true,
            repeat: RepeatMode::One,
            shuffle: true,
            sort: SortOrder {
                key: SortKey::Album,
                descending: true,
            },
            lyrics_enabled: false,
            ..AppConfig::default()
        };
        assert_eq!(loaded.config, expected);
        assert_eq!(AppConfig::load(&paths).unwrap().config, expected);
        // 旧文件留着，退回旧版本时还能用。
        assert_eq!(fs::read_to_string(&paths.legacy_config_file).unwrap(), old);
    }

    #[test]
    fn a_broken_old_config_toml_is_reported_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths_in(temp.path());
        fs::create_dir_all(paths.legacy_config_file.parent().unwrap()).unwrap();
        fs::write(&paths.legacy_config_file, "not valid = [").unwrap();

        let loaded = AppConfig::load(&paths).unwrap();
        let message = loaded.message.unwrap();
        assert!(message.contains("无法解析"), "{message}");
        assert!(message.contains("已按默认设置创建"), "{message}");
        assert_eq!(loaded.config.volume, 100);
        assert_eq!(
            fs::read_to_string(&paths.legacy_config_file).unwrap(),
            "not valid = ["
        );
    }

    #[test]
    fn saving_touches_only_the_changed_lines() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.kdl");
        let text = "// 我的配置\nplayback {\n    volume 60 // 夜里小声点\n}\n";
        fs::write(&path, text).unwrap();
        let start = document::read(text).config;

        // 没有改动就不碰文件。
        start.save_changes(&start, &path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), text);

        let now = AppConfig {
            volume: 35,
            ..start.clone()
        };
        now.save_changes(&start, &path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "// 我的配置\nplayback {\n    volume 35 // 夜里小声点\n}\n"
        );
    }

    #[test]
    fn a_config_deleted_while_running_is_not_recreated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.kdl");
        let start = AppConfig::default();
        let now = AppConfig {
            shuffle: true,
            ..start.clone()
        };
        now.save_changes(&start, &path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn set_library_rewrites_only_the_library_line() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.kdl");
        fs::write(&path, "// 音乐在这\nlibrary \"~/Music\" // 旧的\n").unwrap();
        set_library(&path, Path::new("/srv/音乐")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "// 音乐在这\nlibrary \"/srv/音乐\" // 旧的\n"
        );
    }
}
