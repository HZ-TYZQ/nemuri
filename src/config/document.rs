//! `config.kdl` 的读取与写回。
//!
//! 读取时逐项检查：写错的项只忽略那一项，并记下行号提示用户。
//! 写回时在解析出的文档上原地修改，注释、空行和缩进都保持原样；
//! 改完再读一遍核对，读回来的不是要写的值就宁可不写。

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kdl::{
    KdlDocument, KdlDocumentFormat, KdlEntry, KdlEntryFormat, KdlError, KdlNode, KdlNodeFormat,
    KdlValue,
};
use ratatui::style::Color;

use crate::theme::{PRESET_NAMES, Theme};
use crate::track::{RepeatMode, SortKey, SortOrder};

use super::{AppConfig, LoadedConfig};

const FILE: &str = "config.kdl";
const INDENT: &str = "    ";

const REPEAT_MODES: [(&str, RepeatMode); 4] = [
    ("stop", RepeatMode::Stop),
    ("none", RepeatMode::None),
    ("all", RepeatMode::All),
    ("one", RepeatMode::One),
];

const SORT_KEYS: [(&str, SortKey); 5] = [
    ("path", SortKey::Path),
    ("title", SortKey::Title),
    ("artist", SortKey::Artist),
    ("album", SortKey::Album),
    ("duration", SortKey::Duration),
];

pub(super) fn repeat_name(mode: RepeatMode) -> &'static str {
    name_of(&REPEAT_MODES, mode)
}

pub(super) fn sort_key_name(key: SortKey) -> &'static str {
    name_of(&SORT_KEYS, key)
}

fn name_of<T: PartialEq>(choices: &[(&'static str, T)], value: T) -> &'static str {
    choices
        .iter()
        .find(|(_, choice)| *choice == value)
        .map_or("", |(name, _)| name)
}

/// KDL 双引号字符串的写法。
pub(super) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

pub(super) fn read(text: &str) -> LoadedConfig {
    let document = match KdlDocument::parse(text) {
        Ok(document) => document,
        Err(error) => {
            return LoadedConfig {
                config: AppConfig::default(),
                message: Some(format!(
                    "{}；本次使用默认设置，退出时不会改动配置文件",
                    syntax_error(text, &error)
                )),
                writable: false,
            };
        }
    };
    let mut reader = Reader {
        text,
        problems: Vec::new(),
    };
    let config = reader.document(&document);
    let message = (!reader.problems.is_empty())
        .then(|| format!("{FILE} 有误: {}", reader.problems.join("；")));
    LoadedConfig {
        config,
        message,
        writable: true,
    }
}

fn syntax_error(text: &str, error: &KdlError) -> String {
    let Some(diagnostic) = error.diagnostics.first() else {
        return format!("{FILE} 有语法错误");
    };
    let offset = diagnostic.span.offset();
    let (line, column) = position(text, offset);
    let detail = diagnostic.message.as_deref().unwrap_or("无法解析");
    // KDL v1 和许多别的格式里布尔值不带 #，这是最容易写错的地方。
    let token = text.get(offset..offset + diagnostic.span.len());
    let hint = if matches!(token, Some("true" | "false" | "null")) {
        "（KDL v2 里要写成 #true、#false、#null）"
    } else {
        ""
    };
    format!("{FILE} 第 {line} 行第 {column} 列有语法错误: {detail}{hint}")
}

/// 字节偏移对应的行号和列号（列按字符数算），都从 1 开始。
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    (line, column)
}

fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map_or(&[], KdlDocument::nodes)
}

fn name(node: &KdlNode) -> &str {
    node.name().value()
}

/// 值在文件里的原样写法，用在提示里。
fn shown(entry: &KdlEntry) -> String {
    entry.format().map_or_else(
        || entry.value().to_string(),
        |format| format.value_repr.clone(),
    )
}

struct Reader<'t> {
    text: &'t str,
    problems: Vec<String>,
}

