use falcon_core::project_form::{
    BranchSelection, ProjectFormDraft, ProjectFormError, branch_selection, build_project_input,
};
use falcon_core::worktree_path::{preview_dir, preview_multi_dir};
use falcon_proto::{
    MultiRepoProbe, MultiWorktreeInput, MultiWorktreeMode, ProjectType, RepoBranch,
    RepoInfo, SshAuthMethod, WorktreeInput, WorktreeMode,
};
use nsh_core::element::input;
use nsh_core::element::reactive::Owner;
use nsh_core::prelude::*;
use std::rc::Rc;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::project_operations::{FormOperationContext, ProjectOperations, worktree_reason};
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone)]
struct FormUiGuard {
    operations: Rc<ProjectOperations>,
    context: FormOperationContext,
}

impl FormUiGuard {
    fn is_current(&self) -> bool {
        self.operations.is_current(self.context)
    }

    fn signal<T: PartialEq + 'static>(&self, value: T) -> Option<Signal<T>> {
        self.is_current()
            .then(|| self.context.owner.run(|| signal(value)))
    }
}

fn provide_form_guard(operations: Rc<ProjectOperations>, context: FormOperationContext) -> FormUiGuard {
    let guard = FormUiGuard { operations, context };
    provide_context(guard.clone());
    guard
}

fn use_form_guard() -> FormUiGuard {
    use_context::<FormUiGuard>().expect("FormUiGuard must be provided by a project form")
}

#[derive(Clone, PartialEq, Props)]
pub struct ProjectFormProps {
    pub project_id: Option<String>,
}

