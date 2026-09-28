//! 共享测试向量（设计文档 §6.3）：`tests/vectors/*.json` 里的每条用例都对应 web 的一句
//! TS 断言（`ts` 字段是 describe > it）。两边实现一旦分叉，至少有一边会红。
//!
//! 每个测试把整份向量跑完再报错，失败信息带 TS 用例名，方便回到 TS 测试对照。

use falcon_core::file_search::{FILE_SEARCH_LIMIT, basename, dirname, filter_files, score_path};
use falcon_core::git_graph::{GraphCommit, GraphRow, layout_commit_graph};
use falcon_core::layout::{
    COLUMN_EDGE_PX, COLUMN_MIN_PX, CANVAS_GAP_PX, CanvasFrames, ColumnLayout, ColumnRect, DropSpot, PANE_MIN_PX, PaneAt,
    Point, Viewport, apply_drop, clamp_column_width, clamp_pane_height, clamp_spot, column, column_width, drop_spot,
    find_pane, insert_column, insert_pane, is_pinned, layout_frames, pane_keys, pane_max_height, pin_edge, pin_pane,
    remove_pane, replace_pane, resolve_spot, set_column_basis, set_pane_basis, sync_columns, unpin_all, visible_columns,
};
use falcon_core::project_tree::{ProjectHead, checkout_label, group_servers};
use falcon_core::session_title::{ProjectShell, session_label, session_title, shell_label};
use falcon_proto::{Project, Session, SshHost};
use serde_json::{Value, json};

fn load(name: &str) -> Value {
    let raw = match name {
        "layout" => include_str!("vectors/layout.json"),
        "sessionTitle" => include_str!("vectors/sessionTitle.json"),
        "projectTree" => include_str!("vectors/projectTree.json"),
        "fileSearch" => include_str!("vectors/fileSearch.json"),
        "gitGraph" => include_str!("vectors/gitGraph.json"),
        other => panic!("没有这份向量：{other}"),
    };
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}.json 不是合法 JSON：{e}"))
}

fn cases<'a>(v: &'a Value, section: &str) -> &'a [Value] {
    v[section].as_array().unwrap_or_else(|| panic!("缺少 {section}")).as_slice()
}

fn ts(case: &Value) -> &str {
    case["ts"].as_str().expect("每条用例都要带 TS 用例名")
}

/// 收集失败，最后一起报：一次就能看到所有分叉
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    fn check<T: PartialEq + std::fmt::Debug>(&mut self, case: &Value, what: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!("[{}] {what}\n    got:  {got:?}\n    want: {want:?}", ts(case)));
        }
    }

    fn finish(self, file: &str, total: usize) {
        assert!(total > 0, "{file}：一条用例都没跑");
        assert!(self.0.is_empty(), "{file}：{} 处与 TS 不一致\n{}", self.0.len(), self.0.join("\n"));
    }
}

/// 数字；`"NaN"` / `"Infinity"` 是 JSON 表示不了的那几个
fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) if s == "NaN" => f64::NAN,
        Value::String(s) if s == "Infinity" => f64::INFINITY,
        Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
        other => other.as_f64().unwrap_or_else(|| panic!("不是数字：{other}")),
    }
}

fn opt_num(v: &Value) -> Option<f64> {
    if v.is_null() { None } else { Some(num(v)) }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().expect("字符串数组").iter().map(|s| s.as_str().expect("字符串").to_string()).collect()
}

// ---------------- layout ----------------

fn cols(groups: &Value) -> Vec<ColumnLayout> {
    groups.as_array().expect("列").iter().map(|g| column(strings(g), None)).collect()
}

fn shape(columns: &[ColumnLayout]) -> Vec<Vec<String>> {
    columns.iter().map(|c| c.panes.iter().map(|p| p.key.clone()).collect()).collect()
}

fn shape_of(v: &Value) -> Vec<Vec<String>> {
    v.as_array().expect("shape").iter().map(strings).collect()
}

fn spot(v: &Value) -> DropSpot {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("落点形状不对：{v}（{e}）"))
}

