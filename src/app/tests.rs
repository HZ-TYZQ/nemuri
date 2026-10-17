use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::config::{AppConfig, AppPaths};
use crate::marks::Role;
use crate::player::fake::{FakeBackend, FakeControl};
use crate::player::{ChannelMix, PlayState, PlaybackBackend, Player};
use crate::progress::Listened;
use crate::track::{Folder, RepeatMode, SortKey, Track};

use crate::ui::{MouseInput, ViewLayout};

use super::playback::short_reason;
use super::{Action, App, BagUpdate, Overlay, PasteSummary};

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
/// 没有画过的布局只影响帮助弹层的滚动，滚动的测试用 `press_in`。
fn press(app: &mut App, key: KeyEvent) {
    press_in(app, &ViewLayout::default(), key);
}

/// 按上一帧的布局按一次键。
fn press_in(app: &mut App, view: &ViewLayout, key: KeyEvent) {
    if let Some(action) = crate::ui::key_action(app, view, key) {
        app.dispatch(action);
    }
}

/// 一次鼠标事件：按上一帧布局翻译成 Action 再分派，与主循环相同。
fn mouse(app: &mut App, input: &mut MouseInput, event: MouseEvent, view: &ViewLayout) {
    for action in input.actions(event, view, app) {
        app.dispatch(action);
    }
}

/// 历史里各项的文件路径，从旧到新。
fn history_paths(app: &App) -> Vec<PathBuf> {
    app.playback
        .history
        .iter()
        .map(|played| played.path.clone())
        .collect()
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
    assert_eq!(history_paths(&app), vec![first]);
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
fn a_paste_goes_into_the_focused_input_as_one_line() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.view.search_active = true;
    app.dispatch(Action::Paste("耳语\r\n掏耳\t安静\n".to_owned()));
    assert_eq!(app.catalog.query(), "耳语 掏耳 安静");

    app.view.search_active = false;
    app.view.overlay = Overlay::NameInput;
    app.view.name_input = "睡前".to_owned();
    app.dispatch(Action::Paste(" 列表\n\n".to_owned()));
    assert_eq!(app.view.name_input, "睡前 列表");
}

#[test]
fn a_paste_outside_an_input_is_not_run_as_shortcuts() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let visualizer = app.config.visualizer_enabled;
    app.dispatch(Action::Paste("v/q".to_owned()));
    assert!(!app.should_quit());
    assert_eq!(app.config.visualizer_enabled, visualizer);
    assert!(!app.view.search_active);
    assert_eq!(app.catalog.query(), "");
}

#[test]
fn control_keys_are_not_typed_into_an_input() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.view.search_active = true;
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.catalog.query(), "");
    // Windows 上 AltGr 打出的字符带着 Ctrl 和 Alt，仍是输入。
    press(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ),
    );
    assert_eq!(app.catalog.query(), "@");
}

/// 帮助弹层所在的各行：去掉最后一行底栏，底栏的提示里也有“q 退出”。
fn popup_text(backend: &ratatui::backend::TestBackend) -> String {
    let rows = screen_rows(backend);
    rows[..rows.len() - 1].join("\n")
}

#[test]
fn the_help_scrolls_on_a_short_terminal_and_stops_at_the_end() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    press(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.view.overlay, Overlay::Help);

    // 24 行的终端放不下全部帮助，最后一行起初看不到。
    let (view, backend) = render(&app, 80, 24);
    let max = view.help_max_scroll;
    assert!(max > 0);
    assert!(!popup_text(&backend).contains("q退出"));
    assert!(popup_text(&backend).contains(&format!("↑↓滚动·下面还有{max}行")));

    for _ in 0..max {
        let (view, _) = render(&app, 80, 24);
        press_in(&mut app, &view, key(KeyCode::Char('j')));
    }
    let (view, backend) = render(&app, 80, 24);
    assert!(popup_text(&backend).contains("q退出"));
    assert!(popup_text(&backend).contains("↑↓滚动·已到末尾"));

    // 到底以后再按也不往下滚，往上按一下就能回去。
    press_in(&mut app, &view, key(KeyCode::Down));
    assert_eq!(app.view.help_scroll, max);
    press_in(&mut app, &view, key(KeyCode::Char('k')));
    assert_eq!(app.view.help_scroll, max - 1);

    // 关掉再打开从头开始；滚轮也只滚到底为止。
    press(&mut app, key(KeyCode::Esc));
    press(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.view.help_scroll, 0);
    let (view, _) = render(&app, 80, 24);
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 40,
        row: 12,
        modifiers: KeyModifiers::NONE,
    };
    for _ in 0..3 {
        mouse(&mut app, &mut MouseInput::default(), wheel, &view);
    }
    assert_eq!(app.view.help_scroll, max);
}

#[test]
fn the_help_fits_a_tall_terminal_without_scrolling() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.dispatch(Action::OpenOverlay(Overlay::Help));
    let (view, backend) = render(&app, 80, 40);
    assert_eq!(view.help_max_scroll, 0);
    assert!(popup_text(&backend).contains("q退出"));
    assert!(!popup_text(&backend).contains("滚动"));
    press_in(
        &mut app,
        &view,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(app.view.help_scroll, 0);
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

    let saved = std::fs::read_to_string(temp.path().join("config/nemuri/config.kdl")).unwrap();
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
    let saved = std::fs::read_to_string(temp.path().join("config/nemuri/config.kdl")).unwrap();
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
    assert_eq!(history_paths(&app), vec![paths[0].clone()]);
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
fn balance_keys_step_by_ten_stop_at_the_ends_and_reach_the_backend() {
    let (_temp, mut app, control) = test_app_with_control(AppConfig {
        balance: -90,
        swap_channels: true,
        ..AppConfig::default()
    });
    // 启动时就按配置设好。
    assert_eq!(
        control.channel_mix(),
        ChannelMix {
            balance: -90,
            swapped: true
        }
    );

    press(&mut app, key(KeyCode::Char(',')));
    assert_eq!(app.config.balance, -100);
    assert_eq!(
        app.view.message.as_deref(),
        Some("平衡：偏左 100（右声道 0%）")
    );
    press(&mut app, key(KeyCode::Char(',')));
    assert_eq!(app.config.balance, -100);

    for _ in 0..10 {
        press(&mut app, key(KeyCode::Char('.')));
    }
    assert_eq!(app.config.balance, 0);
    assert_eq!(app.view.message.as_deref(), Some("平衡：居中"));
    for _ in 0..3 {
        press(&mut app, key(KeyCode::Char('.')));
    }
    assert_eq!(
        app.view.message.as_deref(),
        Some("平衡：偏右 30（左声道 70%）")
    );
    for _ in 0..10 {
        press(&mut app, key(KeyCode::Char('.')));
    }
    assert_eq!(app.config.balance, 100);
    assert_eq!(
        control.channel_mix(),
        ChannelMix {
            balance: 100,
            swapped: true
        }
    );

    press(&mut app, key(KeyCode::Char('w')));
    assert!(!app.config.swap_channels);
    assert!(!control.channel_mix().swapped);
    assert_eq!(app.view.message.as_deref(), Some("左右声道已恢复"));
    press(&mut app, key(KeyCode::Char('w')));
    assert!(control.channel_mix().swapped);
    assert_eq!(app.view.message.as_deref(), Some("已互换左右声道"));
}

#[test]
fn the_now_playing_bar_shows_balance_and_swap_only_when_set() {
    let (_temp, mut app) = test_app(AppConfig::default());
    let bar = |app: &App| {
        let (_, backend) = render(app, 100, 24);
        screen_rows(&backend)
            .into_iter()
            .find(|row| row.contains("队列0"))
            .unwrap()
    };
    assert!(bar(&app).contains("播完停止·100%·队列0"));

    app.dispatch(Action::ShiftBalance(30));
    app.dispatch(Action::ToggleChannelSwap);
    assert!(bar(&app).contains("播完停止·100%·偏右30·左右互换·队列0"));
    app.dispatch(Action::ShiftBalance(-60));
    assert!(bar(&app).contains("100%·偏左30·左右互换·队列0"));
}

#[test]
fn privacy_mode_hides_what_is_playing_from_the_system_media_controls() {
    let (temp, mut app) = test_app(AppConfig::default());
    let path = temp.path().join("music").join("耳语.wav");
    write_test_wav(&path);
    let mut item = long_track(&path);
    item.artist = Some("某人".to_owned());
    item.album = Some("合集".to_owned());
    app.catalog.set_tracks(vec![item]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_secs(40)));

    let open = app.media_snapshot();
    assert_eq!(open.title, "耳语");
    assert_eq!(open.artist.as_deref(), Some("某人"));
    assert_eq!(open.path.as_deref(), Some(path.as_path()));

    press(&mut app, key(KeyCode::Char('i')));
    assert!(app.config.privacy);
    assert_eq!(
        app.view.message.as_deref(),
        Some("隐私模式已开启：系统媒体控制只显示“正在播放”")
    );
    let private = app.media_snapshot();
    assert_eq!(private.title, "正在播放");
    assert_eq!(private.artist, None);
    assert_eq!(private.album, None);
    assert_eq!(private.path, None);
    // 系统那边照常能看进度、暂停和跳转。
    assert_eq!(private.duration, open.duration);
    assert_eq!(private.position, open.position);
    assert_eq!(private.status, open.status);
    assert_eq!(private.track_id, open.track_id);
    assert!(private.identity_changed(&open));

    press(&mut app, key(KeyCode::Char('i')));
    assert!(!app.config.privacy);
    assert_eq!(app.view.message.as_deref(), Some("隐私模式已关闭"));
    assert_eq!(app.media_snapshot(), open);
}

#[test]
fn privacy_mode_without_a_track_reports_no_title() {
    let (_temp, app) = test_app(AppConfig {
        privacy: true,
        ..AppConfig::default()
    });
    assert_eq!(app.media_snapshot().title, "");
}

#[test]
fn the_interface_is_titled_nemuri() {
    let (_temp, app) = test_app(AppConfig::default());
    let (_, backend) = render(&app, 100, 24);
    assert!(screen_rows(&backend)[0].contains("♪Nemuri"));
    // 终端太小时的提示框也用这个名字。
    let (_, backend) = render(&app, 40, 10);
    assert!(screen_rows(&backend)[0].contains("Nemuri"));
}

#[test]
fn the_now_playing_bar_marks_privacy_mode() {
    let (_temp, app) = test_app(AppConfig {
        privacy: true,
        ..AppConfig::default()
    });
    let (_, backend) = render(&app, 100, 24);
    assert!(
        screen_rows(&backend)
            .iter()
            .any(|row| row.contains("隐私·播完停止·100%·队列0"))
    );
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

#[test]
fn a_zero_fade_pauses_at_full_volume() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        sleep_fade: Duration::ZERO,
        ..AppConfig::default()
    });
    let start = sleep_fixture(temp.path(), &mut app);
    assert_eq!(
        app.view.message.as_deref(),
        Some("睡眠定时：15 分钟后暂停（t 切换）")
    );

    app.tick_timers(start + 15 * MINUTE - Duration::from_secs(1));
    assert_eq!(control.fade_gain(), 1.0);
    app.tick_timers(start + 15 * MINUTE);
    assert_eq!(app.player().state(), PlayState::Paused);
}

/// 两段 20 分钟的录音和一首 100 秒的歌，文件都写出来。
fn recordings_fixture(app: &mut App, music: &Path) -> Vec<PathBuf> {
    let mut tracks: Vec<Track> = ["A/rec1.wav", "A/rec2.wav", "Music/song.wav"]
        .into_iter()
        .map(|relative| nested_track(music, relative))
        .collect();
    tracks[0].duration = Some(20 * MINUTE);
    tracks[1].duration = Some(20 * MINUTE);
    tracks[2].duration = Some(Duration::from_secs(100));
    for item in &tracks {
        std::fs::create_dir_all(item.path.parent().unwrap()).unwrap();
        write_test_wav(&item.path);
    }
    let paths = tracks.iter().map(|item| item.path.clone()).collect();
    app.catalog.set_tracks(tracks);
    paths
}

const AT_12_34: Duration = Duration::from_secs(12 * 60 + 34);

#[test]
fn a_long_recording_resumes_where_it_was_left() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));

    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(app.view.message, None);

    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), AT_12_34);
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(
        app.view.message.as_deref(),
        Some("从 12:34 继续（按 0 从头播放）")
    );

    // 0 跳回开头；再离开时这一条算没开始听，下次从头放。
    press(&mut app, key(KeyCode::Char('0')));
    assert_eq!(app.player().position(), Duration::ZERO);
    app.dispatch(Action::Next);
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), Duration::ZERO);
}

#[test]
fn resume_works_through_the_queue_and_previous() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));

    // p 回到历史里的上一条。
    app.dispatch(Action::Next);
    app.dispatch(Action::Previous);
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().position(), AT_12_34);

    // 排进队列的录音在自然播完后接着放，也从记下的位置开始。
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    app.playback.queue.push_back(paths[0].clone());
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().position(), AT_12_34);
}

#[test]
fn songs_and_finished_recordings_start_from_the_beginning() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::One,
        ..AppConfig::default()
    });
    recordings_fixture(&mut app, &temp.path().join("music"));

    // 100 秒的歌不记进度。
    app.play_index(2, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_secs(50)));
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    app.play_index(2, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), Duration::ZERO);

    // 停在离结尾不到 3 秒处离开，算听完。
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(2))
    );
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), Duration::ZERO);

    // 单曲循环自然播完后从头重播，而不是回到播完前记下的位置。
    assert!(app.playback.player.seek_to(AT_12_34));
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().position(), Duration::ZERO);
}

#[test]
fn progress_is_written_every_30_seconds_or_at_once_when_playback_stops() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    let file = temp.path().join("data/nemuri/progress.json");
    let start = app.now;
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));

    app.tick_timers(start + Duration::from_secs(10));
    assert!(!file.exists(), "30 秒内不写盘");
    app.tick_timers(start + Duration::from_secs(31));
    assert!(std::fs::read_to_string(&file).unwrap().contains("754000"));

    // 暂停后的下一个 tick 立刻写，不等满 30 秒。
    assert!(app.playback.player.seek_to(AT_12_34 + MINUTE));
    app.tick_timers(start + Duration::from_secs(32));
    press(&mut app, key(KeyCode::Char(' ')));
    app.tick_timers(start + Duration::from_secs(33));
    assert!(std::fs::read_to_string(&file).unwrap().contains("814000"));
}

#[test]
fn progress_survives_a_restart() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));
    app.save_progress().unwrap();

    let mut restarted = app_in(temp.path(), "music", AppConfig::default());
    recordings_fixture(&mut restarted, &temp.path().join("music"));
    restarted
        .play_path(&paths[0], false, BagUpdate::Reanchor)
        .unwrap();
    assert_eq!(restarted.player().position(), AT_12_34);
}

#[test]
fn progress_follows_a_rename_on_rescan() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    recordings_fixture(&mut app, &music);
    let mut tracks = app.catalog.tracks().to_vec();
    for (index, track) in tracks.iter_mut().enumerate() {
        track.file_size = 1_000 + index as u64;
    }
    app.catalog.set_tracks(tracks.clone());
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));
    app.dispatch(Action::Next);

    // 改名后的文件大小和修改时间不变，扫描结果里只有路径和名字变了。
    let renamed = music.join("A/rec1（改过名）.wav");
    std::fs::rename(&tracks[0].path, &renamed).unwrap();
    tracks[0].path = renamed.clone();
    tracks[0].relative_path = PathBuf::from("A/rec1（改过名）.wav");
    app.apply_scan_finished(tracks, Vec::new());
    settle_search_and_selection(&mut app);

    app.play_path(&renamed, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), AT_12_34);
}

