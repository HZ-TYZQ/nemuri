//! 歌词：LRC 同步歌词与纯文本歌词的解析、按播放位置定位当前行，
//! 以及从同名 `.lrc` 或内嵌标签读取。

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use encoding_rs::{Encoding, GB18030};
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::ItemKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub time: Duration,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    lines: Vec<LyricLine>,
    /// 纯文本歌词没有时间标签：各行时间都是零，也没有“当前行”。
    synced: bool,
}

impl Lyrics {
    /// 只收带时间标签的行；一行都没有就不是同步歌词，返回 None。
    ///
    /// 支持一行多个时间标签、`[offset:±ms]`，并去掉逐字歌词的 `<mm:ss.xx>` 标签。
    /// `ti`、`ar` 等元数据标签被忽略。
    pub fn parse(text: &str) -> Option<Self> {
        let mut offset_ms: i64 = 0;
        let mut lines = Vec::new();
        for raw in text.lines() {
            let mut rest = raw.trim();
            let mut times = Vec::new();
            while let Some(after) = rest.strip_prefix('[') {
                let Some(end) = after.find(']') else {
                    break;
                };
                let tag = &after[..end];
                if let Some(time) = parse_timestamp(tag) {
                    times.push(time);
                } else if let Some(value) = tag.strip_prefix("offset:") {
                    offset_ms = value.trim().parse().unwrap_or(offset_ms);
                }
                rest = after[end + 1..].trim_start();
            }
            if times.is_empty() {
                continue;
            }
            let text = strip_word_timestamps(rest);
            lines.extend(times.into_iter().map(|time| LyricLine {
                time,
                text: text.clone(),
            }));
        }
        if lines.is_empty() {
            return None;
        }
        // 正 offset 让歌词提前出现。
        for line in &mut lines {
            let shifted = i64::try_from(line.time.as_millis()).unwrap_or(i64::MAX) - offset_ms;
            line.time = Duration::from_millis(shifted.max(0) as u64);
        }
        // 稳定排序：同一时刻的多行（如原文与译文）保持文件中的先后。
        lines.sort_by_key(|line| line.time);
        Some(Self {
            lines,
            synced: true,
        })
    }

    /// 没有时间标签的歌词按行原样保留；去掉 `[ti:…]` 这类 LRC 元数据行，
    /// 首尾空行去掉，段落之间的多个空行并成一个。一行文字都没有时返回 None。
    pub fn parse_plain(text: &str) -> Option<Self> {
        let mut lines: Vec<LyricLine> = Vec::new();
        for raw in text.lines() {
            let text = raw.trim();
            if is_metadata_tag(text) {
                continue;
            }
            let blank_after_blank =
                text.is_empty() && lines.last().is_none_or(|line| line.text.is_empty());
            if !blank_after_blank {
                lines.push(LyricLine {
                    time: Duration::ZERO,
                    text: text.to_owned(),
                });
            }
        }
        while lines.last().is_some_and(|line| line.text.is_empty()) {
            lines.pop();
        }
        (!lines.is_empty()).then_some(Self {
            lines,
            synced: false,
        })
    }

    /// 能按 LRC 解析就是同步歌词，否则当作纯文本；只是占位文字时返回 None。
    /// 带时间标签的占位文字不再退回纯文本，免得连时间标签一起显示出来。
    fn parse_any(text: &str) -> Option<Self> {
        Self::parse(text)
            .or_else(|| Self::parse_plain(text))
            .filter(|lyrics| !lyrics.is_placeholder())
    }

    /// 每一行有字的歌词都是“纯音乐”“暂无歌词”这类占位文字：
    /// 音乐平台给没有歌词的曲目填上一句，下载的文件常把它带进标签。
    fn is_placeholder(&self) -> bool {
        let mut texts = self
            .lines
            .iter()
            .map(|line| placeholder_key(&line.text))
            .filter(|key| !key.is_empty())
            .peekable();
        texts.peek().is_some() && texts.all(|key| PLACEHOLDERS.contains(&key.as_str()))
    }