#[component]
pub fn ProjectForm(props: ProjectFormProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let resolved = theme.resolved();
    let ui = &resolved.ui;
    let project_id = props.project_id;
    let original = project_id.as_ref().and_then(|id| {
        store
            .projects
            .get()
            .into_iter()
            .find(|project| &project.id == id)
            .and_then(|project| project.source)
    });
    let missing_project = project_id.is_some() && original.is_none();
    let derived = original.as_ref().is_some_and(|project| project.worktree.is_some());
    let root_multi = original.as_ref().is_some_and(|project| {
        project.multi.is_some() && project.worktree.is_none()
    });
    let initial = original
        .as_ref()
        .map(ProjectFormDraft::from_project)
        .unwrap_or_default();
    let context = store.project_operations.begin_form();
    let form_guard = provide_form_guard(store.project_operations.clone(), context);

    let name = signal(initial.name);
    let project_type = signal(initial.project_type);
    let working_dir = signal(initial.working_dir);
    let shell = signal(initial.shell);
    let host_id = signal(initial.host_id);
    let ssh_host = signal(initial.ssh_host);
    let ssh_port = signal(initial.ssh_port);
    let ssh_username = signal(initial.ssh_username);
    let ssh_auth_method = signal(initial.ssh_auth_method);
    let ssh_key_path = signal(initial.ssh_key_path);
    let ssh_secret = signal(initial.ssh_secret);
    let multi = signal(initial.multi);
    let repos = signal(
        initial
            .repos
            .into_iter()
            .map(signal)
            .collect::<Vec<Signal<String>>>(),
    );
    let default_worktree_branch = signal(initial.default_worktree_branch);
    let busy = signal(false);
    let error = signal(None::<String>);

    let title = if project_id.is_some() {
        t!("project.editTitle")
    } else {
        t!("project.createTitle")
    };
    let header = form_header(&store, &resolved, title, context);

    let type_store = store.clone();
    let is_editing = project_id.is_some();
    let type_theme = resolved.clone();
    let type_selector = dynamic(move || {
        if is_editing {
            return view().into_element();
        }
        let resolved = type_theme.clone();
        let local = project_type.get() == ProjectType::Local;
        let local_value = project_type;
        let ssh_value = project_type;
        view()
            .flex_row()
            .gap(zpx(8.0))
            .child(choice_button(
                t!("project.typeLocal"),
                local,
                false,
                &resolved,
                move || {
                    local_value.set(ProjectType::Local);
                    host_id.set(None);
                },
            ))
            .child(choice_button(
                t!("project.typeSsh"),
                !local,
                false,
                &resolved,
                move || {
                    ssh_value.set(ProjectType::Ssh);
                },
            ))
            .into_element()
    });

    let ssh_type = project_type;
    let ssh_selected_host = host_id;
    let ssh_hosts_store = type_store.clone();
    let ssh_original = original.clone();
    let ssh_theme = resolved.clone();
    let ssh_section = dynamic(move || {
        let resolved = ssh_theme.clone();
        if ssh_type.get() != ProjectType::Ssh {
            return view().into_element();
        }
        if derived {
            let current = ssh_original
                .as_ref()
                .and_then(|project| project.ssh.as_ref())
                .map(|ssh| format!("{}@{}:{}", ssh.username, ssh.host, ssh.port))
                .unwrap_or_default();
            return readonly_field(t!("project.host"), current, &resolved).into_element();
        }

        let hosts = ssh_hosts_store.hosts.get();
        let selected = ssh_selected_host.get();
        let mut choices = view().flex_col().gap(zpx(6.0));
        let host_signal = ssh_selected_host;
        choices = choices.child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(zpx(6.0))
                .children(hosts.into_iter().map(|host| {
                    let id = host.id.clone();
                    choice_button(
                        host.name,
                        selected.as_deref() == Some(id.as_str()),
                        false,
                        &resolved,
                        move || host_signal.set(Some(id.clone())),
                    )
                }))
                .child(choice_button(
                    t!("project.inlineSsh"),
                    selected.is_none(),
                    false,
                    &resolved,
                    move || ssh_selected_host.set(None),
                )),
        );

        if selected.is_none() {
            let auth_key = ssh_auth_method;
            choices = choices.child(
                view()
                    .flex_row()
                    .gap(zpx(6.0))
                    .child(choice_button(
                        t!("project.authAgent"),
                        auth_key.get() == SshAuthMethod::Agent,
                        false,
                        &resolved,
                        move || auth_key.set(SshAuthMethod::Agent),
                    ))
                    .child(choice_button(
                        t!("project.authKey"),
                        auth_key.get() == SshAuthMethod::Key,
                        false,
                        &resolved,
                        move || auth_key.set(SshAuthMethod::Key),
                    ))
                    .child(choice_button(
                        t!("project.authPassword"),
                        auth_key.get() == SshAuthMethod::Password,
                        false,
                        &resolved,
                        move || auth_key.set(SshAuthMethod::Password),
                    )),
            );
            choices = choices
                .child(text_field(
                    t!("project.host"),
                    ssh_host,
                    t!("project.host"),
                    &resolved,
                    false,
                    false,
                ))
                .child(text_field(
                    t!("project.port"),
                    ssh_port,
                    "22".to_string(),
                    &resolved,
                    false,
                    false,
                ))
                .child(text_field(
                    t!("project.username"),
                    ssh_username,
                    t!("project.username"),
                    &resolved,
                    false,
                    false,
                ));
            if auth_key.get() == SshAuthMethod::Key {
                choices = choices.child(text_field(
                    t!("project.keyPath"),
                    ssh_key_path,
                    t!("project.keyPath"),
                    &resolved,
                    true,
                    false,
                ));
            }
            if auth_key.get() != SshAuthMethod::Agent {
                choices = choices
                    .child(text_field(
                        t!("project.sshSecret"),
                        ssh_secret,
                        t!("project.secretKept"),
                        &resolved,
                        false,
                        true,
                    ))
                    .child(
                        text(t!("project.secretKept"))
                            .text_xs()
                            .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                    );
            }
        }
        choices.into_element()
    });

    let multi_toggle_signal = multi;
    let toggle_theme = resolved.clone();
    let multi_toggle = dynamic(move || {
        if root_multi || derived || is_editing {
            return view().into_element();
        }
        let enabled = multi_toggle_signal.get();
        view()
            .child(choice_button(
                t!("multi.typeMulti"),
                enabled,
                false,
                &toggle_theme,
                move || multi_toggle_signal.set(!multi_toggle_signal.get()),
            ))
            .into_element()
    });

    let repo_rows = repos;
    let show_repos = root_multi || derived || project_id.is_none();
    let repos_original = original.clone();
    let repos_theme = resolved.clone();
    let repos_guard = form_guard.clone();
    let repos_section = dynamic(move || {
        let resolved = repos_theme.clone();
        let visible = if derived {
            repos_original.as_ref().is_some_and(|project| project.multi.is_some())
        } else if root_multi {
            true
        } else {
            multi.get()
        };
        if !show_repos || !visible {
            return view().into_element();
        }
        if derived {
            let members = repos_original
                .as_ref()
                .and_then(|project| project.multi.as_ref())
                .map(|multi| multi.repos.clone())
                .unwrap_or_default();
            let mut rows = view().flex_col().gap(zpx(4.0));
            for member in members {
                rows = rows.child(readonly_field(
                    t!("multi.repos"),
                    member.dir,
                    &resolved,
                ));
            }
            return rows.into_element();
        }

        let rows = repo_rows.get();
        let mut list = view().flex_col().gap(zpx(6.0));
        for (index, value) in rows.into_iter().enumerate() {
            let remove = repo_rows;
            let down = repo_rows;
            let len = repo_rows.get().len();
            list = list.child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(4.0))
                    .child(text_field(
                        t!("multi.repos"),
                        value,
                        t!("project.workingDir"),
                        &resolved,
                        true,
                        false,
                    ))
                    .child(choice_button(
                        t!("multi.moveUp"),
                        false,
                        index == 0,
                        &resolved,
                        move || {
                            if index > 0 {
                                remove.update(|items| items.swap(index, index - 1));
                            }
                        },
                    ))
                    .child(choice_button(
                        t!("multi.moveDown"),
                        false,
                        index + 1 >= len,
                        &resolved,
                        move || {
                            if index + 1 < len {
                                down.update(|items| items.swap(index, index + 1));
                            }
                        },
                    ))
                    .child(choice_button(
                        t!("multi.removeRepo"),
                        false,
                        false,
                        &resolved,
                        move || {
                            remove.update(|items| {
                                if index < items.len() {
                                    items.remove(index);
                                }
                            });
                        },
                    )),
            );
        }
        let add = repo_rows;
        let add_guard = repos_guard.clone();
        list.child(choice_button(
            t!("multi.addRepo"),
            false,
            repo_rows.get().len() >= falcon_proto::MULTI_REPO_MAX,
            &resolved,
            move || {
                if let Some(repo_path) = add_guard.signal(String::new()) {
                    add.update(|items| {
                        if items.len() < falcon_proto::MULTI_REPO_MAX {
                            items.push(repo_path);
                        }
                    });
                }
            },
        ))
        .into_element()
    });

    let save_store = store.clone();
    let save_ops = store.project_operations.clone();
    let save_busy = busy;
    let save_error = error;
    let save_original = original.clone();
    let save_project_id = project_id.clone();
    let save = move || {
        if !save_ops.is_current(context) {
            return;
        }
        if save_project_id.is_some() && save_original.is_none() {
            save_error.set(Some(t!("project.missingProject")));
            return;
        }
        let draft = ProjectFormDraft {
            name: name.get(),
            project_type: project_type.get(),
            working_dir: working_dir.get(),
            shell: shell.get(),
            host_id: host_id.get(),
            ssh_host: ssh_host.get(),
            ssh_port: ssh_port.get(),
            ssh_username: ssh_username.get(),
            ssh_auth_method: ssh_auth_method.get(),
            ssh_key_path: ssh_key_path.get(),
            ssh_secret: ssh_secret.get(),
            repos: repos
                .get()
                .iter()
                .map(|value| value.get())
                .collect(),
            multi: multi.get(),
            default_worktree_branch: default_worktree_branch.get(),
        };
        match build_project_input(&draft, save_original.as_ref()) {
            Ok(input) => save_ops.save_project(
                save_store.clone(),
                context,
                save_project_id.clone(),
                input,
                save_busy,
                save_error,
            ),
            Err(error) => save_error.set(Some(project_form_error(error))),
        }
    };
    let save_button_owner = Owner::current();
    let save_button_operations = store.project_operations.clone();
    let save_action = save;

    let save_label_busy = busy;
    let save_label_theme = resolved.clone();
    let save_label = dynamic(move || {
        text(if save_label_busy.get() {
            t!("project.creating")
        } else if is_editing {
            t!("project.save")
        } else {
            t!("project.create")
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb_to_hsla(save_label_theme.ui.primary_foreground))
        .into_element()
    });

    let working_dir_field = if derived {
        readonly_field(t!("project.workingDir"), working_dir.get(), &resolved).into_element()
    } else {
        text_field(
            t!("project.workingDir"),
            working_dir,
            t!("project.workingDir"),
            &resolved,
            true,
            false,
        )
        .into_element()
    };

    let default_branch_field = if derived {
        view().into_element()
    } else {
        text_field(
            t!("project.defaultWorktreeBranch"),
            default_worktree_branch,
            t!("project.defaultWorktreeBranchPlaceholder"),
            &resolved,
            true,
            false,
        )
        .into_element()
    };

    let name_field = text_field(
        t!("project.name"),
        name,
        t!("project.name"),
        &resolved,
        false,
        false,
    );
    let shell_field = text_field(
        t!("project.shell"),
        shell,
        t!("project.shellAuto"),
        &resolved,
        true,
        false,
    );
    let error_theme = resolved.clone();
    let error_view = dynamic(move || {
        error
            .get()
            .map(|message| {
                text(message)
                    .text_xs()
                    .text_color(rgb_to_hsla(error_theme.ui.destructive))
                    .into_element()
            })
            .unwrap_or_else(|| view().into_element())
    });

    view()
        .flex_col()
        .child(header)
        .child(
            view()
                .flex_col()
                .overflow_y_auto()
                .p(zpx(14.0))
                .gap(zpx(10.0))
                .child(type_selector)
                .child(name_field)
                .child(working_dir_field)
                .child(ssh_section)
                .child(shell_field)
                .child(default_branch_field)
                .child(multi_toggle)
                .child(repos_section)
                .when(missing_project, |view| {
                    view.child(text(t!("project.missingProject")).text_xs())
                })
                .child(error_view)
                .child(
                    view()
                        .items_center()
                        .py(zpx(7.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint_strong))
                        .on_press(move || {
                            if save_button_owner.is_live()
                                && save_button_operations.is_current(context)
                            {
                                save_action();
                            }
                        })
                        .child(save_label),
                ),
        )
}

