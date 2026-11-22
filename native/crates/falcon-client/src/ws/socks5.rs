//! 经回环 SOCKS5 代理建立 TCP 连接（RFC 1928 + RFC 1929，ADR 0030 决定二）。
//!
//! 域名直接发 ATYP=0x03，交由 tsnet 代理做 MagicDNS 解析。

use falcon_tailnet::ProxyInfo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Host;

pub(crate) async fn connect(proxy: &ProxyInfo, host: &Host<&str>, port: u16) -> Result<TcpStream, String> {
    let mut tcp = TcpStream::connect(&proxy.addr)
        .await
        .map_err(|e| format!("连不上内置 Tailscale 代理 {}：{e}", proxy.addr))?;
    let _ = tcp.set_nodelay(true);

    let has_auth = !proxy.user.is_empty() || !proxy.pass.is_empty();
    if has_auth {
        tcp.write_all(&[0x05, 0x01, 0x02])
            .await
            .map_err(|e| format!("SOCKS5 握手发送失败：{e}"))?;
    } else {
        tcp.write_all(&[0x05, 0x01, 0x00])
            .await
            .map_err(|e| format!("SOCKS5 握手发送失败：{e}"))?;
    }

    let mut greeting = [0u8; 2];
    tcp.read_exact(&mut greeting)
        .await
        .map_err(|e| format!("SOCKS5 握手响应读取失败：{e}"))?;
    if greeting[0] != 0x05 {
        return Err(format!("SOCKS5 代理协议版本不对：{}", greeting[0]));
    }
    match greeting[1] {
        0x00 if !has_auth => {}
        0x02 => {
            let user = proxy.user.as_bytes();
            let pass = proxy.pass.as_bytes();
            if user.len() > 255 || pass.len() > 255 {
                return Err("SOCKS5 认证凭据过长".to_owned());
            }
            let mut auth = Vec::with_capacity(3 + user.len() + pass.len());
            auth.push(0x01);
            auth.push(user.len() as u8);
            auth.extend_from_slice(user);
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass);
            tcp.write_all(&auth)
                .await
                .map_err(|e| format!("SOCKS5 认证发送失败：{e}"))?;
            let mut resp = [0u8; 2];
            tcp.read_exact(&mut resp)
                .await
                .map_err(|e| format!("SOCKS5 认证响应读取失败：{e}"))?;
            if resp[0] != 0x01 || resp[1] != 0x00 {
                return Err(format!("SOCKS5 代理认证被拒（状态 {}）", resp[1]));
            }
        }
        0xff => return Err("SOCKS5 代理没有可接受的认证方式".to_owned()),
        other => return Err(format!("SOCKS5 代理返回了未预期的认证方式：{other}")),
    }

    let mut req = Vec::with_capacity(64);
    req.extend_from_slice(&[0x05, 0x01, 0x00]);
    match host {
        Host::Domain(d) => {
            let bytes = d.as_bytes();
            if bytes.len() > 255 {
                return Err(format!("主机名过长：{d}"));
            }
            req.push(0x03);
            req.push(bytes.len() as u8);
            req.extend_from_slice(bytes);
        }
        Host::Ipv4(ip) => {
            req.push(0x01);
            req.extend_from_slice(&ip.octets());
        }
        Host::Ipv6(ip) => {
            req.push(0x04);
            req.extend_from_slice(&ip.octets());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&req)
        .await
        .map_err(|e| format!("SOCKS5 CONNECT 发送失败：{e}"))?;

    let mut head = [0u8; 4];
    tcp.read_exact(&mut head)
        .await
        .map_err(|e| format!("SOCKS5 CONNECT 响应读取失败：{e}"))?;
    if head[0] != 0x05 {
        return Err(format!("SOCKS5 CONNECT 响应版本不对：{}", head[0]));
    }
    if head[1] != 0x00 {
        return Err(format!("经内置 Tailscale 连不上 {host}:{port}（SOCKS5 错误码 {}）", head[1]));
    }
    let tail_len = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len)
                .await
                .map_err(|e| format!("SOCKS5 响应地址长度读取失败：{e}"))?;
            len[0] as usize + 2
        }
        other => return Err(format!("SOCKS5 响应地址类型未知：{other}")),
    };
    let mut discard = vec![0u8; tail_len];
    tcp.read_exact(&mut discard)
        .await
        .map_err(|e| format!("SOCKS5 响应地址读取失败：{e}"))?;
    Ok(tcp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn socks5_auth_and_domain_connect_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut g = [0u8; 3];
            s.read_exact(&mut g).await.unwrap();
            assert_eq!(g, [0x05, 0x01, 0x02]);
            s.write_all(&[0x05, 0x02]).await.unwrap();

            let mut ah = [0u8; 2];
            s.read_exact(&mut ah).await.unwrap();
            assert_eq!(ah[0], 0x01);
            let mut u = vec![0u8; ah[1] as usize];
            s.read_exact(&mut u).await.unwrap();
            let mut pl = [0u8; 1];
            s.read_exact(&mut pl).await.unwrap();
            let mut p = vec![0u8; pl[0] as usize];
            s.read_exact(&mut p).await.unwrap();
            assert_eq!(u, b"tsnet");
            assert_eq!(p, b"s3cr3t");
            s.write_all(&[0x01, 0x00]).await.unwrap();

            let mut ch = [0u8; 5];
            s.read_exact(&mut ch).await.unwrap();
            assert_eq!(&ch[..4], &[0x05, 0x01, 0x00, 0x03]);
            let mut dom = vec![0u8; ch[4] as usize];
            s.read_exact(&mut dom).await.unwrap();
            let mut port = [0u8; 2];
            s.read_exact(&mut port).await.unwrap();
            assert_eq!(dom, b"box.tail.ts.net");
            assert_eq!(u16::from_be_bytes(port), 4923);

            s.write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x13, 0x3b]).await.unwrap();
            s.write_all(b"hello").await.unwrap();
        });

        let proxy = ProxyInfo { addr: addr.to_string(), user: "tsnet".into(), pass: "s3cr3t".into() };
        let mut stream = connect(&proxy, &Host::Domain("box.tail.ts.net"), 4923).await.unwrap();
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        server.await.unwrap();
    }
}