#[test]
fn library_rows_show_listened_progress_for_long_recordings_only() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    // rec1 听到 35%，rec2 听完，100 秒的歌听了一半。
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_secs(7 * 60 + 2)));
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(1))
    );
    app.play_index(2, true, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_secs(50)));

    let (_, backend) = render(&app, 100, 24);
    let rows = screen_rows(&backend);
    let line_after = |name: &str| {
        let at = rows.iter().position(|row| row.contains(name)).unwrap();
        rows[at + 1].clone()
    };
    assert!(line_after("rec1").contains("35%"), "{}", line_after("rec1"));
    assert!(
        line_after("rec2").contains("✓听完"),
        "{}",
        line_after("rec2")
    );
    assert!(!line_after("song").contains('%'), "{}", line_after("song"));
}

#[test]
fn the_folder_filter_comes_back_after_a_restart() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    let tracks = app.catalog.tracks().to_vec();
    // 没有在播放也要记住筛选。
    choose_folder(&mut app, 2);
    app.save_session().unwrap();
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(tracks.clone(), Vec::new());
    settle_search_and_selection(&mut app);
    assert_eq!(app.catalog.folder(), Some(&named("b")));
    assert_eq!(app.visible_indices().len(), 2);
    // 恢复是安静的，不覆盖扫描完成的提示。
    assert_eq!(app.view.message.as_deref(), Some("扫描完成，共 5 首歌曲"));

    // 换到别的音乐库不恢复这个库的筛选。
    app.save_session().unwrap();
    drop(app);
    std::fs::create_dir(temp.path().join("elsewhere")).unwrap();
    let mut app = app_in(temp.path(), "elsewhere", AppConfig::default());
    app.apply_scan_finished(tracks, Vec::new());
    assert_eq!(app.catalog.folder(), None);
}

#[test]
fn a_remembered_folder_that_is_gone_shows_everything() {
    let (temp, mut app) = test_app(AppConfig::default());
    folder_fixture(&mut app, &temp.path().join("music"));
    let tracks = app.catalog.tracks().to_vec();
    choose_folder(&mut app, 2);
    app.save_session().unwrap();
    drop(app);

    let without_b: Vec<Track> = tracks
        .into_iter()
        .filter(|track| Folder::of(track) != named("b"))
        .collect();
    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(without_b, Vec::new());
    assert_eq!(app.catalog.folder(), None);
    assert_eq!(app.visible_indices().len(), 3);

    // 退出时既没有曲目也没有筛选：会话文件被清掉。
    app.save_session().unwrap();
    assert!(!temp.path().join("data/nemuri/session.toml").exists());
}

#[test]
fn a_startup_note_outlasts_the_first_scan_and_the_restored_session() {
    let (temp, mut app) = test_app(AppConfig::default());
    let long = play_long_track_at(temp.path(), &mut app, Duration::from_secs(40));
    app.save_session().unwrap();
    drop(app);

    let music = temp.path().join("music");
    let paths = AppPaths::from_roots(
        temp.path().join("config"),
        temp.path().join("data"),
        temp.path().join("cache"),
        Some(music.clone()),
    );
    let note = "config.kdl 有误: 第 2 行 volume 的值 900 应为 0–100 的整数，已忽略";
    let mut app = App::with_player(
        Box::new(FakeBackend::new().0),
        music,
        paths,
        AppConfig::default(),
        Some(note.to_owned()),
        false,
    )
    .unwrap();
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    settle_search_and_selection(&mut app);

    // 上次的曲目照样恢复，只是底栏留给启动时的提示。
    assert_eq!(app.playback.player.state(), PlayState::Paused);
    assert_eq!(
        app.current_track().map(|t| t.path.clone()),
        Some(long.clone())
    );
    assert_eq!(app.view.message.as_deref(), Some(note));

    // 之后的重扫照常报告结果。
    app.apply_scan_finished(vec![long_track(&long)], Vec::new());
    assert_eq!(app.view.message.as_deref(), Some("扫描完成，共 1 首歌曲"));
}

#[test]
fn a_startup_note_already_replaced_by_the_user_is_not_brought_back() {
    let temp = tempfile::tempdir().unwrap();
    let music = temp.path().join("music");
    let paths = AppPaths::from_roots(
        temp.path().join("config"),
        temp.path().join("data"),
        temp.path().join("cache"),
        Some(music.clone()),
    );
    let mut app = App::with_player(
        Box::new(FakeBackend::new().0),
        music.clone(),
        paths,
        AppConfig::default(),
        Some("已把 tui-music-player 目录里的设置和播放记录复制到新目录".to_owned()),
        false,
    )
    .unwrap();
    press(&mut app, key(KeyCode::Char('m')));
    app.apply_scan_finished(vec![track(music.join("a.wav"))], Vec::new());
    assert_eq!(app.view.message.as_deref(), Some("扫描完成，共 1 首歌曲"));
}

#[test]
fn reordering_the_queue_right_after_a_natural_end_stays_in_range() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(2, false, BagUpdate::Reanchor).unwrap();
    app.playback.queue = [paths[0].clone(), paths[1].clone()].into_iter().collect();
    press(&mut app, key(KeyCode::Char('Q')));
    press(&mut app, key(KeyCode::Char('j')));
    assert_eq!(app.view.queue_selected, 1);

    // 和主循环一样：tick 先修正选择，再处理播完事件；自动切歌取走队首。
    app.clamp_selections();
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(0));
    // 选择跟着原来选中的 rec2 走，它现在是队首。
    assert_eq!(app.playback.queue, [paths[1].clone()]);
    assert_eq!(app.view.queue_selected, 0);
    // 下一个 tick 之前界面先画一帧，然后用户按 K：已在顶端，什么也不做。
    render(&app, 100, 30);
    press(&mut app, key(KeyCode::Char('K')));
    assert_eq!(app.view.queue_selected, 0);
    assert_eq!(app.playback.queue, [paths[1].clone()]);

    // 选择万一落在队列外面，重排也只是忽略，不会崩溃。
    app.view.queue_selected = 1;
    press(&mut app, key(KeyCode::Char('K')));
    assert_eq!(app.playback.queue, [paths[1].clone()]);
}

#[test]
fn seeking_past_the_end_of_a_recording_marks_it_finished() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(10))
    );

    // L 前进 60 秒越过结尾，切到下一条。
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.playback.playing_index, Some(1));
    let recording = app.catalog.get(0).unwrap().clone();
    assert_eq!(
        app.listened(&recording),
        Some(crate::progress::Listened::Finished)
    );
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), Duration::ZERO);
}

#[test]
fn seeking_past_the_end_of_the_last_recording_marks_it_finished() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    let only = app.catalog.get(0).unwrap().clone();
    app.catalog.set_tracks(vec![only.clone()]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(10))
    );

    // 后面没有可放的了：越过结尾后停止播放。
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.listened(&only),
        Some(crate::progress::Listened::Finished)
    );
}

#[test]
fn seeking_past_the_end_of_a_repeated_recording_starts_a_fresh_playthrough() {
    let (temp, mut app) = test_app(AppConfig {
        repeat: RepeatMode::All,
        ..AppConfig::default()
    });
    recordings_fixture(&mut app, &temp.path().join("music"));
    let only = app.catalog.get(0).unwrap().clone();
    app.catalog.set_tracks(vec![only.clone()]);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(10))
    );

    // 列表里只有这一条：越过结尾后从头重放，而不是回到 19:50。
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(
        app.listened(&only),
        Some(crate::progress::Listened::Finished)
    );

    // 新的一遍照常记录位置。
    assert!(app.playback.player.seek_to(AT_12_34));
    app.tick_timers(app.now);
    assert_eq!(
        app.listened(&only),
        Some(crate::progress::Listened::InProgress(AT_12_34))
    );
}

/// 把一份旧数据复制到新目录、启动 App 并完成第一次扫描，返回底栏的提示。
fn first_start_after_the_rename(corrupt_progress: bool, corrupt_playlist: bool) -> String {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let music = root.join("music");
    let old_data = root.join("data/tui-music-player");
    std::fs::create_dir_all(root.join("config/tui-music-player")).unwrap();
    std::fs::write(root.join("config/tui-music-player/config.kdl"), "").unwrap();
    std::fs::create_dir_all(old_data.join("playlists")).unwrap();
    if corrupt_progress {
        std::fs::write(old_data.join("progress.json"), "{ not json").unwrap();
    }
    if corrupt_playlist {
        std::fs::write(old_data.join("playlists/night.json"), "{ not json").unwrap();
    }
    let paths = AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        Some(music.clone()),
    );
    let adopted = crate::config::adopt_previous_dirs(&paths).unwrap();
    let mut app = App::with_player(
        Box::new(FakeBackend::new().0),
        music.clone(),
        paths,
        AppConfig::default(),
        adopted,
        false,
    )
    .unwrap();
    app.apply_scan_finished(vec![track(music.join("a.wav"))], Vec::new());
    app.view.message.clone().unwrap_or_default()
}

#[test]
fn copied_data_that_cannot_be_loaded_is_reported_before_the_copy_note() {
    let copied = "已把 tui-music-player 目录里的设置和播放记录复制到新目录";
    for (progress, playlist) in [(true, false), (false, true), (true, true)] {
        let message = first_start_after_the_rename(progress, playlist);
        let note = message.find(copied).unwrap_or_else(|| panic!("{message}"));
        if progress {
            let error = message
                .find("收听进度")
                .unwrap_or_else(|| panic!("{message}"));
            assert!(error < note, "{message}");
        }
        if playlist {
            let error = message
                .find("播放列表")
                .unwrap_or_else(|| panic!("{message}"));
            assert!(error < note, "{message}");
        }
    }
    // 数据都读得了时只有复制的说明。
    assert!(first_start_after_the_rename(false, false).starts_with(copied));
}

#[test]
fn startup_notes_are_joined_in_order_and_skip_missing_ones() {
    assert_eq!(super::join_notes([None, None]), None);
    assert_eq!(
        super::join_notes([Some("甲".to_owned()), None, Some("乙".to_owned())]),
        Some("甲\n乙".to_owned())
    );
}

#[test]
fn several_startup_notes_all_show_on_screen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let music = root.join("music");
    let old_data = root.join("data/tui-music-player");
    std::fs::create_dir_all(root.join("config/tui-music-player")).unwrap();
    std::fs::write(root.join("config/tui-music-player/config.kdl"), "").unwrap();
    std::fs::create_dir_all(old_data.join("playlists")).unwrap();
    std::fs::write(old_data.join("progress.json"), "{ not json").unwrap();
    std::fs::write(old_data.join("playlists/night.json"), "{ not json").unwrap();
    let paths = AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        Some(music.clone()),
    );
    let adopted = crate::config::adopt_previous_dirs(&paths).unwrap();
    let mut app = App::with_player(
        Box::new(FakeBackend::new().0),
        music.clone(),
        paths,
        AppConfig::default(),
        adopted,
        false,
    )
    .unwrap();
    app.apply_scan_finished(vec![track(music.join("a.wav"))], Vec::new());

    // 三条提示各自折行显示在底栏，常见的 80 和 100 列都看得全。
    for (width, height) in [(100, 30), (80, 24)] {
        let (_, backend) = render(&app, width, height);
        let screen = screen_rows(&backend).join("");
        for expected in ["播放列表文件无法识别", "从空记录开始", "确认无误后可以删除"]
        {
            assert!(
                screen.contains(expected),
                "{width}×{height} 缺少“{expected}”：\n{screen}"
            );
        }
    }

    // 下一条提示出现后，底栏恢复成一行。
    press(&mut app, key(KeyCode::Char('m')));
    let (_, backend) = render(&app, 80, 24);
    let rows = screen_rows(&backend);
    assert_eq!(rows[23], "•已静音");
    assert!(rows[22].starts_with('╰'), "{}", rows[22]);
}

#[test]
fn a_footer_that_would_take_more_than_a_third_of_the_screen_ends_in_an_ellipsis() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.view.message = Some(vec!["很长的一条提示".repeat(20); 6].join("\n"));
    let (_, backend) = render(&app, 60, 24);
    let rows = screen_rows(&backend);
    // 24 行的三分之一是 8 行：前面照常折行，最后一行以省略号结尾。
    let footer: Vec<&String> = rows
        .iter()
        .rev()
        .take_while(|row| !row.starts_with('╰'))
        .collect();
    assert_eq!(footer.len(), 8, "{rows:#?}");
    assert!(footer[0].ends_with('…'), "{}", footer[0]);
}

/// 正在播放 rec1（20 分钟），在 `minutes` 的每个位置各按一次 b，最后停在 `at`。
fn marks_fixture(app: &mut App, music: &Path, minutes: &[u32], at: Duration) -> Vec<PathBuf> {
    let paths = recordings_fixture(app, music);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    for minute in minutes {
        assert!(app.playback.player.seek_to(MINUTE * *minute));
        press(app, key(KeyCode::Char('b')));
    }
    assert!(app.playback.player.seek_to(at));
    paths
}

fn mark_positions(app: &App) -> Vec<Duration> {
    app.current_marks()
        .iter()
        .map(|mark| mark.position)
        .collect()
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, key(KeyCode::Char(character)));
    }
}

#[test]
fn b_adds_a_mark_at_the_current_position_once() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    let file = temp.path().join("data/nemuri/marks.json");

    for at in [5 * MINUTE, AT_12_34, 2 * MINUTE] {
        assert!(app.playback.player.seek_to(at));
        press(&mut app, key(KeyCode::Char('b')));
    }
    assert_eq!(
        app.view.message.as_deref(),
        Some("已添加跳转点 2:00（B 管理）")
    );
    assert_eq!(mark_positions(&app), [2 * MINUTE, 5 * MINUTE, AT_12_34]);
    // 改了就写盘，不等退出。
    assert!(std::fs::read_to_string(&file).unwrap().contains("754000"));

    // 不到 1 秒算同一处。
    assert!(
        app.playback
            .player
            .seek_to(5 * MINUTE + Duration::from_millis(500))
    );
    press(&mut app, key(KeyCode::Char('b')));
    assert_eq!(app.view.message.as_deref(), Some("这里已有跳转点 5:00"));
    assert_eq!(mark_positions(&app).len(), 3);
}

#[test]
fn braces_jump_between_marks_and_stop_at_either_end() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[5, 10],
        Duration::ZERO,
    );
    let previous = key(KeyCode::Char('{'));
    let next = key(KeyCode::Char('}'));

    press(&mut app, next);
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("跳到 5:00"));
    // 系统媒体控制也知道位置变了。
    assert!(!app.drain_media_events().is_empty());
    press(&mut app, next);
    assert_eq!(app.player().position(), 10 * MINUTE);
    // 最后一个之后不跳到结尾，免得记成听完。
    press(&mut app, next);
    assert_eq!(app.player().position(), 10 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("后面没有跳转点"));

    // 刚跳到一个点之后 3 秒内往前跳，跳到再前面一个。
    assert!(
        app.playback
            .player
            .seek_to(10 * MINUTE + Duration::from_secs(2))
    );
    press(&mut app, previous);
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert!(app.playback.player.seek_to(12 * MINUTE));
    press(&mut app, previous);
    assert_eq!(app.player().position(), 10 * MINUTE);
    // 第一个之前回到开头。
    assert!(app.playback.player.seek_to(6 * MINUTE));
    press(&mut app, previous);
    press(&mut app, previous);
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(app.view.message.as_deref(), Some("跳到开头"));
}

