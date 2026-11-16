//! 文件预览的本地交互；资源始终从服务端签发的 raw token 范围内读取。
use std::time::Duration;

use falcon_core::image_view::ImageGeometry;
use falcon_core::md_link::{external_href, markdown_image_url, relative_href, resolve_file_link};
use nsh_base::text_view::{Block, Document, Inline, Parser};
use nsh_core::element::{GestureEvent, animated_value};
use nsh_core::prelude::*;
use nsh_core::{TouchEvent, TouchPhase};

use crate::i18n::t;
use crate::shell::popups::themed_tooltip;
use crate::shell::widgets::{Glyph, icon_button};
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, PartialEq, Props)]
pub struct ImagePreviewProps {
    pub url: String,
}

#[component]
pub fn ImagePreview(props: ImagePreviewProps) -> impl IntoElement {
    let theme = use_theme();
    let geometry = signal(ImageGeometry {
        viewport: (0.0, 0.0),
        natural: (0.0, 0.0),
        padding: zpx(24.0).0.into(),
        zoom: None,
        scroll: (0.0, 0.0),
    });
    let loaded = signal(false);
    let failed = signal(false);
    let pointer = signal(None::<(f64, f64)>);
    let drag = signal(None::<((f64, f64), (f64, f64))>);
    let pinch = animated_value(1.0);
    let pinching = signal(0_u64);
    let pinch_start = signal(None::<(f64, (f64, f64))>);

    view()
        .flex_1()
        .min_h_0()
        .flex_col()
        .overflow_hidden()
        .child(
            view()
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .overflow_hidden()
                .test_id("file-image-viewport")
                .on_layout(move |b: &Bounds<Points>| {
                    geometry.update(|g| {
                        g.viewport = (b.size.width.0.into(), b.size.height.0.into());
                        g.padding = zpx(24.0).0.into();
                        g.pan(g.scroll);
                    })
                })
                .on_wheel(move |e: &WheelEvent| {
                    let at = (f64::from(e.position.x.0), f64::from(e.position.y.0));
                    pointer.set(Some(at));
                    let delta = e.delta.points(Points(100.0));
                    geometry.update(|g| {
                        if e.modifiers.ctrl || e.modifiers.cmd {
                            g.zoom_to(
                                Some(
                                    g.scale()
                                        * (f64::from(-delta.y.0) * 0.0025).clamp(-10.0, 10.0).exp(),
                                ),
                                at,
                            );
                        } else {
                            g.pan((
                                g.scroll.0 + f64::from(delta.x.0),
                                g.scroll.1 + f64::from(delta.y.0),
                            ));
                        }
                    });
                })
                .on_press_down(move |e: &PressEvent| {
                    let at = (f64::from(e.position.x.0), f64::from(e.position.y.0));
                    pointer.set(Some(at));
                    if e.clicks >= 2 {
                        drag.set(None);
                        geometry.update(|g| g.toggle_actual(at));
                    } else {
                        drag.set(Some((at, geometry.get().scroll)));
                    }
                })
                .on_mouse_move(move |e: &MouseEvent| {
                    let at = (f64::from(e.position.x.0), f64::from(e.position.y.0));
                    pointer.set(Some(at));
                    if e.pressed_button == Some(MouseButton::Left) {
                        if let Some((start, scroll)) = drag.get() {
                            geometry.update(|g| {
                                g.pan((scroll.0 - at.0 + start.0, scroll.1 - at.1 + start.1))
                            });
                        }
                    } else {
                        drag.set(None);
                    }
                })
                .on_press_up(move || drag.set(None))
                .on_mouse_leave(move |_: &MouseEvent| {
                    pointer.set(None);
                    drag.set(None);
                })
                .on_touch(move |e: &TouchEvent| {
                    // 图片不拦截命中测试，location 始终相对视口；触屏捏合使用两指中点。
                    let points: Vec<_> = e
                        .points
                        .iter()
                        .filter(|p| p.pointer_type == PointerType::Touch)
                        .collect();
                    if points.is_empty() {
                        return;
                    }
                    let count = points.len() as f64;
                    let at = (
                        points
                            .iter()
                            .map(|p| f64::from(p.location.x.0))
                            .sum::<f64>()
                            / count,
                        points
                            .iter()
                            .map(|p| f64::from(p.location.y.0))
                            .sum::<f64>()
                            / count,
                    );
                    pointer.set(Some(at));
                    if points.len() != 1 {
                        drag.set(None);
                        return;
                    }
                    match e.phase {
                        TouchPhase::Start => drag.set(Some((at, geometry.get().scroll))),
                        TouchPhase::Move => {
                            if let Some((start, scroll)) = drag.get() {
                                geometry.update(|g| {
                                    g.pan((scroll.0 - at.0 + start.0, scroll.1 - at.1 + start.1))
                                });
                            }
                        }
                        _ => drag.set(None),
                    }
                })
                .pinch_value(&pinch)
                .on_pinch_start(move |_: &GestureEvent| {
                    drag.set(None);
                    let g = geometry.get();
                    let at = pointer.get().unwrap_or_else(|| g.center());
                    pinch_start.set(Some((g.scale(), at)));
                    let generation = pinching.get().wrapping_add(1);
                    pinching.set(generation);
                    // NSH 把连续捏合写进原生 AnimatedValue；读取它更新几何，才能同时保持指针锚点和边界。
                    // 不重新挂载图片，也不在每一帧创建新的原生手势识别器。
                    spawn(async move {
                        while pinch_start.get().is_some() && pinching.get() == generation {
                            let ratio = pinch.value().await;
                            if pinching.get() != generation {
                                break;
                            }
                            if let Some((scale, at)) = pinch_start.get() {
                                geometry.update(|g| g.zoom_to(Some(scale * f64::from(ratio)), at));
                            }
                            timer(Duration::from_millis(16)).await;
                        }
                    });
                })
                .on_pinch_end(move |e: &GestureEvent| {
                    if let Some((scale, at)) = pinch_start.get() {
                        geometry.update(|g| g.zoom_to(Some(scale * f64::from(e.value)), at));
                    }
                    pinch_start.set(None);
                    pinching.update(|n| *n = n.wrapping_add(1));
                    pinch.set(1.0);
                })
                .child(
                    image(props.url)
                        .pointer_events(nsh_core::element::PointerEvents::None)
                        .test_id("file-image")
                        .absolute()
                        .left(move || Points(geometry.get().origin().0 as f32))
                        .top(move || Points(geometry.get().origin().1 as f32))
                        .w(move || {
                            let g = geometry.get();
                            Points((g.natural.0 * g.scale()).max(1.0) as f32)
                        })
                        .h(move || {
                            let g = geometry.get();
                            Points((g.natural.1 * g.scale()).max(1.0) as f32)
                        })
                        .on_load(move |s: &Size<nsh_core::geometry::DevicePixels>| {
                            geometry.update(|g| g.natural = (s.width.0 as f64, s.height.0 as f64));
                            loaded.set(true);
                        })
                        .on_error(move |_: &str| {
                            failed.set(true);
                            loaded.set(true);
                        }),
                )
                .child(dynamic(move || {
                    if loaded.get() && !failed.get() {
                        return view().into_element();
                    }
                    view()
                        .p(zpx(12.0))
                        .child(
                            text(if failed.get() {
                                t!("native.files.imageFailed")
                            } else {
                                t!("native.files.imageLoading")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(theme.resolved().ui.muted_foreground)),
                        )
                        .into_element()
                })),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(zpx(4.0))
                .p(zpx(4.0))
                .border_t_1()
                .border_color(move || rgba_to_hsla(theme.resolved().ui.border))
                .child(dynamic(move || {
                    let resolved = theme.resolved();
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(4.0))
                        // 图片尺寸（GPUI 缩放条左侧的 `image-dims`）：解码完成才有固有尺寸。
                        .when(loaded.get() && !failed.get(), |bar| {
                            let g = geometry.get();
                            bar.child(
                                text(format!(
                                    "{} × {}",
                                    g.natural.0.round(),
                                    g.natural.1.round()
                                ))
                                .test_id("image-dims")
                                .tooltip(themed_tooltip(
                                    text(t!("files.imageDims")),
                                    None,
                                    &resolved,
                                ))
                                .text_xs()
                                .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                            )
                            .child(
                                view()
                                    .w(zpx(1.0))
                                    .h(zpx(14.0))
                                    .bg(rgba_to_hsla(resolved.ui.border)),
                            )
                        })
                        .child(icon_button(
                            "image-zoom-out",
                            t!("native.files.zoomOut"),
                            Glyph::Minus,
                            false,
                            &resolved,
                            move || {
                                geometry.update(|g| g.zoom_to(Some(g.scale() / 1.25), g.center()))
                            },
                        ))
                        .child(
                            text(format!("{}%", (geometry.get().scale() * 100.0).round()))
                                .text_xs()
                                .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                        )
                        .child(icon_button(
                            "image-zoom-in",
                            t!("native.files.zoomIn"),
                            Glyph::Plus,
                            false,
                            &resolved,
                            move || {
                                geometry.update(|g| g.zoom_to(Some(g.scale() * 1.25), g.center()))
                            },
                        ))
                        .child(zoom_button(
                            "image-zoom-actual",
                            t!("native.files.zoomActual"),
                            geometry.get().zoom == Some(1.0),
                            &resolved,
                            move || geometry.update(|g| g.zoom_to(Some(1.0), g.center())),
                        ))
                        .child(zoom_button(
                            "image-zoom-fit",
                            t!("native.files.zoomFit"),
                            geometry.get().zoom.is_none(),
                            &resolved,
                            move || geometry.update(|g| g.zoom_to(None, g.center())),
                        ))
                })),
        )
}

