//! 线上格式的两件小工具：字符串字面量联合的枚举宏、`?: T | null` 的三态字段。

/// 把 TS 的字符串字面量联合镜像成 Rust 枚举。
///
/// 每个变体**显式**写出线上的字面量，不靠 `rename_all` 推断——shared 里
/// `multiProjectView`、`this_week`、`cherry-pick` 几种大小写风格混着用，推断错一个
/// 就是一次静默的反序列化失败，而这种错只有真服务端的 fixture 才测得出来。
///
/// 两种形态：
/// - `pub enum X { … }`：闭集，认不出的值直接报错。用于客户端写出去的值，以及
///   结构性的判别（local / ssh 这种，TS 注释里明说不会再加值）。
/// - `pub enum X open { … }`：多一个 `Unknown` 兜底（`#[serde(other)]`）。用于服务端
///   将来可能扩展的状态 / 原因类：新服务端多一个原因值，老客户端不该因此让整张
///   会话列表反序列化失败。`Unknown` 序列化写回 `"unknown"`——原值已经丢了，
///   客户端也不该把它再发回服务端。
///
/// 两种形态都生成 `as_str()`（线上字面量，拼 query 参数用）、`from_wire()`（只认
/// 已知值，`Unknown` 不算）、`ALL`（已知值全集，按 TS 的书写顺序）与 `Display`。
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(::serde::Serialize, ::serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* #[serde(rename = $wire)] $variant, )+
        }

        impl $name {
            /// 已知值全集，按 TS 的书写顺序。
            pub const ALL: &'static [$name] = &[$( $name::$variant, )+];

            /// 线上的字面量。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $wire, )+
                }
            }

            /// 按线上字面量取值；认不出返回 `None`。
            pub fn from_wire(s: &str) -> Option<Self> {
                match s {
                    $( $wire => Some($name::$variant), )+
                    _ => None,
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
    (
        $(#[$meta:meta])*
        pub enum $name:ident open {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(::serde::Serialize, ::serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* #[serde(rename = $wire)] $variant, )+
            /// 本版本不认识的值（服务端比客户端新）。只在反序列化时出现。
            #[serde(rename = "unknown", other)]
            Unknown,
        }

        impl $name {
            /// 已知值全集，按 TS 的书写顺序；不含 `Unknown`。
            pub const ALL: &'static [$name] = &[$( $name::$variant, )+];

            /// 线上的字面量；`Unknown` 是 `"unknown"`。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $wire, )+
                    $name::Unknown => "unknown",
                }
            }

            /// 按线上字面量取值；认不出返回 `None`（不会返回 `Unknown`）。
            pub fn from_wire(s: &str) -> Option<Self> {
                match s {
                    $( $wire => Some($name::$variant), )+
                    _ => None,
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub(crate) use wire_enum;

/// `?: T | null` 的三态字段：缺省 / null / 有值，对应 `Option<Option<T>>` 的
/// `None` / `Some(None)` / `Some(Some(v))`。
///
/// serde 默认把 null 与缺省都读成外层 `None`，三态就塌成两态了。这里把 null 读成
/// `Some(None)`；字段上再配 `default` 与 `skip_serializing_if = "Option::is_none"`，
/// 写回时缺省仍缺省、null 仍是 null。
pub(crate) mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}
