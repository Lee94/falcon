//! 右侧栏插件双轨渲染与 `.nshpkg` Ed25519 信任链校验（ADR 0025 + ADR 0028）。
//!
//! - **轨道 A（声明式面板 `PluginView`）**：完整绘制全部 12 种 `PluginBlock` 原语（含 `Chart` 多序列折线画布），
//!   `PluginTone` 严格经 `plugin_tone_color` 映射到当前激活的 `falcon-theme` 语义 token，未知块静默跳过；
//! - **轨道 B（`.nshpkg` 签名沙盒面板包）**：[`ClientPluginVerifier`] 包装 `nsh_package::TrustStore`，
//!   只信任当前连接的自托管 `falcon-server` Ed25519 信任根公钥，执行签名链校验、SHA-256 摘要固定与构建号防回滚拦截。

use std::collections::BTreeMap;
use std::path::Path;

use falcon_proto::{
    PluginAction, PluginAlign, PluginBlock, PluginButton, PluginColumn, PluginInfo, PluginKind,
    PluginKv, PluginListItem, PluginOption, PluginSeries, PluginStat, PluginTone, PluginView,
};
use nsh_core::canvas::CanvasTextBaseline;
use nsh_core::prelude::*;
use nsh_package::{Error as PkgError, PublicKey, TrustStore};

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{PackageVerificationState, PluginPanelState, WorkspaceStore, use_workspace};
use crate::theme::{plugin_tone_color, rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::{pt_px, zpx};

/// 客户端 `.nshpkg` 插件包校验器（包装 `nsh_package::TrustStore` 的根公钥锚定与版本防回滚水位）。
#[derive(Debug, Default)]
pub struct ClientPluginVerifier {
    store: TrustStore,
}

impl ClientPluginVerifier {
    /// 创建纯内存校验器（供离线运行、Web 端与单元测试使用）。
    pub fn new_in_memory() -> Self {
        Self {
            store: TrustStore::new(),
        }
    }

    /// 打开持久化防回滚信任库（记录每个插件包已接受的最高 `build` 水位）。
    pub fn open_persistent(trust_dir: &Path) -> Result<Self, String> {
        let store = TrustStore::open(trust_dir).map_err(|e| e.to_string())?;
        Ok(Self { store })
    }

    /// 注入服务端 `GET /api/plugins/trust` 返回的 Ed25519 信任根公钥，校验并接受 `.nshpkg` 包字节。
    pub fn verify_and_accept(
        &mut self,
        root_public_key_b64: &str,
        expected_app_id: Option<&str>,
        pkg_bytes: &[u8],
    ) -> PackageVerificationState {
        let root_pub = match PublicKey::from_base64(root_public_key_b64.trim()) {
            Ok(pk) => pk,
            Err(e) => {
                return PackageVerificationState::Untrusted {
                    reason: e.to_string(),
                };
            }
        };
        self.store.add_root(root_pub);

        match self.store.verify(pkg_bytes, "") {
            Ok(verified) => {
                if let Some(expected) = expected_app_id.filter(|s| !s.is_empty())
                    && verified.app.id != expected
                {
                    return PackageVerificationState::Untrusted {
                        reason: format!(
                            "app_id mismatch: expected {expected}, got {}",
                            verified.app.id
                        ),
                    };
                }
                if let Err(e) = self.store.accept(&verified) {
                    return PackageVerificationState::Untrusted {
                        reason: e.to_string(),
                    };
                }
                PackageVerificationState::Verified {
                    app_id: verified.app.id,
                    version: verified.app.version,
                    build: verified.app.build,
                    sha256: verified.sha256,
                }
            }
            Err(PkgError::Rollback {
                build, accepted, ..
            }) => PackageVerificationState::RollbackBlocked {
                current: accepted,
                incoming: build,
            },
            Err(other) => PackageVerificationState::Untrusted {
                reason: other.to_string(),
            },
        }
    }
}

/// 右侧栏插件面板属性。
#[derive(Clone, PartialEq, Props)]
pub struct PluginPanelProps {
    pub plugin_id: String,
}

/// 右侧栏插件面板组件（同时覆盖声明式 JSON `PluginView` 与 `.nshpkg` 签名包安全卡片）。
#[component]
pub fn PluginPanel(props: PluginPanelProps) -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let pid = props.plugin_id.clone();

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let list = store.plugin_list.get();
            let states = store.plugin_states.get();
            let info = list.plugins.iter().find(|p| p.id == pid).cloned();
            let state = states.get(&pid).cloned();

            let Some(info) = info else {
                let id_str = pid.clone();
                return view()
                    .p(zpx(12.0))
                    .child(
                        text(t!("plugin.missing", id = id_str.as_str()))
                            .text_sm()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .into_element();
            };

            render_plugin_body(&store, &resolved, &info, state.as_ref()).into_element()
        }))
}