/// 一步操作；`unchanged` 只对 applyDrop 有意义
fn apply_step(columns: &[ColumnLayout], step: &Value) -> (Vec<ColumnLayout>, Option<bool>) {
    let s = |k: &str| step[k].as_str().unwrap_or_else(|| panic!("{step} 缺 {k}"));
    let u = |k: &str| step[k].as_u64().unwrap_or_else(|| panic!("{step} 缺 {k}")) as usize;
    let out = match step["op"].as_str().expect("op") {
        "removePane" => remove_pane(columns, s("key")),
        "insertPane" => insert_pane(columns, s("key"), PaneAt { col: u("col"), index: u("index") }),
        "insertColumn" => insert_column(columns, s("key"), u("at")),
        "replacePane" => replace_pane(columns, s("from"), s("to")),
        "syncColumns" => sync_columns(columns, &strings(&step["live"])),
        "visibleColumns" => {
            let visible = strings(&step["visible"]);
            visible_columns(columns, |k| visible.iter().any(|v| v == k))
        }
        "pinPane" => pin_pane(columns, s("key")),
        "unpinAll" => unpin_all(columns),
        "setColumnBasis" => {
            let id = match step.get("col") {
                Some(c) => columns[c.as_u64().expect("col") as usize].id.clone(),
                None => s("id").to_string(),
            };
            set_column_basis(columns, &id, opt_num(&step["basis"]))
        }
        "setPaneBasis" => set_pane_basis(columns, s("key"), opt_num(&step["basis"])),
        "applyDrop" => {
            return match apply_drop(columns, s("key"), spot(&step["spot"])) {
                Some(next) => (next, Some(false)),
                None => (columns.to_vec(), Some(true)),
            };
        }
        other => panic!("不认识的 op：{other}"),
    };
    (out, None)
}

fn run_steps(case: &Value) -> (Vec<ColumnLayout>, Vec<ColumnLayout>, Option<bool>) {
    let mut current = cols(&case["columns"]);
    let mut before = current.clone();
    let mut unchanged = None;
    for step in case["steps"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        before = current.clone();
        let (next, u) = apply_step(&current, step);
        current = next;
        unchanged = u;
    }
    (before, current, unchanged)
}

fn pane_basis(columns: &[ColumnLayout], key: &str) -> Option<f64> {
    columns.iter().flat_map(|c| &c.panes).find(|p| p.key == key).and_then(|p| p.basis)
}

