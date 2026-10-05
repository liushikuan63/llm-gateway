//! B3 审计检索与导出的验收测试。
//!
//! 直接对**真 SQLite**（内存库）跑，不起网关：本卡的判定全在数据层
//! （过滤、分页、导出塑形），走 HTTP 只会多一层噪音。
//!
//! 每条「过滤生效」的用例都造了 10 条数据并断言命中 3 条 ——
//! 只断言「命中了」是不够的：过滤条件写错成「全返回」时，
//! `contains` 之类的断言照样绿。

use llm_gateway_lib::audit::{
    check_export_size, AuditConfig, RequestFilter, EXPORT_MAX_ROWS, MAX_PAGE_SIZE,
};
use llm_gateway_lib::db::{self, repo};

/// 造一条请求记录。
#[allow(clippy::too_many_arguments)]
async fn seed(
    db: &db::Db,
    ts: i64,
    provider: &str,
    model: &str,
    status: i64,
    error: Option<&str>,
    cost: Option<f64>,
    currency: &str,
    fallback_attempts: i64,
    attempts_json: Option<&str>,
    refined_prompt: Option<&str>,
) {
    repo::log_request_at(
        db.pool(),
        ts,
        repo::RequestLog {
            session_id: Some("s-audit"),
            client: Some("local-unified-key"),
            requested_model: "auto",
            routed_provider: Some(provider),
            routed_model: Some(model),
            status: Some(status),
            latency_ms: 100,
            prompt_tokens: 10,
            completion_tokens: 5,
            fallback_attempts,
            error,
            cost,
            currency: Some(currency),
            rate_label: None,
            estimated_prompt_tokens: Some(9),
            attempts_json,
            route: Default::default(),
            access_key_id: None,
            refined_prompt,
        },
    )
    .await
    .unwrap();
}

/// 造 10 条基准数据：`id` 1..10，`ts` 1000..1009，
/// provider 交替 `alpha`/`beta`，其中 3 条带 error、3 条有降级。
///
/// 返回它们的 id（升序）。
async fn seed_ten(db: &db::Db) -> Vec<i64> {
    let mut ids = Vec::new();
    for i in 0..10i64 {
        let provider = if i % 2 == 0 { "alpha" } else { "beta" };
        let status = if i < 3 { 500 } else { 200 };
        let error = if i < 3 { Some("上游 500") } else { None };
        let fallback = if i >= 7 { 2 } else { 0 };
        seed(
            db,
            1000 + i,
            provider,
            if i < 5 { "cheap" } else { "pricey" },
            status,
            error,
            Some(i as f64 * 0.1),
            "USD",
            fallback,
            None,
            None,
        )
        .await;
        ids.push(i + 1);
    }
    ids
}

async fn new_db() -> db::Db {
    let db = db::Db::connect_in_memory().await.unwrap();
    db
}

// ---------- 过滤 ----------