impl Reader<'_> {
    fn problem(&mut self, node: &KdlNode, what: impl Display) {
        let (line, _) = position(self.text, node.span().offset());
        self.problems.push(format!("第 {line} 行 {what}"));
    }

    fn unknown(&mut self, node: &KdlNode) {
        self.problem(node, format_args!("未知项 {}，已忽略", name(node)));
    }

    fn document(&mut self, document: &KdlDocument) -> AppConfig {
        let mut config = AppConfig::default();
        for node in document.nodes() {
            match name(node) {
                "library" => {
                    if let Some(path) = self.string(node) {
                        config.library_dir = Some(expand_home(path));
                    }
                }
                "playback" => self.playback(node, &mut config),
                "sort" => self.sort(node, &mut config.sort),
                "interface" => self.interface(node, &mut config),
                "sleep" => self.sleep(node, &mut config),
                "theme" => config.theme = self.theme(node),
                _ => self.unknown(node),
            }
        }
        config
    }

    fn playback(&mut self, section: &KdlNode, config: &mut AppConfig) {
        for node in children(section) {
            match name(node) {
                "volume" => {
                    if let Some(volume) = self.volume(node) {
                        config.volume = volume;
                    }
                }
                "muted" => self.flag(node, &mut config.muted),
                "repeat" => {
                    if let Some(repeat) = self.choice(node, &REPEAT_MODES) {
                        config.repeat = repeat;
                    }
                }
                "shuffle" => self.flag(node, &mut config.shuffle),
                _ => self.unknown(node),
            }
        }
    }

    fn sort(&mut self, node: &KdlNode, sort: &mut SortOrder) {
        // 只写 descending 不写排序键也可以。
        if node.entries().iter().any(|entry| entry.name().is_none())
            && let Some(key) = self.choice(node, &SORT_KEYS)
        {
            sort.key = key;
        }
        for entry in node.entries() {
            let Some(property) = entry.name() else {
                continue;
            };
            match (property.value(), entry.value()) {
                ("descending", KdlValue::Bool(descending)) => sort.descending = *descending,
                ("descending", _) => self.problem(
                    node,
                    format_args!(
                        "descending 的值 {} 应为 #true 或 #false，已忽略",
                        shown(entry)
                    ),
                ),
                (other, _) => self.problem(node, format_args!("sort 没有 {other} 这一项，已忽略")),
            }
        }
    }

    fn interface(&mut self, section: &KdlNode, config: &mut AppConfig) {
        for node in children(section) {
            match name(node) {
                "visualizer" => self.flag(node, &mut config.visualizer_enabled),
                "lyrics" => self.flag(node, &mut config.lyrics_enabled),
                "mouse" => self.flag(node, &mut config.mouse_enabled),
                "privacy" => self.flag(node, &mut config.privacy),
                _ => self.unknown(node),
            }
        }
    }

    fn sleep(&mut self, section: &KdlNode, config: &mut AppConfig) {
        for node in children(section) {
            match name(node) {
                "fade" => {
                    if let Some(fade) = self.fade(node) {
                        config.sleep_fade = fade;
                    }
                }
                _ => self.unknown(node),
            }
        }
    }

    /// 淡出的秒数。上限 600 秒，比最短的 15 分钟定时短得多，淡出不会从按下就开始。
    fn fade(&mut self, node: &KdlNode) -> Option<Duration> {
        let entry = self.argument(node)?;
        let seconds = match entry.value() {
            KdlValue::Integer(value) => {
                u64::try_from(*value).ok().filter(|seconds| *seconds <= 600)
            }
            _ => None,
        };
        if seconds.is_none() {
            self.problem(
                node,
                format_args!("fade 的值 {} 应为 0–600 的整数（秒），已忽略", shown(entry)),
            );
        }
        seconds.map(Duration::from_secs)
    }

    /// 先取预设，再逐项覆盖。预设名或某项颜色写错时只回退那一处。
    fn theme(&mut self, section: &KdlNode) -> Theme {
        let nodes = children(section);
        let mut theme = Theme::default();
        if let Some(node) = nodes.iter().rfind(|node| name(node) == "preset")
            && let Some(preset) = self.string(node)
        {
            match Theme::preset(preset.trim()) {
                Some(found) => theme = found,
                None => self.problem(
                    node,
                    format_args!(
                        "未知的主题预设 \"{preset}\"（可选 {PRESET_NAMES}），已改用 default"
                    ),
                ),
            }
        }
        for node in nodes {
            let key = name(node);
            if key == "preset" {
                continue;
            }
            let Some(color) = theme.color_mut(key) else {
                self.unknown(node);
                continue;
            };
            let Some(value) = self.string(node) else {
                continue;
            };
            match value.trim().parse::<Color>() {
                Ok(parsed) => *color = parsed,
                Err(_) => self.problem(
                    node,
                    format_args!("{key} 的值 \"{value}\" 无法识别，已沿用预设"),
                ),
            }
        }
        theme
    }

    /// 节点的第一个参数；没有时记下问题。
    fn argument<'n>(&mut self, node: &'n KdlNode) -> Option<&'n KdlEntry> {
        let entry = node.entries().iter().find(|entry| entry.name().is_none());
        if entry.is_none() {
            self.problem(node, format_args!("{} 缺少值，已忽略", name(node)));
        }
        entry
    }

    fn string<'n>(&mut self, node: &'n KdlNode) -> Option<&'n str> {
        let entry = self.argument(node)?;
        match entry.value() {
            KdlValue::String(text) => Some(text),
            _ => {
                self.problem(
                    node,
                    format_args!(
                        "{} 的值 {} 应为加双引号的字符串，已忽略",
                        name(node),
                        shown(entry)
                    ),
                );
                None
            }
        }
    }

    fn flag(&mut self, node: &KdlNode, target: &mut bool) {
        let Some(entry) = self.argument(node) else {
            return;
        };
        match entry.value() {
            KdlValue::Bool(value) => *target = *value,
            _ => self.problem(
                node,
                format_args!(
                    "{} 的值 {} 应为 #true 或 #false，已忽略",
                    name(node),
                    shown(entry)
                ),
            ),
        }
    }

    fn volume(&mut self, node: &KdlNode) -> Option<u8> {
        let entry = self.argument(node)?;
        let volume = match entry.value() {
            KdlValue::Integer(value) => u8::try_from(*value).ok().filter(|volume| *volume <= 100),
            _ => None,
        };
        if volume.is_none() {
            self.problem(
                node,
                format_args!("volume 的值 {} 应为 0–100 的整数，已忽略", shown(entry)),
            );
        }
        volume
    }

    fn choice<T: Copy>(&mut self, node: &KdlNode, choices: &[(&str, T)]) -> Option<T> {
        let value = self.string(node)?;
        let found = choices
            .iter()
            .find(|(choice, _)| choice.eq_ignore_ascii_case(value.trim()))
            .map(|&(_, choice)| choice);
        if found.is_none() {
            let names: Vec<_> = choices.iter().map(|(choice, _)| quoted(choice)).collect();
            self.problem(
                node,
                format_args!(
                    "{} 的值 \"{value}\" 应为 {} 之一，已忽略",
                    name(node),
                    names.join("、")
                ),
            );
        }
        found
    }
}