#[test]
fn mark_keys_only_explain_themselves_without_a_playing_long_recording() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    let keys = ['b', 'B', '{', '}'];

    for character in keys {
        press(&mut app, key(KeyCode::Char(character)));
        assert_eq!(
            app.view.message.as_deref(),
            Some("先开始播放，才能使用跳转点")
        );
    }

    // 100 秒的歌。
    app.play_index(2, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(Duration::from_secs(50)));
    for character in keys {
        press(&mut app, key(KeyCode::Char(character)));
        assert_eq!(
            app.view.message.as_deref(),
            Some("只有 10 分钟以上的录音能设跳转点")
        );
        assert_eq!(app.player().position(), Duration::from_secs(50));
    }
    assert!(!temp.path().join("data/nemuri/marks.json").exists());

    // 没有跳转点的长录音：往前也不会跳回开头；B 打开空面板，好在里面粘贴时间轴。
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(AT_12_34));
    for character in ['{', '}'] {
        press(&mut app, key(KeyCode::Char(character)));
        assert_eq!(
            app.view.message.as_deref(),
            Some("这首还没有跳转点：b 在当前位置添加，或在 B 里粘贴时间轴")
        );
        assert_eq!(app.view.overlay, Overlay::None);
        assert_eq!(app.player().position(), AT_12_34);
    }
    press(&mut app, key(KeyCode::Char('B')));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(app.player().position(), AT_12_34);
}

#[test]
fn marks_work_while_paused_but_not_once_playback_has_stopped() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[], 5 * MINUTE);
    press(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.player().state(), PlayState::Paused);

    press(&mut app, key(KeyCode::Char('b')));
    assert_eq!(mark_positions(&app), [5 * MINUTE]);
    assert!(app.playback.player.seek_to(MINUTE));
    press(&mut app, key(KeyCode::Char('}')));
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(app.player().state(), PlayState::Paused);

    // 播完停止以后没有位置可跳。
    press(&mut app, key(KeyCode::Char(' ')));
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    press(&mut app, key(KeyCode::Char('{')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("先开始播放，才能使用跳转点")
    );
    assert_eq!(app.player().state(), PlayState::Stopped);
}

#[test]
fn the_marks_panel_jumps_renames_and_deletes() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[5, 10, 15],
        11 * MINUTE,
    );
    let file = temp.path().join("data/nemuri/marks.json");

    // 光标落在当前所在段；Enter 跳过去并关闭面板。
    press(&mut app, key(KeyCode::Char('B')));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(app.view.marks_selected, 1);
    press(&mut app, key(KeyCode::Char('j')));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.player().position(), 15 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("跳到 15:00"));

    // 改名：输入框里的字母是文字，不是面板的按键。
    press(&mut app, key(KeyCode::Char('B')));
    assert_eq!(app.view.marks_selected, 2);
    press(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.view.overlay, Overlay::MarkName);
    type_text(&mut app, "哄睡 ed");
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(app.current_marks()[2].name.as_deref(), Some("哄睡"));
    assert!(std::fs::read_to_string(&file).unwrap().contains("哄睡"));

    // 再改名时预填现有名字；Esc 放弃，清空后确定就只剩时间。
    press(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.view.name_input, "哄睡");
    type_text(&mut app, "了");
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(app.current_marks()[2].name.as_deref(), Some("哄睡"));
    press(&mut app, key(KeyCode::Char('e')));
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.current_marks()[2].name, None);

    // 删除不再确认；光标留在范围内，删光时面板留着，变成空面板。
    press(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.view.message.as_deref(), Some("已删除跳转点 15:00"));
    assert_eq!(app.view.marks_selected, 1);
    press(&mut app, key(KeyCode::Char('d')));
    press(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已删除最后一个跳转点 5:00")
    );
    assert!(!std::fs::read_to_string(&file).unwrap().contains("rec1"));
}

#[test]
fn the_marks_panel_follows_a_track_change() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    marks_fixture(&mut app, &music, &[5], 6 * MINUTE);
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    for minute in [3, 8] {
        assert!(app.playback.player.seek_to(MINUTE * minute));
        press(&mut app, key(KeyCode::Char('b')));
    }
    assert!(app.playback.player.seek_to(9 * MINUTE));
    press(&mut app, key(KeyCode::Char('B')));
    assert_eq!(app.view.marks_selected, 1);
    press(&mut app, key(KeyCode::Char('e')));

    // 系统媒体控制切到 rec1：改名作废，光标落到 rec1 当前所在段。
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    app.sync_marks_panel();
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(app.view.marks_selected, 0);
    assert_eq!(mark_positions(&app), [5 * MINUTE]);

    // 换成用不了跳转点的歌：面板关掉。
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
    app.play_index(2, true, BagUpdate::Reanchor).unwrap();
    app.sync_marks_panel();
    assert_eq!(app.view.overlay, Overlay::None);
}

#[test]
fn a_double_click_in_the_marks_panel_jumps_there() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5, 10], MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    let (view, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend);
    assert!(screen.iter().any(|row| row.contains("跳转点·rec1")));
    let row = screen.iter().position(|row| row.contains("10:00")).unwrap() as u16;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 50,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let mut input = MouseInput::default();
    mouse(&mut app, &mut input, click, &view);
    mouse(&mut app, &mut input, click, &view);
    assert_eq!(app.view.overlay, Overlay::None);
    assert_eq!(app.player().position(), 10 * MINUTE);
}

#[test]
fn the_marks_panel_lists_times_names_and_the_current_section() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5, 10], 11 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('k')));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "掏耳");
    press(&mut app, key(KeyCode::Enter));

    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend);
    assert!(
        screen.iter().any(|row| row.contains("▸5:00掏耳")),
        "{screen:#?}"
    );
    assert!(
        screen.iter().any(|row| row.contains("●10:00")),
        "{screen:#?}"
    );
    assert!(
        screen
            .iter()
            .any(|row| row.contains("Enter跳到这里·a加入列表·s/S起点/终点·e改名·d删除"))
    );
    assert!(screen.iter().any(|row| row.contains("v粘贴时间轴·Esc关闭")));

    press(&mut app, key(KeyCode::Char('e')));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(
        screen.contains("5:00的名字（留空只显示时间）："),
        "{screen}"
    );
    assert!(screen.contains(">掏耳█"), "{screen}");
}

/// 在跳转点面板里按 v，把 `text` 当作一整段粘贴进去（Linux 上终端这样送来）。
fn paste_timeline(app: &mut App, text: &str) {
    press(app, key(KeyCode::Char('v')));
    app.dispatch(Action::Paste(text.to_owned()));
}

fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn named_marks(app: &App) -> Vec<(Duration, Option<String>)> {
    app.current_marks()
        .iter()
        .map(|mark| (mark.position, mark.name.clone()))
        .collect()
}

#[test]
fn a_pasted_timeline_adds_named_marks_in_one_go() {
    let (temp, mut app) = test_app(AppConfig::default());
    // 20 分钟的录音，5:00 已有一个跳转点。
    marks_fixture(&mut app, &temp.path().join("music"), &[5], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    paste_timeline(
        &mut app,
        "时间轴：\r\n00:00 开场\r\n05:00.4 重复\r\n12:34 掏耳\r\n30:00 超出\r\n",
    );
    assert_eq!(app.view.overlay, Overlay::PasteMarks);
    assert_eq!(
        app.paste_summary(),
        PasteSummary {
            recognized: 4,
            new: 2,
            existing: 1,
            beyond: 1,
        }
    );

    press(&mut app, ctrl('s'));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已添加 2 个跳转点（1 个已有，1 个超出时长）")
    );
    assert_eq!(
        named_marks(&app),
        [
            (Duration::ZERO, Some("开场".to_owned())),
            (5 * MINUTE, None),
            (AT_12_34, Some("掏耳".to_owned())),
        ]
    );
    // 光标落在新加的最早的一个上；改了就写盘。
    assert_eq!(app.view.marks_selected, 0);
    let saved = std::fs::read_to_string(temp.path().join("data/nemuri/marks.json")).unwrap();
    assert!(saved.contains("掏耳"), "{saved}");

    // 再开粘贴框时是空的。
    press(&mut app, key(KeyCode::Char('v')));
    assert!(app.view.paste_text.is_empty());
}

#[test]
fn the_paste_box_takes_typed_lines_and_stays_open_until_something_can_be_added() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('v')));
    // Windows 上的粘贴逐键送来：Enter 是换行，不是确定。
    type_text(&mut app, "随便写点");
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::PasteMarks);
    press(&mut app, ctrl('s'));
    assert_eq!(app.view.overlay, Overlay::PasteMarks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("没有认出时间，每行写成 03:21 名字")
    );

    type_text(&mut app, "1:00:00 超出了");
    press(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.view.paste_text, "随便写点\n1:00:00 超出");
    press(&mut app, ctrl('s'));
    assert_eq!(app.view.overlay, Overlay::PasteMarks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("没有新的跳转点：1 个超出时长")
    );

    // Esc 回到面板，什么也没加，内容不留。
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert!(app.current_marks().is_empty());
    assert!(app.view.paste_text.is_empty());
}

#[test]
fn the_paste_box_reparses_only_when_its_text_changes() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    paste_timeline(&mut app, "03:00 a\n04:00 b");
    assert_eq!(app.view.paste_parsed.len(), 2);
    // 退格删掉第二行，识别结果跟着变。
    for _ in 0.."\n04:00 b".chars().count() {
        press(&mut app, key(KeyCode::Backspace));
    }
    assert_eq!(app.view.paste_text, "03:00 a");
    assert_eq!(app.view.paste_parsed.len(), 1);

    // 已有的跳转点变了，摘要现查，跟着变。
    let track = app.catalog.tracks()[0].clone();
    app.marks.store.add(&track, 3 * MINUTE).unwrap();
    assert_eq!(
        app.paste_summary(),
        PasteSummary {
            recognized: 1,
            new: 0,
            existing: 1,
            beyond: 0,
        }
    );
    press(&mut app, key(KeyCode::Esc));
    assert!(app.view.paste_parsed.is_empty());
}

#[test]
fn a_paste_longer_than_the_limit_keeps_only_the_start() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    paste_timeline(&mut app, &"耳".repeat(30_000));
    // 64 KiB 里放得下 21845 个三字节的字，不会切在一个字的中间。
    assert_eq!(app.view.paste_text.chars().count(), 21_845);
    assert_eq!(
        app.view.message.as_deref(),
        Some("内容太长，只保留了前面的部分")
    );
}

#[test]
fn a_timeline_pasted_while_a_clip_plays_uses_the_recording_time() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    play_from_playlist(&mut app, 0);
    press(&mut app, key(KeyCode::Char('B')));
    assert_eq!(app.view.overlay, Overlay::Marks);

    paste_timeline(&mut app, "02:00 开头之前\n06:00 片段里\n16:00 结尾之后");
    press(&mut app, ctrl('s'));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已添加 3 个跳转点（其中 2 个在这一段之外）")
    );
    // 面板只列片段里的。
    assert_eq!(mark_positions(&app), [6 * MINUTE]);
    assert_eq!(app.view.marks_selected, 0);
}

#[test]
fn the_paste_box_keeps_its_recording_when_the_track_changes() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = marks_fixture(&mut app, &temp.path().join("music"), &[], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    paste_timeline(&mut app, "03:00 掏耳");

    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(app.view.overlay, Overlay::PasteMarks);
    press(&mut app, ctrl('s'));
    assert_eq!(app.marks.store.marks(&paths[0]).len(), 1);
    // 回到的面板是现在这一首的，它还没有跳转点。
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert!(app.current_marks().is_empty());
}

#[test]
fn the_empty_panel_ignores_row_keys_and_closes_only_for_a_short_track() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    for code in [
        KeyCode::Enter,
        KeyCode::Char('a'),
        KeyCode::Char('s'),
        KeyCode::Char('S'),
        KeyCode::Char('e'),
        KeyCode::Char('d'),
        KeyCode::Char('j'),
    ] {
        press(&mut app, key(code));
        assert_eq!(app.view.overlay, Overlay::Marks, "{code:?}");
    }
    assert_eq!(app.player().position(), 2 * MINUTE);

    // 换到另一个长录音还是空面板；换到短的歌用不了跳转点，关掉面板。
    app.dispatch(Action::Next);
    assert_eq!(app.view.overlay, Overlay::Marks);
    app.dispatch(Action::Next);
    assert_eq!(app.playback.playing_index, Some(2));
    assert_eq!(app.view.overlay, Overlay::None);
}

#[test]
fn the_empty_panel_and_the_paste_box_explain_themselves() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5], 2 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('d')));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend);
    assert!(
        screen
            .iter()
            .any(|row| row.contains("还没有跳转点：b在当前位置添加，v粘贴时间轴")),
        "{screen:#?}"
    );

    press(&mut app, key(KeyCode::Char('v')));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("粘贴时间轴·rec1"), "{screen}");
    assert!(
        screen.contains("█每行一个时间和名字，例如03:21掏耳"),
        "{screen}"
    );
    assert!(screen.contains("还没有认出时间"), "{screen}");
    assert!(screen.contains("Ctrl+S添加·Esc取消"), "{screen}");

    // 放不下时显示最后几行，光标在末尾。
    let timeline: Vec<String> = (1..=30)
        .map(|minute| format!("{minute}:00 第{minute}段"))
        .collect();
    app.dispatch(Action::Paste(timeline.join("\n")));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("30:00第30段█"), "{screen}");
    assert!(!screen.contains("1:00第1段"), "{screen}");
    assert!(
        screen.contains("认出30个：新增19个·11个超出时长"),
        "{screen}"
    );
}

#[test]
fn marks_survive_a_restart_and_follow_a_rename() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    marks_fixture(&mut app, &music, &[5, 10], MINUTE);
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    recordings_fixture(&mut app, &music);
    let mut tracks = app.catalog.tracks().to_vec();
    for (index, track) in tracks.iter_mut().enumerate() {
        track.file_size = 1_000 + index as u64;
    }
    // 三个文件原本一样大，改名后认不出是哪一个。给它们不同的大小，
    // 再在原名下改一次名，记录就带上了新的大小。
    app.catalog.set_tracks(tracks.clone());
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert_eq!(mark_positions(&app), [5 * MINUTE, 10 * MINUTE]);
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "开头");
    press(&mut app, key(KeyCode::Enter));
    press(&mut app, key(KeyCode::Esc));
    app.dispatch(Action::Next);

    let renamed = music.join("A/rec1（改过名）.wav");
    std::fs::rename(&tracks[0].path, &renamed).unwrap();
    tracks[0].path = renamed.clone();
    tracks[0].relative_path = PathBuf::from("A/rec1（改过名）.wav");
    app.apply_scan_finished(tracks, Vec::new());
    settle_search_and_selection(&mut app);
    // 跟过去的跳转点马上写盘。
    let file = temp.path().join("data/nemuri/marks.json");
    assert!(std::fs::read_to_string(&file).unwrap().contains("改过名"));

    app.play_path(&renamed, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(mark_positions(&app), [5 * MINUTE, 10 * MINUTE]);
    assert_eq!(app.current_marks()[0].name.as_deref(), Some("开头"));
}

