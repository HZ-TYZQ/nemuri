//! 粘贴进来的时间轴：每行认出一个时间和名字，例如 B 站简介里的“03:21 掏耳”。
//!
//! 只认位置，不认时长：DLsite 曲目表里“タイトル (12:34)”这样的时长也会被当成位置，
//! 粘贴框里的识别结果让人在添加前看出来。

use std::time::Duration;

use crate::marks::SAME_SPOT;

/// 时间和名字之间、名字两头常见的分隔符。
const SEPARATORS: &[char] = &[
    '-', '–', '—', '~', '～', '|', '｜', '·', '・', ':', '：', '、', '，', ',', '.', '。',
];
const OPENING: &[char] = &['(', '[', '（', '【', '「', '『', '<', '《'];
/// 和 [`OPENING`] 一一对应。
const CLOSING: &[char] = &[')', ']', '）', '】', '」', '』', '>', '》'];
/// 范围写法“03:21～05:00”中间的符号。
const RANGE_MARKS: &[char] = &['-', '–', '—', '~', '～', '至', '到'];
/// 分钟或小时最多几位，免得一长串数字溢出。
const MAX_LEADING_DIGITS: usize = 5;

/// 时间轴里认出的一个跳转点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub position: Duration,
    pub name: Option<String>,
    /// 第几行，从 0 开始。
    pub line: usize,
}

/// 逐行认出时间和名字。认不出时间的行跳过；和前面认出的相距不到 [`SAME_SPOT`] 的只取前面那个。
pub fn parse(text: &str) -> Vec<Parsed> {
    let mut parsed: Vec<Parsed> = Vec::new();
    for (line, text) in text.lines().enumerate() {
        let Some((position, name)) = parse_line(text) else {
            continue;
        };
        if parsed
            .iter()
            .any(|earlier| earlier.position.abs_diff(position) < SAME_SPOT)
        {
            continue;
        }
        parsed.push(Parsed {
            position,
            name,
            line,
        });
    }
    parsed
}

/// 这一行能不能认出时间；粘贴框用它区分显示。
pub fn recognizes(line: &str) -> bool {
    parse_line(line).is_some()
}

/// 一行里的第一个时间，以及它的名字：时间后面的文字，后面没有时取前面的。
fn parse_line(line: &str) -> Option<(Duration, Option<String>)> {
    let original: Vec<char> = line.chars().collect();
    // 只在认时间时把全角换成半角，名字照原样。换的都是单个字符，位置不变。
    let chars: Vec<char> = original.iter().copied().map(half_width).collect();
    let (start, mut end, position) = (0..chars.len())
        .find_map(|start| time_at(&chars, start).map(|(end, position)| (start, end, position)))?;
    // 范围写法只取开头，结尾就是下一段的开头。
    let mark = skip_spaces(&chars, end);
    if chars
        .get(mark)
        .is_some_and(|mark| RANGE_MARKS.contains(mark))
        && let Some((range_end, _)) = time_at(&chars, skip_spaces(&chars, mark + 1))
    {
        end = range_end;
    }
    let name = clean_name(&original[end..])
        .or_else(|| clean_name(&original[..start]).filter(|name| !is_numbering(name)));
    Some((position, name))
}

/// 从 `start` 开始的时间：`m:ss`、`mmm:ss` 或 `h:mm:ss`，可以带 `.毫秒`。
/// 前后紧挨着数字，或隔着冒号还有数字时，是更长的一串里的一截，不算。返回时间结束的位置和时间。
fn time_at(chars: &[char], start: usize) -> Option<(usize, Duration)> {
    let before = |offset: usize| {
        start
            .checked_sub(offset)
            .and_then(|index| chars.get(index))
            .copied()
    };
    if continues_time(before(1), before(2)) {
        return None;
    }
    let leading = digit_run(chars, start);
    if leading == 0 || leading > MAX_LEADING_DIGITS {
        return None;
    }
    let mut index = start + leading;
    let first = two_digits_after_colon(chars, index)?;
    index += 3;
    let (hours, minutes, seconds) = match two_digits_after_colon(chars, index) {
        Some(second) => {
            index += 3;
            (value(&chars[start..start + leading]), first, second)
        }
        None => (0, value(&chars[start..start + leading]), first),
    };
    if seconds >= 60 || (hours > 0 && minutes >= 60) {
        return None;
    }
    let mut millis = 0;
    if chars.get(index) == Some(&'.') {
        let fraction = digit_run(chars, index + 1);
        if fraction > 0 {
            // 只要前三位，再往后的不要了。
            let kept = fraction.min(3);
            millis = value(&chars[index + 1..index + 1 + kept]) * 10u64.pow(3 - kept as u32);
            index += 1 + fraction;
        }
    }
    if continues_time(chars.get(index).copied(), chars.get(index + 1).copied()) {
        return None;
    }
    let total = (hours * 60 + minutes) * 60 + seconds;
    Some((
        index,
        Duration::from_secs(total) + Duration::from_millis(millis),
    ))
}

