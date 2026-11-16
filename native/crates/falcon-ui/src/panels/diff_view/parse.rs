//! unified diff → 表格行（旧 React 版 `GitDiffView.tsx` 的 `parseDiff` / `toSplitRows`），纯函数。
//!
//! 与 React 版的一处差别：行一律等高。React 版的文件头 / hunk 头 / 说明行各有各的 padding
//! （36 / 30 / 26px），所以大 diff 要自己算前缀偏移做行窗口；这里全部压成正文行高交给
//! `uniform_list`，虚拟化是白送的，不再有"500 行以上才启用"的分支。

/// 正文行
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub sign: Sign,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    Add,
    Del,
    Ctx,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// 一个文件段的开头（diff --git ...）
    File(String),
    /// @@ hunk 头
    Hunk(String),
    Line(Line),
    /// 值得保留的元信息：new file / deleted file / rename / Binary files / \ No newline
    Note(String),
}

/// 分栏里一侧的一行
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideLine {
    pub no: u32,
    pub text: String,
    pub sign: Sign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitRow {
    File(String),
    Hunk(String),
    Note(String),
    Pair {
        left: Option<SideLine>,
        right: Option<SideLine>,
    },
}

/// 制表符按 8 列的制表位展开（React 版表格是 `white-space: pre`，浏览器默认 tab-size 8）。
/// GPUI 的文本不认制表位，不展开的话缩进全乱
pub fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut col = 0usize;
    for ch in s.chars() {
        if ch == '\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// 解析 unified diff。行号从 @@ 头累加。index / mode / --- / +++ 这些头部行不进表格——
/// 文件名与状态已经在别处说清了。
pub fn parse_diff(text: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;
    let body = text.strip_suffix('\n').unwrap_or(text);
    for raw in body.split('\n') {
        // CRLF 文件的行尾 \r 不是内容，留着会在 GPUI 里画成一个怪字符
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if in_hunk && raw.starts_with('\\') {
            // "\ No newline at end of file"
            rows.push(DiffRow::Note(raw[1..].trim().to_string()));
            continue;
        }
        if in_hunk && (raw.starts_with('+') || raw.starts_with('-') || raw.starts_with(' ') || raw.is_empty()) {
            let text = expand_tabs(raw.get(1..).unwrap_or(""));
            let line = match raw.as_bytes().first() {
                Some(b'+') => {
                    new_no += 1;
                    Line {
                        sign: Sign::Add,
                        old: None,
                        new: Some(new_no - 1),
                        text,
                    }
                }
                Some(b'-') => {
                    old_no += 1;
                    Line {
                        sign: Sign::Del,
                        old: Some(old_no - 1),
                        new: None,
                        text,
                    }
                }
                _ => {
                    old_no += 1;
                    new_no += 1;
                    Line {
                        sign: Sign::Ctx,
                        old: Some(old_no - 1),
                        new: Some(new_no - 1),
                        text,
                    }
                }
            };
            rows.push(DiffRow::Line(line));
            continue;
        }
        in_hunk = false;
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            rows.push(DiffRow::File(b_path_of(rest).to_string()));
        } else if raw.starts_with("@@") {
            if let Some((o, n)) = hunk_starts(raw) {
                old_no = o;
                new_no = n;
                in_hunk = true;
            }
            rows.push(DiffRow::Hunk(raw.to_string()));
        } else if [
            "new file",
            "deleted file",
            "rename from",
            "rename to",
            "copy from",
            "copy to",
            "Binary files ",
        ]
        .iter()
        .any(|p| raw.starts_with(p))
        {
            rows.push(DiffRow::Note(raw.to_string()));
        }
        // 其余头部行（index / mode / --- / +++）不展示
    }
    rows
}

/// `@@ -12,7 +12,8 @@` → (12, 12)
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let first = |s: &str| s.split(',').next().and_then(|n| n.parse::<u32>().ok());
    Some((first(old)?, first(new)?))
}

/// 从 `diff --git a/x b/y` 里取 b 侧路径。路径含空格时 a/b 边界本就有歧义，按最后一个
/// " b/" 切足够准（左边是 a 路径，git 不会在 b 路径后再拼别的）。
fn b_path_of(rest: &str) -> &str {
    match rest.rfind(" b/") {
        Some(at) => &rest[at + 3..],
        None => rest,
    }
}

/// 单文件段时不重复画文件名条——标题上已经有了；重命名等多段 diff 才需要分隔
pub fn drop_single_file_header(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let files = rows.iter().filter(|r| matches!(r, DiffRow::File(_))).count();
    if files > 1 {
        rows
    } else {
        rows.into_iter().filter(|r| !matches!(r, DiffRow::File(_))).collect()
    }
}

