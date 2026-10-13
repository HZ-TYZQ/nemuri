use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::prelude::{Color, Rect};
use unicode_width::UnicodeWidthStr;

use crate::player::PlayState;
use crate::theme::{DEFAULT_THEME, Theme};
use crate::track::Track;

use super::ListView;
use super::keymap::{
    self, DELETE_CONFIRM, FOLDERS, HELP, Key, LIBRARY, NAME_INPUT, PLAYLIST_TRACKS, PLAYLISTS,
    QUEUE, SEARCH,
};
use super::library::{
    INACTIVE_ICON, LIST_ICON_WIDTH, PAUSE_ACTION_ICON, PLAY_ACTION_ICON, STOPPED_ICON,
    duration_column_width, playback_action_indicator, track_row_text,
};
use super::overlays::resolve_queue_rows;
use super::text::{ascii_progress_bar, fmt_duration, now_playing_text, truncate_display};
use super::visualizer::{frequency_color, resample_spectrum, spectrum_block, visualizer_height};

#[test]
fn formats_short_and_long_durations() {
    assert_eq!(fmt_duration(Duration::from_secs(65)), "1:05");
    assert_eq!(fmt_duration(Duration::from_secs(3661)), "1:01:01");
}

#[test]
fn visualizer_uses_available_height_without_breaking_base_layout() {
    assert_eq!(visualizer_height(12, true), 0);
    assert_eq!(visualizer_height(13, true), 2);
    assert_eq!(visualizer_height(16, true), 5);
    assert_eq!(visualizer_height(40, true), 5);
    assert_eq!(visualizer_height(40, false), 0);
}

#[test]
fn spectrum_resampling_handles_narrow_wide_and_invalid_input() {
    assert_eq!(resample_spectrum(&[0.1, 0.8, 0.3, 0.6], 2), vec![0.8, 0.6]);
    assert_eq!(
        resample_spectrum(&[0.25, 0.75], 4),
        vec![0.25, 0.25, 0.75, 0.75]
    );
    assert_eq!(resample_spectrum(&[f32::NAN, 2.0], 2), vec![0.0, 1.0]);
    assert!(resample_spectrum(&[], 10).is_empty());
    assert!(resample_spectrum(&[0.5], 0).is_empty());
}

#[test]
fn spectrum_blocks_cover_empty_fractional_and_full_cells() {
    assert_eq!(spectrum_block(0.0), ' ');
    assert_eq!(spectrum_block(0.01), '▁');
    assert_eq!(spectrum_block(0.5), '▄');
    assert_eq!(spectrum_block(1.0), '█');
}

#[test]
fn playback_icon_describes_the_space_key_action() {
    assert_eq!(
        playback_action_indicator(PlayState::Playing, &DEFAULT_THEME).0,
        PAUSE_ACTION_ICON
    );
    assert_eq!(
        playback_action_indicator(PlayState::Paused, &DEFAULT_THEME).0,
        PLAY_ACTION_ICON
    );
    assert_eq!(
        playback_action_indicator(PlayState::Stopped, &DEFAULT_THEME).0,
        STOPPED_ICON
    );
}

#[test]
fn playback_icons_have_fixed_display_width() {
    for state in [PlayState::Playing, PlayState::Paused, PlayState::Stopped] {
        assert_eq!(
            UnicodeWidthStr::width(playback_action_indicator(state, &DEFAULT_THEME).0),
            LIST_ICON_WIDTH
        );
    }
    assert_eq!(UnicodeWidthStr::width(INACTIVE_ICON), LIST_ICON_WIDTH);
}

#[test]
fn ascii_progress_bar_has_fixed_width_and_expected_charset() {
    for width in 0..=40 {
        for ratio in [0.0, 0.25, 0.5, 0.99, 1.0] {
            let bar = ascii_progress_bar(ratio, width);
            assert_eq!(bar.chars().count(), width);
            assert!(bar.chars().all(|cell| matches!(cell, '=' | '>' | '-')));
        }
    }
}

#[test]
fn ascii_progress_bar_renders_head_and_completion() {
    assert_eq!(ascii_progress_bar(0.0, 10), ">---------");
    assert_eq!(ascii_progress_bar(0.5, 10), "=====>----");
    assert_eq!(ascii_progress_bar(0.999, 10), "=========>");
    assert_eq!(ascii_progress_bar(1.0, 10), "==========");
    assert_eq!(ascii_progress_bar(0.5, 1), ">");
    assert_eq!(ascii_progress_bar(1.0, 1), "=");
    assert_eq!(ascii_progress_bar(0.5, 0), "");
}

#[test]
fn truncate_display_limits_by_terminal_width() {
    assert_eq!(truncate_display("hello", 10), "hello");
    assert_eq!(truncate_display("hello world", 8), "hello w…");
    assert_eq!(truncate_display("你好世界", 5), "你好…");
    assert_eq!(truncate_display("ab你好cd", 6), "ab你…");
    assert_eq!(truncate_display("abc", 1), "…");
    assert_eq!(truncate_display("abc", 0), "");
}

