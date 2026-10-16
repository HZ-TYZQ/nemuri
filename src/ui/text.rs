//! 终端显示宽度与时间文本工具。

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) use crate::track::fmt_duration;

/// 生成 width 列的 ASCII 进度条：'=' 已播放、'>' 播放头、'-' 未播放。
pub(super) fn ascii_progress_bar(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let ratio = ratio.clamp(0.0, 1.0);
    if ratio >= 1.0 {
        return "=".repeat(width);
    }
    let filled = (ratio * width as f64).floor() as usize;
    let mut bar = "=".repeat(filled);
    bar.push('>');
    bar.push_str(&"-".repeat(width - filled - 1));
    bar
}

/// 在 [`ascii_progress_bar`] 画好的进度条上用 '|' 标出跳转点，`ratios` 是它们各自占总时长的比例。
/// 和播放头 '>' 落在同一格时显示播放头。
pub(super) fn mark_ticks(bar: &str, ratios: impl IntoIterator<Item = f64>) -> String {
    let mut cells: Vec<char> = bar.chars().collect();
    let Some(last) = cells.len().checked_sub(1) else {
        return String::new();
    };
    let width = cells.len() as f64;
    for ratio in ratios {
        let cell = ((ratio.clamp(0.0, 1.0) * width).floor() as usize).min(last);
        if cells[cell] != '>' {
            cells[cell] = '|';
        }
    }
    cells.into_iter().collect()
}

/// 按终端显示宽度截断：超出时保留 max_width-1 列内容后接省略号。
pub(super) fn truncate_display(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut truncated = String::new();
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width + 1 > max_width {
            break;
        }
        truncated.push(character);
        used += character_width;
    }
    truncated.push('…');
    truncated
}

/// 按终端显示宽度逐字折行（不按词）：每行不超过 width 列，宽字符不会被拆到两行。
pub(super) fn wrap_display(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        if let Some(row) = rows.last_mut() {
            row.push(character);
        }
        used += character_width;
    }
    rows
}

pub(super) fn column_text(text: &str, width: usize, right: bool) -> String {
    let truncated = truncate_display(text, width);
    let padding = " ".repeat(width.saturating_sub(UnicodeWidthStr::width(truncated.as_str())));
    if right {
        format!("{padding}{truncated}")
    } else {
        format!("{truncated}{padding}")
    }
}

pub(super) fn now_playing_text(
    title: &str,
    artist: Option<&str>,
    budget: usize,
) -> (String, String) {
    if UnicodeWidthStr::width(title) <= budget {
        let remaining = budget - UnicodeWidthStr::width(title);
        let artist = artist
            .map(|artist| format!(" — {artist}"))
            .unwrap_or_default();
        (title.to_owned(), truncate_display(&artist, remaining))
    } else {
        (truncate_display(title, budget), String::new())
    }
}
