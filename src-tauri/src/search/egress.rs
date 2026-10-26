//! Explicit unauthenticated search-only transport; never consult model/system proxies.
use parking_lot::Mutex;

pub(crate) fn validate_proxy(raw: &str) -> Result<reqwest::Url, &'static str> {
    let invalid = "搜索出口仅支持不含认证、路径、参数的HTTP或SOCKS5h根地址";
    if raw.len() > 2048
        || raw.is_empty()
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid);
    }
    let url = reqwest::Url::parse(raw).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "socks5h")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || raw.contains('@')
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(invalid);
    }
    // reqwest0.12 resolves SOCKS proxy hostnames synchronously when building
    // Proxy. Require an IP for that endpoint; destination DNS remains remote.
    if url.scheme() == "socks5h"
        && url
            .host_str()
            .unwrap()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_err()
    {
        return Err("SOCKS5h代理地址须使用IP，目标域名仍由代理解析");
    }
    Ok(url)
}

#[derive(Default)]
pub(super) struct ClientPool {
    cached: Mutex<Option<(String, reqwest::Client)>>,
}
impl ClientPool {
    pub(super) fn client(&self, proxy: Option<&str>) -> Result<reqwest::Client, &'static str> {
        let Some(raw) = proxy else {
            return super::executor::free_client().cloned();
        };
        let url = validate_proxy(raw)?;
        let key = url.to_string();
        let mut cached = self.cached.lock();
        if let Some((saved, client)) = cached.as_ref() {
            if *saved == key {
                return Ok(client.clone());
            }
        }
        let failed = "搜索专用出口初始化失败；未改用直连";
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(url).map_err(|_| failed)?)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|_| failed)?;
        *cached = Some((key, client.clone()));
        Ok(client)
    }
    pub(super) fn clear(&self) {
        self.cached.lock().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_noncredential_root_endpoints_are_valid() {
        for url in [
            "http://127.0.0.1:8080",
            "http://owned-proxy.invalid:8080/",
            "socks5h://127.0.0.1:1080",
            "socks5h://[::1]:1080",
        ] {
            assert!(validate_proxy(url).is_ok(), "{url}");
        }
        for url in [
            "",
            "http://owned:never@127.0.0.1:1",
            "http://@127.0.0.1:1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:1/path",
            "http://127.0.0.1:1/?q=owned",
            "http://127.0.0.1:1/#owned",
            "http://127.0.0.1:1\n",
            "socks5://127.0.0.1:1080",
            "socks5h://owned-proxy.invalid:1080",
            "https://127.0.0.1:1",
            "file:///owned",
        ] {
            assert!(validate_proxy(url).is_err(), "{url}");
        }
    }
}