#[test]
fn an_unreadable_marks_file_is_kept_aside_at_startup() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("music")).unwrap();
    let file = temp.path().join("data/nemuri/marks.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not json").unwrap();

    let app = app_in(temp.path(), "music", AppConfig::default());
    let message = app.view.message.clone().unwrap();
    assert!(message.starts_with("跳转点文件无法识别"), "{message}");
    assert!(!file.exists());
}

#[test]
fn a_failed_save_is_reported_and_retried_on_exit() {
    let (temp, mut app) = test_app(AppConfig::default());
    // 目标位置被一个目录占着，写不进去。
    let file = temp.path().join("data/nemuri/marks.json");
    std::fs::create_dir_all(&file).unwrap();
    marks_fixture(&mut app, &temp.path().join("music"), &[5], MINUTE);
    let message = app.view.message.clone().unwrap();
    assert!(message.starts_with("无法保存跳转点"), "{message}");
    assert_eq!(mark_positions(&app), [5 * MINUTE]);
    assert!(app.save_marks().is_err());

    std::fs::remove_dir(&file).unwrap();
    app.save_marks().unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().contains("300000"));
}

#[test]
fn the_now_playing_bar_shows_marks_and_the_named_section() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5, 10], 11 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "耳语");
    press(&mut app, key(KeyCode::Enter));
    press(&mut app, key(KeyCode::Esc));

    let (_, backend) = render(&app, 100, 24);
    let rows = screen_rows(&backend);
    assert!(
        rows.iter().any(|row| row.contains("♫正在播放·耳语")),
        "{rows:#?}"
    );
    // 跳转点在总时长的 25% 和 50% 处，播放头在 55%。
    let row = rows
        .iter()
        .find(|row| row.contains("11:00") && row.contains("20:00"))
        .unwrap();
    let start = row.find("11:00").unwrap() + "11:00".len();
    let end = row.rfind("20:00").unwrap();
    let bar: Vec<char> = row[start..end].chars().collect();
    let cell = |ratio: f64| (ratio * bar.len() as f64).floor() as usize;
    let ticks: Vec<usize> = (0..bar.len()).filter(|&at| bar[at] == '|').collect();
    assert_eq!(ticks, [cell(0.25), cell(0.5)], "{row}");
    assert_eq!(bar[cell(0.55)], '>', "{row}");

    // 没有名字的段和第一个跳转点之前都不显示段名。
    for at in [6 * MINUTE, 4 * MINUTE] {
        assert!(app.playback.player.seek_to(at));
        let (_, backend) = render(&app, 100, 24);
        assert!(
            !screen_rows(&backend)
                .iter()
                .any(|row| row.contains("正在播放·"))
        );
    }

    // 窄终端截断名字，不挤掉标题。
    assert!(app.playback.player.seek_to(11 * MINUTE));
    app.dispatch(Action::OpenOverlay(Overlay::Marks));
    app.dispatch(Action::RenameMark);
    type_text(&mut app, "很长的名字".repeat(10).as_str());
    app.dispatch(Action::Activate);
    app.dispatch(Action::Back);
    let (_, backend) = render(&app, 50, 24);
    let rows = screen_rows(&backend);
    let title = rows.iter().find(|row| row.contains("正在播放")).unwrap();
    assert!(title.contains("正在播放·耳语很长的名字"), "{title}");
    assert!(title.contains('…'), "{title}");
}

#[cfg(unix)]
#[test]
fn an_unreadable_marks_file_is_left_alone_and_b_explains_why() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let music = temp.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let file = temp.path().join("data/nemuri/marks.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let saved = r#"{"version":1,"recordings":[]}"#;
    std::fs::write(&file, saved).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&file).is_ok() {
        // 以 root 运行时权限挡不住读取，这个场景无从模拟。
        return;
    }

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    let message = app.view.message.clone().unwrap();
    assert!(message.starts_with("无法读取跳转点"), "{message}");
    recordings_fixture(&mut app, &music);
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    press(&mut app, key(KeyCode::Char('b')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("启动时跳转点文件没能读取或保留，本次运行不能添加跳转点")
    );
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("启动时跳转点文件没能读取或保留，本次运行不能添加跳转点")
    );
    app.save_marks().unwrap();

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), saved);
}

/// rec1 在 2、5、15、18 分钟有跳转点，2:00 是起点、18:00 是终点（都在面板里设），
/// 播放中停在 `at`；返回三个曲目的路径。
fn bounds_fixture(app: &mut App, music: &Path, at: Duration) -> Vec<PathBuf> {
    let paths = marks_fixture(app, music, &[2, 5, 15, 18], at);
    set_role(app, 0, 's');
    set_role(app, 3, 'S');
    paths
}

/// 打开跳转点面板，在第 `row` 行按 `s`（起点）或 `S`（终点），再关掉面板。
fn set_role(app: &mut App, row: usize, character: char) {
    press(app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(row));
    press(app, key(KeyCode::Char(character)));
    press(app, key(KeyCode::Esc));
}

const START_POINT: Duration = Duration::from_secs(2 * 60);
const END_POINT: Duration = Duration::from_secs(18 * 60);

#[test]
fn a_recording_that_would_start_at_0_00_starts_at_its_start_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);
    assert_eq!(control.stop_at(), Some(END_POINT));

    // 没听过（停在起点之前离开，不记进度）：从起点开始，终点交给播放器。
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(control.stop_at(), None);
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已跳过开头，从起点 2:00 开始")
    );
    assert_eq!(control.stop_at(), Some(END_POINT));

    // 听到一半：照常续播，提示 0 回到起点。
    assert!(app.playback.player.seek_to(10 * MINUTE));
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), 10 * MINUTE);
    assert_eq!(
        app.view.message.as_deref(),
        Some("从 10:00 继续（按 0 回到起点）")
    );

    // 跳过终点去听片尾再离开：算听完，下次从起点开始；马上离开也不抹掉“听完”。
    assert!(app.playback.player.seek_to(19 * MINUTE));
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    let rec1 = app.catalog.tracks()[0].clone();
    assert_eq!(app.listened(&rec1), Some(Listened::Finished));
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), START_POINT);
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.listened(&rec1), Some(Listened::Finished));
}

#[test]
fn progress_saved_before_the_start_or_end_was_set_still_skips_the_ends() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[2, 18],
        START_POINT + Duration::from_secs(1),
    );
    let rec1 = app.catalog.tracks()[0].clone();
    // 2:01 记下的进度，之后才把 2:00 设为起点：还算没开始听。
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    app.marks
        .store
        .toggle_role(&rec1, 0, Role::Start, 20 * MINUTE)
        .unwrap();
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), START_POINT);

    // 17:58 记下的进度，之后才把 18:00 设为终点：已在终点前 3 秒以内，算听完。
    assert!(
        app.playback
            .player
            .seek_to(END_POINT - Duration::from_secs(2))
    );
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(
        app.listened(&rec1),
        Some(Listened::InProgress(END_POINT - Duration::from_secs(2)))
    );
    app.marks
        .store
        .toggle_role(&rec1, 1, Role::End, 20 * MINUTE)
        .unwrap();
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), START_POINT);
}

#[test]
fn reaching_the_end_point_ends_the_track_like_a_natural_end() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::None,
        ..AppConfig::default()
    });
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    let rec1 = app.catalog.tracks()[0].clone();

    // 播放器在终点报告播完：记为听完，接下一首；rec2 没有终点。
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.listened(&rec1), Some(Listened::Finished));
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(control.stop_at(), None);

    // 播完停止：停下并说明。
    app.config.repeat = RepeatMode::Stop;
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已播完，播放停止（按 z 切换循环方式）")
    );
}

#[test]
fn repeat_one_loops_from_the_start_point_to_the_end_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::One,
        ..AppConfig::default()
    });
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(app.player().position(), START_POINT);
    // 重新装载后终点又交给了播放器。
    assert_eq!(control.stop_at(), Some(END_POINT));
}

#[test]
fn jumping_past_the_end_point_leaves_the_stop_to_the_player() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 17 * MINUTE);
    // 播放器只在一路播到终点时停：跳过去、跳回来，App 都不用改停止位置。
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.player().position(), 17 * MINUTE + MINUTE);
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.player().position(), 19 * MINUTE);
    assert_eq!(app.player().state(), PlayState::Playing);
    assert_eq!(control.stop_at(), Some(END_POINT));
    press(&mut app, key(KeyCode::Char('H')));
    press(&mut app, key(KeyCode::Char('H')));
    assert_eq!(control.stop_at(), Some(END_POINT));
}

#[test]
fn zero_goes_to_the_start_point_first_and_then_to_0_00() {
    let (temp, mut app) = test_app(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    let zero = key(KeyCode::Char('0'));

    app.drain_media_events();
    press(&mut app, zero);
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(
        app.view.message.as_deref(),
        Some("回到起点 2:00（再按 0 从 0:00 播放）")
    );
    assert!(!app.drain_media_events().is_empty());
    press(&mut app, zero);
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(app.view.message.as_deref(), Some("跳到开头"));

    // 起点后 3 秒以内、起点之前：回到 0:00。
    for at in [START_POINT + Duration::from_secs(2), MINUTE] {
        assert!(app.playback.player.seek_to(at));
        press(&mut app, zero);
        assert_eq!(app.player().position(), Duration::ZERO);
    }

    // 暂停中照常跳，仍是暂停；有名字时提示带上名字。
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "正片");
    press(&mut app, key(KeyCode::Enter));
    press(&mut app, key(KeyCode::Esc));
    press(&mut app, key(KeyCode::Char(' ')));
    assert!(app.playback.player.seek_to(10 * MINUTE));
    press(&mut app, zero);
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(app.player().state(), PlayState::Paused);
    assert_eq!(
        app.view.message.as_deref(),
        Some("回到起点 2:00 正片（再按 0 从 0:00 播放）")
    );

    // 没有起点的录音和短曲目：和以前一样回到 0:00，不提示。
    for index in [1, 2] {
        app.play_index(index, true, BagUpdate::Reanchor).unwrap();
        assert!(app.playback.player.seek_to(Duration::from_secs(50)));
        app.view.message = None;
        press(&mut app, zero);
        assert_eq!(app.player().position(), Duration::ZERO);
        assert_eq!(app.view.message, None);
    }
}

#[test]
fn the_panel_sets_and_clears_the_start_and_end_points() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[2, 5, 15, 18],
        10 * MINUTE,
    );
    let file = temp.path().join("data/nemuri/marks.json");
    let message = |app: &App| app.view.message.clone().unwrap();

    set_role(&mut app, 0, 's');
    assert_eq!(message(&app), "已设为起点 2:00，以后从头播放时从这里开始");
    set_role(&mut app, 3, 'S');
    assert_eq!(message(&app), "已设为终点 18:00，以后播到这里就算播完");
    assert_eq!(control.stop_at(), Some(END_POINT));
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("\"start\": true") && saved.contains("\"end\": true"));

    // 先后不对：只提示，不改。
    set_role(&mut app, 3, 's');
    assert_eq!(
        message(&app),
        "已把终点 18:00 改为起点，以后从这里开始，播到文件结尾"
    );
    assert_eq!(control.stop_at(), None);
    set_role(&mut app, 2, 'S');
    assert_eq!(message(&app), "终点要在起点 18:00 之后");
    set_role(&mut app, 1, 's');
    assert_eq!(message(&app), "已设为起点 5:00，以后从头播放时从这里开始");
    set_role(&mut app, 3, 'S');
    assert_eq!(message(&app), "已设为终点 18:00，以后播到这里就算播完");
    set_role(&mut app, 3, 's');
    assert_eq!(
        message(&app),
        "已把终点 18:00 改为起点，以后从这里开始，播到文件结尾"
    );
    set_role(&mut app, 1, 'S');
    assert_eq!(message(&app), "终点要在起点 18:00 之后");
    set_role(&mut app, 3, 'S');
    assert_eq!(
        message(&app),
        "已把起点 18:00 改为终点，以后从 0:00 开始，播到这里就算播完"
    );
    set_role(&mut app, 2, 'S');
    assert_eq!(message(&app), "已设为终点 15:00，以后播到这里就算播完");
    set_role(&mut app, 3, 's');
    assert_eq!(message(&app), "起点要在终点 15:00 之前");

    // 再按一次取消。
    set_role(&mut app, 2, 'S');
    assert_eq!(message(&app), "已取消终点，以后播到文件结尾");
    set_role(&mut app, 0, 's');
    set_role(&mut app, 0, 's');
    assert_eq!(message(&app), "已取消起点，以后从 0:00 开始");
    assert_eq!(control.stop_at(), None);
    assert!(!std::fs::read_to_string(&file).unwrap().contains("true"));

    // 删掉起点或终点，起点或终点也就没了。
    set_role(&mut app, 0, 's');
    set_role(&mut app, 3, 'S');
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(3));
    press(&mut app, key(KeyCode::Char('d')));
    assert_eq!(message(&app), "已删除终点 18:00，以后播到文件结尾");
    assert_eq!(control.stop_at(), None);
    app.dispatch(Action::SelectRow(0));
    press(&mut app, key(KeyCode::Char('d')));
    assert_eq!(message(&app), "已删除起点 2:00，以后从 0:00 开始");
    assert_eq!(mark_positions(&app), [5 * MINUTE, 15 * MINUTE]);
}

#[test]
fn panel_jumps_name_the_start_and_end_points() {
    let (temp, mut app) = test_app(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    press(&mut app, key(KeyCode::Char('}')));
    assert_eq!(app.view.message.as_deref(), Some("跳到 15:00"));
    press(&mut app, key(KeyCode::Char('}')));
    assert_eq!(app.view.message.as_deref(), Some("跳到终点 18:00"));
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(0));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.message.as_deref(), Some("跳到起点 2:00"));
}

#[test]
fn a_restored_session_near_or_past_the_end_point_goes_back_to_the_start_point() {
    for (left_at, restored_at) in [
        (END_POINT + MINUTE, START_POINT),
        (END_POINT - Duration::from_secs(2), START_POINT),
        // 其他位置照原样恢复，起点之前也一样。
        (10 * MINUTE, 10 * MINUTE),
        (MINUTE, MINUTE),
    ] {
        let (temp, mut app) = test_app(AppConfig::default());
        let music = temp.path().join("music");
        bounds_fixture(&mut app, &music, left_at);
        app.save_session().unwrap();
        drop(app);

        let (backend, control) = FakeBackend::new();
        let mut app = app_with_backend(
            temp.path(),
            "music",
            AppConfig::default(),
            Box::new(backend),
        );
        recordings_fixture(&mut app, &music);
        let tracks = app.catalog.tracks().to_vec();
        app.apply_scan_finished(tracks, Vec::new());
        assert_eq!(app.player().position(), restored_at, "上次停在 {left_at:?}");
        assert_eq!(control.stop_at(), Some(END_POINT));
    }
}

#[test]
fn the_track_end_timer_fades_out_before_the_end_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    sleep_at_track_end(&mut app);

    assert!(
        app.playback
            .player
            .seek_to(END_POINT - Duration::from_secs(15))
    );
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    // 跳过了终点：一直放到文件结尾，按离结尾的时间淡出。
    assert!(app.playback.player.seek_to(END_POINT + MINUTE));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
    assert!(
        app.playback
            .player
            .seek_to(20 * MINUTE - Duration::from_secs(15))
    );
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);

    // 播到终点：停止，不接下一首。
    assert!(app.playback.player.seek_to(10 * MINUTE));
    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.view.message.as_deref(),
        Some("睡眠定时：本条已播完，播放停止")
    );
}

