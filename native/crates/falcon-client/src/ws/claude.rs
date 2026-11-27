use falcon_proto::claude::{ClaudeEvent, ClaudeInput, UPLOAD_CHUNK_CHARS};
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
        // 附件按段排队（见 upload），容量要放得下一份最大附件的全部分段
        let (input, mut commands) = channel::<ClaudeInput>(UPLOAD_QUEUE);
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
                                // 节点可能比客户端新：认不出的事件跳过，不把整条对话断掉
                                let Ok(event) = serde_json::from_str::<ClaudeEvent>(&s) else { continue };
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

    /// 把一份附件分段传给宿主机（WS 单帧上限 1 MiB），之后的 Prompt 用同一个 id 引用它。
    /// 队列放不下（附件超过上限）返回 false
    pub fn upload(&self, id: &str, bytes: &[u8]) -> bool {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let chunks: Vec<&str> = data
            .as_bytes()
            .chunks(UPLOAD_CHUNK_CHARS)
            // base64 全是 ASCII，按字节切不会切坏字符
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect();
        if chunks.len() > self.0.capacity() {
            return false;
        }
        chunks
            .into_iter()
            .all(|data| self.send(ClaudeInput::Upload { id: id.to_owned(), data: data.to_owned() }))
    }
}

/// 输入队列的容量：一份 24 MiB 的附件 base64 后约 64 段
const UPLOAD_QUEUE: usize = 96;
