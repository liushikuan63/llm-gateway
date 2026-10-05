//! 开机自启（用户级注册表 `Run` 项）。
//!
//! ## 为什么不用 `tauri-plugin-autostart`
//!
//! 本项目铁律：不许加依赖，只用 `Cargo.toml` 里已有的 crate。官方插件虽然省事，
//! 但破一次例后面就会破第二次。`windows-sys` 已在依赖里，写 `HKCU` 只要几行。
//!
//! ## 为什么不用 Windows 服务
//!
//! 服务要管理员权限、msi 要特殊打包配置，而且当前架构是「带 GUI 的托盘应用」——
//! 做成服务得把进程拆开，改动量远大于收益。而用户要的是「开机就在后台跑着」，
//! 登录后自启完全满足。
//!
//! ## 写 `HKCU` 而不是 `HKLM`
//!
//! `HKLM` 需要管理员权限，普通用户会直接失败。`HKCU\Software\Microsoft\Windows\
//! CurrentVersion\Run` 是**当前用户**级，无需提权、用户可随时在「任务管理器 → 启动」
//! 里关掉，符合「可开关」的诉求。
//!
//! ## 为什么命令行要带 `--minimized`
//!
//! 自启时如果直接弹主窗口，用户每次开机都要手动关一次。带这个参数时
//! 启动后隐藏主窗口、只留托盘图标（见 `lib.rs` 的 `should_start_hidden`）。

use std::path::Path;

/// 注册表值名。用产品名而不是 exe 名 —— 换了安装位置也不会变成孤儿项。
pub const RUN_VALUE_NAME: &str = "LLMGateway";

/// `Run` 项的路径。分成常量是为了能**逐段断言**：
/// 路径写错不会编译失败，但注册表会静默不生效。
pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// 静默启动参数。
pub const MINIMIZED_FLAG: &str = "--minimized";

/// 当前进程的命令行。抽成函数是为了测试能替换掉它 ——
/// 真实测试环境里 exe 路径因机器而异，写死断言必然在别的机器上失败。
pub fn current_command() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("llm-gateway.exe"));
    let quoted = quote_if_needed(&exe);
    format!("{quoted} {MINIMIZED_FLAG}")
}