/// `index` 处的冒号和紧跟的正好两位数字。
fn two_digits_after_colon(chars: &[char], index: usize) -> Option<u64> {
    (chars.get(index) == Some(&':') && digit_run(chars, index + 1) == 2)
        .then(|| value(&chars[index + 1..index + 3]))
}

/// 从 `start` 开始连着几个数字。
fn digit_run(chars: &[char], start: usize) -> usize {
    chars
        .get(start..)
        .unwrap_or_default()
        .iter()
        .take_while(|character| character.is_ascii_digit())
        .count()
}

/// 一串半角数字的值；位数由调用方限制，不会溢出。
fn value(digits: &[char]) -> u64 {
    digits.iter().fold(0, |value, digit| {
        value * 10 + u64::from(*digit as u8 - b'0')
    })
}

/// 时间旁边的字符 `neighbor`（再往外一个是 `beyond`）说明这串时间还没完：
/// 是数字，或者是冒号且冒号外面还是数字。“掏耳：03:21”里的冒号只是分隔符。
fn continues_time(neighbor: Option<char>, beyond: Option<char>) -> bool {
    let digit =
        |character: Option<char>| character.is_some_and(|character| character.is_ascii_digit());
    digit(neighbor) || (neighbor == Some(':') && digit(beyond))
}

fn skip_spaces(chars: &[char], start: usize) -> usize {
    start
        + chars
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|character| character.is_whitespace())
            .count()
}

