package com.falcon.android.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.os.Build
import com.falcon.android.bridge.FalconNative
import com.falcon.android.model.ServerRoute
import com.falcon.android.model.TailnetProbeHit
import com.falcon.android.model.TailnetProxy
import com.falcon.android.model.TailnetStatus
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.URI
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

data class TailnetUiState(
    val libraryAvailable: Boolean = false,
    val enabled: Boolean = false,
    val busy: Boolean = false,
    val hostname: String = "",
    val defaultHostname: String = "falcon-android",
    val controlUrl: String = "",
    val status: TailnetStatus = TailnetStatus.STOPPED,
    val proxy: TailnetProxy? = null,
    val lastError: String? = null
) {
    val effectiveHostname: String
        get() = sanitizeHostname(hostname).ifEmpty { defaultHostname }
}

sealed interface ResolvedRoute {
    data object Direct : ResolvedRoute
    data class Tailnet(val proxy: TailnetProxy) : ResolvedRoute
    data class Unavailable(val message: String) : ResolvedRoute
}

private fun sanitizeHostname(raw: String): String {
    val sb = StringBuilder()
    var prevDash = false
    for (ch in raw.trim()) {
        if (ch in 'a'..'z' || ch in 'A'..'Z' || ch in '0'..'9') {
            sb.append(ch.lowercaseChar())
            prevDash = false
        } else if ((ch == '-' || ch == '_' || ch == ' ') && sb.isNotEmpty() && !prevDash) {
            sb.append('-')
            prevDash = true
        }
        if (sb.length >= 63) break
    }
    while (sb.endsWith('-')) sb.deleteCharAt(sb.length - 1)
    return sb.toString()
}

object TailnetManager {
    private const val PREFS_NAME = "falcon_tailnet_prefs"
    private const val KEY_ENABLED = "enabled"
    private const val KEY_HOSTNAME = "hostname"
    private const val KEY_CONTROL_URL = "control_url"
    private const val KEY_AUTH_KEY = "auth_key"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val initialized = AtomicBoolean(false)

    private lateinit var appContext: Context
    private var pollJob: Job? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    private val _state = MutableStateFlow(TailnetUiState())
    val state: StateFlow<TailnetUiState> = _state.asStateFlow()

    fun init(context: Context) {
        if (!initialized.compareAndSet(false, true)) return
        appContext = context.applicationContext
        val libAvailable = File(appContext.applicationInfo.nativeLibraryDir, "libfalcon_tailnet.so").isFile
        val prefs = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val enabled = prefs.getBoolean(KEY_ENABLED, false)
        val hostname = prefs.getString(KEY_HOSTNAME, "") ?: ""
        val controlUrl = prefs.getString(KEY_CONTROL_URL, "") ?: ""
        val model = sanitizeHostname(Build.MODEL ?: "").ifEmpty { "android" }
        val defaultHost = sanitizeHostname("falcon-$model").ifEmpty { "falcon-android" }

        _state.value = TailnetUiState(
            libraryAvailable = libAvailable,
            enabled = enabled,
            hostname = hostname,
            defaultHostname = defaultHost,
            controlUrl = controlUrl
        )

        registerNetworkCallback()
        if (enabled && libAvailable) {
            startNode()
        }
    }

    private fun stateDir(): File = File(appContext.filesDir, "tailnet")

    private fun registerNetworkCallback() {
        val cm = appContext.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager ?: return
        runCatching {
            cm.activeNetwork?.let { net ->
                cm.getLinkProperties(net)?.let { pushLinkProperties(it) }
            }
        }
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                runCatching { cm.getLinkProperties(network)?.let { pushLinkProperties(it) } }
            }

