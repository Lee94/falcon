//! 飞书项目列表的分组、本地翻页与过滤。对应旧 React 版的 `lib/meegleGroups.ts`。
//!
//! 三件事都只作用于**手上这一页**（搜索 / 最近的快照），不去猜远端的总数。

use falcon_proto::{MEEGLE_PAGE_SIZE, MeegleTodoItem, MeegleWorkItem, MeegleWorkItemDetail};

use crate::js::js_trim;

/// 列表的一行：工作项本体，外加待办独有的两个字段（普通工作项没有）
pub trait MeegleRow {
    fn work_item(&self) -> &MeegleWorkItem;
    fn node_name(&self) -> Option<&str> {
        None
    }
    fn state_name(&self) -> Option<&str> {
        None
    }
}

impl MeegleRow for MeegleWorkItem {
    fn work_item(&self) -> &MeegleWorkItem {
        self
    }
}

impl MeegleRow for MeegleTodoItem {
    fn work_item(&self) -> &MeegleWorkItem {
        &self.item
    }
    fn node_name(&self) -> Option<&str> {
        self.node_name.as_deref()
    }
    fn state_name(&self) -> Option<&str> {
        self.state_name.as_deref()
    }
}

impl MeegleRow for MeegleWorkItemDetail {
    fn work_item(&self) -> &MeegleWorkItem {
        &self.item
    }
}

impl<T: MeegleRow> MeegleRow for &T {
    fn work_item(&self) -> &MeegleWorkItem {
        (*self).work_item()
    }
    fn node_name(&self) -> Option<&str> {
        (*self).node_name()
    }
    fn state_name(&self) -> Option<&str> {
        (*self).state_name()
    }
}

/// 一个分组：业务线 → 类型 → 状态三层，最内层带行
#[derive(Debug, Clone, PartialEq)]
pub struct ItemGroup<'a, T> {
    /// `JSON.stringify(身份 ?? null)`：缺值的一组是 `null`，与真叫 "null" 的那组（`"\"null\""`）
    /// 不会撞
    pub key: String,
    pub label: Option<String>,
    pub count: usize,
    pub children: Option<Vec<ItemGroup<'a, T>>>,
    pub items: Option<Vec<&'a T>>,
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

/// 按业务线 / 类型 / 状态三层分组，只分手上这一页。缺值的归成一组，且不与真实的标签撞。
pub fn group_meegle_items<T: MeegleRow>(items: &[T]) -> Vec<ItemGroup<'_, T>> {
    fn group<'a, T: MeegleRow>(rows: Vec<&'a T>, level: u8) -> Vec<ItemGroup<'a, T>> {
        // 保持首次出现的顺序（JS 的 Map）
        let mut buckets: Vec<(String, Option<String>, Vec<&'a T>)> = Vec::new();
        for row in rows {
            let w = row.work_item();
            let raw = match level {
                0 => w.business.as_deref(),
                1 => non_empty(w.type_name.as_deref()).or(Some(w.type_key.as_str())),
                _ => w.status.as_deref(),
            };
            let label = raw.map(js_trim).filter(|s| !s.is_empty()).map(str::to_string);
            // 类型那层按 typeKey 认身份：同一个类型换了显示名也还是一组
            let identity = if level == 1 { non_empty(Some(w.type_key.as_str())).map(str::to_string).or_else(|| label.clone()) } else { label.clone() };
            let key = serde_json::to_string(&identity).unwrap_or_else(|_| "null".into());
            match buckets.iter_mut().find(|(k, _, _)| *k == key) {
                Some((_, _, rows)) => rows.push(row),
                None => buckets.push((key, label, vec![row])),
            }
        }
        buckets
            .into_iter()
            .map(|(key, label, rows)| {
                let count = rows.len();
                if level == 2 {
                    ItemGroup { key, label, count, children: None, items: Some(rows) }
                } else {
                    ItemGroup { key, label, count, children: Some(group(rows, level + 1)), items: None }
                }
            })
            .collect()
    }
    group(items.iter().collect(), 0)
}

/// 本地翻页的一页
#[derive(Debug, Clone, PartialEq)]
pub struct LocalPage<'a, T> {
    /// 从 1 起，已夹到 [1, pages]
    pub page: usize,
    pub pages: usize,
    pub items: &'a [T],
    pub has_more: bool,
}

