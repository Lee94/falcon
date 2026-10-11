//! History 左侧那条提交图的泳道布局。对应旧 React 版的 `lib/gitGraph.ts`，纯函数。
//!
//! 输入是服务端按 `--date-order` 给的一页提交（新 → 旧），输出每行画什么：圆点落在
//! 第几条泳道、以及经过这一行的线段怎么走。渲染层只管把这些序号翻译成坐标，一行一个
//! 独立的小图——subject 可能换行导致行高不等，一整张贯穿的大图会跟行对不齐。
//!
//! 算法是 gitk 那一套的简化版：维护一个"泳道 → 我在等哪个 sha"的数组，
//! 每处理一条提交就把它占的泳道换成它的第一个父提交，其余父提交各开一条。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// 一行里的一段线。序号是泳道号，渲染层再乘泳道宽度
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSegment {
    /// 从行顶部进来的泳道；从圆点出发的线为 `None`
    pub from: Option<usize>,
    /// 往行底部去的泳道；汇入圆点的线为 `None`
    pub to: Option<usize>,
    /// 上色用的泳道号：斜线跟着它连去的那条泳道走
    pub lane: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    /// 圆点所在泳道
    pub lane: usize,
    /// 这一行占了多少条泳道（含圆点），渲染层据此算图形区宽度
    pub width: usize,
    pub segments: Vec<GraphSegment>,
    /// 合并提交（多于一个父）。图上画成空心点，与 git 图形客户端的惯例一致
    pub merge: bool,
}

/// 一条提交的最小面：sha 与父提交（完整 sha，第一个是 first parent）。
/// `falcon_proto::GitLogCommit` 实现了它。
pub trait GraphInput {
    fn sha(&self) -> &str;
    fn parents(&self) -> &[String];
}

impl GraphInput for falcon_proto::GitLogCommit {
    fn sha(&self) -> &str {
        &self.sha
    }
    fn parents(&self) -> &[String] {
        &self.parents
    }
}

/// 测试与简单调用用的平铺形状（TS 的 `GraphInput`）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GraphCommit {
    pub sha: String,
    pub parents: Vec<String>,
}

impl GraphInput for GraphCommit {
    fn sha(&self) -> &str {
        &self.sha
    }
    fn parents(&self) -> &[String] {
        &self.parents
    }
}

/// 每行的泳道与线段。
///
/// **只连本页里真实存在的父提交**。两个理由，缺一个都会画出错的图：
/// - 翻页边界上，最后几条的父提交在下一页。照连的话页尾会挂着几根通向
///   空白的竖线，看起来像历史断在这里。
/// - 搜索 / 按作者筛选时，结果之间根本没有父子关系。照连的话每条提交都要
///   开一条新泳道，一页下来就是一道六十级的阶梯，还全是断线。
pub fn layout_commit_graph<C: GraphInput>(commits: &[C]) -> Vec<GraphRow> {
    let present: HashSet<&str> = commits.iter().map(|c| c.sha()).collect();
    // lanes[i] = 第 i 条泳道正在等的 sha；None = 这条泳道空着
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    fn first_free(lanes: &mut Vec<Option<String>>) -> usize {
        if let Some(i) = lanes.iter().position(Option::is_none) {
            return i;
        }
        lanes.push(None);
        lanes.len() - 1
    }

    for commit in commits {
        let sha = commit.sha();
        // 本行开始前的样子：进来的线段都取自它
        let before = lanes.clone();

        // 谁在等这条提交？第一条泳道归它，其余的在本行汇入后关闭
        let waiting: Vec<usize> =
            before.iter().enumerate().filter(|(_, s)| s.as_deref() == Some(sha)).map(|(i, _)| i).collect();
        let lane = match waiting.first() {
            Some(&i) => i,
            None => first_free(&mut lanes),
        };
        for &i in waiting.iter().skip(1) {
            lanes[i] = None;
        }

        // 从圆点往下走的泳道。第一个父继承本泳道，其余的（合并进来的那些分支）各开一条
        let parents: Vec<&String> = commit.parents().iter().filter(|p| present.contains(p.as_str())).collect();
        // 用 Vec 保持插入顺序（TS 的 Set），重复插入忽略
        let mut outgoing: Vec<usize> = Vec::new();
        lanes[lane] = parents.first().map(|p| (*p).clone());
        // `if (parents[0])`：空串 sha 在 JS 里是假，照抄
        if parents.first().is_some_and(|p| !p.is_empty()) {
            outgoing.push(lane);
        }
        for parent in parents.iter().skip(1) {
            // 已经有泳道在等这个父提交就复用，否则同一条历史会被画成两根平行线
            let target = match lanes.iter().position(|s| s.as_deref() == Some(parent.as_str())) {
                Some(i) => i,
                None => first_free(&mut lanes),
            };
            lanes[target] = Some((*parent).clone());
            if !outgoing.contains(&target) {
                outgoing.push(target);
            }
        }

        // 尾部的空泳道收掉，免得 width 被历史最大值一直撑着
        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }

        let mut segments = Vec::new();
        for (i, s) in before.iter().enumerate() {
            let Some(s) = s else { continue };
            if s == sha {
                // 等的就是本行这条提交：线走到圆点为止
                segments.push(GraphSegment { from: Some(i), to: None, lane });
            } else {
                // 与本行无关：直着穿过去。泳道号中途不会变（只有 first_free 分配时才动，
                // 而那只发生在空位上），所以穿过去的线永远是竖直的
                segments.push(GraphSegment { from: Some(i), to: Some(i), lane: i });
            }
        }
        for i in outgoing {
            segments.push(GraphSegment { from: None, to: Some(i), lane: i });
        }

        rows.push(GraphRow {
            lane,
            width: before.len().max(lanes.len()).max(lane + 1),
            segments,
            merge: commit.parents().len() > 1,
        });
    }

    rows
}

/// 泳道配色的色数。
///
/// 按泳道序号取，不按分支——一条提交上可能没有任何 ref，"它属于哪个分支"
/// 在 log 输出里根本不成立，而泳道是画面上唯一稳定的东西。第 0 条泳道永远
/// 同一个颜色，于是主线在整页上颜色一致，这正是扫一眼时要的信息。
pub const GRAPH_COLOR_COUNT: usize = 6;

/// 泳道用第几号图色（1..=6）。React 版是 `var(--graph-N)`，这里只给 N，颜色由主题出
pub fn lane_color(lane: usize) -> usize {
    lane % GRAPH_COLOR_COUNT + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_colors_cycle_from_one() {
        assert_eq!(lane_color(0), 1);
        assert_eq!(lane_color(5), 6);
        assert_eq!(lane_color(6), 1);
    }

    #[test]
    fn works_on_proto_log_commits() {
        let page: Vec<falcon_proto::GitLogCommit> = serde_json::from_str(
            r#"[{"sha":"b","short":"b","author":"a","authorEmail":"a@x","authoredAt":0,"subject":"s","parents":["a"],"refs":[]},
                {"sha":"a","short":"a","author":"a","authorEmail":"a@x","authoredAt":0,"subject":"s","parents":[],"refs":[]}]"#,
        )
        .unwrap();
        let rows = layout_commit_graph(&page);
        assert_eq!(rows.iter().map(|r| r.lane).collect::<Vec<_>>(), [0, 0]);
    }
}
