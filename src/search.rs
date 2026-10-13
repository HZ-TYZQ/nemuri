//! `nucleo` 后台模糊搜索的轻量封装。

use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo, Utf32String};

use crate::track::Track;

pub struct SearchIndex {
    matcher: Nucleo<usize>,
    query: String,
    results: Vec<usize>,
    running: bool,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self {
            matcher: Nucleo::new(Config::DEFAULT, Arc::new(|| ()), None, 1),
            query: String::new(),
            results: Vec::new(),
            running: false,
        }
    }

    /// 换上新的曲目，只索引 `scope` 里的下标（按给定顺序）。
    pub fn replace_tracks(&mut self, tracks: &[Track], scope: &[usize]) {
        self.matcher.restart(true);
        self.results.clear();
        let injector = self.matcher.injector();
        for &index in scope {
            let columns = tracks[index].searchable_columns();
            let searchable = columns.join("  ");
            injector.push(index, move |_, matcher_columns| {
                matcher_columns[0] = Utf32String::from(searchable);
            });
        }
        drop(injector);
        self.reparse(false);
        if self.query.is_empty() {
            self.results = scope.to_vec();
        }
    }

    pub fn set_query(&mut self, query: String) {
        let append = query.starts_with(&self.query);
        self.query = query;
        self.reparse(append);
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn tick(&mut self) -> bool {
        let status = self.matcher.tick(0);
        self.running = status.running;
        if status.changed {
            self.refresh_results();
        }
        status.changed
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    pub fn results(&self) -> &[usize] {
        &self.results
    }

    fn reparse(&mut self, append: bool) {
        self.matcher.pattern.reparse(
            0,
            &self.query,
            CaseMatching::Ignore,
            Normalization::Smart,
            append,
        );
        let status = self.matcher.tick(0);
        self.running = status.running;
        if status.changed {
            self.refresh_results();
        }
    }

    fn refresh_results(&mut self) {
        self.results = self
            .matcher
            .snapshot()
            .matched_items(..)
            .map(|item| *item.data)
            .collect();
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::*;

    fn track(title: &str, artist: &str, album: &str, path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            title: title.to_owned(),
            artist: Some(artist.to_owned()),
            album: Some(album.to_owned()),
            duration: Some(Duration::from_secs(1)),
            format: Some("FLAC".to_owned()),
            track_number: None,
            disc_number: None,
            file_size: 1,
            modified_ns: 1,
        }
    }

    #[test]
    fn searches_all_metadata_fields_case_insensitively() {
        let tracks = vec![
            track("夜曲", "周杰伦", "十一月的萧邦", "jay/01.flac"),
            track("Hello", "Adele", "25", "adele/hello.flac"),
        ];
        let mut search = SearchIndex::new();
        search.replace_tracks(&tracks, &all(&tracks));
        assert_search(&mut search, "ADELE", &[1]);
        assert_search(&mut search, "萧邦", &[0]);
        assert_search(&mut search, "jay/01", &[0]);
    }

    #[test]
    fn tag_titles_stay_searchable_beside_the_file_name() {
        // 界面只显示文件名，标签标题不再出现在界面上，但仍应能搜到。
        let tracks = vec![
            track(
                "studio_video_1761120035627.mp4",
                "某人",
                "",
                "ASMR(Other)/音频 - 涩涩ASMR.mp3",
            ),
            track("Hello", "Adele", "25", "adele/hello.flac"),
        ];
        let mut search = SearchIndex::new();
        search.replace_tracks(&tracks, &all(&tracks));
        assert_search(&mut search, "studio_video", &[0]);
        assert_search(&mut search, "涩涩", &[0]);
    }

    #[test]
    fn replacing_tracks_clears_stale_results_while_query_is_active() {
        let tracks = vec![
            track("Hello", "Adele", "25", "adele/hello.flac"),
            track("夜曲", "周杰伦", "十一月的萧邦", "jay/01.flac"),
        ];
        let mut search = SearchIndex::new();
        search.replace_tracks(&tracks, &all(&tracks));
        assert_search(&mut search, "ADELE", &[0]);

        let replacement = vec![track(
            "Goldberg Variations",
            "Glenn Gould",
            "Bach",
            "gould/goldberg.flac",
        )];
        search.replace_tracks(&replacement, &all(&replacement));
        assert!(search.results().is_empty());
        assert_search(&mut search, "BACH", &[0]);
    }

    fn all(tracks: &[Track]) -> Vec<usize> {
        (0..tracks.len()).collect()
    }

    #[test]
    fn only_tracks_in_scope_are_listed_and_searched() {
        let tracks = vec![
            track("Hello", "Adele", "25", "adele/hello.flac"),
            track("夜曲", "周杰伦", "十一月的萧邦", "jay/01.flac"),
            track("Someone Like You", "Adele", "21", "adele/someone.flac"),
        ];
        let mut search = SearchIndex::new();
        search.replace_tracks(&tracks, &[0, 2]);
        assert_eq!(search.results(), [0, 2]);
        assert_search(&mut search, "someone", &[2]);
        // 范围外的曲目即使匹配也不出现。
        assert_search(&mut search, "萧邦", &[]);
    }

    fn assert_search(search: &mut SearchIndex, query: &str, expected: &[usize]) {
        search.set_query(query.to_owned());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            search.tick();
            if !search.is_running() && search.results() == expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!search.is_running(), "搜索未在截止时间内完成");
        assert_eq!(search.results(), expected);
    }
}