    pub fn lines(&self) -> &[LyricLine] {
        &self.lines
    }

    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// 已经开始的最后一行；第一行之前或纯文本歌词返回 None。
    pub fn current_index(&self, position: Duration) -> Option<usize> {
        if !self.synced {
            return None;
        }
        self.lines
            .partition_point(|line| line.time <= position)
            .checked_sub(1)
    }

    /// 同步歌词优先于纯文本；同一种里，音频旁的同名 `.lrc` 优先于内嵌标签。
    /// `.lrc` 读不了、解不了码或没有同步歌词时都会再看内嵌歌词。
    /// 没有歌词不是错误；只有 `.lrc` 读不了、内嵌也没有歌词时才返回 Err，
    /// 让界面说明原因。
    pub fn load_for(track: &Path) -> Result<Option<Self>, String> {
        let (sidecar, error) = match sidecar_path(track).map(|path| read_sidecar(&path)) {
            None => (None, None),
            Some(Ok(lyrics)) => (lyrics, None),
            Some(Err(error)) => (None, Some(error)),
        };
        if sidecar.as_ref().is_some_and(Self::is_synced) {
            return Ok(sidecar);
        }
        let embedded = embedded(track);
        if embedded.as_ref().is_some_and(Self::is_synced) {
            return Ok(embedded);
        }
        match (sidecar.or(embedded), error) {
            (None, Some(error)) => Err(error),
            (lyrics, _) => Ok(lyrics),
        }
    }
}

/// 占位歌词按 [`placeholder_key`] 处理后的样子。
const PLACEHOLDERS: [&str; 8] = [
    "纯音乐",
    "純音樂",
    "纯音乐请欣赏",
    "纯音乐请您欣赏",
    "此歌曲为没有填词的纯音乐请您欣赏",
    "暂无歌词",
    "暫無歌詞",
    "instrumental",
];

/// 只留文字和数字并转成小写：标点、空格和括号的写法各不相同，`[Instrumental]` 也能认出。
fn placeholder_key(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 整行只是一个 `[ti:…]`、`[ar:…]`、`[offset:…]` 之类的 LRC 元数据标签。
fn is_metadata_tag(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .and_then(|tag| tag.split_once(':'))
        .is_some_and(|(key, _)| !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphabetic()))
}

fn read_sidecar(path: &Path) -> Result<Option<Lyrics>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("无法读取歌词 {}: {error}", file_label(path)))?;
    let text = decode(&bytes).ok_or_else(|| {
        format!(
            "歌词 {} 无法按 UTF-8、UTF-16 或 GBK 解码，已忽略",
            file_label(path)
        )
    })?;
    Ok(Lyrics::parse_any(&text))
}

/// `mm:ss`、`mm:ss.xx`、`mm:ss.xxx` 以及少见的 `mm:ss:xx`。
fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (minutes, rest) = tag.split_once(':')?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let all_digits = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(minutes) || !all_digits(seconds) {
        return None;
    }
    let minutes: u64 = minutes.parse().ok()?;
    let seconds: u64 = seconds.parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(fraction) if all_digits(fraction) => {
            // 只取前三位：百分之一秒和毫秒都常见，个别文件写到微秒。
            let digits = &fraction[..fraction.len().min(3)];
            let value: u64 = digits.parse().ok()?;
            value * 10_u64.pow(3 - digits.len() as u32)
        }
        Some(_) => return None,
    };
    Some(Duration::from_millis(
        (minutes * 60 + seconds) * 1000 + millis,
    ))
}

fn strip_word_timestamps(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        result.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) if parse_timestamp(&rest[start + 1..start + end]).is_some() => {
                rest = &rest[start + end + 1..];
            }
            _ => {
                result.push('<');
                rest = &rest[start + 1..];
            }
        }
    }
    result.push_str(rest);
    result.trim().to_owned()
}