/// 路径含空格时加引号。`C:\Program Files\...` 不加引号，注册表会把它
/// 截断成 `C:\Program`，自启时表现为「双击没反应」。
fn quote_if_needed(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if s.contains(' ') && !s.starts_with('"') {
        format!("\"{s}\"")
    } else {
        s
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
    };

    /// 注册表 API 要宽字符串；含中文的路径/命令行也必须走 W 版，
    /// 否则按本地代码页截断（中文系统 ACP 可能是 936）。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 用当前用户的 Run 项做读写。`KEY_SET_VALUE | KEY_QUERY_VALUE` 一次开够，
    /// 免得读和写开两次句柄。
    fn open_key(access: REG_SAM_FLAGS) -> Result<HKEY, String> {
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(RUN_KEY_PATH);
        // SAFETY: `path` 与 `key` 都是有效的本地值；`RegOpenKeyExW` 只写 `key`。
        let code = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                access,
                &mut key,
            )
        };
        match code {
            ERROR_SUCCESS => Ok(key),
            ERROR_FILE_NOT_FOUND => {
                // Run 键默认存在，理论上不该走到这；真出现时给出可操作的提示
                // 而不是裸错误码。
                Err(format!("注册表路径不存在：{RUN_KEY_PATH}（代码 {code}）"))
            }
            other => Err(format!("打开注册表失败，代码 {other}")),
        }
    }

    fn query() -> Result<Option<String>, String> {
        let key = open_key(KEY_QUERY_VALUE)?;
        let name = wide(RUN_VALUE_NAME);
        // 先问容量：问一个不存在的值会得到 ERROR_FILE_NOT_FOUND，
        // 这正是「未开启」的判据。
        let mut bytes: u32 = 0;
        // SAFETY: 句柄来自 open_key 且未关闭；缓冲区指针有效。
        let code = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            // SAFETY: key 有效。
            unsafe { RegCloseKey(key) };
            return Ok(None);
        }
        if code != ERROR_SUCCESS {
            // SAFETY: key 有效。
            unsafe { RegCloseKey(key) };
            return Err(format!("读取注册表失败，代码 {code}"));
        }
        let mut buf = vec![0u16; (bytes as usize / 2) + 1];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: buf 有 size 字节容量；size 是 in-out。
        let code = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        // SAFETY: key 有效。
        unsafe { RegCloseKey(key) };
        if code != ERROR_SUCCESS {
            return Err(format!("读取注册表值失败，代码 {code}"));
        }
        // `size` 是 in-out：进去时是缓冲区容量，回来时是**实际写入的字节数**。
        // 必须按它裁剪，不能只 pop 一个尾零 ——
        // 缓冲区按「字节数/2 + 1」分配，比实际内容多一个元素，
        // 只 pop 一次会在末尾留下 \0，读出来的命令行多一个不可见字符。
        let actual_chars = (size as usize / 2).saturating_sub(1); // 减去终止符
        buf.truncate(actual_chars.min(buf.len()));
        Ok(Some(String::from_utf16_lossy(&buf)))
    }

    /// 写入 Run 项。
    pub fn enable(command: &str) -> Result<(), String> {
        let key = open_key(KEY_SET_VALUE)?;
        let name = wide(RUN_VALUE_NAME);
        let data = wide(command);
        // SAFETY: 句柄有效；name/data 是带 NUL 的宽字符串，长度按字节算。
        let code = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                // reserved 必须是 0。这是 u32 而不是指针 —— 写成
                // std::ptr::null_mut() 会报类型不匹配。
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        // SAFETY: key 有效。
        unsafe { RegCloseKey(key) };
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("写入注册表失败，代码 {code}"))
        }
    }

    /// 删除 Run 项。没开启时**不算错误** —— 关闭开关要幂等，
    /// 否则「重复点关闭」会给用户一个红色报错。
    pub fn disable() -> Result<(), String> {
        let key = open_key(KEY_SET_VALUE)?;
        let name = wide(RUN_VALUE_NAME);
        // SAFETY: 句柄有效；name 是带 NUL 的宽字符串。
        let code = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        // SAFETY: key 有效。
        unsafe { RegCloseKey(key) };
        match code {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            other => Err(format!("删除注册表项失败，代码 {other}")),
        }
    }

    pub fn status() -> Result<Option<String>, String> {
        query()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn enable(_command: &str) -> Result<(), String> {
        Err("开机自启目前只实现了 Windows 注册表版本".into())
    }
    pub fn disable() -> Result<(), String> {
        Err("开机自启目前只实现了 Windows 注册表版本".into())
    }
    pub fn status() -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// 对外：当前是否已开启，以及已注册的命令行。
pub fn status() -> Result<Option<String>, String> {
    imp::status()
}

/// 对外：开启自启。`command` 为空时用当前进程的命令行。
pub fn enable(command: Option<String>) -> Result<(), String> {
    imp::enable(&command.unwrap_or_else(current_command))
}

/// 对外：关闭自启。幂等 —— 未开启时返回 `Ok(())`。
pub fn disable() -> Result<(), String> {
    imp::disable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// 串行化**所有**读写注册表的测试。
    ///
    /// 为什么必须用文件锁而不是 `Mutex`：这些测试改的是**同一个注册表键**，
    /// 而 `cargo test` 会把不同测试二进制并行跑（lib 与 tests/* 各一个进程），
    /// 进程内的 `Mutex` 跨不过去。实测症状是串行 7 全过、并行稳定 1 条失败
    /// ——`读回的值末尾不得有多余字符` 与 `读写状态必须自洽` 互相把键删掉。
    ///
    /// 判据：加锁后**默认并行**也必须全绿；只跑 `--test-threads=1` 是掩盖。
    fn lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        // 锁被 poison 说明上一个持锁的测试 panic 了；这里必须继续往下走，
        // 否则一次失败会让后续所有自启测试永久失败。
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn 注册表路径逐段正确() {
        // 路径写错不会编译失败，但注册表会静默不生效 —— 逐段钉死。
        let parts: Vec<&str> = RUN_KEY_PATH.split('\\').collect();
        assert_eq!(
            parts,
            vec!["Software", "Microsoft", "Windows", "CurrentVersion", "Run"]
        );
    }

    #[test]
    fn 命令行一定带静默标记() {
        let cmd = current_command();
        assert!(
            cmd.contains(MINIMIZED_FLAG),
            "自启命令必须带 {MINIMIZED_FLAG}，否则每次开机都弹面板：{cmd}"
        );
        assert!(
            cmd.to_lowercase().ends_with(&format!(" {MINIMIZED_FLAG}")),
            "静默标记必须在末尾，exe 后还有参数会让它失效：{cmd}"
        );
    }

    #[test]
    fn 含空格的路径必须加引号() {
        let p = Path::new(r"C:\Program Files\LLM Gateway\llm-gateway.exe");
        assert_eq!(quote_if_needed(p), r#""C:\Program Files\LLM Gateway\llm-gateway.exe""#);
        // 不含空格的不能加 —— 多余引号在某些 shell 下会被当成路径的一部分
        let q = Path::new(r"C:\tools\gw.exe");
        assert_eq!(quote_if_needed(q), r"C:\tools\gw.exe");
    }

    #[test]
    fn 已加引号的不能重复加() {
        // 幂等：重复加引号会产生 ""C:\..."" 这种废值
        let p = Path::new(r#"""C:\a b\g.exe""#);
        assert_eq!(quote_if_needed(p), r#"""C:\a b\g.exe""#);
    }

    #[test]
    fn 读写状态必须自洽() {
        let _guard = lock();
        // 真写注册表：先关再开，确认 status 与实际写入的命令一致，
        // 最后恢复原状 —— 测试绝不能把用户的开机自启留在改动后的状态。
        let original = status().expect("读注册表");
        disable().expect("关闭自启");
        assert_eq!(status().expect("关闭后读状态"), None, "关闭后不该读到值");

        enable(None).expect("开启自启");
        let now = status().expect("开启后读状态").expect("应已开启");
        assert_eq!(now, current_command(), "注册表里的命令行要与 current_command 一致");

        disable().expect("恢复：关闭");
        // 恢复原状：原来开着就按原命令写回。
        if let Some(cmd) = original.clone() {
            enable(Some(cmd)).expect("恢复原命令");
        }
        assert_eq!(status().expect("恢复后读状态"), original, "必须恢复到测试前的状态");
    }

    #[test]
    fn 读回的值末尾不得有多余字符() {
        let _guard = lock();
        // 真踩过：`size` 是 in-out 参数，读回后是实际字节数（含终止符）。
        // 只 pop 一次尾零会留下 \0 —— 注册表里的命令行与写入的**看起来一样**
        // （控制台看不见），但比较失败，而且真自启时该字符会进路径解析。
        disable().ok();
        enable(Some(r"C:\a b\gw.exe --minimized".into())).expect("写测试值");
        let got = status().expect("读").expect("应存在");
        assert_eq!(got, r"C:\a b\gw.exe --minimized", "读回值必须与写入值逐字相同");
        assert!(!got.ends_with('\0'), "读回值末尾不得有 NUL");
        assert_eq!(got.chars().count(), r"C:\a b\gw.exe --minimized".chars().count());
        disable().ok();
    }

    #[test]
    fn 重复关闭必须幂等() {
        let _guard = lock();
        // 不幂等的话，用户连点两次「关闭」就会看到红色报错。
        disable().expect("第一次关闭");
        disable().expect("第二次关闭也必须成功");
    }
}