/// `~` 或 `~/` 开头的路径展开到家目录。
fn expand_home(path: &str) -> PathBuf {
    let rest = if path == "~" {
        Some("")
    } else {
        path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
    };
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// 运行中可能改动、退出时要写回的一项设置。
enum Edit {
    /// `section { key value }`
    Child {
        section: &'static str,
        key: &'static str,
        value: KdlValue,
    },
    /// `sort "key"` 的参数
    SortKey(SortKey),
    /// `sort descending=#bool` 的属性
    SortDescending(bool),
}

impl Edit {
    fn id(&self) -> (&'static str, &'static str) {
        match self {
            Self::Child { section, key, .. } => (section, key),
            Self::SortKey(_) => ("sort", "key"),
            Self::SortDescending(_) => ("sort", "descending"),
        }
    }
}

fn edits(start: &AppConfig, now: &AppConfig) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut child = |changed: bool, section, key, value: KdlValue| {
        if changed {
            edits.push(Edit::Child {
                section,
                key,
                value,
            });
        }
    };
    child(
        start.volume != now.volume,
        "playback",
        "volume",
        i128::from(now.volume).into(),
    );
    child(
        start.muted != now.muted,
        "playback",
        "muted",
        now.muted.into(),
    );
    child(
        start.repeat != now.repeat,
        "playback",
        "repeat",
        repeat_name(now.repeat).into(),
    );
    child(
        start.shuffle != now.shuffle,
        "playback",
        "shuffle",
        now.shuffle.into(),
    );
    child(
        start.visualizer_enabled != now.visualizer_enabled,
        "interface",
        "visualizer",
        now.visualizer_enabled.into(),
    );
    child(
        start.lyrics_enabled != now.lyrics_enabled,
        "interface",
        "lyrics",
        now.lyrics_enabled.into(),
    );
    child(
        start.mouse_enabled != now.mouse_enabled,
        "interface",
        "mouse",
        now.mouse_enabled.into(),
    );
    child(
        start.privacy != now.privacy,
        "interface",
        "privacy",
        now.privacy.into(),
    );
    if start.sort.key != now.sort.key {
        edits.push(Edit::SortKey(now.sort.key));
    }
    if start.sort.descending != now.sort.descending {
        edits.push(Edit::SortDescending(now.sort.descending));
    }
    edits
}

