package com.falcon.android.net

import com.falcon.android.bridge.FalconNative
import com.falcon.android.model.DashboardData
import com.falcon.android.model.JsonParsers
import com.falcon.android.model.RepoBranch
import com.falcon.android.model.ServerProfile
import com.falcon.android.model.ServerRoute
import com.falcon.android.model.SessionUi
import com.falcon.android.model.ServerUpdateCheck
import com.falcon.android.model.ServerUpdateTask
import com.falcon.android.model.NodeUpdateTarget
import com.falcon.android.model.NodeStatus
import com.falcon.android.model.TailnetProxy
import com.falcon.android.ui.FalconThemeTokens
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONArray
import org.json.JSONObject
import java.net.Inet4Address
import java.net.NetworkInterface
import java.net.URLEncoder
import java.util.concurrent.TimeUnit

object FalconApiClient {
    private val jsonMediaType = "application/json; charset=utf-8".toMediaType()

    val httpClient: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(8, TimeUnit.SECONDS)
        .readTimeout(25, TimeUnit.SECONDS)
        .writeTimeout(25, TimeUnit.SECONDS)
        .pingInterval(20, TimeUnit.SECONDS)
        .build()

    private val serverRoutes = java.util.concurrent.ConcurrentHashMap<String, ServerRoute>()
    private val tailnetClientLock = Any()
    private var cachedTailnetProxy: TailnetProxy? = null
    private var cachedTailnetClient: OkHttpClient? = null

    fun updateServerRoutes(profiles: List<ServerProfile>) {
        serverRoutes.clear()
        for (p in profiles) {
            serverRoutes[normalizeBaseUrl(p.url)] = p.route
        }
    }

    fun routeFor(baseUrl: String): ServerRoute =
        serverRoutes[normalizeBaseUrl(baseUrl)] ?: ServerRoute.AUTO

    fun clientFor(baseUrl: String, route: ServerRoute = routeFor(baseUrl)): OkHttpClient {
        return when (val resolved = TailnetManager.resolveRoute(baseUrl, route)) {
            is ResolvedRoute.Direct -> httpClient
            is ResolvedRoute.Unavailable -> throw RuntimeException(resolved.message)
            is ResolvedRoute.Tailnet -> tailnetHttpClient(resolved.proxy)
        }
    }

    private fun tailnetHttpClient(proxy: TailnetProxy): OkHttpClient = synchronized(tailnetClientLock) {
        val existing = cachedTailnetClient
        if (existing != null && cachedTailnetProxy == proxy) {
            return existing
        }
        val built = httpClient.newBuilder()
            .dns(TailnetDns)
            .socketFactory(TailnetSocketFactory(proxy))
            .build()
        cachedTailnetProxy = proxy
        cachedTailnetClient = built
        built
    }

    fun normalizeBaseUrl(raw: String): String {
        var trimmed = raw.trim().trimEnd('/')
        if (trimmed.isEmpty()) return "http://10.0.2.2:4923"
        if (!trimmed.startsWith("http://") && !trimmed.startsWith("https://")) {
            trimmed = "http://$trimmed"
        }
        return trimmed
    }

    fun toWebSocketUrl(baseUrl: String, path: String): String {
        val norm = normalizeBaseUrl(baseUrl)
        val wsBase = when {
            norm.startsWith("https://") -> "wss://" + norm.removePrefix("https://")
            norm.startsWith("http://") -> "ws://" + norm.removePrefix("http://")
            else -> "ws://$norm"
        }
        return "$wsBase$path"
    }

    private fun extractErrorMessage(bodyText: String, statusCode: Int): String {
        if (bodyText.isNotBlank()) {
            try {
                val obj = JSONObject(bodyText)
                val err = obj.optString("error")
                if (err.isNotBlank()) return err
                val msg = obj.optString("message")
                if (msg.isNotBlank()) return msg
            } catch (_: Exception) {
            }
        }
        return "HTTP $statusCode"
    }

    internal class HttpStatusException(val status: Int, message: String) : RuntimeException(message)

    private fun getRaw(baseUrl: String, path: String): String {
        val url = "${normalizeBaseUrl(baseUrl)}$path"
        val req = Request.Builder().url(url).get().build()
        clientFor(baseUrl).newCall(req).execute().use { resp ->
            val bodyStr = resp.body?.string() ?: ""
            if (!resp.isSuccessful) {
                throw HttpStatusException(resp.code, extractErrorMessage(bodyStr, resp.code))
            }
            return bodyStr
        }
    }

