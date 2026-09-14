import { DatabaseSync, type SQLInputValue } from "node:sqlite";
import fs from "node:fs";
import path from "node:path";
import { isSessionAgent } from "@falcon/shared";
import type {
  DeadReason,
  ForwardKind,
  MultiRepoMember,
  Project,
  Session,
  SessionState,
  SshAuthMethod,
  SshHost,
  MeeglePin,
  MeeglePinKind,
} from "@falcon/shared";

export interface ProjectRow {
  id: string;
  name: string;
  type: "local" | "ssh";
  working_dir: string | null;
  shell: string | null;
  ssh_host: string | null;
  ssh_port: number | null;
  ssh_username: string | null;
  ssh_auth_method: string | null;
  ssh_key_path: string | null;
  ssh_secret_enc: string | null;
  /** 已保存主机；存量项目与手写 ssh 的请求为 null */
  host_id: string | null;
  created_at: number;
  // ---- 附属项目（git worktree）。四列全可空，普通项目一律为 null ----
  source_project_id: string | null;
  worktree_branch: string | null;
  /** 派生时记录的仓库根。删除护栏拿它比对，不读 working_dir */
  worktree_repo_dir: string | null;
  /** 1 = falcon 建的目录，删除项目时才允许删它；null / 0 一律不删 */
  worktree_created_by_mojito: number | null;
  /** 存档时间（unix 毫秒）。非 null ⇔ 已存档，到期由后台清扫删除；普通项目恒为 null */
  worktree_archived_at: number | null;
  /**
   * 多仓库项目的成员清单（JSON 的 MultiRepoMember[]）。非 null ⇔ 多仓库项目；
   * 容器与派生产物共用这一列，语义由 source_project_id 判别（见 shared 的注释）。
   * 派生行上它是删除目标清单——写入路径与 worktree 四列同样必须不可达。
   */
  multi_repos: string | null;
}

export interface SessionRow {
  id: string;
  project_id: string;
  name: string;
  state: SessionState;
  durable: number;
  dead_reason: string | null;
  non_durable_reason: string | null;
  created_at: number;
  last_active_at: number;
  /** 上次 Viewer 量到的格子；null = 从未量过，接回时不能当 80×24 用 */
  cols: number | null;
  rows: number | null;
  /** 开场跑的 CLI（claude / codex / grok）；null = 普通 shell */
  agent: string | null;
}

export interface SshHostRow {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  auth_method: string;
  key_path: string | null;
  secret_enc: string | null;
  created_at: number;
}

export interface SshForwardRow {
  id: string;
  project_id: string;
  name: string | null;
  kind: string;
  bind_host: string;
  bind_port: number;
  dest_host: string;
  dest_port: number;
  enabled: number;
  created_at: number;
}

export interface MeeglePinRow {
  id: string;
  kind: string;
  space_key: string;
  space_name: string | null;
  target_id: string;
  type_key: string | null;
  label: string;
  url: string | null;
  created_at: number;
}

export interface ZellijHostRow {
  host: string;
  port: number;
  username: string;
  /** null = 未询问过；1 = 已授权在该主机安装；0 = 已拒绝 */
  authorized: number | null;
  installed_version: string | null;
  /** 该主机的下载源覆盖；null 表示用官方地址 */
  base_url: string | null;
  /** Windows 远端的真实断线验证结果；null = 未验证 */
  verified_durable: number | null;
  updated_at: number;
}

/** 行接口是封闭类型，node:sqlite 的命名参数要求带索引签名的 Record，这里统一收窄 */
const bindRow = (row: object) => row as Record<string, SQLInputValue>;

/** 新库名 falcon.db；旧安装只有 mojito.db 时接着用，避免改名后打开空库。 */
function dbFile(dataDir: string): string {
  const next = path.join(dataDir, "falcon.db");
  const prev = path.join(dataDir, "mojito.db");
  if (!fs.existsSync(next) && fs.existsSync(prev)) return prev;
  return next;
}

export class Db {
  private db: DatabaseSync;
  /** prepare 的语句缓存：node:sqlite 不缓存，热路径上每次现场编译 SQL 白费 */
  private stmts = new Map<string, ReturnType<DatabaseSync["prepare"]>>();

