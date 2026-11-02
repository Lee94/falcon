//! px0 在 falcon 源下的挂载前缀（ADR 0017）。对应 shared 的 `px0BasePath`。
//!
//! 原生客户端没有嵌 px0：菜单项把 `<服务地址><前缀>` 交给系统浏览器。

use falcon_proto::Project;

use crate::js::encode_uri_component;

/// `/px0/<项目 id>/`。服务端起 px0 时的 `-base-path`、反代路由、客户端菜单里开的地址都是它。
///
/// 多仓库项目在这里是选成员的页面，px0 按成员各起一个，挂在 [`px0_member_base_path`]
pub fn px0_base_path(project_id: &str) -> String {
    format!("/px0/{}/", encode_uri_component(project_id))
}

/// `/px0/<项目 id>/<成员序号>/`：多仓库项目某个成员仓库的 px0。序号是 `multi.repos` 的下标
pub fn px0_member_base_path(project_id: &str, member: usize) -> String {
    format!("{}{member}/", px0_base_path(project_id))
}

/// 「修改」面板上「用 px0 审阅」打开哪里。多仓库项目直接进 `member` 那个成员（面板顶上选着的
/// 成员路径；对不上就第一个），省掉服务端的选成员页；普通项目进整个工作目录，没有工作目录
/// 就没有东西可看（None，不给按钮）。
///
/// 序号按客户端手里的成员列表算：成员在别处刚改过、这边还没刷新时可能指到别的仓库，px0 的
/// 页面标题带着成员名，看得出来（ADR 0017 决定九）
pub fn px0_review_path(project: &Project, member: Option<&str>) -> Option<String> {
    if let Some(multi) = project.multi.as_ref().filter(|m| !m.repos.is_empty()) {
        let i = member.and_then(|dir| multi.repos.iter().position(|m| m.dir == dir)).unwrap_or(0);
        return Some(px0_member_base_path(&project.id, i));
    }
    project.working_dir.as_deref().is_some_and(|d| !d.trim().is_empty()).then(|| px0_base_path(&project.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_shared_px0_base_path() {
        assert_eq!(px0_base_path("d7697ab3-292d-4ef9-a8de-b10c4ebd962a"), "/px0/d7697ab3-292d-4ef9-a8de-b10c4ebd962a/");
        assert_eq!(px0_base_path("a/b c"), "/px0/a%2Fb%20c/");
        assert_eq!(px0_member_base_path("a/b c", 3), "/px0/a%2Fb%20c/3/");
    }

    #[test]
    fn review_path_goes_straight_to_the_selected_member() {
        use falcon_proto::{MultiRepoInfo, MultiRepoMember};
        let member = |dir: &str| MultiRepoMember { dir: dir.into(), repo_dir: None };
        let mut p: Project =
            serde_json::from_str(r#"{"id":"p1","name":"p","type":"local","workingDir":"/w","createdAt":1}"#).unwrap();
        assert_eq!(px0_review_path(&p, None).as_deref(), Some("/px0/p1/"));
        p.working_dir = Some("  ".into());
        assert_eq!(px0_review_path(&p, None), None);

        // 多仓库：有没有工作目录都按成员走；选中的对不上（成员刚删）就回到第一个
        p.multi = Some(MultiRepoInfo { repos: vec![member("/a/web"), member("/a/api")] });
        assert_eq!(px0_review_path(&p, Some("/a/api")).as_deref(), Some("/px0/p1/1/"));
        assert_eq!(px0_review_path(&p, None).as_deref(), Some("/px0/p1/0/"));
        assert_eq!(px0_review_path(&p, Some("/gone")).as_deref(), Some("/px0/p1/0/"));
        // 成员是空表（坏数据）当普通项目看
        p.multi = Some(MultiRepoInfo { repos: vec![] });
        assert_eq!(px0_review_path(&p, None), None);
    }
}