#[test]
fn an_end_point_past_a_shortened_file_no_longer_stops_playback() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    assert_eq!(control.stop_at(), Some(END_POINT));
    // 重扫时文件变成 17 分钟：18:00 的终点不显示，也不再生效。
    let mut tracks = app.catalog.tracks().to_vec();
    tracks[0].duration = Some(17 * MINUTE);
    app.apply_scan_finished(tracks, Vec::new());
    assert_eq!(mark_positions(&app), [2 * MINUTE, 5 * MINUTE, 15 * MINUTE]);
    assert_eq!(control.stop_at(), None);
}

#[test]
fn the_marks_panel_tags_the_start_and_end_points_and_keeps_the_whole_hint() {
    let (temp, mut app) = test_app(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(0));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(
        &mut app,
        "正片开始之前的一大段说明和广告，名字长到这个面板加宽之后也还是放不下",
    );
    press(&mut app, key(KeyCode::Enter));

    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend);
    // 名字太长时截短名字，标签照样看得见。
    assert!(
        screen
            .iter()
            .any(|row| row.contains("▸2:00正片开始之前") && row.contains("…起点")),
        "{screen:#?}"
    );
    assert!(
        screen.iter().any(|row| row.contains("18:00终点")),
        "{screen:#?}"
    );
    assert!(
        screen.iter().any(|row| row.contains("●5:00│")),
        "普通跳转点不加标签：{screen:#?}"
    );
    assert!(
        screen
            .iter()
            .any(|row| row.contains("Enter跳到这里·a加入列表·s/S起点/终点·e改名·d删除")),
        "{screen:#?}"
    );
    assert!(
        screen.iter().any(|row| row.contains("v粘贴时间轴·Esc关闭")),
        "{screen:#?}"
    );

    // s 和 S 是面板里的键，不会漏到曲库去开关随机播放。
    let shuffle = app.config.shuffle;
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(app.config.shuffle, shuffle);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已取消起点，以后从 0:00 开始")
    );
}

#[test]
fn the_help_says_zero_goes_to_the_start_point_first() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.dispatch(Action::OpenOverlay(Overlay::Help));
    let (_, backend) = render(&app, 80, 40);
    assert!(
        popup_text(&backend).contains("0–9跳到0%–90%位置（有起点时0先回到起点）"),
        "{}",
        popup_text(&backend)
    );
}

/// 跳转点文件里记下的跳转点：位置（毫秒）和起点、终点标志。
fn saved_marks(app: &App) -> Vec<(u64, bool, bool)> {
    let (store, _) = crate::marks::MarkStore::load(&app.paths.marks_file);
    store
        .marks(&app.catalog.tracks()[0].path)
        .iter()
        .map(|mark| {
            (
                mark.position.as_millis() as u64,
                mark.role == Role::Start,
                mark.role == Role::End,
            )
        })
        .collect()
}

#[test]
fn parentheses_set_the_start_and_end_points_where_playback_is() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    app.tick_timers(app.now);

    // 附近没有跳转点：在这里加一个并设为起点，一次写进文件；不跳转，也不改音量。
    let start = MINUTE + 30 * SECOND;
    assert!(app.playback.player.seek_to(start));
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已设为起点 1:30，以后从头播放时从这里开始")
    );
    assert_eq!(mark_positions(&app), [start]);
    assert_eq!(saved_marks(&app), [(90_000, true, false)]);
    assert_eq!(app.player().position(), start);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);

    // 设终点：交给播放器；已经在终点上，这一遍不淡出，接着放。
    assert!(app.playback.player.seek_to(END_POINT));
    press(&mut app, key(KeyCode::Char(')')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已设为终点 18:00，以后播到这里就算播完")
    );
    assert_eq!(control.stop_at(), Some(END_POINT));
    assert_eq!(
        saved_marks(&app),
        [(90_000, true, false), (1_080_000, false, true)]
    );
    assert_eq!(app.player().state(), PlayState::Playing);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);

    // 以后从头播放时从起点开始。
    reload_rec1(&mut app);
    assert_eq!(app.player().position(), start);
    assert_eq!(control.stop_at(), Some(END_POINT));
}

#[test]
fn parentheses_use_a_mark_within_a_second_and_say_which_one_they_replace() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5, 15], MINUTE);
    name_mark(&mut app, 0, "正片");

    // 5:00.6 离 5:00 的跳转点不到 1 秒：用它，名字不变，不多出一个。
    assert!(
        app.playback
            .player
            .seek_to(5 * MINUTE + Duration::from_millis(600))
    );
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已设为起点 5:00 正片，以后从头播放时从这里开始")
    );
    assert_eq!(mark_positions(&app), [5 * MINUTE, 15 * MINUTE]);
    // 再按一次取消，跳转点还在。
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已取消起点，以后从 0:00 开始")
    );
    assert_eq!(mark_positions(&app), [5 * MINUTE, 15 * MINUTE]);

    // 另设一个起点：原来的变回普通跳转点，提示里说出来。
    press(&mut app, key(KeyCode::Char('(')));
    assert!(app.playback.player.seek_to(3 * MINUTE));
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已设为起点 3:00，以后从头播放时从这里开始（原来的起点 5:00 正片 留作普通跳转点）")
    );
    assert_eq!(mark_positions(&app), [3 * MINUTE, 5 * MINUTE, 15 * MINUTE]);
    let bounds = crate::marks::bounds(app.current_marks());
    assert_eq!(bounds.start, Some(3 * MINUTE));

    // 起点上按 )：改为终点要在 0:00 之后，可以；它从起点改成终点。
    press(&mut app, key(KeyCode::Char(')')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已把起点 3:00 改为终点，以后从 0:00 开始，播到这里就算播完")
    );
}

#[test]
fn parentheses_refuse_like_s_and_b_and_then_change_nothing() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    bounds_fixture(&mut app, &music, 19 * MINUTE);
    let all = [2 * MINUTE, 5 * MINUTE, 15 * MINUTE, END_POINT];

    // 终点之后设起点、起点之前设终点：照 s / S 的提示，不加跳转点。
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(app.view.message.as_deref(), Some("起点要在终点 18:00 之前"));
    assert!(app.playback.player.seek_to(MINUTE));
    press(&mut app, key(KeyCode::Char(')')));
    assert_eq!(app.view.message.as_deref(), Some("终点要在起点 2:00 之后"));
    assert_eq!(mark_positions(&app), all);

    // 放播放列表的条目时不能设。
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    play_from_playlist(&mut app, 0);
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("播放列表里不用录音的起点和终点，从曲库播放时再设")
    );

    // 短曲、停止时：和 b 一样说明原因。
    play_library_row(&mut app, 2);
    press(&mut app, key(KeyCode::Char(')')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("只有 10 分钟以上的录音能设跳转点")
    );
    app.playback.player.stop();
    press(&mut app, key(KeyCode::Char('(')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("先开始播放，才能使用跳转点")
    );

    play_library_row(&mut app, 0);
    assert_eq!(mark_positions(&app), all);
}

#[test]
fn parentheses_are_typed_into_the_search_box() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    app.play_index(0, false, BagUpdate::Reanchor).unwrap();
    assert!(app.playback.player.seek_to(5 * MINUTE));
    press(&mut app, key(KeyCode::Char('/')));
    type_text(&mut app, "()");
    assert_eq!(app.catalog.query(), "()");
    assert!(mark_positions(&app).is_empty());
}

#[test]
fn the_help_lists_the_start_and_end_keys() {
    let (_temp, mut app) = test_app(AppConfig::default());
    app.dispatch(Action::OpenOverlay(Overlay::Help));
    let (_, backend) = render(&app, 80, 40);
    assert!(
        popup_text(&backend).contains("(/)在当前位置设起点/终点"),
        "{}",
        popup_text(&backend)
    );
}

#[test]
fn a_start_point_past_a_shortened_file_does_not_block_a_new_end_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[2, 5, 15, 18],
        10 * MINUTE,
    );
    set_role(&mut app, 3, 's');
    // 文件变成 17 分钟：18:00 的起点不显示，也不生效。
    let mut tracks = app.catalog.tracks().to_vec();
    tracks[0].duration = Some(17 * MINUTE);
    app.apply_scan_finished(tracks, Vec::new());
    assert_eq!(mark_positions(&app), [2 * MINUTE, 5 * MINUTE, 15 * MINUTE]);

    set_role(&mut app, 2, 'S');
    assert_eq!(
        app.view.message.as_deref(),
        Some("已设为终点 15:00，以后播到这里就算播完")
    );
    assert_eq!(control.stop_at(), Some(15 * MINUTE));
}

#[test]
fn adding_a_track_twice_to_a_playlist_is_allowed_with_a_note() {
    let (temp, mut app) = test_app(AppConfig::default());
    let song = temp.path().join("music/a.wav");
    app.catalog.set_tracks(vec![track(song.clone())]);
    app.playlists.create("睡前").unwrap();
    app.view.overlay = Overlay::Playlists;

    press(&mut app, key(KeyCode::Char('a')));
    assert_eq!(app.view.message.as_deref(), Some("已加入“睡前”"));
    press(&mut app, key(KeyCode::Char('a')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已加入“睡前”（列表里已有这首，现在有 2 条）")
    );
    let reloaded = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(reloaded.all()[0].entries.len(), 2);
}

#[test]
fn a_playlist_change_that_cannot_be_saved_is_kept_and_saved_on_exit() {
    let (temp, mut app) = test_app(AppConfig::default());
    let song = temp.path().join("music/a.wav");
    app.catalog.set_tracks(vec![track(song.clone())]);
    app.playlists.create("x").unwrap();
    app.view.overlay = Overlay::Playlists;
    // 列表文件的位置被一个目录占住，写盘会失败。
    let file = app.paths.playlists_dir.join("78.json");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();

    press(&mut app, key(KeyCode::Char('a')));
    let message = app.view.message.clone().unwrap();
    assert!(
        message.starts_with("已加入“x”，但无法保存播放列表“x”")
            && message.ends_with("；之后改动或退出时再试"),
        "{message}"
    );
    assert_eq!(app.playlists.all()[0].entries.len(), 1);
    assert!(app.save_playlists().is_err());

    std::fs::remove_dir(&file).unwrap();
    app.save_playlists().unwrap();
    let reloaded = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(reloaded.all()[0].entries[0].path, song);
}

/// 曲库里放 `names` 这几首能播的短曲，建一个列表“睡前”，按顺序装着其中前 `listed` 首。
/// 返回各首的路径。
fn playlist_fixture(temp: &Path, app: &mut App, names: &[&str], listed: usize) -> Vec<PathBuf> {
    let music = temp.join("music");
    let tracks: Vec<Track> = names
        .iter()
        .map(|name| {
            let path = music.join(name);
            write_test_wav(&path);
            track(path)
        })
        .collect();
    app.catalog.set_tracks(tracks.clone());
    settle_search_and_selection(app);
    let index = app.playlists.create("睡前").unwrap();
    for item in &tracks[..listed] {
        let saved = app
            .playlists
            .add_entry(index, item, crate::playlist::Range::WHOLE, None)
            .unwrap();
        assert_eq!(saved, crate::playlist::Saved::Yes);
    }
    tracks.into_iter().map(|item| item.path).collect()
}

/// 打开播放列表面板，进入“睡前”，在第 `position` 条上按 Enter。
fn play_from_playlist(app: &mut App, position: usize) {
    press(app, key(KeyCode::Char('P')));
    press(app, key(KeyCode::Enter));
    app.dispatch(Action::SelectRow(position));
    press(app, key(KeyCode::Enter));
}

/// 在“睡前”的条目面板里删掉第 `position` 条，再关掉面板。
fn remove_from_playlist(app: &mut App, position: usize) {
    press(app, key(KeyCode::Char('P')));
    press(app, key(KeyCode::Enter));
    app.dispatch(Action::SelectRow(position));
    press(app, key(KeyCode::Char('d')));
    press(app, key(KeyCode::Esc));
    press(app, key(KeyCode::Esc));
}

/// 在曲库里选中第 `row` 首按 Enter。
fn play_library_row(app: &mut App, row: usize) {
    app.dispatch(Action::SelectRow(row));
    press(app, key(KeyCode::Enter));
}

fn playing(app: &App) -> PathBuf {
    app.player().current_path().unwrap().to_path_buf()
}

fn finish(app: &mut App, control: &FakeControl) {
    control.finish_track();
    app.drain_player_events();
}

/// 列表范围：列表名、第几条和条目数；曲库范围时为 None。
fn scope(app: &App) -> Option<(String, Option<usize>, usize)> {
    app.scope_summary()
        .map(|(name, number, count)| (name.to_owned(), number, count))
}

fn in_list(number: Option<usize>, count: usize) -> Option<(String, Option<usize>, usize)> {
    Some(("睡前".to_owned(), number, count))
}

#[test]
fn a_playlist_plays_from_the_chosen_entry_to_its_end_and_stops() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 3);

    play_from_playlist(&mut app, 1);
    assert_eq!(playing(&app), paths[1]);
    assert_eq!(app.view.overlay, Overlay::None);
    // 不再把后面的条目倒进队列。
    assert!(app.playback.queue.is_empty());
    assert_eq!(scope(&app), in_list(Some(2), 3));
    // 列表默认按顺序放完，不管曲库的“播完停止”。
    assert_eq!(app.config.repeat, RepeatMode::Stop);
    assert_eq!(app.media_snapshot().repeat, RepeatMode::None);
    assert!(app.media_snapshot().can_go_next);

    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(scope(&app), in_list(Some(3), 3));
    assert!(!app.media_snapshot().can_go_next);
    finish(&mut app, &control);
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(app.view.message.as_deref(), Some("播放列表“睡前”已播完"));

    // 停下后仍在列表范围里，空格从最后一条重放。
    assert_eq!(scope(&app), in_list(Some(3), 3));
    press(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(app.player().state(), PlayState::Playing);
}

#[test]
fn z_changes_only_the_playlists_own_repeat_mode_and_saves_it() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 3);
    play_from_playlist(&mut app, 2);

    press(&mut app, key(KeyCode::Char('z')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("列表“睡前”循环：列表循环")
    );
    assert_eq!(app.config.repeat, RepeatMode::Stop);
    assert_eq!(app.media_snapshot().repeat, RepeatMode::All);
    assert!(app.media_snapshot().can_go_next);
    let saved = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(saved.all()[0].repeat, RepeatMode::All);
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[0]);

    press(&mut app, key(KeyCode::Char('z')));
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[0]);

    press(&mut app, key(KeyCode::Char('z')));
    finish(&mut app, &control);
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已播完，播放停止（按 z 切换循环方式）")
    );

    // 系统媒体控制关掉循环时，列表改为“顺序”，而不是曲库的“播完停止”。
    app.dispatch(Action::SetRepeat(RepeatMode::One));
    app.dispatch(Action::SetRepeat(RepeatMode::None));
    assert_eq!(app.playlists.all()[0].repeat, RepeatMode::None);
    assert_eq!(app.config.repeat, RepeatMode::Stop);

    // 回到曲库范围，`z` 又切曲库的。
    play_library_row(&mut app, 1);
    assert_eq!(scope(&app), None);
    press(&mut app, key(KeyCode::Char('z')));
    assert_eq!(app.view.message.as_deref(), Some("循环：顺序"));
    assert_eq!(app.config.repeat, RepeatMode::None);
    assert_eq!(app.playlists.all()[0].repeat, RepeatMode::None);
}

