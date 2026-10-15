//! 设置里的「AI 助手」（docs/design/ai-assistant.md 2.3）：内置 agent 的模型、API key、
//! 思考强度、审批策略。
//!
//! API key 只写不读：服务端永不下发，这里只看得到「已设置 · sk-ant-…1a2b」。输入框留空保存 =
//! 不改；「清除」才是删掉。模型是一组常用的加「其他」手填——新模型不用等 falcon 升级。

use falcon_core::assistant::{join_command, split_command};
use falcon_proto::assistant::{AcpAgentConfig, AssistantSettings, AssistantSettingsPatch, EFFORTS, MODELS};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use super::widgets::{SegOption, field, rows, section, segmented};
use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

pub struct AssistantPane {
    ws: Entity<Workspace>,
    settings: Option<AssistantSettings>,
    api_key: Entity<InputState>,
    base_url: Entity<InputState>,
    model: Entity<InputState>,
    /// 每个 ACP agent 的命令行（一行：程序 + 参数）
    acp_cmds: Vec<(String, Entity<InputState>)>,
    new_name: Entity<InputState>,
    new_cmd: Entity<InputState>,
    busy: bool,
    error: Option<String>,
}

impl AssistantPane {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sk-ant-…"));
        let base_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.anthropic.com".to_string()));
        let model = cx.new(|cx| InputState::new(window, cx).placeholder(MODELS[0].to_string()));
        for input in [&api_key, &base_url, &model] {
            cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            })
            .detach();
        }
        let new_name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("assistant.settings.acpNamePlaceholder").to_string()));
        let new_cmd = cx.new(|cx| InputState::new(window, cx).placeholder("goose acp".to_string()));
        for input in [&new_name, &new_cmd] {
            cx.subscribe(input, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }
        let mut this = Self {
            ws,
            settings: None,
            api_key,
            base_url,
            model,
            acp_cmds: Vec::new(),
            new_name,
            new_cmd,
            busy: false,
            error: None,
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.ws.read(cx).client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let res = client.assistant_settings().await;
            this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(s) => this.apply(s, window, cx),
                    Err(err) => this.error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply(&mut self, s: AssistantSettings, window: &mut Window, cx: &mut Context<Self>) {
        let (url, model) = (s.base_url.clone(), s.model.clone());
        self.base_url.update(cx, |i, cx| i.set_value(url, window, cx));
        self.model.update(cx, |i, cx| i.set_value(model, window, cx));
        self.api_key.update(cx, |i, cx| i.set_value("", window, cx));
        // 命令行输入框按 agent 建（列表变了才重建，别把正在改的字冲掉）
        let ids: Vec<&str> = s.acp_agents.iter().map(|a| a.id.as_str()).collect();
        let have: Vec<&str> = self.acp_cmds.iter().map(|(id, _)| id.as_str()).collect();
        if ids != have {
            self.acp_cmds = s
                .acp_agents
                .iter()
                .map(|a| {
                    let line = join_command(&a.command, &a.args);
                    (a.id.clone(), cx.new(|cx| InputState::new(window, cx).default_value(line)))
                })
                .collect();
        }
        self.settings = Some(s);
    }

    /// 存 ACP agent 列表：命令行按输入框里的拆开；`edit` 再改一下（开关、删、加）
    fn save_acp(&mut self, edit: impl FnOnce(&mut Vec<AcpAgentConfig>), window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.settings else { return };
        let mut list = s.acp_agents.clone();
        for a in list.iter_mut() {
            if let Some((_, input)) = self.acp_cmds.iter().find(|(id, _)| id == &a.id) {
                let words = split_command(&input.read(cx).value());
                if let Some((cmd, args)) = words.split_first() {
                    a.command = cmd.clone();
                    a.args = args.to_vec();
                }
            }
        }
        edit(&mut list);
        self.put(AssistantSettingsPatch { acp_agents: Some(list), ..Default::default() }, window, cx);
    }

    fn add_acp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_name.read(cx).value().trim().to_string();
        let words = split_command(&self.new_cmd.read(cx).value());
        let Some((cmd, args)) = words.split_first() else { return };
        if name.is_empty() {
            return;
        }
        let agent = AcpAgentConfig {
            id: format!("custom-{}", web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_millis())),
            name,
            command: cmd.clone(),
            args: args.to_vec(),
            env: Vec::new(),
            enabled: true,
        };
        self.new_name.update(cx, |i, cx| i.set_value("", window, cx));
        self.new_cmd.update(cx, |i, cx| i.set_value("", window, cx));
        self.save_acp(move |list| list.push(agent), window, cx);
    }

    fn put(&mut self, patch: AssistantSettingsPatch, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let client = self.ws.read(cx).client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let res = client.update_assistant_settings(&patch).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match res {
                    Ok(s) => {
                        // 助手面板里各段对话的底栏（缺 key 的提示、可选的 ACP agent）跟着变
                        if let Some(panel) = this.ws.read(cx).assistant.clone() {
                            let s = s.clone();
                            panel.update(cx, |p, cx| p.set_settings(s, cx));
                        }
                        this.apply(s, window, cx);
                        this.ws.update(cx, |w, cx| w.toast(ToastKind::Success, t!("assistant.settings.saved").to_string(), None, cx));
                    }
                    Err(err) => this.error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 保存三个输入框：key 留空 = 不改
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).value().trim().to_string();
        let patch = AssistantSettingsPatch {
            api_key: (!key.is_empty()).then_some(key),
            base_url: Some(self.base_url.read(cx).value().trim().to_string()),
            model: Some(self.model.read(cx).value().trim().to_string()).filter(|m| !m.is_empty()),
            ..Default::default()
        };
        self.put(patch, window, cx);
    }
}