/// 去掉两头的空白和分隔符，以及开头的右括号、结尾的左括号（多半是时间两边的括号）；
/// 整个名字包在一对括号里时去掉这对括号。什么也不剩时为 None。
fn clean_name(chars: &[char]) -> Option<String> {
    let name: String = chars.iter().collect();
    let is_separator =
        |character: char| character.is_whitespace() || SEPARATORS.contains(&character);
    let name = name
        .trim_start_matches(|character: char| {
            is_separator(character) || CLOSING.contains(&character)
        })
        .trim_end_matches(|character: char| {
            is_separator(character) || OPENING.contains(&character)
        });
    let name = OPENING
        .iter()
        .zip(CLOSING)
        .find_map(|(open, close)| name.strip_prefix(*open)?.strip_suffix(*close))
        .unwrap_or(name)
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// 行首的序号（“1.”“12”）不当名字。
fn is_numbering(name: &str) -> bool {
    name.chars().all(|character| character.is_ascii_digit())
}

/// 全角数字和冒号换成半角，其他字符不变。
fn half_width(character: char) -> char {
    match character {
        '０'..='９' => char::from_u32(u32::from(character) - u32::from('０') + u32::from('0'))
            .unwrap_or(character),
        '：' => ':',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Option<(Duration, Option<String>)> {
        parse_line(line)
    }

    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn named(seconds: u64, name: &str) -> Option<(Duration, Option<String>)> {
        Some((at(seconds), Some(name.to_owned())))
    }

    #[test]
    fn the_usual_ways_of_writing_a_time_are_recognized() {
        assert_eq!(one("03:21 掏耳"), named(201, "掏耳"));
        assert_eq!(one("3:21 掏耳"), named(201, "掏耳"));
        assert_eq!(one("1:02:03 结束"), named(3723, "结束"));
        // 长视频常把分钟写过 59。
        assert_eq!(one("75:30 很晚了"), named(4530, "很晚了"));
        assert_eq!(one("00:00"), Some((Duration::ZERO, None)));
        let millis = |line: &str| one(line).map(|(position, _)| position - at(201));
        assert_eq!(millis("03:21.5 a"), Some(Duration::from_millis(500)));
        assert_eq!(millis("03:21.25 a"), Some(Duration::from_millis(250)));
        assert_eq!(millis("03:21.1239 a"), Some(Duration::from_millis(123)));
        // 句末的点不是小数点。
        assert_eq!(one("03:21. 掏耳"), named(201, "掏耳"));
    }

    #[test]
    fn things_that_only_look_like_times_are_not() {
        for line in [
            "03:60 秒数不对",
            "1:60:00 分钟不对",
            "103:21:5",
            "12:3",
            "12:345",
            "12:30:45:10",
            "2024-03-21",
            "第3集",
            "",
            "123456:00",
        ] {
            assert_eq!(one(line), None, "{line}");
        }
    }

    #[test]
    fn full_width_digits_and_colons_are_recognized() {
        assert_eq!(one("０３：２１　掏耳"), named(201, "掏耳"));
        assert_eq!(one("03：21 掏耳"), named(201, "掏耳"));
        // 名字里的全角字符照原样保留。
        assert_eq!(one("03:21 第２部：耳语"), named(201, "第２部：耳语"));
    }

    #[test]
    fn a_time_in_brackets_is_recognized() {
        assert_eq!(one("【03:21】掏耳"), named(201, "掏耳"));
        assert_eq!(one("[03:21] 掏耳"), named(201, "掏耳"));
        assert_eq!(one("(03:21)掏耳"), named(201, "掏耳"));
        assert_eq!(one("（03:21）掏耳"), named(201, "掏耳"));
    }

    #[test]
    fn a_range_keeps_only_its_start() {
        assert_eq!(one("03:21～05:00 掏耳"), named(201, "掏耳"));
        assert_eq!(one("03:21 - 05:00 掏耳"), named(201, "掏耳"));
        assert_eq!(one("03:21到05:00 掏耳"), named(201, "掏耳"));
        // 后面不是时间时，范围符号只是分隔符。
        assert_eq!(one("03:21-掏耳"), named(201, "掏耳"));
    }

    #[test]
    fn the_name_comes_from_after_the_time_or_else_before_it() {
        assert_eq!(one("03:21 - 掏耳"), named(201, "掏耳"));
        assert_eq!(one("03:21｜掏耳（左耳）"), named(201, "掏耳（左耳）"));
        assert_eq!(one("03:21：掏耳 "), named(201, "掏耳"));
        assert_eq!(one("掏耳 03:21"), named(201, "掏耳"));
        assert_eq!(one("掏耳：03:21"), named(201, "掏耳"));
        assert_eq!(one("【掏耳】03:21"), named(201, "掏耳"));
        assert_eq!(one("1. 03:21 掏耳"), named(201, "掏耳"));
        // 只有序号时没有名字。
        assert_eq!(one("1. 03:21"), Some((at(201), None)));
        assert_eq!(one("P2 03:21"), named(201, "P2"));
        // 一行只认一个时间，后面的时间算在名字里。
        assert_eq!(one("03:21 掏耳 05:00 耳语"), named(201, "掏耳 05:00 耳语"));
    }

    #[test]
    fn parsing_keeps_line_numbers_and_drops_near_duplicates() {
        let text =
            "时间轴：\r\n\r\n03:21 掏耳\r\n03:21.5 重复\r\n03:22 下一段\n没有时间\n1:00:00 结束";
        assert_eq!(
            parse(text),
            [
                Parsed {
                    position: at(201),
                    name: Some("掏耳".to_owned()),
                    line: 2,
                },
                // 整整 1 秒就是另一处。
                Parsed {
                    position: at(202),
                    name: Some("下一段".to_owned()),
                    line: 4,
                },
                Parsed {
                    position: at(3600),
                    name: Some("结束".to_owned()),
                    line: 6,
                },
            ]
        );
        assert!(recognizes("03:21.5 重复"));
        assert!(!recognizes("没有时间"));
    }
}
