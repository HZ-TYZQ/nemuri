use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::config::{AppConfig, AppPaths};
use crate::player::fake::{FakeBackend, FakeControl};
use crate::player::{PlayState, PlaybackBackend, Player};
use crate::track::{Folder, RepeatMode, SortKey, Track};

use crate::ui::{MouseInput, ViewLayout};

use super::playback::short_reason;
use super::{Action, App, BagUpdate, Overlay};

/// 装着假后端的 App；临时目录里有一个空的 music 子目录作曲库。
fn test_app(config: AppConfig) -> (tempfile::TempDir, App) {
    let (temp, app, _) = test_app_with_control(config);
    (temp, app)
}

/// 同 `test_app`，并交回假后端的控制句柄，用来注入播完、出错和输出中断。
fn test_app_with_control(config: AppConfig) -> (tempfile::TempDir, App, FakeControl) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("music")).unwrap();
    let (backend, control) = FakeBackend::new();
    let app = app_with_backend(temp.path(), "music", config, Box::new(backend));
    (temp, app, control)
}

/// 在已有的临时目录上再开一个 App，模拟退出后重新启动。
fn app_in(root: &Path, library: &str, config: AppConfig) -> App {
    app_with_backend(root, library, config, Box::new(FakeBackend::new().0))
}

fn app_with_backend(
    root: &Path,
    library: &str,
    config: AppConfig,
    backend: Box<dyn PlaybackBackend>,
) -> App {
    let music = root.join(library);
    let paths = AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        Some(music.clone()),
    );
    App::with_player(backend, music, paths, config, None, false).unwrap()
}

fn track(path: PathBuf) -> Track {
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

/// 按一次键：经按键表翻译成 Action 再分派，与主循环相同。
fn press(app: &mut App, key: KeyEvent) {
    if let Some(action) = crate::ui::key_action(app, key) {
        app.dispatch(action);
    }
}

/// 一次鼠标事件：按上一帧布局翻译成 Action 再分派，与主循环相同。
fn mouse(app: &mut App, input: &mut MouseInput, event: MouseEvent, view: &ViewLayout) {
    for action in input.actions(event, view, app) {
        app.dispatch(action);
    }
}

fn settle_search_and_selection(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        app.catalog.tick_search();
        app.restore_pending_selection();
        if !app.catalog.search_running() && app.view.pending_selected_path.is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("搜索和待恢复选择未在截止时间内完成");
}

#[test]
fn playback_modes_choose_expected_library_index() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog.set_tracks(
        (0..3)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    app.playback.playing_index = Some(2);

    app.config.repeat = RepeatMode::None;
    app.config.shuffle = false;
    assert_eq!(app.next_library_index(true), None);
    app.config.repeat = RepeatMode::All;
    assert_eq!(app.next_library_index(true), Some(0));
    app.config.repeat = RepeatMode::One;
    assert_eq!(app.next_library_index(true), Some(2));
    assert_eq!(app.next_library_index(false), Some(0));
}

#[test]
fn immediate_play_does_not_clear_manual_queue() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let selected = PathBuf::from("/missing/selected.wav");
    let queued = PathBuf::from("/music/queued.wav");
    app.catalog.set_tracks(vec![track(selected)]);
    app.playback.queue.push_back(queued.clone());
    app.play_selected();
    assert_eq!(app.playback.queue.front(), Some(&queued));
}

#[test]
fn sequential_next_skips_a_broken_track_without_polluting_history() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let first = music.join("a.wav");
    let broken = music.join("b.wav");
    let third = music.join("c.wav");
    write_test_wav(&first);
    std::fs::write(&broken, b"not audio").unwrap();
    write_test_wav(&third);
    app.catalog.set_tracks(vec![
        track(first.clone()),
        track(broken),
        track(third.clone()),
    ]);
    app.playback.playing_index = Some(0);

    app.play_next(false);

    assert_eq!(app.playback.playing_index, Some(2));
    let current_path = app
        .playback
        .player
        .current_path()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert_eq!(current_path, third.canonicalize().unwrap());
    assert_eq!(app.playback.history, vec![first]);
}

#[test]
fn rescan_preserves_selected_track_by_path() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let paths: Vec<PathBuf> = (0..4)
        .map(|index| PathBuf::from(format!("/music/{index}.wav")))
        .collect();
    app.catalog
        .set_tracks(paths.iter().map(|path| track(path.clone())).collect());
    app.view.selected = 2;

    // 曲库会按配置排序，扫描顺序不再能制造位移；用少一首曲目让下标真正错位。
    let rescanned: Vec<Track> = [0, 2, 3]
        .iter()
        .map(|index| track(paths[*index].clone()))
        .collect();
    app.apply_scan_finished(rescanned, Vec::new());

    assert_eq!(app.view.selected, 1);
    assert_eq!(
        app.selected_track().map(|track| track.path.clone()),
        Some(paths[2].clone())
    );
}

#[test]
fn rescan_preserves_selected_track_after_active_search_settles() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let paths: Vec<PathBuf> = (0..4)
        .map(|index| PathBuf::from(format!("/music/{index}.wav")))
        .collect();
    app.catalog
        .set_tracks(paths.iter().map(|path| track(path.clone())).collect());
    app.catalog.set_query("Song".to_owned());
    settle_search_and_selection(&mut app);
    assert_eq!(app.visible_indices(), &[0, 1, 2, 3]);
    app.view.selected = 2;

    // 2.wav 重扫后位于可见结果第 1 项，确保错误回退到第 0 项无法通过测试。
    let rescanned: Vec<Track> = [0, 2, 3]
        .iter()
        .map(|index| track(paths[*index].clone()))
        .collect();
    app.apply_scan_finished(rescanned, Vec::new());
    settle_search_and_selection(&mut app);

    assert_eq!(app.view.selected, 1);
    assert_eq!(
        app.selected_track().map(|track| track.path.clone()),
        Some(paths[2].clone())
    );
}

#[test]
fn rescan_falls_back_to_first_match_when_selected_track_stops_matching() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let first_path = PathBuf::from("/music/first.wav");
    let selected_path = PathBuf::from("/music/selected.wav");
    let mut first = track(first_path.clone());
    first.title = "Other".to_owned();
    let mut selected = track(selected_path.clone());
    selected.title = "Favorite".to_owned();
    app.catalog.set_tracks(vec![first, selected]);
    app.catalog.set_query("Favorite".to_owned());
    settle_search_and_selection(&mut app);
    assert_eq!(
        app.selected_track().map(|track| &track.path),
        Some(&selected_path)
    );

    let mut replacement_match = track(first_path.clone());
    replacement_match.title = "Favorite".to_owned();
    let mut replacement_selected = track(selected_path);
    replacement_selected.title = "Other".to_owned();
    app.apply_scan_finished(vec![replacement_match, replacement_selected], Vec::new());
    settle_search_and_selection(&mut app);

    assert_eq!(app.view.selected, 0);
    assert_eq!(
        app.selected_track().map(|track| &track.path),
        Some(&first_path)
    );
}

#[test]
fn user_actions_cancel_pending_selection_restore() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog.set_tracks(
        (0..3)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );

    app.view.pending_selected_path = Some(app.catalog.tracks()[2].path.clone());
    app.select_next();
    assert_eq!(app.view.selected, 1);
    assert!(app.view.pending_selected_path.is_none());

    app.view.pending_selected_path = Some(app.catalog.tracks()[2].path.clone());
    app.view.selected = 2;
    app.select_previous();
    assert_eq!(app.view.selected, 1);
    assert!(app.view.pending_selected_path.is_none());

    for action in [
        Action::InputChar('x'),
        Action::InputBackspace,
        Action::Back,
        Action::Activate,
    ] {
        app.view.search_active = true;
        app.view.pending_selected_path = Some(app.catalog.tracks()[2].path.clone());
        app.dispatch(action);
        assert!(app.view.pending_selected_path.is_none());
    }
}

