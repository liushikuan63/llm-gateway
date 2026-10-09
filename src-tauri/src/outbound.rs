//! Shared outbound proxy policy. Local runtimes and controller APIs stay direct.

use reqwest::{Client, ClientBuilder, Proxy, Url};
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};

/// Validate without echoing a URL that may contain credentials.
pub fn validate_proxy(proxy: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = proxy.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    let url = Url::parse(raw).map_err(|_| "代理地址格式无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("代理地址须为 HTTP/HTTPS/SOCKS5 端点，不能包含凭据、路径或查询参数".into());
    }
    Ok(Some(url.to_string()))
}

/// Follow a managed port change only while this proxy still owns the binding.
pub fn rebind_managed_proxy(
    current: Option<&str>,
    previous: &str,
    next: &str,
) -> Result<Option<String>, String> {
    if validate_proxy(current)? == validate_proxy(Some(previous))? {
        validate_proxy(Some(next))
    } else {
        Ok(current.map(str::to_owned))
    }
}

/// Never send Ollama, local model servers or LAN services into the VPN.
pub fn is_local_target(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            // Bit masks preserve the crate's Rust 1.77 minimum version.
            let first = ip.segments()[0];
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
        Err(_) => false,
    }
}

pub fn apply_proxy(builder: ClientBuilder, proxy: Option<&str>) -> Result<ClientBuilder, String> {
    let Some(proxy) = validate_proxy(proxy)? else {
        // Preserve reqwest's existing environment proxy behavior when unset.
        return Ok(builder);
    };
    Ok(builder.no_proxy().proxy(Proxy::custom(move |target| {
        (!is_local_target(target)).then(|| proxy.clone())
    })))
}

/// Keep the connection pool across searches; reload only when the proxy changes.
pub fn search_client(proxy: Option<&str>) -> Result<Client, String> {
    type CachedClient = Option<(Option<String>, Client)>;
    static CACHE: OnceLock<Mutex<CachedClient>> = OnceLock::new();
    let key = validate_proxy(proxy)?;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "搜索代理连接池不可用".to_string())?;
    if let Some((previous, client)) = &*cache {
        if previous == &key {
            return Ok(client.clone());
        }
    }
    let client = apply_proxy(Client::builder(), key.as_deref())?
        .build()
        .map_err(|_| "无法创建搜索代理连接".to_string())?;
    *cache = Some((key, client.clone()));
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_port_change_follows_only_its_own_binding() {
        let old = "http://127.0.0.1:17890";
        let new = "http://127.0.0.1:17891";
        assert_eq!(
            rebind_managed_proxy(Some("http://127.0.0.1:17890/"), old, new).unwrap(),
            Some("http://127.0.0.1:17891/".into())
        );
        assert_eq!(rebind_managed_proxy(None, old, new).unwrap(), None);
        let other = "http://127.0.0.1:7890";
        assert_eq!(
            rebind_managed_proxy(Some(other), old, new).unwrap(),
            Some(other.into())
        );
    }

    #[test]
    fn rejects_invalid_or_credential_bearing_proxy_without_echoing_it() {
        for proxy in [
            "file:///tmp/proxy",
            "http://user:fixture-password@127.0.0.1:7890",
            "http://127.0.0.1:7890/private",
            "https://proxy.test?token=fixture",
        ] {
            let error = validate_proxy(Some(proxy)).unwrap_err();
            assert!(!error.contains("fixture"));
            assert!(!error.contains(proxy));
        }
        assert_eq!(validate_proxy(Some(" ")).unwrap(), None);
        assert!(validate_proxy(Some("socks5h://127.0.0.1:17890")).is_ok());
    }

    #[test]
    fn local_targets_bypass_explicit_proxy() {
        for target in [
            "http://localhost:11434",
            "http://LOCALHOST.:11434",
            "http://127.0.0.1:17909",
            "http://[::1]:8080",
            "http://10.0.0.8",
            "http://192.168.1.8",
            "http://[fd00::8]",
            "http://[fe80::1]",
        ] {
            assert!(is_local_target(&Url::parse(target).unwrap()), "{target}");
        }
        assert!(!is_local_target(
            &Url::parse("https://api.example.test").unwrap()
        ));
        assert!(!is_local_target(
            &Url::parse("https://localhost.example.test").unwrap()
        ));
    }

    #[tokio::test]
    async fn explicit_proxy_handles_public_targets_and_keeps_loopback_direct() {
        use axum::{routing::get, Router};
        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let direct_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let direct_addr = direct_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            axum::serve(
                proxy_listener,
                Router::new().fallback(|| async { "via-proxy" }),
            )
            .await
            .unwrap();
        });
        let direct_task = tokio::spawn(async move {
            axum::serve(
                direct_listener,
                Router::new().route("/", get(|| async { "direct" })),
            )
            .await
            .unwrap();
        });
        let proxy_url = format!("http://{proxy_addr}");
        let client = apply_proxy(Client::builder(), Some(&proxy_url))
            .unwrap()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let public = client
            .get("http://unresolvable.example.test/")
            .send()
            .await
            .unwrap();
        assert_eq!(public.text().await.unwrap(), "via-proxy");
        let local = client
            .get(format!("http://{direct_addr}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(local.text().await.unwrap(), "direct");
        proxy_task.abort();
        direct_task.abort();
    }
}
