//! `terminal.attach` 的单通道桥接器（设计文档 3.5 节）：
//! - 每个 `terminal.attach` 请求在 Framed 连接上分配一个独立的 `channel >= 1`；
//! - 为该 viewer 打开一条直连目标 holder 的 `UnixStream`（`Role::Attach`）：
//!   - 大小仲裁天然复用 holder 的"最近活跃连接"规则（`falcon-server` 与 `hc` viewer 谁最后打字/resize 听谁的）；
//!   - 先发 `Sync { req: 1 }`，收到 `Synced` 后拼装 `first_paint(...)` 送入信用窗口；
//!   - 后续实时 `Output` 经 `QueryFilter` 剥除终端查询后送入信用窗口，并顺带广播给 `output_watchers`（供 `terminal.wait --output` 使用）；
//!   - 若 viewer 长期不读导致积压超过 4 MiB（`ChannelCredit::dropped == true`），待其归还 `CREDIT` 时重新向 holder 请求一次 `Sync`，重绘完整首帧；
//!   - 会话退出或 holder 失联时，在通道上发送 `CLOSE` 帧（JSON `{"reason": ..., "exit_code": ..., "signal": ...}`）。

#![cfg(unix)]

use std::sync::{Arc, Mutex};

use falcon_proto::program_status::ProgramStatusRecord;
use falcon_proto::sessiond::{
    ClientMsg, Event, ExitStatus, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Resize, Role, kind as hkind, negotiate,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::hc::mirror::SharedGateway;
use crate::hc::paint::first_paint;
use crate::hc::queryfilter::QueryFilter;
use crate::hc::wire::{ChannelCredit, MAX_FRAME, decode_credit, decode_resize, encode_raw, kind as wkind};

/// 客户端在 attach 通道上发来的帧
pub enum ViewerFrame {
    Data(Vec<u8>),
    Credit(u32),
    Resize(u16, u16),
    Focus(bool),
    Facts(Vec<u8>),
    Close,
}

pub struct AttachOptions {
    pub sid: String,
    pub endpoint: String,
    pub channel: u32,
    pub observe: bool,
    pub history: Option<u32>,
    pub cols: u16,
    pub rows: u16,
    pub resumed: bool,
}

/// 启动一条 attach 通道桥接任务，返回喂给它的 `mpsc::UnboundedSender<ViewerFrame>`
pub fn spawn_attach_session(
    gw: SharedGateway,
    opts: AttachOptions,
    frame_out_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> mpsc::UnboundedSender<ViewerFrame> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_attach(gw, opts, in_rx, frame_out_tx));
    in_tx
}