/// 本地翻页只在返回的快照里翻（每页 [`MEEGLE_PAGE_SIZE`] 条），不代表远端总数。
/// `requested` 取整后夹到范围里；NaN / 0 当第 1 页。
pub fn meegle_page<T>(items: &[T], requested: f64) -> LocalPage<'_, T> {
    let size = MEEGLE_PAGE_SIZE as usize;
    let pages = items.len().div_ceil(size).max(1);
    let floored = requested.floor();
    // `Math.floor(requested) || 1`：NaN 与 0 都落到 1
    let wanted = if floored.is_nan() || floored == 0.0 { 1.0 } else { floored };
    let page = wanted.clamp(1.0, pages as f64) as usize;
    let start = ((page - 1) * size).min(items.len());
    let end = (page * size).min(items.len());
    LocalPage { page, pages, items: &items[start..end], has_more: page < pages }
}

/// 按关键字过滤（大小写不敏感），只看手上这一页；看名称、编号、空间、类型、节点、
/// 状态、业务线。空关键字原样返回全部（React 版返回同一个数组）。
pub fn filter_meegle_items<'a, T: MeegleRow>(items: &'a [T], filter: &str) -> Vec<&'a T> {
    let q = js_trim(filter).to_lowercase();
    if q.is_empty() {
        return items.iter().collect();
    }
    items
        .iter()
        .filter(|it| {
            let w = it.work_item();
            [
                Some(w.name.as_str()),
                Some(w.id.as_str()),
                w.space_name.as_deref(),
                w.type_name.as_deref(),
                it.node_name(),
                it.state_name(),
                w.status.as_deref(),
                w.business.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .any(|s| s.to_lowercase().contains(&q))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, space: &str, name: &str, business: Option<&str>, type_key: &str, status: Option<&str>) -> MeegleWorkItem {
        MeegleWorkItem {
            id: id.into(),
            name: name.into(),
            business: business.map(Into::into),
            space_key: space.into(),
            space_name: None,
            type_key: type_key.into(),
            type_name: None,
            status: status.map(Into::into),
            url: None,
            updated_at: None,
        }
    }

    #[test]
    fn business_type_status_groups_preserve_records_and_count_each_level() {
        let rows = vec![
            row("1", "a", "one", Some("B"), "story", Some("Open")),
            row("2", "b", "two", Some("B"), "story", Some("Done")),
            row("3", "a", "three", Some(" "), "story", None),
            row("4", "a", "four", Some("null"), "story", None),
        ];
        let groups = group_meegle_items(&rows);
        assert_eq!(groups.iter().map(|g| g.count).collect::<Vec<_>>(), [2, 1, 1]);
        let first_type = &groups[0].children.as_ref().unwrap()[0];
        assert_eq!(first_type.count, 2);
        let statuses: Vec<Option<&str>> =
            first_type.children.as_ref().unwrap().iter().map(|g| g.label.as_deref()).collect();
        assert_eq!(statuses, [Some("Open"), Some("Done")]);
        assert_eq!(groups[1].label, None);
        assert_ne!(groups[1].key, groups[2].key);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["1", "2", "3", "4"]);
    }

    #[test]
    fn pages_of_100_replace_rather_than_accumulate_and_clamp_bounds() {
        let rows: Vec<u32> = (0..201).collect();
        assert_eq!(meegle_page(&rows, 1.0).items.len(), 100);
        assert_eq!(meegle_page(&rows, 2.0).items, &rows[100..200]);
        assert_eq!(meegle_page(&rows, 99.0).items, [200]);
        assert!(!meegle_page(&rows, 3.0).has_more);
        assert_eq!(meegle_page(&rows, 0.0).page, 1);
        assert!(meegle_page::<u32>(&[], 8.0).items.is_empty());
        // 以下是 Rust 侧补的：NaN 当第 1 页、小数向下取整
        assert_eq!(meegle_page(&rows, f64::NAN).page, 1);
        assert_eq!(meegle_page(&rows, 2.9).page, 2);
    }

    #[test]
    fn filtering_is_case_insensitive_and_only_sees_its_page_including_business_and_todo_fields() {
        let rows: Vec<MeegleTodoItem> = (0..101)
            .map(|i| MeegleTodoItem {
                item: row(&i.to_string(), "a", &format!("item {i}"), Some(if i == 100 { "Payments" } else { "Core" }), "story", None),
                node_name: Some("Review".into()),
                state_name: None,
                schedule_start: None,
                schedule_end: None,
                finished_at: None,
            })
            .collect();
        assert!(filter_meegle_items(meegle_page(&rows, 1.0).items, "payments").is_empty());
        assert_eq!(filter_meegle_items(meegle_page(&rows, 2.0).items, " PAYMENTS ").len(), 1);
        assert_eq!(filter_meegle_items(&rows, "review").len(), 101);
        // 空关键字：原样全给（React 版返回同一个数组）
        let all = filter_meegle_items(&rows, " ");
        assert_eq!(all.len(), rows.len());
        assert!(all.iter().zip(&rows).all(|(a, b)| std::ptr::eq(*a, b)));
    }
}
