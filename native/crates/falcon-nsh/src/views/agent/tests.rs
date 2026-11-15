use super::*;
use crate::{FalconLaunchOptions, store::WorkspaceStore};
use falcon_client::SessionSink;
use falcon_proto::{
    AgentModelOption, AgentSlashCommand, AgentToolCall, AgentToolContent, AgentToolKind,
    AgentToolStatus, SessionAgent,
};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

#[component]
fn AgentHarness() -> impl IntoElement {
    crate::theme::install_theme(None);
    crate::zoom::install_zoom();
    crate::shell::action_owner::ActionOwner::install();
    let store = WorkspaceStore::install(&FalconLaunchOptions {
        offline_demo: true,
        ..Default::default()
    });
    let mut state = AgentViewState::new(SessionAgent::Claude, true);
    state.model = Some("model".into());
    state.model_options = vec![AgentModelOption {
        id: "model".into(),
        label: "Test Model".into(),
        resolved_model: None,
        reasoning_efforts: vec!["low".into(), "high".into()],
        default_reasoning_effort: Some("high".into()),
    }];
    state.commands = vec![
        AgentSlashCommand {
            name: "plan".into(),
            description: "Plan work".into(),
        },
        AgentSlashCommand {
            name: "help".into(),
            description: "Show help".into(),
        },
    ];
    state.entries = vec![
        AgentEntry::User {
            id: "u".into(),
            text: "Request".into(),
        },
        AgentEntry::Thought {
            id: "thought".into(),
            text: "first thought\nlast thought".into(),
        },
        AgentEntry::ToolCall(AgentToolCall {
            id: "tool".into(),
            title: "Edit file".into(),
            description: None,
            file_path: Some("src/main.rs".into()),
            kind: AgentToolKind::Edit,
            status: AgentToolStatus::Completed,
            content: vec![AgentToolContent::Diff {
                path: "src/main.rs".into(),
                old_text: Some("old line".into()),
                new_text: Some("new line".into()),
                unified_diff: None,
            }],
            permission_pending: false,
            permission_reason: None,
        }),
        AgentEntry::Message {
            id: "message".into(),
            text: "**Completed**".into(),
        },
    ];
    let session = store.ensure_terminal_session("sess-agent");
    session.on_event(falcon_client::SessionEvent::Connected { reconnected: false });
    session.on_event(falcon_client::SessionEvent::AgentSnapshot(state.clone()));
    store.agent_views.update(|views| {
        let agent = views.get_mut("sess-agent").unwrap();
        agent.snapshot = Some(state);
        agent.available = true;
    });
    agent_pane_view().session_id("sess-agent").focused(true)
}

fn app() -> Headless {
    let _ = falcon_platform::shared();
    let root = Root::of::<AgentHarness>().translations(crate::i18n::translations());
    let mut cfg = EngineConfig::new(root);
    cfg.viewport = Size::new(Points(900.0), Points(900.0));
    cfg.ui_sets = vec![nsh_base::registry(), crate::registry()];
    Headless::start(cfg).unwrap()
}

#[test]
fn slash_selection_and_capability_menus_headless() {
    let mut app = app();
    app.first_render().unwrap();
    app.type_text("id:agent-composer-sess-agent", "/pL")
        .unwrap();
    app.wait_for("id:agent-slash-plan").unwrap();
    assert!(!app.shows("id:agent-slash-help"));
    app.press("id:agent-slash-plan").unwrap();
    assert_eq!(
        app.screen()
            .input_text("id:agent-composer-sess-agent")
            .unwrap(),
        Some("/plan ")
    );
    assert!(!app.shows("id:agent-slash-plan"));
    app.press("id:agent-model").unwrap();
    app.wait_for("role:menuitem[✓ Test Model]").unwrap();
    app.press_key("escape").unwrap();
    app.press("id:agent-effort").unwrap();
    app.wait_for("role:menuitem[High]").unwrap();
    // Without a live socket, submission cannot fabricate a user bubble or erase the draft.
    app.press_key("escape").unwrap();
    app.submit("id:agent-composer-sess-agent").unwrap();
    assert_eq!(
        app.screen()
            .input_text("id:agent-composer-sess-agent")
            .unwrap(),
        Some("/plan ")
    );
}

#[test]
fn grouped_activity_nested_folds_diff_and_style_headless() {
    let mut app = app();
    app.first_render().unwrap();
    assert!(!app.shows("id:agent-tool-tool"));
    app.press("id:agent-activity-thought").unwrap();
    app.wait_for("id:agent-tool-tool").unwrap();
    assert!(!app.shows("text:+ new line"));
    app.press("id:agent-tool-toggle-tool").unwrap();
    app.wait_for("text:+ new line").unwrap();
    app.wait_for("text:- old line").unwrap();
    app.press("id:agent-thought-thought").unwrap();
    app.wait_for("text:first thought\nlast thought").unwrap();
    app.press("label:agent-style-sess-agent").unwrap();
    app.wait_for("id:agent-text-u").unwrap();
    // Style changes retain the nested disclosure overrides.
    app.wait_for("text:+ new line").unwrap();
}
