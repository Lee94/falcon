//! 实时输出流的终端查询过滤器（设计文档 3.5 节）。
//!
//! 原因：holder 的 VT 进程已经回答了程序发出的全部终端查询（DA1/DA2/DA3、DSR 5n/6n/?6n、
//! XTVERSION、DECRQM、窗口尺寸 14t/16t/18t、OSC 4/10/11/12/52 查询、DECRQSS、XTGETTCAP 等）。
//! 如果这些查询原样流到 `hc attach` 下的真实宿主终端，宿主终端会再答一遍，写进 PTY 变成乱码输入。
//! 因此在网关把实时 `Output` 转发给 viewer 之前，用流式状态机剥掉这些查询序列。

#[derive(Debug, Default)]
pub struct QueryFilter {
    state: State,
    seq: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Dcs,
    DcsEsc,
}

const MAX_SEQ: usize = 4096;

impl QueryFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                        self.seq.clear();
                        self.seq.push(0x1b);
                    } else {
                        out.push(b);
                    }
                }
                State::Esc => {
                    self.seq.push(b);
                    match b {
                        b'[' => self.state = State::Csi,
                        b']' => self.state = State::Osc,
                        b'P' => self.state = State::Dcs,
                        // DECID (ESC Z) 是老式 DA 查询，剥掉
                        b'Z' => {
                            self.seq.clear();
                            self.state = State::Ground;
                        }
                        0x1b => {
                            // 上一个孤立的 ESC 放行，留在 Esc 态处理新的 ESC
                            out.push(0x1b);
                            self.seq.clear();
                            self.seq.push(0x1b);
                        }
                        _ => {
                            out.extend_from_slice(&self.seq);
                            self.seq.clear();
                            self.state = State::Ground;
                        }
                    }
                }
                State::Csi => {
                    if b == 0x1b {
                        // 被新 ESC 打断：把半截放行，切回 Esc
                        out.extend_from_slice(&self.seq);
                        self.seq.clear();
                        self.seq.push(0x1b);
                        self.state = State::Esc;
                        continue;
                    }
                    self.seq.push(b);
                    if (0x40..=0x7e).contains(&b) {
                        if !is_csi_query(&self.seq[2..]) {
                            out.extend_from_slice(&self.seq);
                        }
                        self.seq.clear();
                        self.state = State::Ground;
                    } else if self.seq.len() > MAX_SEQ {
                        out.extend_from_slice(&self.seq);
                        self.seq.clear();
                        self.state = State::Ground;
                    }
                }
                State::Osc => {
                    self.seq.push(b);
                    if b == 0x07 {
                        if !is_osc_query(&self.seq[2..self.seq.len() - 1]) {
                            out.extend_from_slice(&self.seq);
                        }
                        self.seq.clear();
                        self.state = State::Ground;
                    } else if b == 0x1b {
                        self.state = State::OscEsc;
                    } else if self.seq.len() > MAX_SEQ {
                        out.extend_from_slice(&self.seq);
                        self.seq.clear();
                        self.state = State::Ground;
                    }
                }
                State::OscEsc => {
                    self.seq.push(b);
                    if b == b'\\' {
                        if !is_osc_query(&self.seq[2..self.seq.len() - 2]) {
                            out.extend_from_slice(&self.seq);
                        }
                        self.seq.clear();
                        self.state = State::Ground;
                    } else {
                        self.state = State::Osc;
                    }
                }
                State::Dcs => {
                    self.seq.push(b);
                    if b == 0x1b {
                        self.state = State::DcsEsc;
                    } else if self.seq.len() > MAX_SEQ {
                        out.extend_from_slice(&self.seq);
                        self.seq.clear();
                        self.state = State::Ground;
                    }
                }
                State::DcsEsc => {
                    self.seq.push(b);
                    if b == b'\\' {
                        if !is_dcs_query(&self.seq[2..self.seq.len() - 2]) {
                            out.extend_from_slice(&self.seq);
                        }
                        self.seq.clear();
                        self.state = State::Ground;
                    } else {
                        self.state = State::Dcs;
                    }
                }
            }
        }
        out
    }
}

/// 判断 `CSI <body_and_final>` 是否是终端查询
fn is_csi_query(body: &[u8]) -> bool {
    let Some((&fin, params)) = body.split_last() else {
        return false;
    };
    match fin {
        // DA1 / DA2 / DA3: CSI c, CSI 0 c, CSI > c, CSI > 0 c, CSI = c
        b'c' => params.is_empty() || params == b"0" || params.starts_with(b">") || params.starts_with(b"="),
        // DSR / CPR / Color scheme query: CSI 5 n, CSI 6 n, CSI ? ... n
        b'n' => params == b"5" || params == b"6" || params.starts_with(b"?"),
        // XTVERSION: CSI > q, CSI > 0 q
        b'q' => params.starts_with(b">"),
        // Kitty keyboard query: CSI ? u
        b'u' => params == b"?",
        // DECRQM: CSI ? Pm $ p 或 CSI Pm $ p
        b'p' => params.ends_with(b"$"),
        // XTWINOPS 尺寸查询: CSI 14 t, CSI 16 t, CSI 18 t, CSI 19 t
        b't' => matches!(params, b"14" | b"16" | b"18" | b"19" | b"14;2"),
        _ => false,
    }
}

/// 判断 `OSC <payload>` 是否是颜色/剪贴板/状态查询（载荷含 `;?`）
fn is_osc_query(payload: &[u8]) -> bool {
    let Some(semi) = payload.iter().position(|&b| b == b';') else {
        return false;
    };
    let code = &payload[..semi];
    let rest = &payload[semi + 1..];
    match code {
        b"4" | b"10" | b"11" | b"12" | b"17" | b"19" => rest == b"?" || rest.ends_with(b";?"),
        b"52" => rest.ends_with(b";?"),
        b"7501" => rest == b"?",
        _ => false,
    }
}

/// 判断 `DCS <payload>` 是否是 DECRQSS (`$q`) 或 XTGETTCAP (`+q`)
fn is_dcs_query(payload: &[u8]) -> bool {
    payload.starts_with(b"$q") || payload.starts_with(b"+q")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_queries_and_keeps_normal_output() {
        let mut qf = QueryFilter::new();
        let input = b"hello\x1b[c\x1b[>0c\x1b[6n\x1b[?996n\x1b[>q\x1b[?u\x1b[?2004$p\x1b]11;?\x1b\\\x1b]10;?\x07\x1bP$qm\x1b\\\x1b[32mworld\x1b[0m\x1b]2;title\x07";
        let mut out = Vec::new();
        // 逐字节喂入，验证跨 chunk 状态机
        for b in input {
            out.extend(qf.filter(&[*b]));
        }
        assert_eq!(out, b"hello\x1b[32mworld\x1b[0m\x1b]2;title\x07");
    }
}