pub(super) fn has_changes(start: &AppConfig, now: &AppConfig) -> bool {
    !edits(start, now).is_empty()
}

pub(super) fn write_changes(
    text: &str,
    start: &AppConfig,
    now: &AppConfig,
) -> Result<String, String> {
    let mut document = parse_for_writing(text, "本次运行中改过的设置没有写回")?;
    let edits = edits(start, now);
    for edit in &edits {
        match edit {
            Edit::Child {
                section,
                key,
                value,
            } => {
                let section = top_node(&mut document, section);
                set_argument(child_node(section, key), value.clone());
            }
            Edit::SortKey(key) => {
                set_argument(top_node(&mut document, "sort"), sort_key_name(*key).into());
            }
            Edit::SortDescending(descending) => {
                let node = top_node(&mut document, "sort");
                set_property(node, "descending", (*descending).into());
            }
        }
    }
    let updated = document.to_string();
    let reread = read(&updated);
    let applied: Vec<_> = edits.iter().map(Edit::id).collect();
    let settled = reread.writable
        && !self::edits(&reread.config, now)
            .iter()
            .any(|edit| applied.contains(&edit.id()));
    if !settled {
        return Err(format!("写回 {FILE} 时出错，文件没有改动"));
    }
    Ok(updated)
}

pub(super) fn set_library(text: &str, library: &Path) -> Result<String, String> {
    let path = library.to_str().ok_or_else(|| {
        format!(
            "音乐库路径 {} 不是 UTF-8，无法写入 {FILE}",
            library.display()
        )
    })?;
    let mut document = parse_for_writing(text, "无法写入主音乐库")?;
    set_argument(top_node(&mut document, "library"), path.into());
    let updated = document.to_string();
    if read(&updated).config.library_dir.as_deref() != Some(library) {
        return Err(format!("写入 {FILE} 时出错，文件没有改动"));
    }
    Ok(updated)
}

fn parse_for_writing(text: &str, consequence: &str) -> Result<KdlDocument, String> {
    KdlDocument::parse(text)
        .map_err(|error| format!("{}，{consequence}", syntax_error(text, &error)))
}

/// 最后一个叫 `name` 的顶层节点（读取时也是后面的覆盖前面的）；没有就追加到文件末尾。
fn top_node<'d>(document: &'d mut KdlDocument, name: &str) -> &'d mut KdlNode {
    if let Some(index) = document
        .nodes()
        .iter()
        .rposition(|node| self::name(node) == name)
    {
        return &mut document.nodes_mut()[index];
    }
    let leading = if document.nodes().is_empty() {
        ""
    } else {
        "\n"
    };
    start_new_line(document, false);
    let mut node = KdlNode::new(name);
    node.set_format(KdlNodeFormat {
        leading: leading.to_owned(),
        before_children: " ".to_owned(),
        terminator: "\n".to_owned(),
        ..KdlNodeFormat::default()
    });
    push(document, node)
}