#[test]
fn rescan_resets_selection_when_selected_track_disappears() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog.set_tracks(
        (0..3)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    app.view.selected = 2;

    app.apply_scan_finished(vec![track(PathBuf::from("/music/new.wav"))], Vec::new());

    assert_eq!(app.view.selected, 0);
    assert_eq!(
        app.selected_track().map(|track| track.path.clone()),
        Some(PathBuf::from("/music/new.wav"))
    );
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn shuffle_does_not_immediately_repeat_current_track() {
    // 默认的播完停止在自然播完时总是返回 None，断言会形同虚设，所以显式用顺序播放。
    let config = AppConfig {
        shuffle: true,
        repeat: RepeatMode::None,
        ..AppConfig::default()
    };
    let (_temp, mut app) = test_app(config);
    app.catalog.set_tracks(
        (0..4)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    app.playback.playing_index = Some(2);
    app.reanchor_shuffle_bag(2);
    for _ in 0..3 {
        assert_ne!(app.next_library_index(true), Some(2));
    }
}

#[test]
fn z_cycles_repeat_only_and_s_toggles_shuffle() {
    let (_temp, mut app) = test_app(AppConfig::default());
    assert_eq!(app.config.repeat, RepeatMode::Stop);
    assert!(!app.config.shuffle);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
    );
    assert_eq!(app.config.repeat, RepeatMode::None);
    assert!(!app.config.shuffle);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    assert_eq!(app.config.repeat, RepeatMode::None);
    assert!(app.config.shuffle);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );
    assert!(!app.config.shuffle);
    assert_eq!(app.config.repeat, RepeatMode::None);
}

#[test]
fn shuffle_bag_exhausts_then_stops_without_repeat() {
    let config = AppConfig {
        shuffle: true,
        repeat: RepeatMode::None,
        ..AppConfig::default()
    };
    let (_temp, mut app) = test_app(config);
    app.catalog.set_tracks(
        (0..3)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    app.playback.playing_index = Some(0);
    app.reanchor_shuffle_bag(0);
    let mut seen = vec![0];
    while let Some(next) = app.next_library_index(true) {
        seen.push(next);
    }
    assert_eq!(seen.len(), 3);
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique, vec![0, 1, 2]);
    assert_eq!(app.next_library_index(true), None);
}

#[test]
fn shuffle_repeat_all_reshuffles_after_bag() {
    let config = AppConfig {
        shuffle: true,
        repeat: RepeatMode::All,
        ..AppConfig::default()
    };
    let (_temp, mut app) = test_app(config);
    app.catalog.set_tracks(
        (0..3)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    app.playback.playing_index = Some(0);
    app.reanchor_shuffle_bag(0);
    for _ in 0..2 {
        assert!(app.next_library_index(true).is_some());
    }
    let first_of_next_round = app.next_library_index(true);
    assert!(first_of_next_round.is_some());
    assert_eq!(app.playback.order.cursor(), 1);
    assert_eq!(app.playback.order.bag().len(), 3);
}

#[test]
fn visualizer_toggle_updates_persisted_setting() {
    let (_temp, mut app) = test_app(AppConfig::default());
    assert!(app.config.visualizer_enabled);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert!(!app.config.visualizer_enabled);
    assert!(app.spectrum_bars().iter().all(|value| *value == 0.0));

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert!(app.config.visualizer_enabled);
}

#[test]
fn visualizer_key_does_not_escape_search_or_overlay_modes() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.view.search_active = true;
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert!(app.config.visualizer_enabled);
    assert_eq!(app.catalog.query(), "v");

    app.view.search_active = false;
    app.view.overlay = Overlay::Help;
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    assert!(app.config.visualizer_enabled);
    assert_eq!(app.view.overlay, Overlay::Help);
}

#[test]
fn the_catalog_tracks_the_longest_duration_across_rescans() {
    let (_temp, mut app) = test_app(AppConfig::default());
    assert_eq!(app.catalog.longest_duration(), None);

    app.apply_scan_finished(vec![track(PathBuf::from("/music/a.wav"))], Vec::new());
    assert_eq!(app.catalog.longest_duration(), Some(Duration::from_secs(1)));

    let mut long = track(PathBuf::from("/music/long.wav"));
    long.duration = Some(Duration::from_secs(3_661));
    app.apply_scan_finished(vec![long], Vec::new());
    assert_eq!(
        app.catalog.longest_duration(),
        Some(Duration::from_secs(3_661))
    );

    app.apply_scan_finished(Vec::new(), Vec::new());
    assert_eq!(app.catalog.longest_duration(), None);
}

#[test]
fn queue_panel_edits_reorder_remove_and_clear_the_queue() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let paths: Vec<PathBuf> = (0..3)
        .map(|index| PathBuf::from(format!("/music/{index}.wav")))
        .collect();
    app.playback.queue = paths.iter().cloned().collect();

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE),
    );
    assert_eq!(app.view.overlay, Overlay::Queue);

    // J 把首项下移一位，选择跟着它走。
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
    );
    assert_eq!(app.view.queue_selected, 1);
    assert_eq!(app.playback.queue[0], paths[1]);
    assert_eq!(app.playback.queue[1], paths[0]);

    // K 移回原位。
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE),
    );
    assert_eq!(app.view.queue_selected, 0);
    assert_eq!(app.playback.queue[0], paths[0]);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert_eq!(app.playback.queue.len(), 2);
    assert_eq!(app.playback.queue[1], paths[2]);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
    );
    assert!(app.playback.queue.is_empty());
    assert_eq!(app.view.queue_selected, 0);

    press(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.view.overlay, Overlay::None);
}

#[test]
fn queue_selection_stays_in_range_when_the_last_entry_is_removed() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.playback.queue = (0..2)
        .map(|index| PathBuf::from(format!("/music/{index}.wav")))
        .collect();
    app.view.overlay = Overlay::Queue;
    app.view.queue_selected = 1;

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );

    assert_eq!(app.playback.queue.len(), 1);
    assert_eq!(app.view.queue_selected, 0);
}

#[test]
fn playing_a_queue_entry_drops_the_entries_before_it() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let skipped = music.join("a.wav");
    let wanted = music.join("b.wav");
    let rest = music.join("c.wav");
    write_test_wav(&skipped);
    write_test_wav(&wanted);
    write_test_wav(&rest);
    app.catalog.set_tracks(vec![
        track(skipped.clone()),
        track(wanted.clone()),
        track(rest.clone()),
    ]);
    app.playback.queue = vec![skipped, wanted.clone(), rest.clone()].into();
    app.view.overlay = Overlay::Queue;
    app.view.queue_selected = 1;

    press(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.playback.playing_index, Some(1));
    let current_path = app
        .playback
        .player
        .current_path()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert_eq!(current_path, wanted.canonicalize().unwrap());
    assert_eq!(app.playback.queue.len(), 1);
    assert_eq!(app.playback.queue.front(), Some(&rest));
}

#[test]
fn queue_keys_do_not_leak_to_the_library_while_the_panel_is_open() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog
        .set_tracks(vec![track(PathBuf::from("/music/a.wav"))]);
    app.view.overlay = Overlay::Queue;

    // c 在曲库里没有绑定，但 q 会退出程序、s 会切随机，都必须被弹层吃掉。
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
    );

    assert!(!app.should_quit);
    assert!(!app.config.shuffle);
    assert_eq!(app.view.overlay, Overlay::Queue);
}

