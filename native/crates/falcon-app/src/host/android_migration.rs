//! Android 旧版 Kotlin 客户端（`com.falcon.android`）一次性配置迁移。
//!
//! 旧版 Jetpack Compose 客户端将服务端地址列表、当前活动服务端 ID、主题名称及字号保存在
//! `<app_root>/shared_prefs/falcon_android_prefs.xml` 中。升级至 NSH 统一客户端后，首次启动若
//! `data_dir/profiles.json` 尚不存在且旧版 `falcon_android_prefs.xml` 存在，本模块直接解析该
//! `SharedPreferences` XML 并原子写入 `profiles.json` 与 `prefs.json`，无需拉入额外 XML 解析库。

#[cfg(any(target_os = "android", test))]
use std::collections::BTreeMap;
#[cfg(any(target_os = "android", test))]
use std::path::Path;

#[cfg(any(target_os = "android", test))]
use serde_json::json;

/// 若 `data_dir` 尚未初始化 `profiles.json`，尝试从 `shared_prefs_xml` 导入旧版配置。
#[cfg(any(target_os = "android", test))]
pub fn migrate_if_needed(shared_prefs_xml: &Path, data_dir: &Path) -> bool {
    if data_dir.join("profiles.json").is_file() {
        return false;
    }
    let Ok(xml) = std::fs::read_to_string(shared_prefs_xml) else {
        return false;
    };
    migrate_xml_str(&xml, data_dir)
}

#[cfg(any(target_os = "android", test))]
pub fn migrate_xml_str(xml: &str, data_dir: &Path) -> bool {
    let entries = parse_shared_prefs_xml(xml);
    if entries.is_empty() {
        return false;
    }

    if let Err(err) = std::fs::create_dir_all(data_dir) {
        log::warn!("Android 配置迁移建目录失败：{err}");
        return false;
    }

    let mut wrote_any = false;

    if let Some(servers_raw) = entries.get("servers_json")
        && let Ok(arr) = serde_json::from_str::<serde_json::Value>(servers_raw)
        && arr.is_array()
    {
        let active_id = entries
            .get("active_server_id")
            .cloned()
            .unwrap_or_default();
        let profiles_doc = json!({
            "activeId": active_id,
            "profiles": arr,
        });
        if let Ok(bytes) = serde_json::to_vec_pretty(&profiles_doc)
            && write_atomic(&data_dir.join("profiles.json"), &bytes).is_ok()
        {
            wrote_any = true;
        }
    }

    let mut prefs: BTreeMap<String, String> = std::fs::read_to_string(data_dir.join("prefs.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut prefs_changed = false;

    if let Some(theme) = entries.get("theme_name").filter(|s| !s.trim().is_empty()) {
        prefs.insert("falcon.theme.dark".into(), theme.clone());
        prefs.insert("falcon.theme.active".into(), theme.clone());
        prefs_changed = true;
    }
    if let Some(font_sp) = entries.get("term_font_size_sp") {
        prefs.insert("falcon.term.fontSize".into(), font_sp.clone());
        prefs_changed = true;
    }
    if let Some(extra_keys) = entries.get("term_extra_keys_expanded") {
        prefs.insert("falcon.mobile.extraKeysExpanded".into(), extra_keys.clone());
        prefs_changed = true;
    }

    if prefs_changed
        && let Ok(bytes) = serde_json::to_vec_pretty(&prefs)
        && write_atomic(&data_dir.join("prefs.json"), &bytes).is_ok()
    {
        wrote_any = true;
    }

    wrote_any
}

/// 解析 Android 标准 `SharedPreferences` XML（支持 `<string name="k">v</string>` 与 `<float|int|long|boolean name="k" value="v" />`）。
#[cfg(any(target_os = "android", test))]
pub fn parse_shared_prefs_xml(xml: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut rest = xml;

    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];
        if rest.starts_with("!--") {
            if let Some(end) = rest.find("-->") {
                rest = &rest[end + 3..];
                continue;
            }
            break;
        }
        if rest.starts_with('?') || rest.starts_with('/') {
            if let Some(gt) = rest.find('>') {
                rest = &rest[gt + 1..];
                continue;
            }
            break;
        }

        let Some(gt) = rest.find('>') else {
            break;
        };
        let open_tag = &rest[..gt];
        let self_closing = open_tag.trim_end().ends_with('/');
        let tag_name = open_tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/');

        match tag_name {
            "string" if !self_closing => {
                let name = extract_attr(open_tag, "name");
                let after_open = &rest[gt + 1..];
                if let Some(close_idx) = after_open.find("</string>") {
                    if let Some(k) = name {
                        let raw_val = &after_open[..close_idx];
                        map.insert(k, unescape_xml(raw_val));
                    }
                    rest = &after_open[close_idx + "</string>".len()..];
                    continue;
                }
                rest = after_open;
            }
            "int" | "long" | "float" | "boolean" => {
                if let (Some(k), Some(v)) = (extract_attr(open_tag, "name"), extract_attr(open_tag, "value")) {
                    map.insert(k, unescape_xml(&v));
                }
                rest = &rest[gt + 1..];
            }
            _ => {
                rest = &rest[gt + 1..];
            }
        }
    }

    map
}

#[cfg(any(target_os = "android", test))]
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle_dq = format!("{attr}=\"");
    if let Some(idx) = tag.find(&needle_dq) {
        let tail = &tag[idx + needle_dq.len()..];
        let end = tail.find('"')?;
        return Some(unescape_xml(&tail[..end]));
    }
    let needle_sq = format!("{attr}='");
    if let Some(idx) = tag.find(&needle_sq) {
        let tail = &tag[idx + needle_sq.len()..];
        let end = tail.find('\'')?;
        return Some(unescape_xml(&tail[..end]));
    }
    None
}

#[cfg(any(target_os = "android", test))]
fn unescape_xml(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(any(target_os = "android", test))]
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_kotlin_shared_prefs_xml_into_profiles_and_prefs() {
        let xml = r#"<?xml version='1.0' encoding='utf-8' standalone='yes' ?>
<map>
    <string name="servers_json">[{&quot;id&quot;:&quot;srv-lan&quot;,&quot;name&quot;:&quot;Home Mac&quot;,&quot;url&quot;:&quot;http://10.0.0.8:4923&quot;}]</string>
    <string name="active_server_id">srv-lan</string>
    <string name="theme_name">Catppuccin Mocha</string>
    <float name="term_font_size_sp" value="14.5" />
    <boolean name="term_extra_keys_expanded" value="false" />
</map>
"#;
        let dir = std::env::temp_dir().join(format!("falcon-android-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let xml_path = dir.join("falcon_android_prefs.xml");
        let out_dir = dir.join("data");
        std::fs::write(&xml_path, xml).unwrap();

        assert!(migrate_if_needed(&xml_path, &out_dir));
        // 幂等保护：已有 profiles.json 后不再覆盖用户后续修改
        assert!(!migrate_if_needed(&xml_path, &out_dir));

        let profiles: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out_dir.join("profiles.json")).unwrap()).unwrap();
        assert_eq!(profiles["activeId"], "srv-lan");
        assert_eq!(profiles["profiles"][0]["url"], "http://10.0.0.8:4923");

        let prefs: BTreeMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(out_dir.join("prefs.json")).unwrap()).unwrap();
        assert_eq!(prefs.get("falcon.theme.active").map(String::as_str), Some("Catppuccin Mocha"));
        assert_eq!(prefs.get("falcon.term.fontSize").map(String::as_str), Some("14.5"));
        assert_eq!(prefs.get("falcon.mobile.extraKeysExpanded").map(String::as_str), Some("false"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