/// 块里最后一个叫 `name` 的子节点；没有就追加到块的末尾，缩进跟上一项一致。
fn child_node<'s>(section: &'s mut KdlNode, name: &str) -> &'s mut KdlNode {
    if section.children().is_none() {
        let mut children = KdlDocument::new();
        children.set_format(KdlDocumentFormat {
            leading: "\n".to_owned(),
            trailing: String::new(),
        });
        *section.children_mut() = Some(children);
        if let Some(format) = section.format_mut()
            && format.before_children.is_empty()
        {
            format.before_children = " ".to_owned();
        }
    }
    let children = section.ensure_children();
    if let Some(index) = children
        .nodes()
        .iter()
        .rposition(|node| self::name(node) == name)
    {
        return &mut children.nodes_mut()[index];
    }
    let indent = children
        .nodes()
        .last()
        .and_then(KdlNode::format)
        .and_then(|format| indentation(&format.leading))
        .unwrap_or(INDENT)
        .to_owned();
    start_new_line(children, true);
    let mut node = KdlNode::new(name);
    node.set_format(KdlNodeFormat {
        leading: indent,
        terminator: "\n".to_owned(),
        ..KdlNodeFormat::default()
    });
    push(children, node)
}

/// 节点前导里最后一行的空白，就是它的缩进。单个空格多半是 `{ a; b }`
/// 这种行内写法里的分隔，不当作缩进。
fn indentation(leading: &str) -> Option<&str> {
    let last_line = leading.rsplit('\n').next()?;
    let blank = last_line.chars().all(|c| c == ' ' || c == '\t');
    (blank && !last_line.is_empty() && last_line != " ").then_some(last_line)
}

/// 保证接下来追加的节点从新的一行开始：上一个节点要有换行或分号结尾，
/// 空文档或空块里已有的注释也要以换行结束。块里的第一项总是另起一行。
fn start_new_line(document: &mut KdlDocument, in_block: bool) {
    if let Some(last) = document.nodes_mut().last_mut() {
        if let Some(format) = last.format_mut()
            && !(format.terminator.ends_with('\n') || format.terminator.ends_with(';'))
        {
            // `{ a #true }` 拆成多行时，原来 `}` 前的空格会变成行尾空白。
            if format.terminator.is_empty() && format.before_terminator.trim().is_empty() {
                format.before_terminator.clear();
            }
            format.terminator.push('\n');
        }
    } else if let Some(format) = document.format_mut() {
        let mut leading = std::mem::take(&mut format.leading);
        leading.push_str(&std::mem::take(&mut format.trailing));
        if (in_block || !leading.is_empty()) && !leading.ends_with('\n') {
            leading.push('\n');
        }
        format.leading = leading;
    }
}

fn push(document: &mut KdlDocument, node: KdlNode) -> &mut KdlNode {
    let nodes = document.nodes_mut();
    nodes.push(node);
    let index = nodes.len() - 1;
    &mut nodes[index]
}

/// 改第一个参数；没有参数就在节点名后补一个。
fn set_argument(node: &mut KdlNode, value: KdlValue) {
    let entries = node.entries_mut();
    match entries.iter_mut().find(|entry| entry.name().is_none()) {
        Some(entry) => set_value(entry, value),
        None => entries.insert(0, new_entry(None, value)),
    }
}

/// 改最后一个同名属性（读取时也是后面的覆盖前面的）；没有就补在末尾。
fn set_property(node: &mut KdlNode, key: &str, value: KdlValue) {
    let entries = node.entries_mut();
    match entries
        .iter_mut()
        .rfind(|entry| entry.name().is_some_and(|name| name.value() == key))
    {
        Some(entry) => set_value(entry, value),
        None => entries.push(new_entry(Some(key), value)),
    }
}

fn new_entry(key: Option<&str>, value: KdlValue) -> KdlEntry {
    let repr = repr(&value);
    let mut entry = match key {
        Some(key) => KdlEntry::new_prop(key, value),
        None => KdlEntry::new(value),
    };
    entry.set_format(KdlEntryFormat {
        value_repr: repr,
        leading: " ".to_owned(),
        ..KdlEntryFormat::default()
    });
    entry
}