#[test]
fn track_rows_fill_the_width_and_right_align_the_duration() {
    let track = Track {
        path: PathBuf::from("/music/歌手/这是一首名字特别特别长的歌曲标题.flac"),
        relative_path: PathBuf::from("歌手/这是一首名字特别特别长的歌曲标题.flac"),
        title: "标签里的标题".to_owned(),
        artist: Some("一个名字同样很长的歌手".to_owned()),
        album: Some("一张名字也非常非常长的专辑".to_owned()),
        duration: Some(Duration::from_secs(3_661)),
        format: Some("FLAC".to_owned()),
        track_number: None,
        disc_number: None,
        file_size: 1,
        modified_ns: 1,
    };
    let duration_width = 7;
    for usable in [22usize, 30, 42, 50, 66, 90] {
        let (title, duration, detail) = track_row_text(&track, usable, duration_width);
        let first =
            UnicodeWidthStr::width(title.as_str()) + UnicodeWidthStr::width(duration.as_str());
        assert_eq!(first + LIST_ICON_WIDTH, usable, "usable={usable}");
        assert_eq!(duration, "1:01:01");
        assert!(UnicodeWidthStr::width(detail.as_str()) + LIST_ICON_WIDTH <= usable);
        assert!(detail.starts_with("歌手"), "详情应以文件夹开头：{detail:?}");
    }

    let (title, _, detail) = track_row_text(&track, 200, duration_width);
    assert!(title.starts_with("这是一首名字特别特别长的歌曲标题 "));
    assert_eq!(
        detail,
        "歌手 · 一个名字同样很长的歌手 · 一张名字也非常非常长的专辑 · FLAC"
    );
}

#[test]
fn track_row_detail_leads_with_the_folder_and_skips_unknown_fields() {
    let mut track = Track {
        path: PathBuf::from("/music/a.mp3"),
        relative_path: PathBuf::from("a.mp3"),
        title: "Song".to_owned(),
        artist: None,
        album: None,
        duration: None,
        format: None,
        track_number: None,
        disc_number: None,
        file_size: 1,
        modified_ns: 1,
    };
    let (_, duration, detail) = track_row_text(&track, 60, 5);
    assert_eq!(duration, "--:--");
    assert_eq!(detail, "(根目录)");

    // 文件夹名原样显示，没有歌手标签时直接省略，不写“未知歌手”。
    track.relative_path = PathBuf::from("ASMR(衣鶴)/合集/音频 - 掏耳.flac");
    track.format = Some("FLAC".to_owned());
    let (_, _, detail) = track_row_text(&track, 60, 5);
    assert_eq!(detail, "ASMR(衣鶴) · FLAC");
    track.artist = Some("衣鹤".to_owned());
    let (_, _, detail) = track_row_text(&track, 60, 5);
    assert_eq!(detail, "ASMR(衣鶴) · 衣鹤 · FLAC");
}

#[test]
fn two_row_items_map_both_rows_to_one_index_and_ignore_the_leftover_row() {
    let mut view = ListView::default();
    // 5 行高的列表放得下两个两行的列表项，最后一行是零头。
    view.record_items(Rect::new(0, 1, 10, 5), 20, 2);
    assert_eq!(view.index_at(1), Some(0));
    assert_eq!(view.index_at(2), Some(0));
    assert_eq!(view.index_at(3), Some(1));
    assert_eq!(view.index_at(4), Some(1));
    assert_eq!(view.index_at(5), None);
}

#[test]
fn now_playing_text_truncates_title_and_artist_as_a_whole() {
    let (title, artist) = now_playing_text("春日影", Some("MyGO!!!!!"), 30);
    assert_eq!(title, "春日影");
    assert_eq!(artist, " — MyGO!!!!!");

    let (title, artist) = now_playing_text("春日影", Some("MyGO!!!!!"), 12);
    assert_eq!(title, "春日影");
    assert_eq!(
        UnicodeWidthStr::width(title.as_str()) + UnicodeWidthStr::width(artist.as_str()),
        12
    );

    let (title, artist) = now_playing_text("这是一首名字特别特别长的歌", Some("X"), 9);
    assert_eq!(artist, "");
    assert!(UnicodeWidthStr::width(title.as_str()) <= 9);

    let (title, artist) = now_playing_text("歌", None, 30);
    assert_eq!(title, "歌");
    assert_eq!(artist, "");
}

#[test]
fn playback_actions_share_primary_color_and_stopped_is_muted() {
    let playing = playback_action_indicator(PlayState::Playing, &DEFAULT_THEME).1;
    let paused = playback_action_indicator(PlayState::Paused, &DEFAULT_THEME).1;
    let stopped = playback_action_indicator(PlayState::Stopped, &DEFAULT_THEME).1;

    assert_eq!(playing, paused);
    assert_eq!(playing.fg, Some(DEFAULT_THEME.primary));
    assert_eq!(stopped.fg, Some(DEFAULT_THEME.muted));
}

#[test]
fn spectrum_gradient_uses_theme_endpoints() {
    assert_eq!(
        frequency_color(0, 32, &DEFAULT_THEME),
        Color::Rgb(168, 168, 168)
    );
    assert_eq!(
        frequency_color(31, 32, &DEFAULT_THEME),
        Color::Rgb(242, 242, 242)
    );
}

