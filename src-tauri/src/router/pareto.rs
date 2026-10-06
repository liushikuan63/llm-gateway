//! D5 ②：质量 × 速度 × 价格的三维 Pareto 前沿。
//!
//! ## 为什么用 Pareto 而不是再加一个总分
//!
//! 卡片原文：乘法打分里「权重设为 0」正好能表达「我只在乎价格」，
//! **三者取舍无法用一个标量表达**。Pareto 把这三种取舍显式呈现给用户，
//! 比猜权重更诚实。
//!
//! ## 缺失值怎么处理（本模块最要紧的一条）
//!
//! D1 的核心约束是「未知 ≠ 零分」。这里延续它：
//! **某一维缺失时，两个候选在这一维上不可比** ——
//! A 既不能因为「B 的价格未知」就宣称自己更便宜，
//! 也不能因此被判为更贵。
//!
//! 这条不是保守，是唯一说得通的做法：
//! 把未知当最差 ⇒ 一个还没标定价格的模型会被所有候选支配、直接出局；
//! 把未知当最好 ⇒ 它反过来支配所有人。两种都会静默产生错误的结论。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 前沿视图里的一个候选。
///
/// 三个目标的方向**都统一成「越大越好」**：价格在写入时就取负值
/// 或倒数由调用方决定 —— 本模块提供一个 [`ParetoPoint::from_price`]
/// 做这件事，免得每个调用方各写一遍、且写反一个符号没有任何报错。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParetoPoint {
    /// 稳定标识（`provider/model`）。
    pub id: String,
    /// 质量：综合能力分，越大越好。
    pub quality: Option<f32>,
    /// 速度：实测吞吐 tok/s，越大越好。
    pub speed: Option<f32>,
    /// 价格：**已取倒数**，越大越便宜。用 [`Self::from_price`] 构造。
    pub cheapness: Option<f32>,
}

impl ParetoPoint {
    /// 用**原始单价**构造（越大越贵的那种）。
    ///
    /// 内部取倒数把方向统一成「越大越好」。
    /// 单价 `<= 0` 或非有限时 `cheapness` 为 `None` ——
    /// **不是无穷大**。免费模型与「价格未知」是两件事，
    /// 而免费的模型通常在别处有代价（自建、限流、质量差），
    /// 让它在价格维上无限占优会掩盖那些代价。
    pub fn from_price(
        id: impl Into<String>,
        quality: Option<f32>,
        speed: Option<f32>,
        unit_price: Option<f32>,
    ) -> Self {
        let cheapness = unit_price.and_then(|p| {
            if p.is_finite() && p > 0.0 {
                Some(1.0 / p)
            } else {
                None
            }
        });
        Self {
            id: id.into(),
            quality,
            speed,
            cheapness,
        }
    }

    /// 三个目标，顺序固定 `[quality, speed, cheapness]`。
    fn objectives(&self) -> [Option<f32>; 3] {
        [self.quality, self.speed, self.cheapness]
    }
}

/// 支配关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dominance {
    /// A 支配 B：每一维都不差，且至少一维严格更好。
    Dominates,
    /// 互不支配（含「某一维缺数据因而不可能判定支配」的情形）。
    Incomparable,
}

/// 前沿视图的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ParetoFront {
    /// 非支配解，按输入顺序。**顺序刻意不重排** ——
    /// 前沿的排列应当由调用方按它关心的那一维排，
    /// 在这里排一次等于替用户做了一个他没提的取舍。
    pub front: Vec<String>,
    /// 被支配的候选 → 支配它的那些 id（可能多个）。
    ///
    /// 卡片要求：「被支配的候选要标出来并**说明被谁支配**。
    /// 只画前沿不标支配关系，用户会以为没上榜的模型是数据缺失。」
    pub dominated_by: BTreeMap<String, Vec<String>>,
}

impl ParetoFront {
    pub fn is_on_front(&self, id: &str) -> bool {
        self.front.iter().any(|x| x == id)
    }