#[test]
fn shuffle_in_a_playlist_plays_every_entry_once_and_leaves_the_library_bag_alone() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        shuffle: true,
        ..AppConfig::default()
    });
    let names = ["a.wav", "b.wav", "c.wav", "d.wav", "e.wav"];
    let paths = playlist_fixture(temp.path(), &mut app, &names, 4);
    play_library_row(&mut app, 4);
    let library_bag = app.playback.order.bag().to_vec();

    play_from_playlist(&mut app, 1);
    let mut played = vec![playing(&app)];
    for _ in 0..3 {
        finish(&mut app, &control);
        played.push(playing(&app));
    }
    finish(&mut app, &control);
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(played[0], paths[1]);
    played.sort();
    assert_eq!(played, paths[..4]);
    assert_eq!(app.playback.order.bag(), library_bag);
}

#[test]
fn queued_tracks_play_in_between_and_the_library_takes_back_the_scope() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 2);
    play_from_playlist(&mut app, 0);
    app.playback.queue.push_back(paths[2].clone());

    // 插播的整首放完，回到列表接当前条目的下一条。
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(app.playback.origin, super::scope::Origin::Library);
    assert_eq!(scope(&app), in_list(Some(1), 2));
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[1]);
    assert_eq!(scope(&app), in_list(Some(2), 2));

    // 在曲库里点播一首，就回到曲库范围。
    play_library_row(&mut app, 2);
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(scope(&app), None);
}

#[test]
fn previous_returns_to_the_scope_each_item_was_played_in() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 2);
    play_library_row(&mut app, 2);
    play_from_playlist(&mut app, 0);
    play_library_row(&mut app, 2);
    assert_eq!(scope(&app), None);

    // 退回列表里的那一条，也回到列表的范围。
    press(&mut app, key(KeyCode::Char('p')));
    assert_eq!(playing(&app), paths[0]);
    assert_eq!(scope(&app), in_list(Some(1), 2));
    // 再退回进入列表之前在曲库里放的那首，回到曲库范围。
    press(&mut app, key(KeyCode::Char('p')));
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(scope(&app), None);

    // 那一条从列表里删掉之后再退回去，只当插播放一次，不改变范围。
    play_from_playlist(&mut app, 1);
    play_library_row(&mut app, 2);
    remove_from_playlist(&mut app, 1);
    press(&mut app, key(KeyCode::Char('p')));
    assert_eq!(playing(&app), paths[1]);
    assert!(matches!(
        app.playback.origin,
        super::scope::Origin::Entry { .. }
    ));
    assert_eq!(scope(&app), None);
}

#[test]
fn a_removed_playing_entry_is_followed_by_the_one_after_it() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 3);
    play_from_playlist(&mut app, 1);

    remove_from_playlist(&mut app, 1);
    // 正在放的这一条照样放完。
    assert_eq!(playing(&app), paths[1]);
    assert_eq!(scope(&app), in_list(None, 2));
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(scope(&app), in_list(Some(2), 2));

    // 单曲循环也接后面的那一条，不重放删掉的那一条；后面没有了就转回开头。
    app.dispatch(Action::SetRepeat(RepeatMode::One));
    remove_from_playlist(&mut app, 1);
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[0]);
    assert_eq!(scope(&app), in_list(Some(1), 1));
}

#[test]
fn deleting_the_playing_playlist_returns_playback_to_the_library() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::None,
        ..AppConfig::default()
    });
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 2);
    play_from_playlist(&mut app, 0);

    press(&mut app, key(KeyCode::Char('P')));
    press(&mut app, key(KeyCode::Char('x')));
    press(&mut app, key(KeyCode::Char('y')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("播放列表已删除，音乐文件未受影响；播放回到曲库")
    );
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(scope(&app), None);
    assert_eq!(playing(&app), paths[0]);
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[1]);
}

#[test]
fn playlist_entries_ignore_the_start_point_end_point_and_listening_progress() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), AT_12_34);
    app.tick_timers(app.now);
    let recording = app.catalog.tracks()[0].clone();
    assert_eq!(
        app.listened(&recording),
        Some(Listened::InProgress(AT_12_34))
    );
    let index = app.playlists.create("睡前").unwrap();
    let _ = app
        .playlists
        .add_entry(index, &recording, crate::playlist::Range::WHOLE, None)
        .unwrap();

    play_from_playlist(&mut app, 0);
    // 不续播，也不跳过开头；终点不交给播放器。
    assert_eq!(playing(&app), recording.path);
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(control.stop_at(), None);
    // `0` 回到条目开头，不经过起点。
    assert!(app.playback.player.seek_to(10 * MINUTE));
    press(&mut app, key(KeyCode::Char('0')));
    assert_eq!(app.player().position(), Duration::ZERO);

    // 不记位置，也不记听完。
    assert!(app.playback.player.seek_to(5 * MINUTE));
    app.tick_timers(app.now);
    assert_eq!(
        app.listened(&recording),
        Some(Listened::InProgress(AT_12_34))
    );
    finish(&mut app, &control);
    app.tick_timers(app.now);
    assert_eq!(
        app.listened(&recording),
        Some(Listened::InProgress(AT_12_34))
    );
    let saved = crate::progress::ProgressStore::load(&app.paths.progress_file).0;
    assert_eq!(
        saved.get(&recording.path),
        Some(Listened::InProgress(AT_12_34))
    );
}

#[test]
fn a_session_in_a_playlist_comes_back_to_the_same_entry_and_scope() {
    let (temp, mut app) = test_app(AppConfig::default());
    playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 3);
    let tracks: Vec<Track> = app
        .catalog
        .tracks()
        .iter()
        .map(|item| long_track(&item.path))
        .collect();
    app.catalog.set_tracks(tracks.clone());
    settle_search_and_selection(&mut app);
    play_from_playlist(&mut app, 1);
    assert!(app.playback.player.seek_to(Duration::from_secs(40)));
    app.save_session().unwrap();
    let saved = std::fs::read_to_string(&app.paths.session_file).unwrap();
    assert!(
        saved.contains("position_ms = 40000\nplaylist = \"睡前\"\nentry = 2\n"),
        "{saved}"
    );
    drop(app);

    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(tracks.clone(), Vec::new());
    assert_eq!(playing(&app), tracks[1].path);
    assert_eq!(app.player().state(), PlayState::Paused);
    assert_eq!(app.player().position(), Duration::from_secs(40));
    assert_eq!(scope(&app), in_list(Some(2), 3));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已恢复上次播放：睡前 2/3 b 0:40，按空格继续")
    );
    app.save_session().unwrap();
    drop(app);

    // 列表删掉了：当作曲库播放恢复。
    for file in std::fs::read_dir(temp.path().join("data/nemuri/playlists")).unwrap() {
        std::fs::remove_file(file.unwrap().path()).unwrap();
    }
    let mut app = app_in(temp.path(), "music", AppConfig::default());
    app.apply_scan_finished(tracks.clone(), Vec::new());
    assert_eq!(playing(&app), tracks[1].path);
    assert_eq!(scope(&app), None);
    assert_eq!(app.playback.origin, super::scope::Origin::Library);
}

/// 在已有的曲库上建一个列表“睡前”。每条是曲库里的第几首、开头和结尾（分钟）以及名字。
fn add_clips(app: &mut App, entries: &[(usize, u32, Option<u32>, Option<&str>)]) {
    let index = app.playlists.create("睡前").unwrap();
    for (track, start, end, name) in entries {
        let item = app.catalog.tracks()[*track].clone();
        let range = crate::playlist::Range {
            start: MINUTE * *start,
            end: end.map(|end| MINUTE * end),
        };
        let _ = app
            .playlists
            .add_entry(index, &item, range, name.map(str::to_owned))
            .unwrap();
    }
}

const SECOND: Duration = Duration::from_secs(1);

/// 跳到文件时间 `position` 再走一个 tick，返回交给播放器的淡出增益。
fn gain_at(app: &mut App, control: &FakeControl, position: Duration) -> f32 {
    assert!(app.playback.player.seek_to(position));
    app.tick_timers(app.now);
    control.fade_gain()
}

const HALF_SECOND: Duration = Duration::from_millis(500);

#[test]
fn a_clip_cut_from_the_middle_fades_out_and_the_next_one_fades_in() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None), (1, 2, Some(4), None)]);

    // 装载之前就设好了：从中间开始的片段从无声开始，不会先漏出一下原音量。
    play_from_playlist(&mut app, 0);
    assert_eq!(control.fade_gain(), 0.0);
    // 默认淡出 3 秒，淡入 1 秒。
    assert_eq!(gain_at(&mut app, &control, 5 * MINUTE + HALF_SECOND), 0.25);
    assert_eq!(gain_at(&mut app, &control, 5 * MINUTE + SECOND), 1.0);
    assert_eq!(gain_at(&mut app, &control, 10 * MINUTE), 1.0);
    assert_eq!(
        gain_at(&mut app, &control, 15 * MINUTE - SECOND - HALF_SECOND),
        0.25
    );
    // 暂停时停在当时的音量。
    press(&mut app, key(KeyCode::Char(' ')));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    press(&mut app, key(KeyCode::Char(' ')));

    // 淡出到一半按 n：立刻换，下一条按它自己的开头从无声淡入，不停在上一条的值。
    press(&mut app, key(KeyCode::Char('n')));
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(control.fade_gain(), 0.0);
    assert_eq!(gain_at(&mut app, &control, 2 * MINUTE + SECOND), 1.0);
}

#[test]
fn whole_entries_clips_to_the_file_end_and_library_tracks_keep_full_volume() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(2, 0, None, None), (1, 18, None, None)]);

    // 整首条目：开头和结尾都不淡。
    play_from_playlist(&mut app, 0);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(gain_at(&mut app, &control, Duration::from_secs(99)), 1.0);
    // 放到文件结尾的片段：开头淡入，结尾不淡出。
    play_from_playlist(&mut app, 1);
    assert_eq!(control.fade_gain(), 0.0);
    assert_eq!(gain_at(&mut app, &control, 20 * MINUTE - SECOND), 1.0);
    // 曲库播放从 0:00 开始、没有终点：不淡。
    play_library_row(&mut app, 0);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(gain_at(&mut app, &control, 20 * MINUTE - SECOND), 1.0);
}

#[test]
fn a_zero_clip_fade_turns_clip_fades_off() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        clip_fade: Duration::ZERO,
        ..AppConfig::default()
    });
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    play_from_playlist(&mut app, 0);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(gain_at(&mut app, &control, 15 * MINUTE - SECOND), 1.0);
}

#[test]
fn with_the_sleep_timer_the_quieter_fade_wins() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    play_from_playlist(&mut app, 0);
    sleep_at_track_end(&mut app);

    // 离片段结尾 15 秒：只有睡眠定时在淡出（30 秒淡到一半）。
    assert_eq!(gain_at(&mut app, &control, 15 * MINUTE - 15 * SECOND), 0.25);
    // 离结尾 1.5 秒：两个都在淡，取较小的那个，不相乘。
    let sleep = 1.5_f32 / 30.0;
    assert_eq!(
        gain_at(&mut app, &control, 15 * MINUTE - SECOND - HALF_SECOND),
        sleep * sleep
    );
}

#[test]
fn the_volume_comes_back_once_the_playlist_has_finished() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    play_from_playlist(&mut app, 0);
    assert!(gain_at(&mut app, &control, 15 * MINUTE - HALF_SECOND / 5) < 0.01);

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().state(), PlayState::Stopped);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
}

#[test]
fn a_clip_that_fails_to_load_leaves_the_playing_one_audible() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None), (1, 2, Some(4), None)]);
    play_from_playlist(&mut app, 0);
    press(&mut app, key(KeyCode::Char('n')));
    assert_eq!(gain_at(&mut app, &control, 3 * MINUTE), 1.0);

    // p 退回上一条，它从中间开始，文件却没了：还在放的这一条在装载期间和之后都没被压到无声。
    std::fs::remove_file(&paths[0]).unwrap();
    press(&mut app, key(KeyCode::Char('p')));
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(control.gain_when_loading(), Some(1.0));
    assert_eq!(control.fade_gain(), 1.0);
}

#[test]
fn a_fading_clip_keeps_its_volume_until_the_next_source_takes_over() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None), (1, 2, Some(4), None)]);
    let fading = 15 * MINUTE - SECOND - HALF_SECOND;

    // 淡出到 0.25 时点播曲库里的歌：装载期间旧的仍是 0.25，新的从满音量开始。
    play_from_playlist(&mut app, 0);
    assert_eq!(gain_at(&mut app, &control, fading), 0.25);
    play_library_row(&mut app, 2);
    assert_eq!(app.playback.playing_index, Some(2));
    assert_eq!(control.gain_when_loading(), Some(0.25));
    assert_eq!(control.gain_when_resumed(), Some(1.0));

    // 换到从中间开始的片段：旧的不会先被压到无声，新的从无声开始。
    play_from_playlist(&mut app, 0);
    assert_eq!(gain_at(&mut app, &control, fading), 0.25);
    press(&mut app, key(KeyCode::Char('n')));
    assert_eq!(app.playback.playing_index, Some(1));
    assert_eq!(control.gain_when_loading(), Some(0.25));
    assert_eq!(control.gain_when_resumed(), Some(0.0));

    // 装载失败：旧的音量自始至终不变。
    play_from_playlist(&mut app, 0);
    assert_eq!(gain_at(&mut app, &control, fading), 0.25);
    std::fs::remove_file(&paths[2]).unwrap();
    play_library_row(&mut app, 2);
    assert_eq!(app.playback.playing_index, Some(0));
    assert_eq!(control.gain_when_loading(), Some(0.25));
    assert_eq!(control.fade_gain(), 0.25);
}

/// 放一下 rec2 再回到 rec1，让 rec1 重新装载：从起点、记下的位置或 0:00 开始。
fn reload_rec1(app: &mut App) {
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
}

#[test]
fn a_recording_fades_in_from_its_start_point_and_out_before_its_end_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);

    // 开始放之前就设好了：从起点无声开始，淡入 1 秒。
    reload_rec1(&mut app);
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(control.gain_when_resumed(), Some(0.0));
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 0.25);
    // 暂停时停在当时的音量。
    press(&mut app, key(KeyCode::Char(' ')));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    press(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(gain_at(&mut app, &control, START_POINT + SECOND), 1.0);
    // 淡入只在刚装载之后，再回到起点附近不淡入。
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 1.0);

    // 终点前 3 秒淡出。
    assert_eq!(gain_at(&mut app, &control, END_POINT - 3 * SECOND), 1.0);
    assert_eq!(
        gain_at(&mut app, &control, END_POINT - SECOND - HALF_SECOND),
        0.25
    );
    // 越过了终点：一直放到文件结尾，不淡出；文件结尾也不淡出。
    assert_eq!(gain_at(&mut app, &control, END_POINT), 1.0);
    assert_eq!(gain_at(&mut app, &control, 20 * MINUTE - SECOND), 1.0);
}

