import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type {
  MultiDeriveError,
  MultiRepoProbe,
  Project,
  RepoBranch,
  RepoInfo,
  WorktreeInput,
} from "@falcon/shared";
import { api, ApiRequestError } from "../api.js";
import { useApp, type WorktreeFormPreset } from "../store.js";
import { worktreeReasonText } from "../lib/reason.js";
import {
  actionBlocksSubmit,
  commonLocalBranches,
  evaluateMember,
  memberBasename,
  type MultiMode,
} from "../lib/multiDerive.js";
import { previewDir, previewMultiDir } from "../lib/worktreePath.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { AppDialog } from "./common/AppDialog.js";
import { Field, Segmented } from "./common/Field.js";

export function WorktreeForm({
  sourceId,
  preset,
  onClose,
}: {
  sourceId: string;
  preset?: WorktreeFormPreset;
  onClose: () => void;
}) {
  const project = useApp((s) => s.projects.find((p) => p.id === sourceId));
  // 多仓库容器走批量派生表单；单仓库路径与从前一字不差
  if (project?.multi) return <MultiWorktreeForm project={project} preset={preset} onClose={onClose} />;
  return <SingleWorktreeForm sourceId={sourceId} preset={preset} onClose={onClose} />;
}

function SingleWorktreeForm({
  sourceId,
  preset,
  onClose,
}: {
  sourceId: string;
  preset?: WorktreeFormPreset;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const project = useApp((s) => s.projects.find((p) => p.id === sourceId));
  const sourceName = project?.name ?? "";
  const defaultStart = project?.defaultWorktreeBranch || "HEAD";
  const refreshProjects = useApp((s) => s.refreshProjects);
  const refreshHosts = useApp((s) => s.refreshHosts);
  const toast = useApp((s) => s.toast);

  const [info, setInfo] = useState<RepoInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"new-branch" | "existing-branch">(preset?.mode ?? "new-branch");
  const [newBranch, setNewBranch] = useState(preset?.branch ?? "");
  const [startPoint, setStartPoint] = useState<string>(preset?.startPoint ?? defaultStart);
  const [pickedRef, setPickedRef] = useState("");
  const [name, setName] = useState(preset?.name ?? "");
  const [dir, setDir] = useState("");
  const [dirTouched, setDirTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    api
      .repoInfo(sourceId)
      .then((r) => {
        if (!alive) return;
        setInfo(r);
        const preferred = project?.defaultWorktreeBranch;
        const prefer = preferred
          ? r.branches.find(
              (b) =>
                !b.checkedOutAt && (b.name === preferred || b.localName === preferred)
            )
          : undefined;
        const first = r.branches.find((b) => !b.remote && !b.checkedOutAt);
        setPickedRef(
          prefer?.name ?? first?.name ?? r.branches.find((b) => !b.checkedOutAt)?.name ?? ""
        );
      })
      .catch((err) => alive && setLoadError((err as Error).message));
    return () => {
      alive = false;
    };
  }, [sourceId, project?.defaultWorktreeBranch]);

  const local = useMemo(() => (info?.branches ?? []).filter((b) => !b.remote), [info]);
  const remote = useMemo(() => (info?.branches ?? []).filter((b) => b.remote), [info]);
  const localNames = useMemo(() => new Set(local.map((b) => b.name)), [local]);
  const picked: RepoBranch | undefined = useMemo(
    () => info?.branches.find((b) => b.name === pickedRef),
    [info, pickedRef]
  );

  /** 当前会落到哪条**本地**分支上——目录名与项目名都跟着它 */
  const effectiveBranch =
    mode === "new-branch" ? newBranch.trim() : (picked?.localName ?? picked?.name ?? "");

  const autoDir =
    mode === "existing-branch" && picked
      ? picked.suggestedDir
      : previewDir(info?.repoDir ?? "", effectiveBranch);

  useEffect(() => {
    if (!dirTouched) setDir(autoDir);
  }, [autoDir, dirTouched]);

  const occupied = mode === "existing-branch" && picked?.dirOccupied && !dirTouched;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!effectiveBranch) return;
    setBusy(true);
    setError(null);

    // 远程分支收敛进 new-branch：直接把 origin/x 当 commit-ish 会得到 detached HEAD，
    // 而 detached worktree 里的提交在 remove 之后立刻不可达、可被 gc 回收。
    // 但如果同名本地分支已经存在，那就是普通的"检出已有分支"，不该再 -b 一次。
    const remotePick = mode === "existing-branch" && picked?.remote ? picked : null;
    const input: WorktreeInput =
      mode === "new-branch"
        ? { mode: "new-branch", branch: effectiveBranch, startPoint }
        : remotePick && !localNames.has(effectiveBranch)
          ? { mode: "new-branch", branch: effectiveBranch, startPoint: remotePick.name }
          : { mode: "existing-branch", branch: effectiveBranch };

    try {
      const created = await api.createWorktree(sourceId, {
        ...input,
        name: name.trim() || undefined,
        dir: dirTouched ? dir.trim() || undefined : undefined,
      });
      await Promise.all([refreshProjects(), refreshHosts()]);
      toast({
        kind: "success",
        title: t("worktree.created", { name: created.name }),
        body: t("worktree.createdBody", {
          dir: created.workingDir ?? "",
          branch: created.worktree?.branch ?? effectiveBranch,
        }),
      });
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const startPointLabel = info?.headBranch
    ? t("worktree.startPointHead", { ref: info.headBranch })
    : t("worktree.startPointDetached", { sha: info?.headSha ?? "?" });
  const startPointKnown =
    startPoint === "HEAD" ||
    local.some((b) => b.name === startPoint) ||
    remote.some((b) => b.name === startPoint);

  const branchOption = (b: RepoBranch) => (
    <SelectItem key={b.name} value={b.name} disabled={!!b.checkedOutAt}>
      {b.checkedOutAt
        ? `${b.name} · ${t("worktree.inUse", { path: b.checkedOutAt })}`
        : b.name}
    </SelectItem>
  );

  return (
    <AppDialog
      title={t("worktree.title", { name: sourceName })}
      onClose={onClose}
      lockOverlay
    >
      {!info && !loadError && (
        <p className="text-xs text-muted-foreground">{t("worktree.probing")}</p>
      )}
      {loadError && <p className="text-[13px] text-destructive">{loadError}</p>}

      {info && !info.derivable && (
        <>
          <p className="text-sm text-muted-foreground">{t("worktree.notDerivable")}</p>
          <p className="text-[13px] text-destructive">
            {worktreeReasonText(t, info.reason)}
            {info.detail ? ` · ${info.detail}` : ""}
          </p>
          <div className="flex justify-end">
            <Button variant="outline" type="button" onClick={onClose}>
              {t("common.close")}
            </Button>
          </div>
        </>
      )}

      {info?.derivable && (
        <form onSubmit={submit} className="grid gap-3">
          <p className="text-xs text-muted-foreground">
            {t("worktree.intro", { repo: info.repoDir })}
          </p>

          <Segmented
            value={mode}
            label={t("worktree.branch")}
            onChange={setMode}
            options={[
              { value: "new-branch", label: t("worktree.modeNew") },
              { value: "existing-branch", label: t("worktree.modeExisting") },
            ]}
          />

          {mode === "new-branch" ? (
            <>
              <Field label={t("worktree.branch")} htmlFor="wt-branch">
                <Input
                  id="wt-branch"
                  className="font-mono"
                  value={newBranch}
                  data-autofocus
                  onChange={(e) => setNewBranch(e.target.value)}
                  placeholder={t("worktree.branchPlaceholder")}
                />
              </Field>
              <Field label={t("worktree.startPoint")}>
                <Select value={startPoint} onValueChange={setStartPoint}>
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="HEAD">{startPointLabel}</SelectItem>
                    {!startPointKnown && startPoint && (
                      <SelectItem value={startPoint}>{startPoint}</SelectItem>
                    )}
                    {local.length > 0 && (
                      <SelectGroup>
                        <SelectLabel>{t("worktree.groupLocal")}</SelectLabel>
                        {local.map((b) => (
                          <SelectItem key={b.name} value={b.name}>
                            {b.name}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    )}
                    {remote.length > 0 && (
                      <SelectGroup>
                        <SelectLabel>{t("worktree.groupRemote")}</SelectLabel>
                        {remote.map((b) => (
                          <SelectItem key={b.name} value={b.name}>
                            {b.name}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    )}
                  </SelectContent>
                </Select>
              </Field>
            </>
          ) : (
            <Field label={t("worktree.pickBranch")}>
              <Select value={pickedRef} onValueChange={setPickedRef}>
                <SelectTrigger className="w-full" data-autofocus>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {local.length > 0 && (
                    <SelectGroup>
                      <SelectLabel>{t("worktree.groupLocal")}</SelectLabel>
                      {local.map(branchOption)}
                    </SelectGroup>
                  )}
                  {remote.length > 0 && (
                    <SelectGroup>
                      <SelectLabel>{t("worktree.groupRemote")}</SelectLabel>
                      {remote.map(branchOption)}
                    </SelectGroup>
                  )}
                </SelectContent>
              </Select>
            </Field>
          )}

          <Field
            label={t("worktree.targetDir")}
            htmlFor="wt-dir"
            hint={occupied ? t("worktree.dirOccupied") : t("worktree.dirHint")}
            tone={occupied ? "err" : undefined}
          >
            <Input
              id="wt-dir"
              className="font-mono"
              value={dir}
              onChange={(e) => {
                setDir(e.target.value);
                setDirTouched(true);
              }}
              placeholder={t("worktree.dirHint")}
            />
          </Field>

          <Field label={t("worktree.name")} htmlFor="wt-name">
            <Input
              id="wt-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={effectiveBranch || t("worktree.namePlaceholder")}
            />
          </Field>

          {error && <p className="text-[13px] text-destructive">{error}</p>}

          <div className="mt-1 flex justify-end gap-2">
            <Button variant="outline" type="button" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={busy || !effectiveBranch}>
              {busy ? t("worktree.creating") : t("worktree.create")}
            </Button>
          </div>
        </form>
      )}
    </AppDialog>
  );
}

/**
 * 多仓库容器的批量派生表单。
 *
 * 与单仓库表单刻意分开：探测形状（逐成员 RepoInfo）、模式集合（多一个 auto）、
 * 失败展示（成员归因 + 回滚残留）都不同，塞进一个组件只会互相绊脚。
 * 提交不带 dir——集中目录由服务端算，这里只渲染只读预览（漂移无害）。
 */
function MultiWorktreeForm({
  project,
  preset,
  onClose,
}: {
  project: Project;
  preset?: WorktreeFormPreset;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const refreshProjects = useApp((s) => s.refreshProjects);
  const refreshHosts = useApp((s) => s.refreshHosts);
  const toast = useApp((s) => s.toast);

  const [probe, setProbe] = useState<MultiRepoProbe | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<MultiMode>(preset?.mode ?? "auto");
  const [branchText, setBranchText] = useState(preset?.branch ?? "");
  const [pickedRef, setPickedRef] = useState("");
  const [name, setName] = useState(preset?.name ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    api
      .repoInfoMulti(project.id)
      .then((r) => alive && setProbe(r))
      .catch((err) => alive && setLoadError((err as Error).message));
    return () => {
      alive = false;
    };
  }, [project.id]);

  const branch = (mode === "existing-branch" ? pickedRef : branchText).trim();
  const common = useMemo(() => (probe ? commonLocalBranches(probe.members) : []), [probe]);
  const evals = useMemo(
    () =>
      (probe?.members ?? []).map((m) => ({
        ...m,
        action: branch ? evaluateMember(m.info, mode, branch) : null,
      })),
    [probe, mode, branch]
  );
  const blockedCount = evals.filter((m) =>
    m.action ? actionBlocksSubmit(m.action) : !m.info.derivable
  ).length;
  const central = previewMultiDir(probe?.baseDir ?? "", project.name, branch);

  const memberLine = (m: (typeof evals)[number]): { text: string; err: boolean } => {
    if (!m.action) {
      return m.info.derivable
        ? {
            text: t("multi.memberOk", {
              head: m.info.headBranch ?? m.info.headSha ?? "?",
            }),
            err: false,
          }
        : {
            text:
              worktreeReasonText(t, m.info.reason) +
              (m.info.detail ? ` · ${m.info.detail}` : ""),
            err: true,
          };
    }
    switch (m.action.kind) {
      case "blocked":
        return {
          text:
            worktreeReasonText(t, m.action.reason) +
            (m.action.detail ? ` · ${m.action.detail}` : ""),
          err: true,
        };
      case "create":
        return {
          text: project.defaultWorktreeBranch
            ? t("multi.willCreateFrom", { ref: project.defaultWorktreeBranch })
            : t("multi.willCreate"),
          err: false,
        };
      case "checkout":
        return { text: t("multi.willCheckout"), err: false };
      case "branch-exists":
        return { text: t("multi.memberBranchExists"), err: true };
      case "branch-missing":
        return { text: t("multi.memberBranchMissing"), err: true };
      case "branch-in-use":
        return { text: t("worktree.inUse", { path: m.action.at }), err: true };
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!branch || blockedCount > 0) return;
    setBusy(true);
    setError(null);
    try {
      const created = await api.createWorktree(project.id, {
        mode,
        branch,
        name: name.trim() || undefined,
      });
      await Promise.all([refreshProjects(), refreshHosts()]);
      toast({
        kind: "success",
        title: t("worktree.created", { name: created.name }),
        body: t("multi.createdBody", {
          dir: created.workingDir ?? "",
          n: created.multi?.repos.length ?? evals.length,
          branch: created.worktree?.branch ?? branch,
        }),
      });
      onClose();
    } catch (err) {
      // 服务端的成员归因错误体优先；读不出结构就原样显示 error 字符串
      const body =
        err instanceof ApiRequestError ? (err.body as Partial<MultiDeriveError> | undefined) : undefined;
      if (body?.member) {
        setError(
          t("multi.memberFailed", {
            repo: memberBasename(body.member.dir),
            reason: worktreeReasonText(t, body.member.reason),
          }) + (body.member.detail ? ` · ${body.member.detail}` : "")
        );
      } else {
        setError((err as Error).message);
      }
      if (body?.leftover?.length) {
        toast({
          kind: "warning",
          sticky: true,
          title: t("worktree.leftoverTitle"),
          body: body.leftover.join("\n"),
        });
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppDialog title={t("multi.deriveTitle", { name: project.name })} onClose={onClose} lockOverlay>
      {!probe && !loadError && (
        <p className="text-xs text-muted-foreground">{t("worktree.probing")}</p>
      )}
      {loadError && <p className="text-[13px] text-destructive">{loadError}</p>}

      {probe && (
        <form onSubmit={submit} className="grid gap-3">
          <p className="text-xs text-muted-foreground">
            {t("multi.intro", { n: probe.members.length })}
          </p>

          <div className="grid max-h-48 gap-1 overflow-y-auto rounded-md border p-2">
            {evals.map((m) => {
              const line = memberLine(m);
              return (
                <div key={m.dir} className="flex items-baseline justify-between gap-3 text-[13px]">
                  <span className="shrink-0 font-mono" title={m.dir}>
                    {memberBasename(m.info.repoDir ?? m.dir)}
                  </span>
                  <span
                    className={`truncate text-right text-xs ${
                      line.err ? "text-destructive" : "text-muted-foreground"
                    }`}
                    title={line.text}
                  >
                    {line.text}
                  </span>
                </div>
              );
            })}
          </div>

          <Segmented
            value={mode}
            label={t("worktree.branch")}
            onChange={setMode}
            options={[
              { value: "auto", label: t("multi.modeAuto") },
              { value: "new-branch", label: t("worktree.modeNew") },
              { value: "existing-branch", label: t("worktree.modeExisting") },
            ]}
          />

          {mode === "existing-branch" ? (
            <Field
              label={t("worktree.pickBranch")}
              hint={common.length === 0 ? t("multi.noCommonBranch") : undefined}
            >
              <Select value={pickedRef} onValueChange={setPickedRef}>
                <SelectTrigger className="w-full" data-autofocus>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {common.map((b) => (
                    <SelectItem key={b.name} value={b.name} disabled={!!b.usedAt}>
                      {b.usedAt ? `${b.name} · ${t("worktree.inUse", { path: b.usedAt })}` : b.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          ) : (
            <Field label={t("worktree.branch")} htmlFor="mwt-branch">
              <Input
                id="mwt-branch"
                className="font-mono"
                value={branchText}
                data-autofocus
                onChange={(e) => setBranchText(e.target.value)}
                placeholder={t("worktree.branchPlaceholder")}
              />
            </Field>
          )}

          {central && (
            <Field label={t("multi.dirPreview")} hint={t("multi.dirPreviewHint")}>
              <div className="rounded-md border bg-muted/30 px-3 py-2 font-mono text-xs">
                <div className="truncate" title={central}>
                  {central}
                </div>
                {evals.map((m) => (
                  <div key={m.dir} className="truncate pl-4 text-muted-foreground">
                    {memberBasename(m.info.repoDir ?? m.dir)}/
                  </div>
                ))}
              </div>
            </Field>
          )}

          <Field label={t("worktree.name")} htmlFor="mwt-name">
            <Input
              id="mwt-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={branch || t("worktree.namePlaceholder")}
            />
          </Field>

          {blockedCount > 0 && branch && (
            <p className="text-[13px] text-destructive">{t("multi.blockedBy", { n: blockedCount })}</p>
          )}
          {error && <p className="text-[13px] text-destructive">{error}</p>}

          <div className="mt-1 flex justify-end gap-2">
            <Button variant="outline" type="button" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={busy || !branch || blockedCount > 0}>
              {busy ? t("worktree.creating") : t("worktree.create")}
            </Button>
          </div>
        </form>
      )}
    </AppDialog>
  );
}
