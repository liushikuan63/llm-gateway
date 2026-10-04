//! 联网搜索：后端响应映射、免 Key 解析、失败语义与注入位置。
//!
//! 这几组用例守的是三条不能退让的性质：
//!
//! 1. **凭据被拒不回落到免 Key 后端**——那会用错的凭据反复打别人的服务；
//! 2. **搜索失败不阻断请求**——失败只体现在 `error` 与响应头上；
//! 3. **注入内容是纯文本**，不会破坏 `ContextStore` 对工具调用成对处理的不变量。

use std::time::Duration;

use llm_gateway_lib::config::{SearchBackendKind, SearchConfig, SearchInjectFormat};
use llm_gateway_lib::domain::{Message, Role};
use llm_gateway_lib::search::parse;
use llm_gateway_lib::search::backend::{
    BraveBackend, DuckDuckGoBackend, SearchBackend, SearchError, SearchQuery,
    SearchResult, SearXngBackend, TavilyBackend,
};
use llm_gateway_lib::search::parse::{decode, duckduckgo_lite, render};

use axum::routing::any;
use axum::Router;

/* ---------------------------- mock 辅助 ---------------------------- */

/// 起一个本地 HTTP 服务并返回 `(base_url, 直连 client)`。
/// client 关掉了代理：测试要的是「打到自己起的那个端口」。
async fn mock(status: u16, body: &'static str) -> (String, reqwest::Client) {
    // 用 `any` 而不是 `post`：Tavily 走 POST，Brave 与 SearXNG 走 GET。
    // 只挂 post 会让后两个拿到 axum 的 405，测试就成了「测的是自己写错的 mock」。
    let app = Router::new().fallback(any(move || async move {
        let code = axum::http::StatusCode::from_u16(status)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        (code, headers, body)
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定随机端口");
    let addr = listener.local_addr().expect("读取本地地址");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client");
    (format!("http://{addr}"), client)
}

fn query(max_results: u32) -> SearchQuery {
    SearchQuery {
        text: "测试查询".into(),
        max_results,
    }
}

/* ---------------------------- 响应映射 ---------------------------- */

#[tokio::test]
async fn tavily_响应映射到统一结构() {
    let (base, client) = mock(
        200,
        r#"{"results":[
            {"title":"Rust 1.99","url":"https://blog.rust-lang.org/1.99","content":"新特性","score":0.87},
            {"title":"无链接","url":"","content":"应当被过滤","score":0.1}
        ]}"#,
    )
    .await;
    let results = TavilyBackend { base_url: base.clone() }
        .search(&client, &query(5), Some("key"))
        .await
        .expect("应当解析成功");
    assert_eq!(results.len(), 1, "没有 URL 的条目必须被过滤掉");
    assert_eq!(results[0].title, "Rust 1.99");
    assert_eq!(results[0].snippet, "新特性", "content 映射为摘要");
    assert!((results[0].score - 0.87).abs() < 1e-6, "Tavily 给了分就要保留");
}

#[tokio::test]
async fn tavily_缺结果字段时报错而不是给空列表() {
    let (base, client) = mock(200, r#"{"ok":true}"#).await;
    let error = TavilyBackend { base_url: base.clone() }
        .search(&client, &query(5), Some("key"))
        .await
        .expect_err("结构不符必须报错");
    assert!(matches!(error, SearchError::Malformed(_)), "实际 {error:?}");
}

#[tokio::test]
async fn brave_响应映射到统一结构() {
    let (base, client) = mock(
        200,
        r#"{"web":{"results":[{"title":"标题","url":"https://example.com","description":"摘要"}]}}"#,
    )
    .await;
    let results = BraveBackend { base_url: base.clone() }
        .search(&client, &query(5), Some("token"))
        .await
        .expect("应当解析成功");
    assert_eq!(results[0].snippet, "摘要", "description 映射为摘要");
    assert_eq!(results[0].score, 0.0, "Brave 不给分就不要编一个");
}

#[tokio::test]
async fn searxng_响应映射到统一结构() {
    let (base, client) = mock(
        200,
        r#"{"results":[{"title":"T","url":"https://e.com","content":"C","score":0.5}]}"#,
    )
    .await;
    let results = SearXngBackend { base_url: base.clone() }
        .search(&client, &query(5), None)
        .await
        .expect("应当解析成功");
    assert_eq!(results[0].snippet, "C");
}

#[tokio::test]
async fn searxng_拒绝非_http_地址() {
    let backend = SearXngBackend {
        base_url: "file:///etc".into(),
    };
    let http = reqwest::Client::new();
    let error = backend
        .search(&http, &query(5), None)
        .await
        .expect_err("必须拒绝");
    assert!(matches!(error, SearchError::Malformed(_)));
}

#[tokio::test]
async fn 缺少凭据时需要_key_的后端直接报错() {
    let http = reqwest::Client::new();
    assert!(matches!(
        TavilyBackend::default().search(&http, &query(5), None).await,
        Err(SearchError::MissingCredential)
    ));
    assert!(
        matches!(
            TavilyBackend::default().search(&http, &query(5), Some("   ")).await,
            Err(SearchError::MissingCredential)
        ),
        "空白串不算已配置"
    );
    assert!(matches!(
        BraveBackend::default().search(&http, &query(5), None).await,
        Err(SearchError::MissingCredential)
    ));
}

#[tokio::test]
async fn http_401_被标记为凭据失效以便调用方停止回落() {
    let (base, client) = mock(401, r#"{"error":"bad key"}"#).await;
    let error = TavilyBackend { base_url: base.clone() }
        .search(&client, &query(5), Some("k"))
        .await
        .expect_err("401 必须报错");
    assert!(
        matches!(error, SearchError::CredentialRejected),
        "401/403 必须与网络错误区分开，否则调用方会继续拿错凭据重试"
    );
}

#[tokio::test]
async fn 结果条数上限被遵守() {
    let (base, client) = mock(
        200,
        r#"{"results":[
            {"title":"1","url":"https://a","content":""},
            {"title":"2","url":"https://b","content":""},
            {"title":"3","url":"https://c","content":""},
            {"title":"4","url":"https://d","content":""}
        ]}"#,
    )
    .await;
    let results = TavilyBackend { base_url: base.clone() }
        .search(&client, &query(2), Some("k"))
        .await
        .expect("应当解析成功");
    assert_eq!(results.len(), 2, "max_results 必须真的限制条数");
}

/* -------------------------- 免 Key 解析 -------------------------- */

/// 按 lite 版结构手写的 HTML，含实体与百分号转义。
/// 第二条用**中文百分号编码**的 URL：它必须保持编码原样，因为解开会得到
/// 一个语义已改变、且不再是合法转义的地址。
fn lite_html() -> String {
    r#"
<table>
<tr><td><a rel="nofollow" href="https://example.com/a%20b" class='result-link'>A &amp; B</a></td></tr>
<tr><td class='result-snippet'>摘要一</td></tr>
<tr><td><a rel="nofollow" href="https://example.org/%E4%B8%AD%E6%96%87" class='result-link'>中文标题</a></td></tr>
<tr><td class='result-snippet'>摘要二 &lt;转义&gt;</td></tr>
<tr><td><a rel="nofollow" href="https://example.net/c" class="result-link">第三个</a></td></tr>
</table>
"#
    .to_string()
}

#[test]
fn lite_html_解析出标题_链接_摘要三元组() {
    let results = duckduckgo_lite(&lite_html(), 10);
    assert_eq!(results.len(), 3, "三条链接都应被解析出来");
    assert_eq!(results[0].title, "A & B", "HTML 实体必须解码");
    assert_eq!(
        results[0].url, "https://example.com/a%20b",
        "URL 里的百分号必须保持编码：解开会得到带裸空格的不可用链接"
    );
    assert_eq!(results[0].snippet, "摘要一");
    assert_eq!(results[1].title, "中文标题", "UTF-8 不能被逐字节解码破坏");
    assert_eq!(
        results[1].url, "https://example.org/%E4%B8%AD%E6%96%87",
        "中文 URL 的多字节转义是合法地址，必须保持原样"
    );
    assert_eq!(results[1].snippet, "摘要二 <转义>", "摘要里的实体要解码");
    assert_eq!(results[2].title, "第三个");
    assert_eq!(
        results[2].snippet, "",
        "没有摘要时留空而不是编一个"
    );
}

#[test]
fn 双引号写法也能解析() {
    // 上游改一次引号风格就静默返回空列表的代价太大，两种都必须吃。
    let html = r#"<a href="https://x.com/1" class="result-link">标题</a><td class="result-snippet">摘要</td>"#;
    let results = duckduckgo_lite(html, 5);
    assert_eq!(results.len(), 1, "双引号 class 必须被识别");
    assert_eq!(results[0].url, "https://x.com/1");
    assert_eq!(results[0].title, "标题");
    assert_eq!(results[0].snippet, "摘要");
}

#[test]
fn 跳转链接被解包成真实地址() {
    let html = r#"<a href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa%26b%3D1&amp;rut=x" class='result-link'>标题</a>"#;
    let results = duckduckgo_lite(html, 5);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].url, "https://example.org/a&b=1",
        "uddg 包装必须解开，否则注入上下文里全是点不开的跳转地址"
    );
}

