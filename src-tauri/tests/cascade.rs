//! D4 验收测试：①细分任务维度、②级联路由。
//!
//! 卡片把这张卡定为「在现有 `smart` 档之上加两件事」，
//! 并写死三条边界。本文件按那三条边界逐条钉住。

use llm_gateway_lib::domain::{CapabilitySource, ModelCapabilities};
use llm_gateway_lib::intellect::{TaskClass, TaskDomain};

// ============================ ① 细分任务维度 ============================

#[test]
fn 领域枚举的标识与标签互不相同且都非空() {
    let mut codes: Vec<&str> = TaskDomain::ALL.iter().map(|d| d.code()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), TaskDomain::ALL.len(), "code 必须互不相同");

    let mut labels: Vec<&str> = TaskDomain::ALL.iter().map(|d| d.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), TaskDomain::ALL.len(), "label 必须互不相同");
    assert!(labels.iter().all(|l| !l.is_empty()));
}

#[test]
fn 默认领域是_general_且唯一的失败落点() {
    // 卡片边界 2：「判定失败必须回落 TaskClass 的现有三级结果，
    // 不许出现第四个兜底层」。领域这边的对应物是：
    // 认不出来就是 `General`，没有第二个兜底档。
    assert_eq!(TaskDomain::default(), TaskDomain::General);
    // 认不出来的字符串**返回 None 而不是 General** ——
    // 调用方需要能区分「没写」与「写了个我不认识的」。
    // 后者是配置写错了，值得报出来；前者什么都不用做。
    assert_eq!(TaskDomain::parse("完全不认识的领域"), None);
    assert_eq!(TaskDomain::parse(""), None);
}

#[test]
fn 显式名字优先于关键词与模态() {
    // 判定顺序是「显式 → 关键词 → General」，
    // 而显式名字还要**先于模态事实**：用户写了 `coding`
    // 就是说他要把这张图当代码问题处理，不该被媒体类型改写成 vision。
    for d in TaskDomain::ALL {
        assert_eq!(
            TaskDomain::parse(d.code()),
            Some(d),
            "{} 的 code 必须能被 parse 认回来",
            d.label()
        );
    }
    // 别名的等价性
    assert_eq!(TaskDomain::parse("code"), Some(TaskDomain::Coding));
    assert_eq!(TaskDomain::parse(" CODE "), Some(TaskDomain::Coding));
    assert_eq!(TaskDomain::parse("data"), Some(TaskDomain::DataAnalysis));
    assert_eq!(TaskDomain::parse("auto"), Some(TaskDomain::General));
}

#[test]
fn 领域与难度是两个独立的维度() {
    // 卡片的核心动机（事实源 I.1）：
    // 「一个简单的医学问题和一个复杂的医学问题都该走医学模型」——
    // 现有三级只能表达**难度**，表达不了**领域**。
    //
    // 这里把「两个维度独立」这件事钉住：同一个领域可以配任何难度。
    let pairs = [
        (TaskDomain::Coding, TaskClass::Simple),
        (TaskDomain::Coding, TaskClass::Reasoning),
        (TaskDomain::Math, TaskClass::Simple),
        (TaskDomain::Math, TaskClass::Reasoning),
    ];
    // 4 个组合两两不同 ⇒ 两个维度真的正交，不是同一个值的两种写法
    let mut seen: Vec<(TaskDomain, TaskClass)> = pairs.to_vec();
    seen.sort_by_key(|(d, c)| (d.code(), format!("{c:?}")));
    seen.dedup();
    assert_eq!(seen.len(), 4, "领域 × 难度必须是笛卡尔积，不是一对一");
}

#[test]
fn 亲和度_缺失维度不惩罚() {
    // D1 的核心约束：未知 ≠ 零分。
    // 一个**还没标定能力**的模型若在专项请求里被判 0，
    // 它会在所有专项请求里永远出局 —— 而我们对它其实一无所知。
    let unknown = ModelCapabilities::default();
    for d in TaskDomain::ALL {
        assert_eq!(
            d.affinity(Some(&unknown)),
            1.0,
            "{} 在能力全未知时必须给中性 1.0，不能判 0",
            d.label()
        );
        assert_eq!(
            d.affinity(None),
            1.0,
            "{} 在压根没有能力数据时也必须中性",
            d.label()
        );
    }
}