fn render_plugin_body(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    info: &PluginInfo,
    state: Option<&PluginPanelState>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let pid = info.id.clone();
    let subtitle = state
        .and_then(|s| s.view.as_ref())
        .and_then(|v| v.title.clone())
        .or_else(|| info.description.clone());

    let mut header = view()
        .flex_row()
        .items_center()
        .justify_between()
        .px(zpx(12.0))
        .py(zpx(8.0))
        .border_b_1()
        .border_color(rgba_to_hsla(ui.border))
        .child(
            view()
                .flex_col()
                .gap(zpx(2.0))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.0))
                        .child(
                            text(info.name.clone())
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(render_kind_badge(resolved, &info.kind)),
                )
                .when_some(subtitle, |el, sub| {
                    el.child(
                        text(sub)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                }),
        );

    {
        let store_refresh = store.clone();
        let pid_refresh = pid.clone();
        header = header.child(
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || {
                    store_refresh.dispatch_plugin_action(&pid_refresh, None, None);
                })
                .child(
                    text(t!("plugin.refresh"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    let mut content = view().flex_col().gap(zpx(10.0)).p(zpx(12.0));

    if !info.enabled {
        content = content.child(
            text(t!("plugin.disabled"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
        return view().size_full().flex_col().child(header).child(content);
    }

    if let Some(err) = &info.error {
        content = content.child(
            view()
                .flex_col()
                .gap(zpx(4.0))
                .p(zpx(8.0))
                .rounded(zpx(6.0))
                .bg(rgb_to_hsla(ui.muted))
                .child(
                    text(t!("plugin.broken"))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.destructive)),
                )
                .child(
                    text(err.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        );
        return view().size_full().flex_col().child(header).child(content);
    }

    // 轨道 B：签名沙盒面板包信任链状态卡片
    if info.kind == PluginKind::Package
        || state.and_then(|s| s.package_status.as_ref()).is_some()
    {
        content = content.child(render_package_card(resolved, info, state));
    }

    if let Some(st) = state {
        if let Some(err) = &st.error {
            content = content.child(
                text(format!("{} {err}", t!("plugin.failed")))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            );
        }
        if let Some(doc) = &st.view {
            content = content.child(render_plugin_document(
                store,
                resolved,
                &pid,
                doc,
                &st.params,
            ));
        } else if st.loading {
            content = content.child(
                text(t!("plugin.loading"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        } else {
            content = content.child(
                text(t!("plugin.empty"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
    } else {
        content = content.child(
            text(t!("plugin.empty"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }

    view()
        .size_full()
        .flex_col()
        .child(header)
        .child(scroll().flex_1().child(content))
}

fn render_kind_badge(resolved: &falcon_theme::ResolvedTheme, kind: &PluginKind) -> impl IntoElement {
    let ui = &resolved.ui;
    let (label, fg) = match kind {
        PluginKind::Builtin => (t!("plugin.kindBuiltin"), rgb_to_hsla(ui.muted_foreground)),
        PluginKind::Command | PluginKind::Unknown => {
            (t!("plugin.kindCommand"), rgb_to_hsla(ui.muted_foreground))
        }
        PluginKind::Package => (t!("plugin.kindPackage"), rgb_to_hsla(ui.tint_strong)),
    };
    view()
        .px(zpx(6.0))
        .py(zpx(1.5))
        .rounded(zpx(4.0))
        .bg(rgb_to_hsla(ui.muted))
        .child(text(label).text_xs().text_color(fg))
}

fn render_package_card(
    resolved: &falcon_theme::ResolvedTheme,
    info: &PluginInfo,
    state: Option<&PluginPanelState>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let status = state.and_then(|s| s.package_status.as_ref());

    let (headline, tone_color, hash_line) = match status {
        Some(PackageVerificationState::Verified {
            version,
            build,
            sha256,
            ..
        }) => (
            t!(
                "plugin.packageVerified",
                version = version.as_str(),
                build = build.to_string().as_str()
            ),
            rgb_to_hsla(ui.success),
            Some(sha256.clone()),
        ),
        Some(PackageVerificationState::RollbackBlocked { current, incoming }) => (
            t!(
                "plugin.packageRollback",
                current = current.to_string().as_str(),
                incoming = incoming.to_string().as_str()
            ),
            rgb_to_hsla(ui.warning),
            None,
        ),
        Some(PackageVerificationState::Untrusted { reason }) => (
            t!("plugin.packageUntrusted", reason = reason.as_str()),
            rgb_to_hsla(ui.destructive),
            None,
        ),
        None => {
            if let Some(pkg) = &info.package {
                (
                    t!(
                        "plugin.packageVerified",
                        version = pkg.version.as_str(),
                        build = pkg.build.to_string().as_str()
                    ),
                    rgb_to_hsla(ui.tint_strong),
                    Some(pkg.sha256.clone()),
                )
            } else {
                (t!("plugin.packageLoad"), rgb_to_hsla(ui.muted_foreground), None)
            }
        }
    };

    view()
        .flex_col()
        .gap(zpx(4.0))
        .p(zpx(8.0))
        .rounded(zpx(8.0))
        .border_1()
        .border_color(rgba_to_hsla(ui.border))
        .bg(rgb_to_hsla(ui.card))
        .child(
            text(headline)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tone_color),
        )
        .when_some(hash_line, |el, sha| {
            let short = if sha.len() > 16 { &sha[..16] } else { &sha };
            el.child(
                text(t!("plugin.packageHash", sha256 = short))
                    .text_xs()
                    .font_family(FONT_FAMILY_MONO)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
        })
        .child(
            text(t!("plugin.packageSandbox"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
}

/// 轨道 A：声明式 `PluginView` 块级渲染器（覆盖全部 12 种 `PluginBlock`）。
fn render_plugin_document(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    plugin_id: &str,
    doc: &PluginView,
    current_params: &BTreeMap<String, String>,
) -> impl IntoElement {
    let mut col = view().flex_col().gap(zpx(10.0));
    for block in &doc.blocks {
        match block {
            PluginBlock::Heading {
                text: heading,
                detail,
            } => {
                col = col.child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .pt(zpx(4.0))
                        .child(
                            text(heading.clone())
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(resolved.ui.foreground)),
                        )
                        .when_some(detail.clone(), |el, d| {
                            el.child(
                                text(d)
                                    .text_xs()
                                    .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                            )
                        }),
                );
            }
            PluginBlock::Text {
                text: body,
                tone,
                mono,
            } => {
                let mut t_el = text(body.clone())
                    .text_xs()
                    .text_color(plugin_tone_color(resolved, tone.as_ref()));
                if *mono {
                    t_el = t_el.font_family(FONT_FAMILY_MONO);
                }
                col = col.child(t_el);
            }
            PluginBlock::Notice { text: msg, tone } => {
                col = col.child(
                    view()
                        .p(zpx(8.0))
                        .rounded(zpx(6.0))
                        .bg(rgb_to_hsla(resolved.ui.muted))
                        .child(
                            text(msg.clone())
                                .text_xs()
                                .text_color(plugin_tone_color(resolved, tone.as_ref())),
                        ),
                );
            }
            PluginBlock::Stats { items } => {
                col = col.child(render_stats_block(resolved, items));
            }
            PluginBlock::Meter {
                label,
                value,
                max,
                text: right_text,
                tone,
            } => {
                col = col.child(render_meter_block(
                    resolved,
                    label,
                    *value,
                    max.unwrap_or(100.0),
                    right_text.as_deref(),
                    tone.as_ref(),
                ));
            }
            PluginBlock::Chart {
                label,
                text: reading,
                series,
                max,
                slots,
            } => {
                col = col.child(render_chart_block(
                    resolved,
                    label.as_deref(),
                    reading.as_deref(),
                    series,
                    *max,
                    *slots,
                ));
            }
            PluginBlock::Kv { items } => {
                col = col.child(render_kv_block(resolved, items));
            }
            PluginBlock::Table { columns, rows } => {
                col = col.child(render_table_block(resolved, columns, rows));
            }
            PluginBlock::List { items } => {
                col = col.child(render_list_block(store, resolved, plugin_id, items));
            }
            PluginBlock::Actions { items } => {
                col = col.child(render_actions_block(store, resolved, plugin_id, items));
            }
            PluginBlock::Select {
                param,
                label,
                value,
                options,
            } => {
                let active_val = value
                    .clone()
                    .or_else(|| current_params.get(param).cloned())
                    .or_else(|| options.first().map(|o| o.value.clone()))
                    .unwrap_or_default();
                col = col.child(render_select_block(
                    store,
                    resolved,
                    plugin_id,
                    param,
                    label.as_deref(),
                    &active_val,
                    options,
                ));
            }
            PluginBlock::Divider => {
                col = col.child(
                    view()
                        .w_full()
                        .h(pt_px(1.0))
                        .bg(rgba_to_hsla(resolved.ui.border)),
                );
            }
            PluginBlock::Unknown => {}
        }
    }
    col
}

fn render_stats_block(
    resolved: &falcon_theme::ResolvedTheme,
    items: &[PluginStat],
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .flex_row()
        .flex_wrap()
        .gap(zpx(8.0))
        .children(items.iter().map(|item| {
            view()
                .min_w(zpx(124.0))
                .flex_1()
                .flex_col()
                .gap(zpx(2.0))
                .p(zpx(8.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.card))
                .border_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(item.label.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    text(item.value.clone())
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(plugin_tone_color(resolved, item.tone.as_ref())),
                )
                .when_some(item.detail.clone(), |el, d| {
                    el.child(
                        text(d)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                })
        }))
}

fn render_meter_block(
    resolved: &falcon_theme::ResolvedTheme,
    label: &str,
    value: f64,
    max: f64,
    right_text: Option<&str>,
    tone: Option<&PluginTone>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let denom = if max > 0.0 { max } else { 100.0 };
    let ratio = (value / denom).clamp(0.0, 1.0) as f32;
    let reading = right_text
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:.0}%", ratio * 100.0));
    let bar_color = match tone {
        None | Some(PluginTone::Default) => rgb_to_hsla(ui.tint_strong),
        other => plugin_tone_color(resolved, other),
    };

    view()
        .flex_col()
        .gap(zpx(4.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text(label.to_string())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(reading)
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                ),
        )
        .child(
            view()
                .w_full()
                .h(zpx(6.0))
                .rounded(zpx(3.0))
                .overflow_hidden()
                .bg(rgb_to_hsla(ui.muted))
                .child(
                    view()
                        .h_full()
                        .w(relative(ratio))
                        .rounded(zpx(3.0))
                        .bg(bar_color),
                ),
        )
}

fn render_chart_block(
    resolved: &falcon_theme::ResolvedTheme,
    label: Option<&str>,
    reading: Option<&str>,
    series: &[PluginSeries],
    max_override: Option<f64>,
    slots_override: Option<u32>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let computed_max = max_override
        .unwrap_or_else(|| {
            series
                .iter()
                .flat_map(|s| s.values.iter().copied())
                .fold(1.0_f64, f64::max)
        })
        .max(1e-6);

    let series_owned = series.to_vec();
    let grid_stroke = rgba_to_hsla(ui.border);
    let palette_lines: Vec<Hsla> = series
        .iter()
        .enumerate()
        .map(|(idx, s)| match &s.tone {
            Some(tone) => plugin_tone_color(resolved, Some(tone)),
            None => rgb_to_hsla(ui.graph[idx % ui.graph.len()]),
        })
        .collect();

    let chart_w = 272.0_f32;
    let chart_h = 64.0_f32;

    view()
        .flex_col()
        .gap(zpx(4.0))
        .p(zpx(8.0))
        .rounded(zpx(8.0))
        .bg(rgb_to_hsla(ui.card))
        .border_1()
        .border_color(rgba_to_hsla(ui.border))
        .when(label.is_some() || reading.is_some(), |el| {
            el.child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .when_some(label.map(str::to_owned), |r, l| {
                        r.child(
                            text(l)
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                    })
                    .when_some(reading.map(str::to_owned), |r, rd| {
                        r.child(
                            text(rd)
                                .text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                    }),
            )
        })
        .child(
            canvas(move || {
                let mut p = Painter::new()
                    .line_width(1.0)
                    .stroke_style(grid_stroke)
                    .begin_path()
                    .move_to(0.0, chart_h * 0.5)
                    .line_to(chart_w, chart_h * 0.5)
                    .stroke()
                    .text_baseline(CanvasTextBaseline::Top);

                for (idx, s) in series_owned.iter().enumerate() {
                    if s.values.is_empty() {
                        continue;
                    }
                    let total_slots = slots_override
                        .map(|n| n.max(2) as usize)
                        .unwrap_or_else(|| s.values.len().max(2));
                    let count = s.values.len().min(total_slots);
                    let tail = &s.values[s.values.len() - count..];
                    let start_slot = total_slots - count;
                    let step_x = chart_w / (total_slots.saturating_sub(1).max(1) as f32);

                    p = p
                        .stroke_style(palette_lines[idx])
                        .line_width(1.6)
                        .begin_path();
                    for (i, &val) in tail.iter().enumerate() {
                        let slot = start_slot + i;
                        let x = (slot as f32) * step_x;
                        let norm = (val / computed_max).clamp(0.0, 1.0) as f32;
                        let y = (chart_h - 4.0) * (1.0 - norm) + 2.0;
                        if i == 0 {
                            p = p.move_to(x, y);
                        } else {
                            p = p.line_to(x, y);
                        }
                    }
                    p = p.stroke();
                }
                p
            })
            .w(pt_px(chart_w))
            .h(pt_px(chart_h)),
        )
}

fn render_kv_block(resolved: &falcon_theme::ResolvedTheme, items: &[PluginKv]) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .flex_col()
        .gap(zpx(4.0))
        .children(items.iter().map(|kv| {
            let mut val = text(kv.value.clone())
                .text_xs()
                .text_color(plugin_tone_color(resolved, kv.tone.as_ref()));
            if kv.mono {
                val = val.font_family(FONT_FAMILY_MONO);
            }
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(zpx(8.0))
                .child(
                    text(kv.key.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(val)
        }))
}

fn render_table_block(
    resolved: &falcon_theme::ResolvedTheme,
    columns: &[PluginColumn],
    rows: &[Vec<String>],
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .flex_col()
        .rounded(zpx(6.0))
        .border_1()
        .border_color(rgba_to_hsla(ui.border))
        .overflow_hidden()
        .child(
            view()
                .flex_row()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .bg(rgb_to_hsla(ui.muted))
                .children(columns.iter().map(|c| {
                    let mut cell = view().flex_1();
                    if matches!(c.align, Some(PluginAlign::Right)) {
                        cell = cell.items_end();
                    }
                    cell.child(
                        text(c.title.clone())
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                })),
        )
        .children(rows.iter().map(|row| {
            view()
                .flex_row()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .border_t_1()
                .border_color(rgba_to_hsla(ui.border))
                .children(columns.iter().enumerate().map(|(i, c)| {
                    let val = row.get(i).cloned().unwrap_or_default();
                    let mut cell = view().flex_1();
                    if matches!(c.align, Some(PluginAlign::Right)) {
                        cell = cell.items_end();
                    }
                    cell.child(
                        text(val)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                }))
        }))
}

fn render_list_block(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    plugin_id: &str,
    items: &[PluginListItem],
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .flex_col()
        .gap(zpx(4.0))
        .children(items.iter().map(|item| {
            let mut row = view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(zpx(6.0))
                .p(zpx(6.0))
                .rounded(zpx(6.0))
                .bg(rgb_to_hsla(ui.card));

            if let Some(act) = item.action.clone() {
                let st = store.clone();
                let pid = plugin_id.to_string();
                row = row.cursor_pointer().on_press(move || {
                    execute_plugin_action(&st, &pid, &act);
                });
            }

            let left = view()
                .flex_row()
                .items_center()
                .gap(zpx(6.0))
                .when_some(item.tone.as_ref(), |el, tone| {
                    el.child(
                        view()
                            .w(zpx(6.0))
                            .h(zpx(6.0))
                            .rounded_full()
                            .bg(plugin_tone_color(resolved, Some(tone))),
                    )
                })
                .child(
                    view()
                        .flex_col()
                        .child(
                            text(item.title.clone())
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .when_some(item.detail.clone(), |el, d| {
                            el.child(
                                text(d)
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                        }),
                );

            let mut right = view().flex_row().items_center().gap(zpx(4.0));
            if let Some(badge) = &item.badge {
                right = right.child(
                    text(badge.clone())
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            }
            for btn in &item.menu {
                right = right.child(render_plugin_button(store, resolved, plugin_id, btn));
            }

            row.child(left).child(right)
        }))
}

fn render_actions_block(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    plugin_id: &str,
    items: &[PluginButton],
) -> impl IntoElement {
    view()
        .flex_row()
        .flex_wrap()
        .gap(zpx(6.0))
        .children(
            items
                .iter()
                .map(|btn| render_plugin_button(store, resolved, plugin_id, btn)),
        )
}

fn render_plugin_button(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    plugin_id: &str,
    btn: &PluginButton,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let (bg, fg) = match btn.tone {
        Some(PluginTone::Accent) => (
            rgb_to_hsla(ui.tint_strong),
            rgb_to_hsla(ui.primary_foreground),
        ),
        Some(PluginTone::Danger) => (
            rgb_to_hsla(ui.destructive),
            rgb_to_hsla(ui.destructive_foreground),
        ),
        _ => (
            rgb_to_hsla(ui.secondary),
            rgb_to_hsla(ui.secondary_foreground),
        ),
    };
    let st = store.clone();
    let pid = plugin_id.to_string();
    let act = btn.action.clone();
    view()
        .px(zpx(8.0))
        .py(zpx(4.0))
        .rounded(zpx(6.0))
        .cursor_pointer()
        .bg(bg)
        .on_press(move || execute_plugin_action(&st, &pid, &act))
        .child(text(btn.label.clone()).text_xs().text_color(fg))
}

fn render_select_block(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    plugin_id: &str,
    param: &str,
    label: Option<&str>,
    active_value: &str,
    options: &[PluginOption],
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .flex_col()
        .gap(zpx(4.0))
        .when_some(label.map(str::to_owned), |el, l| {
            el.child(
                text(l)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
        })
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(zpx(4.0))
                .children(options.iter().map(|opt| {
                    let selected = opt.value == active_value;
                    let bg = if selected {
                        rgb_to_hsla(ui.tint)
                    } else {
                        rgb_to_hsla(ui.secondary)
                    };
                    let fg = if selected {
                        rgb_to_hsla(ui.tint_foreground)
                    } else {
                        rgb_to_hsla(ui.secondary_foreground)
                    };
                    let st = store.clone();
                    let pid = plugin_id.to_string();
                    let p_key = param.to_string();
                    let p_val = opt.value.clone();
                    view()
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(5.0))
                        .cursor_pointer()
                        .bg(bg)
                        .on_press(move || {
                            set_plugin_param(&st, &pid, &p_key, &p_val);
                        })
                        .child(text(opt.label.clone()).text_xs().text_color(fg))
                })),
        )
}

fn set_plugin_param(store: &WorkspaceStore, plugin_id: &str, key: &str, val: &str) {
    store.plugin_states.update(|map| {
        let entry = map
            .entry(plugin_id.to_string())
            .or_insert_with(|| PluginPanelState {
                view: None,
                params: BTreeMap::new(),
                package_status: None,
                loading: false,
                error: None,
            });
        entry.params.insert(key.to_string(), val.to_string());
    });
    store.dispatch_plugin_action(plugin_id, None, None);
}

fn execute_plugin_action(store: &WorkspaceStore, plugin_id: &str, action: &PluginAction) {
    match action {
        PluginAction::Invoke { id, value, .. } => {
            store.dispatch_plugin_action(plugin_id, Some(id.as_str()), value.clone());
        }
        PluginAction::OpenUrl { url } => {
            if url.starts_with("https://") || url.starts_with("http://") {
                let _ = open_url(url.clone());
            }
        }
        PluginAction::Copy { text: content } => {
            set_clipboard(content.clone());
            store.show_toast(t!("plugin.copied"));
        }
        PluginAction::Refresh => {
            store.dispatch_plugin_action(plugin_id, None, None);
        }
        PluginAction::SetParam { param, value } => {
            set_plugin_param(store, plugin_id, param, value);
        }
        PluginAction::Unknown => {}
    }
}