#[test]
fn 非_http_跳转链接被丢弃而不是当成结果() {
    let html = r#"<a href="//duckduckgo.com/l/?uddg=javascript:alert(1)" class='result-link'>标题</a>"#;
    assert!(duckduckgo_lite(html, 5).is_empty());
}

#[test]
fn data_href_不会把真_href_顶掉() {
    // `data-href` 里也含 "href"，而且后面**同样紧跟 `=`**。
    // 只判「后面是不是 =」会从 `data-href="x"` 里取出 `x` 当成 URL。
    let html = r#"<a data-href="x" href="https://real.example/1" class='result-link'>标题</a>"#;
    let results = duckduckgo_lite(html, 5);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://real.example/1");
}

#[test]
fn 只有_data_href_时取不到_url_整条丢弃() {
    // 反向用例：没有真 `href` 时不能把 `data-href` 的值当地址塞进注入上下文。
    let html = r#"<a data-href="https://fake.example/1" class='result-link'>标题</a>"#;
    assert!(
        duckduckgo_lite(html, 5).is_empty(),
        "取不到真实 href 时应当丢弃这条结果，而不是给出一个假地址"
    );
}

#[test]
fn 解析条数受_limit_限制() {
    assert_eq!(duckduckgo_lite(&lite_html(), 2).len(), 2);
    assert_eq!(duckduckgo_lite(&lite_html(), 0).len(), 0, "limit=0 时不返回任何结果");
}