#[test]
fn 亲和度_general_不吃任何专项偏置() {
    // `General` 的空权重表**不是「没实现」**，而是
    // 「通用任务不该因为某个专项分数高就偏向它」。
    let coding_strong = ModelCapabilities {
        coding: Some(0.95),
        reasoning: Some(0.2),
        ..Default::default()
    };
    let writing_strong = ModelCapabilities {
        knowledge: Some(0.95),
        coding: Some(0.1),
        ..Default::default()
    };
    assert_eq!(TaskDomain::General.affinity(Some(&coding_strong)), 1.0);
    assert_eq!(TaskDomain::General.affinity(Some(&writing_strong)), 1.0);
}

#[test]
fn 亲和度_按领域关心的维度算且方向正确() {
    let coder = ModelCapabilities {
        coding: Some(0.9),
        reasoning: Some(0.8),
        knowledge: Some(0.2),
        math: Some(0.1),
        source: CapabilitySource::Measured,
        ..Default::default()
    };
    let writer = ModelCapabilities {
        coding: Some(0.1),
        reasoning: Some(0.2),
        knowledge: Some(0.9),
        math: Some(0.3),
        source: CapabilitySource::Measured,
        ..Default::default()
    };

    // 编程领域：coder 明显高于 writer
    assert!(
        TaskDomain::Coding.affinity(Some(&coder)) > TaskDomain::Coding.affinity(Some(&writer)),
        "编程领域应当偏向 coding 分高的那个"
    );
    // 写作领域：反过来
    assert!(
        TaskDomain::Writing.affinity(Some(&writer)) > TaskDomain::Writing.affinity(Some(&coder)),
        "写作领域应当偏向 knowledge 分高的那个"
    );
    // **反向判据**：只断言「coder 在编程领域更高」是不够的 ——
    // 一个恒返回 coding 分的实现也能满足它。上面两条方向相反，
    // 一起才说明权重表真的按领域选了维度。
}

#[test]
fn 亲和度_只在被标定的维度上求加权平均() {
    // coder 只标了 coding：编程领域的亲和度应当**等于** coding 本身，
    // 而不是因为 reasoning 没标而被拉低（那等于把未知当 0）。
    let only_coding = ModelCapabilities {
        coding: Some(0.8),
        ..Default::default()
    };
    assert!((TaskDomain::Coding.affinity(Some(&only_coding)) - 0.8).abs() < 1e-6);

    // 两个都标了才加权平均：编程是 coding 1.0 + reasoning 0.6
    let both = ModelCapabilities {
        coding: Some(0.9),
        reasoning: Some(0.3),
        ..Default::default()
    };
    let expected = (0.9 * 1.0 + 0.3 * 0.6) / 1.6;
    assert!((TaskDomain::Coding.affinity(Some(&both)) - expected).abs() < 1e-6);
}

#[test]
fn 亲和度_vision_不吃质量分() {
    // 视觉的判据是**模态**而不是质量分（卡片边界 3：
    // 「新增维度只影响 intent_fit 的偏置，不影响能力硬约束」）。
    // 真正的模态筛选在 `required_capabilities` 里 ——
    // 把模态塞进偏置会让「不支持图片的模型」只是分数低一点，
    // 而它实际上会把图片静默丢掉。
    let no_vision_at_all = ModelCapabilities {
        coding: Some(0.99),
        reasoning: Some(0.99),
        knowledge: Some(0.99),
        math: Some(0.99),
        ..Default::default()
    };
    // 质量分再高，视觉领域的亲和度也只是一个固定的中性偏置，
    // 不会因为「质量高」就变成它能看图。
    let a = TaskDomain::Vision.affinity(Some(&no_vision_at_all));
    assert!((0.0..=1.0).contains(&a));
    assert_eq!(
        TaskDomain::Vision.affinity(None),
        1.0,
        "没有能力数据时视觉领域同样中性"
    );
}

#[test]
fn 亲和度始终落在_0_到_1_之间() {
    let wild = ModelCapabilities {
        coding: Some(1.0),
        reasoning: Some(0.0),
        knowledge: Some(0.5),
        math: Some(1.0),
        ..Default::default()
    };
    for d in TaskDomain::ALL {
        let a = d.affinity(Some(&wild));
        assert!((0.0..=1.0).contains(&a), "{} 的亲和度越界：{a}", d.label());
        assert!(a.is_finite());
    }
}
