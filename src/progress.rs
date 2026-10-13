//! 长录音各自的收听进度：听到哪里、是否听完。它是用户数据，不放在可以随时删掉的缓存里。
//!
//! 只记录不短于 [`LONG_RECORDING`] 的录音，歌曲照旧每次从头播放。时长以扫描时读到的为准，
//! 记录、续播和曲库里的显示因此总是一致；时长未知的文件不记录。
//! 文件读不了时先改名保留，再从空记录开始，下次保存不会覆盖还能找回的数据。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::track::Track;

const PROGRESS_VERSION: u32 = 1;
/// 达到这个时长的录音才记进度、才续播。
pub const LONG_RECORDING: Duration = Duration::from_secs(600);
/// 离结尾这么近时视为已经听完，下次从头开始。
pub const NEAR_END: Duration = Duration::from_secs(3);
/// 离开头这么近时视为还没开始听。
const NEAR_START: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listened {
    /// 听到一半，下次从这里继续。
    InProgress(Duration),
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    listened: Listened,
    /// 文件大小和修改时间，改名后靠它们找回同一个文件。
    file_size: u64,
    modified_ns: i64,
}

#[derive(Serialize, Deserialize)]
struct RawStore {
    version: u32,
    recordings: Vec<RawEntry>,
}

#[derive(Serialize, Deserialize)]
struct RawEntry {
    path: String,
    position_ms: u64,
    finished: bool,
    file_size: u64,
    modified_ns: i64,
}

/// 是否是记进度、会续播的长录音。
pub fn is_long(track: &Track) -> bool {
    track
        .duration
        .is_some_and(|duration| duration >= LONG_RECORDING)
}

#[derive(Debug, Default)]
pub struct ProgressStore {
    entries: HashMap<PathBuf, Entry>,
}

impl ProgressStore {
    /// 读取进度文件；还没有时是空记录。读不了或格式不对时把文件改名保留，
    /// 返回空记录和一句说明。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("无法读取收听进度 {}: {error}", path.display())),
                );
            }
        };
        match serde_json::from_str::<RawStore>(&contents) {
            Ok(raw) if raw.version == PROGRESS_VERSION => (Self::from_raw(raw), None),
            _ => (Self::default(), Some(preserve_unreadable(path))),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut recordings: Vec<RawEntry> = self
            .entries
            .iter()
            // JSON 只能存 UTF-8 文本；这样的路径极少见，不记录它们的进度。
            .filter_map(|(path, entry)| {
                let (position, finished) = match entry.listened {
                    Listened::InProgress(position) => (position, false),
                    Listened::Finished => (Duration::ZERO, true),
                };
                Some(RawEntry {
                    path: path.to_str()?.to_owned(),
                    position_ms: u64::try_from(position.as_millis()).unwrap_or(u64::MAX),
                    finished,
                    file_size: entry.file_size,
                    modified_ns: entry.modified_ns,
                })
            })
            .collect();
        recordings.sort_by(|left, right| left.path.cmp(&right.path));
        let raw = RawStore {
            version: PROGRESS_VERSION,
            recordings,
        };
        let contents = serde_json::to_string_pretty(&raw)
            .map_err(|error| io::Error::other(format!("无法序列化收听进度: {error}")))?;
        atomic_write(path, contents.as_bytes())
    }

    pub fn get(&self, path: &Path) -> Option<Listened> {
        self.entries.get(path).map(|entry| entry.listened)
    }

    /// 记下 `track` 停在 `position`，总时长 `duration`；记录有变化时返回 true。
    ///
    /// 离结尾很近算听完；离开头很近算没开始听，只是打开一首听完的录音不会抹掉“听完”。
    pub fn record(&mut self, track: &Track, position: Duration, duration: Duration) -> bool {
        let listened = if position + NEAR_END >= duration {
            Some(Listened::Finished)
        } else if position < NEAR_START {
            self.get(&track.path)
                .filter(|listened| *listened == Listened::Finished)
        } else {
            Some(Listened::InProgress(position))
        };
        self.set(track, listened)
    }

    /// 记下 `track` 已经听完；记录有变化时返回 true。
    pub fn finish(&mut self, track: &Track) -> bool {
        self.set(track, Some(Listened::Finished))
    }

    fn set(&mut self, track: &Track, listened: Option<Listened>) -> bool {
        match listened {
            Some(listened) => {
                let entry = Entry {
                    listened,
                    file_size: track.file_size,
                    modified_ns: track.modified_ns,
                };
                self.entries.insert(track.path.clone(), entry.clone()) != Some(entry)
            }
            None => self.entries.remove(&track.path).is_some(),
        }
    }

    fn from_raw(raw: RawStore) -> Self {
        let entries = raw
            .recordings
            .into_iter()
            .map(|entry| {
                let listened = if entry.finished {
                    Listened::Finished
                } else {
                    Listened::InProgress(Duration::from_millis(entry.position_ms))
                };
                (
                    PathBuf::from(entry.path),
                    Entry {
                        listened,
                        file_size: entry.file_size,
                        modified_ns: entry.modified_ns,
                    },
                )
            })
            .collect();
        Self { entries }
    }
}