/// 把 unified 行配成左右两栏：hunk 里连续的 − 段与紧随的 + 段按序号对齐（第 i 行删除对
/// 第 i 行新增），上下文两边同现。
pub fn to_split_rows(rows: &[DiffRow]) -> Vec<SplitRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut dels: Vec<SideLine> = Vec::new();
    let mut adds: Vec<SideLine> = Vec::new();
    fn flush(out: &mut Vec<SplitRow>, dels: &mut Vec<SideLine>, adds: &mut Vec<SideLine>) {
        let n = dels.len().max(adds.len());
        let mut d = dels.drain(..);
        let mut a = adds.drain(..);
        for _ in 0..n {
            out.push(SplitRow::Pair {
                left: d.next(),
                right: a.next(),
            });
        }
    }
    for row in rows {
        match row {
            DiffRow::Line(l) => match l.sign {
                Sign::Del => dels.push(SideLine {
                    no: l.old.unwrap_or(0),
                    text: l.text.clone(),
                    sign: Sign::Del,
                }),
                Sign::Add => adds.push(SideLine {
                    no: l.new.unwrap_or(0),
                    text: l.text.clone(),
                    sign: Sign::Add,
                }),
                Sign::Ctx => {
                    flush(&mut out, &mut dels, &mut adds);
                    out.push(SplitRow::Pair {
                        left: Some(SideLine {
                            no: l.old.unwrap_or(0),
                            text: l.text.clone(),
                            sign: Sign::Ctx,
                        }),
                        right: Some(SideLine {
                            no: l.new.unwrap_or(0),
                            text: l.text.clone(),
                            sign: Sign::Ctx,
                        }),
                    });
                }
            },
            other => {
                flush(&mut out, &mut dels, &mut adds);
                out.push(match other {
                    DiffRow::File(s) => SplitRow::File(s.clone()),
                    DiffRow::Hunk(s) => SplitRow::Hunk(s.clone()),
                    DiffRow::Note(s) => SplitRow::Note(s.clone()),
                    DiffRow::Line(_) => unreachable!(),
                });
            }
        }
    }
    flush(&mut out, &mut dels, &mut adds);
    out
}

/// 估一行文字的显示宽度（列数）：CJK 等宽字符算 2 列。只用来挑"最宽的那一行"给
/// uniform_list 量内容宽度，不需要精确
pub fn display_cols(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if (0x1100..=0x115F).contains(&u)
                || (0x2E80..=0xA4CF).contains(&u)
                || (0xAC00..=0xD7A3).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFE30..=0xFE4F).contains(&u)
                || (0xFF00..=0xFF60).contains(&u)
                || (0xFFE0..=0xFFE6).contains(&u)
                || (0x1F300..=0x1FAFF).contains(&u)
                || (0x20000..=0x3FFFD).contains(&u)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/x.ts b/x.ts\nindex 1..2 100644\n--- a/x.ts\n+++ b/x.ts\n@@ -1,3 +1,4 @@\n a\n-b\n+B\n+C\n c\n\\ No newline at end of file\n";

    #[test]
    fn parses_rows_and_line_numbers() {
        let rows = parse_diff(SAMPLE);
        assert_eq!(rows[0], DiffRow::File("x.ts".into()));
        assert_eq!(rows[1], DiffRow::Hunk("@@ -1,3 +1,4 @@".into()));
        assert_eq!(
            rows[2],
            DiffRow::Line(Line {
                sign: Sign::Ctx,
                old: Some(1),
                new: Some(1),
                text: "a".into()
            })
        );
        assert_eq!(
            rows[3],
            DiffRow::Line(Line {
                sign: Sign::Del,
                old: Some(2),
                new: None,
                text: "b".into()
            })
        );
        assert_eq!(
            rows[4],
            DiffRow::Line(Line {
                sign: Sign::Add,
                old: None,
                new: Some(2),
                text: "B".into()
            })
        );
        assert_eq!(
            rows[5],
            DiffRow::Line(Line {
                sign: Sign::Add,
                old: None,
                new: Some(3),
                text: "C".into()
            })
        );
        assert_eq!(
            rows[6],
            DiffRow::Line(Line {
                sign: Sign::Ctx,
                old: Some(3),
                new: Some(4),
                text: "c".into()
            })
        );
        assert_eq!(rows[7], DiffRow::Note("No newline at end of file".into()));
        assert_eq!(rows.len(), 8);
        assert_eq!(drop_single_file_header(rows).len(), 7);
    }

    #[test]
    fn split_pairs_deletions_with_additions() {
        let split = to_split_rows(&drop_single_file_header(parse_diff(SAMPLE)));
        assert!(matches!(&split[0], SplitRow::Hunk(_)));
        // a | a
        assert!(matches!(&split[1], SplitRow::Pair { left: Some(l), right: Some(r) } if l.no == 1 && r.no == 1));
        // b | B
        assert!(
            matches!(&split[2], SplitRow::Pair { left: Some(l), right: Some(r) } if l.text == "b" && r.text == "B")
        );
        // _ | C
        assert!(matches!(&split[3], SplitRow::Pair { left: None, right: Some(r) } if r.no == 3));
        // c | c
        assert!(matches!(&split[4], SplitRow::Pair { left: Some(l), right: Some(r) } if l.no == 3 && r.no == 4));
        assert!(matches!(&split[5], SplitRow::Note(_)));
    }

    #[test]
    fn keeps_notes_and_multi_file_headers() {
        let text = "diff --git a/old name.ts b/new name.ts\nsimilarity index 90%\nrename from old name.ts\nrename to new name.ts\ndiff --git a/img.png b/img.png\nnew file mode 100644\nBinary files /dev/null and b/img.png differ\n";
        let rows = drop_single_file_header(parse_diff(text));
        assert_eq!(rows[0], DiffRow::File("new name.ts".into()));
        assert_eq!(rows[1], DiffRow::Note("rename from old name.ts".into()));
        assert_eq!(rows[3], DiffRow::File("img.png".into()));
        assert_eq!(rows.len(), 6);
    }

    #[test]
    fn empty_context_lines_and_tabs() {
        let rows = parse_diff("@@ -5 +5,2 @@\n\n+\tx\n");
        assert_eq!(
            rows[1],
            DiffRow::Line(Line {
                sign: Sign::Ctx,
                old: Some(5),
                new: Some(5),
                text: String::new()
            })
        );
        assert_eq!(
            rows[2],
            DiffRow::Line(Line {
                sign: Sign::Add,
                old: None,
                new: Some(6),
                text: "        x".into()
            })
        );
        assert_eq!(expand_tabs("ab\tc"), "ab      c");
        assert_eq!(display_cols("a中b"), 4);
    }
}