    private fun updatePath(hostId: String?): String = if (hostId == null) "/api/system/update"
        else "/api/hosts/${URLEncoder.encode(hostId, "UTF-8")}/node/system/update"

    suspend fun checkServerUpdate(baseUrl: String, hostId: String?): Result<ServerUpdateCheck> =
        withContext(Dispatchers.IO) {
            runCatching { ServerUpdateCheck.parse(JSONObject(getRaw(baseUrl, updatePath(hostId)))) }
        }

    suspend fun serverUpdateStatus(baseUrl: String, hostId: String?): Result<ServerUpdateTask> =
        withContext(Dispatchers.IO) {
            runCatching { ServerUpdateTask.parse(JSONObject(getRaw(baseUrl, "${updatePath(hostId)}/status"))) }
        }

    class UpdateRejectedException(message: String) : Exception(message)

    suspend fun startServerUpdate(baseUrl: String, hostId: String?, version: String): Result<ServerUpdateTask> =
        withContext(Dispatchers.IO) {
            runCatching {
                val req = Request.Builder().url("${normalizeBaseUrl(baseUrl)}${updatePath(hostId)}")
                    .post(JSONObject().put("version", version).toString().toRequestBody(jsonMediaType)).build()
                // 升级 POST 只发送一次；重启丢失应答时由调用方查询状态，不能由传输层重发。
                clientFor(baseUrl).newBuilder().retryOnConnectionFailure(false).build().newCall(req).execute().use { resp ->
                    val body = resp.body?.string().orEmpty()
                    if (!resp.isSuccessful) {
                        val message = extractErrorMessage(body, resp.code)
                        if (resp.code in 400..499 && resp.code != 409) throw UpdateRejectedException(message)
                        throw RuntimeException(message)
                    }
                    ServerUpdateTask.parse(JSONObject(body))
                }
            }
        }

    suspend fun fetchUpdateHosts(baseUrl: String): Result<List<NodeUpdateTarget>> = withContext(Dispatchers.IO) {
        runCatching {
            val list = JSONArray(getRaw(baseUrl, "/api/hosts"))
            (0 until list.length()).map { i ->
                val host = list.getJSONObject(i)
                NodeUpdateTarget(host.getString("id"), host.getString("name"),
                    "${host.optString("username")}@${host.optString("host")}:${host.optInt("port", 22)}")
            }
        }
    }

    suspend fun fetchNodeStatus(baseUrl: String): Result<List<NodeStatus>> = withContext(Dispatchers.IO) {
        runCatching {
            val list = JSONArray(getRaw(baseUrl, "/api/nodes"))
            (0 until list.length()).map { i ->
                val node = list.getJSONObject(i)
                NodeStatus(if (node.isNull("hostId")) null else node.optString("hostId"),
                    if (node.isNull("version")) null else node.optString("version"), node.getBoolean("online"),
                    if (node.isNull("error")) null else node.optString("error"))
            }
        }
    }

    suspend fun fetchDashboard(baseUrl: String): Result<DashboardData> = withContext(Dispatchers.IO) {
        runCatching {
            val projectsJson = getRaw(baseUrl, "/api/projects")
            val sessionsJson = getRaw(baseUrl, "/api/sessions")
            val hostsJson = runCatching { getRaw(baseUrl, "/api/hosts") }.getOrDefault("[]")
            // 列表请求会触发节点探测，因此随后读取状态。旧服务端没有 /api/nodes 时仍可使用列表。
            val nodes = fetchNodeStatus(baseUrl).getOrDefault(emptyList())
            val enrichedJson = FalconNative.buildDashboardView(projectsJson, sessionsJson, hostsJson)
            JsonParsers.parseDashboard(enrichedJson, projectsJson).withNodeStatuses(nodes)
        }
    }

