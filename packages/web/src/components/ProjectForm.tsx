import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Project, ProjectInput, ProjectType, ShellsInfo } from "@falcon/shared";
import { ArrowDown, ArrowUp, X } from "lucide-react";
import { api } from "../api.js";
import { sshConn } from "../lib/hostColor.js";
import { commonParentDir } from "../lib/multiPath.js";
import { useApp, type ProjectFormPreset } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { AppDialog } from "./common/AppDialog.js";
import { Field, Segmented } from "./common/Field.js";
import { FolderPicker } from "./FolderPicker.js";
import { SshFields, type SshFieldValues } from "./SshFields.js";

/** shell 下拉的两个哨兵值：Radix Select 不接受空字符串当 value */
const SHELL_AUTO = "__auto__";
const SHELL_CUSTOM = "__custom__";
/** 多仓库档「位置」下拉的本机哨兵，理由同上 */
const MULTI_LOCAL = "__local__";

/**
 * 表单的第三档「多仓库」是**纯 UI 概念**：shared 层的 type 仍只有 local/ssh，
 * 提交时映射成 type + repos（选了主机 ⇒ ssh，否则 local）。
 */
type FormKind = ProjectType | "multi";

export function ProjectForm({
  existing,
  preset,
  onClose,
}: {
  existing: Project | null;
  preset?: ProjectFormPreset;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const refreshProjects = useApp((s) => s.refreshProjects);
  const refreshHosts = useApp((s) => s.refreshHosts);
  const hosts = useApp((s) => s.hosts);
  const openHostForm = useApp((s) => s.openHostForm);

  const [kind, setKind] = useState<FormKind>(
    existing ? (existing.multi ? "multi" : existing.type) : (preset?.type ?? "local")
  );
  const [name, setName] = useState(existing?.name ?? "");
  const [workingDir, setWorkingDir] = useState(existing?.workingDir ?? "");
  /** 多仓库档的成员清单（成员路径原文，可手改可从选择器追加） */
  const [repos, setRepos] = useState<string[]>(
    existing?.multi?.repos.map((r) => r.dir) ?? []
  );
  /** 派生产物的成员由派生决定：只读展示，提交不带 repos（PUT 会拒） */
  const reposReadonly = Boolean(existing?.multi && existing.worktree);
  const [shell, setShell] = useState(existing?.shell ?? "");
  const [shellCustom, setShellCustom] = useState(false);
  const [shells, setShells] = useState<ShellsInfo | null>(null);
  const [hostId, setHostId] = useState(existing?.hostId ?? preset?.hostId ?? "");
  const [ssh, setSsh] = useState<SshFieldValues>({
    host: existing?.ssh?.host ?? "",
    port: existing?.ssh?.port ?? 22,
    username: existing?.ssh?.username ?? "",
    authMethod: existing?.ssh?.authMethod ?? "key",
    keyPath: existing?.ssh?.keyPath ?? "",
    secret: "",
  });
  const [pathHint, setPathHint] = useState<{ ok: boolean; text: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 从某台服务器进来：类型已定，直接选文件夹 */
  const locked = Boolean(!existing && preset?.type);
  const [picking, setPicking] = useState(locked);
  /** 多仓库档「添加仓库」的选择器（与 picking 互斥地打开） */
  const [pickingRepo, setPickingRepo] = useState(false);

  // 多仓库档映射回 shared 的 type：编辑时跟 existing 走（PUT 不许改类型），
  // 新建时看有没有选主机
  const type: ProjectType =
    kind === "multi" ? (existing ? existing.type : hostId ? "ssh" : "local") : kind;

  // 存量项目没有 hostId：可以继续手写 ssh，也可以改绑到已保存主机
  const legacy = Boolean(existing && !existing.hostId);
  const usingSavedHost = type === "ssh" && (Boolean(hostId) || !legacy);

  // 自动侦测宿主机可用的 shell：本地问后端本机；SSH 有目标（选了主机或编辑
  // 已有 SSH 项目）就问远端。失败静默——表单退回"默认 + 自定义"，不打断创建。
  const existingSshId = existing?.type === "ssh" ? existing.id : null;
  useEffect(() => {
    setShells(null);
    const opts =
      type === "local"
        ? undefined
        : hostId
          ? { hostId }
          : existingSshId
            ? { projectId: existingSshId }
            : null;
    if (opts === null) return;
    let stale = false;
    api.listShells(opts).then(
      (info) => {
        if (!stale) setShells(info);
      },
      () => {}
    );
    return () => {
      stale = true;
    };
  }, [type, hostId, existingSshId]);

  // 下拉里必须包含当前值，哪怕侦测没跑完或它不在侦测结果里（存量项目手填过）
  const shellOptions = (() => {
    const opts = shells ? [...shells.shells] : [];
    if (shell && !shellCustom && !opts.includes(shell)) opts.unshift(shell);
    return opts;
  })();
  const shellValue = shellCustom ? SHELL_CUSTOM : shell ? shell : SHELL_AUTO;
  const pickShell = (v: string) => {
    if (v === SHELL_AUTO) {
      setShell("");
      setShellCustom(false);
    } else if (v === SHELL_CUSTOM) {
      setShellCustom(true);
    } else {
      setShell(v);
      setShellCustom(false);
    }
  };
  const shellBaseName = (s: string) => s.split(/[\\/]/).pop() ?? s;

  const validatePath = async () => {
    if (!workingDir.trim()) {
      setPathHint(null);
      return;
    }
    try {
      const res = await api.validatePath(workingDir);
      setPathHint(
        res.ok
          ? { ok: true, text: t("project.pathOk") }
          : { ok: false, text: res.error ?? "?" }
      );
    } catch (err) {
      setPathHint({ ok: false, text: (err as Error).message });
    }
  };

  const folderName = (dir: string) =>
    dir.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";

  const save = async (next: { name: string; workingDir: string }) => {
    if (type === "ssh" && usingSavedHost && !hostId) {
      setError(t("host.required"));
      return;
    }
    const cleanRepos = repos.map((r) => r.trim()).filter(Boolean);
    if (kind === "multi" && !reposReadonly && cleanRepos.length === 0) {
      setError(t("multi.needRepo"));
      return;
    }
    setBusy(true);
    setError(null);
    const input: ProjectInput = {
      name: next.name,
      type,
      workingDir: next.workingDir || undefined,
      shell: shell || undefined,
      // 派生产物的成员由派生决定，不上送（PUT 会拒）；容器每次全量替换
      repos: kind === "multi" && !reposReadonly ? cleanRepos : undefined,
      hostId: type === "ssh" && hostId ? hostId : undefined,
      ssh:
        type === "ssh" && !hostId
          ? {
              host: ssh.host,
              port: ssh.port,
              username: ssh.username,
              authMethod: ssh.authMethod,
              keyPath: ssh.authMethod === "key" ? ssh.keyPath : undefined,
              secret: ssh.secret || undefined,
            }
          : undefined,
    };
    try {
      if (existing) {
        await api.updateProject(existing.id, input);
        await Promise.all([refreshProjects(), refreshHosts()]);
      } else {
        const created = await api.createProject(input);
        await Promise.all([refreshProjects(), refreshHosts()]);
        useApp.getState().selectProject(created.id);
      }
      onClose();
    } catch (err) {
      setError((err as Error).message);
      setPicking(false);
    } finally {
      setBusy(false);
    }
  };

  const pickFolder = (dir: string) => {
    if (busy) return;
    const base = folderName(dir);
    const nextName = name.trim() || base;
    setWorkingDir(dir);
    setPathHint({ ok: true, text: t("project.pathOk") });
    if (!existing && !name.trim() && base) setName(base);
    // 从服务器新建：选完目录就创建，不再问本机还是 SSH
    if (locked && nextName) {
      void save({ name: nextName, workingDir: dir });
      return;
    }
    setPicking(false);
  };

  const closePicker = () => {
    if (busy) return;
    if (locked && !workingDir) onClose();
    else setPicking(false);
  };

  /** 多仓库档「添加仓库」：选中即追加，不关外层表单；重复选中静默去重 */
  const pickRepoFolder = (dir: string) => {
    if (busy) return;
    setRepos((prev) => (prev.includes(dir) ? prev : [...prev, dir]));
    setPickingRepo(false);
  };
  const parentOf = (p: string) => {
    const s = p.replace(/[\\/]+$/, "");
    const i = Math.max(s.lastIndexOf("/"), s.lastIndexOf("\\"));
    return i > 0 ? s.slice(0, i) : s;
  };
  // 连续添加同级仓库是常态：选择器从上一个成员的父目录开起
  const lastRepo = repos.filter((r) => r.trim()).pop();
  const repoInitial = lastRepo ? parentOf(lastRepo) : "";

  const canBrowseRemote = Boolean(hostId || (existing && existing.type === "ssh"));
  const openPicker = () => {
    if (type === "ssh" && !canBrowseRemote) return;
    setPicking(true);
  };

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    void save({ name, workingDir });
  };

  const title = existing ? t("project.editTitle") : t("project.createTitle");
  const selected = hosts.find((h) => h.id === hostId);

  const anyPicking = picking || pickingRepo;
  return (
    <AppDialog
      title={
        anyPicking
          ? type === "ssh"
            ? t("project.pickRemoteTitle")
            : t("project.pickTitle")
          : title
      }
      onClose={picking ? closePicker : pickingRepo ? () => setPickingRepo(false) : onClose}
      lockOverlay
      wide={anyPicking}
      className={anyPicking ? "overflow-hidden" : undefined}
    >
      {anyPicking ? (
        <FolderPicker
          initialPath={pickingRepo ? repoInitial : workingDir}
          onSelect={pickingRepo ? pickRepoFolder : pickFolder}
          onClose={pickingRepo ? () => setPickingRepo(false) : closePicker}
          remote={type === "ssh"}
          confirming={busy}
          listDir={(dir) =>
            api.listDir(
              dir,
              existing?.type === "ssh"
                ? { projectId: existing.id }
                : hostId
                  ? { hostId }
                  : undefined
            )
          }
        />
      ) : (
        <form onSubmit={submit} className="grid gap-3">
        {!existing && !locked && (
          <Segmented
            value={kind}
            label={t("project.name")}
            onChange={setKind}
            options={[
              { value: "local", label: t("project.typeLocal") },
              { value: "ssh", label: t("project.typeSsh") },
              { value: "multi", label: t("multi.typeMulti") },
            ]}
          />
        )}

        <Field label={t("project.name")} htmlFor="project-name">
          <Input
            id="project-name"
            value={name}
            data-autofocus
            onChange={(e) => setName(e.target.value)}
          />
        </Field>

        {kind === "multi" ? (
          <>
            {!existing && (
              <Field
                label={t("multi.location")}
                hint={selected ? sshConn(selected) : undefined}
              >
                <div className="flex gap-2.5">
                  <Select
                    value={hostId || MULTI_LOCAL}
                    onValueChange={(v) => setHostId(v === MULTI_LOCAL ? "" : v)}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={MULTI_LOCAL}>{t("multi.locationLocal")}</SelectItem>
                      {hosts.map((h) => (
                        <SelectItem key={h.id} value={h.id}>
                          {h.name}
                          <span className="ml-2 font-mono text-xs text-muted-foreground">
                            {sshConn(h)}
                          </span>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => openHostForm(null, (h) => setHostId(h.id))}
                  >
                    {t("host.add")}
                  </Button>
                </div>
              </Field>
            )}

            <Field
              label={t("multi.repos")}
              hint={reposReadonly ? t("multi.reposReadonly") : undefined}
            >
              <div className="grid gap-1.5">
                {repos.map((r, i) => (
                  <div key={i} className="flex items-center gap-1">
                    <Input
                      className="flex-1 font-mono"
                      value={r}
                      readOnly={reposReadonly}
                      onChange={(e) =>
                        setRepos((prev) => prev.map((x, j) => (j === i ? e.target.value : x)))
                      }
                    />
                    {!reposReadonly && (
                      <>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          aria-label={t("multi.moveUp")}
                          disabled={i === 0}
                          onClick={() =>
                            setRepos((prev) => {
                              const next = [...prev];
                              [next[i - 1], next[i]] = [next[i]!, next[i - 1]!];
                              return next;
                            })
                          }
                        >
                          <ArrowUp className="size-4" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          aria-label={t("multi.moveDown")}
                          disabled={i === repos.length - 1}
                          onClick={() =>
                            setRepos((prev) => {
                              const next = [...prev];
                              [next[i], next[i + 1]] = [next[i + 1]!, next[i]!];
                              return next;
                            })
                          }
                        >
                          <ArrowDown className="size-4" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          aria-label={t("multi.removeRepo")}
                          onClick={() => setRepos((prev) => prev.filter((_, j) => j !== i))}
                        >
                          <X className="size-4" />
                        </Button>
                      </>
                    )}
                  </div>
                ))}
                {!reposReadonly && (
                  <Button
                    type="button"
                    variant="outline"
                    disabled={type === "ssh" && !canBrowseRemote}
                    onClick={() => setPickingRepo(true)}
                  >
                    {t("multi.addRepo")}
                  </Button>
                )}
              </div>
            </Field>

            {!reposReadonly && (
              <Field
                label={t("project.workingDir")}
                htmlFor="multi-cwd"
                hint={t("multi.workingDirHint")}
              >
                <div className="flex gap-2.5">
                  <Input
                    id="multi-cwd"
                    className="font-mono"
                    value={workingDir}
                    onChange={(e) => setWorkingDir(e.target.value)}
                  />
                  <Button
                    type="button"
                    variant="outline"
                    disabled={repos.every((r) => !r.trim())}
                    onClick={() => setWorkingDir(commonParentDir(repos))}
                  >
                    {t("multi.useCommonParent")}
                  </Button>
                </div>
              </Field>
            )}
          </>
        ) : type === "local" ? (
          <Field
            label={t("project.workingDir")}
            htmlFor="project-dir"
            hint={pathHint?.text}
            tone={pathHint ? (pathHint.ok ? "ok" : "err") : undefined}
          >
            <div className="flex gap-2.5">
              <Input
                id="project-dir"
                className="font-mono"
                value={workingDir}
                onChange={(e) => {
                  setWorkingDir(e.target.value);
                  setPathHint(null);
                }}
                onBlur={() => void validatePath()}
                placeholder="D:\code\my-project"
              />
              <Button variant="outline" type="button" onClick={() => setPicking(true)}>
                {t("project.browse")}
              </Button>
            </div>
          </Field>
        ) : (
          <>
            {!locked && (
              <Field
                label={legacy && !hostId ? t("host.bind") : t("host.title")}
                hint={
                  hosts.length === 0
                    ? t("host.emptyCreate")
                    : selected
                      ? sshConn(selected)
                      : undefined
                }
              >
                {hosts.length === 0 ? (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => openHostForm(null, (h) => setHostId(h.id))}
                  >
                    {t("host.add")}
                  </Button>
                ) : (
                  <div className="flex gap-2.5">
                    <Select value={hostId || undefined} onValueChange={setHostId}>
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder={t("host.pick")} />
                      </SelectTrigger>
                      <SelectContent>
                        {hosts.map((h) => (
                          <SelectItem key={h.id} value={h.id}>
                            {h.name}
                            <span className="ml-2 font-mono text-xs text-muted-foreground">
                              {sshConn(h)}
                            </span>
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => openHostForm(null, (h) => setHostId(h.id))}
                    >
                      {t("host.add")}
                    </Button>
                  </div>
                )}
              </Field>
            )}
            {legacy && !hostId && (
              <SshFields value={ssh} onChange={setSsh} hasSecret={existing?.ssh?.hasSecret} />
            )}
            <Field
              label={t("project.remoteDir")}
              htmlFor="ssh-dir"
              hint={
                pathHint?.text ??
                (!canBrowseRemote ? t("project.pickNeedHost") : undefined)
              }
              tone={pathHint ? (pathHint.ok ? "ok" : "err") : undefined}
            >
              <div className="flex gap-2.5">
                <Input
                  id="ssh-dir"
                  className="font-mono"
                  value={workingDir}
                  onChange={(e) => {
                    setWorkingDir(e.target.value);
                    setPathHint(null);
                  }}
                  placeholder="/home/user/project"
                />
                <Button
                  variant="outline"
                  type="button"
                  disabled={!canBrowseRemote}
                  onClick={openPicker}
                >
                  {t("project.browse")}
                </Button>
              </div>
            </Field>
          </>
        )}

        <Field label={t("project.shell")} htmlFor="project-shell">
          <div className="grid gap-2">
            <Select value={shellValue} onValueChange={pickShell}>
              <SelectTrigger id="project-shell" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={SHELL_AUTO}>
                  {shells
                    ? t("project.shellAutoDetected", {
                        shell: shellBaseName(shells.default),
                      })
                    : t("project.shellAuto")}
                </SelectItem>
                {shellOptions.map((s) => (
                  <SelectItem key={s} value={s} className="font-mono">
                    {s}
                  </SelectItem>
                ))}
                <SelectItem value={SHELL_CUSTOM}>{t("project.shellCustom")}</SelectItem>
              </SelectContent>
            </Select>
            {shellCustom && (
              <Input
                className="font-mono"
                value={shell}
                onChange={(e) => setShell(e.target.value)}
                placeholder={type === "local" ? "pwsh.exe / zsh" : "/usr/bin/zsh"}
              />
            )}
          </div>
        </Field>

        {error && <p className="text-[13px] text-destructive">{error}</p>}

        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button
            type="submit"
            disabled={busy || !name || (type === "ssh" && usingSavedHost && !hostId)}
          >
            {existing ? t("project.save") : t("project.create")}
          </Button>
        </div>
        </form>
      )}
    </AppDialog>
  );
}
