//! Host-side file resolution and delayed image previews. Epochs discard late replies.
use std::{sync::Arc, time::Duration};

use falcon_term::links::{LinkSpan, LinkTarget};
use nsh_core::prelude::*;

use super::TerminalSession;
use crate::{
    i18n::t,
    shell::popups::ServerUrlGuard,
    store::WorkspaceStore,
    theme::{rgb_to_hsla, use_theme},
    zoom::zpx,
};

#[derive(Clone, PartialEq)]
pub(super) enum Preview {
    Hidden,
    Loading,
    Ready(String),
    Failed,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct FileContext {
    project_id: String,
    cwd: Option<String>,
    target: falcon_client::ProbeTarget,
    use_home: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileContext {
    fn capture(store: &WorkspaceStore, session: &TerminalSession) -> Option<Self> {
        let record = store
            .sessions
            .get()
            .into_iter()
            .find(|r| r.id == session.session_id())?;
        let cwd = session
            .output_cwd()
            .or_else(|| (!record.cwd.is_empty()).then_some(record.cwd));
        Self::for_session(&record.project_id, cwd, &store.projects.get())
    }

    fn for_session(
        project_id: &str,
        cwd: Option<String>,
        projects: &[crate::store::ProjectRecord],
    ) -> Option<Self> {
        // Standalone owners are virtual groups, not entries in the project catalog.
        if falcon_proto::is_standalone_project_id(project_id) {
            let target = falcon_proto::standalone_host_id(project_id)
                .map(|id| falcon_client::ProbeTarget::SshHost(id.to_string()))
                .unwrap_or(falcon_client::ProbeTarget::Backend);
            return Some(Self {
                project_id: project_id.to_string(),
                cwd,
                target,
                use_home: true,
            });
        }
        let project = projects.iter().find(|p| p.id == project_id)?;
        let target = match project.source.as_ref() {
            Some(source) if source.project_type == falcon_proto::ProjectType::Ssh => {
                falcon_proto::standalone_host_id(&project.id)
                    .map(|id| falcon_client::ProbeTarget::SshHost(id.to_string()))
                    .unwrap_or_else(|| falcon_client::ProbeTarget::Project(project.id.clone()))
            }
            _ if project.host_id.is_some() => {
                falcon_client::ProbeTarget::Project(project.id.clone())
            }
            _ => falcon_client::ProbeTarget::Backend,
        };
        let use_home = project.source.as_ref().is_none_or(|p| p.multi.is_none());
        let cwd = cwd.or_else(|| (!project.path.is_empty()).then(|| project.path.clone()));
        Some(Self {
            project_id: project.id.clone(),
            cwd,
            target,
            use_home,
        })
    }

    async fn resolve(
        &self,
        client: &falcon_client::FalconClient,
        raw: &str,
    ) -> anyhow::Result<String> {
        use falcon_core::terminal_link::{file_path, resolve_file_path};
        let path = file_path(raw).ok_or_else(|| anyhow::anyhow!("invalid terminal file path"))?;
        let relative = !path.starts_with(['/', '\\']) && path.as_bytes().get(1) != Some(&b':');
        let home_cwd = self.use_home && self.cwd.is_none() && relative;
        let home = if home_cwd || path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
            Some(client.list_dir(None, &self.target).await?.home)
        } else {
            None
        };
        resolve_file_path(
            &path,
            self.cwd
                .as_deref()
                .or_else(|| if home_cwd { home.as_deref() } else { None }),
            home.as_deref(),
        )
        .ok_or_else(|| anyhow::anyhow!("invalid terminal file path"))
    }
}

pub(super) fn open_link(store: &WorkspaceStore, session: &Arc<TerminalSession>, link: LinkSpan) {
    match link.target {
        LinkTarget::Url(url) => {
            let _ = open_url(url);
        }
        LinkTarget::File(raw) => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (Some(client), Some(context)) =
                    (store.client(), FileContext::capture(store, session))
                else {
                    return;
                };
                let store = store.clone();
                let guard = ServerUrlGuard::capture(&store);
                spawn(async move {
                    match context.resolve(&client, &raw).await {
                        Ok(path) if guard.is_current() => {
                            store.open_file_pane(&context.project_id, &path)
                        }
                        Err(error) if guard.is_current() => store
                            .show_toast(format!("{}: {error}", t!("native.terminal.viewFailed"))),
                        _ => {}
                    }
                });
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = (raw, session);
                store.show_toast(t!("native.terminal.transferUnavailable"));
            }
        }
    }
}