/// 换掉值，同时换掉它在文件里的写法；只改值的话输出时仍是原来的文字。
fn set_value(entry: &mut KdlEntry, value: KdlValue) {
    let repr = repr(&value);
    entry.set_value(value);
    match entry.format_mut() {
        Some(format) => format.value_repr = repr,
        None => entry.set_format(KdlEntryFormat {
            value_repr: repr,
            leading: " ".to_owned(),
            ..KdlEntryFormat::default()
        }),
    }
}

/// 字符串一律加引号写出，与模板一致；kdl 默认会把像标识符的字符串写成裸词。
fn repr(value: &KdlValue) -> String {
    match value {
        KdlValue::String(text) => quoted(text),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(text: &str, change: impl FnOnce(&mut AppConfig)) -> String {
        let start = read(text).config;
        let mut now = start.clone();
        change(&mut now);
        write_changes(text, &start, &now).unwrap()
    }

    #[test]
    fn an_empty_file_is_the_default_config() {
        let loaded = read("");
        assert_eq!(loaded.config, AppConfig::default());
        assert_eq!(loaded.message, None);
        assert!(loaded.writable);
    }

    #[test]
    fn every_repeat_value_is_read() {
        for (name, mode) in REPEAT_MODES {
            let loaded = read(&format!("playback {{\n    repeat \"{name}\"\n}}\n"));
            assert_eq!(loaded.message, None, "{name}");
            assert_eq!(loaded.config.repeat, mode, "{name}");
            assert_eq!(repeat_name(mode), name);
        }
        assert_eq!(REPEAT_MODES[0], ("stop", RepeatMode::Stop));
    }

    #[test]
    fn the_sleep_fade_accepts_zero_to_ten_minutes() {
        let fade = |text: &str| read(&format!("sleep {{\n    fade {text}\n}}\n"));
        assert_eq!(fade("0").config.sleep_fade, Duration::ZERO);
        assert_eq!(fade("600").config.sleep_fade, Duration::from_secs(600));
        for wrong in ["601", "-1", "1.5", "\"30\""] {
            let loaded = fade(wrong);
            assert_eq!(loaded.config.sleep_fade, Duration::from_secs(30), "{wrong}");
            assert_eq!(
                loaded.message.unwrap(),
                format!(
                    "config.kdl 有误: 第 2 行 fade 的值 {wrong} 应为 0–600 的整数（秒），已忽略"
                ),
            );
        }
    }

    #[test]
    fn every_setting_is_read() {
        let loaded = read(
            r##"
            library "/srv/音乐"
            playback {
                volume 35
                muted #true
                repeat "one"
                shuffle #true
            }
            sort "album" descending=#true
            interface {
                visualizer #false
                lyrics #false
                mouse #false
                privacy #true
            }
            sleep {
                fade 45
            }
            theme {
                preset "light"
                selection-bg "#102030"
            }
            "##,
        );
        assert_eq!(loaded.message, None);
        let mut theme = Theme::preset("light").unwrap();
        *theme.color_mut("selection-bg").unwrap() = Color::Rgb(16, 32, 48);
        assert_eq!(
            loaded.config,
            AppConfig {
                library_dir: Some(PathBuf::from("/srv/音乐")),
                volume: 35,
                muted: true,
                repeat: RepeatMode::One,
                shuffle: true,
                sort: SortOrder {
                    key: SortKey::Album,
                    descending: true,
                },
                visualizer_enabled: false,
                mouse_enabled: false,
                lyrics_enabled: false,
                privacy: true,
                sleep_fade: Duration::from_secs(45),
                theme,
            }
        );
    }

    #[test]
    fn a_tilde_means_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let library = |text| read(text).config.library_dir.unwrap();
        assert_eq!(library("library \"~/Music\""), home.join("Music"));
        assert_eq!(library("library \"~\""), home);
        assert_eq!(library("library \"/~/x\""), PathBuf::from("/~/x"));
    }

    #[test]
    fn later_entries_win() {
        let config = read(
            "playback { volume 10; }\nplayback { volume 20; volume 30; }\nsort \"title\"\nsort descending=#true\n",
        )
        .config;
        assert_eq!(config.volume, 30);
        // 后一个 sort 没写排序键，前一个写的 title 仍然有效。
        assert_eq!(
            config.sort,
            SortOrder {
                key: SortKey::Title,
                descending: true,
            }
        );
    }

    #[test]
    fn mistakes_are_ignored_one_by_one_with_line_numbers() {
        let text = "// 第一行是注释\nplayback {\n    volume 150\n    muted \"yes\"\n    repeat \"forever\"\n    shuffle #true\n    speed 2\n}\nsort 3 descending=1 reverse=#true\ninterface {\n    lyrics\n}\ncolours {}\n";
        let loaded = read(text);
        assert!(loaded.writable);
        assert_eq!(loaded.config.volume, 100);
        assert!(loaded.config.shuffle, "写对的项照常生效");
        assert_eq!(
            loaded.message.unwrap(),
            "config.kdl 有误: 第 3 行 volume 的值 150 应为 0–100 的整数，已忽略；\
             第 4 行 muted 的值 \"yes\" 应为 #true 或 #false，已忽略；\
             第 5 行 repeat 的值 \"forever\" 应为 \"stop\"、\"none\"、\"all\"、\"one\" 之一，已忽略；\
             第 7 行 未知项 speed，已忽略；\
             第 9 行 sort 的值 3 应为加双引号的字符串，已忽略；\
             第 9 行 descending 的值 1 应为 #true 或 #false，已忽略；\
             第 9 行 sort 没有 reverse 这一项，已忽略；\
             第 11 行 lyrics 缺少值，已忽略；\
             第 13 行 未知项 colours，已忽略"
        );
    }

    #[test]
    fn theme_mistakes_fall_back_one_color_at_a_time() {
        let text = "theme {\n    border \"#ABCDEF\"\n    primary \"#12345\"\n    preset \"dark\"\n    background \"black\"\n}\n";
        let loaded = read(text);
        let mut expected = Theme::default();
        *expected.color_mut("border").unwrap() = Color::Rgb(171, 205, 239);
        assert_eq!(loaded.config.theme, expected);
        assert_eq!(
            loaded.message.unwrap(),
            "config.kdl 有误: 第 4 行 未知的主题预设 \"dark\"（可选 default、light、terminal），已改用 default；\
             第 3 行 primary 的值 \"#12345\" 无法识别，已沿用预设；\
             第 5 行 未知项 background，已忽略"
        );
    }

    #[test]
    fn theme_colors_accept_names_indexes_and_reset() {
        let theme = read(
            "theme {\n    preset \"Terminal\"\n    primary \"dark-gray\"\n    muted \"236\"\n    danger \" reset \"\n}\n",
        )
        .config
        .theme;
        let mut expected = Theme::preset("terminal").unwrap();
        *expected.color_mut("primary").unwrap() = Color::DarkGray;
        *expected.color_mut("muted").unwrap() = Color::Indexed(236);
        *expected.color_mut("danger").unwrap() = Color::Reset;
        assert_eq!(theme, expected);
    }

    #[test]
    fn a_syntax_error_names_its_position_and_blocks_writing() {
        let loaded = read("// 中文注释\nplayback {\n    volume 6O\n}\n");
        assert!(!loaded.writable);
        assert_eq!(loaded.config, AppConfig::default());
        let message = loaded.message.unwrap();
        assert!(
            message.starts_with("config.kdl 第 3 行第 12 列有语法错误"),
            "{message}"
        );
        assert!(
            message.ends_with("本次使用默认设置，退出时不会改动配置文件"),
            "{message}"
        );

        let start = AppConfig::default();
        let now = AppConfig {
            volume: 5,
            ..start.clone()
        };
        let error = write_changes("playback {", &start, &now).unwrap_err();
        assert!(error.ends_with("本次运行中改过的设置没有写回"), "{error}");
    }

    #[test]
    fn bare_true_gets_a_hint_about_kdl_v2() {
        let message = read("playback {\n    shuffle true\n}\n").message.unwrap();
        assert!(message.contains("第 2 行第 13 列"), "{message}");
        assert!(message.contains("要写成 #true、#false、#null"), "{message}");
    }

    #[test]
    fn edits_keep_comments_and_layout() {
        let text = "// 我的配置\nlibrary \"~/Music\"\n\nplayback {\n    // 小声点\n    volume 60 // 夜里\n    repeat   \"all\"\n}\n\nsort \"artist\" descending=#false // 按歌手\n";
        let updated = changed(text, |config| {
            config.volume = 45;
            config.repeat = RepeatMode::One;
            config.sort.descending = true;
        });
        assert_eq!(
            updated,
            "// 我的配置\nlibrary \"~/Music\"\n\nplayback {\n    // 小声点\n    volume 45 // 夜里\n    repeat   \"one\"\n}\n\nsort \"artist\" descending=#true // 按歌手\n"
        );
    }

    #[test]
    fn missing_entries_are_added_where_they_belong() {
        let text = "playback {\n\tvolume 60\n}\n// 结尾的注释";
        let updated = changed(text, |config| {
            config.shuffle = true;
            config.lyrics_enabled = false;
            config.sort.key = SortKey::Title;
        });
        assert_eq!(
            updated,
            "playback {\n\tvolume 60\n\tshuffle #true\n}\n\ninterface {\n    lyrics #false\n}\n\nsort \"title\"\n// 结尾的注释"
        );
    }

    #[test]
    fn empty_and_inline_blocks_still_get_valid_lines() {
        let updated = changed("playback {}\ninterface { mouse #true }", |config| {
            config.volume = 5;
            config.lyrics_enabled = false;
        });
        assert_eq!(
            updated,
            "playback {\n    volume 5\n}\ninterface { mouse #true\n    lyrics #false\n}"
        );
        let updated = changed("// 只有注释", |config| config.muted = true);
        assert_eq!(updated, "// 只有注释\nplayback {\n    muted #true\n}\n");
        let updated = changed("", |config| config.muted = true);
        assert_eq!(updated, "playback {\n    muted #true\n}\n");
    }

    #[test]
    fn privacy_mode_is_written_back_into_the_interface_block() {
        let updated = changed("interface {\n    mouse #true\n}\n", |config| {
            config.privacy = true;
        });
        assert_eq!(
            updated,
            "interface {\n    mouse #true\n    privacy #true\n}\n"
        );
    }

    #[test]
    fn values_are_written_in_the_last_place_they_are_read_from() {
        let text = "playback { volume 10; }\nplayback {\n    volume 20\n}\n";
        let updated = changed(text, |config| config.volume = 70);
        assert_eq!(
            updated,
            "playback { volume 10; }\nplayback {\n    volume 70\n}\n"
        );
    }

    #[test]
    fn hand_edits_to_other_settings_survive_a_save() {
        let at_start = "playback {\n    volume 60\n    muted #false\n}\n";
        let start = read(at_start).config;
        let now = AppConfig {
            volume: 80,
            ..start.clone()
        };
        // 运行中用户在文件里改了 muted，退出时只写 volume，不碰 muted。
        let edited = "playback {\n    volume 60\n    muted #true\n}\n";
        assert_eq!(
            write_changes(edited, &start, &now).unwrap(),
            "playback {\n    volume 80\n    muted #true\n}\n"
        );
    }

    #[test]
    fn strings_are_always_quoted_and_escaped() {
        assert_eq!(quoted("artist"), "\"artist\"");
        assert_eq!(quoted("C:\\Music\\\"x\""), "\"C:\\\\Music\\\\\\\"x\\\"\"");
        assert_eq!(quoted("a\u{1}b"), "\"a\\u{1}b\"");
        let updated = changed("sort artist", |config| config.sort.key = SortKey::Album);
        assert_eq!(updated, "sort \"album\"");
    }

    #[test]
    fn set_library_replaces_or_adds_the_library_line() {
        assert_eq!(
            set_library("library \"/old\" // 旧的\n", Path::new("/new/音乐")).unwrap(),
            "library \"/new/音乐\" // 旧的\n"
        );
        assert_eq!(
            set_library("playback {\n    volume 5\n}\n", Path::new("/new")).unwrap(),
            "playback {\n    volume 5\n}\n\nlibrary \"/new\"\n"
        );
        assert!(set_library("library", Path::new("/new")).is_ok());
        assert!(set_library("library {", Path::new("/new")).is_err());
    }
}
