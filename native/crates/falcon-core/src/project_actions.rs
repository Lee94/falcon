use falcon_proto::{Project, SessionState};

pub fn affected_project_ids<'a>(
    project: &Project,
    projects: impl IntoIterator<Item = &'a Project>,
) -> Vec<String> {
    let mut ids = vec![project.id.clone()];
    if project.worktree.is_none() {
        ids.extend(
            projects
                .into_iter()
                .filter(|p| {
                    p.worktree
                        .as_ref()
                        .is_some_and(|w| w.source_project_id == project.id)
                })
                .map(|p| p.id.clone()),
        );
    }
    ids
}

pub fn project_action_snapshot<'a>(
    ids: &[String],
    projects: impl IntoIterator<Item = &'a Project>,
    sessions: impl IntoIterator<Item = (&'a str, &'a str, SessionState)>,
) -> String {
    let mut projects: Vec<_> = projects
        .into_iter()
        .filter(|p| ids.contains(&p.id))
        .collect();
    projects.sort_by(|a, b| a.id.cmp(&b.id));
    let mut live: Vec<_> = sessions
        .into_iter()
        .filter(|(_, project_id, state)| {
            ids.iter().any(|id| id == project_id) && *state != SessionState::Dead
        })
        .map(|(id, project_id, _)| (id, project_id))
        .collect();
    live.sort_unstable();
    serde_json::to_string(&(projects, live)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(id: &str, source: Option<&str>) -> Project {
        serde_json::from_value(json!({
            "id": id, "name": id, "type": "local", "workingDir": format!("/tmp/{id}"),
            "createdAt": 0, "updatedAt": 0,
            "worktree": source.map(|source| json!({
                "sourceProjectId":source,"branch":"feature","repoDir":"/tmp/root","createdByFalcon":true
            }))
        })).unwrap()
    }

    #[test]
    fn deleting_root_selects_only_its_direct_worktrees() {
        let projects = [
            project("root", None),
            project("child", Some("root")),
            project("other", None),
        ];
        assert_eq!(
            affected_project_ids(&projects[0], &projects),
            ["root", "child"]
        );
        assert_eq!(affected_project_ids(&projects[1], &projects), ["child"]);
    }

    #[test]
    fn snapshot_ignores_order_unrelated_projects_and_dead_sessions() {
        let root = project("root", None);
        let child = project("child", Some("root"));
        let other = project("other", None);
        let ids = vec!["root".into(), "child".into()];
        let a = project_action_snapshot(
            &ids,
            [&root, &child, &other],
            [
                ("a", "root", SessionState::Active),
                ("b", "child", SessionState::Unverified),
                ("dead", "root", SessionState::Dead),
                ("other", "other", SessionState::Active),
            ],
        );
        let b = project_action_snapshot(
            &ids,
            [&child, &root],
            [
                ("b", "child", SessionState::Unverified),
                ("a", "root", SessionState::Active),
            ],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn snapshot_changes_when_project_or_live_session_changes() {
        let mut root = project("root", None);
        let ids = vec!["root".into()];
        let initial = project_action_snapshot(&ids, [&root], []);
        assert_ne!(
            initial,
            project_action_snapshot(&ids, [&root], [("new", "root", SessionState::Active)])
        );
        root.working_dir = Some("/new/path".into());
        assert_ne!(initial, project_action_snapshot(&ids, [&root], []));
    }
}
