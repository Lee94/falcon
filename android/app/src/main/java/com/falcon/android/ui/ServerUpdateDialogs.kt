package com.falcon.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.falcon.android.model.NodeStatus
import com.falcon.android.model.NodeUpdateTarget
import com.falcon.android.model.ServerProfile
import com.falcon.android.model.ServerUpdateCheck
import com.falcon.android.model.ServerUpdateTask
import com.falcon.android.net.FalconApiClient
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

data class UpdateTarget(val baseUrl: String, val hostId: String?, val label: String)

private data class UpdateState(
    val checking: Boolean = false,
    val running: Boolean = false,
    val reconnecting: Boolean = false,
    val missingTask: Boolean = false,
    val check: ServerUpdateCheck? = null,
    val task: ServerUpdateTask? = null,
    val error: String? = null
)

/** 按节点单飞。界面离开或 Activity 重建后仍查询，重新打开只观察现有任务，绝不重发 POST。 */
private object ServerUpdates {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val states = HashMap<Pair<String, String?>, MutableStateFlow<UpdateState>>()
    fun state(target: UpdateTarget): MutableStateFlow<UpdateState> = states.getOrPut(
        FalconApiClient.normalizeBaseUrl(target.baseUrl) to target.hostId
    ) { MutableStateFlow(UpdateState()) }

    fun check(target: UpdateTarget) {
        val state = state(target)
        if (state.value.running || state.value.checking) return
        state.value = state.value.copy(checking = true, missingTask = false, error = null)
        scope.launch {
            val status = FalconApiClient.serverUpdateStatus(target.baseUrl, target.hostId)
            val check = FalconApiClient.checkServerUpdate(target.baseUrl, target.hostId)
            status.onSuccess { task ->
                state.value = state.value.copy(checking = false, task = task, running = task.running,
                    check = check.getOrNull(), error = check.exceptionOrNull()?.message ?: check.getOrNull()?.error)
                if (task.running) poll(target, state)
            }.onFailure { error ->
                state.value = state.value.copy(checking = false, check = null, error = error.message)
            }
        }
    }

    fun install(target: UpdateTarget, version: String) {
        val state = state(target)
        if (state.value.running || state.value.checking) return
        val check = state.value.check ?: return
        if (!check.available || check.error != null || check.latestVersion != version) return
        state.value = state.value.copy(running = true, error = null, reconnecting = false,
            task = ServerUpdateTask("downloading", version, null))
        scope.launch {
            FalconApiClient.startServerUpdate(target.baseUrl, target.hostId, version)
                .onSuccess { task -> state.value = state.value.copy(task = task) }
                .onFailure { error ->
                    if (error is FalconApiClient.UpdateRejectedException) {
                        state.value = state.value.copy(running = false, task = null, check = null, error = error.message)
                    } else {
                        state.value = state.value.copy(reconnecting = true, error = error.message)
                    }
                }
            if (state.value.running) poll(target, state)
        }
    }

    private suspend fun poll(target: UpdateTarget, state: MutableStateFlow<UpdateState>) {
        while (state.value.running) {
            delay(2000)
            FalconApiClient.serverUpdateStatus(target.baseUrl, target.hostId)
                .onSuccess { task ->
                    if (task.phase == "idle") {
                        state.value = state.value.copy(running = false, reconnecting = false, missingTask = true,
                            error = null, task = null, check = null)
                    } else {
                        state.value = state.value.copy(reconnecting = false, error = null, task = task, running = !task.terminal)
                    }
                    if (task.terminal) {
                        // 最终检查失败时不能继续展示升级前缓存的可安装版本。
                        state.value = state.value.copy(check = null)
                        FalconApiClient.checkServerUpdate(target.baseUrl, target.hostId).onSuccess {
                            state.value = state.value.copy(check = it, error = it.error)
                        }.onFailure { state.value = state.value.copy(error = it.message) }
                    }
                }
                .onFailure { error -> state.value = state.value.copy(reconnecting = true, error = error.message) }
        }
    }
}

@Composable
internal fun ServerAboutDialog(server: ServerProfile, onDismiss: () -> Unit, onUpdate: (UpdateTarget) -> Unit) {
    val theme = LocalFalconTheme.current
    AlertDialog(
        onDismissRequest = onDismiss, containerColor = theme.card,
        title = { Text(t("settings.aboutTitle"), color = theme.foreground) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(server.name, color = theme.foreground)
                Text(server.url, color = theme.mutedForeground)
                Text(t("serverUpdate.intro"), color = theme.mutedForeground)
            }
        },
        confirmButton = {
            TextButton(onClick = { onUpdate(UpdateTarget(server.url, null, "${server.name} · ${server.url}")) }) {
                Text(t("serverUpdate.open"), color = theme.tintStrong)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(t("serverUpdate.close"), color = theme.mutedForeground) } }
    )
}

