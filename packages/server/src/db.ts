import Database from "better-sqlite3";
import path from "node:path";
import type {
  DeadReason,
  Project,
  Session,
  SessionState,
  SshAuthMethod,
  SshHost,
} from "@mojito/shared";

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
  /** 1 = mojito 建的目录，删除项目时才允许删它；null / 0 一律不删 */
  worktree_created_by_mojito: number | null;
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

export class Db {
  private db: Database.Database;

  constructor(dataDir: string) {
    this.db = new Database(path.join(dataDir, "mojito.db"));
    this.db.pragma("journal_mode = WAL");
    this.migrate();
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
    // v1 用 tmux，接不回来的会话原因是 tmux-gone；改用 Zellij 后统一为 session-gone
    this.db
      .prepare("UPDATE sessions SET dead_reason = 'session-gone' WHERE dead_reason = 'tmux-gone'")
      .run();

    // 附属项目（git worktree）。四列全可空，存量行天然是"普通项目"。
    //
    // 不写 REFERENCES projects(id)：本仓库 PRAGMA foreign_keys 从未开启，写了不生效，
    // 只会给人"有约束"的错觉；级联与 sessions 一样在应用层手写（见 routes 的 DELETE）。
    // 也不建索引：项目数量是个位数到几十，全表扫比多维护一处 schema 便宜。
    this.addColumn("projects", "source_project_id", "TEXT");
    this.addColumn("projects", "worktree_branch", "TEXT");
    this.addColumn("projects", "worktree_repo_dir", "TEXT");
    this.addColumn("projects", "worktree_created_by_mojito", "INTEGER");
    this.addColumn("projects", "host_id", "TEXT");
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

  // ---- settings ----

  getSetting(key: string): string | undefined {
    const row = this.db
      .prepare("SELECT value FROM settings WHERE key = ?")
      .get(key) as { value: string } | undefined;
    return row?.value;
  }

  setSetting(key: string, value: string) {
    this.db
      .prepare(
        "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value"
      )
      .run(key, value);
  }

  // ---- projects ----

  static toProject(row: ProjectRow): Project {
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
            createdByMojito: row.worktree_created_by_mojito === 1,
          }
        : undefined,
      createdAt: row.created_at,
    };
  }

  listProjects(): ProjectRow[] {
    return this.db
      .prepare("SELECT * FROM projects ORDER BY created_at ASC")
      .all() as ProjectRow[];
  }

  getProject(id: string): ProjectRow | undefined {
    return this.db.prepare("SELECT * FROM projects WHERE id = ?").get(id) as
      | ProjectRow
      | undefined;
  }

  insertProject(row: ProjectRow) {
    this.db
      .prepare(
        `INSERT INTO projects (id, name, type, working_dir, shell, ssh_host, ssh_port, ssh_username, ssh_auth_method, ssh_key_path, ssh_secret_enc, host_id, created_at,
           source_project_id, worktree_branch, worktree_repo_dir, worktree_created_by_mojito)
         VALUES (@id, @name, @type, @working_dir, @shell, @ssh_host, @ssh_port, @ssh_username, @ssh_auth_method, @ssh_key_path, @ssh_secret_enc, @host_id, @created_at,
           @source_project_id, @worktree_branch, @worktree_repo_dir, @worktree_created_by_mojito)`
      )
      .run(row);
  }

  /**
   * 刻意不更新 source_project_id / worktree_* 四列。
   *
   * 它们是删除护栏的判据（"这个目录是 mojito 建的、属于那个仓库"）。一旦能从
   * PUT /api/projects/:id 改写，护栏就等于不存在——攻击面是"改一行 JSON 让 mojito
   * 去 rm -rf 任意路径"。附属项目的这些属性在创建时定死，此后只读；
   * ProjectInput 里也没有对应字段，所以这条从类型层面就够不到。
   */
  updateProject(row: ProjectRow) {
    this.db
      .prepare(
        `UPDATE projects SET name=@name, working_dir=@working_dir, shell=@shell, ssh_host=@ssh_host, ssh_port=@ssh_port,
         ssh_username=@ssh_username, ssh_auth_method=@ssh_auth_method, ssh_key_path=@ssh_key_path, ssh_secret_enc=@ssh_secret_enc,
         host_id=@host_id
         WHERE id=@id`
      )
      .run(row);
  }

  /** 某源项目的全部附属项目。删除级联与确认框都要用。 */
  listWorktreeChildren(sourceId: string): ProjectRow[] {
    return this.db
      .prepare("SELECT * FROM projects WHERE source_project_id = ? ORDER BY created_at ASC")
      .all(sourceId) as ProjectRow[];
  }

  /**
   * 源项目改了 SSH 配置后，把五列刷到它的全部附属项目。
   *
   * 附属项目的 ssh_* 是从源项目**复制**来的，不是解引用——SshLink 由 ProjectRow
   * 构造、按 project.id 缓存，getLink / prepareZellij / GET /host 全部直接读
   * project.ssh_host。复制让这些点一处都不用改，代价就是这条手动传播。
   */
  updateChildrenSsh(sourceId: string, src: ProjectRow) {
    this.db
      .prepare(
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
    this.db
      .prepare(
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
    this.db.prepare("DELETE FROM sessions WHERE project_id = ?").run(id);
    this.db.prepare("DELETE FROM projects WHERE id = ?").run(id);
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
    const rows = this.db
      .prepare("SELECT * FROM ssh_hosts ORDER BY created_at ASC")
      .all() as SshHostRow[];
    const counts = this.db
      .prepare(
        "SELECT host_id AS id, COUNT(*) AS n FROM projects WHERE host_id IS NOT NULL GROUP BY host_id"
      )
      .all() as { id: string; n: number }[];
    const byId = new Map(counts.map((c) => [c.id, c.n]));
    return rows.map((row) => Db.toSshHost(row, byId.get(row.id) ?? 0));
  }

  getHost(id: string): SshHostRow | undefined {
    return this.db.prepare("SELECT * FROM ssh_hosts WHERE id = ?").get(id) as
      | SshHostRow
      | undefined;
  }

  findHostByName(name: string, exceptId?: string): SshHostRow | undefined {
    if (exceptId) {
      return this.db
        .prepare(
          "SELECT * FROM ssh_hosts WHERE lower(name) = lower(?) AND id != ?"
        )
        .get(name, exceptId) as SshHostRow | undefined;
    }
    return this.db
      .prepare("SELECT * FROM ssh_hosts WHERE lower(name) = lower(?)")
      .get(name) as SshHostRow | undefined;
  }

  insertHost(row: SshHostRow) {
    this.db
      .prepare(
        `INSERT INTO ssh_hosts (id, name, host, port, username, auth_method, key_path, secret_enc, created_at)
         VALUES (@id, @name, @host, @port, @username, @auth_method, @key_path, @secret_enc, @created_at)`
      )
      .run(row);
  }

  updateHost(row: SshHostRow) {
    this.db
      .prepare(
        `UPDATE ssh_hosts SET name=@name, host=@host, port=@port, username=@username,
         auth_method=@auth_method, key_path=@key_path, secret_enc=@secret_enc
         WHERE id=@id`
      )
      .run(row);
  }

  countProjectsByHost(hostId: string): number {
    const row = this.db
      .prepare("SELECT COUNT(*) AS n FROM projects WHERE host_id = ?")
      .get(hostId) as { n: number };
    return row.n;
  }

  deleteHost(id: string) {
    this.db.prepare("DELETE FROM ssh_hosts WHERE id = ?").run(id);
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
      createdAt: row.created_at,
      lastActiveAt: row.last_active_at,
    };
  }

  listSessions(): SessionRow[] {
    return this.db
      .prepare("SELECT * FROM sessions ORDER BY created_at ASC")
      .all() as SessionRow[];
  }

  listSessionsByProject(projectId: string): SessionRow[] {
    return this.db
      .prepare("SELECT * FROM sessions WHERE project_id = ? ORDER BY created_at ASC")
      .all(projectId) as SessionRow[];
  }

  getSession(id: string): SessionRow | undefined {
    return this.db.prepare("SELECT * FROM sessions WHERE id = ?").get(id) as
      | SessionRow
      | undefined;
  }

  insertSession(row: SessionRow) {
    this.db
      .prepare(
        `INSERT INTO sessions (id, project_id, name, state, durable, dead_reason, non_durable_reason, created_at, last_active_at)
         VALUES (@id, @project_id, @name, @state, @durable, @dead_reason, @non_durable_reason, @created_at, @last_active_at)`
      )
      .run(row);
  }

  updateSessionState(id: string, state: SessionState, deadReason?: DeadReason) {
    this.db
      .prepare("UPDATE sessions SET state = ?, dead_reason = ? WHERE id = ?")
      .run(state, deadReason ?? null, id);
  }

  renameSession(id: string, name: string) {
    this.db.prepare("UPDATE sessions SET name = ? WHERE id = ?").run(name, id);
  }

  touchSession(id: string, ts: number) {
    this.db.prepare("UPDATE sessions SET last_active_at = ? WHERE id = ?").run(ts, id);
  }

  deleteSession(id: string) {
    this.db.prepare("DELETE FROM sessions WHERE id = ?").run(id);
  }

  /** 启动恢复：上次仍 active 的会话，持久 → unverified，非持久 → dead */
  recoverSessionsOnStartup() {
    this.db
      .prepare(
        "UPDATE sessions SET state = 'unverified' WHERE state = 'active' AND durable = 1"
      )
      .run();
    this.db
      .prepare(
        "UPDATE sessions SET state = 'dead', dead_reason = 'backend-restart' WHERE state = 'active' AND durable = 0"
      )
      .run();
  }

  // ---- known hosts (TOFU) ----

  getKnownHost(host: string, port: number): string | undefined {
    const row = this.db
      .prepare("SELECT fingerprint FROM known_hosts WHERE host = ? AND port = ?")
      .get(host, port) as { fingerprint: string } | undefined;
    return row?.fingerprint;
  }

  saveKnownHost(host: string, port: number, fingerprint: string) {
    this.db
      .prepare(
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
    return this.db
      .prepare(
        "SELECT * FROM zellij_hosts WHERE host = ? AND port = ? AND username = ?"
      )
      .get(host, port, username) as ZellijHostRow | undefined;
  }

  /** 部分更新：只写传入的字段，其余保持原值 */
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
    const row: ZellijHostRow = {
      host,
      port,
      username,
      authorized: patch.authorized ?? cur?.authorized ?? null,
      installed_version: patch.installed_version ?? cur?.installed_version ?? null,
      base_url: patch.base_url ?? cur?.base_url ?? null,
      verified_durable: patch.verified_durable ?? cur?.verified_durable ?? null,
      updated_at: Date.now(),
    };
    this.db
      .prepare(
        `INSERT INTO zellij_hosts (host, port, username, authorized, installed_version, base_url, verified_durable, updated_at)
         VALUES (@host, @port, @username, @authorized, @installed_version, @base_url, @verified_durable, @updated_at)
         ON CONFLICT(host, port, username) DO UPDATE SET
           authorized = excluded.authorized,
           installed_version = excluded.installed_version,
           base_url = excluded.base_url,
           verified_durable = excluded.verified_durable,
           updated_at = excluded.updated_at`
      )
      .run(row);
  }
}