    /// 某个候选被谁支配。空表示它在前沿上。
    pub fn dominators(&self, id: &str) -> &[String] {
        self.dominated_by.get(id).map_or(&[], Vec::as_slice)
    }
}

/// 判定 A 是否支配 B。
///
/// - 每一维：**两边都有值**时才比较；有一边缺失 ⇒ 这一维**不构成支配依据**
/// - 至少一维严格更好（且那一维两边都有值）才算支配
///
/// 两个候选 id 相同视为不可比（自己不该支配自己）。
pub fn dominance(a: &ParetoPoint, b: &ParetoPoint) -> Dominance {
    if a.id == b.id {
        return Dominance::Incomparable;
    }
    let (ao, bo) = (a.objectives(), b.objectives());
    let mut strictly_better = false;
    for i in 0..3 {
        // 用 `if let` 而不是 `match`：这里只有一个有意义的分支 ——
        // **任一维缺数据就跳过这一维**（缺数据既不是更好也不是更差，
        // 不构成支配依据，但也不否掉其他维给出的结论）。
        // 写成 `match` 会让 `_ => {}` 看起来像「还有别的分支要处理」。
        if let (Some(x), Some(y)) = (ao[i], bo[i]) {
            if !x.is_finite() || !y.is_finite() {
                // 坏数据不参与支配判定
                return Dominance::Incomparable;
            }
            if x < y {
                // A 在这一维更差 ⇒ 不可能支配
                return Dominance::Incomparable;
            }
            if x > y {
                strictly_better = true;
            }
        }
    }
    if strictly_better {
        Dominance::Dominates
    } else {
        // 三维全等、或全部缺数据 ⇒ 没有「至少一维严格更好」
        Dominance::Incomparable
    }
}

