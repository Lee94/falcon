//! 移植自 `packages/server/src/meegle/pagination.ts`（含 `pagination.test.ts` 的全部用例）。
//!
//! 本身不做 I/O：取哪一页由调用方的 `load` 决定（[`super::client::MeegleClient`] 的
//! view_items / multi_view_items / todo 在里面起 CLI）。

use std::future::Future;

use falcon_proto::{MEEGLE_PAGE_SIZE, MeeglePage};

use super::command::CLI_PAGE_SIZE;

/// CLI 固定 50 条，REST 按 100 条组装；提前结束不再取后一页。
/// hasMore 必须来自最后一个实际取到的 CLI 页，不能用合并后条数猜。
///
/// `load` 的错误原样往上抛（TS 里是 reject），不吞、不拿半页充数。
///
/// `page` 从 1 起（路由的 pageOf 保证）；传 0 按 1 算而不是下溢（TS 会去取第 -1、0 页）。
pub async fn logical_page<T, E, F, Fut>(page: u32, mut load: F) -> Result<MeeglePage<T>, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<MeeglePage<T>, E>>,
{
    let count = MEEGLE_PAGE_SIZE / CLI_PAGE_SIZE;
    let mut items = Vec::new();
    let mut total = None;
    let mut has_more = false;
    for offset in 1..=count {
        let result = load(page.saturating_sub(1) * count + offset).await?;
        items.extend(result.items);
        // result.total ?? total
        total = result.total.or(total);
        has_more = result.has_more;
        if !has_more {
            break;
        }
    }
    Ok(MeeglePage { items, page, has_more, total })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use serde_json::{Value, json};

    use super::*;
    use crate::meegle::command::{normalize_multi_view_items, normalize_todo, normalize_view_items};

    /// 测试里的 load 都是立即就绪的 future，轮询一次必然完成
    fn ready<T>(fut: impl Future<Output = T>) -> T {
        match pin!(fut).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("测试里的 future 应当立即就绪"),
        }
    }

    type Never = std::convert::Infallible;

    /// logical page 2 requests physical 3 and 4; final CLI hasMore wins even for a short page
    #[test]
    fn logical_page_2_requests_physical_3_and_4() {
        let calls = RefCell::new(Vec::new());
        let result = ready(logical_page(2, |page| {
            calls.borrow_mut().push(page);
            async move { Ok::<_, Never>(MeeglePage { items: vec![page], page, has_more: true, total: Some(500) }) }
        }))
        .unwrap();
        assert_eq!(calls.into_inner(), vec![3, 4]);
        assert_eq!(result, MeeglePage { items: vec![3, 4], page: 2, has_more: true, total: Some(500) });
    }

    /// todo logical paging boundaries: ${total} rows（TS 里每个 total 一条用例，这里一条用例跑全部）
    #[test]
    fn todo_logical_paging_boundaries() {
        for total in [0u32, 1, 49, 50, 51, 99, 100, 101, 150, 200] {
            let calls = RefCell::new(Vec::new());
            let result = ready(logical_page(1, |page| {
                calls.borrow_mut().push(page);
                let count = total.saturating_sub((page - 1) * 50).min(50);
                let list: Vec<Value> = (0..count)
                    .map(|i| json!({ "work_item_info": { "work_item_id": ((page - 1) * 50 + i + 1).to_string() } }))
                    .collect();
                let data = json!({ "total": total, "list": list });
                async move { Ok::<_, Never>(normalize_todo(&data, page)) }
            }))
            .unwrap();
            assert_eq!(result.items.len(), total.min(100) as usize, "{total} rows");
            assert_eq!(result.has_more, total > 100, "{total} rows");
            assert_eq!(calls.into_inner(), if total <= 50 { vec![1] } else { vec![1, 2] }, "{total} rows");
        }
    }

    /// view and multi-view logical pages preserve last pagination flag and total
    #[test]
    fn view_and_multi_view_logical_pages_preserve_last_flag_and_total() {
        let view = |page: u32| {
            normalize_view_items(
                &json!({
                    "work_item_list": [{ "work_item_attribute": { "work_item_id": page.to_string() } }],
                    "pagination": { "has_more": page == 1, "total": 2 },
                }),
                None,
                page,
            )
        };
        let multi = |page: u32| {
            normalize_multi_view_items(
                &json!({
                    "data": [{ "work_item_id": page.to_string() }],
                    "pagination": { "has_more": page == 1, "total": 2 },
                }),
                page,
            )
        };
        let normalizers: [&dyn Fn(u32) -> MeeglePage<falcon_proto::MeegleWorkItem>; 2] = [&view, &multi];
        for normalize in normalizers {
            let result = ready(logical_page(1, |page| {
                let r = normalize(page);
                async move { Ok::<_, Never>(r) }
            }))
            .unwrap();
            assert_eq!(result.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
            assert!(!result.has_more);
            assert_eq!(result.total, Some(2));
        }
    }

    /// an empty terminal CLI page ends unknown-total todo pages; errors are not hidden
    #[test]
    fn empty_terminal_page_ends_unknown_total_and_errors_propagate() {
        let result = ready(logical_page(1, |page| {
            let list: Vec<Value> = if page == 1 {
                (0..50).map(|i| json!({ "work_item_info": { "work_item_id": i.to_string() } })).collect()
            } else {
                Vec::new()
            };
            let data = json!({ "list": list });
            async move { Ok::<_, Never>(normalize_todo(&data, page)) }
        }))
        .unwrap();
        assert_eq!(result.items.len(), 50);
        assert!(!result.has_more);
        let err = ready(logical_page(1, |_| async {
            Err::<MeeglePage<falcon_proto::MeegleTodoItem>, _>("CLI unavailable".to_string())
        }));
        assert!(matches!(err, Err(e) if e.contains("CLI unavailable")));
    }
}