#[test]
fn 结构变化时返回空列表而不是报错() {
    assert!(duckduckgo_lite("", 5).is_empty());
    assert!(duckduckgo_lite("<html><body>反爬验证</body></html>", 5).is_empty());
    assert!(
        duckduckgo_lite("<a class='result-link'>没有 href</a>", 5).is_empty(),
        "没有 href 的链接不能被当成结果"
    );
}

#[test]
fn 实体与百分号解码各自成立() {
    assert_eq!(decode("a&amp;b"), "a&b");
    assert_eq!(decode("&lt;tag&gt;"), "<tag>");
    assert_eq!(decode("&quot;x&quot;"), "\"x\"");
    assert_eq!(decode("&#39;"), "'");
    assert_eq!(decode("&nbsp;"), " ");
    assert_eq!(decode("%E4%B8%AD%E6%96%87"), "中文", "多字节百分号序列要按 UTF-8 还原");
    assert_eq!(decode("100%"), "100%", "落单的百分号必须原样保留");
    assert_eq!(decode("a%zzb"), "a%zzb", "非法转义原样保留");
    assert_eq!(decode("普通文本"), "普通文本");
}

#[tokio::test]
async fn 免_key_后端永远不要求凭据() {
    let http = reqwest::Client::builder().no_proxy().build().expect("client");
    let backend = DuckDuckGoBackend {
        // 指向一个不存在的本地端口，让它快速失败而不是去打真实网络。
        base_url: "http://127.0.0.1:1".into(),
    };
    match backend.search(&http, &query(5), None).await {
        Ok(_) => {}
        Err(error) => assert!(
            !matches!(error, SearchError::MissingCredential),
            "免 Key 后端永远不该要求凭据，实际 {error:?}"
        ),
    }
}