  constructor(dataDir: string) {
    // node:sqlite 默认开外键约束；本仓库从未启用过（级联在应用层手写），显式关掉保持语义不变。
    // UPDATE 走 bindRow 传整行，SQL 用不到 created_at 等列；better-sqlite3 会忽略多余命名参数，
    // node:sqlite 默认抛 ERR_INVALID_STATE，这里显式放开。
    this.db = new DatabaseSync(dbFile(dataDir), {
      enableForeignKeyConstraints: false,
      allowUnknownNamedParameters: true,
    });
    this.db.exec("PRAGMA journal_mode = WAL");
    // WAL 下默认仍是 synchronous=FULL（每条隐式事务一次 fsync，且全同步 API 阻塞事件循环）。
    // NORMAL 在 WAL 下不会损坏数据库，最坏掉电丢最后几笔——对本库存的数据完全可接受。
    this.db.exec("PRAGMA synchronous = NORMAL");
    this.migrate();
  }

  private stmt(sql: string) {
    let s = this.stmts.get(sql);
    if (!s) {
      s = this.db.prepare(sql);
      this.stmts.set(sql, s);
    }
    return s;
  }

  private migrate() {
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        type TEXT NOT NULL,
        working_dir TEXT,
        shell TEXT,
        ssh_host TEXT,
        ssh_port INTEGER,
        ssh_username TEXT,
        ssh_auth_method TEXT,
        ssh_key_path TEXT,
        ssh_secret_enc TEXT,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id),
        name TEXT NOT NULL,
        state TEXT NOT NULL,
        durable INTEGER NOT NULL,
        dead_reason TEXT,
        created_at INTEGER NOT NULL,
        last_active_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS known_hosts (
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        fingerprint TEXT NOT NULL,
        PRIMARY KEY (host, port)
      );
      CREATE TABLE IF NOT EXISTS ssh_hosts (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        username TEXT NOT NULL,
        auth_method TEXT NOT NULL,
        key_path TEXT,
        secret_enc TEXT,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS zellij_hosts (
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        username TEXT NOT NULL,
        authorized INTEGER,
        installed_version TEXT,
        base_url TEXT,
        verified_durable INTEGER,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (host, port, username)
      );
    `);
    this.addColumn("sessions", "non_durable_reason", "TEXT");
    this.addColumn("sessions", "cols", "INTEGER");
    this.addColumn("sessions", "rows", "INTEGER");
    this.addColumn("sessions", "agent", "TEXT");
    // v1 用 tmux，接不回来的会话原因是 tmux-gone；改用 Zellij 后统一为 session-gone
    this.stmt("UPDATE sessions SET dead_reason = 'session-gone' WHERE dead_reason = 'tmux-gone'")
      .run();
    this.clearAutoNames();

    // 附属项目（git worktree）。四列全可空，存量行天然是"普通项目"。
    //
    // 不写 REFERENCES projects(id)：本仓库 PRAGMA foreign_keys 从未开启，写了不生效，
    // 只会给人"有约束"的错觉；级联与 sessions 一样在应用层手写（见 routes 的 DELETE）。
    // 也不建索引：项目数量是个位数到几十，全表扫比多维护一处 schema 便宜。
    this.addColumn("projects", "source_project_id", "TEXT");
    this.addColumn("projects", "worktree_branch", "TEXT");
    this.addColumn("projects", "worktree_repo_dir", "TEXT");
    this.addColumn("projects", "worktree_created_by_mojito", "INTEGER");
    this.addColumn("projects", "worktree_archived_at", "INTEGER");
    this.addColumn("projects", "host_id", "TEXT");
    // 多仓库项目的成员清单（JSON）。容器与派生产物共用，语义由 source_project_id 判别
    this.addColumn("projects", "multi_repos", "TEXT");

    // 端口转发规则挂在项目上（走该项目的 SshLink），不是解引用主机。
    // 不写 REFERENCES：本仓库外键从未开启，级联在应用层手写。
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS ssh_forwards (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        name TEXT,
        kind TEXT NOT NULL,
        bind_host TEXT NOT NULL,
        bind_port INTEGER NOT NULL,
        dest_host TEXT NOT NULL,
        dest_port INTEGER NOT NULL,
        enabled INTEGER NOT NULL,
        created_at INTEGER NOT NULL
      );
    `);
    // 飞书项目面板的固定列表（ADR 0010）。不挂在项目上：CLI 的登录态是整台机器一份
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS meegle_pins (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        space_key TEXT NOT NULL,
        space_name TEXT,
        target_id TEXT NOT NULL,
        type_key TEXT,
        label TEXT NOT NULL,
        url TEXT,
        created_at INTEGER NOT NULL
      );
    `);
  }

  /**
   * zellij_hosts / known_hosts / ssh_hosts 三张表键不同，不合：
   * known_hosts 表达"这台机器的身份可信"（主机级事实，与登录用户无关，键是 host+port），
   * zellij_hosts 表达"这个账户下装了什么"（账户级事实，键必须含 username——
   * alice@srv 授权过不代表 bob@srv 的 home 里也有二进制），
   * ssh_hosts 表达"用户预先保存的连接配置"（有别名、可改、被项目引用，键是自己的 id）。
   */
  private addColumn(table: string, column: string, type: string) {
    const cols = this.db.prepare(`PRAGMA table_info(${table})`).all() as {
      name: string;
    }[];
    if (!cols.some((c) => c.name === column)) {
      this.db.exec(`ALTER TABLE ${table} ADD COLUMN ${column} ${type}`);
    }
  }

  /**
   * 存量会话的自动名（`Terminal 3` / `Claude 1`）一次性清空——现在没起名就是空串，
   * UI 走自动标题。
   *
   * 只跑一次，用 settings 里的标记记着：这条清理认不出"用户手动起的名字恰好
   * 长这样"，每次启动都跑会把人家改回去的名字又抹掉。没有版本号迁移表（见 migrate），
   * 标记就是最轻的一次性开关。
   */
  private clearAutoNames() {
    const KEY = "migration.sessions.clearAutoNames";
    if (this.getSetting(KEY)) return;
    // 在 TS 里按正则筛，不在 SQL 里拼 GLOB：会话数量是个位数到几十，精确匹配
    // 比省几次查询值钱（`Terminal 2 号` 这种手起的名字不该被误伤）
    const auto = /^(?:Terminal|Claude|Codex|Grok) \d+$/;
    const rows = this.stmt("SELECT id, name FROM sessions").all() as {
      id: string;
      name: string;
    }[];
    for (const row of rows) {
      if (auto.test(row.name)) this.renameSession(row.id, "");
    }
    this.setSetting(KEY, "1");
  }

  // ---- settings ----

  getSetting(key: string): string | undefined {
    const row = this.stmt("SELECT value FROM settings WHERE key = ?")
      .get(key) as { value: string } | undefined;
    return row?.value;
  }

  setSetting(key: string, value: string) {
    this.stmt(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value"
      )
      .run(key, value);
  }

  // ---- projects ----

  /**
   * 解析 multi_repos。损坏的 JSON / 形状不对一律返回 null：toProject 侧降级成
   * "看起来是普通项目"（普通项目永不删目录，方向安全）；删除侧拿到 null 直接
   * veto（见 remove.ts 的 vetoMultiRemoval），最坏是拒删并告警，不会删错。
   */
  static parseMultiRepos(row: Pick<ProjectRow, "multi_repos">): MultiRepoMember[] | null {
    if (row.multi_repos == null) return null;
    try {
      const parsed: unknown = JSON.parse(row.multi_repos);
      if (!Array.isArray(parsed)) return null;
      const members: MultiRepoMember[] = [];
      for (const item of parsed) {
        if (typeof item !== "object" || item === null) return null;
        const { dir, repoDir } = item as { dir?: unknown; repoDir?: unknown };
        if (typeof dir !== "string" || dir.length === 0) return null;
        if (repoDir !== undefined && typeof repoDir !== "string") return null;
        members.push(repoDir === undefined ? { dir } : { dir, repoDir });
      }
      return members;
    } catch {
      return null;
    }
  }

  static toProject(row: ProjectRow): Project {
    const members = Db.parseMultiRepos(row);
    return {
      id: row.id,
      name: row.name,
      type: row.type,
      workingDir: row.working_dir ?? undefined,
      shell: row.shell ?? undefined,
      ssh:
        row.type === "ssh"
          ? {
              host: row.ssh_host!,
              port: row.ssh_port!,
              username: row.ssh_username!,
              authMethod: row.ssh_auth_method as SshAuthMethod,
              keyPath: row.ssh_key_path ?? undefined,
              hasSecret: row.ssh_secret_enc != null,
            }
          : undefined,
      hostId: row.host_id ?? undefined,
      worktree: row.source_project_id
        ? {
            sourceProjectId: row.source_project_id,
            branch: row.worktree_branch ?? "",
            repoDir: row.worktree_repo_dir ?? "",
            createdByFalcon: row.worktree_created_by_mojito === 1,
            archivedAt: row.worktree_archived_at ?? undefined,
          }
        : undefined,
      multi: members ? { repos: members } : undefined,
      createdAt: row.created_at,
    };
  }

  listProjects(): ProjectRow[] {
    return this.stmt("SELECT * FROM projects ORDER BY created_at ASC")
      .all() as unknown as ProjectRow[];
  }

  getProject(id: string): ProjectRow | undefined {
    return this.stmt("SELECT * FROM projects WHERE id = ?").get(id) as
      | ProjectRow
      | undefined;
  }

  insertProject(row: ProjectRow) {
    this.stmt(
        `INSERT INTO projects (id, name, type, working_dir, shell, ssh_host, ssh_port, ssh_username, ssh_auth_method, ssh_key_path, ssh_secret_enc, host_id, created_at,
           source_project_id, worktree_branch, worktree_repo_dir, worktree_created_by_mojito, worktree_archived_at, multi_repos)
         VALUES (@id, @name, @type, @working_dir, @shell, @ssh_host, @ssh_port, @ssh_username, @ssh_auth_method, @ssh_key_path, @ssh_secret_enc, @host_id, @created_at,
           @source_project_id, @worktree_branch, @worktree_repo_dir, @worktree_created_by_mojito, @worktree_archived_at, @multi_repos)`
      )
      .run(bindRow(row));
  }

  /**
   * 刻意不更新 source_project_id / worktree_* 四列，也不更新 multi_repos。
   *
   * 它们是删除护栏的判据（"这个目录是 falcon 建的、属于那个仓库"）。一旦能从
   * PUT /api/projects/:id 改写，护栏就等于不存在——攻击面是"改一行 JSON 让 falcon
   * 去 rm -rf 任意路径"。附属项目的这些属性在创建时定死，此后只读；
   * ProjectInput 里也没有对应字段，所以这条从类型层面就够不到。
   * multi_repos 在派生行上是删除目标清单，同罪；容器改成员走 updateMultiRepos。
   */
  updateProject(row: ProjectRow) {
    this.stmt(
        `UPDATE projects SET name=@name, working_dir=@working_dir, shell=@shell, ssh_host=@ssh_host, ssh_port=@ssh_port,
         ssh_username=@ssh_username, ssh_auth_method=@ssh_auth_method, ssh_key_path=@ssh_key_path, ssh_secret_enc=@ssh_secret_enc,
         host_id=@host_id
         WHERE id=@id`
      )
      .run(bindRow(row));
  }

  /**
   * 替换多仓库**容器**的成员清单。
   *
   * SQL 里的 `source_project_id IS NULL` 不是防御式编程，是护栏本体：派生行的
   * multi_repos 是删除目标清单，写入路径必须不可达——即使路由层将来写错，
   * 这条 UPDATE 也够不到派生行（与 updateProject 刻意不更新 worktree 四列同理）。
   */
  updateMultiRepos(id: string, repos: MultiRepoMember[]) {
    this.stmt(
        "UPDATE projects SET multi_repos = ? WHERE id = ? AND source_project_id IS NULL"
      )
      .run(JSON.stringify(repos), id);
  }

  /**
   * 一行项目贡献给"别删到我"清单（otherDirs）的全部路径：working_dir、
   * 容器成员的仓库路径、派生产物成员的 worktree 路径与仓库根。
   * 容器的成员是用户的真仓库，必须能挡住别的删除踩上去。
   * worktree_repo_dir 维持现状不收——单仓库附属项目的仓库根本来就没进过这个清单，
   * 这里只为新增的多仓库形态补齐，不悄悄改既有语义。
   */
  static guardDirsOf(row: ProjectRow): string[] {
    const dirs: string[] = [];
    if (row.working_dir) dirs.push(row.working_dir);
    for (const m of Db.parseMultiRepos(row) ?? []) {
      dirs.push(m.dir);
      if (m.repoDir) dirs.push(m.repoDir);
    }
    return dirs;
  }

  /**
   * 存档 / 恢复附属项目。ts=null 即恢复。
   * 只动这一列：worktree 四列仍是删除护栏的只读判据（见 updateProject 的注释），
   * 存档时间不参与任何路径判断，可写不构成攻击面。
   */
  setWorktreeArchived(id: string, ts: number | null) {
    this.stmt("UPDATE projects SET worktree_archived_at = ? WHERE id = ?")
      .run(ts, id);
  }

  /** 存档已到期（archived_at ≤ archivedBefore）的附属项目，供后台清扫 */
  listArchivedExpired(archivedBefore: number): ProjectRow[] {
    return this.stmt(
        `SELECT * FROM projects
         WHERE source_project_id IS NOT NULL AND worktree_archived_at IS NOT NULL
           AND worktree_archived_at <= ?
         ORDER BY worktree_archived_at ASC`
      )
      .all(archivedBefore) as unknown as ProjectRow[];
  }

  /** 某源项目的全部附属项目。删除级联与确认框都要用。 */
  listWorktreeChildren(sourceId: string): ProjectRow[] {
    return this.stmt("SELECT * FROM projects WHERE source_project_id = ? ORDER BY created_at ASC")
      .all(sourceId) as unknown as ProjectRow[];
  }

  /**
   * 源项目改了 SSH 配置后，把五列刷到它的全部附属项目。
   *
   * 附属项目的 ssh_* 是从源项目**复制**来的，不是解引用——SshLink 由 ProjectRow
   * 构造、按 project.id 缓存，getLink / prepareZellij / GET /host 全部直接读
   * project.ssh_host。复制让这些点一处都不用改，代价就是这条手动传播。
   */
  updateChildrenSsh(sourceId: string, src: ProjectRow) {
    this.stmt(
        `UPDATE projects SET ssh_host=@ssh_host, ssh_port=@ssh_port, ssh_username=@ssh_username,
           ssh_auth_method=@ssh_auth_method, ssh_key_path=@ssh_key_path, ssh_secret_enc=@ssh_secret_enc,
           host_id=@host_id
         WHERE source_project_id=@source_id`
      )
      .run({
        ssh_host: src.ssh_host,
        ssh_port: src.ssh_port,
        ssh_username: src.ssh_username,
        ssh_auth_method: src.ssh_auth_method,
        ssh_key_path: src.ssh_key_path,
        ssh_secret_enc: src.ssh_secret_enc,
        host_id: src.host_id,
        source_id: sourceId,
      });
  }

  /**
   * 已保存主机改了连接配置后，把五列刷到引用它的全部项目。
   * 与 updateChildrenSsh 同构：项目上的 ssh_* 是复制，不是解引用。
   */
  updateProjectsFromHost(host: SshHostRow) {
    this.stmt(
        `UPDATE projects SET ssh_host=@host, ssh_port=@port, ssh_username=@username,
           ssh_auth_method=@auth_method, ssh_key_path=@key_path, ssh_secret_enc=@secret_enc
         WHERE host_id=@id`
      )
      .run({
        host: host.host,
        port: host.port,
        username: host.username,
        auth_method: host.auth_method,
        key_path: host.key_path,
        secret_enc: host.secret_enc,
        id: host.id,
      });
  }

  deleteProject(id: string) {
    this.stmt("DELETE FROM ssh_forwards WHERE project_id = ?").run(id);
    this.stmt("DELETE FROM sessions WHERE project_id = ?").run(id);
    this.stmt("DELETE FROM projects WHERE id = ?").run(id);
  }

  // ---- SSH port forwards ----

  listForwards(projectId: string): SshForwardRow[] {
    return this.stmt("SELECT * FROM ssh_forwards WHERE project_id = ? ORDER BY created_at ASC")
      .all(projectId) as unknown as SshForwardRow[];
  }

  listEnabledForwardProjectIds(): string[] {
    const rows = this.stmt("SELECT DISTINCT project_id FROM ssh_forwards WHERE enabled = 1")
      .all() as { project_id: string }[];
    return rows.map((r) => r.project_id);
  }

  getForward(id: string): SshForwardRow | undefined {
    return this.stmt("SELECT * FROM ssh_forwards WHERE id = ?").get(id) as
      | SshForwardRow
      | undefined;
  }

  findForwardBind(
    projectId: string,
    kind: ForwardKind,
    bindHost: string,
    bindPort: number,
    exceptId?: string
  ): SshForwardRow | undefined {
    if (exceptId) {
      return this.stmt(
          `SELECT * FROM ssh_forwards
           WHERE project_id = ? AND kind = ? AND bind_host = ? AND bind_port = ? AND id != ?`
        )
        .get(projectId, kind, bindHost, bindPort, exceptId) as SshForwardRow | undefined;
    }
    return this.stmt(
        `SELECT * FROM ssh_forwards
         WHERE project_id = ? AND kind = ? AND bind_host = ? AND bind_port = ?`
      )
      .get(projectId, kind, bindHost, bindPort) as SshForwardRow | undefined;
  }

  /** 本地转发绑在后端本机上，跨项目也不能抢同一个回环端口。 */
  findLocalBindConflict(
    bindHost: string,
    bindPort: number,
    exceptId?: string
  ): SshForwardRow | undefined {
    if (exceptId) {
      return this.stmt(
          `SELECT * FROM ssh_forwards
           WHERE kind = 'local' AND bind_host = ? AND bind_port = ? AND id != ?`
        )
        .get(bindHost, bindPort, exceptId) as SshForwardRow | undefined;
    }
    return this.stmt(
        `SELECT * FROM ssh_forwards
         WHERE kind = 'local' AND bind_host = ? AND bind_port = ?`
      )
      .get(bindHost, bindPort) as SshForwardRow | undefined;
  }

  insertForward(row: SshForwardRow) {
    this.stmt(
        `INSERT INTO ssh_forwards
           (id, project_id, name, kind, bind_host, bind_port, dest_host, dest_port, enabled, created_at)
         VALUES
           (@id, @project_id, @name, @kind, @bind_host, @bind_port, @dest_host, @dest_port, @enabled, @created_at)`
      )
      .run(bindRow(row));
  }

  updateForward(row: SshForwardRow) {
    this.stmt(
        `UPDATE ssh_forwards SET name=@name, kind=@kind, bind_host=@bind_host, bind_port=@bind_port,
           dest_host=@dest_host, dest_port=@dest_port, enabled=@enabled
         WHERE id=@id`
      )
      .run(bindRow(row));
  }

  deleteForward(id: string) {
    this.stmt("DELETE FROM ssh_forwards WHERE id = ?").run(id);
  }

  // ---- 飞书项目面板的固定列表 ----

  static toMeeglePin(row: MeeglePinRow): MeeglePin {
    return {
      id: row.id,
      kind: row.kind as MeeglePinKind,
      spaceKey: row.space_key,
      spaceName: row.space_name ?? undefined,
      targetId: row.target_id,
      typeKey: row.type_key ?? undefined,
      label: row.label,
      url: row.url ?? undefined,
      createdAt: row.created_at,
    };
  }

  listMeeglePins(): MeeglePinRow[] {
    return this.stmt("SELECT * FROM meegle_pins ORDER BY created_at ASC").all() as unknown as MeeglePinRow[];
  }

  getMeeglePin(id: string): MeeglePinRow | undefined {
    return this.stmt("SELECT * FROM meegle_pins WHERE id = ?").get(id) as MeeglePinRow | undefined;
  }

  /** 同一个东西只固定一次 */
  findMeeglePin(kind: string, spaceKey: string, targetId: string): MeeglePinRow | undefined {
    return this.stmt(
        "SELECT * FROM meegle_pins WHERE kind = ? AND space_key = ? AND target_id = ?"
      )
      .get(kind, spaceKey, targetId) as MeeglePinRow | undefined;
  }

  insertMeeglePin(row: MeeglePinRow) {
    this.stmt(
        `INSERT INTO meegle_pins
           (id, kind, space_key, space_name, target_id, type_key, label, url, created_at)
         VALUES
           (@id, @kind, @space_key, @space_name, @target_id, @type_key, @label, @url, @created_at)`
      )
      .run(bindRow(row));
  }

  renameMeeglePin(id: string, label: string) {
    this.stmt("UPDATE meegle_pins SET label = ? WHERE id = ?").run(label, id);
  }

  deleteMeeglePin(id: string) {
    this.stmt("DELETE FROM meegle_pins WHERE id = ?").run(id);
  }

  // ---- saved SSH hosts ----

  static toSshHost(row: SshHostRow, projectCount: number): SshHost {
    return {
      id: row.id,
      name: row.name,
      host: row.host,
      port: row.port,
      username: row.username,
      authMethod: row.auth_method as SshAuthMethod,
      keyPath: row.key_path ?? undefined,
      hasSecret: row.secret_enc != null,
      projectCount,
      createdAt: row.created_at,
    };
  }

  listHosts(): SshHost[] {
    const rows = this.stmt("SELECT * FROM ssh_hosts ORDER BY created_at ASC")
      .all() as unknown as SshHostRow[];
    const counts = this.stmt(
        "SELECT host_id AS id, COUNT(*) AS n FROM projects WHERE host_id IS NOT NULL GROUP BY host_id"
      )
      .all() as { id: string; n: number }[];
    const byId = new Map(counts.map((c) => [c.id, c.n]));
    return rows.map((row) => Db.toSshHost(row, byId.get(row.id) ?? 0));
  }

  getHost(id: string): SshHostRow | undefined {
    return this.stmt("SELECT * FROM ssh_hosts WHERE id = ?").get(id) as
      | SshHostRow
      | undefined;
  }

  findHostByName(name: string, exceptId?: string): SshHostRow | undefined {
    if (exceptId) {
      return this.stmt(
          "SELECT * FROM ssh_hosts WHERE lower(name) = lower(?) AND id != ?"
        )
        .get(name, exceptId) as SshHostRow | undefined;
    }
    return this.stmt("SELECT * FROM ssh_hosts WHERE lower(name) = lower(?)")
      .get(name) as SshHostRow | undefined;
  }

  insertHost(row: SshHostRow) {
    this.stmt(
        `INSERT INTO ssh_hosts (id, name, host, port, username, auth_method, key_path, secret_enc, created_at)
         VALUES (@id, @name, @host, @port, @username, @auth_method, @key_path, @secret_enc, @created_at)`
      )
      .run(bindRow(row));
  }

  updateHost(row: SshHostRow) {
    this.stmt(
        `UPDATE ssh_hosts SET name=@name, host=@host, port=@port, username=@username,
         auth_method=@auth_method, key_path=@key_path, secret_enc=@secret_enc
         WHERE id=@id`
      )
      .run(bindRow(row));
  }

  countProjectsByHost(hostId: string): number {
    const row = this.stmt("SELECT COUNT(*) AS n FROM projects WHERE host_id = ?")
      .get(hostId) as { n: number };
    return row.n;
  }

  deleteHost(id: string) {
    this.stmt("DELETE FROM ssh_hosts WHERE id = ?").run(id);
  }

  // ---- sessions ----

  static toSession(row: SessionRow): Session {
    return {
      id: row.id,
      projectId: row.project_id,
      name: row.name,
      state: row.state,
      durable: row.durable === 1,
      nonDurableReason:
        (row.non_durable_reason as Session["nonDurableReason"] | null) ?? undefined,
      deadReason: (row.dead_reason as DeadReason | null) ?? undefined,
      agent: isSessionAgent(row.agent) ? row.agent : undefined,
      createdAt: row.created_at,
      lastActiveAt: row.last_active_at,
    };
  }

  listSessions(): SessionRow[] {
    return this.stmt("SELECT * FROM sessions ORDER BY created_at ASC")
      .all() as unknown as SessionRow[];
  }

  listSessionsByProject(projectId: string): SessionRow[] {
    return this.stmt("SELECT * FROM sessions WHERE project_id = ? ORDER BY created_at ASC")
      .all(projectId) as unknown as SessionRow[];
  }

  getSession(id: string): SessionRow | undefined {
    return this.stmt("SELECT * FROM sessions WHERE id = ?").get(id) as
      | SessionRow
      | undefined;
  }

  insertSession(row: SessionRow) {
    this.stmt(
        `INSERT INTO sessions (id, project_id, name, state, durable, dead_reason, non_durable_reason, created_at, last_active_at, cols, rows, agent)
         VALUES (@id, @project_id, @name, @state, @durable, @dead_reason, @non_durable_reason, @created_at, @last_active_at, @cols, @rows, @agent)`
      )
      .run(bindRow(row));
  }

  updateSessionState(id: string, state: SessionState, deadReason?: DeadReason) {
    this.stmt("UPDATE sessions SET state = ?, dead_reason = ? WHERE id = ?")
      .run(state, deadReason ?? null, id);
  }

  renameSession(id: string, name: string) {
    this.stmt("UPDATE sessions SET name = ? WHERE id = ?").run(name, id);
  }

  touchSession(id: string, ts: number) {
    this.stmt("UPDATE sessions SET last_active_at = ? WHERE id = ?").run(ts, id);
  }

  updateSessionSize(id: string, cols: number, rows: number) {
    this.stmt("UPDATE sessions SET cols = ?, rows = ? WHERE id = ?").run(cols, rows, id);
  }

  deleteSession(id: string) {
    this.stmt("DELETE FROM sessions WHERE id = ?").run(id);
  }

  /** 启动恢复：上次仍 active 的会话，持久 → unverified，非持久 → dead */
  recoverSessionsOnStartup() {
    this.stmt(
        "UPDATE sessions SET state = 'unverified' WHERE state = 'active' AND durable = 1"
      )
      .run();
    this.stmt(
        "UPDATE sessions SET state = 'dead', dead_reason = 'backend-restart' WHERE state = 'active' AND durable = 0"
      )
      .run();
  }

  // ---- known hosts (TOFU) ----

  getKnownHost(host: string, port: number): string | undefined {
    const row = this.stmt("SELECT fingerprint FROM known_hosts WHERE host = ? AND port = ?")
      .get(host, port) as { fingerprint: string } | undefined;
    return row?.fingerprint;
  }

  saveKnownHost(host: string, port: number, fingerprint: string) {
    this.stmt(
        "INSERT INTO known_hosts (host, port, fingerprint) VALUES (?, ?, ?) ON CONFLICT(host, port) DO UPDATE SET fingerprint = excluded.fingerprint"
      )
      .run(host, port, fingerprint);
  }

  // ---- Zellij 主机状态 ----

  getZellijHost(
    host: string,
    port: number,
    username: string
  ): ZellijHostRow | undefined {
    return this.stmt(
        "SELECT * FROM zellij_hosts WHERE host = ? AND port = ? AND username = ?"
      )
      .get(host, port, username) as ZellijHostRow | undefined;
  }

  /**
   * 部分更新：只写传入的字段，其余保持原值。
   *
   * 判据是"键在不在 patch 里"而不是 `??`：显式传 null 表示**清空该字段**
   * （作废安装记录、作废持久性判定），用 `??` 会被旧值顶回去、清空静默失效。
   */
  upsertZellijHost(
    host: string,
    port: number,
    username: string,
    patch: Partial<
      Pick<
        ZellijHostRow,
        "authorized" | "installed_version" | "base_url" | "verified_durable"
      >
    >
  ) {
    const cur = this.getZellijHost(host, port, username);
    const pick = <K extends keyof typeof patch>(key: K): ZellijHostRow[K] =>
      (key in patch ? patch[key] : cur?.[key]) ?? null;
    const row: ZellijHostRow = {
      host,
      port,
      username,
      authorized: pick("authorized"),
      installed_version: pick("installed_version"),
      base_url: pick("base_url"),
      verified_durable: pick("verified_durable"),
      updated_at: Date.now(),
    };
    this.stmt(
        `INSERT INTO zellij_hosts (host, port, username, authorized, installed_version, base_url, verified_durable, updated_at)
         VALUES (@host, @port, @username, @authorized, @installed_version, @base_url, @verified_durable, @updated_at)
         ON CONFLICT(host, port, username) DO UPDATE SET
           authorized = excluded.authorized,
           installed_version = excluded.installed_version,
           base_url = excluded.base_url,
           verified_durable = excluded.verified_durable,
           updated_at = excluded.updated_at`
      )
      .run(bindRow(row));
  }
}
