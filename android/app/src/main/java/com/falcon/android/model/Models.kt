package com.falcon.android.model

import org.json.JSONArray
import org.json.JSONObject

data class ServerProfile(
    val id: String,
    val name: String,
    val url: String
)

data class ProgramBadgeUi(
    val tone: String,
    val label: String,
    val shortLabel: String,
    val progress: Int?,
    val detail: String?
)

data class SessionUi(
    val id: String,
    val projectId: String,
    val projectName: String,
    val projectType: String,
    val name: String,
    val title: String?,
    val displayLabel: String,
    val state: String,
    val agent: String?,
    val durable: Boolean,
    val nonDurableReason: String?,
    val nonDurableText: String?,
    val deadReason: String?,
    val deadReasonText: String?,
    val badge: ProgramBadgeUi?,
    val createdAt: Long,
    val lastActiveAt: Long
)

data class ProjectUi(
    val id: String,
    val name: String,
    val type: String,
    val workingDir: String,
    val branch: String?,
    val isWorktree: Boolean,
    val isMulti: Boolean,
    val sessions: List<SessionUi>
)

data class FolderGroupUi(
    val project: ProjectUi,
    val worktrees: List<ProjectUi>
)

data class ServerGroupUi(
    val key: String,
    val kind: String,
    val name: String,
    val conn: String?,
    val standaloneId: String?,
    val standaloneSessions: List<SessionUi>,
    val folders: List<FolderGroupUi>
)

data class DashboardData(
    val servers: List<ServerGroupUi>,
    val allSessions: List<SessionUi>
)

data class AskpassPromptUi(
    val id: String,
    val prompt: String
)

data class ThemeItemUi(
    val name: String,
    val isDark: Boolean,
    val backgroundHex: String,
    val foregroundHex: String,
    val accentHex: String
)

data class AgentOption(
    val id: String?,
    val label: String
)

data class AgentSlashCommandUi(
    val name: String,
    val description: String
)

data class AgentModelOptionUi(
    val id: String,
    val label: String,
    val resolvedModel: String?,
    val reasoningEfforts: List<String>,
    val defaultReasoningEffort: String?
)

data class AgentPlanItemUi(
    val content: String,
    val status: String
)

sealed class AgentToolContentUi {
    data class Text(val text: String) : AgentToolContentUi()
    data class Diff(val path: String, val unifiedDiff: String) : AgentToolContentUi()
    data class Terminal(val command: String, val output: String, val exitCode: Int?) : AgentToolContentUi()
}

data class AgentToolCallUi(
    val id: String,
    val kind: String,
    val title: String,
    val description: String?,
    val status: String,
    val content: List<AgentToolContentUi>,
    val filePath: String?,
    val permissionPending: Boolean,
    val permissionReason: String?
)

sealed class AgentEntryUi {
    abstract val id: String

    data class User(override val id: String, val text: String) : AgentEntryUi()
    data class Thought(override val id: String, val text: String) : AgentEntryUi()
    data class Message(override val id: String, val text: String) : AgentEntryUi()
    data class ToolCall(val call: AgentToolCallUi) : AgentEntryUi() {
        override val id: String get() = call.id
    }
    data class Plan(override val id: String, val items: List<AgentPlanItemUi>) : AgentEntryUi()
    data class Notice(override val id: String, val level: String, val text: String) : AgentEntryUi()
}