/* ---------------------------- 注入渲染 ---------------------------- */

#[test]
fn 渲染包含查询_来源与条数() {
    let results = vec![SearchResult {
        title: "标题".into(),
        url: "https://example.com".into(),
        snippet: "摘要".into(),
        score: 0.0,
    }];
    let text = render("rust 1.99", "duckduckgo", &results);
    assert!(text.contains("rust 1.99"));
    assert!(text.contains("duckduckgo"));
    assert!(text.contains("命中：1"));
    assert!(text.contains("https://example.com"));
    assert!(text.contains("摘要"));
}

#[test]
fn 渲染为空结果时如实写零() {
    let text = render("x", "duckduckgo", &[]);
    assert!(text.contains("命中：0"));
}

/* --------------------------- 注入与配置 --------------------------- */

#[test]
fn 注入内容是纯文本_不会破坏工具调用的成对处理() {
    let cfg = SearchConfig::default();
    let message = llm_gateway_lib::search::executor::as_message(
        &cfg,
        render("q", "duckduckgo", &[]),
    );
    assert_eq!(message.role, Role::System);
    assert!(message.tool_calls.is_none(), "注入消息绝不能带 tool_calls");
    assert!(message.content_text().contains("联网搜索结果"));
}

#[test]
fn 注入为_user_时角色正确() {
    let cfg = SearchConfig {
        inject_as: SearchInjectFormat::User,
        ..SearchConfig::default()
    };
    let message = llm_gateway_lib::search::executor::as_message(&cfg, "内容".into());
    assert_eq!(message.role, Role::User);
}

#[test]
fn 空检索词不触发网络往返() {
    let message = Message::user("这是一个没有任何检索意图的句子");
    let text = message.content_text();
    let query = llm_gateway_lib::search::query_from(&text, 400);
    assert!(!query.is_empty(), "正文本身应当成为检索词");
    assert_eq!(llm_gateway_lib::search::query_from("   ", 400), "");
}

#[test]
fn 检索词超长时保留尾部() {
    let text = format!("前置{}", "填充".repeat(500));
    let cut = llm_gateway_lib::search::query_from(&text, 100);
    assert!(cut.chars().count() <= 100, "必须被截断到上限");
    assert!(text.ends_with("填充") && cut.ends_with("填充"), "要保留尾部");
}

#[test]
fn 检索词折叠成单行() {
    assert_eq!(
        llm_gateway_lib::search::query_from("第一行\n\n  第二行  \n", 400),
        "第一行 第二行"
    );
}

#[test]
fn 搜索条数被钳制在_一到十() {
    let mut cfg = SearchConfig::default();
    cfg.max_results = 999;
    assert_eq!(cfg.normalized_max_results(), 10);
    cfg.max_results = 0;
    assert_eq!(cfg.normalized_max_results(), 1);
    cfg.max_results = 5;
    assert_eq!(cfg.normalized_max_results(), 5);
}

#[test]
fn searxng_后端必须填地址() {
    let mut cfg = SearchConfig {
        backend: SearchBackendKind::SearXng,
        searxng_url: None,
        ..SearchConfig::default()
    };
    assert!(
        llm_gateway_lib::search::validate(&cfg).is_err(),
        "选了 SearXNG 却没地址必须报错"
    );
    cfg.searxng_url = Some("http://127.0.0.1:8888".into());
    assert!(llm_gateway_lib::search::validate(&cfg).is_ok());
    cfg.searxng_url = Some("ftp://x".into());
    assert!(
        llm_gateway_lib::search::validate(&cfg).is_err(),
        "非 http/https 必须拒绝"    );
}

