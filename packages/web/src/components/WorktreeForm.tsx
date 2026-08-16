import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { RepoBranch, RepoInfo, WorktreeInput } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { worktreeReasonText } from "../lib/reason.js";
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

/**
 * 目标目录的**预览**。
 *
 * 权威实现在后端 git/path.ts（siblingWorktreePath + branchSlug），这里只是让用户
 * 在敲分支名时看得见结果。因此提交时只有用户**手动改过**目录才把 dir 发上去，
 * 没改就留空由服务端自己算——预览与真实值万一漂移，也绝不会让我们建到别处去。
 */
function previewDir(repoDir: string, branch: string): string {
  if (!repoDir || !branch) return "";
  const sep = repoDir.includes("\\") ? "\\" : "/";
  const root = repoDir.replace(/[\\/]+$/, "");
  const i = Math.max(root.lastIndexOf("\\"), root.lastIndexOf("/"));
  const parent = i <= 0 ? root : root.slice(0, i);
  const base = root.slice(i + 1);
  const slug =
    Array.from(
      branch
        .replace(/[/"<>|\s]+/g, "-")
        .replace(/-{2,}/g, "-")
        .replace(/^[-.]+/, "")
        .replace(/[-.]+$/, "")
    )
      .slice(0, 48)
      .join("")
      .replace(/[-.]+$/, "") || "wt";
  return `${parent}${sep}${base}-${slug}`;
}

export function WorktreeForm({ sourceId, onClose }: { sourceId: string; onClose: () => void }) {
  const { t } = useTranslation();
  const sourceName = useApp((s) => s.projects.find((p) => p.id === sourceId)?.name ?? "");
  const refreshProjects = useApp((s) => s.refreshProjects);
  const toast = useApp((s) => s.toast);

  const [info, setInfo] = useState<RepoInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"new-branch" | "existing-branch">("new-branch");
  const [newBranch, setNewBranch] = useState("");
  const [startPoint, setStartPoint] = useState("HEAD");
  const [pickedRef, setPickedRef] = useState("");
  const [name, setName] = useState("");
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
        const first = r.branches.find((b) => !b.remote && !b.checkedOutAt);
        setPickedRef(first?.name ?? r.branches.find((b) => !b.checkedOutAt)?.name ?? "");
      })
      .catch((err) => alive && setLoadError((err as Error).message));
    return () => {
      alive = false;
    };
  }, [sourceId]);

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
      await refreshProjects();
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
