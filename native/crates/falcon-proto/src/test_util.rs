//! 往返测试的公共断言。

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt::Debug;

/// TS 形状的 JSON 样本 → `T` → JSON，要求与原样本语义相等，并返回解出来的值供
/// 调用方再断言字段。
///
/// "语义相等"只放宽一处：数字按数值比（`42` 与 `42.0` 相等），因为 JS 那边没有
/// 整数 / 浮点之分。键的有无**不放宽**——TS 的可选字段缺省就是缺省，Rust 写出一个
/// `null` 也算错，正是要抓 `skip_serializing_if` 漏写这种事。
#[track_caller]
pub fn roundtrip<T>(json: &str) -> T
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let original: Value = serde_json::from_str(json).expect("样本不是合法 JSON");
    // 走 from_str 而不是 from_value：客户端真实的解码路径就是字符串
    let value: T = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("反序列化失败：{e}\n样本：{original:#}"));
    let back = serde_json::to_value(&value).expect("序列化失败");
    assert!(
        same(&original, &back),
        "往返不一致\n样本：{original:#}\n回写：{back:#}"
    );
    // 写出来的东西自己也得认，而且认出来是同一个值
    let again: T = serde_json::from_value(back).expect("回写的 JSON 解不回来");
    assert_eq!(again, value);
    value
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}
