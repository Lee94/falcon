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
}
