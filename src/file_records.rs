//! 按文件记的用户数据（收听进度、跳转点）共用的部分。
//!
//! 记录以文件路径为键，同时记下文件大小和修改时间：文件在音乐库里改名后，
//! 重扫时靠这两项把记录跟到新路径上。记录存成 JSON。文件内容用不了时先改名保留，
//! 再从空记录开始；文件读不了、或者没能改名保留时，这次运行不写盘，
//! 两种情况下都不会拿空记录覆盖还能找回的数据。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::atomic_write;
use crate::track::Track;

/// 一条记录，以及记下它时文件的大小和修改时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamped<T> {
    pub(crate) value: T,
    /// 文件大小和修改时间，改名后靠它们找回同一个文件。
    pub(crate) file_size: u64,
    pub(crate) modified_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRecords<T> {
    entries: HashMap<PathBuf, Stamped<T>>,
}

impl<T> Default for FileRecords<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<T> FromIterator<(PathBuf, Stamped<T>)> for FileRecords<T> {
    fn from_iter<I: IntoIterator<Item = (PathBuf, Stamped<T>)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

impl<T: Clone + PartialEq> FileRecords<T> {
    pub(crate) fn get(&self, path: &Path) -> Option<&T> {
        self.entries.get(path).map(|entry| &entry.value)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Path, &Stamped<T>)> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.as_path(), entry))
    }

    /// 给 `track` 记下 `value`，同时更新文件大小和修改时间；`None` 删掉这条记录。
    /// 记录有变化时返回 true。
    pub(crate) fn set(&mut self, track: &Track, value: Option<T>) -> bool {
        match value {
            Some(value) => {
                let entry = Stamped {
                    value,
                    file_size: track.file_size,
                    modified_ns: track.modified_ns,
                };
                self.entries.insert(track.path.clone(), entry.clone()) != Some(entry)
            }
            None => self.entries.remove(&track.path).is_some(),
        }
    }

    /// 文件改名后把记录跟过去：改名不改变文件大小和修改时间。
    ///
    /// 只处理音乐库目录下、已经找不到的记录（失联记录）。大小和修改时间相同的失联记录
    /// 只有一条，新曲库里自己还没有记录的曲目也只有一首时，才把这条记录搬过去。
    /// 两边任何一边有多个时都不动：多条失联记录对上同一首曲目，不知道改名的是哪一个；
    /// 一条对上多首，不知道改成了哪一个。猜错会把别的录音的进度和跳转点安到这一首上。
    /// 对不上的记录也保留（比如移动硬盘暂时没挂上）。有搬动时返回 true。
    pub(crate) fn follow_renames(&mut self, tracks: &[Track], library_dir: &Path) -> bool {
        let present: HashSet<&Path> = tracks.iter().map(|track| track.path.as_path()).collect();
        let mut targets: HashMap<(u64, i64), Vec<&Track>> = HashMap::new();
        for track in tracks {
            if !self.entries.contains_key(&track.path) {
                targets
                    .entry((track.file_size, track.modified_ns))
                    .or_default()
                    .push(track);
            }
        }
        let mut orphans: HashMap<(u64, i64), Vec<PathBuf>> = HashMap::new();
        for (path, entry) in &self.entries {
            if path.starts_with(library_dir) && !present.contains(path.as_path()) {
                orphans
                    .entry((entry.file_size, entry.modified_ns))
                    .or_default()
                    .push(path.clone());
            }
        }
        let mut moved = false;
        for (stamp, sources) in orphans {
            let ([source], Some([target])) =
                (sources.as_slice(), targets.get(&stamp).map(Vec::as_slice))
            else {
                continue;
            };
            // 目标自己没有记录，也只有这一组失联记录对得上它，不会覆盖别的记录。
            let entry = self
                .entries
                .remove(source)
                .expect("orphan comes from entries");
            self.entries.insert(target.path.clone(), entry);
            moved = true;
        }
        moved
    }
}

/// 读取记录文件的结果。
pub(crate) struct Loaded<R> {
    /// 读到的内容；还没有文件或内容用不了时为 None，从空记录开始。
    pub(crate) raw: Option<R>,
    /// 给用户的一句说明。
    pub(crate) message: Option<String>,
    /// 原文件还在原处，内容却没读出来：文件读不了，或者内容无法识别又没能改名保留。
    /// 这次运行不能写盘，否则空记录会覆盖原文件。
    pub(crate) read_only: bool,
}

