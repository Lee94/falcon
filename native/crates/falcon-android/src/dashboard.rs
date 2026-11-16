//! 安卓端列表、项目树、会话自动标题与主题语义色导出：复用 [`falcon_core`] 与 [`falcon_theme`]。

use falcon_core::program_status::{BadgeTone, badge};
use falcon_core::project_tree::group_servers;
use falcon_core::session_title::session_label;
use falcon_proto::{
    DeadReason, NonDurableReason, ProgramStatusSummary, Project, SessionWithProject, SshHost,
};
use falcon_theme::{Appearance, load_catalog};
use serde_json::{Value, json};

use crate::term::resolve_theme_by_name;

/// 将服务端的 `/api/projects`、`/api/sessions`、`/api/hosts` 响应聚合为安卓端直接渲染的分组树，
/// 同时计算每个会话的显示标题（`session_label`）、程序状态徽标（`program_status::badge`）与状态说明。
pub fn build_dashboard_view(projects_json: &str, sessions_json: &str, hosts_json: &str) -> String {
    let projects: Vec<Project> = serde_json::from_str(projects_json).unwrap_or_default();
    let sessions: Vec<SessionWithProject> = serde_json::from_str(sessions_json).unwrap_or_default();
    let hosts: Vec<SshHost> = serde_json::from_str(hosts_json).unwrap_or_default();

    let groups = group_servers(&projects, &hosts, "本机", false);

    let session_to_json = |s: &SessionWithProject| -> Value {
        let label = session_label(&s.session, &projects);
        let status_badge = s.session.program_status.as_ref().and_then(badge_to_json);
        let dead_reason_text = s.session.dead_reason.map(dead_reason_label);
        let non_durable_text = s.session.non_durable_reason.map(non_durable_label);
        json!({
            "id": s.session.id,
            "projectId": s.session.project_id,
            "projectName": s.project_name,
            "projectType": s.project_type.as_str(),
            "name": s.session.name,
            "title": s.session.title,
            "displayLabel": label,
            "state": s.session.state.as_str(),
            "agent": s.session.agent.map(|a| a.as_str()),
            "durable": s.session.durable,
            "nonDurableReason": s.session.non_durable_reason.map(|r| r.as_str()),
            "nonDurableText": non_durable_text,
            "deadReason": s.session.dead_reason.map(|r| r.as_str()),
            "deadReasonText": dead_reason_text,
            "badge": status_badge,
            "createdAt": s.session.created_at,
            "lastActiveAt": s.session.last_active_at,
        })
    };

    let project_to_json = |p: &Project| -> Value {
        let proj_sessions: Vec<Value> = sessions
            .iter()
            .filter(|s| s.session.project_id == p.id)
            .map(session_to_json)
            .collect();
        json!({
            "id": p.id,
            "name": p.name,
            "type": p.project_type.as_str(),
            "workingDir": p.working_dir,
            "branch": p.worktree.as_ref().map(|w| w.branch.clone()),
            "isWorktree": p.worktree.is_some(),
            "isMulti": p.multi.is_some(),
            "sessions": proj_sessions,
        })
    };

    let server_groups: Vec<Value> = groups
        .into_iter()
        .map(|g| {
            let standalone_sessions: Vec<Value> = match &g.standalone_id {
                Some(sid) => sessions
                    .iter()
                    .filter(|s| &s.session.project_id == sid)
                    .map(session_to_json)
                    .collect(),
                None => Vec::new(),
            };
            let folders: Vec<Value> = g
                .folders
                .iter()
                .map(|f| {
                    let worktrees: Vec<Value> =
                        f.worktrees.iter().map(project_to_json).collect();
                    json!({
                        "project": project_to_json(&f.project),
                        "worktrees": worktrees,
                    })
                })
                .collect();
            json!({
                "key": g.key,
                "kind": g.kind.as_str(),
                "name": g.name,
                "conn": g.conn,
                "standaloneId": g.standalone_id,
                "standaloneSessions": standalone_sessions,
                "folders": folders,
            })
        })
        .collect();

    let all_sessions: Vec<Value> = sessions.iter().map(session_to_json).collect();

    json!({
        "servers": server_groups,
        "allSessions": all_sessions,
    })
    .to_string()
}

pub fn badge_to_json(summary: &ProgramStatusSummary) -> Option<Value> {
    let b = badge(summary)?;
    let tone = match b.tone {
        BadgeTone::Blocked => "blocked",
        BadgeTone::Error => "error",
        BadgeTone::Working => "working",
        BadgeTone::Done => "done",
    };
    let label = match b.label_key {
        "programStatus.blocked_permission" => "在等你授权",
        "programStatus.blocked_question" => "在等你回答",
        "programStatus.blocked_auth" => "在等你提供凭据",
        "programStatus.blocked" => "在等你操作",
        "programStatus.error" => "出错了",
        "programStatus.working" => "运行中",
        "programStatus.done" => "已完成",
        _ => "运行中",
    };
    let short = match b.short_key {
        "programStatus.short_permission" => "授权",
        "programStatus.short_question" => "回答",
        "programStatus.short_auth" => "凭据",
        "programStatus.short_blocked" => "等你",
        "programStatus.short_error" => "出错",
        "programStatus.short_working" => "运行中",
        "programStatus.short_done" => "完成",
        _ => "状态",
    };
    Some(json!({
        "tone": tone,
        "label": label,
        "shortLabel": short,
        "progress": b.progress,
        "detail": b.detail,
    }))
}