fn project_form_error(error: ProjectFormError) -> String {
    match error {
        ProjectFormError::NameRequired => t!("project.nameRequired"),
        ProjectFormError::SshConfigRequired => t!("project.sshRequired"),
        ProjectFormError::InvalidSshPort => t!("project.invalidSshPort"),
        ProjectFormError::RepoRequired => t!("multi.repoRequired"),
        ProjectFormError::TooManyRepos => t!("multi.repoLimit"),
    }
}

fn form_header(
    store: &crate::store::WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    title: String,
    context: FormOperationContext,
) -> impl IntoElement {
    let close_store = store.clone();
    let operations = store.project_operations.clone();
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .px(zpx(14.0))
        .py(zpx(10.0))
        .border_b_1()
        .border_color(rgba_to_hsla(resolved.ui.border))
        .child(
            text(title)
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(resolved.ui.popover_foreground)),
        )
        .child(choice_button(
            t!("common.close"),
            false,
            false,
            resolved,
            move || operations.close_form(&close_store, context),
        ))
}

fn text_field(
    label: String,
    value: Signal<String>,
    placeholder: String,
    resolved: &falcon_theme::ResolvedTheme,
    mono: bool,
    masked: bool,
) -> impl IntoElement {
    let owner = Owner::current();
    let guard = use_form_guard();
    let ui = &resolved.ui;
    let foreground = rgb_to_hsla(ui.foreground);
    let muted = rgb_to_hsla(ui.muted_foreground);
    let background = rgba_to_hsla(ui.input);
    view()
        .flex_col()
        .gap(zpx(4.0))
        .child(text(label).text_xs().text_color(muted))
        .child(
            input()
                .w_full()
                .px(zpx(8.0))
                .py(zpx(6.0))
                .rounded(zpx(6.0))
                .bg(background)
                .text_xs()
                .text_color(foreground)
                .when(mono, |view| view.font_family(FONT_FAMILY_MONO))
                .when(masked, |view| view.masked(true))
                .placeholder(placeholder)
                .placeholder_color(muted)
                .value(move || value.get())
                .on_change(move |next: &str| {
                    if owner.is_live() && guard.is_current() {
                        value.set(next.to_string());
                    }
                }),
        )
}