#[tokio::test]
async fn 时间区间过滤只返回区间内的行() {
    let db = new_db().await;
    seed_ten(&db).await;

    // ts 是 1000..1009，取 [1002, 1004] 应恰好命中 3 条
    let (rows, total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            from_ts: Some(1002),
            to_ts: Some(1004),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 3, "命中总数必须是 3，不是 10");
    assert_eq!(rows.len(), 3);
    for r in &rows {
        assert!((1002..=1004).contains(&r.ts), "跑出区间了：ts={}", r.ts);
    }

    // 反向：不加区间必须返回全部 10 条 —— 否则上面那条可能只是因为
    // 查询永远只返回 3 条
    let (all, all_total) = repo::query_requests(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(all_total, 10);
    assert_eq!(all.len(), 10);
}

#[tokio::test]
async fn provider_过滤生效() {
    let db = new_db().await;
    seed_ten(&db).await;

    let (rows, total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            provider: Some("alpha".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 5, "alpha 是偶数位，应命中 5 条");
    assert!(rows
        .iter()
        .all(|r| r.routed_provider.as_deref() == Some("alpha")));

    let (b_rows, b_total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            provider: Some("beta".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(b_total, 5);
    assert!(b_rows
        .iter()
        .all(|r| r.routed_provider.as_deref() == Some("beta")));

    // 不存在的 provider 要返回 0，而不是「忽略这个条件返回全部」
    let (_, none) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            provider: Some("不存在".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(none, 0, "条件写错时返回 0，不是忽略条件");
}

#[tokio::test]
async fn 只看有_error_的_过滤生效() {
    let db = new_db().await;
    seed_ten(&db).await;

    let (rows, total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            only_errors: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 3, "只有前 3 条带 error");
    assert!(rows.iter().all(|r| r.error.is_some()));

    // 关掉开关必须回到 10 条（对照组）
    let (_, off) = repo::query_requests(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(off, 10);
}

#[tokio::test]
async fn 只看降级的_过滤生效() {
    let db = new_db().await;
    seed_ten(&db).await;
    let (rows, total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            only_fallbacks: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 3, "id 8/9/10 有降级");
    assert!(rows.iter().all(|r| r.fallback_attempts > 0));
}

#[tokio::test]
async fn 状态类与具体状态码都能过滤() {
    let db = new_db().await;
    seed_ten(&db).await;
    let f = |s: &str| RequestFilter {
        status: Some(s.into()),
        ..Default::default()
    };
    assert_eq!(
        repo::query_requests(db.pool(), &f("5xx")).await.unwrap().1,
        3
    );
    assert_eq!(
        repo::query_requests(db.pool(), &f("2xx")).await.unwrap().1,
        7
    );
    assert_eq!(
        repo::query_requests(db.pool(), &f("500")).await.unwrap().1,
        3
    );
    assert_eq!(
        repo::query_requests(db.pool(), &f("200")).await.unwrap().1,
        7
    );
    // 没出现过的码是 0
    assert_eq!(
        repo::query_requests(db.pool(), &f("404")).await.unwrap().1,
        0
    );
}

#[tokio::test]
async fn 成本区间与币种过滤生效() {
    let db = new_db().await;
    seed_ten(&db).await;
    // cost = i * 0.1，即 0.0, 0.1, ..., 0.9
    //
    // 上界刻意取 0.75 而不是 0.7：`7 * 0.1` 在 f64 里是 0.7000000000000001，
    // `cost <= 0.7` 会把它排掉，于是「0.7 这一条到底算不算」变成一个
    // 用户无法预测的行为。第一版就踩了这个，期望 3 实际 2。
    //
    // 没去给 `<=` 加 epsilon：那属于口径变更（用户设的上限是不是闭区间），
    // 要单独定，不该由一条测试顺手改掉。这里只把边界挪开，
    // 并把「恰好落在边界上的行行为不可预测」这件事记下来。
    let (_, total) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            min_cost: Some(0.5),
            max_cost: Some(0.75),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total, 3, "0.5 / 0.6 / 0.7 三条");

    let (_, usd) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            currency: Some("USD".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(usd, 10);
    let (_, cny) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            currency: Some("CNY".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(cny, 0);
}

// ---------- 分页 ----------

#[tokio::test]
async fn 分页不重复不遗漏() {
    let db = new_db().await;
    // 造 25 条，每页 7 条 ⇒ 4 页（最后一页 4 条）
    for i in 0..25i64 {
        seed(
            &db,
            2000 + i,
            "alpha",
            "m",
            200,
            None,
            None,
            "USD",
            0,
            None,
            None,
        )
        .await;
    }

    let page_size = 7u32;
    let mut seen: Vec<i64> = Vec::new();
    let mut offset = 0u32;
    loop {
        let (rows, total) = repo::query_requests(
            db.pool(),
            &RequestFilter {
                limit: Some(page_size),
                offset: Some(offset),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(total, 25, "总数每页都要一样");
        if rows.is_empty() {
            break;
        }
        seen.extend(rows.iter().map(|r| r.id));
        offset += page_size;
        if offset > 100 {
            panic!("分页没有终止");
        }
    }

    assert_eq!(seen.len(), 25, "翻完所有页应恰好拿到 25 条");
    let unique: std::collections::BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), 25, "有重复：说明分页边界算错了");
    // 且是全集的子集（不遗漏）
    let (all, _) = repo::query_requests(
        db.pool(),
        &RequestFilter {
            limit: Some(500),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let all_ids: std::collections::BTreeSet<i64> = all.iter().map(|r| r.id).collect();
    assert_eq!(unique, all_ids, "分页拿到的集合必须等于全集");
}

#[tokio::test]
async fn 同一秒内的多条在分页时也有稳定顺序() {
    // 这条挡的是「只按 ts 排序」：同一秒内多条时相对顺序由 SQLite 决定，
    // 翻页会重复或漏掉。
    let db = new_db().await;
    for _ in 0..12 {
        seed(
            &db, 3000, "alpha", "m", 200, None, None, "USD", 0, None, None,
        )
        .await;
    }
    let mut seen = Vec::new();
    for page in 0..3u32 {
        let (rows, _) = repo::query_requests(
            db.pool(),
            &RequestFilter {
                limit: Some(5),
                offset: Some(page * 5),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        seen.extend(rows.iter().map(|r| r.id));
    }
    assert_eq!(seen.len(), 12, "三页各 5 条应是 15 个位置里的 12 条");
    let unique: std::collections::BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), 12, "同一秒内的行也必须不重不漏：{seen:?}");
}

#[tokio::test]
async fn 每页上限被夹住() {
    let db = new_db().await;
    seed_ten(&db).await;
    // 请求 10000 条，实际只能拿到 10 条（数据只有 10），但页大小被夹到 500
    let f = RequestFilter {
        limit: Some(10_000),
        ..Default::default()
    };
    assert_eq!(f.page_size(), MAX_PAGE_SIZE);
    let (rows, _) = repo::query_requests(db.pool(), &f).await.unwrap();
    assert_eq!(rows.len(), 10);
}

// ---------- 导出塑形 ----------

#[tokio::test]
async fn jsonl_导出的_attempts_是数组不是字符串() {
    let db = new_db().await;
    seed(
        &db,
        4000,
        "alpha",
        "m",
        200,
        None,
        None,
        "USD",
        2,
        Some(r#"[{"provider":"p1","status":500},{"provider":"p2","status":200}]"#),
        None,
    )
    .await;

    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let text = llm_gateway_lib::audit::to_jsonl(&rows);
    let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();

    assert!(
        line["attempts"].is_array(),
        "attempts 必须是数组，塞成字符串就失去了导出的意义：{}",
        line["attempts"]
    );
    assert_eq!(line["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(line["attempts"][0]["provider"], "p1");
    assert_eq!(line["attempts"][1]["status"], 200);
}

#[tokio::test]
async fn csv_导出的行数等于命中条数加表头() {
    let db = new_db().await;
    seed_ten(&db).await;

    let rows = repo::query_requests_for_export(
        db.pool(),
        &RequestFilter {
            only_errors: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    let csv = llm_gateway_lib::audit::to_csv(&rows);
    assert_eq!(csv.lines().count(), 4, "3 条数据 + 1 行表头");
}

#[tokio::test]
async fn 导出内容里的_prompt_已被脱敏() {
    let db = new_db().await;
    // 直接塞一条**已脱敏**的（因为我们走 store 路径）
    let cfg = AuditConfig {
        store_refined_prompt: true,
    };
    let stored = llm_gateway_lib::audit::refined_prompt_to_store(
        &cfg,
        Some("帮我调一下 sk-abcdefghijklmnopqrstuvwxyz 这个 key"),
    );
    seed(
        &db,
        5000,
        "alpha",
        "m",
        200,
        None,
        None,
        "USD",
        0,
        None,
        stored.as_deref(),
    )
    .await;

    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let jsonl = llm_gateway_lib::audit::to_jsonl(&rows);
    let csv = llm_gateway_lib::audit::to_csv(&rows);

    assert!(
        !jsonl.contains("sk-abcdefghijklmnopqrstuvwxyz"),
        "JSONL 里有明文密钥"
    );
    assert!(
        !csv.contains("sk-abcdefghijklmnopqrstuvwxyz"),
        "CSV 里有明文密钥"
    );
    assert!(jsonl.contains("[已脱敏:密钥]"), "应留下脱敏标记");
}

#[test]
fn 导出超过上限时如实报错而不是静默截断() {
    // 不去真造 10 万条（太慢），直接喂数字给判定函数。
    // 第一版这里写的是 `assert!(EXPORT_MAX_ROWS > 0 && ...)` ——
    // clippy 直接指出那是常量断言，等于没测。
    assert!(check_export_size(0).is_ok());
    assert!(
        check_export_size(EXPORT_MAX_ROWS).is_ok(),
        "恰好等于上限应当放行"
    );
    assert!(
        check_export_size(EXPORT_MAX_ROWS + 1).is_err(),
        "超过一条就该拒绝"
    );
    // 报错必须**说清命中多少、上限多少、怎么办**，不能只说「失败」
    let msg = check_export_size(999_999).unwrap_err();
    assert!(msg.contains("999999"), "要报出实际命中数：{msg}");
    assert!(
        msg.contains(&EXPORT_MAX_ROWS.to_string()),
        "要报出上限：{msg}"
    );
    assert!(msg.contains("缩小时间区间"), "要给出可操作的建议：{msg}");
}

#[tokio::test]
async fn 少量数据能正常导出() {
    // 反向：证明上面那个上限不是「一律拒绝」
    let db = new_db().await;
    seed_ten(&db).await;
    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 10);
}

// ---------- 提示词存储开关 ----------

#[tokio::test]
async fn 默认配置下_prompt_列不被写入() {
    let db = new_db().await;
    let cfg = AuditConfig::default();
    assert!(!cfg.store_refined_prompt);

    // 默认配置下 refined_prompt_to_store 必须返回 None，
    // 而写入方拿到的就是 None ⇒ 数据库里那一列是 NULL。
    let stored = llm_gateway_lib::audit::refined_prompt_to_store(&cfg, Some("用户的原话"));
    assert_eq!(stored, None, "默认配置下不该产生任何可写入的内容");

    seed(
        &db,
        6000,
        "alpha",
        "m",
        200,
        None,
        None,
        "USD",
        0,
        None,
        stored.as_deref(),
    )
    .await;
    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].refined_prompt.is_none(),
        "默认配置下 refined_prompt 必须是 NULL，实际：{:?}",
        rows[0].refined_prompt
    );

    // 对照组：开启后同一条内容会被写入（且已脱敏）——
    // 证明上面那个 NULL 是「开关关着」，不是「这一列永远空」
    let on = AuditConfig {
        store_refined_prompt: true,
    };
    let stored_on = llm_gateway_lib::audit::refined_prompt_to_store(&on, Some("用户的原话"));
    assert_eq!(
        stored_on.as_deref(),
        Some("用户的原话"),
        "没有敏感内容时应原样存入"
    );
}

// ---------- 分页辅助 ----------

#[tokio::test]
async fn query_page_的_truncated_标记正确() {
    let db = new_db().await;
    seed_ten(&db).await;

    // 第一页 4 条，还有更多
    let p1 = repo::query_page(
        db.pool(),
        &RequestFilter {
            limit: Some(4),
            offset: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p1.rows.len(), 4);
    assert_eq!(p1.total, 10);
    assert!(p1.truncated, "10 条里只看 4 条，必须标还有更多");

    // 最后一页：offset 8 + 2 条 = 10 ⇒ 不再有更多
    let p3 = repo::query_page(
        db.pool(),
        &RequestFilter {
            limit: Some(4),
            offset: Some(8),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p3.rows.len(), 2);
    assert!(!p3.truncated, "已经是最后一页，不该标还有更多");
}
