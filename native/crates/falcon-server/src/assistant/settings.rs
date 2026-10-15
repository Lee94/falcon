//! AI 助手的设置（docs/design/ai-assistant.md 2.3）：存在 `settings` 表，API key 用 `SecretBox`
//! 加密落库，**永不下发**——GET 只给有没有、末四位。
//!
//! 库里没设 key 时退回服务端进程的 `ANTHROPIC_API_KEY`（`ANTHROPIC_BASE_URL` 同理）：在终端里起
//! 服务端、环境里本来就有 key 的人不用再填一遍。

use falcon_proto::assistant::{
    AcpAgentConfig, AssistantSettings, AssistantSettingsPatch, DEFAULT_EFFORT, EFFORTS, MODELS, acp_presets,
};

use super::claude::ApiConfig;
use crate::crypto::SecretBox;
use crate::db::Db;

const KEY_BASE_URL: &str = "assistant.baseUrl";
const KEY_MODEL: &str = "assistant.model";
const KEY_EFFORT: &str = "assistant.effort";
const KEY_API_KEY: &str = "assistant.apiKey";
const KEY_AUTO_APPROVE: &str = "assistant.autoApprove";
const KEY_ACP: &str = "assistant.acpAgents";

/// ACP agent 列表：库里存的那份，后来的版本新加的预设补在后面（用户删不掉预设，只能关掉）
pub fn acp_agents(db: &Db) -> Vec<AcpAgentConfig> {
    let mut list: Vec<AcpAgentConfig> =
        db.get_setting(KEY_ACP).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    for p in acp_presets() {
        if !list.iter().any(|a| a.id == p.id) {
            list.push(p);
        }
    }
    list
}

/// `acp:<id>` → 它的配置（关掉的不算）
pub fn acp_agent(db: &Db, agent: &str) -> Option<AcpAgentConfig> {
    let id = agent.strip_prefix(falcon_proto::assistant::ACP_AGENT_PREFIX)?;
    acp_agents(db).into_iter().find(|a| a.id == id && a.enabled)
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn stored_key(db: &Db, secrets: &SecretBox) -> Option<String> {
    db.get_setting(KEY_API_KEY).and_then(|enc| secrets.decrypt(&enc).ok()).filter(|k| !k.is_empty())
}

fn hint(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    let head: String = key.chars().take(7).collect();
    format!("{head}…{tail}")
}

pub fn view(db: &Db, secrets: &SecretBox) -> AssistantSettings {
    let (has_api_key, api_key_hint) = match stored_key(db, secrets) {
        Some(k) => (true, Some(hint(&k))),
        None => match env("ANTHROPIC_API_KEY") {
            Some(_) => (true, Some("ANTHROPIC_API_KEY".into())),
            None => (false, None),
        },
    };
    AssistantSettings {
        base_url: db.get_setting(KEY_BASE_URL).unwrap_or_default(),
        model: db.get_setting(KEY_MODEL).filter(|m| !m.is_empty()).unwrap_or_else(|| MODELS[0].into()),
        effort: db.get_setting(KEY_EFFORT).filter(|e| EFFORTS.contains(&e.as_str())).unwrap_or_else(|| DEFAULT_EFFORT.into()),
        has_api_key,
        api_key_hint,
        auto_approve: db.get_setting(KEY_AUTO_APPROVE).as_deref() == Some("1"),
        acp_agents: acp_agents(db),
    }
}

/// 改设置。认不出的思考强度报错；模型可以手填（新模型不用等 falcon 升级）
pub fn apply(db: &Db, secrets: &SecretBox, patch: &AssistantSettingsPatch) -> Result<(), String> {
    if let Some(effort) = &patch.effort
        && !EFFORTS.contains(&effort.as_str())
    {
        return Err(format!("不认识的思考强度：{effort}"));
    }
    if let Some(url) = &patch.base_url {
        let url = url.trim();
        if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("Base URL 要以 http:// 或 https:// 开头".into());
        }
        db.set_setting(KEY_BASE_URL, url);
    }
    if let Some(model) = &patch.model {
        db.set_setting(KEY_MODEL, model.trim());
    }
    if let Some(effort) = &patch.effort {
        db.set_setting(KEY_EFFORT, effort);
    }
    if let Some(key) = &patch.api_key {
        let key = key.trim();
        db.set_setting(KEY_API_KEY, &if key.is_empty() { String::new() } else { secrets.encrypt(key) });
    }
    if let Some(auto) = patch.auto_approve {
        db.set_setting(KEY_AUTO_APPROVE, if auto { "1" } else { "0" });
    }
    if let Some(list) = &patch.acp_agents {
        let mut seen = std::collections::HashSet::new();
        for a in list {
            if a.id.trim().is_empty() || a.name.trim().is_empty() || a.command.trim().is_empty() {
                return Err("ACP agent 的名称和命令都不能为空".into());
            }
            if !seen.insert(a.id.clone()) {
                return Err(format!("ACP agent 的 id 重复：{}", a.id));
            }
        }
        db.set_setting(KEY_ACP, &serde_json::to_string(list).unwrap_or_else(|_| "[]".into()));
    }
    Ok(())
}

