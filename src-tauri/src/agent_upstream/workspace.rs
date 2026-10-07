//! 任务卡二 A8：产物的**落盘边界**。
//!
//! ## 卡片原文
//!
//! 「产物落盘边界（默认写到 `runtimes\<id>\workspace`，**拒绝写出该目录**，
//! 除非显式配置输出根）」
//!
//! ## 为什么单独一个模块、且先写它
//!
//! 这是**安全相关**的一段：A8 让 agent 真的去读写文件，
//! 而「它到底能写到哪」只由这一个函数决定。把它写成纯函数、
//! 先于路由与执行器落地，是因为：
//!
//! - 纯函数可以**穷举边界用例**（`..` 穿越、绝对路径、软链接前缀无关的大小写…），
//!   而混在异步分发里就只能靠端到端碰运气；
//! - 它是**唯一**该做路径判断的地方 —— 分散到各调用点必然有一处漏掉。
//!
//! ## 判定用「规范化后的前缀比较」，不用字符串 `starts_with`
//!
//! 字符串比较会把 `C:\ws-evil` 判成在 `C:\ws` 之内（前缀相同），
//! 也会把 `C:\ws\..\..\Windows` 判成在内。两者都是真实的越界。
//! 所以先 `canonicalize` 再比**路径组件**，而不是比字符。

use std::path::{Component, Path, PathBuf};

/// 一个运行时的产物根。所有由 agent 产出的文件都必须落在它里面。
#[derive(Debug, Clone)]
pub struct WorkspaceRoot {
    /// **已规范化**的绝对路径。构造时就算好 ——
    /// 每次判断再算一遍既慢又可能因为期间目录被改而结果不一致。
    canonical: PathBuf,
}

impl WorkspaceRoot {
    /// 建立产物根。目录不存在时会创建。
    ///
    /// 失败时返回错误而**不是退化成当前目录** —— 「悄悄把产物写在
    /// 用户的项目目录里」比「起不来」危险得多。
    pub fn new(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .map_err(|e| format!("创建产物根失败（{}）：{e}", path.display()))?;
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("规范化产物根失败（{}）：{e}", path.display()))?;
        Ok(Self { canonical })
    }

    pub fn as_path(&self) -> &Path {
        &self.canonical
    }

    /// 目标路径是否**落在产物根之内**。
    ///
    /// ## 对不存在的路径也要能判断
    ///
    /// `canonicalize` 要求路径存在，而我们要判的恰恰是**即将被创建**的文件。
    /// 所以做法是：找到**已存在的最深祖先**并规范化它，
    /// 再把剩下还没存在的组件逐个拼上去（`..` 就地消解）。
    /// 这样 `root/new/../../etc` 也会被正确判成越界。
    pub fn contains(&self, candidate: impl AsRef<Path>) -> bool {
        let candidate = candidate.as_ref();
        // **先按产物根拼成绝对路径**。不拼的话，像 `"a.txt"` 这种单组件
        // 相对路径在 `normalize` 里往上找不到任何已存在的祖先，
        // 会直接被判成越界 —— 而它显然在根之内。
        // （agent 给的路径多半就是相对的，它的 cwd 就是产物根。）
        let absolute = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.canonical.join(candidate)
        };
        let normalized = match normalize(&absolute, &self.canonical) {
            Some(p) => p,
            None => return false,
        };

        // 逐组件比较，**不用字符串 starts_with**
        let mut target = normalized.components();
        for expected in self.canonical.components() {
            match target.next() {
                Some(actual) if same_component(expected, actual) => {}
                _ => return false,
            }
        }
        // 根自身也算「之内」（调用方通常传文件，但传根本身不该报错）
        true
    }

    /// 把候选路径规范化成绝对路径；无法安全规范化时返回 `None`。
    ///
    /// `relative_to` 是产物根 —— 相对路径按它解析（agent 给的多半是
    /// 相对路径，而它的 cwd 就是产物根）。
    pub fn resolve(&self, candidate: impl AsRef<Path>) -> Option<PathBuf> {
        let candidate = candidate.as_ref();
        let absolute = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.canonical.join(candidate)
        };
        normalize(&absolute, &self.canonical)
    }
}