data class AgentViewStateUi(
    val agent: String,
    val active: Boolean,
    val status: String,
    val model: String?,
    val permissionMode: String?,
    val cwd: String?,
    val commands: List<AgentSlashCommandUi>,
    val models: List<String>,
    val entries: List<AgentEntryUi>,
    val modelOptions: List<AgentModelOptionUi> = emptyList(),
    val reasoningEffort: String? = null,
    val terminalBacked: Boolean = false
) {
    // 旧镜像只有历史内容，不能把它当成可交互 GUI 并隐藏真实终端。
    val interactive: Boolean get() = active && !terminalBacked

    // 旧服务端只提供模型名称时保留选择能力，思考程度仍以真实能力目录为准。
    fun modelChoices(): List<AgentModelOptionUi> = modelOptions.ifEmpty {
        models.filter { it.isNotBlank() }.map {
            AgentModelOptionUi(it, it, null, emptyList(), null)
        }
    }

    fun selectedModelOption(): AgentModelOptionUi? = model?.let { current ->
        modelOptions.firstOrNull { it.id == current || it.resolvedModel == current }
    }

    fun upsertEntry(entry: AgentEntryUi): AgentViewStateUi {
        val next = entries.toMutableList()
        val idx = next.indexOfFirst { it.id == entry.id && it::class == entry::class }
        if (idx >= 0) {
            next[idx] = entry
        } else {
            next.add(entry)
        }
        return copy(entries = next)
    }

    fun applyDelta(id: String, deltaKind: String, delta: String): AgentViewStateUi {
        if (delta.isEmpty()) return this
        val next = entries.toMutableList()
        when (deltaKind) {
            "thought" -> {
                val idx = next.indexOfLast { it is AgentEntryUi.Thought && it.id == id }
                if (idx >= 0) {
                    val cur = next[idx] as AgentEntryUi.Thought
                    next[idx] = cur.copy(text = cur.text + delta)
                } else {
                    next.add(AgentEntryUi.Thought(id = id, text = delta))
                }
            }
            "message" -> {
                val idx = next.indexOfLast { it is AgentEntryUi.Message && it.id == id }
                if (idx >= 0) {
                    val cur = next[idx] as AgentEntryUi.Message
                    next[idx] = cur.copy(text = cur.text + delta)
                } else {
                    next.add(AgentEntryUi.Message(id = id, text = delta))
                }
            }
            "tool_output" -> {
                val idx = next.indexOfLast { it is AgentEntryUi.ToolCall && it.id == id }
                if (idx >= 0) {
                    val cur = (next[idx] as AgentEntryUi.ToolCall).call
                    val nextContent = cur.content.toMutableList()
                    val lastTermIdx = nextContent.indexOfLast { it is AgentToolContentUi.Terminal }
                    if (lastTermIdx >= 0) {
                        val term = nextContent[lastTermIdx] as AgentToolContentUi.Terminal
                        nextContent[lastTermIdx] = term.copy(output = term.output + delta)
                    } else {
                        nextContent.add(AgentToolContentUi.Terminal(command = cur.title, output = delta, exitCode = null))
                    }
                    next[idx] = AgentEntryUi.ToolCall(cur.copy(content = nextContent))
                }
            }
        }
        return copy(entries = next)
    }
}

object AgentCatalog {
    private val LABELS = mapOf(
        "claude" to "Claude Code",
        "codex" to "Codex",
        "grok" to "Grok",
        "devin" to "Devin",
        "gemini" to "Gemini CLI",
        "opencode" to "OpenCode",
        "cursor-agent" to "Cursor Agent",
        "copilot" to "GitHub Copilot",
        "aider" to "Aider",
        "amp" to "Amp",
        "qwen" to "Qwen Code",
        "droid" to "Factory Droid",
        "agy" to "Antigravity CLI",
        "omp" to "Oh My Pi"
    )

    fun labelFor(agentId: String): String = LABELS[agentId] ?: agentId
}

object JsonParsers {
    fun parseBadge(obj: JSONObject?): ProgramBadgeUi? {
        if (obj == null) return null
        val progress = if (obj.has("progress") && !obj.isNull("progress")) obj.optInt("progress") else null
        val detail = if (obj.has("detail") && !obj.isNull("detail")) obj.optString("detail") else null
        return ProgramBadgeUi(
            tone = obj.optString("tone", "working"),
            label = obj.optString("label", "运行中"),
            shortLabel = obj.optString("shortLabel", "运行中"),
            progress = progress,
            detail = detail
        )
    }

