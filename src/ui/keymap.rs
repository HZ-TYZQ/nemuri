//! 终端按键到 [`Action`] 的映射。
//!
//! 每个界面一张表，表里每一行同时写明显示用的按键、说明文字和实际绑定。
//! 帮助弹层、底栏和弹层底部的提示都从这些表生成，改键时不会和说明脱节。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use crate::app::{Action, App, Overlay};

const SHORT_SEEK_MICROS: i64 = 10_000_000;
const LONG_SEEK_MICROS: i64 = 60_000_000;
/// 帮助弹层里说明文字开始的列。
const HELP_KEY_COLUMN: usize = 15;

#[derive(Debug, Clone, Copy)]
pub(super) enum Key {
    /// 不看修饰键。
    Any(KeyCode),
    /// 必须按着 Shift。
    Shift(KeyCode),
    /// 不能按着 Shift。
    NoShift(KeyCode),
}

impl Key {
    fn matches(self, event: KeyEvent) -> bool {
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        match self {
            Self::Any(code) => event.code == code,
            Self::Shift(code) => shift && event.code == code,
            Self::NoShift(code) => !shift && event.code == code,
        }
    }
}

pub(super) struct Binding {
    /// 显示用的按键写法；为空时不列入帮助和提示。
    pub(super) keys: &'static str,
    pub(super) text: &'static str,
    /// 主界面底栏里的简写，只有曲库表用到。
    pub(super) hint: Option<&'static str>,
    /// 按顺序匹配，第一条命中的生效。
    pub(super) map: &'static [(Key, Action)],
}

const fn listed(keys: &'static str, text: &'static str, map: &'static [(Key, Action)]) -> Binding {
    Binding {
        keys,
        text,
        hint: None,
        map,
    }
}

const fn hinted(
    keys: &'static str,
    text: &'static str,
    hint: &'static str,
    map: &'static [(Key, Action)],
) -> Binding {
    Binding {
        keys,
        text,
        hint: Some(hint),
        map,
    }
}

const fn hidden(map: &'static [(Key, Action)]) -> Binding {
    Binding {
        keys: "",
        text: "",
        hint: None,
        map,
    }
}

const fn ch(character: char) -> Key {
    Key::Any(KeyCode::Char(character))
}

const fn code(code: KeyCode) -> Key {
    Key::Any(code)
}

const CURSOR: &[(Key, Action)] = &[
    (code(KeyCode::Down), Action::CursorDown),
    (ch('j'), Action::CursorDown),
    (code(KeyCode::Up), Action::CursorUp),
    (ch('k'), Action::CursorUp),
];

