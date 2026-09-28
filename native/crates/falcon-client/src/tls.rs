//! TLS 配置：rustls + 系统信任库（设计文档 §4.7）。
//!
//! 自签证书的做法是用户把它加进钥匙串并设为信任，客户端不做自己的指纹固定——
//! 所以校验器必须是 rustls-platform-verifier（走 macOS Security.framework /
//! Windows CryptoAPI），而不是 webpki-roots 那份编进二进制的根证书清单。

use std::sync::{Arc, OnceLock};

use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;

static CONFIG: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();

/// REST 与 WS 共用的一份 rustls 配置，进程内只建一次。
///
/// 建失败（平台校验器初始化不了）时把原因存下来，每次用到都如实报错，而不是
/// 悄悄退回一个不校验的配置。
pub(crate) fn client_config() -> Result<Arc<ClientConfig>, String> {
    CONFIG
        .get_or_init(|| build().map(Arc::new).map_err(|e| format!("TLS 初始化失败：{e}")))
        .clone()
}

fn build() -> Result<ClientConfig, rustls::Error> {
    // 显式指定 ring：rustls 的进程级默认 provider 没人装过时，builder() 会 panic；
    // 而且整个 workspace 只有 ring 这一个加密后端（Cargo.toml 的注释）。
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    // 只报 http/1.1：WebSocket 升级必须走 HTTP/1.1（tungstenite 不会 h2 上的扩展
    // CONNECT），反代若支持 h2、而我们又报了 h2，就会协商到一条升级不了的连接。
    // reqwest 这边本来也没开 http2 feature。
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds_with_platform_verifier() {
        let config = super::client_config().expect("系统信任库应该可用");
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}
