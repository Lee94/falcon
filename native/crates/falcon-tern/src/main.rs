mod config;
mod snapshot;
mod terminal;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use falcon_client::{FalconClient, ProbeTarget};
use falcon_proto::{CreateSessionRequest, ProjectType, SessionAgent, STANDALONE_LOCAL, standalone_host_id};
use serde::Serialize;

const USAGE: &str = "Usage: falcon-tern --config PATH snapshot\n       falcon-tern --config PATH attach SESSION_ID\n       falcon-tern --config PATH agents PROJECT_ID\n       falcon-tern --config PATH create PROJECT_ID [--agent omp]\n\nPROJECT_ID may also be a standalone owner ID.\nFALCON_TERN_CONFIG supplies PATH when --config is omitted.\nDetach with Ctrl+\\; closing this viewer leaves the Falcon session running.";

enum Command {
    Snapshot,
    Attach(String),
    Agents(String),
    Create { project_id: String, agent: Option<SessionAgent> },
}

struct Args {
    config: PathBuf,
    command: Command,
}

fn arguments() -> Result<Option<Args>> {
    let mut values = std::env::args_os().skip(1);
    let mut config = std::env::var_os("FALCON_TERN_CONFIG").map(PathBuf::from);
    let mut command = None;
    while let Some(value) = values.next() {
        match value.to_str() {
            Some("--help" | "-h") => {
                println!("{USAGE}");
                return Ok(None);
            }
            Some("--config") => {
                config = Some(PathBuf::from(values.next().context("--config requires a path")?));
            }
            Some("snapshot") if command.is_none() => command = Some(Command::Snapshot),
            Some("attach" | "agents" | "create") if command.is_none() => {
                let name = value.to_str().expect("matched UTF-8 command");
                let id = values.next().with_context(|| format!("{name} requires an ID"))?;
                let id = id.into_string().map_err(|_: OsString| anyhow::anyhow!("ID must be UTF-8"))?;
                if id.is_empty() { bail!("{name} requires a nonempty ID"); }
                command = Some(match name {
                    "attach" => Command::Attach(id),
                    "agents" => Command::Agents(id),
                    _ => Command::Create { project_id: id, agent: None },
                });
            }
            Some("--agent") => {
                let Some(Command::Create { agent, .. }) = command.as_mut() else {
                    bail!("--agent is only accepted after create PROJECT_ID");
                };
                if agent.is_some() || values.next().as_deref() != Some(std::ffi::OsStr::new("omp")) {
                    bail!("create accepts --agent omp exactly once");
                }
                *agent = Some(SessionAgent::Omp);
            }
            _ => bail!("unexpected argument; {USAGE}"),
        }
    }
    Ok(Some(Args {
        config: config.context("provide --config PATH or set FALCON_TERN_CONFIG")?,
        command: command.context(USAGE)?,
    }))
}

async fn agents(client: &FalconClient, owner: &str) -> Result<Vec<SessionAgent>> {
    let target = if owner == STANDALONE_LOCAL {
        ProbeTarget::Backend
    } else if let Some(host) = standalone_host_id(owner) {
        ProbeTarget::SshHost(host.to_owned())
    } else {
        let project = config::request(client.list_projects()).await?.into_iter()
            .find(|project| project.id == owner).context("project does not exist")?;
        if project.project_type == ProjectType::Local { ProbeTarget::Backend }
        else { ProbeTarget::Project(project.id) }
    };
    config::request(client.agents(&target)).await
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = async {
        let Some(args) = arguments()? else { return Ok(()) };
        let connection = config::load_connection(&args.config).await?;
        match args.command {
            Command::Snapshot => snapshot::print(&connection.client).await,
            Command::Attach(id) => terminal::attach(&connection.client, &id, connection.sessiond_root.as_deref()).await,
            Command::Agents(owner) => print_json(&agents(&connection.client, &owner).await?),
            Command::Create { project_id, agent } => {
                if agent == Some(SessionAgent::Omp) && !agents(&connection.client, &project_id).await?.contains(&SessionAgent::Omp) {
                    bail!("omp is not installed on the target host");
                }
                let session = config::request(connection.client.create_session(&project_id, &CreateSessionRequest {
                    agent, ..Default::default()
                })).await?;
                print_json(&session)
            }
        }
    }.await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("falcon-tern: {error:#}");
            ExitCode::FAILURE
        }
    }
}