/// 规范化：把 `..` / `.` 消解掉，并对**已存在的最深祖先**做
/// `canonicalize`（处理软链接与大小写）。
fn normalize(path: &Path, _root: &Path) -> Option<PathBuf> {
    // 找出最深的已存在祖先
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() {
            break;
        }
        let name = existing.file_name()?.to_os_string();
        tail.push(name);
        if !existing.pop() {
            // 一直退到空 —— 说明整个路径都不存在（连盘符都没有）
            return None;
        }
    }
    let mut result = existing.canonicalize().ok()?;
    // 从深到浅拼回不存在的部分，边拼边消解 `..`
    for name in tail.iter().rev() {
        if name == ".." {
            if !result.pop() {
                return None;
            }
            continue;
        }
        if name == "." {
            continue;
        }
        result.push(name);
    }
    Some(result)
}

/// 路径组件相等。Windows 上盘符与文件名**不区分大小写**。
fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    let (a, b) = (
        a.as_os_str().to_string_lossy(),
        b.as_os_str().to_string_lossy(),
    );
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> WorkspaceRoot {
        let p = std::env::temp_dir().join(format!("llmgw-ws-test-{name}"));
        WorkspaceRoot::new(&p).expect("建产物根")
    }

    #[test]
    fn 根本身与其中的文件都算之内() {
        let r = root("basic");
        assert!(r.contains(r.as_path()));
        assert!(r.contains(r.as_path().join("a.txt")));
        assert!(r.contains(r.as_path().join("sub").join("deep").join("b.txt")));
    }

    /// **这条是本模块存在的理由**：`..` 穿越必须被拒。
    #[test]
    fn 上跳穿越必须被拒() {
        let r = root("traverse");
        assert!(!r.contains(r.as_path().join("..")), "`..` 应当越界");
        assert!(
            !r.contains(r.as_path().join("sub").join("..").join("..")),
            "`sub/../..` 应当越界"
        );
        assert!(
            !r.contains(r.as_path().join("..").join("evil.txt")),
            "`../evil.txt` 应当越界 —— 这是最常见的写法"
        );
    }

    /// 前缀相同但**不是**子目录 —— 字符串 `starts_with` 会在这里判错。
    #[test]
    fn 前缀相同的兄弟目录必须被拒() {
        let r = root("prefix");
        // 造一个名字是产物根名 + 后缀的兄弟目录
        let sibling = std::path::PathBuf::from(format!("{}-evil", r.as_path().display()));
        assert!(
            !r.contains(sibling.join("x.txt")),
            "`<root>-evil/x.txt` 与 `<root>` 前缀相同但不在其内 —— \
             用字符串 starts_with 的实现会在这里放行"
        );
    }

    #[test]
    fn 绝对路径越界必须被拒() {
        let r = root("absolute");
        #[cfg(windows)]
        let outside = PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts");
        #[cfg(not(windows))]
        let outside = PathBuf::from("/etc/passwd");
        assert!(!r.contains(&outside), "绝对路径越界必须被拒");
        assert!(!r.contains(&outside), "同一判断重复调用结果必须一致");
    }

    #[test]
    fn 相对路径按产物根解析() {
        let r = root("relative");
        // 相对路径在根之内
        assert!(r.contains("a.txt"));
        assert!(r.contains("sub/b.txt"));
        // 相对路径上跳也一样要被拒
        assert!(!r.contains("../a.txt"));
        // resolve 给出的绝对路径确实在根下面
        let resolved = r.resolve("sub/b.txt").expect("应当能规范化");
        assert!(resolved.starts_with(r.as_path()));
    }

    #[test]
    fn 不存在的路径也能判断() {
        // 这是关键：要判的恰恰是**即将被创建**的文件
        let r = root("nonexistent");
        assert!(r.contains("还不存在的目录/还不存在的文件.txt"));
        assert!(!r.contains("../../还不存在.txt"));
    }

    #[test]
    fn 点号组件会被消解() {
        let r = root("dots");
        assert!(r.contains("./a.txt"));
        assert!(r.contains("sub/././b.txt"));
    }

    #[test]
    fn 建不出来的产物根要报错而不是退化成当前目录() {
        // 用一个**文件**当路径 —— create_dir_all 必然失败。
        // 这条守的是「不许悄悄把产物写到别处」。
        let file = std::env::temp_dir().join("llmgw-ws-test-not-a-dir.txt");
        std::fs::write(&file, b"x").expect("造一个文件");
        let err = WorkspaceRoot::new(&file).expect_err("拿文件当目录必须报错");
        assert!(err.contains("创建产物根失败"), "错误要说清是哪一步：{err}");
    }
}