#[test]
fn layout_vectors() {
    let v = load("layout");
    let mut f = Failures::default();
    let mut total = 0;

    let c = &v["constants"];
    f.check(c, "COLUMN_MIN_PX", COLUMN_MIN_PX, num(&c["COLUMN_MIN_PX"]));
    f.check(c, "PANE_MIN_PX", PANE_MIN_PX, num(&c["PANE_MIN_PX"]));
    f.check(c, "CANVAS_GAP_PX", CANVAS_GAP_PX, num(&c["CANVAS_GAP_PX"]));
    f.check(c, "COLUMN_EDGE_PX", COLUMN_EDGE_PX, num(&c["COLUMN_EDGE_PX"]));

    for case in cases(&v, "transforms") {
        total += 1;
        let (before, after, unchanged) = run_steps(case);
        let e = &case["expect"];
        let e = e.as_object().expect("expect");
        for (key, want) in e {
            match key.as_str() {
                "shape" => f.check(case, "shape", shape(&after), shape_of(want)),
                "inputShape" => f.check(case, "inputShape", shape(&before), shape_of(want)),
                "unchanged" => f.check(case, "unchanged", unchanged, want.as_bool()),
                "pinEdge" => f.check(case, "pinEdge", pin_edge(&after), want.as_u64().expect("pinEdge") as usize),
                "pinnedCount" => f.check(
                    case,
                    "pinnedCount",
                    after.iter().filter(|c| c.pinned).count(),
                    want.as_u64().expect("pinnedCount") as usize,
                ),
                "paneKeys" => f.check(case, "paneKeys", pane_keys(&after), strings(want)),
                "columnBasis" => f.check(
                    case,
                    "columnBasis",
                    after.iter().map(|c| c.basis).collect::<Vec<_>>(),
                    want.as_array().expect("columnBasis").iter().map(opt_num).collect(),
                ),
                "paneBasis" => {
                    for (k, b) in want.as_object().expect("paneBasis") {
                        f.check(case, &format!("paneBasis[{k}]"), pane_basis(&after, k), opt_num(b));
                    }
                }
                "isPinned" => {
                    for (k, b) in want.as_object().expect("isPinned") {
                        f.check(case, &format!("isPinned[{k}]"), is_pinned(&after, k), b.as_bool().expect("bool"));
                    }
                }
                "keyCount" => {
                    for (k, n) in want.as_object().expect("keyCount") {
                        let count = pane_keys(&after).iter().filter(|x| *x == k).count();
                        f.check(case, &format!("keyCount[{k}]"), count, n.as_u64().expect("n") as usize);
                    }
                }
                "findPane" => {
                    for (k, at) in want.as_object().expect("findPane") {
                        let want = (!at.is_null()).then(|| PaneAt {
                            col: at["col"].as_u64().expect("col") as usize,
                            index: at["index"].as_u64().expect("index") as usize,
                        });
                        f.check(case, &format!("findPane[{k}]"), find_pane(&after, k), want);
                    }
                }
                other => panic!("[{}] 不认识的断言：{other}", ts(case)),
            }
        }
    }

    for case in cases(&v, "clampSpot") {
        total += 1;
        let (_, columns, _) = run_steps(case);
        f.check(case, "clampSpot", clamp_spot(&columns, spot(&case["spot"])), spot(&case["expected"]));
    }

    for case in cases(&v, "dropSpot") {
        total += 1;
        let rects_value = match &case["rects"] {
            Value::String(name) => v["rects"][name].clone(),
            other => other.clone(),
        };
        let rects: Vec<ColumnRect> = serde_json::from_value(rects_value).expect("rects");
        let point: Point = serde_json::from_value(case["point"].clone()).expect("point");
        let edge = case.get("edge").map(num).unwrap_or(COLUMN_EDGE_PX);
        f.check(case, "dropSpot", drop_spot(point, &rects, edge), spot(&case["expected"]));
    }

    for case in cases(&v, "resolveSpot") {
        total += 1;
        let columns = cols(&case["columns"]);
        let visible_keys = strings(&case["visible"]);
        let visible = visible_columns(&columns, |k| visible_keys.iter().any(|v| v == k));
        f.check(case, "resolveSpot", resolve_spot(&columns, &visible, spot(&case["spot"])), spot(&case["expected"]));
    }

    for case in cases(&v, "clampColumnWidth") {
        total += 1;
        let max = case.get("max").map(num).unwrap_or(f64::INFINITY);
        f.check(case, "clampColumnWidth", clamp_column_width(num(&case["px"]), max), num(&case["expected"]));
    }

    for case in cases(&v, "clampPaneHeight") {
        total += 1;
        f.check(case, "clampPaneHeight", clamp_pane_height(num(&case["px"]), num(&case["max"])), num(&case["expected"]));
    }

    for case in cases(&v, "paneMaxHeight") {
        total += 1;
        let got = pane_max_height(num(&case["columnHeight"]), num(&case["above"]), num(&case["below"]));
        f.check(case, "paneMaxHeight", got, num(&case["expected"]));
    }

    for case in cases(&v, "columnWidth") {
        total += 1;
        let mut c = column(Vec::<String>::new(), None);
        c.basis = opt_num(&case["basis"]);
        let gap = case.get("gap").map(num).unwrap_or(CANVAS_GAP_PX);
        let count = case["count"].as_u64().expect("count") as usize;
        f.check(case, "columnWidth", column_width(&c, count, num(&case["viewportWidth"]), gap), num(&case["expected"]));
    }

    for case in cases(&v, "layoutFrames") {
        total += 1;
        let (_, columns, _) = run_steps(case);
        let viewport: Viewport = serde_json::from_value(case["viewport"].clone()).expect("viewport");
        let gap = case.get("gap").map(num).unwrap_or(CANVAS_GAP_PX);
        let frames: CanvasFrames = layout_frames(&columns, viewport, gap);
        // 列 id 每次现生成，比对时去掉
        let mut got = serde_json::to_value(&frames).expect("frames");
        for col in got["columns"].as_array_mut().expect("columns") {
            col.as_object_mut().expect("column").remove("id");
        }
        f.check(case, "layoutFrames", normalize_numbers(got), normalize_numbers(case["expected"].clone()));
    }

    f.finish("layout.json", total);
}

/// JSON 里 `640` 与 `640.0` 是同一个数
fn normalize_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64()),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_numbers).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, normalize_numbers(v))).collect()),
        other => other,
    }
}

// ---------------- sessionTitle ----------------

/// 只写了 name / title / agent / projectId 的会话补成完整的 Session
fn session(partial: &Value) -> Session {
    let mut full = json!({
        "id": "s", "projectId": "p", "name": "", "state": "active", "durable": true,
        "createdAt": 0, "lastActiveAt": 0
    });
    for (k, v) in partial.as_object().expect("session") {
        full[k] = v.clone();
    }
    serde_json::from_value(full).expect("session 形状不对")
}

struct ProjectRow {
    id: String,
    shell: Option<String>,
}

impl ProjectShell for ProjectRow {
    fn project_id(&self) -> &str {
        &self.id
    }
    fn shell(&self) -> Option<&str> {
        self.shell.as_deref()
    }
}

