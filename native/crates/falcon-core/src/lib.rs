//! falcon-core：客户端里与 GPUI 无关的纯逻辑，不依赖 GPUI。
//!
//! 原是 React 前端纯函数模块（`packages/web/src/lib/*.ts`、`store.ts` 的纯函数部分、shared 的
//! `ttlCache.ts`）的移植；React 前端删除后（TS 原文在提交 `9c9d045`）这里就是真相来源。
//! 移植时保留了 TS 里解释"为什么"的注释；与 TS 行为有出入的地方都在就近的注释里写明
//! （多半是 JS 语义在 Rust 里表达不了，比如 localeCompare 的 ICU 排序、Date 的本地时区）。
//!
//! 回归向量：`layout`、`session_title`、`project_tree`、`file_search`、`git_graph` 的用例写成
//! `tests/vectors/*.json`（当年与 TS 共用口径，每条带着对应 TS 用例的名字），`tests/vectors.rs`
//! 读它们；其余模块的单测直接写在模块里。
//!
//! 数字：几何一律 `f64`（与 TS 的 number 逐位同算，取整走 JS 的 `Math.round`），
//! 下标 / 计数用 `usize`，时间戳 `i64` 毫秒。

mod js;

/// 对应 shared 的 appIcon.ts（客户端用得到的部分）+ 原生 Dock 版式的像素运算（ADR 0018）
pub mod app_icon;
/// 对应 React 版的 lib/layout.ts：列式工作区排布（增删移、对账、过滤、拖拽落点、几何、固定列）
pub mod layout;
/// 对应 React 版的 lib/paneKey.ts：窗口 key 约定
pub mod pane_key;
/// 对应 React 版的 lib/projectTree.ts：侧栏「服务器 → 文件夹 → 检出」分组
pub mod project_tree;
/// 对应 React 版的 lib/sessionTitle.ts：会话自动标题
pub mod session_title;
/// 对应 React 版的 lib/hostColor.ts：主机身份色
pub mod host_color;
/// 对应服务端的 sessions/relay_spec.rs：中转的同端口槽位（设置「中转」页的徽标）
pub mod relay;
/// 对应 React 版的 lib/filePath.ts：宿主机路径（posix / windows，不用 std::path）
pub mod file_path;
/// 对应 React 版的 lib/fileTree.ts：改动文件的目录树
pub mod file_tree;
/// 对应 React 版的 lib/fileSearch.ts：⌘P 文件搜索打分
pub mod file_search;
/// 对应 shared 的 px0BasePath：px0 审阅的挂载前缀
pub mod px0;
/// 对应 React 版的 lib/mdLink.ts：Markdown 链接解析
pub mod md_link;
/// 对应 React 版的 lib/gitGraph.ts：提交图泳道
pub mod git_graph;
/// 对应 React 版的 lib/multiPath.ts：多仓库成员的公共父目录
pub mod multi_path;
/// 对应 React 版的 lib/multiDerive.ts：批量派生的预演
pub mod multi_derive;
/// 对应 React 版的 lib/worktreePath.ts：派生目录预览
pub mod worktree_path;
/// 对应 React 版的 lib/panelWidth.ts：左右栏宽度
pub mod panel_width;
/// 对应 React 版的 lib/reason.ts：失败 / 非持久原因的文案 key
pub mod reason;
/// 对应 React 版的 lib/termCanvas.ts：画布滚轮轴向锁定、横向手势翻画布
pub mod term_canvas;
/// 对应 React 版的 lib/termScroll.ts：终端滚动条的滑块几何（ADR 0019）
pub mod term_scroll;
/// 对应 React 版的 lib/pasteImage.ts：粘贴 / 拖入图片的判定
pub mod paste_image;
/// 对应 React 版的 lib/term.ts：终端偏好模型（falcon.term）
pub mod term;
/// 对应 React 版的 lib/shortcuts.ts：快捷键定义表
pub mod shortcuts;
/// 对应 packages/shared/src/ttlCache.ts：TTL + in-flight 合并缓存
pub mod ttl_cache;
/// 原生要 `Send`、浏览器不要的那层约束（MaybeSend / MaybeBoxFuture）
pub mod maybe_send;
/// 对应 React 版的 lib/meegleCache.ts：飞书项目面板缓存
pub mod meegle_cache;
/// 对应 React 版的 lib/meegleContext.ts：复制给 AI 的工作项上下文
pub mod meegle_context;
/// 对应 React 版的 lib/meegleDrag.ts：拖工作项的载荷
pub mod meegle_drag;
/// 对应 React 版的 lib/meegleDrill.ts：面板内下钻栈
pub mod meegle_drill;
/// 对应 React 版的 lib/meegleGroups.ts：分组、本地翻页、过滤
pub mod meegle_groups;
/// 对应 React 版的 lib/meegleKey.ts：工作项显示编号
pub mod meegle_key;
/// 对应 React 版的 store.ts 里的纯逻辑：falcon.workspace 的形状与清洗、selector、状态迁移
pub mod workspace;

/// 已安装本机服务的启动参数：升级时原样带上、"本机"配置按它的端口连
pub mod service_args;