#[test]
fn 免_key_后端不需要地址校验() {
    let cfg = SearchConfig {
        backend: SearchBackendKind::DuckDuckGo,
        searxng_url: None,
        ..SearchConfig::default()
    };
    assert!(llm_gateway_lib::search::validate(&cfg).is_ok());
}

#[test]
fn 条数越界时校验期就报错() {
    let mut cfg = SearchConfig::default();
    cfg.max_results = 0;
    assert!(llm_gateway_lib::search::validate(&cfg).is_err());
    cfg.max_results = 11;
    assert!(llm_gateway_lib::search::validate(&cfg).is_err());
}

#[test]
fn 密钥掩码不泄露原文() {
    let masked = llm_gateway_lib::search::mask_key("tvly-abcdefghijklmnop").unwrap();
    assert!(masked.starts_with("tvly"));
    assert!(masked.ends_with("mnop"));
    assert!(!masked.contains("efghij"), "中间必须被遮住");
    assert_eq!(llm_gateway_lib::search::mask_key(""), None);
    assert_eq!(
        llm_gateway_lib::search::mask_key("short"),
        Some("••••".to_string())
    );
}

#[test]
fn 搜索与智能模式都默认关闭_不影响既有请求路径() {
    let cfg = llm_gateway_lib::config::AppConfig::default();
    assert!(!cfg.search.enabled, "联网搜索必须默认关闭");
    assert!(
        !cfg.smart_routing.enabled,
        "智能模式必须默认关闭，`auto` 的行为才不受影响"
    );
    assert_eq!(
        cfg.routing_strategy,
        llm_gateway_lib::config::RoutingStrategy::Priority,
        "默认策略没有被本批改动"
    );
}

/* -------------------- 必应中国站：真实样本解析 -------------------- */
//
// 样本是 2026-10-05 从 cn.bing.com 实抓的一整条结果块（未手工修饰），
// 存在 `tests/fixtures/bing_cn_sample.html`。
// 用真实样本而不是手写的简化 HTML：必应的 `<h2><a>` 里塞了 favicon、
// aria-label、`RedirectUrl` 等一堆干扰，手写样本会让测试「绿而无效」。

fn bing_sample() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bing_cn_sample.html"
    ))
    .expect("缺少必应样本（tests/fixtures/bing_cn_sample.html）")
}

#[test]
fn 必应_真实样本能解析出标题与地址() {
    let html = bing_sample();
    let results = parse::bing_cn(&html, 5);
    assert!(!results.is_empty(), "真实样本必须能解析出结果");
    let first = &results[0];
    assert_eq!(first.url, "https://rust-lang.org/", "地址应从 <h2><a href> 里取出");
    assert!(
        !first.title.is_empty(),
        "标题不能为空（样本里 <a> 内含 favicon 等嵌套标签）"
    );
    assert!(
        !first.title.contains('<'),
        "标题里的标签必须被剥掉，实际：{:?}",
        first.title
    );
    assert!(
        first.title.chars().any(|c| c.is_ascii_alphabetic() || c == '中' as char),
        "标题应留下文字，实际：{:?}",
        first.title
    );
}

#[test]
fn 必应_limit为零时返回空() {
    assert!(parse::bing_cn(&bing_sample(), 0).is_empty());
}

#[test]
fn 必应_没有结果块时返回空而不是报错() {
    assert!(parse::bing_cn("<html><body>没有结果</body></html>", 5).is_empty());
    assert!(parse::bing_cn("", 5).is_empty());
}