fn sidecar_path(track: &Path) -> Option<PathBuf> {
    ["lrc", "LRC"]
        .into_iter()
        .map(|extension| track.with_extension(extension))
        .find(|path| path.is_file())
}

/// 有 BOM 按 BOM（UTF-8 / UTF-16）；没有 BOM 先按 UTF-8，不合法再按 GB18030。
/// GB18030 是 GBK 的超集，覆盖了中文 `.lrc` 最常见的非 UTF-8 编码。
fn decode(bytes: &[u8]) -> Option<String> {
    if let Some((encoding, bom_length)) = Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling_and_without_replacement(&bytes[bom_length..])
            .map(Cow::into_owned);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(text.to_owned());
    }
    GB18030
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(Cow::into_owned)
}

/// 内嵌歌词放在 LYRICS / USLT / ©lyr 里，可能是 LRC 文本，也可能是纯文本；
/// 任何一处有同步歌词就用它，否则用第一段纯文本。占位文字不算歌词。
fn embedded(track: &Path) -> Option<Lyrics> {
    let tagged = Probe::open(track)
        .ok()?
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let texts: Vec<&str> = tagged
        .tags()
        .iter()
        .flat_map(|tag| {
            [ItemKey::Lyrics, ItemKey::UnsyncLyrics]
                .into_iter()
                .filter_map(|key| tag.get_string(key))
        })
        .collect();
    let parsed: Vec<Lyrics> = texts
        .iter()
        .filter_map(|text| Lyrics::parse_any(text))
        .collect();
    let chosen = parsed.iter().position(Lyrics::is_synced).unwrap_or(0);
    parsed.into_iter().nth(chosen)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use lofty::config::WriteOptions;
    use lofty::tag::{Tag, TagExt, TagType};

    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn parses_timestamps_metadata_and_multiple_tags() {
        let lyrics = Lyrics::parse(
            "[ti:测试]\n[ar:歌手]\n\n[00:12.50]第一行\n[01:02.345][00:05]重复\n[00:20:30]冒号小数\n纯文本行\n",
        )
        .unwrap();
        let lines: Vec<_> = lyrics
            .lines()
            .iter()
            .map(|line| (line.time, line.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (ms(5_000), "重复"),
                (ms(12_500), "第一行"),
                (ms(20_300), "冒号小数"),
                (ms(62_345), "重复"),
            ]
        );
    }

    #[test]
    fn current_line_follows_the_position() {
        let lyrics = Lyrics::parse("[00:01.00]a\n[00:03.00]b\n[00:03.00]b 译文\n").unwrap();
        assert_eq!(lyrics.current_index(ms(500)), None);
        assert_eq!(lyrics.current_index(ms(1_000)), Some(0));
        assert_eq!(lyrics.current_index(ms(2_999)), Some(0));
        assert_eq!(lyrics.current_index(ms(3_000)), Some(2));
        assert_eq!(lyrics.lines()[1].text, "b");
        assert_eq!(lyrics.current_index(ms(99_000)), Some(2));
    }

    #[test]
    fn offset_shifts_lines_earlier_and_never_below_zero() {
        let lyrics = Lyrics::parse("[offset:+500]\n[00:00.20]a\n[00:02.00]b\n").unwrap();
        assert_eq!(lyrics.lines()[0].time, Duration::ZERO);
        assert_eq!(lyrics.lines()[1].time, ms(1_500));

        let lyrics = Lyrics::parse("[offset:-250]\n[00:01.00]a\n").unwrap();
        assert_eq!(lyrics.lines()[0].time, ms(1_250));
    }

    #[test]
    fn word_timestamps_are_stripped_but_other_angle_brackets_stay() {
        let lyrics = Lyrics::parse("[00:01.00]<00:01.00>你 <00:01.50>好 <a>\n").unwrap();
        assert_eq!(lyrics.lines()[0].text, "你 好 <a>");
    }

    #[test]
    fn plain_text_is_not_synced_lyrics() {
        assert_eq!(Lyrics::parse("只是歌词\n没有时间"), None);
        assert_eq!(Lyrics::parse("[ar:x]\n[99:99]x"), None);
    }

    #[test]
    fn sidecar_lrc_is_found_and_decoded() {
        let temp = tempfile::tempdir().unwrap();
        let track = temp.path().join("歌.flac");
        assert_eq!(Lyrics::load_for(&track), Ok(None));

        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend_from_slice("[00:01.00]带 BOM\n".as_bytes());
        fs::write(temp.path().join("歌.lrc"), &bom).unwrap();
        let lyrics = Lyrics::load_for(&track).unwrap().unwrap();
        assert_eq!(lyrics.lines()[0].text, "带 BOM");

        let mut utf16 = b"\xFF\xFE".to_vec();
        for unit in "[00:02.00]宽字符\n".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(temp.path().join("歌.lrc"), &utf16).unwrap();
        let lyrics = Lyrics::load_for(&track).unwrap().unwrap();
        assert_eq!(lyrics.lines()[0].text, "宽字符");

        let mut utf16_be = b"\xFE\xFF".to_vec();
        for unit in "[00:03.00]大端\n".encode_utf16() {
            utf16_be.extend_from_slice(&unit.to_be_bytes());
        }
        fs::write(temp.path().join("歌.lrc"), &utf16_be).unwrap();
        let lyrics = Lyrics::load_for(&track).unwrap().unwrap();
        assert_eq!(lyrics.lines()[0].text, "大端");

        // GBK 的“你好，世界”：不是合法 UTF-8，按 GB18030 解出。
        fs::write(
            temp.path().join("歌.lrc"),
            b"[ti:\xB2\xE2\xCA\xD4]\n[00:01.00]\xC4\xE3\xBA\xC3\xA3\xAC\xCA\xC0\xBD\xE7\n",
        )
        .unwrap();
        let lyrics = Lyrics::load_for(&track).unwrap().unwrap();
        assert_eq!(lyrics.lines()[0].text, "你好，世界");

        // 0xFF 在 UTF-8 和 GB18030 里都不合法。
        fs::write(temp.path().join("歌.lrc"), b"[00:01.00]\xFF\n").unwrap();
        assert!(Lyrics::load_for(&track).unwrap_err().contains("无法按"));
    }

    #[test]
    fn plain_text_keeps_lines_and_paragraphs_but_drops_metadata() {
        let lyrics = Lyrics::parse_plain("[ti:标题]\n\n 第一段 \n第二行\n\n\n第二段\n\n").unwrap();
        assert!(!lyrics.is_synced());
        let texts: Vec<_> = lyrics
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(texts, ["第一段", "第二行", "", "第二段"]);
        assert_eq!(lyrics.current_index(ms(10_000)), None);

        assert_eq!(Lyrics::parse_plain("[ti:x]\n[ar:y]\n\n"), None);
        // 只有方括号开头、不是元数据标签的行照常保留。
        assert_eq!(
            Lyrics::parse_plain("[副歌]").unwrap().lines()[0].text,
            "[副歌]"
        );
    }

    /// 写一个极短的静音 WAV，需要时用 ID3v2 USLT 内嵌歌词。
    fn write_wav_with_lyrics(path: &Path, lyrics: Option<&str>) {
        let samples = 8_u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8_000_u32.to_le_bytes());
        wav.extend_from_slice(&16_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.resize(wav.len() + samples as usize * 2, 0);
        fs::write(path, wav).unwrap();
        if let Some(text) = lyrics {
            let mut tag = Tag::new(TagType::Id3v2);
            // lofty 把 ID3v2 的 USLT 对应到 UnsyncLyrics；ID3v2 不支持 ItemKey::Lyrics。
            tag.insert_text(ItemKey::UnsyncLyrics, text.to_owned());
            tag.save_to_path(path, WriteOptions::default()).unwrap();
        }
    }

    fn first_line(track: &Path) -> (bool, String) {
        let lyrics = Lyrics::load_for(track).unwrap().unwrap();
        (lyrics.is_synced(), lyrics.lines()[0].text.clone())
    }

    #[test]
    fn embedded_synced_and_plain_lyrics_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let track = temp.path().join("歌.wav");
        write_wav_with_lyrics(&track, None);
        assert_eq!(Lyrics::load_for(&track), Ok(None));

        write_wav_with_lyrics(&track, Some("[00:01.00]内嵌同步"));
        assert_eq!(first_line(&track), (true, "内嵌同步".to_owned()));

        write_wav_with_lyrics(&track, Some("内嵌纯文本\n第二行"));
        assert_eq!(first_line(&track), (false, "内嵌纯文本".to_owned()));
    }

    #[test]
    fn synced_lyrics_win_over_plain_and_the_sidecar_wins_ties() {
        let temp = tempfile::tempdir().unwrap();
        let track = temp.path().join("歌.wav");
        let sidecar = temp.path().join("歌.lrc");

        // .lrc 没有时间标签：回退到内嵌的同步歌词。
        write_wav_with_lyrics(&track, Some("[00:01.00]内嵌同步"));
        fs::write(&sidecar, "旁边纯文本").unwrap();
        assert_eq!(first_line(&track), (true, "内嵌同步".to_owned()));

        fs::write(&sidecar, "[00:01.00]旁边同步").unwrap();
        assert_eq!(first_line(&track), (true, "旁边同步".to_owned()));

        write_wav_with_lyrics(&track, Some("内嵌纯文本"));
        fs::write(&sidecar, "旁边纯文本").unwrap();
        assert_eq!(first_line(&track), (false, "旁边纯文本".to_owned()));
    }

    #[test]
    fn placeholder_lyrics_are_not_lyrics() {
        for text in [
            "纯音乐",
            "[00:00.00]纯音乐，请欣赏",
            "[00:00.000] 此歌曲为没有填词的纯音乐，请您欣赏\n[00:05.00]",
            "[Instrumental]",
            "\n 暂无歌词 \n\n",
        ] {
            assert_eq!(Lyrics::parse_any(text), None, "{text:?}");
        }
        // 只是提到“纯音乐”的歌词照常显示。
        let lyrics = Lyrics::parse_any("纯音乐\n这一段是真的歌词").unwrap();
        assert_eq!(lyrics.lines().len(), 2);
        assert!(Lyrics::parse_any("[00:01.00]纯音乐时间").is_some());
    }

    #[test]
    fn placeholder_lyrics_fall_through_to_the_next_source() {
        let temp = tempfile::tempdir().unwrap();
        let track = temp.path().join("歌.wav");
        write_wav_with_lyrics(&track, Some("纯音乐"));
        assert_eq!(Lyrics::load_for(&track), Ok(None));

        // 旁边的 .lrc 是占位文字时改用内嵌的真歌词。
        write_wav_with_lyrics(&track, Some("内嵌纯文本"));
        fs::write(temp.path().join("歌.lrc"), "[00:00.00]纯音乐，请欣赏").unwrap();
        assert_eq!(first_line(&track), (false, "内嵌纯文本".to_owned()));
    }

    #[test]
    fn an_undecodable_sidecar_falls_back_to_embedded_lyrics() {
        let temp = tempfile::tempdir().unwrap();
        let track = temp.path().join("歌.wav");
        fs::write(temp.path().join("歌.lrc"), b"[00:01.00]\xFF\n").unwrap();

        write_wav_with_lyrics(&track, Some("内嵌纯文本"));
        assert_eq!(first_line(&track), (false, "内嵌纯文本".to_owned()));

        // 内嵌也没有歌词时才报告 .lrc 的问题。
        write_wav_with_lyrics(&track, None);
        assert!(Lyrics::load_for(&track).unwrap_err().contains("无法按"));
    }
}