/// 发请求用的配置；没有 key 时给一句能直接显示的话
pub fn api_config(db: &Db, secrets: &SecretBox) -> Result<ApiConfig, String> {
    let api_key = stored_key(db, secrets)
        .or_else(|| env("ANTHROPIC_API_KEY"))
        .ok_or("还没有配置 API key：到「设置 → AI 助手」里填一个")?;
    let s = view(db, secrets);
    let base_url = if s.base_url.is_empty() { env("ANTHROPIC_BASE_URL").unwrap_or_default() } else { s.base_url };
    Ok(ApiConfig { base_url, api_key, model: s.model, effort: s.effort })
}

pub fn auto_approve(db: &Db) -> bool {
    db.get_setting(KEY_AUTO_APPROVE).as_deref() == Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Db, SecretBox) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let secrets = SecretBox::open(dir.path()).unwrap();
        (dir, db, secrets)
    }

    #[test]
    fn key_is_encrypted_and_only_hinted() {
        let (_d, db, secrets) = fixture();
        apply(&db, &secrets, &AssistantSettingsPatch { api_key: Some("sk-ant-api03-abcdef1a2b".into()), ..Default::default() })
            .unwrap();
        // 库里存的不是明文
        assert!(!db.get_setting(KEY_API_KEY).unwrap().contains("abcdef"));
        let v = view(&db, &secrets);
        assert!(v.has_api_key);
        assert_eq!(v.api_key_hint.as_deref(), Some("sk-ant-…1a2b"));
        assert_eq!(api_config(&db, &secrets).unwrap().api_key, "sk-ant-api03-abcdef1a2b");
        // 空串 = 清掉（环境里有 key 时会退回环境变量，这里只断言库里没了）
        apply(&db, &secrets, &AssistantSettingsPatch { api_key: Some(String::new()), ..Default::default() }).unwrap();
        assert_eq!(stored_key(&db, &secrets), None);
    }

    #[test]
    fn acp_presets_are_always_there_and_custom_ones_persist() {
        let (_d, db, secrets) = fixture();
        let ids: Vec<String> = view(&db, &secrets).acp_agents.into_iter().map(|a| a.id).collect();
        assert_eq!(ids, ["claude-code", "codex", "gemini"]);
        let mut list = acp_agents(&db);
        list[0].enabled = false;
        list.retain(|a| a.id != "codex");
        list.push(AcpAgentConfig { id: "x1".into(), name: "Goose".into(), command: "goose".into(), args: vec!["acp".into()], env: vec![], enabled: true });
        apply(&db, &secrets, &AssistantSettingsPatch { acp_agents: Some(list), ..Default::default() }).unwrap();
        let ids: Vec<String> = acp_agents(&db).into_iter().map(|a| a.id).collect();
        // 删掉的预设又回来了（补在后面），自定义的留着
        assert_eq!(ids, ["claude-code", "gemini", "x1", "codex"]);
        assert!(acp_agent(&db, "acp:claude-code").is_none(), "关掉的不算");
        assert_eq!(acp_agent(&db, "acp:x1").unwrap().command, "goose");
        let bad = vec![AcpAgentConfig { id: "y".into(), name: "".into(), command: "c".into(), ..Default::default() }];
        assert!(apply(&db, &secrets, &AssistantSettingsPatch { acp_agents: Some(bad), ..Default::default() }).is_err());
    }

    #[test]
    fn defaults_and_validation() {
        let (_d, db, secrets) = fixture();
        let v = view(&db, &secrets);
        assert_eq!(v.model, "claude-opus-5-5");
        assert_eq!(v.effort, "high");
        assert!(!v.auto_approve);
        assert!(apply(&db, &secrets, &AssistantSettingsPatch { effort: Some("turbo".into()), ..Default::default() }).is_err());
        assert!(apply(&db, &secrets, &AssistantSettingsPatch { base_url: Some("gw.local".into()), ..Default::default() }).is_err());
        apply(
            &db,
            &secrets,
            &AssistantSettingsPatch {
                model: Some(" claude-sonnet-5-5 ".into()),
                effort: Some("xhigh".into()),
                auto_approve: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let v = view(&db, &secrets);
        assert_eq!((v.model.as_str(), v.effort.as_str(), v.auto_approve), ("claude-sonnet-5-5", "xhigh", true));
    }
}
