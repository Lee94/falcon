//! 存档附属项目的到期清扫：archived_at 超过 WORKTREE_ARCHIVE_TTL_MS 的行，
//! 自动删除项目并清理 worktree 目录。启动时跑一次，此后每小时一轮。移植自
//! `packages/server/src/archive.ts`。
//!
//! 与手动删除（DELETE /api/projects/:id）刻意不同的一点：那边"DB 行无条件删，
//! 文件系统清理 best-effort"，因为 warning 会原样摆到用户面前；这边没人在看——
//! **目录还在就把行留着**，下一轮再试。链路抖动（SSH 不在线）自愈；删不掉的
//! 目录（worktree lock、只读文件）让项目留在"已存档"列表里，用户手动删除时
//! 能看到具体原因。绝不静默地把目录变成孤儿。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use falcon_proto::WORKTREE_ARCHIVE_TTL_MS;

use crate::auth::now_ms;
use crate::db::{Db, ProjectRow};
use crate::engine::Engine;
use crate::git::error::WorktreeError;
use crate::git::remove::cleanup_worktree;
use crate::git::repo::{RunOpts, TIMEOUT_REMOVE, path_exists};

const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 在会话引擎上起清扫（启动时一轮，此后每小时一轮）。返回的句柄 abort 即停
pub fn start_archive_sweeper(engine: &Rc<Engine>) -> tokio::task::AbortHandle {
    let engine = engine.clone();
    let sweeping = Rc::new(Cell::new(false));
    tokio::task::spawn_local(async move {
        let mut tick = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            tick.tick().await;
            if sweeping.replace(true) {
                continue;
            }
            sweep(&engine).await;
            sweeping.set(false);
        }
    })
    .abort_handle()
}

async fn sweep(engine: &Engine) {
    let expired = engine.db.list_archived_expired(now_ms() - WORKTREE_ARCHIVE_TTL_MS);
    if expired.is_empty() {
        return;
    }
    let doomed: Vec<&str> = expired.iter().map(|p| p.id.as_str()).collect();
    // guard_dirs_of：与手动删除同一份"别删到我"清单（含多仓库项目的成员路径）
    let other_dirs: Vec<String> = engine
        .db
        .list_projects()
        .iter()
        .filter(|p| !doomed.contains(&p.id.as_str()))
        .flat_map(Db::guard_dirs_of)
        .collect();
    for row in &expired {
        // 一台宿主机连不上不能拖垮整轮清扫，也不能删行——目录清没清成还不知道
        if let Err(e) = sweep_one(engine, row, &other_dirs).await {
            log::warn!("清扫存档附属项目「{}」失败，保留待下一轮重试：{}", row.name, e.message);
        }
    }
}

async fn sweep_one(engine: &Engine, row: &ProjectRow, other_dirs: &[String]) -> Result<(), WorktreeError> {
    // 存档时会话已随手清掉；这里再兜一遍——绝不带着活进程删目录
    for s in engine.db.list_sessions_by_project(&row.id) {
        if s.state == "dead" {
            engine.sessions.delete_dead(&s.id);
        } else {
            engine.sessions.terminate(&s.id, true).await;
        }
    }

    // 没有目录可清的脏数据行，直接删掉了事
    let Some(working_dir) = row.working_dir.as_deref().filter(|d| !d.is_empty()) else {
        engine.sessions.dispose_link(&row.id);
        engine.db.delete_project(&row.id);
        return Ok(());
    };

    let host = engine.git_host(row).await?;
    let warnings = cleanup_worktree(row, &host, other_dirs).await;
    let gone = !path_exists(&host, working_dir, &RunOpts::timeout(TIMEOUT_REMOVE)).await.unwrap_or(true);
    if !gone {
        let tail = if warnings.is_empty() { String::new() } else { format!("（{}）", warnings.join("；")) };
        log::warn!("存档到期的附属项目「{}」目录未能清理，保留待下一轮重试：{working_dir}{tail}", row.name);
        return Ok(());
    }
    engine.sessions.dispose_link(&row.id);
    engine.db.delete_project(&row.id);
    log::info!("已自动删除存档到期的附属项目「{}」（{working_dir}）", row.name);
    Ok(())
}