    suspend fun fetchDetectedAgents(baseUrl: String, projectId: String): Result<List<String>> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(projectId, "UTF-8")
                val raw = getRaw(baseUrl, "/api/agents?projectId=$encodedId")
                val arr = JSONArray(raw)
                val out = ArrayList<String>(arr.length())
                for (i in 0 until arr.length()) {
                    out.add(arr.getString(i))
                }
                out
            }
        }

    suspend fun fetchEnvironmentBranches(baseUrl: String, hostId: String?, path: String): Result<List<RepoBranch>?> =
        withContext(Dispatchers.IO) {
            runCatching {
                val query = "?path=${URLEncoder.encode(path, "UTF-8")}" +
                    (hostId?.let { "&hostId=${URLEncoder.encode(it, "UTF-8")}" } ?: "")
                val raw = getRaw(baseUrl, "/api/environments/branches$query").trim()
                if (raw == "null") return@runCatching null
                val branches = JSONArray(raw)
                List(branches.length()) { i ->
                    val branch = branches.getJSONObject(i)
                    RepoBranch(branch.getString("name"), branch.getBoolean("head"))
                }
            }
        }

    suspend fun saveEnvironment(
        baseUrl: String,
        hostId: String?,
        id: String?,
        name: String,
        context: String,
        workingDir: String? = null,
        defaultWorktreeBranch: String? = null,
        legacyRepos: List<String>? = null
    ): Result<Unit> {
        val payload = JSONObject().put("name", name).put("environmentContext", context)
            .put("type", if (hostId == null) "local" else "ssh")
        if (workingDir != null) payload.put("workingDir", workingDir)
        if (defaultWorktreeBranch != null) payload.put("defaultWorktreeBranch", defaultWorktreeBranch)
        if (legacyRepos != null) payload.put("repos", JSONArray(legacyRepos))
        if (hostId != null) payload.put("hostId", hostId)
        val path = id?.let { "/api/projects/${URLEncoder.encode(it, "UTF-8")}" } ?: "/api/environments"
        return saveWorkspace(baseUrl, path, payload, id != null)
    }

    suspend fun createThread(baseUrl: String, env: String, name: String, task: String): Result<Unit> =
        saveWorkspace(baseUrl, "/api/projects/${URLEncoder.encode(env, "UTF-8")}/threads",
            JSONObject().put("name", name).put("task", task), false)

    private suspend fun saveWorkspace(baseUrl: String, path: String, data: JSONObject, edit: Boolean): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            val request = Request.Builder().url(normalizeBaseUrl(baseUrl) + path)
                .header("Idempotency-Key", java.util.UUID.randomUUID().toString())
                .method(if (edit) "PUT" else "POST", data.toString().toRequestBody(jsonMediaType)).build()
            clientFor(baseUrl).newCall(request).execute().use {
                if (!it.isSuccessful) error(extractErrorMessage(it.body.string(), it.code))
            }
        }
    }

    suspend fun createSession(
        baseUrl: String,
        projectId: String,
        name: String,
        agent: String?,
        theme: FalconThemeTokens
    ): Result<SessionUi> = withContext(Dispatchers.IO) {
        runCatching {
            val encodedId = URLEncoder.encode(projectId, "UTF-8")
            val payload = JSONObject().apply {
                if (name.isNotBlank()) put("name", name.trim())
                if (!agent.isNullOrBlank()) put("agent", agent)
                put("appearance", theme.appearance)
                put("background", theme.backgroundHex)
                put("foreground", theme.foregroundHex)
            }
            val req = Request.Builder()
                .url("${normalizeBaseUrl(baseUrl)}/api/projects/$encodedId/sessions")
                // 在逻辑请求构建时生成；OkHttp 内部重试复用同一个 Request 和请求头。
                .header("Idempotency-Key", java.util.UUID.randomUUID().toString())
                .post(payload.toString().toRequestBody(jsonMediaType))
                .build()
            clientFor(baseUrl).newCall(req).execute().use { resp ->
                val bodyStr = resp.body?.string() ?: ""
                if (!resp.isSuccessful) {
                    throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                }
                val sessionObj = JSONObject(bodyStr)
                // 通过 buildDashboardView 走统一的标签与状态计算
                val singleArray = JSONArray().put(sessionObj).toString()
                val dash = JsonParsers.parseDashboard(
                    FalconNative.buildDashboardView("[]", singleArray, "[]")
                )
                dash.allSessions.firstOrNull() ?: JsonParsers.parseSession(sessionObj)
            }
        }
    }

    suspend fun reattachSession(baseUrl: String, sessionId: String): Result<SessionUi> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(sessionId, "UTF-8")
                val req = Request.Builder()
                    .url("${normalizeBaseUrl(baseUrl)}/api/sessions/$encodedId/reattach")
                    .post("{}".toRequestBody(jsonMediaType))
                    .build()
                clientFor(baseUrl).newCall(req).execute().use { resp ->
                    val bodyStr = resp.body?.string() ?: ""
                    if (!resp.isSuccessful) {
                        throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                    }
                    val obj = JSONObject(bodyStr)
                    JsonParsers.parseSession(obj)
                }
            }
        }

    suspend fun checkForegroundBusy(baseUrl: String, sessionId: String): Pair<Boolean, String?> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(sessionId, "UTF-8")
                val raw = getRaw(baseUrl, "/api/sessions/$encodedId/foreground")
                val obj = JSONObject(raw)
                val busy = obj.optBoolean("busy", false)
                val cmd = if (obj.has("command") && !obj.isNull("command")) obj.optString("command") else null
                busy to cmd
            }.getOrDefault(false to null)
        }

    suspend fun terminateSession(baseUrl: String, sessionId: String): Result<Unit> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(sessionId, "UTF-8")
                val req = Request.Builder()
                    .url("${normalizeBaseUrl(baseUrl)}/api/sessions/$encodedId/terminate")
                    .post("{}".toRequestBody(jsonMediaType))
                    .build()
                clientFor(baseUrl).newCall(req).execute().use { resp ->
                    val bodyStr = resp.body?.string() ?: ""
                    if (!resp.isSuccessful) {
                        throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                    }
                }
            }
        }

    suspend fun deleteDeadSession(baseUrl: String, sessionId: String): Result<Unit> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(sessionId, "UTF-8")
                val req = Request.Builder()
                    .url("${normalizeBaseUrl(baseUrl)}/api/sessions/$encodedId")
                    .delete()
                    .build()
                clientFor(baseUrl).newCall(req).execute().use { resp ->
                    val bodyStr = resp.body?.string() ?: ""
                    if (!resp.isSuccessful) {
                        throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                    }
                }
            }
        }

    suspend fun renameSession(baseUrl: String, sessionId: String, newName: String): Result<Unit> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(sessionId, "UTF-8")
                val payload = JSONObject().put("name", newName.trim())
                val req = Request.Builder()
                    .url("${normalizeBaseUrl(baseUrl)}/api/sessions/$encodedId")
                    .patch(payload.toString().toRequestBody(jsonMediaType))
                    .build()
                clientFor(baseUrl).newCall(req).execute().use { resp ->
                    val bodyStr = resp.body?.string() ?: ""
                    if (!resp.isSuccessful) {
                        throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                    }
                }
            }
        }

    suspend fun pasteImage(
        baseUrl: String,
        sessionId: String,
        mimeType: String,
        bytes: ByteArray
    ): Result<String> = withContext(Dispatchers.IO) {
        runCatching {
            val encodedId = URLEncoder.encode(sessionId, "UTF-8")
            val mediaType = mimeType.ifBlank { "image/png" }.toMediaType()
            val req = Request.Builder()
                .url("${normalizeBaseUrl(baseUrl)}/api/sessions/$encodedId/paste-image")
                .post(bytes.toRequestBody(mediaType))
                .build()
            clientFor(baseUrl).newCall(req).execute().use { resp ->
                val bodyStr = resp.body?.string() ?: ""
                if (!resp.isSuccessful) {
                    throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                }
                val obj = JSONObject(bodyStr)
                val path = obj.optString("path")
                if (path.isBlank()) throw RuntimeException("服务端未返回图片路径")
                path
            }
        }
    }

    suspend fun answerAskpass(
        baseUrl: String,
        askpassId: String,
        password: String?,
        cancel: Boolean
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            val encodedId = URLEncoder.encode(askpassId, "UTF-8")
            val payload = JSONObject().apply {
                if (cancel) {
                    put("cancel", true)
                } else {
                    put("password", password ?: "")
                }
            }
            val req = Request.Builder()
                .url("${normalizeBaseUrl(baseUrl)}/api/askpass/$encodedId/answer")
                .post(payload.toString().toRequestBody(jsonMediaType))
                .build()
            clientFor(baseUrl).newCall(req).execute().use { resp ->
                val bodyStr = resp.body?.string() ?: ""
                if (!resp.isSuccessful) {
                    throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                }
            }
        }
    }

    suspend fun authorizeHostInstall(baseUrl: String, projectId: String): Result<Unit> =
        withContext(Dispatchers.IO) {
            runCatching {
                val encodedId = URLEncoder.encode(projectId, "UTF-8")
                val payload = JSONObject().put("authorized", true)
                val req = Request.Builder()
                    .url("${normalizeBaseUrl(baseUrl)}/api/projects/$encodedId/host")
                    .post(payload.toString().toRequestBody(jsonMediaType))
                    .build()
                httpClient.newCall(req).execute().use { resp ->
                    val bodyStr = resp.body?.string() ?: ""
                    if (!resp.isSuccessful) {
                        throw RuntimeException(extractErrorMessage(bodyStr, resp.code))
                    }
                }
            }
        }

    fun openSessionWebSocket(
        baseUrl: String,
        sessionId: String,
        listener: WebSocketListener
    ): WebSocket {
        val encodedId = URLEncoder.encode(sessionId, "UTF-8")
        val wsUrl = toWebSocketUrl(baseUrl, "/ws/sessions/$encodedId")
        val req = Request.Builder().url(wsUrl).build()
        val client = try {
            clientFor(baseUrl)
        } catch (e: RuntimeException) {
            val dummy = object : WebSocket {
                override fun request(): Request = req
                override fun queueSize(): Long = 0L
                override fun send(text: String): Boolean = false
                override fun send(bytes: okio.ByteString): Boolean = false
                override fun close(code: Int, reason: String?): Boolean = false
                override fun cancel() = Unit
            }
            listener.onFailure(dummy, e, null)
            return dummy
        }
        return client.newWebSocket(req, listener)
    }

    data class DiscoveredServer(
        val url: String,
        val platform: String,
        val version: String
    )

    suspend fun scanLanServers(port: Int = 4923): List<DiscoveredServer> = withContext(Dispatchers.IO) {
        val probeClient = httpClient.newBuilder()
            .connectTimeout(450, TimeUnit.MILLISECONDS)
            .readTimeout(600, TimeUnit.MILLISECONDS)
            .build()

        val prefixes = LinkedHashSet<String>()
        try {
            val interfaces = NetworkInterface.getNetworkInterfaces()
            while (interfaces != null && interfaces.hasMoreElements()) {
                val nif = interfaces.nextElement()
                if (!nif.isUp || nif.isLoopback || nif.isVirtual) continue
                val name = nif.name.lowercase()
                // 蜂窝与空接口不扫；tun 不能一刀切——Tailscale 在 Android 上就是 tun 网卡，
                // 其地址属于 CGNAT 段 100.64.0.0/10，在下方按地址放行；其它 tun（代理等）仍跳过。
                if (name.startsWith("rmnet") || name.startsWith("dummy")) continue
                val addrs = nif.inetAddresses
                while (addrs.hasMoreElements()) {
                    val addr = addrs.nextElement()
                    if (addr is Inet4Address && !addr.isLoopbackAddress && !addr.isLinkLocalAddress) {
                        val parts = (addr.hostAddress ?: continue).split('.')
                        if (parts.size != 4) continue
                        if (parts[0] == "198" && (parts[1] == "18" || parts[1] == "19")) continue
                        val second = parts[1].toIntOrNull() ?: continue
                        val isTailscale = parts[0] == "100" && second in 64..127
                        if (name.startsWith("tun") && !isTailscale) continue
                        prefixes.add("${parts[0]}.${parts[1]}.${parts[2]}")
                    }
                }
            }
        } catch (_: Exception) {
        }

        if (prefixes.isEmpty()) return@withContext emptyList()

        val sem = Semaphore(48)
        coroutineScope {
            val jobs = prefixes.flatMap { prefix ->
                (1..254).map { host ->
                    val candidateUrl = "http://$prefix.$host:$port"
                    async {
                        sem.withPermit {
                            runCatching {
                                val req = Request.Builder().url("$candidateUrl/api/system").get().build()
                                probeClient.newCall(req).execute().use { resp ->
                                    if (!resp.isSuccessful) return@use null
                                    val body = resp.body?.string() ?: return@use null
                                    val obj = JSONObject(body)
                                    val version = obj.optString("version")
                                    val platform = obj.optString("platform")
                                    if (version.isBlank() && platform.isBlank()) return@use null
                                    DiscoveredServer(
                                        url = candidateUrl,
                                        platform = platform.ifBlank { "unknown" },
                                        version = version.ifBlank { "?" }
                                    )
                                }
                            }.getOrNull()
                        }
                    }
                }
            }
            jobs.awaitAll().filterNotNull()
        }
    }
}