fn readonly_field(
    label: String,
    value: String,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    view()
        .flex_col()
        .gap(zpx(4.0))
        .child(text(label).text_xs().text_color(rgb_to_hsla(resolved.ui.muted_foreground)))
        .child(
            text(value)
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.foreground)),
        )
}

fn choice_button(
    label: String,
    selected: bool,
    disabled: bool,
    resolved: &falcon_theme::ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let owner = Owner::current();
    let guard = use_form_guard();
    view()
        .px(zpx(8.0))
        .py(zpx(5.0))
        .rounded(zpx(6.0))
        .when(!disabled, |view| view.cursor_pointer())
        .when(selected, |view| view.bg(rgb_to_hsla(resolved.ui.tint)))
        .when(disabled, |view| view.opacity(0.5))
        .on_press(move || {
            if owner.is_live() && guard.is_current() && !disabled {
                action();
            }
        })
        .child(
            text(label)
                .text_xs()
                .text_color(rgb_to_hsla(if selected {
                    resolved.ui.tint_foreground
                } else {
                    resolved.ui.foreground
                })),
        )
}

#[derive(Clone, PartialEq, Props)]
pub struct WorktreeFormProps {
    pub project_id: String,
}

#[component]
pub fn WorktreeForm(props: WorktreeFormProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let resolved = theme.resolved();
    let project_id = props.project_id;
    let project = store
        .projects
        .get()
        .into_iter()
        .find(|project| project.id == project_id)
        .and_then(|project| project.source);
    let is_multi = project.as_ref().is_some_and(|project| project.multi.is_some());
    let project_name = project
        .as_ref()
        .map(|project| project.name.clone())
        .unwrap_or_default();
    let context = store.project_operations.begin_form();
    let _form_guard = provide_form_guard(store.project_operations.clone(), context);

    let loading = signal(false);
    let probe_error = signal(None::<String>);
    let repo = signal(None::<RepoInfo>);
    let multi_repo = signal(None::<MultiRepoProbe>);
    let name = signal(String::new());
    let branch = signal(String::new());
    let start_point = signal(
        project
            .as_ref()
            .and_then(|project| project.default_worktree_branch.clone())
            .unwrap_or_default(),
    );
    let mode = signal(WorktreeMode::NewBranch);
    let multi_mode = signal(MultiWorktreeMode::Auto);
    let directory = signal(String::new());
    let manual_directory = signal(false);
    let busy = signal(false);
    let error = signal(None::<String>);

    if project.is_some() {
        store.project_operations.load_repo_probe(
            store.clone(),
            context,
            project_id.clone(),
            is_multi,
            loading,
            probe_error,
            repo,
            multi_repo,
        );
    }
    if project.is_none() {
        probe_error.set(Some(t!("project.missingProject")));
    }

    let title = t!("worktree.title", name = project_name.as_str());
    let header = form_header(&store, &resolved, title, context);

    let retry_store = store.clone();
    let retry_operations = store.project_operations.clone();
    let retry_id = project_id.clone();
    let retry = move || {
        if retry_operations.is_current(context) {
            retry_operations.load_repo_probe(
                retry_store.clone(),
                context,
                retry_id.clone(),
                is_multi,
                loading,
                probe_error,
                repo,
                multi_repo,
            );
        }
    };

    let retry_probe = retry.clone();
    let probe_theme = resolved.clone();
    let probe_view = dynamic(move || {
        let resolved = probe_theme.clone();
        if loading.get() {
            return text(t!("worktree.probing"))
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.muted_foreground))
                .into_element();
        }
        if let Some(message) = probe_error.get() {
            return view()
                .flex_col()
                .gap(zpx(6.0))
                .child(text(message).text_xs().text_color(rgb_to_hsla(resolved.ui.destructive)))
                .child(choice_button(
                    t!("worktree.retry"),
                    false,
                    false,
                    &resolved,
                    retry_probe.clone(),
                ))
                .into_element();
        }
        if is_multi {
            let Some(probe) = multi_repo.get() else {
                return text(t!("worktree.probing"))
                    .text_xs()
                    .text_color(rgb_to_hsla(resolved.ui.muted_foreground))
                    .into_element();
            };
            if probe.members.is_empty() {
                return view()
                    .flex_col()
                    .gap(zpx(6.0))
                    .child(text(t!("worktree.notDerivable")).text_xs().text_color(rgb_to_hsla(resolved.ui.destructive)))
                    .child(choice_button(
                        t!("worktree.retry"),
                        false,
                        false,
                        &resolved,
                        retry_probe.clone(),
                    ))
                    .into_element();
            }
            let selected_branch = branch.get();
            let selected_mode = multi_mode.get();
            let mut rows = view().flex_col().gap(zpx(5.0));
            for member in &probe.members {
                let status = multi_member_status(&member.info, &selected_branch, selected_mode);
                let color = rgb_to_hsla(if status.is_err() {
                    resolved.ui.destructive
                } else {
                    resolved.ui.muted_foreground
                });
                let label = match status {
                    Ok(label) => label,
                    Err(reason) => reason,
                };
                rows = rows.child(
                    view()
                        .flex_col()
                        .gap(zpx(2.0))
                        .child(text(member.dir.clone()).text_xs().text_color(rgb_to_hsla(resolved.ui.foreground)))
                        .child(text(label).text_xs().text_color(color)),
                );
            }
            if probe.members.iter().any(|member| !member.info.derivable) {
                let retry = retry_probe.clone();
                rows = rows.child(choice_button(
                    t!("worktree.retry"),
                    false,
                    false,
                    &resolved,
                    move || retry(),
                ));
            }
            return rows.into_element();
        }

        let Some(info) = repo.get() else {
            return text(t!("worktree.probing"))
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.muted_foreground))
                .into_element();
        };
        if !info.derivable {
            let reason = info
                .reason
                .as_ref()
                .map(worktree_reason)
                .unwrap_or_else(|| t!("worktree.notDerivable"));
            let detail = info.detail.unwrap_or_default();
            return view()
                .flex_col()
                .gap(zpx(4.0))
                .child(text(reason).text_xs().text_color(rgb_to_hsla(resolved.ui.destructive)))
                .child(text(detail).text_xs().text_color(rgb_to_hsla(resolved.ui.muted_foreground)))
                .child(choice_button(
                    t!("worktree.retry"),
                    false,
                    false,
                    &resolved,
                    retry_probe.clone(),
                ))
                .into_element();
        }
        let selected = branch.get();
        let mut list = view().flex_col().gap(zpx(4.0));
        for branch_info in info.branches {
            let branch_name = if branch_info.remote {
                format!("{} · {}", branch_info.name, t!("worktree.groupRemote"))
            } else {
                format!("{} · {}", branch_info.name, t!("worktree.groupLocal"))
            };
            let checked_out = branch_info.checked_out_at.is_some();
            let selection = branch_selection(&branch_info);
            let unavailable = checked_out || selection.is_none();
            let chosen = match &selection {
                Some(BranchSelection::ExistingLocal(local)) => {
                    selected == *local && mode.get() == WorktreeMode::ExistingBranch
                }
                Some(BranchSelection::NewFromRemote { local_branch, remote_ref }) => {
                    selected == *local_branch
                        && start_point.get() == *remote_ref
                        && mode.get() == WorktreeMode::NewBranch
                }
                None => false,
            };
            let branch_signal = branch;
            let base_signal = start_point;
            let mode_signal = mode;
            let branch_value = branch_info.clone();
            list = list.child(choice_button(
                if checked_out {
                    format!("{} · {}", branch_name, t!("worktree.inUse", path = branch_info.checked_out_at.as_deref().unwrap_or_default()))
                } else {
                    branch_name
                },
                chosen,
                unavailable,
                &resolved,
                move || {
                    match branch_selection(&branch_value) {
                        Some(BranchSelection::NewFromRemote { local_branch, remote_ref }) => {
                            branch_signal.set(local_branch);
                            base_signal.set(remote_ref);
                            mode_signal.set(WorktreeMode::NewBranch);
                        }
                        Some(BranchSelection::ExistingLocal(local_branch)) => {
                            branch_signal.set(local_branch);
                            mode_signal.set(WorktreeMode::ExistingBranch);
                        }
                        None => {}
                    }
                },
            ));
        }
        list.into_element()
    });

    let mode_signal = mode;
    let mode_theme = resolved.clone();
    let mode_selector = if is_multi {
        let mode_signal = multi_mode;
        dynamic(move || {
            let selected = mode_signal.get();
            view()
                .flex_row()
                .gap(zpx(6.0))
                .child(choice_button(
                    t!("multi.modeAuto"),
                    selected == MultiWorktreeMode::Auto,
                    false,
                    &mode_theme,
                    move || mode_signal.set(MultiWorktreeMode::Auto),
                ))
                .child(choice_button(
                    t!("worktree.modeNew"),
                    selected == MultiWorktreeMode::NewBranch,
                    false,
                    &mode_theme,
                    move || mode_signal.set(MultiWorktreeMode::NewBranch),
                ))
                .child(choice_button(
                    t!("worktree.modeExisting"),
                    selected == MultiWorktreeMode::ExistingBranch,
                    false,
                    &mode_theme,
                    move || mode_signal.set(MultiWorktreeMode::ExistingBranch),
                ))
                .into_element()
        })
        .into_element()
    } else {
        dynamic(move || {
            let selected = mode_signal.get();
            view()
                .flex_row()
                .gap(zpx(6.0))
                .child(choice_button(
                    t!("worktree.modeNew"),
                    selected == WorktreeMode::NewBranch,
                    false,
                    &mode_theme,
                    move || mode_signal.set(WorktreeMode::NewBranch),
                ))
                .child(choice_button(
                    t!("worktree.modeExisting"),
                    selected == WorktreeMode::ExistingBranch,
                    false,
                    &mode_theme,
                    move || mode_signal.set(WorktreeMode::ExistingBranch),
                ))
                .into_element()
        })
        .into_element()
    };

    let branch_field = text_field(
        t!("worktree.branch"),
        branch,
        t!("worktree.branchPlaceholder"),
        &resolved,
        true,
        false,
    );
    let name_field = text_field(
        t!("worktree.name"),
        name,
        t!("worktree.namePlaceholder"),
        &resolved,
        false,
        false,
    );

    let start_point_field = if is_multi {
        view().into_element()
    } else {
        let start_signal = start_point;
        let start_mode = mode;
        let repo_info = repo;
        let start_theme = resolved.clone();
        dynamic(move || {
            let resolved = start_theme.clone();
            if start_mode.get() != WorktreeMode::NewBranch {
                return view().into_element();
            }
            let mut section = view()
                .flex_col()
                .gap(zpx(5.0))
                .child(text_field(
                    t!("worktree.startPoint"),
                    start_signal,
                    t!("worktree.startPointHead", ref = "HEAD"),
                    &resolved,
                    true,
                    false,
                ));
            if let Some(info) = repo_info.get() {
                let fallback = info
                    .head_branch
                    .clone()
                    .or(info.head_sha.clone())
                    .unwrap_or_else(|| "HEAD".to_string());
                let label = if let Some(branch) = info.head_branch {
                    t!("worktree.startPointHead", ref = branch.as_str())
                } else {
                    t!("worktree.startPointDetached", sha = fallback.as_str())
                };
                let selected_start = start_signal;
                section = section.child(choice_button(
                    label,
                    false,
                    false,
                    &resolved,
                    move || selected_start.set(fallback.clone()),
                ));
            }
            section.into_element()
        })
        .into_element()
    };

    let path_theme = resolved.clone();
    let path_warning = dynamic(move || {
        let resolved = path_theme.clone();
        let branch_value = branch.get();
        if is_multi {
            return view().into_element();
        }
        let Some(info) = repo.get() else {
            return view().into_element();
        };
        let suggested = single_suggested_dir(&info, &branch_value);
        let custom = manual_directory.get();
        let chosen = if custom { directory.get() } else { suggested.clone() };
        let occupied = info.branches.iter().any(|branch| {
            branch.dir_occupied && branch.suggested_dir == suggested && chosen == suggested
        });
        if occupied {
            text(t!("worktree.dirOccupied"))
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.destructive))
                .into_element()
        } else {
            view().into_element()
        }
    });

    let multi_default_hint = if is_multi {
        let configured = project
            .as_ref()
            .and_then(|project| project.default_worktree_branch.as_deref())
            .unwrap_or("HEAD");
        text(t!("multi.defaultBase", branch = configured))
            .text_xs()
            .text_color(rgb_to_hsla(resolved.ui.muted_foreground))
            .into_element()
    } else {
        view().into_element()
    };

    let multi_branch_theme = resolved.clone();
    let multi_branch_options = dynamic(move || {
        let resolved = multi_branch_theme.clone();
        if !is_multi {
            return view().into_element();
        }
        let Some(probe) = multi_repo.get() else {
            return view().into_element();
        };
        let current = branch.get();
        let branch_names = match multi_mode.get() {
            MultiWorktreeMode::Auto => union_branch_names(&probe),
            MultiWorktreeMode::NewBranch => Vec::new(),
            MultiWorktreeMode::ExistingBranch => common_branch_names(&probe),
        };
        let mut choices = view().flex_row().flex_wrap().gap(zpx(5.0));
        for name in branch_names {
            let signal = branch;
            let chosen = current == name;
            let value = name.clone();
            choices = choices.child(choice_button(
                name,
                chosen,
                false,
                &resolved,
                move || signal.set(value.clone()),
            ));
        }
        choices.into_element()
    });

    let dir_value = directory;
    let dir_manual_value = manual_directory;
    let dir_owner = Owner::current();
    let dir_operations = store.project_operations.clone();
    let directory_project_name = project_name.clone();
    let single_dir_repo = repo;
    let multi_dir_probe = multi_repo;
    let directory_input = input()
        .w_full()
        .px(zpx(8.0))
        .py(zpx(6.0))
        .rounded(zpx(6.0))
        .bg(rgba_to_hsla(resolved.ui.input))
        .font_family(FONT_FAMILY_MONO)
        .text_xs()
        .text_color(rgb_to_hsla(resolved.ui.foreground))
        .placeholder(t!("worktree.dirHint"))
        .placeholder_color(rgb_to_hsla(resolved.ui.muted_foreground))
        .value(move || {
            if dir_manual_value.get() {
                return dir_value.get();
            }
            let branch = branch.get();
            if is_multi {
                multi_dir_probe
                    .get()
                    .and_then(|probe| probe.base_dir.map(|base| preview_multi_dir(&base, &directory_project_name, &branch)))
                    .unwrap_or_default()
            } else {
                single_dir_repo
                    .get()
                    .map(|probe| single_suggested_dir(&probe, &branch))
                    .unwrap_or_default()
            }
        })
        .on_change(move |value: &str| {
            if dir_owner.is_live() && dir_operations.is_current(context) {
                dir_value.set(value.to_string());
                dir_manual_value.set(true);
            }
        });

    let multi_status_store = multi_repo;
    let multi_status_branch = branch;
    let multi_status_mode = multi_mode;
    let status_theme = resolved.clone();
    let multi_status = dynamic(move || {
        let resolved = status_theme.clone();
        if !is_multi {
            return view().into_element();
        }
        let Some(probe) = multi_status_store.get() else {
            return view().into_element();
        };
        let branch = multi_status_branch.get();
        let mode = multi_status_mode.get();
        let mut rows = view().flex_col().gap(zpx(5.0));
        for member in probe.members {
            let status = multi_member_status(&member.info, &branch, mode);
            let (label, color) = match status {
                Ok(label) => (label, rgb_to_hsla(resolved.ui.muted_foreground)),
                Err(reason) => (reason, rgb_to_hsla(resolved.ui.destructive)),
            };
            rows = rows.child(
                view()
                    .flex_col()
                    .gap(zpx(2.0))
                    .child(text(member.dir).text_xs().text_color(rgb_to_hsla(resolved.ui.foreground)))
                    .child(text(label).text_xs().text_color(color)),
            );
        }
        rows.into_element()
    });

    let submit_store = store.clone();
    let submit_ops = store.project_operations.clone();
    let submit_id = project_id.clone();
    let submit_probe = repo;
    let submit_multi_probe = multi_repo;
    let submit_busy = busy;
    let submit_error = error;
    let submit = move || {
        if !submit_ops.is_current(context) || submit_busy.get() {
            return;
        }
        if project.is_none() {
            submit_error.set(Some(t!("project.missingProject")));
            return;
        }
        if loading.get() || probe_error.get().is_some() {
            submit_error.set(Some(t!("worktree.probeRequired")));
            return;
        }
        let branch_name = branch.get().trim().to_string();
        if branch_name.is_empty() {
            submit_error.set(Some(t!("worktree.branchRequired")));
            return;
        }
        let name_value = name.get().trim().to_string();
        let name_value = (!name_value.is_empty()).then_some(name_value);
        let manual = manual_directory.get();
        let dir_value = manual.then(|| directory.get().trim().to_string());
        if is_multi {
            let Some(probe) = submit_multi_probe.get() else {
                submit_error.set(Some(t!("worktree.probeRequired")));
                return;
            };
            if probe.members.is_empty() {
                submit_error.set(Some(t!("worktree.notDerivable")));
                return;
            }
            if !probe.members.iter().all(|member| {
                multi_member_status(&member.info, &branch_name, multi_mode.get()).is_ok()
            }) {
                submit_error.set(Some(t!("multi.blocked")));
                return;
            }
            let input = MultiWorktreeInput {
                name: name_value,
                mode: multi_mode.get(),
                branch: branch_name,
                dir: dir_value.filter(|dir| !dir.is_empty()),
            };
            submit_ops.derive_multi_worktree(
                submit_store.clone(),
                context,
                submit_id.clone(),
                input,
                submit_busy,
                submit_error,
            );
        } else {
            let Some(info) = submit_probe.get() else {
                submit_error.set(Some(t!("worktree.probeRequired")));
                return;
            };
            if !info.derivable {
                submit_error.set(Some(t!("worktree.notDerivable")));
                return;
            }
            let selected = selected_branch(&info, &branch_name);
            let local_exists = info.branches.iter().any(|item| !item.remote && item.name == branch_name);
            if selected.is_some_and(|item| item.checked_out_at.is_some()) {
                submit_error.set(Some(t!("worktree.branchInUse")));
                return;
            }
            let chosen_mode = mode.get();
            if chosen_mode == WorktreeMode::ExistingBranch && !local_exists {
                submit_error.set(Some(t!("worktree.selectExisting")));
                return;
            }
            if chosen_mode == WorktreeMode::NewBranch && local_exists {
                submit_error.set(Some(t!("worktree.branchExists")));
                return;
            }
            let suggested = single_suggested_dir(&info, &branch_name);
            let chosen_dir = if manual {
                directory.get().trim().to_string()
            } else {
                suggested.clone()
            };
            let occupied = info.branches.iter().any(|item| {
                item.dir_occupied && item.suggested_dir == suggested && chosen_dir == suggested
            });
            if occupied {
                submit_error.set(Some(t!("worktree.dirOccupied")));
                return;
            }
            let start_point = if chosen_mode == WorktreeMode::NewBranch {
                nonempty(&start_point.get())
            } else {
                None
            };
            let input = WorktreeInput {
                name: name_value,
                mode: chosen_mode,
                branch: branch_name,
                start_point,
                dir: manual.then(|| directory.get().trim().to_string()).filter(|dir| !dir.is_empty()),
            };
            submit_ops.derive_worktree(
                submit_store.clone(),
                context,
                submit_id.clone(),
                input,
                submit_busy,
                submit_error,
            );
        }
    };
    let submit_button_owner = Owner::current();
    let submit_button_operations = store.project_operations.clone();
    let submit_action = submit;

    let save_theme = resolved.clone();
    let save_label = dynamic(move || {
        text(if busy.get() {
            t!("worktree.creating")
        } else {
            t!("worktree.create")
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb_to_hsla(save_theme.ui.primary_foreground))
        .into_element()
    });
    let error_theme = resolved.clone();
    let error_view = dynamic(move || {
        error
            .get()
            .map(|message| text(message).text_xs().text_color(rgb_to_hsla(error_theme.ui.destructive)).into_element())
            .unwrap_or_else(|| view().into_element())
    });

    view()
        .flex_col()
        .child(header)
        .child(
            view()
                .flex_col()
                .overflow_y_auto()
                .p(zpx(14.0))
                .gap(zpx(10.0))
                .child(text(t!("worktree.intro", repo = project_name.as_str())).text_xs())
                .child(probe_view)
                .child(mode_selector)
                .child(branch_field)
                .child(multi_branch_options)
                .child(start_point_field)
                .child(name_field)
                .child(text(t!("worktree.suggestedDirectory")).text_xs().text_color(rgb_to_hsla(resolved.ui.muted_foreground)))
                .child(directory_input)
                .child(path_warning)
                .child(multi_default_hint)
                .child(multi_status)
                .child(error_view)
                .child(
                    view()
                        .items_center()
                        .py(zpx(7.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(resolved.ui.tint_strong))
                        .on_press(move || {
                            if submit_button_owner.is_live()
                                && submit_button_operations.is_current(context)
                            {
                                submit_action();
                            }
                        })
                        .child(save_label),
                ),
        )
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn selected_branch<'a>(info: &'a RepoInfo, name: &str) -> Option<&'a RepoBranch> {
    info.branches
        .iter()
        .find(|branch| !branch.remote && branch.name == name)
}