#[test]
fn session_title_vectors() {
    let v = load("sessionTitle");
    let mut f = Failures::default();
    let mut total = 0;

    for case in cases(&v, "sessionTitle") {
        total += 1;
        let want = case["expected"].as_str().map(str::to_string);
        f.check(case, "sessionTitle", session_title(&session(&case["session"])), want);
    }

    for case in cases(&v, "shellLabel") {
        total += 1;
        let shell = case.get("shell").and_then(Value::as_str);
        f.check(case, "shellLabel", shell_label(shell), case["expected"].as_str().expect("expected").to_string());
    }

    for case in cases(&v, "sessionLabel") {
        total += 1;
        let projects: Vec<ProjectRow> = case["projects"]
            .as_array()
            .expect("projects")
            .iter()
            .map(|p| ProjectRow { id: p["id"].as_str().expect("id").into(), shell: p["shell"].as_str().map(Into::into) })
            .collect();
        let got = session_label(&session(&case["session"]), &projects);
        f.check(case, "sessionLabel", got, case["expected"].as_str().expect("expected").to_string());
    }

    f.finish("sessionTitle.json", total);
}

// ---------------- projectTree ----------------

#[test]
fn project_tree_vectors() {
    let v = load("projectTree");
    let mut f = Failures::default();
    let mut total = 0;

    for case in cases(&v, "groupServers") {
        total += 1;
        let projects: Vec<Project> = serde_json::from_value(case["projects"].clone()).expect("projects");
        let hosts: Vec<SshHost> = serde_json::from_value(case["hosts"].clone()).expect("hosts");
        let servers = group_servers(
            &projects,
            &hosts,
            case["localName"].as_str().expect("localName"),
            case["showArchived"].as_bool().expect("showArchived"),
        );
        let summary: Vec<Value> = servers
            .iter()
            .map(|s| {
                json!({
                    "key": s.key,
                    "kind": s.kind.as_str(),
                    "name": s.name,
                    "folders": s.folders.iter().map(|folder| json!({
                        "project": folder.project.id,
                        "worktrees": folder.worktrees.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        f.check(case, "groupServers", Value::Array(summary), case["expected"].clone());
    }

    for case in cases(&v, "checkoutLabel") {
        total += 1;
        let project: Project = serde_json::from_value(case["project"].clone()).expect("project");
        let head = case.get("head").map(|h| ProjectHead {
            branch: h["branch"].as_str().map(Into::into),
            sha: h["sha"].as_str().map(Into::into),
        });
        let got = checkout_label(&project, head.as_ref());
        f.check(case, "checkoutLabel", got, case["expected"].as_str().expect("expected").to_string());
    }

    f.finish("projectTree.json", total);
}

// ---------------- fileSearch ----------------

#[test]
fn file_search_vectors() {
    let v = load("fileSearch");
    let mut f = Failures::default();
    let mut total = 0;

    for case in cases(&v, "basename") {
        total += 1;
        f.check(case, "basename", basename(case["path"].as_str().expect("path")), case["expected"].as_str().expect("expected"));
    }

    for case in cases(&v, "dirname") {
        total += 1;
        f.check(case, "dirname", dirname(case["path"].as_str().expect("path")), case["expected"].as_str().expect("expected"));
    }

    for case in cases(&v, "filterFiles") {
        total += 1;
        let limit = case.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(FILE_SEARCH_LIMIT);
        let got = filter_files(&strings(&case["paths"]), case["query"].as_str().expect("query"), limit);
        f.check(case, "filterFiles", got, strings(&case["expected"]));
    }

    for case in cases(&v, "scorePath") {
        total += 1;
        let got = score_path(case["path"].as_str().expect("path"), case["needle"].as_str().expect("needle"));
        f.check(case, "scorePath", got, case["expected"].as_u64().map(|n| n as u32));
    }

    f.finish("fileSearch.json", total);
}

// ---------------- gitGraph ----------------

#[test]
fn git_graph_vectors() {
    let v = load("gitGraph");
    let mut f = Failures::default();
    let mut total = 0;

    for case in cases(&v, "layoutCommitGraph") {
        total += 1;
        let commits: Vec<GraphCommit> = case["commits"]
            .as_array()
            .expect("commits")
            .iter()
            .map(|spec| {
                let mut parts = strings(spec).into_iter();
                GraphCommit { sha: parts.next().expect("sha"), parents: parts.collect() }
            })
            .collect();
        let want: Vec<GraphRow> = serde_json::from_value(case["expected"].clone()).expect("expected");
        f.check(case, "layoutCommitGraph", layout_commit_graph(&commits), want);
    }

    f.finish("gitGraph.json", total);
}
