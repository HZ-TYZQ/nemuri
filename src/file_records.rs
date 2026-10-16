//! 按文件记的用户数据（收听进度、跳转点）共用的部分。
//!
//! 记录以文件路径为键，同时记下文件大小和修改时间：文件在音乐库里改名后，
//! 重扫时靠这两项把记录跟到新路径上。记录存成 JSON；文件读不了时先改名保留，
//! 再从空记录开始，下次保存不会覆盖还能找回的数据。

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
    /// 只处理音乐库目录下、已经找不到的记录，而且只在新曲库里恰好有一首
    /// 大小和修改时间都相同、自己还没有记录的曲目时才搬。对不上或有多个候选时不动，
    /// 找不到的记录也保留（比如移动硬盘暂时没挂上）。有搬动时返回 true。
    pub(crate) fn follow_renames(&mut self, tracks: &[Track], library_dir: &Path) -> bool {
        let present: HashSet<&Path> = tracks.iter().map(|track| track.path.as_path()).collect();
        let mut candidates: HashMap<(u64, i64), Vec<&Track>> = HashMap::new();
        for track in tracks {
            if !self.entries.contains_key(&track.path) {
                candidates
                    .entry((track.file_size, track.modified_ns))
                    .or_default()
                    .push(track);
            }
        }
        let orphans: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|path| path.starts_with(library_dir) && !present.contains(path.as_path()))
            .cloned()
            .collect();
        let mut moved = false;
        for orphan in orphans {
            let entry = &self.entries[&orphan];
            let Some([target]) = candidates
                .get(&(entry.file_size, entry.modified_ns))
                .map(Vec::as_slice)
            else {
                continue;
            };
            let target = target.path.clone();
            if self.entries.contains_key(&target) {
                continue;
            }
            let entry = self
                .entries
                .remove(&orphan)
                .expect("orphan comes from entries");
            self.entries.insert(target, entry);
            moved = true;
        }
        moved
    }
}

/// 读取记录文件，`what` 是提示里的名称（如“收听进度”）。还没有文件时返回 None；
/// 读不了时返回 None 和一句说明；内容不对或 `accept` 不认（比如版本不对）时
/// 把文件改名保留，返回 None 和一句说明。
pub(crate) fn load_json<R: DeserializeOwned>(
    path: &Path,
    what: &str,
    accept: impl FnOnce(&R) -> bool,
) -> (Option<R>, Option<String>) {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, None),
        Err(error) => {
            return (
                None,
                Some(format!("无法读取{what} {}: {error}", path.display())),
            );
        }
    };
    match serde_json::from_str::<R>(&contents) {
        Ok(raw) if accept(&raw) => (Some(raw), None),
        _ => (None, Some(preserve_unreadable(path, what))),
    }
}

pub(crate) fn save_json<R: Serialize>(path: &Path, what: &str, raw: &R) -> io::Result<()> {
    let contents = serde_json::to_string_pretty(raw)
        .map_err(|error| io::Error::other(format!("无法序列化{what}: {error}")))?;
    atomic_write(path, contents.as_bytes())
}

/// 读不了的记录文件改名留在原处，换一个空文件重新开始。
fn preserve_unreadable(path: &Path, what: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let extension = path
        .extension()
        .map(|extension| format!("{}.", extension.to_string_lossy()))
        .unwrap_or_default();
    let backup = path.with_extension(format!("{extension}corrupt-{nanos}"));
    match fs::rename(path, &backup) {
        Ok(()) => format!(
            "{what}文件无法识别，已改名保留为 {}，从空记录开始",
            backup.display()
        ),
        Err(error) => format!("{what}文件无法识别，也无法改名保留: {error}"),
    }
}