#[test]
fn queue_panel_right_aligns_durations_and_keeps_every_entry_visible() {
    let (_temp, mut app) = test_app(AppConfig::default());
    // 覆盖三种排版分支：缺歌手、标题短到留白多、路径已不在曲库中。
    let mut tracks: Vec<Track> = (0..12)
        .map(|index| {
            let name = if index == 4 {
                "Short".to_owned()
            } else {
                format!("第{index}首很长很长很长很长很长很长的中文歌名 Extra Long Tail")
            };
            let mut item = track(PathBuf::from(format!("/music/{name}.wav")));
            item.artist = Some("某位歌手名字也不短".to_owned());
            item.duration = Some(Duration::from_secs(61 * (index as u64 + 1)));
            item
        })
        .collect();
    tracks[3].artist = None;
    app.catalog.set_tracks(tracks);
    let missing = PathBuf::from("/gone/不在库里的一首歌.flac");
    app.playback.queue = app
        .catalog
        .tracks()
        .iter()
        .map(|track| track.path.clone())
        .chain(std::iter::once(missing))
        .collect();
    app.view.overlay = Overlay::Queue;

    let backend = ratatui::backend::TestBackend::new(100, 22);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    let mut view = crate::ui::ViewLayout::default();
    terminal
        .draw(|frame| crate::ui::draw(frame, &app, &mut view))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered: Vec<String> = (0..22)
        .map(|row| {
            (0..100)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect();

    // 每首曲目的时长都必须落在同一列右端，长中文标题也不例外。
    // 按缓冲区列测量：字节偏移会被中文字符的多字节编码带偏。
    let duration_ends: Vec<u16> = rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("Extra Long Tail") || line.contains("5. Short"))
        .map(|(row, _)| {
            (0..100)
                .rev()
                .find(|column| {
                    let symbol = buffer[(*column, row as u16)].symbol();
                    symbol.len() == 1
                        && symbol.starts_with(|character: char| character.is_ascii_digit())
                })
                .expect("时长列缺失")
        })
        .collect();
    assert_eq!(duration_ends.len(), 12);
    assert!(duration_ends.windows(2).all(|pair| pair[0] == pair[1]));

    // 最后一条和底部提示行必须同时可见，提示行不能盖住任何条目。
    // TestBackend 把宽字符的第二格填成空格，所以只能用 ASCII 片段定位这两行。
    let last_entry = rendered
        .iter()
        .position(|line| line.contains(".flac"))
        .expect("队列末项被遮挡");
    let help = rendered
        .iter()
        .position(|line| line.contains("J/K"))
        .expect("提示行缺失");
    assert!(last_entry < help);
}

#[test]
fn skipping_a_broken_track_leaves_a_notice_after_the_next_track_starts() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let first = music.join("a.wav");
    let broken = music.join("坏文件.wav");
    let good = music.join("c.wav");
    write_test_wav(&first);
    std::fs::write(&broken, b"not audio").unwrap();
    write_test_wav(&good);
    app.catalog
        .set_tracks(vec![track(first), track(broken), track(good)]);
    app.playback.playing_index = Some(0);

    app.play_next(false);

    // 成功切歌会清空 message，跳过原因必须在那之后重新写回来。
    assert_eq!(app.playback.playing_index, Some(2));
    let message = app.view.message.clone().expect("跳过提示丢失");
    assert!(message.contains("已跳过"), "{message}");
    assert!(message.contains("坏文件"), "{message}");
}

#[test]
fn several_skips_in_one_advance_collapse_into_a_counted_notice() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let good = music.join("c.wav");
    write_test_wav(&good);
    let mut broken = Vec::new();
    for name in ["b1.wav", "b2.wav"] {
        let path = music.join(name);
        std::fs::write(&path, b"not audio").unwrap();
        broken.push(path);
    }
    app.catalog.set_tracks(vec![
        track(music.join("start.wav")),
        track(broken[0].clone()),
        track(broken[1].clone()),
        track(good),
    ]);
    app.playback.playing_index = Some(0);

    app.play_next(false);

    assert_eq!(app.playback.playing_index, Some(3));
    let message = app.view.message.clone().expect("跳过提示丢失");
    assert!(message.starts_with("已跳过 2 首"), "{message}");
}

#[test]
fn a_queue_entry_missing_from_the_library_is_named_in_the_notice() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let good = music.join("c.wav");
    write_test_wav(&good);
    app.catalog.set_tracks(vec![track(good)]);
    app.playback.queue = vec![PathBuf::from("/gone/不在库里.flac")].into();
    app.view.overlay = Overlay::Queue;
    // 列表循环让“下一首”与光标是否已经落到曲库上无关，避免依赖搜索线程的时序。
    app.config.repeat = RepeatMode::All;

    press(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.playback.playing_index, Some(0));
    let message = app.view.message.clone().expect("跳过提示丢失");
    assert!(message.contains("不在库里.flac"), "{message}");
    assert!(message.contains("不在曲库中"), "{message}");
}

#[test]
fn playing_a_track_directly_reports_the_failure_instead_of_a_skip() {
    let (temp, mut app) = test_app(AppConfig::default());
    let broken = temp.path().join("music").join("坏文件.wav");
    std::fs::write(&broken, b"not audio").unwrap();
    app.catalog.set_tracks(vec![track(broken)]);

    app.play_selected();

    assert_eq!(app.playback.playing_index, None);
    let message = app.view.message.clone().expect("失败提示丢失");
    assert!(message.starts_with("无法播放“坏文件”"), "{message}");
}

#[test]
fn a_clean_track_change_still_clears_the_previous_message() {
    let (temp, mut app) = test_app(AppConfig::default());
    let good = temp.path().join("music").join("a.wav");
    write_test_wav(&good);
    app.catalog.set_tracks(vec![track(good)]);
    app.view.message = Some("音量 50%".to_owned());

    app.play_selected();

    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.view.message, None);
}

#[test]
fn short_reason_drops_the_path_from_every_player_error_shape() {
    let path = Path::new("/music/曲目 名.mp3");
    let cases = [
        ("音频文件不存在: /music/曲目 名.mp3", "音频文件不存在"),
        ("空文件: /music/曲目 名.mp3", "空文件"),
        ("暂不支持 Opus: /music/曲目 名.mp3", "暂不支持 Opus"),
        (
            "无法识别或解码 /music/曲目 名.mp3: end of stream",
            "无法识别或解码: end of stream",
        ),
        (
            "无法打开 /music/曲目 名.mp3: Permission denied",
            "无法打开: Permission denied",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(short_reason(error, path), expected, "{error}");
    }
}

/// 播放器内部要 canonicalize 才能打开文件，但那个路径不能当成曲目的身份：
/// Windows 上它带 \\?\ 前缀，Linux 上软链接的库根目录会被展开，两种情况都会
/// 让重扫或重排之后按路径找不回正在播放的曲目。这里用软链接复现后者。
#[cfg(unix)]
#[test]
fn a_rescan_keeps_the_playing_track_when_the_library_path_is_not_canonical() {
    let (temp, mut app) = test_app(AppConfig::default());
    let real = temp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    write_test_wav(&real.join("a.wav"));
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let path = link.join("a.wav");
    assert_ne!(path, path.canonicalize().unwrap());
    app.catalog.set_tracks(vec![track(path.clone())]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.playback.player.current_path(), Some(path.as_path()));

    app.replace_tracks(vec![track(path)]);

    assert_eq!(app.playback.playing_index, Some(0));
}

/// 放在子目录里的曲目。界面名称取自文件名，标签标题设成同一个名字，
/// 断言可以直接比较 `title`。
fn nested_track(root: &Path, relative: &str) -> Track {
    let relative = PathBuf::from(relative);
    let mut item = track(root.join(&relative));
    item.title = relative.file_stem().unwrap().to_str().unwrap().to_owned();
    item.relative_path = relative;
    item
}

/// 三首歌的路径顺序（1、2、3）和名称顺序（alpha、mid、zeta）刻意不同，
/// 这样排序切换一定会移动下标，测试不会因为两种顺序恰好一致而失效。
/// 界面名称取自文件名，所以名称写在文件名里，再放进不同的目录打乱路径顺序；
/// 标签标题与文件名相同，断言可以直接比较 `title`。
fn sort_fixture(app: &mut App, music: &Path) -> Vec<PathBuf> {
    let tracks: Vec<Track> = ["1/zeta.wav", "2/alpha.wav", "3/mid.wav"]
        .into_iter()
        .map(|relative| nested_track(music, relative))
        .collect();
    for item in &tracks {
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        write_test_wav(&item.path);
    }
    let paths = tracks.iter().map(|item| item.path.clone()).collect();
    app.catalog.set_tracks(tracks);
    paths
}

#[test]
fn cycling_the_sort_key_reorders_the_library_and_keeps_the_playing_track() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = sort_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.playback.playing_index, Some(0));

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
    );

    assert_eq!(app.config.sort.key, SortKey::Title);
    let order: Vec<&str> = app
        .catalog
        .tracks()
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert_eq!(order, ["alpha", "mid", "zeta"]);
    // 正在播放的是同一个文件，只是它在列表里的位置变了。
    assert_eq!(app.playback.playing_index, Some(2));
    assert_eq!(
        app.playback
            .player
            .current_path()
            .unwrap()
            .canonicalize()
            .unwrap(),
        paths[0].canonicalize().unwrap()
    );
}

#[test]
fn resorting_keeps_the_cursor_on_the_selected_track() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = sort_fixture(&mut app, &temp.path().join("music"));
    settle_search_and_selection(&mut app);
    assert_eq!(app.view.selected, 0);

    // 按标题排序后 zeta 从第一行移到最后一行，光标要跟着它走。
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
    );
    settle_search_and_selection(&mut app);
    assert_eq!(app.view.selected, 2);
    assert_eq!(
        app.selected_track().map(|track| &track.path),
        Some(&paths[0])
    );

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE),
    );
    settle_search_and_selection(&mut app);
    assert_eq!(app.view.selected, 0);
    assert_eq!(
        app.selected_track().map(|track| &track.path),
        Some(&paths[0])
    );
}