#[test]
fn a_spectrum_without_rgb_ends_splits_at_the_middle() {
    let theme = Theme {
        spectrum_low: Color::Gray,
        spectrum_high: Color::Reset,
        ..DEFAULT_THEME
    };
    let colors: Vec<_> = (0..6)
        .map(|index| frequency_color(index, 6, &theme))
        .collect();
    assert_eq!(
        colors,
        [
            Color::Gray,
            Color::Gray,
            Color::Gray,
            Color::Reset,
            Color::Reset,
            Color::Reset
        ]
    );
    // 一端是 RGB 也不能插值。
    let mixed = Theme {
        spectrum_low: Color::Rgb(0, 0, 0),
        ..theme
    };
    assert_eq!(frequency_color(0, 6, &mixed), Color::Rgb(0, 0, 0));
    assert_eq!(frequency_color(5, 6, &mixed), Color::Reset);
}

#[test]
fn queue_rows_resolve_duplicates_and_report_paths_missing_from_the_library() {
    let present = PathBuf::from("/music/a.wav");
    let absent = PathBuf::from("/music/gone.wav");
    let tracks = vec![
        queue_test_track(PathBuf::from("/music/other.wav")),
        queue_test_track(present.clone()),
    ];
    let queue = VecDeque::from(vec![present.clone(), absent, present]);

    assert_eq!(
        resolve_queue_rows(&tracks, &queue),
        vec![Some(1), None, Some(1)]
    );
    assert!(resolve_queue_rows(&tracks, &VecDeque::new()).is_empty());
}

fn queue_test_track(path: PathBuf) -> Track {
    Track {
        relative_path: path.file_name().unwrap().into(),
        path,
        title: "Song".to_owned(),
        artist: None,
        album: None,
        duration: Some(Duration::from_secs(1)),
        format: Some("WAV".to_owned()),
        track_number: None,
        disc_number: None,
        file_size: 1,
        modified_ns: 1,
    }
}

#[test]
fn every_binding_is_reachable_in_its_table() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let tables = [
        ("library", LIBRARY, false),
        ("search", SEARCH, true),
        ("help", HELP, false),
        ("playlists", PLAYLISTS, false),
        ("playlist tracks", PLAYLIST_TRACKS, false),
        ("queue", QUEUE, false),
        ("folders", FOLDERS, false),
        ("name input", NAME_INPUT, true),
        ("delete confirm", DELETE_CONFIRM, false),
    ];
    for (name, table, text_input) in tables {
        for binding in table {
            for (key, action) in binding.map {
                let event = match *key {
                    Key::Any(code) | Key::NoShift(code) => KeyEvent::new(code, KeyModifiers::NONE),
                    Key::Shift(code) => KeyEvent::new(code, KeyModifiers::SHIFT),
                };
                assert_eq!(
                    keymap::lookup(table, text_input, event).as_ref(),
                    Some(action),
                    "{name}: {key:?} 被前面的绑定遮住了"
                );
            }
        }
    }
}

#[test]
fn generated_hints_keep_the_overlay_wording() {
    assert_eq!(
        keymap::hints(PLAYLISTS),
        "c 新建 · a 加入选中歌曲 · Enter 查看 · x 删除 · Esc 关闭"
    );
    assert_eq!(
        keymap::hints(PLAYLIST_TRACKS),
        "Enter 从此处播放 · d 从列表移除 · Esc 返回"
    );
    assert_eq!(
        keymap::hints(QUEUE),
        "Enter 跳到此处播放 · d 移除 · J/K 上下移动 · c 清空 · Esc 关闭"
    );
    assert_eq!(keymap::hints(FOLDERS), "Enter 只看此文件夹 · Esc 关闭");
    assert_eq!(keymap::hints(NAME_INPUT), "Enter 创建 · Esc 取消");
    assert_eq!(keymap::hints(DELETE_CONFIRM), "y 确认 · n/Esc 取消");
    assert_eq!(keymap::hints(HELP), "? / Esc 关闭");
    assert_eq!(keymap::hints(SEARCH), "Enter 播放 · Esc 清除");
    assert!(keymap::library_hints().ends_with("? 帮助 · q 退出"));
}

#[test]
fn help_lines_align_their_descriptions() {
    let lines = keymap::help_lines();
    assert_eq!(lines.len(), 21, "帮助弹层按 21 行设计高度");
    for (line, binding) in lines
        .iter()
        .zip(LIBRARY.iter().filter(|b| !b.keys.is_empty()))
    {
        let prefix = line.strip_suffix(binding.text).unwrap();
        assert_eq!(UnicodeWidthStr::width(prefix), 15, "{line:?}");
    }
}

#[test]
fn the_duration_column_fits_the_longest_track() {
    assert_eq!(duration_column_width(None), 5);
    assert_eq!(duration_column_width(Some(Duration::from_secs(1))), 5);
    assert_eq!(duration_column_width(Some(Duration::from_secs(3_661))), 7);
}