fn zoom_button(
    id: &str,
    label: String,
    active: bool,
    theme: &falcon_theme::ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .test_id(id)
        .px(zpx(8.0))
        .py(zpx(4.0))
        .rounded(zpx(6.0))
        .cursor_pointer()
        .tooltip(themed_tooltip(text(label.clone()), None, theme))
        .bg(rgb_to_hsla(if active {
            theme.ui.secondary
        } else {
            theme.ui.background
        }))
        .on_press(action)
        .child(
            text(label)
                .text_xs()
                .text_color(rgb_to_hsla(theme.ui.foreground)),
        )
}

#[derive(Clone, PartialEq, Props)]
pub struct MarkdownPreviewProps {
    pub source: String,
    pub project_id: String,
    pub path: String,
    pub raw_url: Option<String>,
}

#[component]
pub fn MarkdownPreview(props: MarkdownPreviewProps) -> impl IntoElement {
    let store = use_workspace();
    let document = markdown_document(&props.source, props.raw_url.as_deref());
    scroll().flex_1().min_h_0().child(
        nsh_base::text_view()
            .document(document)
            .selectable(true)
            .p(zpx(20.0))
            .on_link_press(move |e: &nsh_base::text_view::TextViewLinkPressEvent| {
                if let Some(url) = external_href(&e.url) {
                    drop(nsh_core::element::open_url(url));
                } else if relative_href(&e.url) {
                    if let Some(path) = resolve_file_link(&props.path, &e.url) {
                        store.open_file_pane(&props.project_id, &path);
                    }
                }
            }),
    )
}