#[test]
fn toggling_the_sort_direction_reverses_the_library() {
    let (temp, mut app) = test_app(AppConfig::default());
    sort_fixture(&mut app, &temp.path().join("music"));
    app.config.sort.key = SortKey::Title;

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE),
    );

    assert!(app.config.sort.descending);
    let order: Vec<&str> = app
        .catalog
        .tracks()
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert_eq!(order, ["zeta", "mid", "alpha"]);
    assert!(app.view.message.as_deref().unwrap().contains("标题 ↓"));
}

#[test]
fn resorting_reanchors_the_shuffle_bag_onto_the_new_indices() {
    let (temp, mut app) = test_app(AppConfig::default());
    sort_fixture(&mut app, &temp.path().join("music"));
    app.config.shuffle = true;
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
    );

    // 重排让所有下标失效，随机袋必须重建到新下标上，并从当前曲目开始。
    assert_eq!(
        app.playback.order.bag().first().copied(),
        app.playback.playing_index
    );
    let mut covered = app.playback.order.bag().to_vec();
    covered.sort_unstable();
    assert_eq!(covered, (0..app.catalog.tracks().len()).collect::<Vec<_>>());
}

#[test]
fn a_rescan_applies_the_configured_sort() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.config.sort.key = SortKey::Title;

    app.apply_scan_finished(
        vec![
            nested_track(Path::new("/music"), "1/zeta.wav"),
            nested_track(Path::new("/music"), "2/alpha.wav"),
        ],
        Vec::new(),
    );

    let order: Vec<&str> = app
        .catalog
        .tracks()
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert_eq!(order, ["alpha", "zeta"]);
}

/// 渲染一帧，得到与真实运行一致的布局，之后才能按坐标做命中测试。
fn render(app: &App, width: u16, height: u16) -> (ViewLayout, ratatui::backend::TestBackend) {
    let mut view = ViewLayout::default();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, app, &mut view))
        .unwrap();
    (view, terminal.backend().clone())
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn wheel(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn clicking_a_library_row_selects_it_and_double_click_plays_it() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let paths: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav"]
        .iter()
        .map(|name| music.join(name))
        .collect();
    for path in &paths {
        write_test_wav(path);
    }
    app.catalog
        .set_tracks(paths.iter().map(|path| track(path.clone())).collect());
    settle_search_and_selection(&mut app);

    // 曲库列表从第 1 行开始（第 0 行是边框），每首两行，因此第 5、6 行是第 3 首。
    let (view, _) = render(&app, 90, 24);
    let mut input = MouseInput::default();
    mouse(&mut app, &mut input, click(10, 6), &view);
    assert_eq!(app.view.selected, 2);
    assert_eq!(app.playback.playing_index, None);

    mouse(&mut app, &mut input, click(10, 6), &view);
    assert_eq!(app.playback.playing_index, Some(2));

    // 点标题行同样算这一首。
    mouse(&mut app, &mut input, click(10, 3), &view);
    assert_eq!(app.view.selected, 1);
}

#[test]
fn two_clicks_on_different_rows_are_not_a_double_click() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    for name in ["a.wav", "b.wav"] {
        write_test_wav(&music.join(name));
    }
    app.catalog.set_tracks(
        ["a.wav", "b.wav"]
            .iter()
            .map(|name| track(music.join(name)))
            .collect(),
    );
    settle_search_and_selection(&mut app);

    let (view, _) = render(&app, 90, 24);

    let mut input = MouseInput::default();
    mouse(&mut app, &mut input, click(10, 1), &view);
    mouse(&mut app, &mut input, click(10, 3), &view);

    assert_eq!(app.view.selected, 1);
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn clicks_below_the_last_row_and_outside_the_list_are_ignored() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    write_test_wav(&music.join("a.wav"));
    app.catalog.set_tracks(vec![track(music.join("a.wav"))]);
    settle_search_and_selection(&mut app);

    let (view, _) = render(&app, 90, 24);

    let mut input = MouseInput::default();
    // 列表里只有一首，下面全是空行；点空行不应该改变选中项，也不应该 panic。
    mouse(&mut app, &mut input, click(10, 6), &view);
    mouse(&mut app, &mut input, click(10, 6), &view);
    assert_eq!(app.view.selected, 0);
    assert_eq!(app.playback.playing_index, None);

    // 边框列同样在列表区域之外。
    mouse(&mut app, &mut input, click(0, 1), &view);
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn clicking_the_progress_bar_seeks_within_the_current_track() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = temp.path().join("music").join("long.wav");
    write_test_wav(&long);
    let mut item = track(long.clone());
    item.duration = Some(Duration::from_secs(100));
    app.catalog.set_tracks(vec![item]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    let (view, _) = render(&app, 90, 24);

    let mut input = MouseInput::default();
    let bar = view.progress.expect("进度条没有记录位置");
    mouse(
        &mut app,
        &mut input,
        click(bar.x + bar.width / 2, bar.y),
        &view,
    );

    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(40) && position <= Duration::from_secs(60),
        "点击进度条中点后位置是 {position:?}"
    );
}

#[test]
fn digit_keys_jump_to_tenths_of_the_track() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = temp.path().join("music").join("long.wav");
    write_test_wav(&long);
    let mut item = track(long.clone());
    item.duration = Some(Duration::from_secs(100));
    app.catalog.set_tracks(vec![item]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE),
    );
    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(45) && position <= Duration::from_secs(55),
        "按 5 后位置是 {position:?}"
    );

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
    );
    assert!(app.playback.player.position() < Duration::from_secs(5));
}

#[test]
fn long_seek_moves_a_minute_with_shift_arrows_or_uppercase_keys() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = temp.path().join("music").join("long.wav");
    write_test_wav(&long);
    let mut item = track(long.clone());
    item.duration = Some(Duration::from_secs(100));
    app.catalog.set_tracks(vec![item]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    press(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(60) && position <= Duration::from_secs(65),
        "Shift+→ 后位置是 {position:?}"
    );

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT),
    );
    assert!(app.playback.player.position() < Duration::from_secs(5));

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT),
    );
    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(60) && position <= Duration::from_secs(65),
        "L 后位置是 {position:?}"
    );

    // 普通方向键仍是 10 秒。
    press(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(50) && position <= Duration::from_secs(55),
        "← 后位置是 {position:?}"
    );
}

#[test]
fn bracket_keys_adjust_volume_by_one_percent() {
    let (_temp, mut app) = test_app(AppConfig {
        volume: 50,
        ..AppConfig::default()
    });

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
    );
    assert_eq!(app.playback.player.volume(), 51);
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
    );
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
    );
    assert_eq!(app.playback.player.volume(), 49);
    assert_eq!(app.config.volume, 49);
}

#[test]
fn the_wheel_moves_the_selection_inside_the_hovered_list() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog.set_tracks(
        (0..20)
            .map(|index| track(PathBuf::from(format!("/music/{index:02}.wav"))))
            .collect(),
    );
    settle_search_and_selection(&mut app);

    let (view, _) = render(&app, 90, 24);

    let mut input = MouseInput::default();
    mouse(
        &mut app,
        &mut input,
        wheel(MouseEventKind::ScrollDown, 10, 3),
        &view,
    );
    assert_eq!(app.view.selected, 3);
    mouse(
        &mut app,
        &mut input,
        wheel(MouseEventKind::ScrollUp, 10, 3),
        &view,
    );
    assert_eq!(app.view.selected, 0);

    // 列表之外的滚动不应该动选中项。
    mouse(
        &mut app,
        &mut input,
        wheel(MouseEventKind::ScrollDown, 10, 23),
        &view,
    );
    assert_eq!(app.view.selected, 0);
}