fn dead_reason_label(reason: DeadReason) -> &'static str {
    match reason {
        DeadReason::Exited => "shell 已退出",
        DeadReason::BackendRestart => "后端重启（该会话非持久）",
        DeadReason::LinkLost => "SSH 链路断开（该会话非持久）",
        DeadReason::SessionGone => "宿主机上找不到这个会话了",
        DeadReason::Unknown => "会话已结束",
    }
}

fn non_durable_label(reason: NonDurableReason) -> &'static str {
    match reason {
        NonDurableReason::ArchUnsupported => "宿主机的系统或架构暂不支持会话守护进程",
        NonDurableReason::DirNotWritable => "无法在宿主机家目录创建 .falcon 目录",
        NonDurableReason::ProbeFailed => "无法探测宿主机环境",
        NonDurableReason::UploadFailed => "上传会话守护进程时中断",
        NonDurableReason::VerifyFailed => "会话守护进程无法在宿主机上运行",
        NonDurableReason::Cancelled => "部署已取消",
        NonDurableReason::SessiondUnavailable => "服务端构建未内嵌对应架构的会话守护进程",
        NonDurableReason::NotAuthorized => "尚未授权在该主机部署会话守护进程",
        NonDurableReason::WindowsJobObject => "后端处于 Windows Job Object 中",
        NonDurableReason::Unknown => "非持久会话",
    }
}

/// 导出主题派生出的全套界面语义 token 与终端配色 JSON，供安卓端 Compose 主题与新建会话参数使用。
pub fn export_theme_tokens(theme_name: &str) -> String {
    let resolved = resolve_theme_by_name(theme_name);
    let ui = &resolved.ui;
    let term = &resolved.terminal;
    let appearance = match resolved.appearance {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    };
    json!({
        "name": if theme_name.trim().is_empty() { "Falcon Dark" } else { theme_name.trim() },
        "isDark": resolved.is_dark,
        "appearance": appearance,
        "backgroundHex": resolved.hint.background.to_hex(),
        "foregroundHex": resolved.hint.foreground.to_hex(),
        "ui": {
            "background": ui.background.to_hex(),
            "foreground": ui.foreground.to_hex(),
            "app": ui.app.to_hex(),
            "card": ui.card.to_hex(),
            "cardForeground": ui.card_foreground.to_hex(),
            "primary": ui.primary.to_hex(),
            "primaryForeground": ui.primary_foreground.to_hex(),
            "secondary": ui.secondary.to_hex(),
            "secondaryForeground": ui.secondary_foreground.to_hex(),
            "muted": ui.muted.to_hex(),
            "mutedForeground": ui.muted_foreground.to_hex(),
            "accent": ui.accent.to_hex(),
            "accentForeground": ui.accent_foreground.to_hex(),
            "tint": ui.tint.to_hex(),
            "tintForeground": ui.tint_foreground.to_hex(),
            "tintStrong": ui.tint_strong.to_hex(),
            "destructive": ui.destructive.to_hex(),
            "success": ui.success.to_hex(),
            "warning": ui.warning.to_hex(),
            "sidebar": ui.sidebar.to_hex(),
            "sidebarForeground": ui.sidebar_foreground.to_hex(),
        },
        "terminal": {
            "background": term.background.to_hex(),
            "foreground": term.foreground.to_hex(),
            "cursor": term.cursor.to_hex(),
            "cursorAccent": term.cursor_accent.to_hex(),
            "selectionBackground": term.selection_background.to_hex(),
        }
    })
    .to_string()
}

/// 导出内置主题列表（Falcon 默认浅/深色 + 精选常用 Ghostty 主题及完整目录检索）。
pub fn list_available_themes(query: &str) -> String {
    let q = query.trim().to_lowercase();
    let catalog = load_catalog();
    let items: Vec<Value> = catalog
        .iter()
        .filter(|entry| q.is_empty() || entry.name.to_lowercase().contains(&q))
        .take(120)
        .map(|entry| {
            let is_dark = matches!(entry.appearance, Appearance::Dark);
            json!({
                "name": entry.name,
                "isDark": is_dark,
                "background": entry.colors.background.to_hex(),
                "foreground": entry.colors.foreground.to_hex(),
                "accent": entry.colors.palette[4].to_hex(),
            })
        })
        .collect();
    Value::Array(items).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_groups_projects_and_computes_labels() {
        let projects = r#"[{"id":"p1","name":"mojito","type":"local","workingDir":"/Users/fay/Code/mojito","shell":"/bin/zsh","createdAt":1}]"#;
        let sessions = r#"[{"id":"s1","projectId":"p1","name":"","title":"cargo test","state":"active","durable":true,"createdAt":1,"lastActiveAt":2,"projectName":"mojito","projectType":"local"}]"#;
        let out = build_dashboard_view(projects, sessions, "[]");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed["allSessions"][0]["displayLabel"].as_str(),
            Some("cargo test")
        );
        assert_eq!(
            parsed["servers"][0]["folders"][0]["project"]["name"].as_str(),
            Some("mojito")
        );
    }

    #[test]
    fn theme_tokens_and_catalog_work() {
        let dark = export_theme_tokens("Falcon Dark");
        let v: Value = serde_json::from_str(&dark).unwrap();
        assert_eq!(v["isDark"], Value::Bool(true));
        assert_eq!(v["appearance"].as_str(), Some("dark"));

        let list = list_available_themes("falcon");
        let arr: Vec<Value> = serde_json::from_str(&list).unwrap();
        assert!(arr.len() >= 2);
    }
}
