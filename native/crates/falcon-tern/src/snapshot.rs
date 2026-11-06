use std::io::{self, Write};

use anyhow::Result;
use falcon_client::FalconClient;
use falcon_core::project_tree::{ServerGroup, group_servers};
use falcon_proto::{Project, SessionWithProject, SshHost};
use serde::Serialize;

use crate::config::request;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Group<'a> {
    key: &'a str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    conn: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    standalone_id: Option<&'a str>,
    folders: Vec<Folder<'a>>,
}

#[derive(Serialize)]
struct Folder<'a> {
    project: &'a Project,
    worktrees: &'a [Project],
}

impl<'a> From<&'a ServerGroup> for Group<'a> {
    fn from(group: &'a ServerGroup) -> Self {
        Self {
            key: &group.key,
            name: &group.name,
            conn: group.conn.as_deref(),
            standalone_id: group.standalone_id.as_deref(),
            folders: group.folders.iter().map(|folder| Folder {
                project: &folder.project,
                worktrees: &folder.worktrees,
            }).collect(),
        }
    }
}

#[derive(Serialize)]
struct Snapshot<'a> {
    groups: Vec<Group<'a>>,
    projects: &'a [Project],
    sessions: &'a [SessionWithProject],
    hosts: &'a [SshHost],
}

pub async fn print(client: &FalconClient) -> Result<()> {
    let (projects, sessions, hosts) = tokio::try_join!(
        request(client.list_projects()),
        request(client.list_sessions()),
        request(client.list_hosts()),
    )?;
    let groups = group_servers(&projects, &hosts, "本机", true);
    // Only response models are serialized; their SSH fields expose hasSecret, never a secret.
    let snapshot = Snapshot {
        groups: groups.iter().map(Group::from).collect(),
        projects: &projects,
        sessions: &sessions,
        hosts: &hosts,
    };
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &snapshot)?;
    stdout.write_all(b"\n")?;
    Ok(())
}