#[test]
fn overlay_clicks_go_to_the_overlay_list_not_the_library() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.catalog.set_tracks(
        (0..5)
            .map(|index| track(PathBuf::from(format!("/music/{index}.wav"))))
            .collect(),
    );
    settle_search_and_selection(&mut app);
    app.playback.queue = app
        .catalog
        .tracks()
        .iter()
        .map(|track| track.path.clone())
        .collect();
    app.view.overlay = Overlay::Queue;

    let (view, backend) = render(&app, 90, 24);

    let mut input = MouseInput::default();
    // 找到队列面板第 3 行的屏幕坐标，点它。
    let row = (0..24)
        .find(|row| {
            (0..90)
                .map(|column| backend.buffer()[(column, *row)].symbol())
                .collect::<String>()
                .contains(" 3. ")
        })
        .expect("队列第 3 行没有渲染出来");
    let queue_area_column = view
        .overlay
        .index_at(row)
        .map(|_| 45)
        .expect("该行不在队列列表内");

    mouse(&mut app, &mut input, click(queue_area_column, row), &view);

    assert_eq!(app.view.queue_selected, 2);
    // 曲库的选中项不能被弹层上的点击带偏。
    assert_eq!(app.view.selected, 0);
}

fn write_test_wav(path: &Path) {
    write_wav(path, 8_000, 800);
}

fn write_wav(path: &Path, sample_rate: u32, sample_count: usize) {
    let data_len = (sample_count * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    std::fs::write(path, wav).unwrap();
}

/// 播放时长记为 100 秒的测试曲并停在 `at`，返回曲目路径。
/// 假后端不解码，时长取自曲目信息，文件本身用短 WAV 就够了。
fn play_long_track_at(temp: &Path, app: &mut App, at: Duration) -> PathBuf {
    let long = temp.join("music").join("long.wav");
    write_test_wav(&long);
    let mut item = track(long.clone());
    item.duration = Some(Duration::from_secs(100));
    app.catalog.set_tracks(vec![item]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(at));
    long
}

fn long_track(path: &Path) -> Track {
    let mut item = track(path.to_path_buf());
    item.duration = Some(Duration::from_secs(100));
    item
}

#[test]
fn restart_resumes_the_last_track_paused_at_its_position() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(40));
    app.save_session().unwrap();
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    let other = temp.path().join("music").join("other.wav");
    app.apply_scan_finished(vec![track(other), long_track(&long)], Vec::new());
    settle_search_and_selection(&mut app);

    assert_eq!(app.playback.player.state(), PlayState::Paused);
    assert_eq!(app.current_track().map(|t| t.path.clone()), Some(long));
    assert_eq!(app.selected_track_index(), app.playback.playing_index);
    let position = app.playback.player.position();
    assert!(
        position >= Duration::from_secs(39) && position <= Duration::from_secs(41),
        "恢复后位置是 {position:?}"
    );
    assert!(
        app.view
            .message
            .as_deref()
            .unwrap()
            .contains("已恢复上次播放")
    );

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    assert_eq!(app.playback.player.state(), PlayState::Playing);
}

#[test]
fn a_finished_track_resumes_from_the_start() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(99));
    app.save_session().unwrap();
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    assert_eq!(app.playback.player.state(), PlayState::Paused);
    assert!(app.playback.player.position() < Duration::from_secs(1));
}

#[test]
fn a_session_from_another_library_is_not_restored() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(40));
    app.save_session().unwrap();
    drop(app);

    std::fs::create_dir(temp.path().join("elsewhere")).unwrap();
    let mut app = app_in(temp.path(), "elsewhere", AppConfig::default());
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    assert_eq!(app.playback.player.state(), PlayState::Stopped);
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn quitting_before_the_first_scan_keeps_the_saved_session() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(40));
    app.save_session().unwrap();
    drop(app);

    // 第一次扫描还没完成就退出：会话必须原样留给下一次。
    let app = app_in(temp.path(), "music", AppConfig::default());
    app.save_session().unwrap();
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    assert_eq!(app.playback.player.state(), PlayState::Paused);
}

#[test]
fn quitting_with_nothing_loaded_clears_the_session() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(40));
    app.save_session().unwrap();
    drop(app);

    // 曲目已从曲库消失：恢复落空，退出时什么都没播，旧会话应被清掉。
    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(Vec::new(), Vec::new());
    app.save_session().unwrap();
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    assert_eq!(app.playback.player.state(), PlayState::Stopped);
}

#[test]
fn the_configured_theme_colors_the_screen() {
    use crate::theme::Theme;
    use ratatui::style::Color;

    let config = AppConfig {
        theme: Theme {
            border: Color::Rgb(0x12, 0x34, 0x56),
            ..Theme::default()
        },
        ..AppConfig::default()
    };
    let (_temp, app) = test_app(config);
    let (_, backend) = render(&app, 100, 24);
    // 左上角是曲库的边框。
    assert_eq!(backend.buffer()[(0, 0)].fg, Color::Rgb(0x12, 0x34, 0x56));
}

/// 按 `config.kdl` 的内容启动一个能写回配置的 App。
fn app_from_config_kdl(root: &Path, text: &str) -> App {
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    let paths = AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        Some(music.clone()),
    );
    crate::config::atomic_write(&paths.config_file, text.as_bytes()).unwrap();
    let loaded = AppConfig::load(&paths).unwrap();
    let backend = Box::new(FakeBackend::new().0);
    App::with_player(
        backend,
        music,
        paths,
        loaded.config,
        loaded.message,
        loaded.writable,
    )
    .unwrap()
}

#[test]
fn settings_changed_while_running_are_written_back_into_config_kdl() {
    let temp = tempfile::tempdir().unwrap();
    let text = "// 我的播放器
playback {
    volume 60 // 夜里小声点
    shuffle #false
}

theme {
    preset \"light\"
}
";
    let mut app = app_from_config_kdl(temp.path(), text);
    assert_eq!(app.view().message, None);

    app.dispatch(Action::ChangeVolume(-5));
    app.dispatch(Action::ToggleShuffle);
    app.dispatch(Action::ToggleLyrics);
    app.save_settings().unwrap();

    let saved =
        std::fs::read_to_string(temp.path().join("config/tui-music-player/config.kdl")).unwrap();
    assert_eq!(
        saved,
        "// 我的播放器\nplayback {\n    volume 55 // 夜里小声点\n    shuffle #true\n}\n\ntheme {\n    preset \"light\"\n}\n\ninterface {\n    lyrics #false\n}\n"
    );
}

#[test]
fn a_config_kdl_with_a_syntax_error_is_never_rewritten() {
    let temp = tempfile::tempdir().unwrap();
    let text = "playback {\n    volume 60\n";
    let mut app = app_from_config_kdl(temp.path(), text);
    let message = app.view().message.clone().unwrap();
    assert!(message.contains("有语法错误"), "{message}");

    app.dispatch(Action::ToggleShuffle);
    app.save_settings().unwrap();
    let saved =
        std::fs::read_to_string(temp.path().join("config/tui-music-player/config.kdl")).unwrap();
    assert_eq!(saved, text);
}

/// 每行文字去掉空格：宽字符后面的占位格也是空格，按原样拼接无法直接比对中文。
fn screen_rows(backend: &ratatui::backend::TestBackend) -> Vec<String> {
    let buffer = backend.buffer();
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .filter(|symbol| *symbol != " ")
                .collect()
        })
        .collect()
}

#[test]
fn now_playing_names_the_folder_when_the_artist_tag_is_missing() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let mut tracks = vec![
        nested_track(&music, "ASMR(月紫)/音频 - 睡眠誘導.wav"),
        nested_track(&music, "ASMR(衣鶴)/音频 - 掏耳合集.wav"),
    ];
    tracks[1].artist = Some("衣鹤".to_owned());
    for item in &tracks {
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        write_test_wav(&item.path);
    }
    app.catalog.set_tracks(tracks);

    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    let (_, backend) = render(&app, 100, 24);
    let rows = screen_rows(&backend);
    assert!(
        rows.iter()
            .any(|row| row.contains("音频-睡眠誘導—ASMR(月紫)"))
    );

    // 有歌手标签时照旧显示歌手。
    app.play_index(1, false, BagUpdate::Reanchor).unwrap();
    let (_, backend) = render(&app, 100, 24);
    let rows = screen_rows(&backend);
    assert!(rows.iter().any(|row| row.contains("音频-掏耳合集—衣鹤")));
}

/// 播放带同名 .lrc 的 100 秒测试曲，停在 35 秒（第二句）。
fn play_track_with_lyrics(temp: &Path, app: &mut App) {
    std::fs::write(
        temp.join("music").join("long.lrc"),
        "[ti:测试]\n[00:00.00]第一句\n[00:30.00]第二句\n[00:30.00]第二句译文\n[00:50.00]第三句\n",
    )
    .unwrap();
    play_long_track_at(temp, app, Duration::from_secs(35));
    app.sync_lyrics();
}

