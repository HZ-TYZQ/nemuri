//! 长录音各自的收听进度：听到哪里、是否听完。它是用户数据，不放在可以随时删掉的缓存里。
//!
//! 只记录不短于 [`LONG_RECORDING`] 的录音，歌曲照旧每次从头播放。时长以扫描时读到的为准，
//! 记录、续播和曲库里的显示因此总是一致；时长未知的文件不记录。
//! 文件内容用不了时先改名保留，再从空记录开始；文件读不了或没能改名保留时，
//! 这次运行只在内存里记，不写盘。两种情况都不会覆盖还能找回的数据。

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::file_records::{self, FileRecords, Stamped};
use crate::track::Track;

const PROGRESS_VERSION: u32 = 1;
/// 提示里对这份数据的称呼。
const WHAT: &str = "收听进度";
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
    entries: FileRecords<Listened>,
    /// 启动时原文件读不了或没能改名保留：这次运行不写盘，免得覆盖它。
    read_only: bool,
}

impl ProgressStore {
    /// 读取进度文件；还没有时是空记录。用不了时返回空记录和一句说明，
    /// 规则见 [`file_records::load_json`]。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let loaded =
            file_records::load_json(path, WHAT, |raw: &RawStore| raw.version == PROGRESS_VERSION);
        let mut store = loaded.raw.map(Self::from_raw).unwrap_or_default();
        store.read_only = loaded.read_only;
        (store, loaded.message)
    }

    /// 写盘。启动时原文件读不了或没能保留时什么也不做：启动时已经提示过，
    /// 收听进度只是便利功能，不值得每次都报错。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut recordings: Vec<RawEntry> = self
            .entries
            .iter()
            // JSON 只能存 UTF-8 文本；这样的路径极少见，不记录它们的进度。
            .filter_map(|(path, entry)| {
                let (position, finished) = match entry.value {
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
        file_records::save_json(path, WHAT, &raw)
    }

    pub fn get(&self, path: &Path) -> Option<Listened> {
        self.entries.get(path).copied()
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
        self.entries.set(track, listened)
    }

    /// 记下 `track` 已经听完；记录有变化时返回 true。
    pub fn finish(&mut self, track: &Track) -> bool {
        self.entries.set(track, Some(Listened::Finished))
    }

    /// 文件改名后把进度跟过去，规则见 [`FileRecords::follow_renames`]。有搬动时返回 true。
    pub fn follow_renames(&mut self, tracks: &[Track], library_dir: &Path) -> bool {
        self.entries.follow_renames(tracks, library_dir)
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
                    Stamped {
                        value: listened,
                        file_size: entry.file_size,
                        modified_ns: entry.modified_ns,
                    },
                )
            })
            .collect();
        Self {
            entries,
            read_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

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

    fn file(path: &str, size: u64, modified: i64) -> Track {
        let mut track = recording(path, 30);
        track.file_size = size;
        track.modified_ns = modified;
        track
    }

    #[test]
    fn progress_follows_a_file_renamed_inside_the_library() {
        let mut store = ProgressStore::default();
        let old = file("/lib/A/旧名字.flac", 100, 1);
        store.record(&old, 5 * MINUTE, 30 * MINUTE);

        let renamed = file("/lib/A/新名字（备注）.flac", 100, 1);
        let other = file("/lib/A/别的.flac", 200, 1);
        assert!(store.follow_renames(&[renamed.clone(), other], Path::new("/lib")));
        assert_eq!(store.get(&old.path), None);
        assert_eq!(
            store.get(&renamed.path),
            Some(Listened::InProgress(5 * MINUTE))
        );
        // 再扫一次没有可搬的了。
        assert!(!store.follow_renames(&[renamed], Path::new("/lib")));
    }

    #[test]
    fn ambiguous_or_foreign_matches_are_left_alone() {
        let mut store = ProgressStore::default();
        let old = file("/lib/旧.flac", 100, 1);
        store.finish(&old);

        // 两个候选：不知道是哪一个，不搬。
        let twins = [file("/lib/甲.flac", 100, 1), file("/lib/乙.flac", 100, 1)];
        assert!(!store.follow_renames(&twins, Path::new("/lib")));
        // 唯一的候选自己已经有记录：不覆盖。
        let mut store_with_target = ProgressStore::default();
        store_with_target.finish(&old);
        let target = file("/lib/甲.flac", 100, 1);
        store_with_target.record(&target, 5 * MINUTE, 30 * MINUTE);
        assert!(
            !store_with_target.follow_renames(std::slice::from_ref(&target), Path::new("/lib"))
        );
        assert_eq!(
            store_with_target.get(&target.path),
            Some(Listened::InProgress(5 * MINUTE))
        );
        // 不在这个音乐库里的记录（比如另一个库或没挂上的硬盘）不动。
        let mut elsewhere = ProgressStore::default();
        let outside = file("/other/旧.flac", 100, 1);
        elsewhere.finish(&outside);
        assert!(!elsewhere.follow_renames(&[file("/lib/新.flac", 100, 1)], Path::new("/lib")));
        assert_eq!(elsewhere.get(&outside.path), Some(Listened::Finished));
        assert_eq!(store.get(&old.path), Some(Listened::Finished));
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

    /// 一条进度记录的 JSON，含 /m/旧.flac 听到 1 分钟。
    const SAVED: &str = r#"{"version":1,"recordings":[{"path":"/m/旧.flac","position_ms":60000,"finished":false,"file_size":7,"modified_ns":9}]}"#;

    #[test]
    fn a_utf16_file_is_kept_aside_before_new_progress_is_written() {
        use crate::file_records::tests::{backups, utf16};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("progress.json");
        fs::write(&path, utf16(SAVED)).unwrap();

        let (mut store, message) = ProgressStore::load(&path);
        assert!(message.unwrap().contains("已改名保留"));
        let track = recording("/m/新.flac", 30);
        store.record(&track, 5 * MINUTE, 30 * MINUTE);
        store.save(&path).unwrap();

        let kept = backups(&path);
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read(&kept[0]).unwrap(), utf16(SAVED));
        let (loaded, _) = ProgressStore::load(&path);
        assert_eq!(
            loaded.get(&track.path),
            Some(Listened::InProgress(5 * MINUTE))
        );
    }

    #[cfg(unix)]
    #[test]
    fn progress_that_could_not_be_loaded_is_never_written_over() {
        use std::os::unix::fs::PermissionsExt;

        use crate::file_records::tests::utf16;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("progress.json");
        let track = recording("/m/新.flac", 30);

        // 读不了的文件：目录照样可写，没有防护就会被整个替换掉。
        fs::write(&path, SAVED).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&path).is_err() {
            let (mut store, message) = ProgressStore::load(&path);
            assert!(message.unwrap().contains("本次运行不保存收听进度"));
            assert!(store.record(&track, 5 * MINUTE, 30 * MINUTE));
            store.save(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        // 无法识别又没能改名保留：之后目录又能写了，也不覆盖。
        fs::write(&path, utf16(SAVED)).unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(temp.path().join("probe"), b"").is_err() {
            let (mut store, message) = ProgressStore::load(&path);
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            assert!(message.unwrap().contains("也无法改名保留"));
            store.record(&track, 5 * MINUTE, 30 * MINUTE);
            store.save(&path).unwrap();
            assert_eq!(fs::read(&path).unwrap(), utf16(SAVED));
        }
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
}
