//! 退出时的播放位置和文件夹筛选，供下次启动恢复。它是可丢弃的状态：
//! 读不出来就当没有，不提示也不阻止启动。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::track::Folder;

const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// 保存时的音乐库；换了库就不恢复，避免选中别的库里的同名路径或同名文件夹。
    pub library_dir: PathBuf,
    /// 退出时装载着的曲目；没有时只恢复文件夹筛选。
    pub track: Option<PathBuf>,
    /// 文件里的位置。放的是播放列表里的片段时也用文件时间，旧版本读到时照样恢复到同一个地方。
    pub position: Duration,
    /// 退出时放的是播放列表里的哪一条；曲库播放时为 None。
    pub entry: Option<ListEntry>,
    /// 退出时的文件夹筛选；None 表示显示全部。
    pub folder: Option<Folder>,
}

/// 播放列表里的一条：列表名和第几条（从 1 数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub playlist: String,
    pub number: usize,
}

/// 没有曲目、列表条目或筛选的字段直接省略。旧版本保存的文件没有 `folder`、`playlist`
/// 和 `entry`，照样能读；旧版本读到这几个字段时忽略它们，只恢复曲目和位置；
/// 读到只有文件夹、没有曲目的文件时当作没有会话。
#[derive(Serialize, Deserialize)]
struct RawSession {
    version: u32,
    library_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track: Option<PathBuf>,
    #[serde(default)]
    position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playlist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<usize>,
    // TOML 里表要排在普通值后面，所以文件夹放在最后。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<RawFolder>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawFolder {
    Root,
    Named(String),
}

impl Session {
    pub fn load(path: &Path) -> Option<Self> {
        let contents = fs::read_to_string(path).ok()?;
        let raw: RawSession = toml::from_str(&contents).ok()?;
        (raw.version == SESSION_VERSION).then(|| Self {
            library_dir: raw.library_dir,
            track: raw.track,
            position: Duration::from_millis(raw.position_ms),
            entry: raw
                .playlist
                .zip(raw.entry)
                .map(|(playlist, number)| ListEntry { playlist, number }),
            folder: raw.folder.map(|folder| match folder {
                RawFolder::Root => Folder::Root,
                RawFolder::Named(name) => Folder::Named(name),
            }),
        })
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let raw = RawSession {
            version: SESSION_VERSION,
            library_dir: self.library_dir.clone(),
            track: self.track.clone(),
            position_ms: u64::try_from(self.position.as_millis()).unwrap_or(u64::MAX),
            playlist: self.entry.as_ref().map(|entry| entry.playlist.clone()),
            entry: self.entry.as_ref().map(|entry| entry.number),
            folder: self.folder.as_ref().map(|folder| match folder {
                Folder::Root => RawFolder::Root,
                Folder::Named(name) => RawFolder::Named(name.clone()),
            }),
        };
        let contents = toml::to_string_pretty(&raw)
            .map_err(|error| io::Error::other(format!("无法序列化会话: {error}")))?;
        atomic_write(path, contents.as_bytes())
    }

    /// 没有可恢复的曲目和筛选时删掉旧会话，下次启动就不会回到更早的歌。
    pub fn clear(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_and_clear_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state/session.toml");
        let session = Session {
            library_dir: PathBuf::from("/music"),
            track: Some(PathBuf::from("/music/测试 歌曲.flac")),
            position: Duration::from_millis(83_250),
            entry: None,
            folder: Some(Folder::Named("ASMR(衣鶴)".to_owned())),
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path), Some(session));

        // 播放列表里的一条：列表名和第几条排在文件夹之前。
        let session = Session {
            library_dir: PathBuf::from("/music"),
            track: Some(PathBuf::from("/music/A.m4a")),
            position: Duration::from_millis(812_000),
            entry: Some(ListEntry {
                playlist: "睡前".to_owned(),
                number: 3,
            }),
            folder: Some(Folder::Root),
        };
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path), Some(session));
        let saved = fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("playlist = \"睡前\"\nentry = 3\n"),
            "{saved}"
        );

        // 只有文件夹筛选、没有曲目也能保存；根目录也是一种筛选。
        for folder in [Folder::Root, Folder::Named("Music".to_owned())] {
            let session = Session {
                library_dir: PathBuf::from("/music"),
                track: None,
                position: Duration::ZERO,
                entry: None,
                folder: Some(folder),
            };
            session.save(&path).unwrap();
            assert_eq!(Session::load(&path), Some(session));
        }

        Session::clear(&path).unwrap();
        assert_eq!(Session::load(&path), None);
        Session::clear(&path).unwrap();
    }

    #[test]
    fn a_session_saved_before_folder_filters_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.toml");
        fs::write(
            &path,
            "version = 1\nlibrary_dir = \"/m\"\ntrack = \"/m/a.wav\"\nposition_ms = 1500\n",
        )
        .unwrap();
        assert_eq!(
            Session::load(&path),
            Some(Session {
                library_dir: PathBuf::from("/m"),
                track: Some(PathBuf::from("/m/a.wav")),
                position: Duration::from_millis(1500),
                entry: None,
                folder: None,
            })
        );
    }

    #[test]
    fn unreadable_or_foreign_session_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.toml");
        fs::write(&path, "not valid = [").unwrap();
        assert_eq!(Session::load(&path), None);

        fs::write(
            &path,
            "version = 99\nlibrary_dir = \"/m\"\ntrack = \"/m/a.wav\"\nposition_ms = 1\n",
        )
        .unwrap();
        assert_eq!(Session::load(&path), None);
    }
}