fn markdown_document(source: &str, raw: Option<&str>) -> Document {
    let mut document = Parser::new().markdown(source);
    rewrite_blocks(&mut document.blocks, raw);
    for note in &mut document.footnotes {
        rewrite_blocks(&mut note.children, raw);
    }
    document
}

fn rewrite_blocks(blocks: &mut [Block], raw: Option<&str>) {
    for block in blocks {
        match block {
            Block::Paragraph { content } | Block::Heading { content, .. } => {
                rewrite_inline(content, raw)
            }
            Block::Blockquote { children, .. } => rewrite_blocks(children, raw),
            Block::List { items, .. } => {
                for item in items {
                    rewrite_blocks(&mut item.children, raw);
                }
            }
            Block::Table(table) => {
                for row in std::iter::once(&mut table.head).chain(&mut table.rows) {
                    for cell in row {
                        rewrite_inline(&mut cell.content, raw);
                    }
                }
            }
            Block::Details {
                summary, children, ..
            } => {
                rewrite_inline(summary, raw);
                rewrite_blocks(children, raw);
            }
            Block::Custom(node) => {
                rewrite_inline(&mut node.content, raw);
                rewrite_blocks(&mut node.children, raw);
            }
            _ => {}
        }
    }
}

fn rewrite_inline(content: &mut [Inline], raw: Option<&str>) {
    for inline in content {
        match inline {
            Inline::Image(image) => {
                if let Some(url) = markdown_image_url(raw, &image.src) {
                    image.src = url;
                } else {
                    *inline = Inline::Link {
                        url: image.src.clone(),
                        title: image.title.clone(),
                        content: vec![Inline::text(if image.alt.is_empty() {
                            &image.src
                        } else {
                            &image.alt
                        })],
                    };
                }
            }
            Inline::Strong { content }
            | Inline::Emphasis { content }
            | Inline::Strikethrough { content }
            | Inline::Underline { content }
            | Inline::Highlight { content, .. }
            | Inline::Superscript { content }
            | Inline::Small { content }
            | Inline::Subscript { content }
            | Inline::Link { content, .. } => rewrite_inline(content, raw),
            Inline::Custom(node) => {
                rewrite_inline(&mut node.content, raw);
                rewrite_blocks(&mut node.children, raw);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::element::Root;
    use nsh_core::{EventKind, EventPayload, PayloadValue};
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    #[component]
    fn ImageFixture() -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        view()
            .size_full()
            .child(image_preview().url("https://example.test/image.png"))
    }

    fn node(app: &Headless, id: &str) -> nsh_testing::LayoutNode {
        app.screen()
            .layout_nodes()
            .into_iter()
            .find(|n| n.test_id.as_deref() == Some(id))
            .unwrap()
    }

    #[test]
    fn image_events_zoom_at_pointer_pan_and_toggle_without_remounting_headless() {
        let mut config =
            EngineConfig::new(Root::of::<ImageFixture>().translations(crate::i18n::translations()));
        config.viewport = Size::new(Points(848.0), Points(680.0));
        config.ui_sets = vec![nsh_base::registry(), crate::registry()];
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        let image_id = node(&app, "file-image").id;
        app.engine().control().dispatch_event(
            image_id,
            EventKind::Load,
            EventPayload::Map(vec![
                ("width", PayloadValue::Number(1600.0)),
                ("height", PayloadValue::Number(1200.0)),
            ]),
        );
        app.wait_until("image loaded", Duration::from_secs(2), |screen| {
            screen
                .layout_nodes()
                .iter()
                .find(|n| n.id == image_id)
                .unwrap()
                .bounds
                .size
                .width
                .0
                > 100.0
        })
        .unwrap();
        app.press("id:image-zoom-actual").unwrap();
        app.wait_for("text:100%").unwrap();

        let viewport = node(&app, "file-image-viewport");
        let at = Point::new(Points(230.0), Points(170.0));
        for command in [false, true] {
            let before = node(&app, "file-image").bounds;
            let mut modifiers = nsh_core::keymap::Modifiers::default();
            modifiers.ctrl = !command;
            modifiers.cmd = command;
            app.engine().control().dispatch_event(
                viewport.id,
                EventKind::Wheel,
                EventPayload::Wheel(WheelEvent::new(
                    at,
                    at,
                    WheelDelta::Points(Point::new(Points(0.0), Points(-160.0))),
                    modifiers,
                )),
            );
            app.wait_until("wheel zoom", Duration::from_secs(2), |screen| {
                screen
                    .layout_nodes()
                    .iter()
                    .find(|n| n.id == image_id)
                    .unwrap()
                    .bounds
                    .size
                    .width
                    .0
                    > before.size.width.0
            })
            .unwrap();
            let after = node(&app, "file-image").bounds;
            let pixel = |b: Bounds<Points>| {
                (
                    (at.x.0 - b.origin.x.0) / b.size.width.0,
                    (at.y.0 - b.origin.y.0) / b.size.height.0,
                )
            };
            // Headless 和原生布局都会把 frame 取整；纯几何测试另以 1e-8 验证锚点。
            assert!(
                (pixel(before).0 - pixel(after).0).abs() * after.size.width.0 < 2.0,
                "command={command} before={before:?} after={after:?}"
            );
            assert!(
                (pixel(before).1 - pixel(after).1).abs() * after.size.height.0 < 2.0,
                "command={command} before={before:?} after={after:?}"
            );
        }

        let mut press = PressEvent::default();
        press.position = at;
        press.clicks = 2;
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::PressDown,
            EventPayload::Press(press),
        );
        app.wait_until("fit", Duration::from_secs(2), |screen| {
            screen
                .layout_nodes()
                .iter()
                .find(|n| n.id == image_id)
                .unwrap()
                .bounds
                .size
                .width
                .0
                < 848.0
        })
        .unwrap();
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::PressDown,
            EventPayload::Press(press),
        );
        app.wait_for("text:100%").unwrap();

        press.clicks = 1;
        let before = node(&app, "file-image").bounds;
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::PressDown,
            EventPayload::Press(press),
        );
        let to = Point::new(Points(210.0), Points(150.0));
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::MouseMove,
            EventPayload::Mouse(MouseEvent::new(
                to,
                to,
                Default::default(),
                Some(MouseButton::Left),
            )),
        );
        app.wait_until("pan", Duration::from_secs(2), |screen| {
            screen
                .layout_nodes()
                .iter()
                .find(|n| n.id == image_id)
                .unwrap()
                .bounds
                .origin
                .x
                .0
                < before.origin.x.0
        })
        .unwrap();
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::PinchStart,
            EventPayload::Map(vec![("scale", PayloadValue::Number(1.0))]),
        );
        app.engine().control().dispatch_event(
            viewport.id,
            EventKind::PinchEnd,
            EventPayload::Map(vec![("scale", PayloadValue::Number(2.0))]),
        );
        app.wait_for("text:200%").unwrap();
        assert_eq!(node(&app, "file-image").id, image_id);
    }

    #[test]
    fn markdown_rewrites_images_inside_nested_formatting_but_preserves_code() {
        let raw = Some("https://example.test/api/projects/p/raw/token/docs/readme.md");
        let doc = markdown_document(
            "**![nested](./img%20one.png)**\n\n```md\n![code](./keep.png)\n```",
            raw,
        );
        let Block::Paragraph { content } = &doc.blocks[0] else {
            panic!("paragraph");
        };
        let Inline::Strong { content } = &content[0] else {
            panic!("strong");
        };
        let Inline::Image(image) = &content[0] else {
            panic!("image");
        };
        assert_eq!(
            image.src,
            "https://example.test/api/projects/p/raw/token/docs/img%20one.png"
        );
        let Block::CodeBlock(code) = &doc.blocks[1] else {
            panic!("code block");
        };
        assert_eq!(code.code, "![code](./keep.png)");
    }

    #[test]
    fn markdown_keeps_workspace_links_and_demotes_blocked_images_to_alt_text() {
        let doc = markdown_document(
            "[next](../next.md#title) ![blocked](data:image/png;base64,abc)",
            None,
        );
        let Block::Paragraph { content } = &doc.blocks[0] else {
            panic!("paragraph");
        };
        assert!(
            content
                .iter()
                .any(|node| matches!(node, Inline::Link { url, .. } if url == "../next.md#title"))
        );
        assert!(!content.iter().any(|node| matches!(node, Inline::Image(_))));
        assert!(content.iter().any(|node| matches!(node, Inline::Link { content, .. } if content == &vec![Inline::text("blocked")])));
    }
}
