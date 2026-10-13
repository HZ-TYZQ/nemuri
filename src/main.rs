use std::io::{self, stdout};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;

use music_player::app::App;
use music_player::cli::{Cli, validate_directory};
use music_player::config::{self, AppConfig, AppPaths, LoadedConfig};
use music_player::media::{MediaEvent, MediaSession};
use music_player::ui::{self, MouseInput, ViewLayout};

fn main() -> ExitCode {
    match run_application() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("music-player: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_application() -> Result<(), String> {
    let cli = Cli::parse();
    let paths = AppPaths::discover().map_err(|error| format!("无法确定用户数据目录: {error}"))?;
    let LoadedConfig {
        mut config,
        message: mut warning,
        writable,
    } = AppConfig::load(&paths)
        .map_err(|error| format!("无法读取配置 {}: {error}", paths.config_file.display()))?;

    let library_dir = if let Some(path) = cli.set_library {
        let path = validate_directory(&path).map_err(|error| error.to_string())?;
        config::set_library(&paths.config_file, &path)?;
        config.library_dir = Some(path.clone());
        warning = Some(format!("主音乐库已设置为 {}", path.display()));
        path
    } else if let Some(path) = cli.directory {
        validate_directory(&path).map_err(|error| error.to_string())?
    } else if let Some(path) = config.library_dir.as_ref() {
        validate_directory(path).map_err(|error| {
            format!("配置的主音乐库不可用: {error}。请使用 --set-library PATH 设置新目录")
        })?
    } else {
        let path = paths.default_music_dir.as_ref().ok_or_else(|| {
            "系统没有提供默认 Music 目录，请使用 --set-library PATH 设置主音乐库".to_owned()
        })?;
        validate_directory(path).map_err(|error| {
            format!("默认 Music 目录不可用: {error}。请使用 --set-library PATH 设置主音乐库")
        })?
    };

    // 播放器、播放列表目录及工作线程都在切换终端模式前初始化。
    // 这样启动失败时错误仍是普通、可复制的终端文本。
    let mut app = App::new(library_dir, paths, config, warning, writable)?;
    let session = match MediaSession::start() {
        Ok(session) => Some(session),
        Err(error) => {
            app.notify_if_idle(error);
            None
        }
    };
    let mut terminal = setup_terminal().map_err(|error| format!("无法初始化终端: {error}"))?;
    let run_result = run(&mut terminal, &mut app, session.as_ref())
        .map_err(|error| format!("终端运行失败: {error}"));
    let restore_result =
        restore_terminal(&mut terminal).map_err(|error| format!("无法恢复终端状态: {error}"));
    let save_result = app.save_settings();
    // 会话只是便利功能，保存失败不应让整个程序以失败退出。
    if let Err(error) = app.save_session() {
        eprintln!("music-player: {error}");
    }

    run_result?;
    restore_result?;
    save_result?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    session: Option<&MediaSession>,
) -> io::Result<()> {
    let mut view = ViewLayout::default();
    let mut mouse = MouseInput::default();
    let mut mouse_captured = false;
    while !app.should_quit() {
        if app.config().mouse_enabled != mouse_captured {
            set_mouse_capture(app.config().mouse_enabled)?;
            mouse_captured = app.config().mouse_enabled;
        }
        terminal.draw(|frame| ui::draw(frame, app, &mut view))?;
        // 频谱开启时把主循环提到约 50Hz，与 spectrum 源 20ms interval 对齐；
        // 关闭时回到 50ms 降低空闲唤醒。
        let poll_ms = if app.config().visualizer_enabled {
            20
        } else {
            50
        };
        if event::poll(Duration::from_millis(poll_ms))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = ui::key_action(app, key) {
                        app.dispatch(action);
                    }
                }
                // 鼠标捕获会连移动一起上报，这里直接丢弃，避免空闲时被持续唤醒。
                // 关闭鼠标后即便仍有序列送达（例如被上游注入），也不再处理。
                Event::Mouse(event)
                    if mouse_captured && !matches!(event.kind, MouseEventKind::Moved) =>
                {
                    for action in mouse.actions(event, &view, app) {
                        app.dispatch(action);
                    }
                }
                _ => {}
            }
        }
        if let Some(session) = session {
            for command in session.try_recv() {
                app.dispatch(command.into());
            }
        }
        app.on_tick(Instant::now());
        if let Some(session) = session {
            session.publish(app.media_snapshot());
            for event in app.drain_media_events() {
                let MediaEvent::Seeked { position } = event;
                session.notify_seeked(position);
            }
        }
    }
    if mouse_captured {
        set_mouse_capture(false)?;
    }
    Ok(())
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(stdout(), EnableMouseCapture)
    } else {
        execute!(stdout(), DisableMouseCapture)
    }
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut output = stdout();
    execute!(output, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(output))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()
}