/// 读取记录文件，`what` 是提示里的名称（如“收听进度”）。还没有文件时是空记录。
///
/// 内容不是 UTF-8（比如被编辑器另存成了 UTF-16 或 GBK）、不是 JSON 或 `accept` 不认
/// （比如版本不对）时，把文件改名保留后从空记录开始。文件读不了（权限、磁盘出错）时
/// 留在原处，问题排除后下次启动照常读取；它和没能改名保留的文件一样，这次运行不写盘。
pub(crate) fn load_json<R: DeserializeOwned>(
    path: &Path,
    what: &str,
    accept: impl FnOnce(&R) -> bool,
) -> Loaded<R> {
    let keep_original = format!("；本次运行不保存{what}，以免覆盖原文件");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Loaded {
                raw: None,
                message: None,
                read_only: false,
            };
        }
        Err(error) => {
            return Loaded {
                raw: None,
                message: Some(format!(
                    "无法读取{what} {}: {error}{keep_original}",
                    path.display()
                )),
                read_only: true,
            };
        }
    };
    // 整个文件先得是 UTF-8：serde_json 直接读字节时，跳过的字段里有坏字节也不报错。
    let parsed = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| serde_json::from_str::<R>(text).ok())
        .filter(accept);
    match parsed {
        Some(raw) => Loaded {
            raw: Some(raw),
            message: None,
            read_only: false,
        },
        None => match preserve_unreadable(path) {
            Ok(backup) => Loaded {
                raw: None,
                message: Some(format!(
                    "{what}文件无法识别，已改名保留为 {}，从空记录开始",
                    backup.display()
                )),
                read_only: false,
            },
            Err(error) => Loaded {
                raw: None,
                message: Some(format!(
                    "{what}文件无法识别，也无法改名保留: {error}{keep_original}"
                )),
                read_only: true,
            },
        },
    }
}

pub(crate) fn save_json<R: Serialize>(path: &Path, what: &str, raw: &R) -> io::Result<()> {
    let contents = serde_json::to_string_pretty(raw)
        .map_err(|error| io::Error::other(format!("无法序列化{what}: {error}")))?;
    atomic_write(path, contents.as_bytes())
}

/// 用不了的记录文件改名留在原处，返回改成的路径。
fn preserve_unreadable(path: &Path) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let extension = path
        .extension()
        .map(|extension| format!("{}.", extension.to_string_lossy()))
        .unwrap_or_default();
    let backup = path.with_extension(format!("{extension}corrupt-{nanos}"));
    fs::rename(path, &backup)?;
    Ok(backup)
}

#[cfg(test)]
pub(crate) mod tests {
    use serde::Deserialize;

    use super::*;

