//! 曲库视图：全部曲目（按当前排序）、文件夹筛选、模糊搜索结果，以及按路径查找。
//!
//! 曲目和搜索索引只能一起换，界面看到的下标因此总是同一份曲库的下标。
//! 文件夹筛选在搜索之前：搜索只在筛选范围内进行，曲目下标本身不受筛选影响。

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use crate::search::SearchIndex;
use crate::track::{Folder, SortOrder, Track};

pub struct Catalog {
    tracks: Vec<Track>,
    search: SearchIndex,
    longest_duration: Option<Duration>,
    /// 只看的文件夹；None 表示全部。
    folder: Option<Folder>,
    /// 筛选范围内的曲库下标，按列表顺序；没有筛选时是全部下标。
    scope: Vec<usize>,
    /// 曲库里的每个文件夹及其曲目数，按名称排列，根目录在最后。
    folders: Vec<(Folder, usize)>,
}

impl Catalog {
    pub(super) fn new() -> Self {
        Self {
            tracks: Vec::new(),
            search: SearchIndex::new(),
            longest_duration: None,
            folder: None,
            scope: Vec::new(),
            folders: Vec::new(),
        }
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn get(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.tracks.iter().position(|track| track.path == path)
    }

    /// 当前查询在筛选范围内的结果，按显示顺序排列的曲库下标；
    /// 没有查询词时是筛选范围内的全部曲目。
    pub fn visible(&self) -> &[usize] {
        self.search.results()
    }

    pub fn query(&self) -> &str {
        self.search.query()
    }

    pub fn folder(&self) -> Option<&Folder> {
        self.folder.as_ref()
    }

    pub fn folders(&self) -> &[(Folder, usize)] {
        &self.folders
    }

    /// 顺序播放、循环和随机的范围：筛选的文件夹里的曲库下标，按列表顺序升序排列；
    /// 没有筛选时是整个曲库。搜索只用来定位曲目，不缩小这个范围。
    pub fn scope(&self) -> &[usize] {
        &self.scope
    }

    /// 只看某个文件夹（None 为全部），并在新的范围内重建搜索索引。
    pub(super) fn set_folder(&mut self, folder: Option<Folder>) {
        self.folder = folder;
        self.rebuild_scope();
    }

    /// 最长的曲目时长，界面据此定时长列的宽度。
    pub fn longest_duration(&self) -> Option<Duration> {
        self.longest_duration
    }

    /// 换上新的曲目集合，按 `sort` 排好并重建搜索索引。
    pub(super) fn replace(&mut self, mut tracks: Vec<Track>, sort: SortOrder) {
        tracks.sort_by(|left, right| sort.compare(left, right));
        self.set_unsorted(tracks);
    }

    /// 按 `sort` 重排现有曲目并重建搜索索引。
    pub(super) fn resort(&mut self, sort: SortOrder) {
        let tracks = std::mem::take(&mut self.tracks);
        self.replace(tracks, sort);
    }

    pub(super) fn set_query(&mut self, query: String) {
        self.search.set_query(query);
    }

    /// 推进后台搜索；结果有变化时返回 true。
    pub(super) fn tick_search(&mut self) -> bool {
        self.search.tick()
    }

    pub(super) fn search_running(&self) -> bool {
        self.search.is_running()
    }

    /// 测试用：按给定顺序换上曲目，不排序。
    #[cfg(test)]
    pub(super) fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.set_unsorted(tracks);
    }

    fn set_unsorted(&mut self, tracks: Vec<Track>) {
        self.tracks = tracks;
        self.longest_duration = self.tracks.iter().filter_map(|track| track.duration).max();
        self.folders = count_folders(&self.tracks);
        // 筛选的文件夹在新曲库里已经不存在时回到全部，列表不会莫名其妙地变空。
        if let Some(folder) = &self.folder
            && !self.folders.iter().any(|(known, _)| known == folder)
        {
            self.folder = None;
        }
        self.rebuild_scope();
    }

    fn rebuild_scope(&mut self) {
        self.scope = match &self.folder {
            None => (0..self.tracks.len()).collect(),
            Some(folder) => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| Folder::of(track) == *folder)
                .map(|(index, _)| index)
                .collect(),
        };
        self.search.replace_tracks(&self.tracks, &self.scope);
    }
}

/// 每个文件夹的曲目数。文件夹只从曲目推出来，没有音频的空文件夹不会出现。
fn count_folders(tracks: &[Track]) -> Vec<(Folder, usize)> {
    let mut counts: HashMap<Folder, usize> = HashMap::new();
    for track in tracks {
        *counts.entry(Folder::of(track)).or_default() += 1;
    }
    let mut folders: Vec<_> = counts.into_iter().collect();
    folders.sort_by(|(left, _), (right, _)| match (left, right) {
        (Folder::Root, Folder::Root) => Ordering::Equal,
        (Folder::Root, Folder::Named(_)) => Ordering::Greater,
        (Folder::Named(_), Folder::Root) => Ordering::Less,
        (Folder::Named(left), Folder::Named(right)) => left
            .to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right)),
    });
    folders
}