/// 算非支配解集。
///
/// 复杂度 O(n²)：候选数是**一次请求的候选**（十几到几十），
/// 不值得为它上更复杂的算法；而 O(n²) 的直白写法更容易看出
/// 「缺失值怎么处理」这件事被做对了没有。
pub fn pareto_front(points: &[ParetoPoint]) -> ParetoFront {
    let mut front = Vec::new();
    let mut dominated_by: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (i, candidate) in points.iter().enumerate() {
        // 用**下标显式循环**而不是 `iter().filter(|other| ...)`：
        // 后者里 `other: &&ParetoPoint` 与 `candidate: &ParetoPoint` 的
        // 自动解引用看起来等价，但第一版就出了
        // 「`dominance(&a,&c)` 单独测是 Dominates、放进这里却不算数」的现象。
        // 显式取值不留这个疑点。
        let mut dominators = Vec::new();
        for (j, other) in points.iter().enumerate() {
            if i == j {
                continue; // 自己不该支配自己（`dominance` 也会挡，双保险）
            }
            if dominance(other, candidate) == Dominance::Dominates {
                dominators.push(other.id.clone());
            }
        }
        if dominators.is_empty() {
            front.push(candidate.id.clone());
        } else {
            dominated_by.insert(candidate.id.clone(), dominators);
        }
    }
    ParetoFront {
        front,
        dominated_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, q: Option<f32>, s: Option<f32>, c: Option<f32>) -> ParetoPoint {
        ParetoPoint {
            id: id.into(),
            quality: q,
            speed: s,
            cheapness: c,
        }
    }

    #[test]
    fn 单一最优模型_前沿只含它自己() {
        // 卡片点名的第一条用例。
        let a = p("a", Some(0.9), Some(100.0), Some(0.1));
        let b = p("b", Some(0.5), Some(50.0), Some(0.01));
        let c = p("c", Some(0.7), Some(80.0), Some(0.05));
        // 先单独验支配关系，报错时能直接指出是哪一对
        assert_eq!(dominance(&a, &b), Dominance::Dominates, "a 应当支配 b");
        assert_eq!(dominance(&a, &c), Dominance::Dominates, "a 应当支配 c");
        assert_eq!(dominance(&c, &a), Dominance::Incomparable);
        let front = pareto_front(&[a, b, c]);
        assert_eq!(front.front, vec!["a".to_string()], "只有 a 在前沿上");
        assert!(front.is_on_front("a"));
        assert!(!front.is_on_front("b"));
        assert!(!front.is_on_front("c"));

        // **b 被 a 与 c 同时支配** —— `c`=(0.7, 80, 0.05) 在三维上都优于
        // `b`=(0.5, 50, 0.01)。第一版我写成「b 只被 a 支配」，
        // 被用例抓到（`left: ["a","c"]`）。
        // 支配者按**输入顺序**给出，所以是 ["a", "c"] 而不是 ["c", "a"]。
        assert_eq!(front.dominators("b"), &["a".to_string(), "c".to_string()]);
        assert_eq!(front.dominators("c"), &["a".to_string()], "c 只被 a 支配");
    }

    #[test]
    fn 三个各有所长时全部在前沿上() {
        // 贵但强、快但弱、便宜但慢 —— 互不支配
        let strong = p("strong", Some(0.95), Some(20.0), Some(0.01));
        let fast = p("fast", Some(0.5), Some(200.0), Some(0.02));
        let cheap = p("cheap", Some(0.4), Some(30.0), Some(1.0));
        let front = pareto_front(&[strong, fast, cheap]);
        assert_eq!(front.front.len(), 3, "三个各有所长时谁都不该被支配");
        assert!(front.dominated_by.is_empty());
    }

    #[test]
    fn 同一维更差就一定被支配_只要其余维不更好() {
        let a = p("a", Some(0.9), Some(100.0), Some(0.5));
        let b = p("b", Some(0.9), Some(100.0), Some(0.4));
        let front = pareto_front(&[a, b]);
        assert_eq!(front.front, vec!["a".to_string()], "只有价格差也构成支配");
        assert_eq!(front.dominators("b"), &["a".to_string()]);
    }

    #[test]
    fn 三维全等时互不支配() {
        // 没有「至少一维严格更好」。若写成「不差于即支配」，
        // 两个完全一样的候选会互相支配、双双掉出前沿。
        let a = p("a", Some(0.8), Some(50.0), Some(0.2));
        let b = p("b", Some(0.8), Some(50.0), Some(0.2));
        let front = pareto_front(&[a, b]);
        assert_eq!(front.front.len(), 2);
        assert!(front.dominated_by.is_empty());
    }

    #[test]
    fn 缺失值既不构成更好也不构成更差() {
        // 本模块最要紧的一条。A 有价格、B 没有：
        // B 不能因为「价格未知」被判为更贵（那会让它出局），
        // A 也不能因此宣称自己更便宜（那是拿未知当已知）。
        let known = p("known", Some(0.8), Some(50.0), Some(0.2));
        let unknown = p("unknown", Some(0.8), Some(50.0), None);
        assert_eq!(dominance(&known, &unknown), Dominance::Incomparable);
        assert_eq!(dominance(&unknown, &known), Dominance::Incomparable);
        let front = pareto_front(&[known, unknown]);
        assert_eq!(front.front.len(), 2, "缺一维数据的候选不该被踢出前沿");
    }

    #[test]
    fn 缺失值不阻挡其他维给出的支配() {
        // B 在**两边都有值**的两维上都更差 ⇒ 仍被支配。
        // 缺数据的那一维不参与，但它不该让整个判定失效 ——
        // 那会让「有一个维度没数据」变成免死金牌。
        let a = p("a", Some(0.9), Some(100.0), Some(0.2));
        let b = p("b", Some(0.5), Some(50.0), None);
        assert_eq!(dominance(&a, &b), Dominance::Dominates);
        assert_eq!(dominance(&b, &a), Dominance::Incomparable);
        let front = pareto_front(&[a, b]);
        assert_eq!(front.front, vec!["a".to_string()]);
    }

    #[test]
    fn 全部维度都缺失时互不支配() {
        let a = p("a", None, None, None);
        let b = p("b", None, None, None);
        assert_eq!(dominance(&a, &b), Dominance::Incomparable);
        let front = pareto_front(&[a, b]);
        assert_eq!(front.front.len(), 2, "什么都不知道时不该判定谁支配谁");
    }

    #[test]
    fn 自己不会支配自己() {
        let a = p("a", Some(0.9), Some(100.0), Some(0.5));
        assert_eq!(dominance(&a, &a.clone()), Dominance::Incomparable);
    }

    #[test]
    fn 一个候选可以同时被多个支配() {
        let weak = p("weak", Some(0.2), Some(10.0), Some(0.01));
        let strong = p("strong", Some(0.9), Some(100.0), Some(0.5));
        let mid = p("mid", Some(0.6), Some(50.0), Some(0.2));
        // **构造要当心**：`strong` 在三维上都优于 `mid`（0.9>0.6、100>50、0.5>0.2），
        // 所以它**确实支配 mid** —— 第一版我写成「两者互不支配」，
        // 被用例自己抓到（front.len() 期望 2、实际 1）。
        assert_eq!(dominance(&strong, &mid), Dominance::Dominates);
        assert_eq!(dominance(&mid, &strong), Dominance::Incomparable);

        let front = pareto_front(&[weak, strong, mid]);
        let doms = front.dominators("weak");
        assert_eq!(doms.len(), 2, "strong 与 mid 都支配它：{doms:?}");
        assert!(doms.contains(&"strong".to_string()));
        assert!(doms.contains(&"mid".to_string()));
        // mid 被 strong 支配，所以前沿只剩 strong
        assert_eq!(front.front, vec!["strong".to_string()]);
    }

    #[test]
    fn from_price_把方向统一成越大越便宜() {
        let cheap = ParetoPoint::from_price("cheap", Some(0.5), Some(10.0), Some(1.0));
        let dear = ParetoPoint::from_price("dear", Some(0.5), Some(10.0), Some(100.0));
        assert!(cheap.cheapness.unwrap() > dear.cheapness.unwrap());
        // 于是「便宜」这一维在支配判定里方向正确
        assert_eq!(dominance(&cheap, &dear), Dominance::Dominates);
    }

    #[test]
    fn from_price_对免费与坏数据都给_none() {
        // 免费与「价格未知」是两件事，但在**支配判定**里都只能是「不可比」。
        // 让免费拿到无穷大的 cheapness 的话，它会支配所有候选 ——
        // 而免费的模型通常在别处有代价（自建、限流、质量差）。
        for bad in [
            Some(0.0f32),
            Some(-1.0),
            Some(f32::NAN),
            Some(f32::INFINITY),
            None,
        ] {
            let point = ParetoPoint::from_price("x", Some(0.5), Some(10.0), bad);
            assert_eq!(point.cheapness, None, "单价 {bad:?} 不该被当成「便宜」");
        }
    }

    #[test]
    fn 坏数据不参与支配判定() {
        let bad = p("bad", Some(f32::NAN), Some(50.0), Some(0.2));
        let good = p("good", Some(0.8), Some(50.0), Some(0.2));
        assert_eq!(dominance(&good, &bad), Dominance::Incomparable);
        assert_eq!(dominance(&bad, &good), Dominance::Incomparable);
        // 两者都留在前沿 —— 分歧无法判定时不替用户做决定
        let front = pareto_front(&[good, bad]);
        assert_eq!(front.front.len(), 2);
    }

    #[test]
    fn 前沿保持输入顺序不重排() {
        // 排列顺序是调用方按「他关心的那一维」决定的，
        // 在这里排一次等于替用户做了一个他没提的取舍。
        let a = p("z-last", Some(0.9), Some(10.0), Some(1.0));
        let b = p("a-first", Some(0.8), Some(20.0), Some(0.5));
        let front = pareto_front(&[a, b]);
        assert_eq!(
            front.front,
            vec!["z-last".to_string(), "a-first".to_string()],
            "必须保持输入顺序"
        );
    }

    #[test]
    fn 空输入给出空前沿() {
        let front = pareto_front(&[]);
        assert!(front.front.is_empty());
        assert!(front.dominated_by.is_empty());
        assert!(front.dominators("不存在").is_empty());
    }
}