#[test]
fn lyrics_pane_follows_the_position_beside_the_library() {
    let (temp, mut app) = test_app(AppConfig::default());
    play_track_with_lyrics(temp.path(), &mut app);
    assert_eq!(app.lyrics().map(|lyrics| lyrics.lines().len()), Some(4));

    let (view, backend) = render(&app, 120, 30);
    let rows = screen_rows(&backend);
    assert!(rows[0].contains("歌词"), "宽终端应显示歌词面板");
    assert!(rows.iter().any(|row| row.contains("第二句译文")));
    assert!(rows.iter().any(|row| row.contains("第三句")));
    // 曲库让出右侧，点击面板区域不会落到曲库。
    assert!(!view.library.contains(110, 5));

    let highlighted = |text: &str| {
        let buffer = backend.buffer();
        let row = rows.iter().position(|row| row.contains(text)).unwrap();
        let column = (0..buffer.area.width)
            .rev()
            .find(|column| {
                buffer[(*column, row as u16)].symbol() == text.chars().next().unwrap().to_string()
            })
            .unwrap();
        buffer[(column, row as u16)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    };
    assert!(highlighted("第二句"));
    assert!(!highlighted("第三句"));
}

#[test]
fn narrow_terminals_show_the_current_lyric_on_the_now_playing_border() {
    let (temp, mut app) = test_app(AppConfig::default());
    play_track_with_lyrics(temp.path(), &mut app);

    let (_, backend) = render(&app, 80, 24);
    let rows = screen_rows(&backend);
    assert!(!rows[0].contains("歌词"));
    let border = rows.iter().rev().nth(1).unwrap();
    assert!(border.contains("第二句/第二句译文"), "底边是 {border:?}");
    assert!(!rows.iter().any(|row| row.contains("第三句")));
}

#[test]
fn the_lyrics_key_hides_lyrics_and_the_choice_is_saved() {
    let (temp, mut app) = test_app(AppConfig::default());
    play_track_with_lyrics(temp.path(), &mut app);

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(!app.config.lyrics_enabled);
    assert!(app.lyrics().is_none());
    let (view, backend) = render(&app, 120, 30);
    let rows = screen_rows(&backend);
    assert!(!rows[0].contains("歌词"), "关闭歌词后不留面板");
    assert!(!rows.iter().any(|row| row.contains("第二句")));
    assert!(view.library.contains(110, 5), "曲库占满整行");

    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    assert!(app.lyrics().is_some());
    assert_eq!(app.view.message.as_deref(), Some("已开启歌词"));
}

#[test]
fn plain_lyrics_fill_the_pane_and_scroll_with_the_progress() {
    let (temp, mut app) = test_app(AppConfig::default());
    let text: String = (1..=40).map(|line| format!("L{line:02}\n")).collect();
    std::fs::write(temp.path().join("music").join("long.lrc"), text).unwrap();
    play_long_track_at(temp.path(), &mut app, Duration::from_secs(50));
    app.sync_lyrics();
    assert!(app.lyrics().is_some_and(|lyrics| !lyrics.is_synced()));

    let (_, backend) = render(&app, 120, 30);
    let rows = screen_rows(&backend);
    assert!(rows[0].contains("歌词·纯文本"), "标题是 {:?}", rows[0]);
    // 40 行放不下：播放到一半时开头和结尾都滚出去了，中间那段可见。
    let visible = |label: &str| rows.iter().any(|row| row.contains(label));
    assert!(!visible("L01") && !visible("L40"));
    assert!(visible("L20"));
    let bold = (0..backend.buffer().area.width).any(|column| {
        (1..29).any(|row| {
            let cell = &backend.buffer()[(column, row)];
            cell.symbol() == "L" && cell.modifier.contains(ratatui::style::Modifier::BOLD)
        })
    });
    assert!(!bold, "纯文本没有当前行，不应加粗");

    // 窄终端的播放区边框只显示同步歌词的当前行。
    let (_, backend) = render(&app, 80, 24);
    assert!(!screen_rows(&backend).iter().any(|row| row.contains("L2")));
}

#[test]
fn the_lyrics_pane_stays_beside_the_library_without_lyrics() {
    let (temp, mut app) = test_app(AppConfig::default());
    // 还没播放时面板就在，播放区和面板各显示一次“未在播放”。
    let (view, backend) = render(&app, 120, 30);
    let rows = screen_rows(&backend);
    assert!(rows[0].contains("歌词"));
    assert_eq!(
        rows.iter().filter(|row| row.contains("未在播放")).count(),
        2
    );
    assert!(!view.library.contains(110, 5));

    play_long_track_at(temp.path(), &mut app, Duration::from_secs(35));
    app.sync_lyrics();
    assert!(app.lyrics().is_none());

    // 切到没有歌词的曲目，曲库宽度不变，面板里给出提示。
    let (view, backend) = render(&app, 120, 30);
    let rows = screen_rows(&backend);
    assert!(rows[0].contains("歌词"));
    assert!(rows.iter().any(|row| row.contains("暂无歌词")));
    assert!(!view.library.contains(110, 5));

    // 重扫之后才放进来的 .lrc 会被读到。
    std::fs::write(
        temp.path().join("music").join("long.lrc"),
        "[00:01.00]新歌词\n",
    )
    .unwrap();
    app.apply_scan_finished(app.catalog.tracks().to_vec(), Vec::new());
    app.sync_lyrics();
    assert!(app.lyrics().is_some());
}

#[test]
fn a_natural_end_moves_on_and_repeat_one_replays_the_track() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::None,
        ..AppConfig::default()
    });
    let music = temp.path().join("music");
    let paths: Vec<PathBuf> = ["a.wav", "b.wav"]
        .iter()
        .map(|name| music.join(name))
        .collect();
    for path in &paths {
        write_test_wav(path);
    }
    app.catalog
        .set_tracks(paths.iter().map(|path| track(path.clone())).collect());
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(app.player().state(), PlayState::Playing);

    app.config.repeat = RepeatMode::One;
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(app.player().current_path(), Some(paths[1].as_path()));
    assert_eq!(app.playback.history, vec![paths[0].clone()]);
}

#[test]
fn a_decode_error_mid_track_skips_ahead_and_says_why() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::One,
        ..AppConfig::default()
    });
    let music = temp.path().join("music");
    let paths: Vec<PathBuf> = ["第一首.wav", "b.wav"]
        .iter()
        .map(|name| music.join(name))
        .collect();
    for path in &paths {
        write_test_wav(path);
    }
    app.catalog
        .set_tracks(vec![track(paths[0].clone()), track(paths[1].clone())]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    // 单曲循环也不应重播出错的曲目。
    control.fail_track("帧校验失败");
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(1));
    let message = app.view.message.clone().expect("跳过提示丢失");
    assert!(message.contains("已跳过“第一首”"), "{message}");
    assert!(message.contains("帧校验失败"), "{message}");
}

#[test]
fn an_output_failure_holds_the_track_until_space_recovers_it() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let music = temp.path().join("music");
    let paths: Vec<PathBuf> = ["a.wav", "b.wav"]
        .iter()
        .map(|name| music.join(name))
        .collect();
    for path in &paths {
        write_test_wav(path);
    }
    app.catalog
        .set_tracks(paths.iter().map(|path| track(path.clone())).collect());
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    control.break_output("设备已移除");
    app.drain_player_events();
    assert!(
        app.view
            .message
            .as_deref()
            .unwrap()
            .contains("音频输出中断")
    );

    // 输出中断期间不切歌，曲目和进度都保留。
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(0));
    assert!(app.view.message.as_deref().unwrap().contains("按空格重试"));

    app.dispatch(Action::TogglePause);
    assert_eq!(app.view.message.as_deref(), Some("正在恢复音频输出…"));
    app.drain_player_events();
    assert_eq!(app.view.message.as_deref(), Some("音频输出已恢复"));
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(app.playback.playing_index, Some(0));
}