#[test]
fn resuming_a_recording_fades_in_but_starting_from_0_00_does_not() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));

    // 没听过的 rec2 从 0:00 开始：不淡入。
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(control.gain_when_resumed(), Some(1.0));
    assert_eq!(gain_at(&mut app, &control, HALF_SECOND), 1.0);

    // 听到 5:00 离开再回来：没有起点也一样从记下的位置淡入。
    assert!(app.playback.player.seek_to(5 * MINUTE));
    app.play_index(0, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(control.fade_gain(), 1.0);
    app.play_index(1, true, BagUpdate::Reanchor).unwrap();
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(control.gain_when_resumed(), Some(0.0));
    assert_eq!(gain_at(&mut app, &control, 5 * MINUTE + HALF_SECOND), 0.25);
    assert_eq!(gain_at(&mut app, &control, 5 * MINUTE + SECOND), 1.0);
}

#[test]
fn a_start_point_that_fails_to_load_falls_back_to_0_00_at_full_volume() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);
    // 文件被剪到 1 分钟：从 2:00 的起点打不开，退回 0:00，不先静音一下。
    control.shorten_file(MINUTE);
    reload_rec1(&mut app);
    assert_eq!(app.player().position(), Duration::ZERO);
    assert_eq!(app.view.message, None);
    assert_eq!(control.gain_when_resumed(), Some(1.0));
    assert_eq!(gain_at(&mut app, &control, HALF_SECOND), 1.0);
}

#[test]
fn jumps_during_the_fade_in_and_playing_through_the_start_point_do_not_fade_in() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);

    // 淡入途中往回跳到起点之前：立刻满音量，再一路放过起点也不淡入。
    reload_rec1(&mut app);
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 0.25);
    assert_eq!(gain_at(&mut app, &control, MINUTE), 1.0);
    assert_eq!(gain_at(&mut app, &control, START_POINT), 1.0);
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 1.0);

    // 刚从起点开始就按 0 回到 0:00，从头放过起点：不淡入。
    reload_rec1(&mut app);
    press(&mut app, key(KeyCode::Char('0')));
    assert_eq!(app.player().position(), Duration::ZERO);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(gain_at(&mut app, &control, START_POINT), 1.0);
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 1.0);

    // 刚从起点开始就按 } 跳到下一个跳转点：立刻出声，回到起点也不淡入。
    reload_rec1(&mut app);
    press(&mut app, key(KeyCode::Char('}')));
    assert_eq!(app.player().position(), 5 * MINUTE);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
    press(&mut app, key(KeyCode::Char('{')));
    assert_eq!(app.player().position(), START_POINT);
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
}

#[test]
fn a_jump_that_lands_inside_the_fade_in_ends_it_too() {
    // BUG-020：淡入 10 秒，放到 2:05 时跳回起点或这一段里的别处，仍在刚装载时的那一段里，
    // 也立刻满音量，不从无声重新淡入。
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        clip_fade: 30 * SECOND,
        ..AppConfig::default()
    });
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);
    let fading = START_POINT + 5 * SECOND;

    // 按 0 回到起点：跳成了就满音量，不等下一个 tick。
    reload_rec1(&mut app);
    assert_eq!(gain_at(&mut app, &control, fading), 0.25);
    press(&mut app, key(KeyCode::Char('0')));
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(control.fade_gain(), 1.0);
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 1.0);

    // 系统媒体控制跳到这一段里：没跳成（不是这一首）时照旧淡入，跳成了满音量。
    reload_rec1(&mut app);
    assert_eq!(gain_at(&mut app, &control, fading), 0.25);
    assert!(!app.seek_to_shown(START_POINT + 3 * SECOND, Some("另一首")));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    assert!(app.seek_to_shown(START_POINT + 3 * SECOND, None));
    assert_eq!(control.fade_gain(), 1.0);

    // 终点前的淡出不受影响。
    assert_eq!(gain_at(&mut app, &control, END_POINT - 15 * SECOND), 0.25);
}

#[test]
fn setting_the_start_or_end_point_where_playback_is_keeps_the_volume() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    marks_fixture(
        &mut app,
        &temp.path().join("music"),
        &[2, 5, 15],
        15 * MINUTE,
    );
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);

    // 在 15:00 上把它设为终点：已经在终点上，不淡出。
    set_role(&mut app, 2, 'S');
    assert_eq!(control.stop_at(), Some(15 * MINUTE));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
    // 回到 5:00 把它设为起点：不会变成无声再淡入。
    assert!(app.playback.player.seek_to(5 * MINUTE));
    set_role(&mut app, 1, 's');
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 1.0);
}

#[test]
fn repeat_one_fades_out_at_the_end_point_and_in_again_from_the_start_point() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        repeat: RepeatMode::One,
        ..AppConfig::default()
    });
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    assert_eq!(
        gain_at(&mut app, &control, END_POINT - SECOND - HALF_SECOND),
        0.25
    );

    control.finish_track();
    app.drain_player_events();
    assert_eq!(app.player().position(), START_POINT);
    assert_eq!(control.gain_when_loading(), Some(0.25));
    assert_eq!(control.gain_when_resumed(), Some(0.0));
    assert_eq!(gain_at(&mut app, &control, START_POINT + HALF_SECOND), 0.25);
}

#[test]
fn before_the_end_point_the_quieter_of_the_two_fades_wins() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    bounds_fixture(&mut app, &temp.path().join("music"), 10 * MINUTE);
    sleep_at_track_end(&mut app);

    // 离终点 15 秒：只有睡眠定时在淡出。
    assert_eq!(gain_at(&mut app, &control, END_POINT - 15 * SECOND), 0.25);
    // 离终点 1.5 秒：两个都在淡，取较小的那个。
    let sleep = 1.5_f32 / 30.0;
    assert_eq!(
        gain_at(&mut app, &control, END_POINT - SECOND - HALF_SECOND),
        sleep * sleep
    );
}

#[test]
fn a_zero_clip_fade_turns_the_start_and_end_point_fades_off() {
    let (temp, mut app, control) = test_app_with_control(AppConfig {
        clip_fade: Duration::ZERO,
        ..AppConfig::default()
    });
    bounds_fixture(&mut app, &temp.path().join("music"), MINUTE);
    reload_rec1(&mut app);
    assert_eq!(control.gain_when_resumed(), Some(1.0));
    assert_eq!(gain_at(&mut app, &control, START_POINT), 1.0);
    assert_eq!(gain_at(&mut app, &control, END_POINT - SECOND), 1.0);
}

#[test]
fn a_session_restored_in_the_middle_fades_in_when_space_starts_it() {
    for (left_at, restored_at) in [
        (10 * MINUTE, 10 * MINUTE),
        // 上次停在终点之后，退回起点：同样从起点淡入。
        (END_POINT + MINUTE, START_POINT),
    ] {
        let (temp, mut app) = test_app(AppConfig::default());
        let music = temp.path().join("music");
        bounds_fixture(&mut app, &music, left_at);
        app.save_session().unwrap();
        drop(app);

        let (backend, control) = FakeBackend::new();
        let mut app = app_with_backend(
            temp.path(),
            "music",
            AppConfig::default(),
            Box::new(backend),
        );
        recordings_fixture(&mut app, &music);
        let tracks = app.catalog.tracks().to_vec();
        app.apply_scan_finished(tracks, Vec::new());
        assert_eq!(app.player().position(), restored_at);
        assert_eq!(control.fade_gain(), 0.0, "上次停在 {left_at:?}");

        press(&mut app, key(KeyCode::Char(' ')));
        assert_eq!(control.gain_when_resumed(), Some(0.0));
        assert_eq!(gain_at(&mut app, &control, restored_at + HALF_SECOND), 0.25);
        assert_eq!(gain_at(&mut app, &control, restored_at + SECOND), 1.0);
    }
}

#[test]
fn a_clip_plays_in_its_own_time_from_its_start_to_its_end() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(
        &mut app,
        &[
            (0, 5, Some(15), Some("掏耳")),
            (0, 15, None, None),
            (2, 0, None, None),
        ],
    );

    play_from_playlist(&mut app, 0);
    // 直接从片段开头装载，结尾交给播放器。
    assert_eq!(playing(&app), paths[0]);
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(control.stop_at(), Some(15 * MINUTE));
    assert_eq!(app.shown_position(), Duration::ZERO);
    assert_eq!(app.shown_duration(), Some(10 * MINUTE));
    let first = app.media_snapshot();
    assert_eq!(first.title, "rec1 · 掏耳");
    assert_eq!(first.duration, Some(10 * MINUTE));
    assert!(app.playback.player.seek_to(7 * MINUTE + 30 * SECOND));
    assert_eq!(app.media_snapshot().position, 2 * MINUTE + 30 * SECOND);

    // 放到结尾接下一条：同一个文件的下一段，放到文件结尾；曲目 ID 跟着换。
    finish(&mut app, &control);
    assert_eq!(app.player().position(), 15 * MINUTE);
    assert_eq!(control.stop_at(), None);
    assert_eq!(app.shown_duration(), Some(5 * MINUTE));
    let second = app.media_snapshot();
    assert_eq!(second.title, "rec1");
    assert_ne!(second.track_id, first.track_id);
    // 隐私模式照旧只给通用标题。
    press(&mut app, key(KeyCode::Char('i')));
    assert_eq!(app.media_snapshot().title, "正在播放");

    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[2]);
    assert_eq!(app.player().position(), Duration::ZERO);
}

#[test]
fn seeking_in_a_clip_stops_at_its_start_and_past_its_end_moves_on() {
    let (temp, mut app) = test_app(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(
        &mut app,
        &[(0, 5, Some(15), Some("掏耳")), (2, 0, None, None)],
    );
    play_from_playlist(&mut app, 0);

    // 系统媒体控制的 SetPosition 用片段内的时间；超出片段长度不动。
    let id = app.media_snapshot().track_id;
    app.dispatch(Action::SeekTo {
        position: MINUTE,
        track_id: Some(id.clone()),
    });
    assert_eq!(app.player().position(), 6 * MINUTE);
    app.dispatch(Action::SeekTo {
        position: 10 * MINUTE,
        track_id: Some(id),
    });
    assert_eq!(app.player().position(), 6 * MINUTE);

    // 往前退到片段开头为止，Seeked 报告片段内的时间。
    app.drain_media_events();
    press(&mut app, key(KeyCode::Char('H')));
    press(&mut app, key(KeyCode::Char('H')));
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(
        app.drain_media_events(),
        vec![
            crate::media::MediaEvent::Seeked {
                position: Duration::ZERO
            };
            2
        ]
    );

    // 0–9 按片段长度换算，0 回到片段开头。
    press(&mut app, key(KeyCode::Char('5')));
    assert_eq!(app.player().position(), 10 * MINUTE);
    press(&mut app, key(KeyCode::Char('0')));
    assert_eq!(app.player().position(), 5 * MINUTE);

    // 越过结尾直接放下一条，不会放到片段之后的部分。
    assert!(app.playback.player.seek_to(14 * MINUTE + 30 * SECOND));
    press(&mut app, key(KeyCode::Char('L')));
    assert_eq!(playing(&app), paths[2]);

    // 点到进度条最右端也是下一条；这是最后一条，列表放完了。
    app.dispatch(Action::SeekToRatio(1.0));
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(app.view.message.as_deref(), Some("播放列表“睡前”已播完"));
}

#[test]
fn a_clip_shows_and_jumps_between_its_own_marks_in_clip_time() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let paths = marks_fixture(&mut app, &music, &[2, 5, 8, 12, 15, 18], MINUTE);
    add_clips(&mut app, &[(0, 5, Some(15), Some("掏耳"))]);
    play_from_playlist(&mut app, 0);
    // 片段开头算在里面，结尾不算。
    assert_eq!(mark_positions(&app), [5 * MINUTE, 8 * MINUTE, 12 * MINUTE]);

    press(&mut app, key(KeyCode::Char('}')));
    assert_eq!(app.player().position(), 8 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("跳到 3:00"));
    press(&mut app, key(KeyCode::Char('{')));
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("跳到 0:00"));
    press(&mut app, key(KeyCode::Char('{')));
    assert_eq!(app.player().position(), 5 * MINUTE);
    assert_eq!(app.view.message.as_deref(), Some("跳到开头"));

    // 面板里删除、改名的是片段里的那一个，不会错到片段之前的跳转点上。
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.view.message.as_deref(), Some("已删除跳转点 3:00"));
    app.dispatch(Action::SelectRow(0));
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "x");
    press(&mut app, key(KeyCode::Enter));
    let stored: Vec<(Duration, Option<String>)> = app
        .marks
        .store
        .marks(&paths[0])
        .iter()
        .map(|mark| (mark.position, mark.name.clone()))
        .collect();
    assert_eq!(
        stored,
        [
            (2 * MINUTE, None),
            (5 * MINUTE, Some("x".to_owned())),
            (12 * MINUTE, None),
            (15 * MINUTE, None),
            (18 * MINUTE, None),
        ]
    );

    // 录音的起点和终点在列表里不能设。
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("播放列表里不用录音的起点和终点，从曲库播放时再设")
    );
    assert!(
        app.marks
            .store
            .marks(&paths[0])
            .iter()
            .all(|mark| mark.role == Role::Plain)
    );
    press(&mut app, key(KeyCode::Esc));

    // b 按文件时间存，提示里写片段内的时间。
    assert!(app.playback.player.seek_to(10 * MINUTE));
    press(&mut app, key(KeyCode::Char('b')));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已添加跳转点 5:00（B 管理）")
    );
    assert!(
        app.marks
            .store
            .marks(&paths[0])
            .iter()
            .any(|mark| mark.position == 10 * MINUTE)
    );
}

#[test]
fn a_clip_past_the_end_of_a_shortened_file_is_skipped_and_a_long_end_is_cut() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(
        &mut app,
        &[(0, 25, Some(28), Some("尾声")), (1, 18, Some(30), None)],
    );

    play_from_playlist(&mut app, 0);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已跳过“rec1 · 尾声”：片段开头 25:00 超出文件时长")
    );
    assert_eq!(playing(&app), paths[1]);
    assert_eq!(app.player().position(), 18 * MINUTE);
    assert_eq!(app.shown_duration(), Some(2 * MINUTE));
    assert_eq!(control.stop_at(), Some(30 * MINUTE));
}

#[test]
fn sleeping_at_the_end_of_a_clip_fades_out_before_the_clip_ends() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), None), (1, 0, None, None)]);
    play_from_playlist(&mut app, 0);
    sleep_at_track_end(&mut app);

    // 离片段结尾还剩 15 秒，淡出到一半。
    assert!(app.playback.player.seek_to(15 * MINUTE - 15 * SECOND));
    app.tick_timers(app.now);
    assert_eq!(control.fade_gain(), 0.25);
    finish(&mut app, &control);
    assert_eq!(app.player().state(), PlayState::Stopped);
    assert_eq!(
        app.view.message.as_deref(),
        Some("睡眠定时：本条已播完，播放停止")
    );
}