pub(super) fn schedule_preview(
    store: &WorkspaceStore,
    session: &Arc<TerminalSession>,
    link: LinkSpan,
    epoch: Signal<u64>,
    preview: Signal<Preview>,
) {
    let LinkTarget::File(raw) = &link.target else {
        return;
    };
    let Some(path) = falcon_core::terminal_link::file_path(raw) else {
        return;
    };
    if super::interactions::image_mime(&path).is_none()
        && !path.to_ascii_lowercase().ends_with(".svg")
    {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (Some(client), Some(context)) = (store.client(), FileContext::capture(store, session))
        else {
            return;
        };
        let guard = ServerUrlGuard::capture(store);
        let expected = epoch.get_untracked();
        let session = session.clone();
        set_timeout(Duration::from_secs(2), move || {
            let current = move || {
                guard.is_current()
                    && epoch.get_untracked() == expected
                    && session
                        .snapshot()
                        .link_at(link.start.0, link.start.1)
                        .as_ref()
                        == Some(&link)
            };
            if !current() {
                return;
            }
            preview.set(Preview::Loading);
            spawn(async move {
                let result = async {
                    let path = context.resolve(&client, &path).await?;
                    if !current() {
                        anyhow::bail!("hover cancelled");
                    }
                    let file = client.read_file(&context.project_id, &path).await?;
                    if !current() {
                        anyhow::bail!("hover cancelled");
                    }
                    if !matches!(file.preview, falcon_proto::FilePreview::Image { .. }) {
                        anyhow::bail!("not an image");
                    }
                    let bytes = client
                        .raw_bytes(&falcon_client::raw_url(
                            &file.raw_base,
                            file.raw_path.as_deref().unwrap_or(&path),
                        ))
                        .await?;
                    use base64::Engine;
                    let mime = super::interactions::image_mime(&path).unwrap_or("image/svg+xml");
                    Ok::<_, anyhow::Error>(format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&bytes.bytes)
                    ))
                }
                .await;
                if current() {
                    preview.set(match result {
                        Ok(uri) => Preview::Ready(uri),
                        Err(_) => Preview::Failed,
                    });
                }
            });
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (store, session, epoch, preview);
    }
}

pub(super) fn preview_view(preview: Signal<Preview>) -> AnyElement {
    let theme = use_theme();
    dynamic(move || {
        let status = preview.get();
        if status == Preview::Hidden {
            return view().into_element();
        }
        let ui = theme.resolved().ui.clone();
        let body = match status {
            Preview::Ready(uri) => image(uri).w(zpx(300.0)).h(zpx(200.0)).into_element(),
            Preview::Loading => text(t!("native.terminal.viewLoading"))
                .text_sm()
                .into_element(),
            _ => text(t!("native.terminal.viewImageFailed"))
                .text_sm()
                .into_element(),
        };
        view()
            .absolute()
            .left(zpx(12.0))
            .top(zpx(12.0))
            .p(zpx(8.0))
            .rounded(zpx(8.0))
            .bg(rgb_to_hsla(ui.popover))
            .text_color(rgb_to_hsla(ui.popover_foreground))
            .pointer_events(PointerEvents::None)
            .child(body)
            .into_element()
    })
    .into_element()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn standalone_owners_resolve_without_catalog_projects() {
        let local = FileContext::for_session(
            falcon_proto::STANDALONE_LOCAL,
            Some("/tmp/current".into()),
            &[],
        )
        .unwrap();
        assert_eq!(local.project_id, falcon_proto::STANDALONE_LOCAL);
        assert_eq!(local.cwd.as_deref(), Some("/tmp/current"));
        assert!(matches!(local.target, falcon_client::ProbeTarget::Backend));
        let ssh = FileContext::for_session("standalone:host:prod", None, &[]).unwrap();
        assert_eq!(ssh.project_id, "standalone:host:prod");
        assert!(matches!(ssh.target, falcon_client::ProbeTarget::SshHost(ref id) if id == "prod"));
        assert!(ssh.cwd.is_none() && ssh.use_home);
        assert!(FileContext::for_session("deleted-project", None, &[]).is_none());
    }
}
