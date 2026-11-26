use falcon_proto::claude::{ClaudeEvent, ClaudeInput};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use std::time::Duration;
use tokio::sync::mpsc::{Sender, channel};

use super::{ConnectError, WsMsg};
use crate::{FalconClient, api::seg, runtime};

/// 最后一个输入句柄随窗口释放，驱动随即关闭连接，节点负责结束进程。
pub struct ClaudeSocket(Sender<ClaudeInput>);

impl ClaudeSocket {
    pub fn open(client: &FalconClient, project: &str) -> (Self, UnboundedReceiver<ClaudeEvent>) {
        let path = format!("/ws/projects/{}/claude", seg(project));
        let client = client.clone();
        let (input, mut commands) = channel::<ClaudeInput>(16);
        let (events, rx) = unbounded();
        runtime::spawn(async move {
            let result: Result<(), String> = async {
                let mut ws = super::connect(&client.inner, &path, Duration::from_secs(20)).await.map_err(|e| match e {
                    ConnectError::Unauthorized => "登录已失效".into(),
                    ConnectError::Other(s) => s,
                })?;
                loop {
                    tokio::select! {
                        command = commands.recv() => {
                            let Some(cmd) = command else { ws.close().await; return Ok(()); };
                            ws.send_text(serde_json::to_string(&cmd).map_err(|e| e.to_string())?).await?;
                        }
                        message = ws.recv() => match message.transpose()? {
                            Some(WsMsg::Text(s)) => {
                                let event = serde_json::from_str(&s).map_err(|e| e.to_string())?;
                                let closed = matches!(event, ClaudeEvent::Closed { .. });
                                if events.unbounded_send(event).is_err() || closed { return Ok(()); }
                            }
                            None | Some(WsMsg::Close(_)) => return Err("Claude 对话连接已关闭".into()),
                            _ => {}
                        }
                    }
                }
            }.await;
            if let Err(reason) = result {
                let _ = events.unbounded_send(ClaudeEvent::Closed { reason });
            }
        });
        (Self(input), rx)
    }

    pub fn send(&self, input: ClaudeInput) -> bool {
        self.0.try_send(input).is_ok()
    }
}