#[test]
fn a_session_in_a_clip_comes_back_in_clip_time() {
    let (temp, mut app) = test_app(AppConfig::default());
    recordings_fixture(&mut app, &temp.path().join("music"));
    add_clips(&mut app, &[(0, 5, Some(15), Some("掏耳"))]);
    let tracks = app.catalog.tracks().to_vec();
    play_from_playlist(&mut app, 0);

    // 离结尾 3 秒以内、或者不在片段里（条目改过）时，从片段开头开始。
    for (at, restored) in [
        (7 * MINUTE, 7 * MINUTE),
        (15 * MINUTE - 2 * SECOND, 5 * MINUTE),
        (3 * MINUTE, 5 * MINUTE),
    ] {
        assert!(app.playback.player.seek_to(at));
        app.save_session().unwrap();
        let mut restarted = app_in(temp.path(), "music", AppConfig::default());
        restarted.apply_scan_finished(tracks.clone(), Vec::new());
        assert_eq!(restarted.player().position(), restored, "{at:?}");
        assert_eq!(scope(&restarted), in_list(Some(1), 1));
        assert_eq!(restarted.shown_duration(), Some(10 * MINUTE));
        if at == 7 * MINUTE {
            assert_eq!(
                restarted.view.message.as_deref(),
                Some("已恢复上次播放：睡前 1/1 rec1 · 掏耳 2:00，按空格继续")
            );
        }
    }
}

#[test]
fn the_screen_shows_a_clips_name_and_its_own_time() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[2, 8], MINUTE);
    add_clips(&mut app, &[(0, 5, Some(15), Some("掏耳"))]);
    play_from_playlist(&mut app, 0);
    assert!(app.playback.player.seek_to(8 * MINUTE));

    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("rec1·掏耳"), "{screen}");
    assert!(screen.contains("睡前1/1·顺序"), "{screen}");
    // 进度条两端是片段内的位置和片段长度。
    let bar = screen
        .lines()
        .find(|row| row.contains("10:00"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(bar.starts_with("│3:00"), "{bar}");

    // 面板只列片段里的跳转点，时间也是片段内的。
    press(&mut app, key(KeyCode::Char('B')));
    let (_, backend) = render(&app, 100, 30);
    let popup = popup_text(&backend);
    assert!(popup.contains("●3:00"), "{popup}");
    assert!(
        !popup.contains("2:00") && !popup.contains("8:00"),
        "{popup}"
    );
}

#[test]
fn a_rescan_moves_playlist_entries_to_a_renamed_file() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    let mut tracks = Vec::new();
    for (name, size) in [("a.wav", 10), ("b.wav", 20)] {
        let path = music.join(name);
        write_test_wav(&path);
        tracks.push(Track {
            file_size: size,
            ..track(path)
        });
    }
    app.catalog.set_tracks(tracks.clone());
    let index = app.playlists.create("睡前").unwrap();
    for item in &tracks {
        let _ = app
            .playlists
            .add_entry(index, item, crate::playlist::Range::WHOLE, None)
            .unwrap();
    }
    let renamed = music.join("renamed.wav");
    std::fs::rename(&tracks[0].path, &renamed).unwrap();
    let rescanned = vec![
        Track {
            file_size: 10,
            ..track(renamed.clone())
        },
        tracks[1].clone(),
    ];
    // 列表文件的位置被一个目录占住：跟上了，但没能写盘，说明接在扫描完成后面。
    let file = app.paths.playlists_dir.join("e79da1e5898d.json");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();

    app.apply_scan_finished(rescanned, Vec::new());
    let message = app.view.message.clone().unwrap();
    assert!(
        message.starts_with("扫描完成，共 2 首歌曲；无法保存播放列表“睡前”")
            && message.ends_with("；之后改动或退出时再试"),
        "{message}"
    );
    std::fs::remove_dir(&file).unwrap();
    app.save_playlists().unwrap();
    let saved = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(saved.all()[0].entries[0].path, renamed);

    settle_search_and_selection(&mut app);
    play_from_playlist(&mut app, 0);
    assert_eq!(playing(&app), renamed);
}

/// 打开跳转点面板，给第 `row` 个跳转点改名，再关掉面板。
fn name_mark(app: &mut App, row: usize, name: &str) {
    press(app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(row));
    press(app, key(KeyCode::Char('e')));
    type_text(app, name);
    press(app, key(KeyCode::Enter));
    press(app, key(KeyCode::Esc));
}

/// 列表里各条的范围和名字（分钟数和秒数都按 `Duration` 比）。
fn entries_of(app: &App, list: usize) -> Vec<(crate::playlist::Range, Option<String>)> {
    app.playlists.all()[list]
        .entries
        .iter()
        .map(|entry| (entry.range, entry.name.clone()))
        .collect()
}

fn range(start: Duration, end: Option<Duration>) -> crate::playlist::Range {
    crate::playlist::Range { start, end }
}

#[test]
fn a_in_the_marks_panel_adds_the_section_to_a_chosen_playlist() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[2, 5, 8], 6 * MINUTE);
    name_mark(&mut app, 1, "掏耳");
    app.playlists.create("睡前").unwrap();
    app.playlists.create("白天").unwrap();

    // 从选中的跳转点到下一个跳转点，名字用跳转点的名字；选好列表就回到跳转点面板。
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('a')));
    assert_eq!(app.view.overlay, Overlay::PickPlaylist);
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已加进“睡前”：掏耳（3:00）")
    );
    assert_eq!(
        entries_of(&app, 1),
        [(range(5 * MINUTE, Some(8 * MINUTE)), Some("掏耳".to_owned()))]
    );

    // 最后一个跳转点到文件结尾；光标先落在上次加过的列表上。
    app.dispatch(Action::SelectRow(2));
    press(&mut app, key(KeyCode::Char('a')));
    assert_eq!(app.view.playlist_selected, 1);
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(
        app.view.message.as_deref(),
        Some("已加进“睡前”：12:00 的片段")
    );
    assert_eq!(entries_of(&app, 1)[1], (range(8 * MINUTE, None), None));

    // Esc 不加，回到跳转点面板。
    press(&mut app, key(KeyCode::Char('a')));
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(entries_of(&app, 1).len(), 2);

    // 在这里新建一个列表，直接加进去；新建时按 Esc 回到选列表。
    app.dispatch(Action::SelectRow(0));
    press(&mut app, key(KeyCode::Char('a')));
    press(&mut app, key(KeyCode::Char('c')));
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::PickPlaylist);
    press(&mut app, key(KeyCode::Char('c')));
    type_text(&mut app, "午睡");
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::Marks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("已加进“午睡”：3:00 的片段")
    );
    let saved = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    let names: Vec<&str> = saved.all().iter().map(|list| list.name.as_str()).collect();
    assert_eq!(names, ["午睡", "白天", "睡前"]);
    assert_eq!(
        saved.all()[0].entries[0].range,
        range(2 * MINUTE, Some(5 * MINUTE))
    );
}

#[test]
fn a_clip_added_while_playing_a_clip_may_reach_past_its_end() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[5, 8, 12], MINUTE);
    add_clips(&mut app, &[(0, 5, Some(10), None)]);
    play_from_playlist(&mut app, 0);

    // 面板只列片段里的 5:00 和 8:00；8:00 之后的下一个跳转点在片段之外。
    press(&mut app, key(KeyCode::Char('B')));
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('a')));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(
        entries_of(&app, 0)[1],
        (range(8 * MINUTE, Some(12 * MINUTE)), None)
    );
    // 加到正在播放的列表里，当前条目不变。
    assert_eq!(scope(&app), in_list(Some(1), 2));
}

#[test]
fn the_entries_panel_moves_renames_and_picks_the_bounds_of_an_entry() {
    let (temp, mut app) = test_app(AppConfig::default());
    marks_fixture(&mut app, &temp.path().join("music"), &[2, 5, 8], MINUTE);
    name_mark(&mut app, 1, "掏耳");
    add_clips(&mut app, &[(0, 0, None, None), (2, 0, None, None)]);
    press(&mut app, key(KeyCode::Char('P')));
    press(&mut app, key(KeyCode::Enter));

    // J / K 移动，选择跟着走。
    press(&mut app, key(KeyCode::Char('J')));
    assert_eq!(app.view.playlist_track_selected, 1);
    assert_eq!(
        app.playlists.all()[0].entries[1].path,
        app.catalog.tracks()[0].path
    );
    press(&mut app, key(KeyCode::Char('J')));
    assert_eq!(app.view.playlist_track_selected, 1);
    press(&mut app, key(KeyCode::Char('K')));
    assert_eq!(app.view.playlist_track_selected, 0);
    assert_eq!(
        app.playlists.all()[0].entries[0].path,
        app.catalog.tracks()[0].path
    );

    // 改名，留空清除。
    press(&mut app, key(KeyCode::Char('e')));
    type_text(&mut app, "全集");
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    assert_eq!(entries_of(&app, 0)[0].1.as_deref(), Some("全集"));
    press(&mut app, key(KeyCode::Char('e')));
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(entries_of(&app, 0)[0].1, None);

    // 开头从文件开头和跳转点里挑，光标落在现在的开头上。
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(app.view.overlay, Overlay::EntryBound);
    let starts: Vec<Option<Duration>> = app
        .bound_choices()
        .iter()
        .map(|choice| choice.position)
        .collect();
    assert_eq!(
        starts,
        [
            Some(Duration::ZERO),
            Some(2 * MINUTE),
            Some(5 * MINUTE),
            Some(8 * MINUTE)
        ]
    );
    assert_eq!(app.view.bound_selected, 0);
    app.dispatch(Action::SelectRow(2));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    assert_eq!(app.view.message.as_deref(), Some("开头改为 5:00 掏耳"));

    // 结尾是开头之后的跳转点和文件结尾。
    press(&mut app, key(KeyCode::Char('S')));
    let ends: Vec<Option<Duration>> = app
        .bound_choices()
        .iter()
        .map(|choice| choice.position)
        .collect();
    assert_eq!(ends, [Some(8 * MINUTE), None]);
    assert_eq!(app.view.bound_selected, 1);
    press(&mut app, key(KeyCode::Char('k')));
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.message.as_deref(), Some("结尾改为 8:00"));
    assert_eq!(
        entries_of(&app, 0)[0].0,
        range(5 * MINUTE, Some(8 * MINUTE))
    );

    // 开头只能挑结尾之前的；Esc 不改。
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(app.bound_choices().len(), 3);
    assert_eq!(app.view.bound_selected, 2);
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    let saved = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(
        saved.all()[0].entries[0].range,
        range(5 * MINUTE, Some(8 * MINUTE))
    );

    // 短曲目没有跳转点，只能整首。
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('S')));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("这首没有跳转点，只能整首播放")
    );
}

#[test]
fn a_clip_can_be_reset_to_the_whole_file_after_its_marks_are_deleted() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    marks_fixture(&mut app, &music, &[5, 15], MINUTE);
    add_clips(&mut app, &[(0, 5, Some(15), None)]);
    let _ = app
        .playlists
        .add_entry(
            0,
            &track(music.join("gone.wav")),
            range(5 * MINUTE, Some(15 * MINUTE)),
            None,
        )
        .unwrap();
    // 删光这条录音的跳转点，列表里的片段还是 5:00–15:00。
    press(&mut app, key(KeyCode::Char('B')));
    press(&mut app, key(KeyCode::Char('d')));
    press(&mut app, key(KeyCode::Char('d')));
    press(&mut app, key(KeyCode::Esc));
    assert!(app.current_marks().is_empty());
    press(&mut app, key(KeyCode::Char('P')));
    press(&mut app, key(KeyCode::Enter));

    // 开头只剩文件开头可挑。
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(app.view.overlay, Overlay::EntryBound);
    let starts: Vec<Option<Duration>> = app
        .bound_choices()
        .iter()
        .map(|choice| choice.position)
        .collect();
    assert_eq!(starts, [Some(Duration::ZERO)]);
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.message.as_deref(), Some("开头改为 0:00"));
    assert_eq!(
        entries_of(&app, 0)[0].0,
        range(Duration::ZERO, Some(15 * MINUTE))
    );

    // 开头已经是 0:00，再按 s 只提示；结尾还能改回文件结尾。
    press(&mut app, key(KeyCode::Char('s')));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("这首没有跳转点，开头只能是 0:00")
    );
    press(&mut app, key(KeyCode::Char('S')));
    assert_eq!(app.view.overlay, Overlay::EntryBound);
    assert_eq!(app.bound_choices().len(), 1);
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(app.view.message.as_deref(), Some("结尾改为 文件结尾"));
    assert_eq!(entries_of(&app, 0)[0].0, crate::playlist::Range::WHOLE);
    let saved = crate::playlist::PlaylistStore::load(app.paths.playlists_dir.clone()).unwrap();
    assert_eq!(
        saved.all()[0].entries[0].range,
        crate::playlist::Range::WHOLE
    );

    // 整首了就和以前一样只提示。
    press(&mut app, key(KeyCode::Char('S')));
    assert_eq!(app.view.overlay, Overlay::PlaylistTracks);
    assert_eq!(
        app.view.message.as_deref(),
        Some("这首没有跳转点，只能整首播放")
    );

    // 文件不在曲库中的片段也能改回去，标题用文件名。
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('S')));
    assert_eq!(app.view.overlay, Overlay::EntryBound);
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("结尾·gone.wav"), "{screen}");
    assert!(screen.contains("文件结尾"), "{screen}");
    press(&mut app, key(KeyCode::Enter));
    assert_eq!(entries_of(&app, 0)[1].0, range(5 * MINUTE, None));
}

#[test]
fn moving_entries_of_the_playing_playlist_keeps_the_current_entry() {
    let (temp, mut app, control) = test_app_with_control(AppConfig::default());
    let paths = playlist_fixture(temp.path(), &mut app, &["a.wav", "b.wav", "c.wav"], 3);
    play_from_playlist(&mut app, 1);

    press(&mut app, key(KeyCode::Char('P')));
    press(&mut app, key(KeyCode::Enter));
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('K')));
    press(&mut app, key(KeyCode::Esc));
    press(&mut app, key(KeyCode::Esc));
    assert_eq!(scope(&app), in_list(Some(1), 3));
    finish(&mut app, &control);
    assert_eq!(playing(&app), paths[0]);
}

#[test]
fn the_playlist_panels_show_counts_modes_ranges_and_the_playing_entry() {
    let (temp, mut app) = test_app(AppConfig::default());
    let music = temp.path().join("music");
    recordings_fixture(&mut app, &music);
    add_clips(
        &mut app,
        &[(0, 5, Some(15), Some("掏耳")), (2, 0, None, None)],
    );
    let _ = app
        .playlists
        .add_entry(
            0,
            &track(music.join("gone.wav")),
            crate::playlist::Range::WHOLE,
            None,
        )
        .unwrap();
    play_from_playlist(&mut app, 0);

    press(&mut app, key(KeyCode::Char('P')));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("▸●睡前3条·顺序"), "{screen}");

    press(&mut app, key(KeyCode::Enter));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    for expected in [
        "睡前·3条·顺序",
        "▸●1.rec1·掏耳5:00–15:00",
        "2.song1:40",
        "⚠3.gone.wav不在曲库中",
        "Enter从此处播放·J/K移动·s/S开头/结尾·e改名·d移除·Esc返回",
    ] {
        assert!(screen.contains(expected), "缺少“{expected}”：\n{screen}");
    }

    // 整首的条目没有跳转点：只提示，不打开选择面板。截过的片段还能改回去，见
    // a_clip_can_be_reset_to_the_whole_file_after_its_marks_are_deleted。
    app.dispatch(Action::SelectRow(1));
    press(&mut app, key(KeyCode::Char('S')));
    let (_, backend) = render(&app, 100, 30);
    let screen = screen_rows(&backend).join("\n");
    assert!(screen.contains("这首没有跳转点"), "{screen}");
}