impl Render for AssistantPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let Some(s) = self.settings.clone() else {
            return div()
                .text_sm()
                .text_color(ui.muted_foreground)
                .child(self.error.clone().unwrap_or_else(|| t!("assistant.settings.loading").to_string()))
                .into_any_element();
        };
        let this = cx.entity();

        // ---- 模型与密钥 ----
        let key_state = match &s.api_key_hint {
            Some(h) if s.has_api_key => t!("assistant.settings.keySet", hint = h).to_string(),
            _ => t!("assistant.settings.keyUnset").to_string(),
        };
        let clear = {
            let this = this.clone();
            Button::new("assistant-key-clear")
                .ghost()
                .small()
                .label(t!("assistant.settings.clearKey").to_string())
                .disabled(!s.has_api_key || self.busy)
                .on_click(move |_, window, cx| {
                    this.update(cx, |v, cx| {
                        v.put(AssistantSettingsPatch { api_key: Some(String::new()), ..Default::default() }, window, cx)
                    })
                })
        };
        let models = div().flex().flex_wrap().gap_1().children(MODELS.iter().map(|m| {
            let this = this.clone();
            let on = self.model.read(cx).value() == *m;
            Button::new(gpui_kit::SharedString::from(format!("assistant-model-{m}")))
                .xsmall()
                .map(|b| if on { b.primary() } else { b.ghost() })
                .label(m.to_string())
                .on_click(move |_, window, cx| {
                    this.update(cx, |v, cx| v.model.update(cx, |i, cx| i.set_value(m.to_string(), window, cx)))
                })
        }));
        let save = {
            let this = this.clone();
            Button::new("assistant-save")
                .primary()
                .small()
                .label(t!("assistant.settings.save").to_string())
                .disabled(self.busy)
                .on_click(move |_, window, cx| this.update(cx, |v, cx| v.save(window, cx)))
        };
        let model_section = section(
            t!("assistant.settings.modelTitle").to_string(),
            Some(t!("assistant.settings.modelHint").to_string()),
            vec![
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .max_w(zpx(560.))
                    .child(field(
                        t!("assistant.settings.apiKey").to_string(),
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Input::new(&self.api_key))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().text_xs().text_color(ui.muted_foreground).child(key_state))
                                    .child(clear),
                            ),
                        cx,
                    ))
                    .child(field(
                        t!("assistant.settings.model").to_string(),
                        div().flex().flex_col().gap_2().child(models).child(Input::new(&self.model)),
                        cx,
                    ))
                    .child(field(
                        t!("assistant.settings.baseUrl").to_string(),
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Input::new(&self.base_url))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(ui.muted_foreground)
                                    .child(t!("assistant.settings.baseUrlHint").to_string()),
                            ),
                        cx,
                    ))
                    .children(self.error.clone().map(|e| div().text_xs().text_color(ui.destructive).child(e)))
                    .child(div().flex().child(save))
                    .into_any_element(),
            ],
            cx,
        );

        // ---- 行为 ----
        let effort = {
            let this = this.clone();
            let current = EFFORTS.iter().position(|e| *e == s.effort).unwrap_or(2);
            segmented(
                "assistant-effort",
                current,
                EFFORTS
                    .iter()
                    .enumerate()
                    .map(|(i, e)| SegOption { value: i, label: t!(format!("assistant.settings.effort_{e}")).to_string(), icon: None })
                    .collect(),
                true,
                move |i, window, cx| {
                    this.update(cx, |v, cx| {
                        v.put(AssistantSettingsPatch { effort: Some(EFFORTS[i].to_string()), ..Default::default() }, window, cx)
                    })
                },
                cx,
            )
        };
        let auto = {
            let this = this.clone();
            Switch::new("assistant-auto-approve").checked(s.auto_approve).on_click(move |checked: &bool, window, cx| {
                let checked = *checked;
                this.update(cx, |v, cx| {
                    v.put(AssistantSettingsPatch { auto_approve: Some(checked), ..Default::default() }, window, cx)
                })
            })
        };
        let behavior = section(
            t!("assistant.settings.behaviorTitle").to_string(),
            None,
            rows(
                vec![
                    (t!("assistant.settings.effort").to_string(), Some(t!("assistant.settings.effortHint").to_string()), effort.into_any_element()),
                    (
                        t!("assistant.settings.autoApprove").to_string(),
                        Some(t!("assistant.settings.autoApproveHint").to_string()),
                        auto.into_any_element(),
                    ),
                ],
                false,
                cx,
            ),
            cx,
        );

        // ---- ACP agent ----
        let mut acp_rows = Vec::new();
        for (i, a) in s.acp_agents.iter().enumerate() {
            let Some((_, input)) = self.acp_cmds.iter().find(|(id, _)| id == &a.id) else { continue };
            let toggle = {
                let this = this.clone();
                let id = a.id.clone();
                Switch::new(gpui_kit::SharedString::from(format!("acp-on-{}", a.id))).checked(a.enabled).on_click(
                    move |checked: &bool, window, cx| {
                        let (checked, id) = (*checked, id.clone());
                        this.update(cx, |v, cx| {
                            v.save_acp(
                                move |list| {
                                    if let Some(x) = list.iter_mut().find(|x| x.id == id) {
                                        x.enabled = checked;
                                    }
                                },
                                window,
                                cx,
                            )
                        })
                    },
                )
            };
            let remove = (!a.is_preset()).then(|| {
                let this = this.clone();
                let id = a.id.clone();
                Button::new(gpui_kit::SharedString::from(format!("acp-del-{}", a.id)))
                    .ghost()
                    .xsmall()
                    .label(t!("assistant.settings.acpRemove").to_string())
                    .on_click(move |_, window, cx| {
                        let id = id.clone();
                        this.update(cx, |v, cx| v.save_acp(move |list| list.retain(|x| x.id != id), window, cx))
                    })
            });
            acp_rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_3()
                    .when(i + 1 < s.acp_agents.len(), |d| d.border_b_1().border_color(ui.border))
                    .child(div().w(zpx(120.)).flex_none().text_sm().child(a.name.clone()))
                    .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                    .children(remove)
                    .child(toggle)
                    .into_any_element(),
            );
        }
        let save_acp = {
            let this = this.clone();
            Button::new("acp-save")
                .small()
                .label(t!("assistant.settings.acpSave").to_string())
                .disabled(self.busy)
                .on_click(move |_, window, cx| this.update(cx, |v, cx| v.save_acp(|_| {}, window, cx)))
        };
        let can_add = !self.new_name.read(cx).value().trim().is_empty() && !split_command(&self.new_cmd.read(cx).value()).is_empty();
        let add = {
            let this = this.clone();
            Button::new("acp-add")
                .small()
                .label(t!("assistant.settings.acpAdd").to_string())
                .disabled(!can_add || self.busy)
                .on_click(move |_, window, cx| this.update(cx, |v, cx| v.add_acp(window, cx)))
        };
        acp_rows.push(div().pt_3().flex().child(save_acp).into_any_element());
        acp_rows.push(
            div()
                .mt_4()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w(zpx(120.)).flex_none().child(Input::new(&self.new_name).small()))
                .child(div().flex_1().min_w_0().child(Input::new(&self.new_cmd).small()))
                .child(add)
                .into_any_element(),
        );
        let acp = section(
            t!("assistant.settings.acpTitle").to_string(),
            Some(t!("assistant.settings.acpHint").to_string()),
            acp_rows,
            cx,
        );

        div().flex().flex_col().child(model_section).child(behavior).child(acp).into_any_element()
    }
}