    /// 带 BOM 的 UTF-16LE：Windows 记事本“Unicode”另存出来的样子。
    pub(crate) fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        bytes
    }

    /// 和 `path` 同目录、改名保留下来的文件。
    pub(crate) fn backups(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|backup| backup.to_string_lossy().contains(".corrupt-"))
            .collect()
    }

    #[derive(Deserialize)]
    struct Raw {
        version: u32,
    }

    fn load(path: &Path) -> Loaded<Raw> {
        load_json(path, "测试记录", |raw: &Raw| raw.version == 1)
    }

    fn file(path: &str, size: u64) -> Track {
        Track {
            path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            title: "x".to_owned(),
            artist: None,
            album: None,
            duration: None,
            format: None,
            track_number: None,
            disc_number: None,
            file_size: size,
            modified_ns: 1,
        }
    }

    /// 给每个文件记下一个值，值就是它在 `files` 里的位置。
    fn records(files: &[Track]) -> FileRecords<usize> {
        let mut records = FileRecords::default();
        for (index, file) in files.iter().enumerate() {
            records.set(file, Some(index));
        }
        records
    }

    #[test]
    fn a_record_moves_when_one_lost_record_matches_one_new_file() {
        let old = [file("/lib/甲.flac", 100), file("/lib/乙.flac", 200)];
        let mut records = records(&old);
        // 两个文件各自改名：大小不同，两边都唯一，各搬各的。
        let renamed = [
            file("/lib/甲（备注）.flac", 100),
            file("/lib/乙（备注）.flac", 200),
        ];
        assert!(records.follow_renames(&renamed, Path::new("/lib")));
        assert_eq!(records.get(&renamed[0].path), Some(&0));
        assert_eq!(records.get(&renamed[1].path), Some(&1));
        assert!(records.get(&old[0].path).is_none());
        assert!(!records.follow_renames(&renamed, Path::new("/lib")));
    }

    #[test]
    fn several_lost_records_matching_one_new_file_are_all_left_alone() {
        // 两条记录的文件大小和修改时间相同（硬链接、保留修改时间的副本）。
        // 一个改了名，另一个移出了音乐库：不知道新文件是哪一个，两条都不动。
        // HashMap 的遍历顺序每次不同，多试几次，以前每次随便搬走其中一条。
        let old = [file("/lib/甲.flac", 100), file("/lib/乙.flac", 100)];
        let renamed = [file("/lib/新名字.flac", 100)];
        for _ in 0..20 {
            let mut records = records(&old);
            assert!(!records.follow_renames(&renamed, Path::new("/lib")));
            assert_eq!(records.get(&old[0].path), Some(&0));
            assert_eq!(records.get(&old[1].path), Some(&1));
            assert!(records.get(&renamed[0].path).is_none());
        }
    }

    #[test]
    fn one_lost_record_matching_several_new_files_is_left_alone() {
        let old = [file("/lib/旧.flac", 100)];
        let mut records = records(&old);
        let twins = [file("/lib/甲.flac", 100), file("/lib/乙.flac", 100)];
        assert!(!records.follow_renames(&twins, Path::new("/lib")));
        assert_eq!(records.get(&old[0].path), Some(&0));
        assert!(twins.iter().all(|twin| records.get(&twin.path).is_none()));
    }

    #[test]
    fn content_that_is_not_utf8_is_kept_aside_like_any_unrecognized_content() {
        // UTF-16 的合法 JSON，以及用不到的字段里是 GBK 字节（“掏耳”）的 JSON。
        let gbk = b"{\"version\":1,\"name\":\"\xcc\xcd\xb6\xfa\"}".to_vec();
        for original in [utf16(r#"{"version":1}"#), gbk] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("records.json");
            fs::write(&path, &original).unwrap();

            let loaded = load(&path);
            assert!(loaded.raw.is_none());
            assert!(!loaded.read_only);
            let message = loaded.message.unwrap();
            assert!(
                message.starts_with("测试记录文件无法识别，已改名保留为"),
                "{message}"
            );
            assert!(!path.exists());
            let kept = backups(&path);
            assert_eq!(kept.len(), 1);
            assert_eq!(fs::read(&kept[0]).unwrap(), original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_stays_where_it_is_and_nothing_is_written() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("records.json");
        fs::write(&path, r#"{"version":1}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&path).is_ok() {
            // 以 root 运行时权限挡不住读取，这个场景无从模拟。
            return;
        }

        let loaded = load(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(loaded.raw.is_none());
        assert!(loaded.read_only);
        let message = loaded.message.unwrap();
        assert!(message.starts_with("无法读取测试记录"), "{message}");
        assert!(
            message.ends_with("；本次运行不保存测试记录，以免覆盖原文件"),
            "{message}"
        );
        // 不改名：问题排除后下次启动照常读取。
        assert!(backups(&path).is_empty());
        assert!(load(&path).raw.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn unrecognized_content_that_cannot_be_kept_aside_makes_the_store_read_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("records.json");
        fs::write(&path, utf16(r#"{"version":1}"#)).unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(temp.path().join("probe"), b"").is_ok() {
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let loaded = load(&path);
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(loaded.raw.is_none());
        assert!(loaded.read_only);
        let message = loaded.message.unwrap();
        assert!(
            message.starts_with("测试记录文件无法识别，也无法改名保留"),
            "{message}"
        );
        assert!(message.ends_with("以免覆盖原文件"), "{message}");
        assert_eq!(fs::read(&path).unwrap(), utf16(r#"{"version":1}"#));
    }
}