#[test]
fn 必应_多个结果块按顺序取出且不超过_limit() {
    // 造两个块，验证块边界而不是只认第一块。
    let html = format!(
        "<li class=\"b_algo\"><h2><a href=\"https://a.example/\">第一个标题</a></h2><p>摘要一</p></li>{}\
         <li class=\"b_algo\"><h2><a href=\"https://b.example/\">第二个标题</a></h2><p>摘要二</p></li>",
        ""
    );
    let all = parse::bing_cn(&html, 10);
    assert_eq!(all.len(), 2, "两个块都要取出，实际 {}", all.len());
    assert_eq!(all[0].url, "https://a.example/");
    assert_eq!(all[1].url, "https://b.example/");
    assert_eq!(all[0].snippet, "摘要一", "摘要应从 <p> 里取出");

    let one = parse::bing_cn(&html, 1);
    assert_eq!(one.len(), 1, "limit=1 时只能给一条，实际 {}", one.len());
    assert_eq!(one[0].url, "https://a.example/");
}

#[test]
fn 必应_锚点地址为空或井号时跳过该条() {
    let html = "<li class=\"b_algo\"><h2><a href=\"#\">跳转到别处</a></h2></li>\
                <li class=\"b_algo\"><h2><a href=\"https://ok.example/\">真结果</a></h2></li>";
    let results = parse::bing_cn(&html, 10);
    assert_eq!(results.len(), 1, "空/井号地址必须跳过，实际 {}", results.len());
    assert_eq!(results[0].url, "https://ok.example/");
}

#[tokio::test]
async fn 必应_枚举名是_bing_cn_不是_bingcn() {
    // 枚举值写错会让 TOML 解析失败、整个应用起不来（真机踩过）。
    let parsed: SearchConfig = toml::from_str("backend = \"bing_cn\"").unwrap();
    assert_eq!(parsed.backend, SearchBackendKind::BingCn);
    assert!(
        toml::from_str::<SearchConfig>("backend = \"bingcn\"").is_err(),
        "错误拼法必须报错，不能悄悄兜底"
    );
}

#[test]
fn 必应_中文标题与地址不得被字节偏移截断() {
    // 这条钉的是**真实踩到的 bug**：`find()` 返回相对偏移，被当成绝对下标后
    // `&s[a..=b]` 会在 UTF-8 多字节字符中间切开 —— 中文标题必然触发。
    // 症状：标题变成 `e/"第一个标题`、地址变成 `https://a.exampl`。
    let html = "<li class=\"b_algo\"><h2><a href=\"https://a.example/\">第一个中文标题</a></h2>\
                <p>第一段中文摘要</p></li>";
    let results = parse::bing_cn(html, 5);
    assert_eq!(results.len(), 1, "必须恰好取出一条");
    assert_eq!(results[0].url, "https://a.example/", "地址不得被截断");
    assert_eq!(results[0].title, "第一个中文标题", "标题不得被截断");
    assert_eq!(results[0].snippet, "第一段中文摘要", "摘要不得被截断");
}

#[test]
fn 实体_必应摘要里的_ensp_与数字实体要解掉() {
    // 实测必应摘要是 `19 小时之前&ensp;&#0183;&ensp;生产环境…`，
    // 不解就会把这几串字符原样塞进模型上下文。
    assert_eq!(
        parse::decode_entities("19 小时之前&ensp;&#0183;&ensp;正文"),
        // `&ensp;` → 空格，`&#0183;` → `·`，`&ensp;` → 空格
        "19 小时之前 · 正文"
    );
    // 十六进制形式
    assert_eq!(parse::decode_entities("A&#xB7;B"), "A·B");
    assert_eq!(parse::decode_entities("&rarr;"), "→");
    // 没有分号 / 空内容 / 超长，都必须原样保留而不是瞎解
    assert_eq!(parse::decode_entities("&#183"), "&#183");
    assert_eq!(parse::decode_entities("&#;"), "&#;");
    assert_eq!(parse::decode_entities("&#999999999999999;"), "&#999999999999999;");
    // 控制字符不该被解进来（会污染界面与日志）
    assert_eq!(parse::decode_entities("&#0;"), "&#0;");
}

#[test]
fn 必应_真实摘要不得残留未解码实体() {
    let html = bing_sample();
    for r in parse::bing_cn(&html, 5) {
        assert!(
            !r.snippet.contains("&ensp;") && !r.snippet.contains("&#"),
            "摘要里仍有未解码实体：{:?}",
            r.snippet
        );
        assert!(
            !r.title.contains('&') || !r.title.contains("ensp"),
            "标题里仍有未解码实体：{:?}",
            r.title
        );
    }
}