    fun parseSession(obj: JSONObject): SessionUi {
        val title = if (obj.has("title") && !obj.isNull("title")) obj.optString("title") else null
        val agent = if (obj.has("agent") && !obj.isNull("agent")) obj.optString("agent") else null
        val nonDurableReason = if (obj.has("nonDurableReason") && !obj.isNull("nonDurableReason")) obj.optString("nonDurableReason") else null
        val nonDurableText = if (obj.has("nonDurableText") && !obj.isNull("nonDurableText")) obj.optString("nonDurableText") else null
        val deadReason = if (obj.has("deadReason") && !obj.isNull("deadReason")) obj.optString("deadReason") else null
        val deadReasonText = if (obj.has("deadReasonText") && !obj.isNull("deadReasonText")) obj.optString("deadReasonText") else null
        val badge = parseBadge(obj.optJSONObject("badge"))
        return SessionUi(
            id = obj.optString("id"),
            projectId = obj.optString("projectId"),
            projectName = obj.optString("projectName", "项目"),
            projectType = obj.optString("projectType", "local"),
            name = obj.optString("name", ""),
            title = title,
            displayLabel = obj.optString("displayLabel", "shell"),
            state = obj.optString("state", "active"),
            agent = agent,
            durable = obj.optBoolean("durable", false),
            nonDurableReason = nonDurableReason,
            nonDurableText = nonDurableText,
            deadReason = deadReason,
            deadReasonText = deadReasonText,
            badge = badge,
            createdAt = obj.optLong("createdAt", 0L),
            lastActiveAt = obj.optLong("lastActiveAt", 0L)
        )
    }

    private fun parseSessionList(arr: JSONArray?): List<SessionUi> {
        if (arr == null) return emptyList()
        val list = ArrayList<SessionUi>(arr.length())
        for (i in 0 until arr.length()) {
            val item = arr.optJSONObject(i) ?: continue
            list.add(parseSession(item))
        }
        return list
    }

    private fun parseProject(obj: JSONObject): ProjectUi {
        val branch = if (obj.has("branch") && !obj.isNull("branch")) obj.optString("branch") else null
        return ProjectUi(
            id = obj.optString("id"),
            name = obj.optString("name"),
            type = obj.optString("type", "local"),
            workingDir = obj.optString("workingDir", ""),
            branch = branch,
            isWorktree = obj.optBoolean("isWorktree", false),
            isMulti = obj.optBoolean("isMulti", false),
            sessions = parseSessionList(obj.optJSONArray("sessions"))
        )
    }

    fun parseDashboard(rawJson: String): DashboardData {
        if (rawJson.isBlank()) return DashboardData(emptyList(), emptyList())
        return try {
            val root = JSONObject(rawJson)
            val serversArr = root.optJSONArray("servers") ?: JSONArray()
            val servers = ArrayList<ServerGroupUi>(serversArr.length())
            for (i in 0 until serversArr.length()) {
                val sObj = serversArr.optJSONObject(i) ?: continue
                val foldersArr = sObj.optJSONArray("folders") ?: JSONArray()
                val folders = ArrayList<FolderGroupUi>(foldersArr.length())
                for (j in 0 until foldersArr.length()) {
                    val fObj = foldersArr.optJSONObject(j) ?: continue
                    val pObj = fObj.optJSONObject("project") ?: continue
                    val wtArr = fObj.optJSONArray("worktrees") ?: JSONArray()
                    val worktrees = ArrayList<ProjectUi>(wtArr.length())
                    for (k in 0 until wtArr.length()) {
                        val wObj = wtArr.optJSONObject(k) ?: continue
                        worktrees.add(parseProject(wObj))
                    }
                    folders.add(
                        FolderGroupUi(
                            project = parseProject(pObj),
                            worktrees = worktrees
                        )
                    )
                }
                val conn = if (sObj.has("conn") && !sObj.isNull("conn")) sObj.optString("conn") else null
                val standaloneId = if (sObj.has("standaloneId") && !sObj.isNull("standaloneId")) sObj.optString("standaloneId") else null
                servers.add(
                    ServerGroupUi(
                        key = sObj.optString("key"),
                        kind = sObj.optString("kind", "local"),
                        name = sObj.optString("name", "本机"),
                        conn = conn,
                        standaloneId = standaloneId,
                        standaloneSessions = parseSessionList(sObj.optJSONArray("standaloneSessions")),
                        folders = folders
                    )
                )
            }
            val allSessions = parseSessionList(root.optJSONArray("allSessions"))
            DashboardData(servers = servers, allSessions = allSessions)
        } catch (_: Exception) {
            DashboardData(emptyList(), emptyList())
        }
    }