pub(super) const LIBRARY: &[Binding] = &[
    hinted("↑/↓ 或 j/k", "选择歌曲", "↑↓/jk 选择", CURSOR),
    hinted(
        "Enter",
        "立即播放（保留队列）",
        "Enter 播放",
        &[(code(KeyCode::Enter), Action::Activate)],
    ),
    hinted(
        "Space",
        "暂停 / 继续",
        "Space 暂停",
        &[(ch(' '), Action::TogglePause)],
    ),
    listed(
        "←/→ 或 h/l",
        "后退 / 前进 10 秒",
        &[
            (
                Key::NoShift(KeyCode::Left),
                Action::SeekBy(-SHORT_SEEK_MICROS),
            ),
            (
                Key::NoShift(KeyCode::Right),
                Action::SeekBy(SHORT_SEEK_MICROS),
            ),
            (ch('h'), Action::SeekBy(-SHORT_SEEK_MICROS)),
            (ch('l'), Action::SeekBy(SHORT_SEEK_MICROS)),
        ],
    ),
    // 部分终端不区分 Shift+方向键，H/L 始终可用。
    listed(
        "H/L、Shift+←/→",
        "后退 / 前进 60 秒",
        &[
            (Key::Shift(KeyCode::Left), Action::SeekBy(-LONG_SEEK_MICROS)),
            (Key::Shift(KeyCode::Right), Action::SeekBy(LONG_SEEK_MICROS)),
            (ch('H'), Action::SeekBy(-LONG_SEEK_MICROS)),
            (ch('L'), Action::SeekBy(LONG_SEEK_MICROS)),
        ],
    ),
    listed(
        "0–9",
        "跳到 0% – 90% 位置",
        &[
            (ch('0'), Action::SeekToRatio(0.0)),
            (ch('1'), Action::SeekToRatio(0.1)),
            (ch('2'), Action::SeekToRatio(0.2)),
            (ch('3'), Action::SeekToRatio(0.3)),
            (ch('4'), Action::SeekToRatio(0.4)),
            (ch('5'), Action::SeekToRatio(0.5)),
            (ch('6'), Action::SeekToRatio(0.6)),
            (ch('7'), Action::SeekToRatio(0.7)),
            (ch('8'), Action::SeekToRatio(0.8)),
            (ch('9'), Action::SeekToRatio(0.9)),
        ],
    ),
    listed(
        "- / = · [ / ]",
        "音量 ±5% / ±1%",
        &[
            (ch('-'), Action::ChangeVolume(-5)),
            (ch('='), Action::ChangeVolume(5)),
            (ch('+'), Action::ChangeVolume(5)),
            (ch('['), Action::ChangeVolume(-1)),
            (ch(']'), Action::ChangeVolume(1)),
        ],
    ),
    listed("m", "静音", &[(ch('m'), Action::ToggleMute)]),
    listed(
        "n / p",
        "下一首 / 上一首历史",
        &[(ch('n'), Action::Next), (ch('p'), Action::Previous)],
    ),
    listed(
        "z",
        "循环方式：播完停止 / 顺序 / 列表 / 单曲",
        &[(ch('z'), Action::CycleRepeat)],
    ),
    listed("s", "开 / 关随机播放", &[(ch('s'), Action::ToggleShuffle)]),
    hinted(
        "t",
        "睡眠定时：关 / 15–120 分钟",
        "t 定时",
        &[(ch('t'), Action::CycleSleepTimer)],
    ),
    hinted(
        "v / y",
        "显示 / 隐藏频谱 / 歌词",
        "v 频谱",
        &[
            (ch('v'), Action::ToggleVisualizer),
            (ch('y'), Action::ToggleLyrics),
        ],
    ),
    hinted(
        "o / O",
        "排序字段 / 升降序",
        "o 排序",
        &[
            (ch('o'), Action::CycleSort),
            (ch('O'), Action::ToggleSortDirection),
        ],
    ),
    listed("M", "开 / 关鼠标", &[(ch('M'), Action::ToggleMouse)]),
    hinted(
        "/",
        "实时模糊搜索",
        "/ 搜索",
        &[(ch('/'), Action::StartSearch)],
    ),
    hinted(
        "f",
        "按文件夹筛选曲库",
        "f 文件夹",
        &[(ch('f'), Action::OpenOverlay(Overlay::Folders))],
    ),
    listed("r", "后台重新扫描", &[(ch('r'), Action::Rescan)]),
    listed(
        "a / A",
        "加到队尾 / 设为下一首",
        &[(ch('a'), Action::Enqueue), (ch('A'), Action::EnqueueNext)],
    ),
    hinted(
        "Q",
        "播放队列",
        "Q 队列",
        &[(ch('Q'), Action::OpenOverlay(Overlay::Queue))],
    ),
    hinted(
        "P",
        "播放列表",
        "P 列表",
        &[(ch('P'), Action::OpenOverlay(Overlay::Playlists))],
    ),
    // 帮助键写在帮助弹层的标题里，不占列表行。
    Binding {
        keys: "",
        text: "",
        hint: Some("? 帮助"),
        map: &[(ch('?'), Action::OpenOverlay(Overlay::Help))],
    },
    hinted("q", "退出", "q 退出", &[(ch('q'), Action::Quit)]),
    hidden(&[(code(KeyCode::Esc), Action::Back)]),
];

/// 搜索框里没有绑定的字符都当作输入。
pub(super) const SEARCH: &[Binding] = &[
    listed("Enter", "播放", &[(code(KeyCode::Enter), Action::Activate)]),
    listed("Esc", "清除", &[(code(KeyCode::Esc), Action::Back)]),
    hidden(&[
        (code(KeyCode::Down), Action::CursorDown),
        (code(KeyCode::Up), Action::CursorUp),
        (code(KeyCode::Backspace), Action::InputBackspace),
    ]),
];

pub(super) const HELP: &[Binding] = &[listed(
    "? / Esc",
    "关闭",
    &[(ch('?'), Action::Back), (code(KeyCode::Esc), Action::Back)],
)];

pub(super) const PLAYLISTS: &[Binding] = &[
    hidden(CURSOR),
    listed("c", "新建", &[(ch('c'), Action::NewPlaylist)]),
    listed("a", "加入选中歌曲", &[(ch('a'), Action::AddToPlaylist)]),
    listed("Enter", "查看", &[(code(KeyCode::Enter), Action::Activate)]),
    listed("x", "删除", &[(ch('x'), Action::DeletePlaylist)]),
    listed(
        "Esc",
        "关闭",
        &[(code(KeyCode::Esc), Action::Back), (ch('P'), Action::Back)],
    ),
];