async fn run_attach(
    gw: SharedGateway,
    opts: AttachOptions,
    mut in_rx: mpsc::UnboundedReceiver<ViewerFrame>,
    frame_out_tx: mpsc::UnboundedSender<Vec<u8>>,
) {
    let channel = opts.channel;
    let stream = match UnixStream::connect(&opts.endpoint).await {
        Ok(s) => s,
        Err(_) => {
            let close_payload = serde_json::to_vec(&json!({
                "reason": "holder_lost",
                "exit_code": null,
                "signal": null,
            }))
            .unwrap_or_default();
            let _ = frame_out_tx.send(encode_raw(channel, wkind::CLOSE, &close_payload));
            return;
        }
    };
    let (mut h_read, mut h_write) = stream.into_split();

    let hello = ClientMsg::Hello(Hello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        role: Role::Attach,
        client: "hc-attach".into(),
    });
    if h_write.write_all(&hello.encode()).await.is_err() {
        return;
    }
    // resumed=true 或 observe=true 时不在握手阶段抢尺寸，等用户实际按键或发 RESIZE 再激活尺寸
    if !opts.observe && !opts.resumed && opts.cols > 0 && opts.rows > 0 {
        let rmsg = ClientMsg::Resize(Resize {
            cols: opts.cols,
            rows: opts.rows,
        });
        if h_write.write_all(&rmsg.encode()).await.is_err() {
            return;
        }
    }

    let mut next_sync_req = 1u32;
    let sync_msg = ClientMsg::Sync { req: next_sync_req };
    if h_write.write_all(&sync_msg.encode()).await.is_err() {
        return;
    }

    let credit = Arc::new(Mutex::new(ChannelCredit::new(channel)));
    let mut qfilter = QueryFilter::new();
    let mut cur_title = String::new();
    let mut cur_records: Vec<ProgramStatusRecord> = Vec::new();
    let mut cur_rows = opts.rows.max(24);
    let mut synced_once = false;
    let mut sync_in_flight = true;
    let mut exit_status: Option<ExitStatus> = None;

    let flush_credit = |credit: &Arc<Mutex<ChannelCredit>>, tx: &mpsc::UnboundedSender<Vec<u8>>| {
        let frames = credit.lock().unwrap_or_else(|p| p.into_inner()).drain_frames();
        for f in frames {
            let _ = tx.send(f);
        }
    };

    loop {
        tokio::select! {
            vf = in_rx.recv() => {
                let Some(vf) = vf else {
                    break;
                };
                match vf {
                    ViewerFrame::Data(bytes) => {
                        if !opts.observe && !bytes.is_empty() {
                            let msg = ClientMsg::Input(bytes);
                            if h_write.write_all(&msg.encode()).await.is_err() {
                                break;
                            }
                        }
                    }
                    ViewerFrame::Credit(amount) => {
                        let need_repaint = {
                            let mut cc = credit.lock().unwrap_or_else(|p| p.into_inner());
                            cc.add_credit(amount);
                            cc.dropped && cc.credit > 0 && !sync_in_flight
                        };
                        if need_repaint {
                            next_sync_req = next_sync_req.wrapping_add(1);
                            sync_in_flight = true;
                            let msg = ClientMsg::Sync { req: next_sync_req };
                            if h_write.write_all(&msg.encode()).await.is_err() {
                                break;
                            }
                        } else {
                            flush_credit(&credit, &frame_out_tx);
                        }
                    }
                    ViewerFrame::Resize(cols, rows) => {
                        if !opts.observe && cols > 0 && rows > 0 {
                            cur_rows = rows;
                            let msg = ClientMsg::Resize(Resize { cols, rows });
                            if h_write.write_all(&msg.encode()).await.is_err() {
                                break;
                            }
                        }
                    }
                    ViewerFrame::Focus(_gained) => {
                        // 第一期忽略焦点上报（避免与 falcon 网页端焦点冲突）
                    }
                    ViewerFrame::Facts(_json) => {
                        // 第一期忽略 FACTS（颜色由 falcon 会话级 Appearance 决定）
                    }
                    ViewerFrame::Close => {
                        let _ = frame_out_tx.send(encode_raw(channel, wkind::CLOSE, b""));
                        return;
                    }
                }
            }
            hframe = read_holder_frame(&mut h_read) => {
                let Ok(Some((kind, payload))) = hframe else {
                    break;
                };
                let Ok(msg) = HolderMsg::decode(kind, payload) else {
                    continue;
                };
                match msg {
                    HolderMsg::Welcome(w) => {
                        if negotiate((PROTO_MIN, PROTO_MAX), (w.proto, w.proto)).is_none() {
                            break;
                        }
                        cur_rows = w.rows.max(1);
                    }
                    HolderMsg::Status { records } => {
                        cur_records = records;
                    }
                    HolderMsg::Synced { snapshot, continuation, .. } => {
                        sync_in_flight = false;
                        let hist = if synced_once { Some(0) } else { opts.history };
                        synced_once = true;
                        let paint = first_paint(
                            &snapshot,
                            &continuation,
                            &cur_title,
                            &cur_records,
                            cur_rows,
                            hist,
                        );
                        {
                            let mut cc = credit.lock().unwrap_or_else(|p| p.into_inner());
                            if cc.dropped {
                                cc.reset_with_first_paint(&paint);
                            } else {
                                cc.push_data(&paint);
                            }
                        }
                        flush_credit(&credit, &frame_out_tx);
                    }
                    HolderMsg::Output { bytes, .. } => {
                        // 广播给 `terminal.wait --output` 观察者
                        {
                            let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(watchers) = st.output_watchers.get_mut(&opts.sid) {
                                watchers.retain(|tx| tx.send(bytes.clone()).is_ok());
                            }
                        }
                        if !synced_once {
                            continue;
                        }
                        let clean = qfilter.filter(&bytes);
                        if clean.is_empty() {
                            continue;
                        }
                        {
                            let mut cc = credit.lock().unwrap_or_else(|p| p.into_inner());
                            cc.push_data(&clean);
                        }
                        flush_credit(&credit, &frame_out_tx);
                    }
                    HolderMsg::Event(ev) => match ev {
                        Event::Title { title } => {
                            cur_title = title;
                        }
                        Event::Size { rows, .. } => {
                            cur_rows = rows.max(1);
                        }
                        Event::VtDegraded | Event::VtRestarted { .. } | Event::Bell | Event::Prompt { .. } | Event::Unknown => {}
                    },
                    HolderMsg::Exit(status) => {
                        exit_status = Some(status);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // 把剩余能发的帧发完，再发 CLOSE 帧告知客户端原因
    flush_credit(&credit, &frame_out_tx);
    let close_json = match exit_status {
        Some(st) => {
            let reason = if st.signal.is_some() && st.code.is_none() {
                "killed"
            } else {
                "exited"
            };
            json!({
                "reason": reason,
                "exit_code": st.code,
                "signal": st.signal,
            })
        }
        None => json!({
            "reason": "holder_lost",
            "exit_code": null,
            "signal": null,
        }),
    };
    let payload = serde_json::to_vec(&close_json).unwrap_or_default();
    let _ = frame_out_tx.send(encode_raw(channel, wkind::CLOSE, &payload));
}

async fn read_holder_frame(
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut hdr = [0u8; 5];
    match reader.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let kind = hdr[0];
    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("holder 帧过长: {len}"),
        ));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut payload).await?;
    }
    // 忽略未知的未来帧类型前先交由上层 HolderMsg::decode 处理
    let _ = hkind::WELCOME;
    Ok(Some((kind, payload)))
}

/// 解析 Framed 连接上发给 `channel >= 1` 的帧
pub fn decode_viewer_frame(kind: u8, payload: Vec<u8>) -> Option<ViewerFrame> {
    match kind {
        wkind::DATA => Some(ViewerFrame::Data(payload)),
        wkind::CREDIT => decode_credit(&payload).map(ViewerFrame::Credit),
        wkind::RESIZE => decode_resize(&payload).map(|(c, r)| ViewerFrame::Resize(c, r)),
        wkind::FOCUS => Some(ViewerFrame::Focus(payload.first() == Some(&1))),
        wkind::FACTS => Some(ViewerFrame::Facts(payload)),
        wkind::CLOSE => Some(ViewerFrame::Close),
        _ => None,
    }
}