/// App 的其余测试都用假后端；这一条确认真实的 Rodio 后端也能经 App 播放和暂停。
#[test]
fn the_rodio_backend_plays_and_pauses_through_app() {
    let temp = tempfile::tempdir().unwrap();
    let music = temp.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let path = music.join("a.wav");
    write_test_wav(&path);
    let player = Player::new_for_tests().unwrap();
    let mut app = app_with_backend(temp.path(), "music", AppConfig::default(), Box::new(player));
    app.catalog.set_tracks(vec![track(path.clone())]);

    app.play_selected();
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(app.player().current_path(), Some(path.as_path()));

    app.dispatch(Action::TogglePause);
    assert_eq!(app.player().state(), PlayState::Paused);
}

/// 两个文件夹加一首放在根目录的歌：a 和 b 各两首。文件都写出来，播放也能用。
fn folder_fixture(app: &mut App, music: &Path) -> Vec<PathBuf> {
    let tracks: Vec<Track> = ["a/1.wav", "a/2.wav", "b/3.wav", "b/4.wav", "5.wav"]
        .into_iter()
        .map(|relative| nested_track(music, relative))
        .collect();
    for item in &tracks {
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        write_test_wav(&item.path);
    }
    let paths = tracks.iter().map(|item| item.path.clone()).collect();
    app.catalog.set_tracks(tracks);
    paths
}

fn named(name: &str) -> Folder {
    Folder::Named(name.to_owned())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 经文件夹面板选中第 `row` 行（第 0 行是“全部”）。
fn choose_folder(app: &mut App, row: usize) {
    press(app, key(KeyCode::Char('f')));
    assert_eq!(app.view.overlay, Overlay::Folders);
    app.dispatch(Action::SelectRow(row));
    press(app, key(KeyCode::Enter));
    settle_search_and_selection(app);
}

#[test]
fn the_folder_panel_lists_every_folder_with_its_count() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));

    press(&mut app, key(KeyCode::Char('f')));

    assert_eq!(app.view.overlay, Overlay::Folders);
    assert_eq!(app.view.folder_selected, 0);
    // 根目录排在最后。
    assert_eq!(
        app.catalog.folders(),
        [(named("a"), 2), (named("b"), 2), (Folder::Root, 1)]
    );
    let (_, backend) = render(&app, 80, 24);
    let rows = screen_rows(&backend);
    for expected in [
        "●全部5首",
        "a2首",
        "b2首",
        "(根目录)1首",
        "Enter只看此文件夹",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(expected)),
            "缺少 {expected}"
        );
    }
}

#[test]
fn choosing_a_folder_filters_the_library_and_search_stays_inside_it() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));

    choose_folder(&mut app, 1);

    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.catalog.folder(), Some(&named("a")));
    assert_eq!(app.visible_indices(), [0, 1]);
    assert_eq!(app.view.message.as_deref(), Some("文件夹：a（2 首）"));

    // 整个曲库里搜“3”能找到 b/3，筛选后找不到。
    press(&mut app, key(KeyCode::Char('/')));
    press(&mut app, key(KeyCode::Char('3')));
    settle_search_and_selection(&mut app);
    assert!(app.visible_indices().is_empty());
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Char('2')));
    settle_search_and_selection(&mut app);
    assert_eq!(app.visible_indices(), [1]);

    press(&mut app, key(KeyCode::Esc));
    settle_search_and_selection(&mut app);
    assert_eq!(app.visible_indices(), [0, 1]);
}

#[test]
fn choosing_all_clears_the_filter() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    choose_folder(&mut app, 2);
    assert_eq!(app.visible_indices(), [2, 3]);

    // 再次打开时光标停在当前的筛选上。
    press(&mut app, key(KeyCode::Char('f')));
    assert_eq!(app.view.folder_selected, 2);
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.catalog.folder(), Some(&named("b")));

    choose_folder(&mut app, 0);
    assert_eq!(app.catalog.folder(), None);
    assert_eq!(app.visible_indices(), [0, 1, 2, 3, 4]);
    assert_eq!(app.view.message.as_deref(), Some("显示全部 5 首"));
}

#[test]
fn the_cursor_follows_the_selected_or_playing_track_into_the_folder() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    app.view.selected = 1;

    // 选中的 a/2 在 a 里，光标留在它上面。
    choose_folder(&mut app, 1);
    assert_eq!(app.selected_track_index(), Some(1));

    // 选中的 a/2 不在 b 里，改落到正在播放的 b/4。
    app.play_index(3, false, BagUpdate::Reanchor).unwrap();
    choose_folder(&mut app, 2);
    assert_eq!(app.selected_track_index(), Some(3));

    // 两者都不在根目录里，回到第一行。
    choose_folder(&mut app, 3);
    assert_eq!(app.view.selected, 0);
    assert_eq!(app.selected_track_index(), Some(4));
}

#[test]
fn a_rescan_keeps_the_folder_filter_until_the_folder_is_gone() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    choose_folder(&mut app, 2);

    let tracks = app.catalog.tracks().to_vec();
    app.apply_scan_finished(tracks.clone(), Vec::new());
    settle_search_and_selection(&mut app);
    assert_eq!(app.catalog.folder(), Some(&named("b")));
    // 重扫按路径重排，根目录的 5.wav 排到了最前面。
    let shown: Vec<&str> = app
        .visible_indices()
        .iter()
        .map(|index| app.catalog.tracks()[*index].display_title())
        .collect();
    assert_eq!(shown, ["3", "4"]);

    let without_b = tracks
        .into_iter()
        .filter(|track| Folder::of(track) != named("b"))
        .collect();
    app.apply_scan_finished(without_b, Vec::new());
    settle_search_and_selection(&mut app);
    assert_eq!(app.catalog.folder(), None);
    assert_eq!(app.visible_indices(), [0, 1, 2]);
    assert_eq!(
        app.view.message.as_deref(),
        Some("扫描完成，共 3 首歌曲；文件夹“b”已不存在，改为显示全部")
    );
}

#[test]
fn double_clicking_a_folder_row_applies_it() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    press(&mut app, key(KeyCode::Char('f')));

    let (view, backend) = render(&app, 80, 24);
    let row = screen_rows(&backend)
        .iter()
        .position(|row| row.contains("b2首"))
        .unwrap() as u16;
    let mut input = MouseInput::default();
    mouse(&mut app, &mut input, click(40, row), &view);
    assert_eq!(app.view.folder_selected, 2);
    mouse(&mut app, &mut input, click(40, row), &view);

    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.catalog.folder(), Some(&named("b")));
}

#[test]
fn the_library_title_names_the_folder_and_counts_it_against_the_whole_library() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    choose_folder(&mut app, 1);

    let (_, backend) = render(&app, 100, 24);
    let rows = screen_rows(&backend);
    assert!(rows[0].contains("文件夹a·2/5首"), "{}", rows[0]);
}

#[test]
fn next_and_list_loop_stay_inside_the_folder() {
    let (temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::None,
        ..AppConfig::default()
    });
    folder_fixture(&mut app, &temp.path().join("music"));
    choose_folder(&mut app, 1);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
    // 顺序播放到文件夹末尾就停，不会走进 b。
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, None);
    assert_eq!(app.player().state(), PlayState::Stopped);

    app.config.repeat = RepeatMode::All;
    app.play_index(1, false, BagUpdate::Reanchor).unwrap();
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(0));
}

#[test]
fn a_track_outside_the_folder_continues_at_the_folders_first_track() {
    let (temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::None,
        ..AppConfig::default()
    });
    folder_fixture(&mut app, &temp.path().join("music"));
    app.play_index(4, false, BagUpdate::Reanchor).unwrap();

    // 筛选不打断正在播放的曲目。
    choose_folder(&mut app, 2);
    assert_eq!(app.playback.playing_index, Some(4));
    assert_eq!(app.player().state(), PlayState::Playing);

    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(2));
}

#[test]
fn shuffle_reshuffles_inside_the_folder_when_it_changes() {
    let (temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::All,
        shuffle: true,
        ..AppConfig::default()
    });
    folder_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.playback.order.bag().len(), 5);

    choose_folder(&mut app, 2);
    let mut bag = app.playback.order.bag().to_vec();
    bag.sort_unstable();
    assert_eq!(bag, [2, 3]);

    for _ in 0..6 {
        app.dispatch(Action::Next);
        let playing = app.playback.playing_index.unwrap();
        assert!([2, 3].contains(&playing), "跑出了文件夹：{playing}");
    }
    assert!(app.media_snapshot().can_go_next);
}