/// 读不了的进度文件改名留在原处，换一个空文件重新开始。
fn preserve_unreadable(path: &Path) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let backup = path.with_extension(format!("json.corrupt-{nanos}"));
    match fs::rename(path, &backup) {
        Ok(()) => format!(
            "收听进度文件无法识别，已改名保留为 {}，从空记录开始",
            backup.display()
        ),
        Err(error) => format!("收听进度文件无法识别，也无法改名保留: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(path: &str, minutes: u64) -> Track {
        Track {
            path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            title: "x".to_owned(),
            artist: None,
            album: None,
            duration: Some(Duration::from_secs(minutes * 60)),
            format: None,
            track_number: None,
            disc_number: None,
            file_size: 7,
            modified_ns: 9,
        }
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn only_recordings_of_ten_minutes_or_more_are_long() {
        assert!(is_long(&recording("/m/a.flac", 10)));
        assert!(!is_long(&recording("/m/b.flac", 9)));
        let mut unknown = recording("/m/c.flac", 30);
        unknown.duration = None;
        assert!(!is_long(&unknown));
    }

    #[test]
    fn positions_near_the_ends_mean_finished_or_not_started() {
        let track = recording("/m/a.flac", 20);
        let duration = 20 * MINUTE;
        let mut store = ProgressStore::default();

        assert!(store.record(&track, 5 * MINUTE, duration));
        assert_eq!(
            store.get(&track.path),
            Some(Listened::InProgress(5 * MINUTE))
        );
        assert!(!store.record(&track, 5 * MINUTE, duration));

        assert!(store.record(&track, duration - Duration::from_secs(2), duration));
        assert_eq!(store.get(&track.path), Some(Listened::Finished));
        // 打开一首听完的录音，还停在开头，不算重新开始听。
        assert!(!store.record(&track, Duration::from_secs(1), duration));
        assert_eq!(store.get(&track.path), Some(Listened::Finished));

        // 听了一段又拖回开头：当作没开始听。
        store.record(&track, 2 * MINUTE, duration);
        assert!(store.record(&track, Duration::ZERO, duration));
        assert_eq!(store.get(&track.path), None);
    }

    #[test]
    fn the_store_round_trips_through_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data/progress.json");
        let mut store = ProgressStore::default();
        let first = recording("/m/音频 - 第一段.flac", 30);
        let second = recording("/m/音频 - 第二段.m4a", 30);
        store.record(&first, Duration::from_millis(754_250), 30 * MINUTE);
        store.finish(&second);
        store.save(&path).unwrap();

        let (loaded, message) = ProgressStore::load(&path);
        assert_eq!(message, None);
        assert_eq!(
            loaded.get(&first.path),
            Some(Listened::InProgress(Duration::from_millis(754_250)))
        );
        assert_eq!(loaded.get(&second.path), Some(Listened::Finished));
        assert_eq!(loaded.entries, store.entries);
    }

    #[test]
    fn a_missing_file_is_an_empty_store_and_an_unreadable_one_is_kept() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("progress.json");
        let (store, message) = ProgressStore::load(&path);
        assert!(store.entries.is_empty());
        assert_eq!(message, None);

        for contents in ["not json", r#"{"version":99,"recordings":[]}"#] {
            fs::write(&path, contents).unwrap();
            let (store, message) = ProgressStore::load(&path);
            assert!(store.entries.is_empty());
            assert!(message.unwrap().contains("已改名保留"));
            assert!(!path.exists(), "原文件应已改名");
        }
        let kept = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
            .count();
        assert_eq!(kept, 2);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_left_out_of_the_file() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("progress.json");
        let mut store = ProgressStore::default();
        let mut odd = recording("/m/x.flac", 30);
        odd.path = PathBuf::from(OsStr::from_bytes(b"/m/non-utf8-\xFF.flac"));
        let plain = recording("/m/plain.flac", 30);
        store.finish(&odd);
        store.finish(&plain);
        store.save(&path).unwrap();

        let (loaded, _) = ProgressStore::load(&path);
        assert_eq!(loaded.get(&plain.path), Some(Listened::Finished));
        assert_eq!(loaded.get(&odd.path), None);
    }
}