#[test]
fn 实体_两个解码函数必须给出一致结果() {
    // 这条钉的是**已踩两次的坑**：`decode_entities()`（URL 用）与 `decode()`
    // （展示文本用）各写一份实体分支，改一处漏一处。
    // 症状：必应摘要在模型眼里留着 `&ensp;&#0183;` —— 单独测 `&#0183;` 是好的，
    // 夹在 `&ensp;` 后面就解不掉，因为位数上限算错了对象。
    for s in [
        "&ensp;",
        "&#0183;",
        "&#xB7;",
        "A&ensp;&#0183;&ensp;B",
        "19 小时之前&ensp;&#0183;&ensp;正文",
        "x&hellip;y&rarr;z",
        "&#0;",
        "&#999999999999;",
        "普通文本",
    ] {
        assert_eq!(
            parse::decode_entities(s),
            parse::decode(s),
            "「{s}」在两个解码函数里结果不一致"
        );
    }
}

#[test]
fn 后端码_响应头用的必须是展示拼法() {
    // 同一个后端有**两套字符串**，用途不同（见设计方案 §6.1）：
    //   配置值 = serde snake_case：`sear_xng` / `duck_duck_go`
    //   展示值 = 手写 backend_code：`searxng` / `duckduckgo`
    // 写混的后果是不对称的：配置值写错 → 应用打不开；展示值写错 → 日志难读但不影响功能。
    // 这里钉的是展示值，配置值那一侧由 tests/config.rs 的前后端一致性测试负责。
    assert_eq!(llm_gateway_lib::search::backend_code(SearchBackendKind::Tavily), "tavily");
    assert_eq!(llm_gateway_lib::search::backend_code(SearchBackendKind::Brave), "brave");
    assert_eq!(llm_gateway_lib::search::backend_code(SearchBackendKind::SearXng), "searxng");
    assert_eq!(llm_gateway_lib::search::backend_code(SearchBackendKind::DuckDuckGo), "duckduckgo");
    assert_eq!(llm_gateway_lib::search::backend_code(SearchBackendKind::BingCn), "bing_cn");

    // **对照组**：展示值不得等于配置值，否则两套字符串会在下一次维护时被合并成一个，
    // 而配置侧会随之解析失败。BingCn 恰好同形，必须显式豁免而不是默认成立。
    for kind in [SearchBackendKind::SearXng, SearchBackendKind::DuckDuckGo] {
        assert_ne!(
            llm_gateway_lib::search::backend_code(kind.clone()),
            llm_gateway_lib::search::backend_serde_value(&kind),
            "展示值与配置值对 {:?} 同形，两套字符串会在维护中被误合并",
            kind
        );
    }
}

#[test]
fn searxng_只有它自己要求地址_对照组() {
    // 前端「选中 SearXNG 时延后保存」的修复依赖这条性质：
    // 其它后端不因 searxng_url 为空而被拒，所以选中它们可以立即保存。
    // 反过来若这条不成立，前端的延后逻辑就会让所有后端都存不下去。
    for kind in [
        SearchBackendKind::Tavily,
        SearchBackendKind::Brave,
        SearchBackendKind::BingCn,
        SearchBackendKind::DuckDuckGo,
    ] {
        let cfg = SearchConfig { backend: kind.clone(), searxng_url: None, ..SearchConfig::default() };
        assert!(
            llm_gateway_lib::search::validate(&cfg).is_ok(),
            "{kind:?} 不该要求 searxng_url —— 否则界面选中它却存不下去",
        );
    }
    // 组对照：同一个 searxng_url=None，SearXng 必须失败。
    let searx = SearchConfig { backend: SearchBackendKind::SearXng, searxng_url: None, ..SearchConfig::default() };
    assert!(llm_gateway_lib::search::validate(&searx).is_err());
}
