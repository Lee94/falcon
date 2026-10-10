//! 通知层：自己挂一个 Root 插件画 gpui-component 的 `NotificationList`，推迟到所有对话框之后画。
//!
//! 通知必须压在对话框上面（设置里点「检测连接」的结果就是在对话框开着时弹的）。gpui-component
//! 的对话框是 `deferred(..).with_priority(10 + 层号)` 画的，它自带的通知层没推迟——0.6 时由我们
//! 自己调 `render_notification_layer` 再套一层 deferred 解决；0.7 起抽屉 / 对话框 / 通知三层改由
//! 它的 Root 插件自动挂，那份通知列表在 crate 私有的 `WindowState` 里拿不到、套不了，实测又被
//! 设置对话框整个盖住。所以另挂一份，priority 100 压在任何一层对话框上面。
//!
//! **所有通知一律走这里的 [`ToastExt`]**，别再用 `WindowExt::push_notification`：那份进的是插件
//! 自带的列表，对话框开着时看不见。

use std::collections::HashMap;

use gpui_kit::base::{Root, RootPlugin};
use gpui_kit::component::notification::{Notification, NotificationList};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EntityId, SharedString, WeakEntity, Window, deferred, div};

pub fn init(cx: &mut App) {
    // 插件按注册顺序叠放，这里晚于 gpui_kit::init 里组件库那个；真正决定上下的是下面的 priority
    Root::register_plugin::<Toasts>(cx, Toasts::new);
}

pub struct Toasts {
    list: Entity<NotificationList>,
    /// 带 key 的通知（传输进度）→ 当前那条的实体。`NotificationList` 按 id 关通知的接口是 crate
    /// 私有的，只能拿着实体调 `Notification::dismiss`；同 key 重推会换成新实体，这里跟着换
    keyed: HashMap<SharedString, WeakEntity<Notification>>,
}

impl Toasts {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { list: cx.new(|cx| NotificationList::new(window, cx)), keyed: HashMap::new() }
    }

    fn entity(window: &Window, cx: &App) -> Option<Entity<Self>> {
        window.root::<Root>()??.read(cx).plugin::<Self>()
    }
}

impl RootPlugin for Toasts {}

impl Render for Toasts {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        deferred(div().absolute().inset_0().child(self.list.clone())).with_priority(100)
    }
}

pub trait ToastExt {
    /// 弹一条通知（同 id 的会原地替换，见 `Notification::id` / `id1`）
    fn push_toast(&mut self, note: impl Into<Notification>, cx: &mut App);
    /// 弹一条以后要按 `key` 撤掉的通知；`note` 自己要带上同一个 `id1`，重推才是原地替换
    fn push_keyed_toast(&mut self, key: SharedString, note: impl Into<Notification>, cx: &mut App);
    /// 撤掉 [`push_keyed_toast`](Self::push_keyed_toast) 弹的那条（已经没了就什么都不做）
    fn dismiss_toast(&mut self, key: &SharedString, cx: &mut App);
}

impl ToastExt for Window {
    fn push_toast(&mut self, note: impl Into<Notification>, cx: &mut App) {
        push(self, None, note.into(), cx);
    }

    fn push_keyed_toast(&mut self, key: SharedString, note: impl Into<Notification>, cx: &mut App) {
        push(self, Some(key), note.into(), cx);
    }

    fn dismiss_toast(&mut self, key: &SharedString, cx: &mut App) {
        let Some(toasts) = Toasts::entity(self, cx) else { return };
        let Some(note) = toasts.update(cx, |t, _| t.keyed.remove(key)).and_then(|n| n.upgrade()) else {
            return;
        };
        note.update(cx, |n, cx| n.dismiss(self, cx));
    }
}

fn push(window: &mut Window, key: Option<SharedString>, note: Notification, cx: &mut App) {
    // 窗口还没挂上 Root（不会发生在正常路径上）就丢掉，与组件库 expect 崩掉相比宁可少一条通知
    let Some(toasts) = Toasts::entity(window, cx) else { return };
    let list = toasts.read(cx).list.clone();
    let before: Vec<EntityId> = list.read(cx).notifications().iter().map(|n| n.entity_id()).collect();
    list.update(cx, |l, cx| l.push(note, window, cx));
    if let Some(key) = key {
        let pushed = list.read(cx).notifications().into_iter().find(|n| !before.contains(&n.entity_id()));
        toasts.update(cx, |t, _| {
            // 自己关掉 / 超时消失的那些实体已经释放，顺手清掉
            t.keyed.retain(|_, n| n.upgrade().is_some());
            match pushed {
                Some(n) => {
                    t.keyed.insert(key, n.downgrade());
                }
                None => {
                    t.keyed.remove(&key);
                }
            }
        });
    }
}