#[test]
fn stop_mode_ends_playback_after_a_natural_end_and_says_why() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::Stop,
        ..AppConfig::default()
    });
    folder_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();

    control.finish_track();
    app.drain_player_events();

    assert_eq!(app.playback.playing_index, None);
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已播完，播放停止（按 z 切换循环方式）")
    );

    // 手动切歌照常往后走。
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
}

#[test]
fn stop_mode_still_plays_the_queue_after_a_natural_end() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::Stop,
        ..AppConfig::default()
    });
    let paths = folder_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    app.playback.queue.push_back(paths[3].clone());

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(3));

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn z_cycles_through_all_four_repeat_modes() {
    let (_temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::Stop,
        ..AppConfig::default()
    });
    let mut seen = Vec::new();
    for _ in 0..4 {
        press(&mut app, key(KeyCode::Char('z')));
        seen.push(app.view.message.clone().unwrap());
    }
    assert_eq!(
        seen,
        [
            "循环：顺序",
            "循环：列表循环",
            "循环：单曲循环",
            "循环：播完停止"
        ]
    );
    assert_eq!(app.config.repeat, RepeatMode::Stop);
}

#[test]
fn media_controls_turning_loop_off_keep_or_choose_stop() {
    let (_temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::Stop,
        ..AppConfig::default()
    });
    // 系统媒体控制分不出播完停止和顺序播放，“不循环”不能把其中一种改成另一种。
    app.dispatch(Action::SetRepeat(RepeatMode::None));
    assert_eq!(app.config.repeat, RepeatMode::Stop);
    app.config.repeat = RepeatMode::None;
    app.dispatch(Action::SetRepeat(RepeatMode::None));
    assert_eq!(app.config.repeat, RepeatMode::None);

    app.dispatch(Action::SetRepeat(RepeatMode::All));
    assert_eq!(app.config.repeat, RepeatMode::All);
    app.dispatch(Action::SetRepeat(RepeatMode::None));
    assert_eq!(app.config.repeat, RepeatMode::Stop);
}

const MINUTE: Duration = Duration::from_secs(60);

/// 正在播放、停在 0.5 秒处的一首歌，音量 80。返回定时开始的时间。
fn sleep_fixture(temp: &Path, app: &mut App) -> Instant {
    folder_fixture(app, &temp.join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_millis(500)));
    let start = app.now;
    press(app, key(KeyCode::Char('t')));
    start
}

fn sleep_config() -> AppConfig {
    AppConfig {
        volume: 80,
        ..AppConfig::default()
    }
}

#[test]
fn t_cycles_through_the_sleep_timer_presets() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let mut seen = Vec::new();
    for _ in 0..7 {
        press(&mut app, key(KeyCode::Char('t')));
        seen.push(app.view.message.clone().unwrap());
    }
    assert_eq!(
        seen,
        [
            "睡眠定时：15 分钟后淡出并暂停（t 切换）",
            "睡眠定时：30 分钟后淡出并暂停（t 切换）",
            "睡眠定时：60 分钟后淡出并暂停（t 切换）",
            "睡眠定时：90 分钟后淡出并暂停（t 切换）",
            "睡眠定时：120 分钟后淡出并暂停（t 切换）",
            "睡眠定时：本条播完时停止，最后 30 秒淡出（t 切换）",
            "睡眠定时已关闭",
        ]
    );
    assert_eq!(app.sleep_remaining(), None);
    assert!(!app.sleeps_at_track_end());
}

#[test]
fn the_sleep_timer_fades_out_then_pauses_without_touching_the_volume() {
    let (temp, mut app, control) = test_app_with_control(sleep_config());
    let start = sleep_fixture(temp.path(), &mut app);
    assert_eq!(app.sleep_remaining(), Some(15 * MINUTE));

    app.tick_timers(start + 14 * MINUTE);
    assert_eq!(control.fade_gain(), 1.0);
    // 还剩 15 秒，即淡出段的一半：增益是一半的平方。
    app.tick_timers(start + 14 * MINUTE + Duration::from_secs(45));
    assert_eq!(control.fade_gain(), 0.25);
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(app.sleep_remaining(), Some(Duration::from_secs(15)));

    app.tick_timers(start + 15 * MINUTE);
    assert_eq!(app.player().state(), PlayState::Paused);
    assert_eq!(app.player().position(), Duration::from_millis(500));
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(app.sleep_remaining(), None);
    assert_eq!(
        app.view.message.as_deref(),
        Some("睡眠定时到，已暂停（按空格继续）")
    );
    // 淡出只动增益，用户音量、配置和系统媒体控制看到的都是原来的 80。
    assert_eq!(app.player().volume(), 80);
    assert_eq!(app.config.volume, 80);
}

#[test]
fn a_timer_that_ends_while_paused_just_switches_off() {
    let (temp, mut app, control) = test_app_with_control(sleep_config());
    let start = sleep_fixture(temp.path(), &mut app);
    app.tick_timers(start + 14 * MINUTE + Duration::from_secs(50));
    assert!(control.fade_gain() < 1.0);

    press(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.player().state(), PlayState::Paused);
    app.tick_timers(start + 15 * MINUTE);

    assert_eq!(app.player().state(), PlayState::Paused);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(app.sleep_remaining(), None);
    assert_eq!(app.view.message.as_deref(), Some("睡眠定时已到"));
}

#[test]
fn changing_tracks_keeps_the_fade_and_t_restores_full_volume() {
    let (temp, mut app, control) = test_app_with_control(sleep_config());
    let start = sleep_fixture(temp.path(), &mut app);
    let fading = start + 14 * MINUTE + Duration::from_secs(45);
    app.tick_timers(fading);

    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(control.fade_gain(), 0.25);

    // 重新计时说明人还醒着，立即恢复音量，倒计时从现在算起。
    press(&mut app, key(KeyCode::Char('t')));
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(app.sleep_remaining(), Some(30 * MINUTE));
    app.tick_timers(fading + MINUTE);
    assert_eq!(control.fade_gain(), 1.0);
}

#[test]
fn the_now_playing_bar_shows_the_sleep_countdown_first() {
    let (_temp, mut app) = test_app(AppConfig::default());
    press(&mut app, key(KeyCode::Char('t')));

    let (_, backend) = render(&app, 100, 24);
    assert!(
        screen_rows(&backend)
            .iter()
            .any(|row| row.contains("⏾15:00·播完停止·100%·队列0"))
    );
    // 窄终端里这一栏被挤短，截掉的是末尾，倒计时仍然可见。
    let (_, backend) = render(&app, 50, 24);
    assert!(
        screen_rows(&backend)
            .iter()
            .any(|row| row.contains("⏾15:00"))
    );
}

/// 选到“本条播完时”：t 按 6 次。
fn sleep_at_track_end(app: &mut App) {
    for _ in 0..6 {
        press(app, key(KeyCode::Char('t')));
    }
    assert!(app.sleeps_at_track_end());
}

#[test]
fn sleeping_at_the_track_end_fades_the_last_seconds_and_ignores_repeat_and_queue() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::All,
        ..AppConfig::default()
    });
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(60));
    app.playback.queue.push_back(long);
    sleep_at_track_end(&mut app);

    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
    // 100 秒的曲目停在 85 秒：还剩 15 秒，淡出到一半。
    assert!(app.playback.player.seek_to(Duration::from_secs(85)));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    // 往回跳转，音量回升。
    assert!(app.playback.player.seek_to(Duration::from_secs(40)));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(app.playback.playing_index, None);
    assert_eq!(app.playback.queue.len(), 1, "队列原样保留");
    assert_eq!(control.fade_gain(), 1.0);
    assert!(!app.sleeps_at_track_end());
    assert_eq!(
        app.view.message.as_deref(),
        Some("睡眠定时：本条已播完，播放停止")
    );
}

#[test]
fn sleeping_at_the_track_end_beats_repeat_one() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::One,
        ..AppConfig::default()
    });
    play_long_track_at(temp.path(), &mut app, Duration::from_secs(99));
    sleep_at_track_end(&mut app);

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(app.playback.playing_index, None);
}

#[test]
fn the_now_playing_bar_marks_a_track_end_timer() {
    let (_temp, mut app) = test_app(AppConfig::default());
    sleep_at_track_end(&mut app);
    let (_, backend) = render(&app, 100, 24);
    assert!(
        screen_rows(&backend)
            .iter()
            .any(|row| row.contains("⏾本条·播完停止"))
    );
}
