//! Zellij 安装通道 `/ws/install/:projectId`：照旧 React 版的 `ZellijInstallModal.tsx` 与服务端
//! `ws.ts` 对齐。
//!
//! 每次连上来服务端都当作用户显式发起的安装（首次或重试），推若干 `stage` 后以
//! `done` / `failed` 收尾并**自己关掉连接**。所以这条通道不重连：断了就是断了，
//! 要重试就再 [`InstallSocket::open`] 一次。唯一的例外是一开始就撞上 4401——那时
//! 安装还没开始，自动重登后重连一次是安全的。

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use falcon_proto::{InstallClientMessage, InstallServerMessage, WS_CLOSE_UNAUTHORIZED};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{Stream, StreamExt};

use super::{ConnectError, WsConn, WsMsg};
use crate::api::seg;
use crate::client::{FalconClient, Relogin};
use crate::runtime;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 安装通道的事件。以 [`InstallEvent::Closed`] 收尾，之后流结束。
#[derive(Debug, Clone, PartialEq)]
pub enum InstallEvent {
    /// 服务端的消息：若干 `Stage`，最后 `Done` 或 `Failed`（本版本认不出的消息不推）
    Message(InstallServerMessage),
    /// 通道本身出了问题（连不上后端、中途断开），跟远端装没装成无关。界面（falcon-ui 的
    /// 安装对话框）把它当 probe-failed 显示并给重试按钮。服务端已经报过 done / failed
    /// 之后的断开不算。
    ChannelError(String),
    /// 未认证，自动重登没设密码或被拒：该弹登录框了
    Unauthorized,
    /// 通道关了。一定是最后一个事件
    Closed,
}

/// 一次 Zellij 安装的进度流。是 `futures::Stream<Item = InstallEvent>`，与执行器无关。
///
/// 丢掉即取消（关掉 socket 服务端就会 abort 这次安装）。
pub struct InstallSocket {
    events: UnboundedReceiver<InstallEvent>,
    cancel: tokio::sync::mpsc::UnboundedSender<()>,
}

impl InstallSocket {
    /// 连上并开始安装（连接在网络运行时上进行，这里立刻返回）。
    pub fn open(client: &FalconClient, project_id: &str) -> Self {
        let (tx, rx) = unbounded();
        let (cancel_tx, cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let path = format!("/ws/install/{}", seg(project_id));
        runtime::spawn(drive(client.clone(), path, tx, cancel_rx));
        InstallSocket { events: rx, cancel: cancel_tx }
    }

    /// 取消这次安装：发 `{"type":"cancel"}` 再关连接。只影响本次，不写入拒绝状态——
    /// 点的是进度条上的取消，不是撤销授权。取消之后只会再收到一个 `Closed`。
    pub fn cancel(&self) {
        let _ = self.cancel.send(());
    }
}

impl Stream for InstallSocket {
    type Item = InstallEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<InstallEvent>> {
        self.events.poll_next_unpin(cx)
    }
}

enum Outcome {
    /// 正常收尾，或被取消
    Finished,
    Unauthorized,
    Broken(String),
}

async fn drive(
    client: FalconClient,
    path: String,
    tx: UnboundedSender<InstallEvent>,
    mut cancel: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    let emit = |ev| {
        let _ = tx.unbounded_send(ev);
    };
    let mut relogged = false;
    loop {
        let token_used = client.token();
        let connected = tokio::select! {
            r = super::connect(&client.inner, &path, CONNECT_TIMEOUT) => r,
            // 取消（或 InstallSocket 被丢掉）时还没连上：什么都不用发
            _ = cancel.recv() => break,
        };
        let outcome = match connected {
            Ok(ws) => serve(ws, &emit, &mut cancel).await,
            Err(ConnectError::Unauthorized) => Outcome::Unauthorized,
            Err(ConnectError::Other(m)) => Outcome::Broken(m),
        };
        match outcome {
            Outcome::Finished => break,
            Outcome::Broken(m) => {
                emit(InstallEvent::ChannelError(m));
                break;
            }
            Outcome::Unauthorized => {
                // 服务端在推任何进度之前就 4401 了，安装还没开始：重登后重连一次是安全的
                if !relogged {
                    relogged = true;
                    match client.inner.relogin(token_used).await {
                        Relogin::Done => continue,
                        Relogin::Failed(e) => {
                            emit(InstallEvent::ChannelError(e.message));
                            break;
                        }
                        Relogin::NoPassword | Relogin::Rejected => {}
                    }
                } else {
                    client.inner.auth.login_required(&token_used);
                }
                emit(InstallEvent::Unauthorized);
                break;
            }
        }
    }
    emit(InstallEvent::Closed);
}

async fn serve(
    mut ws: WsConn,
    emit: &impl Fn(InstallEvent),
    cancel: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> Outcome {
    // 服务端报过结果之后再来的断开只是通道收尾，不能拿它盖掉真正的结果
    let mut settled = false;
    loop {
        tokio::select! {
            msg = ws.recv() => match msg {
                Some(Ok(WsMsg::Text(text))) => {
                    let Ok(m) = serde_json::from_str::<InstallServerMessage>(&text) else { continue };
                    match m {
                        InstallServerMessage::Unknown => continue,
                        InstallServerMessage::Done | InstallServerMessage::Failed { .. } => settled = true,
                        InstallServerMessage::Stage { .. } => {}
                    }
                    emit(InstallEvent::Message(m));
                }
                Some(Ok(WsMsg::Close(code))) => {
                    if !settled && code == Some(WS_CLOSE_UNAUTHORIZED) {
                        return Outcome::Unauthorized;
                    }
                    return if settled { Outcome::Finished } else { Outcome::Broken("安装通道被关闭".to_owned()) };
                }
                Some(Ok(_)) => {}
                Some(Err(e)) if !settled => return Outcome::Broken(e),
                Some(Err(_)) | None => {
                    return if settled { Outcome::Finished } else { Outcome::Broken("安装通道被关闭".to_owned()) };
                }
            },
            // 显式取消与 InstallSocket 被丢掉走同一条路：告诉服务端取消，再关连接
            _ = cancel.recv() => {
                if let Ok(json) = serde_json::to_string(&InstallClientMessage::Cancel) {
                    let _ = ws.send_text(json).await;
                }
                ws.close().await;
                return Outcome::Finished;
            }
        }
    }
}
