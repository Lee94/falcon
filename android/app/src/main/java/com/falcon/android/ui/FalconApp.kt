package com.falcon.android.ui

import android.content.Context
import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import com.falcon.android.model.DashboardData
import com.falcon.android.model.ServerProfile
import com.falcon.android.model.SessionUi
import com.falcon.android.net.FalconApiClient
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

private const val PREFS_NAME = "falcon_android_prefs"
private const val KEY_SERVERS = "servers_json"
private const val KEY_ACTIVE_SERVER_ID = "active_server_id"
private const val KEY_THEME_NAME = "theme_name"

private fun loadSavedServers(context: Context): Pair<List<ServerProfile>, String> {
    val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
    val raw = prefs.getString(KEY_SERVERS, null)
    val list = ArrayList<ServerProfile>()
    if (!raw.isNullOrBlank()) {
        try {
            val arr = JSONArray(raw)
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                list.add(
                    ServerProfile(
                        id = o.optString("id"),
                        name = o.optString("name"),
                        url = o.optString("url")
                    )
                )
            }
        } catch (_: Exception) {
        }
    }
    if (list.isEmpty()) {
        list.add(
            ServerProfile(
                id = "default-mac-lan",
                name = "Mac 局域网 (10.233.21.9)",
                url = "http://10.233.21.9:4923"
            )
        )
        list.add(
            ServerProfile(
                id = "default-usb",
                name = "本机 USB 转发 (127.0.0.1)",
                url = "http://127.0.0.1:4923"
            )
        )
        list.add(
            ServerProfile(
                id = "default-emu",
                name = "安卓模拟器宿主机 (10.0.2.2)",
                url = "http://10.0.2.2:4923"
            )
        )
    } else if (list.none { it.url.contains("10.233.21.9") }) {
        list.add(
            0,
            ServerProfile(
                id = "default-mac-lan",
                name = "Mac 局域网 (10.233.21.9)",
                url = "http://10.233.21.9:4923"
            )
        )
    }
    val savedActive = prefs.getString(KEY_ACTIVE_SERVER_ID, null)
    val activeId = if (savedActive == null || savedActive == "default-emu") {
        list.first().id
    } else {
        savedActive
    }
    return list to activeId
}

private fun saveServersToPrefs(
    context: Context,
    servers: List<ServerProfile>,
    activeId: String
) {
    val arr = JSONArray()
    for (s in servers) {
        arr.put(
            JSONObject().apply {
                put("id", s.id)
                put("name", s.name)
                put("url", s.url)
            }
        )
    }
    context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        .edit()
        .putString(KEY_SERVERS, arr.toString())
        .putString(KEY_ACTIVE_SERVER_ID, activeId)
        .apply()
}

@Composable
fun FalconApp() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    val initialServers = remember { loadSavedServers(context) }
    var serverProfiles by remember { mutableStateOf(initialServers.first) }
    var activeServerId by remember { mutableStateOf(initialServers.second) }

    val activeServer = remember(serverProfiles, activeServerId) {
        serverProfiles.find { it.id == activeServerId } ?: serverProfiles.first()
    }

    var themeName by remember {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        mutableStateOf(prefs.getString(KEY_THEME_NAME, "Falcon Dark") ?: "Falcon Dark")
    }

    var dashboard by remember { mutableStateOf(DashboardData(emptyList(), emptyList())) }
    var isLoading by remember { mutableStateOf(false) }
    var connectionError by remember { mutableStateOf<String?>(null) }
    var refreshTrigger by remember { mutableIntStateOf(0) }

    var openedSession by remember { mutableStateOf<SessionUi?>(null) }

    val showToast: (String) -> Unit = remember {
        { msg ->
            Toast.makeText(context, msg, Toast.LENGTH_SHORT).show()
        }
    }

    // 刷新服务端项目与会话列表
    LaunchedEffect(activeServer.url, refreshTrigger) {
        isLoading = true
        val res = FalconApiClient.fetchDashboard(activeServer.url)
        isLoading = false
        res.onSuccess { data ->
            dashboard = data
            connectionError = null
        }.onFailure { err ->
            connectionError = err.message ?: "连接失败"
        }
    }

    // 在列表页时每 5 秒自动轮询一次会话与程序状态（与桌面端 5s 轮询一致）
    LaunchedEffect(activeServer.url, openedSession) {
        if (openedSession != null) return@LaunchedEffect
        while (true) {
            delay(5000L)
            FalconApiClient.fetchDashboard(activeServer.url).onSuccess { data ->
                dashboard = data
                connectionError = null
            }
        }
    }

    FalconTheme(themeName = themeName) {
        val currentSession = openedSession
        if (currentSession != null) {
            BackHandler {
                openedSession = null
                refreshTrigger++
            }
            TerminalScreen(
                baseUrl = activeServer.url,
                initialSession = currentSession,
                themeName = themeName,
                onBackDetach = {
                    openedSession = null
                    refreshTrigger++
                },
                onSessionTerminated = {
                    openedSession = null
                    refreshTrigger++
                    showToast("已终止会话")
                },
                onToast = showToast
            )
        } else {
            DashboardScreen(
                serverProfiles = serverProfiles,
                activeServer = activeServer,
                dashboard = dashboard,
                isLoading = isLoading,
                connectionError = connectionError,
                themeName = themeName,
                onSelectServer = { profile ->
                    activeServerId = profile.id
                    saveServersToPrefs(context, serverProfiles, activeServerId)
                    refreshTrigger++
                },
                onAddOrUpdateServer = { name, url, editId ->
                    if (editId != null) {
                        serverProfiles = serverProfiles.map {
                            if (it.id == editId) it.copy(name = name, url = url) else it
                        }
                        activeServerId = editId
                    } else {
                        val newProfile = ServerProfile(
                            id = UUID.randomUUID().toString(),
                            name = name,
                            url = url
                        )
                        serverProfiles = serverProfiles + newProfile
                        activeServerId = newProfile.id
                    }
                    saveServersToPrefs(context, serverProfiles, activeServerId)
                    refreshTrigger++
                },
                onDeleteServer = { deleteId ->
                    val remaining = serverProfiles.filterNot { it.id == deleteId }
                    if (remaining.isNotEmpty()) {
                        serverProfiles = remaining
                        if (activeServerId == deleteId) {
                            activeServerId = remaining.first().id
                        }
                        saveServersToPrefs(context, serverProfiles, activeServerId)
                        refreshTrigger++
                    }
                },
                onSelectTheme = { newTheme ->
                    themeName = newTheme
                    context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                        .edit()
                        .putString(KEY_THEME_NAME, newTheme)
                        .apply()
                },
                onRefresh = {
                    scope.launch { refreshTrigger++ }
                },
                onOpenSession = { session ->
                    openedSession = session
                },
                onToast = showToast
            )
        }
    }
}