fn single_suggested_dir(info: &RepoInfo, branch: &str) -> String {
    selected_branch(info, branch)
        .map(|branch| branch.suggested_dir.clone())
        .or_else(|| {
            info.repo_dir
                .as_deref()
                .map(|repo| preview_dir(repo, branch))
        })
        .unwrap_or_default()
}

fn common_branch_names(probe: &MultiRepoProbe) -> Vec<String> {
    let Some(first) = probe.members.first() else {
        return Vec::new();
    };
    first
        .info
        .branches
        .iter()
        .filter(|branch| !branch.remote)
        .map(|branch| branch.name.clone())
        .filter(|name| {
            probe.members.iter().skip(1).all(|member| {
                member.info.branches.iter().any(|branch| !branch.remote && branch.name == *name)
            })
        })
        .collect()
}

fn union_branch_names(probe: &MultiRepoProbe) -> Vec<String> {
    let mut names = Vec::new();
    for member in &probe.members {
        for branch in &member.info.branches {
            if !branch.remote && !names.contains(&branch.name) {
                names.push(branch.name.clone());
            }
        }
    }
    names
}

fn multi_member_status(
    info: &RepoInfo,
    branch: &str,
    mode: MultiWorktreeMode,
) -> Result<String, String> {
    if !info.derivable {
        let reason = info
            .reason
            .as_ref()
            .map(worktree_reason)
            .unwrap_or_else(|| t!("worktree.notDerivable"));
        return Err(info
            .detail
            .as_ref()
            .map(|detail| format!("{reason}: {detail}"))
            .unwrap_or(reason));
    }
    let existing = info
        .branches
        .iter()
        .find(|candidate| !candidate.remote && candidate.name == branch);
    if let Some(existing) = existing.filter(|existing| existing.checked_out_at.is_some()) {
        return Err(t!("worktree.inUse", path = existing.checked_out_at.as_deref().unwrap_or_default()));
    }
    match mode {
        MultiWorktreeMode::Auto => Ok(if existing.is_some() {
            t!("multi.willReuse")
        } else {
            t!("multi.willCreate")
        }),
        MultiWorktreeMode::NewBranch if existing.is_some() => Err(t!("worktree.branchExists")),
        MultiWorktreeMode::NewBranch => Ok(t!("multi.willCreate")),
        MultiWorktreeMode::ExistingBranch if existing.is_some() => Ok(t!("multi.willReuse")),
        MultiWorktreeMode::ExistingBranch => Err(t!("multi.branchMissing")),
    }
}