@Composable
internal fun NodeSettingsDialog(server: ServerProfile, onDismiss: () -> Unit, onUpdate: (UpdateTarget) -> Unit) {
    val theme = LocalFalconTheme.current
    var hosts by remember(server.url) { mutableStateOf<List<NodeUpdateTarget>>(emptyList()) }
    var nodes by remember(server.url) { mutableStateOf<List<NodeStatus>>(emptyList()) }
    var error by remember(server.url) { mutableStateOf<String?>(null) }
    var loading by remember(server.url) { mutableStateOf(true) }
    var refresh by remember(server.url) { mutableStateOf(0) }
    LaunchedEffect(server.url, refresh) {
        loading = true
        error = null
        FalconApiClient.fetchUpdateHosts(server.url).onSuccess { hosts = it }.onFailure { error = it.message }
        FalconApiClient.fetchNodeStatus(server.url).onSuccess { nodes = it }.onFailure { error = it.message }
        loading = false
    }
    AlertDialog(
        onDismissRequest = onDismiss, containerColor = theme.card,
        title = { Text(t("settings.tab_hosts"), color = theme.foreground) },
        text = {
            Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(server.url, color = theme.mutedForeground)
                error?.let { Text(it, color = theme.destructive) }
                if (loading) Text(t("serverUpdate.checking"), color = theme.mutedForeground)
                if (!loading && hosts.isEmpty()) Text(t("host.empty"), color = theme.mutedForeground)
                for (host in hosts) {
                    val node = nodes.firstOrNull { it.hostId == host.hostId }
                    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(host.name, color = theme.foreground, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                            TextButton(onClick = { onUpdate(UpdateTarget(server.url, host.hostId, "${host.name} · ${host.address}")) }) {
                                Text(t("serverUpdate.node"), color = theme.tintStrong, maxLines = 1)
                            }
                        }
                        Text(host.address, color = theme.mutedForeground)
                        if (node != null) {
                            Text(t(if (node.online) "serverUpdate.online" else "serverUpdate.unreachable", "version" to (node.version ?: "—")),
                                color = if (node.online) theme.success else theme.mutedForeground)
                            node.error?.let { Text(it, color = theme.destructive) }
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(enabled = !loading, onClick = { refresh++ }) { Text(t("serverUpdate.check"), color = theme.tintStrong) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(t("serverUpdate.close"), color = theme.mutedForeground) } }
    )
}

@Composable
internal fun ServerUpdateDialog(target: UpdateTarget, onDismiss: () -> Unit) {
    val theme = LocalFalconTheme.current
    val state by remember(target) { ServerUpdates.state(target) }.collectAsState()
    var confirm by remember(target) { mutableStateOf(false) }
    LaunchedEffect(target) { ServerUpdates.check(target) }
    val version = state.check?.takeIf { it.available && it.error == null }?.latestVersion
    AlertDialog(
        onDismissRequest = onDismiss, containerColor = theme.card,
        title = { Text(t("serverUpdate.title"), color = theme.foreground) },
        text = {
            Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(target.label, color = theme.foreground)
                if (target.hostId != null) Text(target.baseUrl, color = theme.mutedForeground)
                Text(t("serverUpdate.intro"), color = theme.mutedForeground)
                state.check?.let { check ->
                    Text(t("serverUpdate.versions", "current" to check.currentVersion, "latest" to (check.latestVersion ?: "—"), "platform" to check.platform), color = theme.foreground)
                }
                state.task?.let { task ->
                    val phase = task.phase.takeIf { it in setOf("idle", "downloading", "verifying", "installing", "restarting", "succeeded", "failed") } ?: "unknown"
                    Text(t("serverUpdate.phase_$phase", "version" to (task.targetVersion ?: "—")), color = if (phase == "failed") theme.destructive else theme.mutedForeground)
                    task.error?.let { Text(it, color = theme.destructive) }
                }
                if (state.reconnecting) Text(t("serverUpdate.reconnecting"), color = theme.mutedForeground)
                if (state.missingTask) Text(t("serverUpdate.missingTask"), color = theme.destructive)
                state.error?.let { Text(it, color = theme.destructive) }
                if (version != null && !state.running) {
                    TextButton(enabled = !state.checking, onClick = { confirm = true }) {
                        Text(t("serverUpdate.install", "version" to version), color = theme.tintStrong)
                    }
                }
                TextButton(enabled = !state.running && !state.checking, onClick = { ServerUpdates.check(target) }) {
                    Text(t(if (state.checking) "serverUpdate.checking" else "serverUpdate.check"), color = theme.tintStrong)
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text(t("serverUpdate.close"), color = theme.mutedForeground) } }
    )
    if (confirm && version != null) {
        AlertDialog(
            onDismissRequest = { confirm = false }, containerColor = theme.card,
            title = { Text(t("serverUpdate.title"), color = theme.foreground) },
            text = { Text(t("serverUpdate.confirm", "target" to target.label, "version" to version), color = theme.foreground) },
            confirmButton = {
                TextButton(enabled = !state.running && !state.checking, onClick = {
                    confirm = false
                    ServerUpdates.install(target, version)
                }) { Text(t("serverUpdate.install", "version" to version), color = theme.tintStrong) }
            },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text(t("common.cancel"), color = theme.mutedForeground) } }
        )
    }
}