            override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
                pushLinkProperties(linkProperties)
            }

            override fun onLost(network: Network) {
                val active = runCatching { cm.activeNetwork?.let { cm.getLinkProperties(it) } }.getOrNull()
                if (active != null) {
                    pushLinkProperties(active)
                } else {
                    pushInterfacesJson("""{"interfaces":[],"dns":[]}""")
                }
            }
        }
        runCatching {
            cm.registerDefaultNetworkCallback(cb)
            networkCallback = cb
        }
    }

    private fun pushLinkProperties(lp: LinkProperties) {
        val ifaces = JSONArray()
        val name = lp.interfaceName
        if (!name.isNullOrBlank()) {
            val addrs = JSONArray()
            for (la in lp.linkAddresses) {
                val hostAddr = la.address?.hostAddress?.substringBefore('%') ?: continue
                addrs.put("$hostAddr/${la.prefixLength}")
            }
            val mtu = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && lp.mtu > 0) lp.mtu else 1500
            ifaces.put(
                JSONObject().apply {
                    put("name", name)
                    put("index", 1)
                    put("mtu", mtu)
                    put("up", true)
                    put("addrs", addrs)
                }
            )
        }
        val dns = JSONArray()
        for (addr in lp.dnsServers) {
            val ip = addr.hostAddress?.substringBefore('%') ?: continue
            dns.put(ip)
        }
        val payload = JSONObject().apply {
            put("interfaces", ifaces)
            put("dns", dns)
        }.toString()
        pushInterfacesJson(payload)
    }

    private fun pushInterfacesJson(json: String) {
        scope.launch {
            runCatching { FalconNative.tailnetSetInterfaces(json) }
        }
    }

    fun setEnabled(enabled: Boolean) {
        val cur = _state.value
        if (cur.busy) return
        if (enabled && !cur.libraryAvailable) return
        savePrefs(enabled = enabled, hostname = cur.hostname, controlUrl = cur.controlUrl, authKey = null)
        _state.value = cur.copy(enabled = enabled)
        if (enabled) {
            startNode()
        } else {
            stopNode()
        }
    }

    fun updateSettings(hostname: String, controlUrl: String, authKey: String?) {
        val cur = _state.value
        val trimmedHost = hostname.trim()
        val trimmedControl = controlUrl.trim()
        val cleanedKey = authKey?.trim()?.takeIf { it.isNotEmpty() }
        savePrefs(enabled = cur.enabled, hostname = trimmedHost, controlUrl = trimmedControl, authKey = cleanedKey)
        _state.value = cur.copy(hostname = trimmedHost, controlUrl = trimmedControl)
        if (cur.enabled && cur.libraryAvailable) {
            startNode()
        }
    }

    fun retry() {
        val cur = _state.value
        if (cur.busy || !cur.libraryAvailable) return
        if (!cur.enabled) {
            savePrefs(enabled = true, hostname = cur.hostname, controlUrl = cur.controlUrl, authKey = null)
            _state.value = cur.copy(enabled = true)
        }
        startNode()
    }

    fun logout() {
        val cur = _state.value
        if (cur.busy) return
        pollJob?.cancel()
        pollJob = null
        clearAuthKey()
        savePrefs(enabled = false, hostname = cur.hostname, controlUrl = cur.controlUrl, authKey = null)
        _state.value = cur.copy(enabled = false, busy = true, lastError = null)
        scope.launch {
            val raw = runCatching { FalconNative.tailnetLogout(stateDir().absolutePath) }.getOrDefault("")
            val err = parseOpError(raw)
            _state.value = _state.value.copy(
                busy = false,
                status = TailnetStatus.STOPPED,
                proxy = null,
                lastError = err
            )
        }
    }

    private fun startNode() {
        pollJob?.cancel()
        pollJob = null
        val cur = _state.value
        _state.value = cur.copy(
            busy = true,
            lastError = null,
            status = TailnetStatus("starting", null, null, emptyList(), null),
            proxy = null
        )
        val prefs = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val authKey = prefs.getString(KEY_AUTH_KEY, null)?.takeIf { it.isNotBlank() }
        val cfg = JSONObject().apply {
            put("stateDir", stateDir().absolutePath)
            put("hostname", cur.effectiveHostname)
            if (cur.controlUrl.isNotBlank()) put("controlUrl", cur.controlUrl)
            if (authKey != null) put("authKey", authKey)
        }.toString()

        scope.launch {
            val raw = runCatching { FalconNative.tailnetStart(cfg) }
                .getOrElse { """{"ok":false,"error":"${it.message ?: "启动失败"}"}""" }
            val obj = runCatching { JSONObject(raw) }.getOrNull()
            val ok = obj?.optBoolean("ok", false) ?: false
            val missing = obj?.optBoolean("missing", false) ?: false
            val err = obj?.optString("error")?.takeIf { it.isNotBlank() }
            if (!ok) {
                _state.value = _state.value.copy(
                    busy = false,
                    libraryAvailable = !missing && _state.value.libraryAvailable,
                    enabled = if (missing) false else _state.value.enabled,
                    status = TailnetStatus("error", null, null, emptyList(), err ?: "启动失败"),
                    proxy = null,
                    lastError = err ?: "启动失败"
                )
                return@launch
            }
            val status = TailnetStatus.parse(FalconNative.tailnetStatus())
            val proxy = if (status.state == "running") TailnetProxy.parse(FalconNative.tailnetProxy()) else null
            if (status.state == "running") clearAuthKey()
            _state.value = _state.value.copy(busy = false, status = status, proxy = proxy, lastError = null)
            startPolling()
        }
    }

    private fun stopNode() {
        pollJob?.cancel()
        pollJob = null
        _state.value = _state.value.copy(busy = true, lastError = null)
        scope.launch {
            runCatching { FalconNative.tailnetStop() }
            _state.value = _state.value.copy(
                busy = false,
                status = TailnetStatus.STOPPED,
                proxy = null,
                lastError = null
            )
        }
    }

    private fun startPolling() {
        pollJob?.cancel()
        pollJob = scope.launch {
            while (_state.value.enabled) {
                val delayMs = when (_state.value.status.state) {
                    "running" -> 5000L
                    else -> 1000L
                }
                delay(delayMs)
                if (!_state.value.enabled) break
                val st = TailnetStatus.parse(runCatching { FalconNative.tailnetStatus() }.getOrDefault(""))
                val px = if (st.state == "running") {
                    TailnetProxy.parse(runCatching { FalconNative.tailnetProxy() }.getOrDefault(""))
                } else {
                    null
                }
                if (st.state == "running") clearAuthKey()
                _state.value = _state.value.copy(status = st, proxy = px)
            }
        }
    }

    private fun savePrefs(enabled: Boolean, hostname: String, controlUrl: String, authKey: String?) {
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit().apply {
            putBoolean(KEY_ENABLED, enabled)
            putString(KEY_HOSTNAME, hostname)
            putString(KEY_CONTROL_URL, controlUrl)
            if (authKey != null) putString(KEY_AUTH_KEY, authKey)
        }.apply()
    }

    private fun clearAuthKey() {
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit().remove(KEY_AUTH_KEY).apply()
    }

    private fun parseOpError(raw: String): String? {
        if (raw.isBlank()) return null
        val obj = runCatching { JSONObject(raw) }.getOrNull() ?: return null
        if (obj.optBoolean("ok", true)) return null
        return obj.optString("error").takeIf { it.isNotBlank() }
    }

    fun resolveRoute(url: String, route: ServerRoute): ResolvedRoute {
        val snap = _state.value
        val host = extractHost(url)
        return when (route) {
            ServerRoute.DIRECT -> ResolvedRoute.Direct
            ServerRoute.TAILNET -> {
                if (snap.status.state == "running" && snap.proxy != null) {
                    ResolvedRoute.Tailnet(snap.proxy)
                } else {
                    ResolvedRoute.Unavailable(unavailableMessage(snap.status))
                }
            }
            ServerRoute.AUTO -> {
                if (snap.status.state == "running" && snap.proxy != null && host != null && snap.status.hasPeer(host)) {
                    ResolvedRoute.Tailnet(snap.proxy)
                } else {
                    ResolvedRoute.Direct
                }
            }
        }
    }

    fun isRoutedViaTailnet(url: String, route: ServerRoute): Boolean =
        resolveRoute(url, route) is ResolvedRoute.Tailnet

    fun failureHint(url: String, route: ServerRoute): String? {
        if (route == ServerRoute.DIRECT) return null
        val host = extractHost(url) ?: return null
        if (route == ServerRoute.AUTO && !looksLikeTailnetHost(host)) return null
        val st = _state.value.status
        return when (st.state) {
            "needs_login", "needs_approval", "starting", "error" -> unavailableMessage(st)
            "stopped" -> if (route == ServerRoute.TAILNET) unavailableMessage(st) else null
            else -> null
        }
    }

    private fun unavailableMessage(st: TailnetStatus): String = when (st.state) {
        "stopped" -> "内置 Tailscale 未开启"
        "starting" -> "内置 Tailscale 正在连接…"
        "needs_login" -> "内置 Tailscale 尚未登录"
        "needs_approval" -> "内置 Tailscale 等待管理员审批"
        "error" -> if (!st.error.isNullOrBlank()) "内置 Tailscale 出错：${st.error}" else "内置 Tailscale 出错"
        else -> "内置 Tailscale 代理尚未就绪"
    }

    private fun extractHost(url: String): String? = runCatching {
        URI(FalconApiClient.normalizeBaseUrl(url)).host
    }.getOrNull()

    private fun looksLikeTailnetHost(host: String): Boolean {
        val h = host.trim().trimEnd('.').lowercase()
        if (h.endsWith(".ts.net")) return true
        val parts = h.split('.')
        if (parts.size == 4) {
            val a = parts[0].toIntOrNull()
            val b = parts[1].toIntOrNull()
            if (a == 100 && b != null && b in 64..127) return true
        }
        return false
    }

    suspend fun probeTailnetServers(port: Int = 4923): List<TailnetProbeHit> = withContext(Dispatchers.IO) {
        val snap = _state.value
        val proxy = snap.proxy ?: return@withContext emptyList()
        if (snap.status.state != "running") return@withContext emptyList()
        val probeClient = OkHttpClient.Builder()
            .dns(TailnetDns)
            .socketFactory(TailnetSocketFactory(proxy))
            .connectTimeout(1500, TimeUnit.MILLISECONDS)
            .readTimeout(1500, TimeUnit.MILLISECONDS)
            .build()
        val sem = Semaphore(8)
        coroutineScope {
            snap.status.peers.filter { it.online }.mapNotNull { peer ->
                val targetHost = peer.connectionHost
                if (targetHost.isEmpty()) return@mapNotNull null
                async {
                    sem.withPermit {
                        runCatching {
                            val req = Request.Builder().url("http://$targetHost:$port/api/system").get().build()
                            probeClient.newCall(req).execute().use { resp ->
                                if (!resp.isSuccessful) return@use null
                                val body = resp.body?.string().orEmpty()
                                val obj = JSONObject(body)
                                val ver = obj.optString("version")
                                val plat = obj.optString("platform")
                                if (ver.isBlank() && plat.isBlank()) return@use null
                                TailnetProbeHit(peer.id, targetHost, port, ver, plat)
                            }
                        }.getOrNull()
                    }
                }
            }.awaitAll().filterNotNull()
        }
    }
}
