use falcon_proto::{
    Project, ProjectInput, ProjectType, RepoBranch, SshAuthMethod, SshConfigInput,
    MULTI_REPO_MAX,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchSelection {
    ExistingLocal(String),
    NewFromRemote { local_branch: String, remote_ref: String },
}

pub fn branch_selection(branch: &RepoBranch) -> Option<BranchSelection> {
    if branch.remote {
        let local_branch = branch.local_name.clone()?;
        if local_branch.is_empty() || local_branch == branch.name {
            return None;
        }
        Some(BranchSelection::NewFromRemote {
            local_branch,
            remote_ref: branch.name.clone(),
        })
    } else {
        Some(BranchSelection::ExistingLocal(branch.name.clone()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectFormDraft {
    pub name: String,
    pub project_type: ProjectType,
    pub working_dir: String,
    pub shell: String,
    pub host_id: Option<String>,
    pub ssh_host: String,
    pub ssh_port: String,
    pub ssh_username: String,
    pub ssh_auth_method: SshAuthMethod,
    pub ssh_key_path: String,
    pub ssh_secret: String,
    pub repos: Vec<String>,
    pub multi: bool,
    pub default_worktree_branch: String,
}

impl ProjectFormDraft {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            project_type: ProjectType::Local,
            working_dir: String::new(),
            shell: String::new(),
            host_id: None,
            ssh_host: String::new(),
            ssh_port: "22".to_string(),
            ssh_username: String::new(),
            ssh_auth_method: SshAuthMethod::Agent,
            ssh_key_path: String::new(),
            ssh_secret: String::new(),
            repos: Vec::new(),
            multi: false,
            default_worktree_branch: String::new(),
        }
    }

    pub fn from_project(project: &Project) -> Self {
        let mut draft = Self {
            name: project.name.clone(),
            project_type: project.project_type,
            working_dir: project.working_dir.clone().unwrap_or_default(),
            shell: project.shell.clone().unwrap_or_default(),
            host_id: project.host_id.clone(),
            default_worktree_branch: project.default_worktree_branch.clone().unwrap_or_default(),
            multi: project.multi.is_some(),
            repos: project
                .multi
                .as_ref()
                .map(|multi| multi.repos.iter().map(|member| member.dir.clone()).collect())
                .unwrap_or_default(),
            ..Self::new()
        };
        if let Some(ssh) = &project.ssh {
            draft.ssh_host = ssh.host.clone();
            draft.ssh_port = ssh.port.to_string();
            draft.ssh_username = ssh.username.clone();
            draft.ssh_auth_method = ssh.auth_method;
            draft.ssh_key_path = ssh.key_path.clone().unwrap_or_default();
        }
        draft
    }
}

impl Default for ProjectFormDraft {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectFormError {
    NameRequired,
    SshConfigRequired,
    InvalidSshPort,
    RepoRequired,
    TooManyRepos,
}

pub fn build_project_input(
    draft: &ProjectFormDraft,
    original: Option<&Project>,
) -> Result<ProjectInput, ProjectFormError> {
    let name = draft.name.trim();
    if name.is_empty() {
        return Err(ProjectFormError::NameRequired);
    }

    let derived = original.is_some_and(|project| project.worktree.is_some());
    let project_type = original
        .map(|project| project.project_type)
        .unwrap_or(draft.project_type);
    let working_dir = if derived {
        original.and_then(|project| project.working_dir.clone())
    } else {
        nonempty(&draft.working_dir)
    };
    let host_id = if project_type == ProjectType::Local {
        None
    } else if derived {
        original.and_then(|project| project.host_id.clone())
    } else {
        draft.host_id.clone()
    };

    let ssh = if derived && host_id.is_some() {
        None
    } else if derived {
        original.and_then(|project| {
            project.ssh.as_ref().map(|ssh| SshConfigInput {
                host: ssh.host.clone(),
                port: ssh.port,
                username: ssh.username.clone(),
                auth_method: ssh.auth_method,
                key_path: ssh.key_path.clone(),
                secret: None,
            })
        })
    } else if project_type == ProjectType::Ssh && host_id.is_none() {
        let host = draft.ssh_host.trim();
        let username = draft.ssh_username.trim();
        if host.is_empty() || username.is_empty() {
            return Err(ProjectFormError::SshConfigRequired);
        }
        let port = draft
            .ssh_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or(ProjectFormError::InvalidSshPort)?;
        Some(SshConfigInput {
            host: host.to_string(),
            port,
            username: username.to_string(),
            auth_method: draft.ssh_auth_method,
            key_path: nonempty(&draft.ssh_key_path),
            secret: optional_secret(&draft.ssh_secret),
        })
    } else {
        None
    };

    if project_type == ProjectType::Ssh && host_id.is_none() && ssh.is_none() {
        return Err(ProjectFormError::SshConfigRequired);
    }

    let is_root_multi = original.map_or(draft.multi, |project| {
        project.multi.is_some() && project.worktree.is_none()
    });
    let repos = if derived || !is_root_multi {
        None
    } else {
        let members: Vec<String> = draft
            .repos
            .iter()
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect();
        if members.is_empty() {
            return Err(ProjectFormError::RepoRequired);
        }
        if members.len() > MULTI_REPO_MAX {
            return Err(ProjectFormError::TooManyRepos);
        }
        Some(members)
    };

    Ok(ProjectInput {
        name: name.to_string(),
        project_type,
        working_dir,
        shell: nonempty(&draft.shell),
        host_id,
        ssh,
        repos,
        default_worktree_branch: if derived {
            original.and_then(|project| project.default_worktree_branch.clone())
        } else {
            nonempty(&draft.default_worktree_branch)
        },
    })
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn optional_secret(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::{MultiRepoInfo, MultiRepoMember, SshConfig, WorktreeInfo};

    #[test]
    fn derived_project_payload_preserves_immutable_fields() {
        let original = Project {
            id: "derived".into(),
            name: "old name".into(),
            project_type: ProjectType::Ssh,
            working_dir: Some("/remote/worktree".into()),
            shell: None,
            ssh: Some(SshConfig {
                host: "server".into(),
                port: 22,
                username: "user".into(),
                auth_method: SshAuthMethod::Agent,
                key_path: None,
                has_secret: false,
            }),
            host_id: Some("saved-host".into()),
            worktree: Some(WorktreeInfo {
                source_project_id: "source".into(),
                branch: "feature".into(),
                repo_dir: "/remote/repo".into(),
                created_by_falcon: true,
                archived_at: None,
            }),
            multi: Some(MultiRepoInfo {
                repos: vec![MultiRepoMember {
                    dir: "/remote/member".into(),
                    repo_dir: None,
                }],
            }),
            default_worktree_branch: Some("origin/main".into()),
            created_at: 0,
        };
        let mut draft = ProjectFormDraft::from_project(&original);
        draft.name = "renamed".into();
        draft.working_dir = "/changed".into();
        draft.host_id = Some("other-host".into());
        draft.repos.clear();

        let input = build_project_input(&draft, Some(&original)).unwrap();
        assert_eq!(input.project_type, original.project_type);
        assert_eq!(input.working_dir, original.working_dir);
        assert_eq!(input.host_id, original.host_id);
        assert_eq!(input.ssh, None);
        assert_eq!(input.repos, None);
        assert_eq!(input.default_worktree_branch, original.default_worktree_branch);
    }

    #[test]
    fn remote_branch_selection_keeps_local_and_remote_intent_distinct() {
        let remote = RepoBranch {
            name: "origin/feature".into(),
            remote: true,
            local_name: Some("feature".into()),
            checked_out_at: None,
            head: false,
            suggested_dir: String::new(),
            dir_occupied: false,
        };
        assert_eq!(
            branch_selection(&remote),
            Some(BranchSelection::NewFromRemote {
                local_branch: "feature".into(),
                remote_ref: "origin/feature".into(),
            })
        );
        let mut unmapped = remote.clone();
        unmapped.local_name = None;
        assert_eq!(branch_selection(&unmapped), None);
        unmapped.local_name = Some(unmapped.name.clone());
        assert_eq!(branch_selection(&unmapped), None);
    }

    #[test]
    fn local_branch_selection_stays_existing() {
        let local = RepoBranch {
            name: "feature".into(),
            remote: false,
            local_name: None,
            checked_out_at: None,
            head: false,
            suggested_dir: String::new(),
            dir_occupied: false,
        };
        assert_eq!(
            branch_selection(&local),
            Some(BranchSelection::ExistingLocal("feature".into()))
        );
    }

    #[test]
    fn ssh_secret_is_not_trimmed() {
        let mut draft = ProjectFormDraft::new();
        draft.name = "remote".into();
        draft.project_type = ProjectType::Ssh;
        draft.ssh_host = "server".into();
        draft.ssh_username = "user".into();
        draft.ssh_secret = " secret ".into();

        let input = build_project_input(&draft, None).unwrap();
        assert_eq!(input.ssh.unwrap().secret.as_deref(), Some(" secret "));
    }

    #[test]
    fn local_project_input_never_sends_a_host_id() {
        let mut draft = ProjectFormDraft::new();
        draft.name = "local".into();
        draft.host_id = Some("stale-host".into());

        let input = build_project_input(&draft, None).unwrap();
        assert_eq!(input.project_type, ProjectType::Local);
        assert_eq!(input.host_id, None);
    }
}
