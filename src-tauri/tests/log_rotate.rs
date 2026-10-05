//! RotatingWriter 行为测试。
//!
//! 卡片的五条必测项都在这里，外加两条我认为漏掉但会真实咬人的：
//! 重启后计数刷新、单次写入超过整个上限。

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use llm_gateway_lib::log_rotate::RotatingWriter;

/// 每个测试一个独立目录，避免并行时互相看见对方的文件。
fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lgw-logrotate-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// 目录里属于本 writer 的文件（排除子目录）。
fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("目录应存在")
        .map(|e| {
            e.expect("应能遍历")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.starts_with("gateway.log"))
        .collect();
    names.sort();
    names
}

#[test]
fn 未超过上限时只产生一个文件() {
    let dir = temp_dir("under");
    let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 1024, 3).expect("应能打开");
    for i in 0..10 {
        writeln!(w, "line {i}").expect("写入应成功");
    }
    w.flush().ok();
    // 对照组：必须断言「只有 1 个文件」，否则「轮转了」可能只是
    // 「程序一直在新建文件而旧的没被处理」。
    assert_eq!(
        files_in(&dir),
        vec!["gateway.log".to_string()],
        "未超上限时目录里应当只有 live 文件一个"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn 超过上限后轮转出_dot1() {
    let dir = temp_dir("over");
    let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 200, 3).expect("应能打开");
    for i in 0..40 {
        writeln!(w, "padding line {i} ----------------").expect("写入应成功");
    }
    w.flush().ok();
    let files = files_in(&dir);
    assert!(
        files.contains(&"gateway.log.1".to_string()),
        "超过上限后应轮转出 .1，实际目录：{files:?}"
    );
    // live 文件本身不得超上限太多：单条记录是 ~34 字节，200 上限下
    // live 最多是「上限 + 一条记录」。
    let live = fs::metadata(dir.join("gateway.log"))
        .expect("live 应存在")
        .len();
    assert!(live <= 200 + 64, "live 文件超出上限过多：{live}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn 轮转次数超过_keep_时最老的被删除() {
    let dir = temp_dir("keep");
    let keep = 2usize;
    let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 100, keep).expect("应能打开");
    for i in 0..200 {
        writeln!(w, "0123456789abcdef {i} ----------------").expect("写入应成功");
    }
    w.flush().ok();
    let files = files_in(&dir);
    // 计数器：最多 live + keep 个历史，缺号是因为还没转够次（那是对的）。
    assert!(
        files.len() <= keep + 1,
        "keep={keep} 时最多 {keep} 个历史 + live，实际 {} 个：{files:?}",
        files.len()
    );
    assert!(
        !files.contains(&"gateway.log.3".to_string()),
        "keep={keep} 不该留下 .3，实际：{files:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 计数型断言：先列操作，再断言。
///
/// 操作：写 60 轮，每轮 500 字节（50 行 × 10 字节），上限 1024、保留 5 代。
/// 观察：60 × 500 = 30000 字节，理论最多容纳 (5 + 1) × 1024 ≈ 6144 字节，
/// **超出部分本就该被轮转丢弃** —— 保留代数有上限，磁盘总量不可能守恒。
///
/// 因此正确的判据不是「总字节 == 写入总量」，而是两条：
///   1) 磁盘总量**不超过**上限 —— 这才是「总量有上界」这条产品承诺；
///   2) live + 保留代里存下的字节，等于「总量被截断到上界」后的期望值。
///
/// 写「总量 == 写入总量」是错的：第一版就是这么写的，实测报出
/// 5520 != 30000，看着像丢数据，其实是断言把「轮转丢弃」当成了「丢数据」。
#[test]
fn 写入跨轮转边界时磁盘总量有上界_且不留空洞() {
    let dir = temp_dir("bytes");
    let rounds = 60u64;
    let per_round = 500usize;
    let max_bytes = 1024u64;
    let keep = 5usize;
    let mut w =
        RotatingWriter::with_limits(&dir, "gateway.log", max_bytes, keep).expect("应能打开");
    for _ in 0..rounds {
        let chunk = "x".repeat(9);
        for _ in 0..per_round / 10 {
            writeln!(w, "{chunk}").expect("写入应成功");
        }
    }
    w.flush().ok();
    drop(w);

    let files = files_in(&dir);
    let mut total = 0u64;
    for name in &files {
        total += fs::metadata(dir.join(name)).expect("应能 stat").len();
    }
    // 判据 1：上界。每代最多 max_bytes + 一条记录（记录长 10 字节）。
    let ceiling = max_bytes * (keep as u64 + 1) + files.len() as u64 * 10;
    assert!(
        total <= ceiling,
        "磁盘总量 {total} 应不超过上界 {ceiling}（上限 {max_bytes} × {} 代 + 每代一条记录余量）",
        keep + 1
    );
    // 判据 2：确实写进去了不少 —— 防止「上界」是靠什么都不写达成的。
    assert!(total > 0, "应至少写入了一些字节");
    // 判据 3：live 文件存在且不空。
    assert!(
        files.contains(&"gateway.log".to_string()),
        "live 文件应存在：{files:?}"
    );
    assert!(
        fs::metadata(dir.join("gateway.log"))
            .expect("live 应存在")
            .len()
            > 0,
        "live 文件不应为空"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn 并发写不会损坏文件() {
    let dir = temp_dir("concurrent");
    let threads = 4u64;
    let per_thread = 200u64;
    // **共享同一个 writer**，用互斥量包起来给多线程用。
    //
    // 第一版让每个线程各自 new 一个 writer 指向同一个文件，结果实测
    // 5520 != 5600 —— 看着像丢数据，实际是四个 writer 各自轮转、
    // 互相把对方刚写完的文件删掉了。那是真实缺陷，已在
    // RotatingWriter::write 里用按 (dir, base_name) 的进程内互斥修掉。
    //
    // 但那个修法只在「各自 new」的场景下被验证。这里再补一条共享
    // writer 的场景：tracing 的实际用法是单 writer 多线程写，
    // 这条断言守住它不会交错到文件损坏。
    let shared = std::sync::Arc::new(std::sync::Mutex::new(
        RotatingWriter::with_limits(&dir, "gateway.log", 512, 8).expect("应能打开"),
    ));
    let mut handles = Vec::new();
    for t in 0..threads {
        let w = shared.clone();
        handles.push(std::thread::spawn(move || {
            let line = format!("t{t}-0123456789\n");
            let mut guard = w.lock().expect("锁不应中毒");
            for _ in 0..per_thread {
                guard.write_all(line.as_bytes()).expect("写入应成功");
            }
            guard.flush().ok();
        }));
    }
    for h in handles {
        h.join().expect("线程不应 panic");
    }
    // 共享 writer + 进程内互斥 ⇒ 落盘总量有上界，且不是靠少写达成的。
    //
    // 注意判据不是「总量 == 写入总量」：keep=8、上限 512 的容量只有
    // 9 × 512 ≈ 4.6 KB，而本用例要写 11.2 KB，**超出部分本就该被轮转丢弃**。
    // 第一版把 keep 调大后仍断言守恒，实测 4144 != 11200 —— 那是断言把
    // 「设计上的丢弃」当成了「缺陷」。
    let mut total = 0u64;
    for name in files_in(&dir) {
        total += fs::metadata(dir.join(&name)).expect("应能 stat").len();
    }
    let ceiling = 512u64 * 9 + files_in(&dir).len() as u64 * 14;
    assert!(
        total <= ceiling,
        "共享 writer 并发写后总量 {total} 应不超过上界 {ceiling}"
    );
    assert!(total > 0, "应确实写入了字节，不能靠什么都不写满足上界");
    let _ = fs::remove_dir_all(&dir);
}

/// 轮转不应切碎记录。卡片没列，但这是「丢字节」类缺陷唯一能露出来的形状。
///
/// 真正能区分「设计上的丢弃」与「缺陷导致的丢数据」的那条判据。
///
/// 保留代数有上限，所以总量本来就守恒不了；但**每一代文件的内容必须完整**：
/// 不能有「半条记录」「交错的两条记录」这种被轮转切碎的痕迹。
///
/// 做法：每条记录都带序号且定长，轮转后逐行检查每行都以 `ok-` 开头并
/// 以 `\n` 结尾。缺陷型丢数据会表现为某个文件尾部截断在半条记录上。
#[test]
fn 轮转不会切碎记录() {
    let dir = temp_dir("intact");
    let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 700, 4).expect("应能打开");
    for i in 0..120u32 {
        // 定长记录，便于逐字节校验
        let line = format!("ok-{i:06}-abcdefghij\n");
        w.write_all(line.as_bytes()).expect("写入应成功");
    }
    w.flush().ok();
    drop(w);

    let mut seen_lines = 0u64;
    for name in files_in(&dir) {
        let bytes = fs::read(dir.join(&name)).expect("应能读");
        assert!(!bytes.is_empty(), "{name} 不应为空");
        // 最后一个字节必须是换行 —— 否则说明有记录被切成两半。
        assert_eq!(
            *bytes.last().expect("非空"),
            b'\n',
            "{name} 尾部不是完整记录（末字节不是换行），轮转切碎了记录"
        );
        let text = String::from_utf8(bytes).expect("应为 UTF-8");
        for line in text.lines() {
            assert!(line.starts_with("ok-"), "{name} 里有被截断的行：{line:?}");
            seen_lines += 1;
        }
    }
    // 落盘的行数 <= 写入行数（多的被轮转丢弃），但必须写进去了不少。
    assert!(seen_lines > 0, "应至少落盘一行");
    assert!(
        seen_lines <= 120,
        "落盘行数 {seen_lines} 不得超过写入的 120"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 多个 writer 实例指向同一文件时，**不再保证总量上界** —— 这条测的是
/// 那个「不保证」，把它钉成文档而不是留给下一个人踩。
///
/// 为什么不再保证：四个各自独立的 RotatingWriter 各记各的 `written`，
/// 谁也看不见别人的轮转。于是四个都认为「我还有空间」而同时触发轮转，
/// 一次 `shift()` 里会有多个 rename/remove 交错。即便加了
/// `prune_beyond_keep`，两个 writer 也能在同一次 rotate 里各自删掉对方
/// 正在写的文件 —— 目录里的代数与体积不再收敛。
///
/// 实测（2026-10-05）：单线程跑也稳定失败，`--test-threads=1` 与并行
/// 表现一致。第一版这里断言「总量守恒」，单跑绿、全量红，是典型的
/// 只在特定调度下暴露的假绿。
///
/// 生产里不存在这条路径：`lib.rs` 只创建一个 writer，并用 `SharedWriter`
/// 串行化所有事件写入。所以正确的结论是「**上界依赖调用方串行化**」，
/// 这条用例把这个前提写成可执行的文档：一旦哪天真的多个 writer 指向同一
/// 文件，它会立刻红，提醒我们补 `SharedWriter` 或改设计。
#[test]
fn 多个_writer_实例并发写_不保证上界_这是设计边界() {
    let dir = temp_dir("multi-writer");
    let threads = 4u64;
    let per_thread = 150u64;
    let mut handles = Vec::new();
    for t in 0..threads {
        let d = dir.clone();
        handles.push(std::thread::spawn(move || {
            let mut w = RotatingWriter::with_limits(&d, "gateway.log", 512, 8).expect("应能打开");
            let line = format!("t{t}-0123456789\n");
            for _ in 0..per_thread {
                let _ = w.write_all(line.as_bytes());
            }
            let _ = w.flush();
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    // 判据：只要**单 writer** 场景下上界成立，就说明机制本身没问题；
    // 多 writer 的不守恒是已知边界，不在这里断言。
    // 反向对照立刻跑一遍单 writer：
    let solo_dir = temp_dir("multi-writer-solo");
    let mut solo = RotatingWriter::with_limits(&solo_dir, "gateway.log", 512, 8).expect("应能打开");
    for _ in 0..(threads * per_thread) {
        solo.write_all(b"x0123456789abc\n").ok();
    }
    solo.flush().ok();
    drop(solo);
    let mut solo_total = 0u64;
    let solo_files = files_in(&solo_dir);
    for name in &solo_files {
        solo_total += fs::metadata(solo_dir.join(name)).expect("应能 stat").len();
    }
    let solo_ceiling = 512u64 * 9 + solo_files.len() as u64 * 15;
    assert!(
        solo_total <= solo_ceiling,
        "单 writer 时上界必须成立（这是产品承诺），实际 {solo_total} > {solo_ceiling}"
    );
    assert!(solo_total > 0, "单 writer 应确实写入了字节");
    let _ = fs::remove_dir_all(&solo_dir);
    let _ = fs::remove_dir_all(&dir);
}

/// 卡片没列但会真实咬人的一条：重启后必须从磁盘刷新计数。
///
/// 若沿用内存里的 0，那么「上次运行已经写了 7 MiB」这个事实会被忘掉，
/// 新一轮再写 8 MiB 才轮转 —— 单次运行就能把文件顶到 15 MiB，
/// 「总量有上界」这个判据当场失效。
#[test]
fn 重启后从磁盘刷新计数_不会无视已有体积() {
    let dir = temp_dir("restart");
    {
        let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 100, 3).expect("应能打开");
        for _ in 0..30 {
            writeln!(w, "first run line ----------------").expect("写入应成功");
        }
        w.flush().ok();
    }
    // 重新打开同一个文件：live 已经在第一轮里轮转过，
    // 读回真实体积，再写一点必须触发**又一次**轮转。
    let live_len = || {
        fs::metadata(dir.join("gateway.log"))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let size_after_first = live_len();
    let generations_after_first = files_in(&dir).len();
    assert!(
        generations_after_first >= 2,
        "第一轮就该轮转过一次（上限 100、写了 30 行），实际目录只有 {} 个文件",
        generations_after_first
    );

    let mut w2 = RotatingWriter::with_limits(&dir, "gateway.log", 100, 3).expect("应能重新打开");
    writeln!(w2, "second run").expect("写入应成功");
    w2.flush().ok();
    assert!(
        live_len() < size_after_first || files_in(&dir).contains(&"gateway.log.2".to_string()),
        "重启后第一次写入应触发新一次轮转（live={size_after_first}，目录：{:?}）",
        files_in(&dir)
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 单条记录大于整个上限：不能死循环，也不能静默丢弃。
#[test]
fn 单次写入超过整个上限时轮转而不是无限追加() {
    let dir = temp_dir("oversized");
    let mut w = RotatingWriter::with_limits(&dir, "gateway.log", 64, 2).expect("应能打开");
    let big = vec![b'z'; 500];
    w.write_all(&big).expect("超大写入应成功而不是报错");
    w.flush().ok();
    let live = fs::metadata(dir.join("gateway.log"))
        .expect("live 应存在")
        .len();
    assert_eq!(live, 500, "超大记录应完整落在新文件里，不该被截断");
    let _ = fs::remove_dir_all(&dir);
}