    fun parseStringArray(rawJson: String): List<String> {
        if (rawJson.isBlank()) return emptyList()
        return try {
            val arr = JSONArray(rawJson)
            val out = ArrayList<String>(arr.length())
            for (i in 0 until arr.length()) {
                out.add(arr.getString(i))
            }
            out
        } catch (_: Exception) {
            emptyList()
        }
    }

    fun parseThemeList(rawJson: String): List<ThemeItemUi> {
        if (rawJson.isBlank()) return emptyList()
        return try {
            val arr = JSONArray(rawJson)
            val out = ArrayList<ThemeItemUi>(arr.length())
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                out.add(
                    ThemeItemUi(
                        name = o.optString("name"),
                        isDark = o.optBoolean("isDark", true),
                        backgroundHex = o.optString("background", "#0a0a0a"),
                        foregroundHex = o.optString("foreground", "#fafafa"),
                        accentHex = o.optString("accent", "#3465a4")
                    )
                )
            }
            out
        } catch (_: Exception) {
            emptyList()
        }
    }

    fun parseAgentEntry(obj: JSONObject?): AgentEntryUi? {
        if (obj == null) return null
        // AgentEntry 的标签是 type；工具调用自己的 kind 则是 execute/edit 等能力类别。
        return when (obj.optString("type")) {
            "user" -> AgentEntryUi.User(
                id = obj.optString("id"),
                text = obj.optString("text")
            )
            "thought" -> AgentEntryUi.Thought(
                id = obj.optString("id"),
                text = obj.optString("text")
            )
            "message" -> AgentEntryUi.Message(
                id = obj.optString("id"),
                text = obj.optString("text")
            )
            "tool_call" -> {
                val contentArr = obj.optJSONArray("content") ?: JSONArray()
                val content = ArrayList<AgentToolContentUi>(contentArr.length())
                for (i in 0 until contentArr.length()) {
                    val cObj = contentArr.optJSONObject(i) ?: continue
                    when (cObj.optString("type")) {
                        "text" -> content.add(AgentToolContentUi.Text(cObj.optString("text")))
                        "diff" -> content.add(
                            AgentToolContentUi.Diff(
                                path = cObj.optString("path"),
                                unifiedDiff = cObj.optString("unifiedDiff")
                            )
                        )
                        "terminal" -> {
                            val exitCode = if (cObj.has("exitCode") && !cObj.isNull("exitCode")) {
                                cObj.optInt("exitCode")
                            } else {
                                null
                            }
                            content.add(
                                AgentToolContentUi.Terminal(
                                    command = cObj.optString("command"),
                                    output = cObj.optString("output"),
                                    exitCode = exitCode
                                )
                            )
                        }
                    }
                }
                val desc = if (obj.has("description") && !obj.isNull("description")) obj.optString("description") else null
                val fp = if (obj.has("filePath") && !obj.isNull("filePath")) obj.optString("filePath") else null
                val reason = if (obj.has("permissionReason") && !obj.isNull("permissionReason")) obj.optString("permissionReason") else null
                AgentEntryUi.ToolCall(
                    AgentToolCallUi(
                        id = obj.optString("id"),
                        kind = obj.optString("kind", "other"),
                        title = obj.optString("title"),
                        description = desc,
                        status = obj.optString("status", "in_progress"),
                        content = content,
                        filePath = fp,
                        permissionPending = obj.optBoolean("permissionPending", false),
                        permissionReason = reason
                    )
                )
            }
            "plan" -> {
                val itemsArr = obj.optJSONArray("items") ?: JSONArray()
                val items = ArrayList<AgentPlanItemUi>(itemsArr.length())
                for (i in 0 until itemsArr.length()) {
                    val iObj = itemsArr.optJSONObject(i) ?: continue
                    items.add(
                        AgentPlanItemUi(
                            content = iObj.optString("content"),
                            status = iObj.optString("status", "pending")
                        )
                    )
                }
                AgentEntryUi.Plan(id = obj.optString("id"), items = items)
            }
            "notice" -> AgentEntryUi.Notice(
                id = obj.optString("id"),
                level = obj.optString("level", "info"),
                text = obj.optString("text")
            )
            else -> null
        }
    }

    fun parseSlashCommands(arr: JSONArray?): List<AgentSlashCommandUi> {
        if (arr == null) return emptyList()
        val list = ArrayList<AgentSlashCommandUi>(arr.length())
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            list.add(
                AgentSlashCommandUi(
                    name = o.optString("name"),
                    description = o.optString("description")
                )
            )
        }
        return list
    }

    fun parseStringList(arr: JSONArray?): List<String> {
        if (arr == null) return emptyList()
        val list = ArrayList<String>(arr.length())
        for (i in 0 until arr.length()) {
            val s = arr.optString(i)
            if (s.isNotEmpty()) list.add(s)
        }
        return list
    }

    fun parseModelOptions(arr: JSONArray?): List<AgentModelOptionUi> {
        if (arr == null) return emptyList()
        val list = ArrayList<AgentModelOptionUi>(arr.length())
        for (i in 0 until arr.length()) {
            val obj = arr.optJSONObject(i) ?: continue
            val id = obj.optString("id")
            if (id.isBlank()) continue
            list.add(AgentModelOptionUi(
                id = id,
                label = obj.optString("label").ifBlank { id },
                resolvedModel = if (obj.isNull("resolvedModel")) null else obj.optString("resolvedModel"),
                reasoningEfforts = parseStringList(obj.optJSONArray("reasoningEfforts")),
                defaultReasoningEffort = if (obj.isNull("defaultReasoningEffort")) null else obj.optString("defaultReasoningEffort")
            ))
        }
        return list
    }

    fun parseAgentSnapshot(obj: JSONObject?): AgentViewStateUi? {
        if (obj == null) return null
        val entriesArr = obj.optJSONArray("entries") ?: JSONArray()
        val entries = ArrayList<AgentEntryUi>(entriesArr.length())
        for (i in 0 until entriesArr.length()) {
            val e = parseAgentEntry(entriesArr.optJSONObject(i))
            if (e != null) entries.add(e)
        }
        val model = if (obj.has("model") && !obj.isNull("model")) obj.optString("model") else null
        val permMode = if (obj.has("permissionMode") && !obj.isNull("permissionMode")) obj.optString("permissionMode") else null
        val cwd = if (obj.has("cwd") && !obj.isNull("cwd")) obj.optString("cwd") else null
        return AgentViewStateUi(
            agent = obj.optString("agent", "claude"),
            active = obj.optBoolean("active", true),
            status = obj.optString("status", "idle"),
            model = model,
            permissionMode = permMode,
            cwd = cwd,
            commands = parseSlashCommands(obj.optJSONArray("commands")),
            models = parseStringList(obj.optJSONArray("models")),
            modelOptions = parseModelOptions(obj.optJSONArray("modelOptions")),
            reasoningEffort = if (obj.isNull("reasoningEffort")) null else obj.optString("reasoningEffort"),
            terminalBacked = obj.optBoolean("terminalBacked", false),
            entries = entries
        )
    }
}