pub(super) const PLAYLIST_TRACKS: &[Binding] = &[
    hidden(CURSOR),
    listed(
        "Enter",
        "从此处播放",
        &[(code(KeyCode::Enter), Action::Activate)],
    ),
    listed("d", "从列表移除", &[(ch('d'), Action::RemoveFromPlaylist)]),
    listed("Esc", "返回", &[(code(KeyCode::Esc), Action::Back)]),
];

pub(super) const QUEUE: &[Binding] = &[
    hidden(CURSOR),
    listed(
        "Enter",
        "跳到此处播放",
        &[(code(KeyCode::Enter), Action::Activate)],
    ),
    listed("d", "移除", &[(ch('d'), Action::RemoveFromQueue)]),
    listed(
        "J/K",
        "上下移动",
        &[
            (ch('J'), Action::MoveInQueue(1)),
            (ch('K'), Action::MoveInQueue(-1)),
        ],
    ),
    listed("c", "清空", &[(ch('c'), Action::ClearQueue)]),
    listed(
        "Esc",
        "关闭",
        &[(code(KeyCode::Esc), Action::Back), (ch('Q'), Action::Back)],
    ),
];

pub(super) const FOLDERS: &[Binding] = &[
    hidden(CURSOR),
    listed(
        "Enter",
        "只看此文件夹",
        &[(code(KeyCode::Enter), Action::Activate)],
    ),
    listed(
        "Esc",
        "关闭",
        &[(code(KeyCode::Esc), Action::Back), (ch('f'), Action::Back)],
    ),
];

/// 输入框里没有绑定的字符都当作输入。
pub(super) const NAME_INPUT: &[Binding] = &[
    listed("Enter", "创建", &[(code(KeyCode::Enter), Action::Activate)]),
    listed("Esc", "取消", &[(code(KeyCode::Esc), Action::Back)]),
    hidden(&[(code(KeyCode::Backspace), Action::InputBackspace)]),
];

pub(super) const DELETE_CONFIRM: &[Binding] = &[
    listed(
        "y",
        "确认",
        &[
            (ch('y'), Action::ConfirmDelete),
            (ch('Y'), Action::ConfirmDelete),
        ],
    ),
    listed(
        "n/Esc",
        "取消",
        &[
            (ch('n'), Action::Back),
            (ch('N'), Action::Back),
            (code(KeyCode::Esc), Action::Back),
        ],
    ),
];

/// 当前焦点的按键表，以及没有绑定的字符是否算文字输入。
/// 弹层打开时只看弹层的表，按键不会漏到下面的曲库。
fn bindings_for(app: &App) -> (&'static [Binding], bool) {
    match app.view().overlay {
        Overlay::None if app.view().search_active => (SEARCH, true),
        Overlay::None => (LIBRARY, false),
        Overlay::Help => (HELP, false),
        Overlay::Playlists => (PLAYLISTS, false),
        Overlay::PlaylistTracks => (PLAYLIST_TRACKS, false),
        Overlay::Queue => (QUEUE, false),
        Overlay::Folders => (FOLDERS, false),
        Overlay::NameInput => (NAME_INPUT, true),
        Overlay::DeleteConfirm => (DELETE_CONFIRM, false),
    }
}

/// 这次按键在当前焦点下对应的操作；没有绑定时为 None。
pub fn key_action(app: &App, event: KeyEvent) -> Option<Action> {
    let (bindings, text_input) = bindings_for(app);
    lookup(bindings, text_input, event)
}

pub(super) fn lookup(bindings: &[Binding], text_input: bool, event: KeyEvent) -> Option<Action> {
    bindings
        .iter()
        .flat_map(|binding| binding.map)
        .find(|(key, _)| key.matches(event))
        .map(|(_, action)| action.clone())
        .or(match event.code {
            KeyCode::Char(character) if text_input => Some(Action::InputChar(character)),
            _ => None,
        })
}

/// 弹层底部那一行提示：列出的绑定按表中顺序，用 “ · ” 连接。
pub(super) fn hints(bindings: &[Binding]) -> String {
    bindings
        .iter()
        .filter(|binding| !binding.keys.is_empty())
        .map(|binding| format!("{} {}", binding.keys, binding.text))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 主界面底栏的简写提示。
pub(super) fn library_hints() -> String {
    LIBRARY
        .iter()
        .filter_map(|binding| binding.hint)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 帮助弹层的每一行：按键左对齐到固定列，后接说明。
pub(super) fn help_lines() -> Vec<String> {
    LIBRARY
        .iter()
        .filter(|binding| !binding.keys.is_empty())
        .map(|binding| {
            let padding = HELP_KEY_COLUMN
                .saturating_sub(UnicodeWidthStr::width(binding.keys))
                .max(1);
            format!("{}{}{}", binding.keys, " ".repeat(padding), binding.text)
        })
        .collect()
}
