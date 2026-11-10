package com.falcon.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Palette
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.falcon.android.bridge.FalconNative
import com.falcon.android.model.AgentCatalog
import com.falcon.android.model.DashboardData
import com.falcon.android.model.JsonParsers
import com.falcon.android.model.ProjectUi
import com.falcon.android.model.ServerGroupUi
import com.falcon.android.model.ServerProfile
import com.falcon.android.model.SessionUi
import com.falcon.android.net.FalconApiClient
import kotlinx.coroutines.launch

@Composable
fun DashboardScreen(
    serverProfiles: List<ServerProfile>,
    activeServer: ServerProfile,
    dashboard: DashboardData,
    isLoading: Boolean,
    connectionError: String?,
    themeName: String,
    onSelectServer: (ServerProfile) -> Unit,
    onAddOrUpdateServer: (name: String, url: String, editId: String?) -> Unit,
    onDeleteServer: (String) -> Unit,
    onSelectTheme: (String) -> Unit,
    onRefresh: () -> Unit,
    onOpenSession: (SessionUi) -> Unit,
    onToast: (String) -> Unit
) {
    val theme = LocalFalconTheme.current
    val scope = rememberCoroutineScope()

    var currentTab by remember { mutableIntStateOf(0) } // 0 = 按项目分组, 1 = 全部会话
    var sessionFilter by remember { mutableStateOf("all") } // all / active / unverified / dead

    var showServerDialog by remember { mutableStateOf(false) }
    var editingServer by remember { mutableStateOf<ServerProfile?>(null) }
    var serverNameInput by remember { mutableStateOf("") }
    var serverUrlInput by remember { mutableStateOf("") }

    var showThemeDialog by remember { mutableStateOf(false) }
    var themeSearchQuery by remember { mutableStateOf("") }

    // 新建会话弹窗状态
    var createTargetProjectId by remember { mutableStateOf<String?>(null) }
    var createTargetTitle by remember { mutableStateOf("") }
    var detectedAgents by remember { mutableStateOf<List<String>>(emptyList()) }
    var loadingAgents by remember { mutableStateOf(false) }
    var selectedAgent by remember { mutableStateOf<String?>(null) }
    var customSessionName by remember { mutableStateOf("") }
    var creatingSession by remember { mutableStateOf(false) }

    // 会话操作确认框
    var renameTarget by remember { mutableStateOf<SessionUi?>(null) }
    var renameInput by remember { mutableStateOf("") }
    var terminateTarget by remember { mutableStateOf<SessionUi?>(null) }
    var terminateBusyCmd by remember { mutableStateOf<String?>(null) }

    val openCreateSessionDialog: (projectId: String, title: String) -> Unit = { pid, title ->
        createTargetProjectId = pid
        createTargetTitle = title
        selectedAgent = null
        customSessionName = ""
        detectedAgents = emptyList()
        loadingAgents = true
        scope.launch {
            FalconApiClient.fetchDetectedAgents(activeServer.url, pid)
                .onSuccess { list ->
                    detectedAgents = list
                }
            loadingAgents = false
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(theme.app)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp)
    ) {
        // 顶部浮动岛：服务端连接栏与主题切换
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .background(theme.background)
                .padding(horizontal = 12.dp, vertical = 10.dp)
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically
            ) {
                // 连接状态指示灯
                val statusColor = when {
                    isLoading -> theme.warning
                    connectionError != null -> theme.destructive
                    else -> theme.success
                }
                Box(
                    modifier = Modifier
                        .size(9.dp)
                        .clip(CircleShape)
                        .background(statusColor)
                )
                Spacer(modifier = Modifier.width(8.dp))

                Column(
                    modifier = Modifier
                        .weight(1f)
                        .clickable { showServerDialog = true }
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            text = activeServer.name,
                            color = theme.foreground,
                            fontSize = 16.sp,
                            fontWeight = FontWeight.Bold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = "切换服务器 ▾",
                            color = theme.tintStrong,
                            fontSize = 11.sp,
                            fontWeight = FontWeight.Medium
                        )
                    }
                    Text(
                        text = activeServer.url,
                        color = theme.mutedForeground,
                        fontSize = 12.sp,
                        fontFamily = FontFamily.Monospace,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                }

                IconButton(onClick = { showThemeDialog = true }) {
                    Icon(
                        imageVector = Icons.Default.Palette,
                        contentDescription = "切换主题",
                        tint = theme.foreground
                    )
                }

                IconButton(onClick = onRefresh) {
                    if (isLoading) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(18.dp),
                            strokeWidth = 2.dp,
                            color = theme.tintStrong
                        )
                    } else {
                        Icon(
                            imageVector = Icons.Default.Refresh,
                            contentDescription = "刷新",
                            tint = theme.foreground
                        )
                    }
                }
            }

            if (connectionError != null) {
                Spacer(modifier = Modifier.height(6.dp))
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(8.dp))
                        .background(theme.destructive.copy(alpha = 0.14f))
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = "无法连接服务端：$connectionError",
                        color = theme.destructive,
                        fontSize = 12.sp,
                        modifier = Modifier.weight(1f)
                    )
                    Text(
                        text = "修改地址",
                        color = theme.tintStrong,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.clickable { showServerDialog = true }
                    )
                }
            }

            Spacer(modifier = Modifier.height(8.dp))

            // 视图切换栏：按项目分组 / 全部会话（含统计）
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                TabPill(
                    title = "按项目分组",
                    selected = currentTab == 0,
                    theme = theme,
                    onClick = { currentTab = 0 }
                )
                TabPill(
                    title = "全部会话 (${dashboard.allSessions.size})",
                    selected = currentTab == 1,
                    theme = theme,
                    onClick = { currentTab = 1 }
                )
                Spacer(modifier = Modifier.weight(1f))
                // 快捷在本机开一个无项目独立终端
                Row(
                    modifier = Modifier
                        .clip(RoundedCornerShape(8.dp))
                        .background(theme.tint)
                        .clickable {
                            openCreateSessionDialog("standalone:local", "本机 · 无项目")
                        }
                        .padding(horizontal = 10.dp, vertical = 5.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Icon(
                        imageVector = Icons.Default.Add,
                        contentDescription = null,
                        tint = theme.tintForeground,
                        modifier = Modifier.size(14.dp)
                    )
                    Spacer(modifier = Modifier.width(3.dp))
                    Text(
                        text = "快速终端",
                        color = theme.tintForeground,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium
                    )
                }
            }
        }

        // 主体内容浮动岛
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .background(theme.background)
        ) {
            if (currentTab == 0) {
                ProjectTreeList(
                    servers = dashboard.servers,
                    theme = theme,
                    onNewSession = openCreateSessionDialog,
                    onClickSession = { s ->
                        handleSessionClick(
                            baseUrl = activeServer.url,
                            session = s,
                            theme = theme,
                            scope = scope,
                            onOpenSession = onOpenSession,
                            onRefresh = onRefresh,
                            onToast = onToast
                        )
                    },
                    onRenameSession = { s ->
                        renameTarget = s
                        renameInput = s.name
                    },
                    onTerminateSession = { s ->
                        scope.launch {
                            val (busy, cmd) = FalconApiClient.checkForegroundBusy(activeServer.url, s.id)
                            terminateBusyCmd = if (busy) cmd else null
                            terminateTarget = s
                        }
                    },
                    onDeleteDeadSession = { s ->
                        scope.launch {
                            FalconApiClient.deleteDeadSession(activeServer.url, s.id)
                                .onSuccess { onRefresh() }
                                .onFailure { e -> onToast("清除失败：${e.message}") }
                        }
                    }
                )
            } else {
                AllSessionsOverview(
                    allSessions = dashboard.allSessions,
                    filter = sessionFilter,
                    onFilterChange = { sessionFilter = it },
                    theme = theme,
                    onClickSession = { s ->
                        handleSessionClick(
                            baseUrl = activeServer.url,
                            session = s,
                            theme = theme,
                            scope = scope,
                            onOpenSession = onOpenSession,
                            onRefresh = onRefresh,
                            onToast = onToast
                        )
                    },
                    onRenameSession = { s ->
                        renameTarget = s
                        renameInput = s.name
                    },
                    onTerminateSession = { s ->
                        scope.launch {
                            val (busy, cmd) = FalconApiClient.checkForegroundBusy(activeServer.url, s.id)
                            terminateBusyCmd = if (busy) cmd else null
                            terminateTarget = s
                        }
                    },
                    onDeleteDeadSession = { s ->
                        scope.launch {
                            FalconApiClient.deleteDeadSession(activeServer.url, s.id)
                                .onSuccess { onRefresh() }
                                .onFailure { e -> onToast("清除失败：${e.message}") }
                        }
                    },
                    onClearAllDead = {
                        val deadList = dashboard.allSessions.filter { it.state == "dead" }
                        scope.launch {
                            for (d in deadList) {
                                FalconApiClient.deleteDeadSession(activeServer.url, d.id)
                            }
                            onRefresh()
                            onToast("已清除 ${deadList.size} 条已丢失记录")
                        }
                    }
                )
            }
        }
    }

    // 新建会话弹窗（支持普通终端与目标宿主机已安装的 Agent CLI）
    val targetPid = createTargetProjectId
    if (targetPid != null) {
        AlertDialog(
            onDismissRequest = { if (!creatingSession) createTargetProjectId = null },
            containerColor = theme.card,
            title = {
                Column {
                    Text(
                        text = "新建终端会话",
                        color = theme.foreground,
                        fontSize = 17.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                    Text(
                        text = createTargetTitle,
                        color = theme.mutedForeground,
                        fontSize = 12.sp
                    )
                }
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    OutlinedTextField(
                        value = customSessionName,
                        onValueChange = { customSessionName = it },
                        label = { Text("会话名称（可选，留空自动跟随前台程序）") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )

                    Text(
                        text = "启动类型",
                        color = theme.mutedForeground,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium
                    )

                    // 普通 Shell 选项
                    AgentChoiceRow(
                        label = "普通终端 (Shell)",
                        subtitle = "启动宿主机的交互式登录 Shell",
                        selected = selectedAgent == null,
                        theme = theme,
                        onClick = { selectedAgent = null }
                    )

                    if (loadingAgents) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp)
                        ) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(14.dp),
                                strokeWidth = 2.dp,
                                color = theme.tintStrong
                            )
                            Text("正在检测宿主机已安装的 Agent CLI…", color = theme.mutedForeground, fontSize = 12.sp)
                        }
                    } else if (detectedAgents.isNotEmpty()) {
                        Column(
                            modifier = Modifier.heightIn(max = 220.dp),
                            verticalArrangement = Arrangement.spacedBy(6.dp)
                        ) {
                            for (agentId in detectedAgents) {
                                AgentChoiceRow(
                                    label = AgentCatalog.labelFor(agentId),
                                    subtitle = agentId,
                                    selected = selectedAgent == agentId,
                                    theme = theme,
                                    onClick = { selectedAgent = agentId }
                                )
                            }
                        }
                    }
                }
            },
            confirmButton = {
                Button(
                    enabled = !creatingSession,
                    onClick = {
                        creatingSession = true
                        scope.launch {
                            val res = FalconApiClient.createSession(
                                baseUrl = activeServer.url,
                                projectId = targetPid,
                                name = customSessionName,
                                agent = selectedAgent,
                                theme = theme
                            )
                            creatingSession = false
                            res.onSuccess { newSession ->
                                createTargetProjectId = null
                                onRefresh()
                                onOpenSession(newSession)
                            }.onFailure { e ->
                                onToast("创建会话失败：${e.message}")
                            }
                        }
                    }
                ) {
                    Text(if (creatingSession) "正在创建…" else "启动终端")
                }
            },
            dismissButton = {
                TextButton(
                    enabled = !creatingSession,
                    onClick = { createTargetProjectId = null }
                ) {
                    Text("取消", color = theme.mutedForeground)
                }
            }
        )
    }

    // 服务器地址管理弹窗
    if (showServerDialog) {
        var scanningLan by remember { mutableStateOf(false) }
        var discoveredServers by remember { mutableStateOf<List<FalconApiClient.DiscoveredServer>>(emptyList()) }
        var scanHint by remember { mutableStateOf<String?>(null) }

        AlertDialog(
            onDismissRequest = {
                showServerDialog = false
                editingServer = null
            },
            containerColor = theme.card,
            title = {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text("Falcon 服务端连接", color = theme.foreground, fontWeight = FontWeight.SemiBold)
                    TextButton(
                        enabled = !scanningLan,
                        onClick = {
                            scanningLan = true
                            scanHint = "正在扫描当前 Wi-Fi 网段 (:4923)…"
                            scope.launch {
                                val found = FalconApiClient.scanLanServers(4923)
                                discoveredServers = found
                                scanningLan = false
                                scanHint = if (found.isEmpty()) {
                                    "未在同一网段发现开启局域网访问的 Falcon（4923）"
                                } else {
                                    "发现 ${found.size} 台局域网服务端，点击即可添加连接"
                                }
                            }
                        }
                    ) {
                        Text(if (scanningLan) "扫描中…" else "扫描局域网", fontSize = 12.sp)
                    }
                }
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (scanHint != null) {
                        Text(
                            text = scanHint!!,
                            color = theme.mutedForeground,
                            fontSize = 11.sp
                        )
                    }
                    for (disc in discoveredServers) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(8.dp))
                                .background(theme.tint)
                                .clickable {
                                    val label = "局域网 ${disc.platform} (${disc.url.removePrefix("http://")})"
                                    onAddOrUpdateServer(label, disc.url, null)
                                    showServerDialog = false
                                }
                                .padding(horizontal = 10.dp, vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = "发现 Falcon v${disc.version} (${disc.platform})",
                                    color = theme.tintForeground,
                                    fontSize = 13.sp,
                                    fontWeight = FontWeight.SemiBold
                                )
                                Text(
                                    text = disc.url,
                                    color = theme.mutedForeground,
                                    fontSize = 11.sp,
                                    fontFamily = FontFamily.Monospace
                                )
                            }
                            Text("一键连接", color = theme.tintForeground, fontSize = 12.sp)
                        }
                    }

                    Text(
                        text = "已保存的服务端",
                        color = theme.mutedForeground,
                        fontSize = 12.sp
                    )
                    for (profile in serverProfiles) {
                        val isCurrent = profile.id == activeServer.id
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(8.dp))
                                .background(if (isCurrent) theme.tint else theme.background)
                                .clickable {
                                    onSelectServer(profile)
                                    showServerDialog = false
                                }
                                .padding(horizontal = 10.dp, vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = profile.name,
                                    color = if (isCurrent) theme.tintForeground else theme.foreground,
                                    fontSize = 14.sp,
                                    fontWeight = FontWeight.SemiBold
                                )
                                Text(
                                    text = profile.url,
                                    color = theme.mutedForeground,
                                    fontSize = 11.sp,
                                    fontFamily = FontFamily.Monospace
                                )
                            }
                            TextButton(
                                onClick = {
                                    editingServer = profile
                                    serverNameInput = profile.name
                                    serverUrlInput = profile.url
                                }
                            ) {
                                Text("编辑", fontSize = 12.sp)
                            }
                            if (serverProfiles.size > 1) {
                                IconButton(onClick = { onDeleteServer(profile.id) }) {
                                    Icon(
                                        imageVector = Icons.Default.DeleteOutline,
                                        contentDescription = "删除",
                                        tint = theme.destructive,
                                        modifier = Modifier.size(18.dp)
                                    )
                                }
                            }
                        }
                    }

                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = if (editingServer != null) "编辑服务端" else "添加新服务端",
                        color = theme.mutedForeground,
                        fontSize = 12.sp
                    )
                    OutlinedTextField(
                        value = serverNameInput,
                        onValueChange = { serverNameInput = it },
                        label = { Text("名称（如：办公 Mac / 实验机 212）") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )
                    OutlinedTextField(
                        value = serverUrlInput,
                        onValueChange = { serverUrlInput = it },
                        label = { Text("地址（如 http://192.168.1.10:4923）") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        if (serverUrlInput.isNotBlank()) {
                            val name = serverNameInput.ifBlank { serverUrlInput.trim() }
                            onAddOrUpdateServer(
                                name,
                                FalconApiClient.normalizeBaseUrl(serverUrlInput),
                                editingServer?.id
                            )
                            serverNameInput = ""
                            serverUrlInput = ""
                            editingServer = null
                            showServerDialog = false
                        } else {
                            showServerDialog = false
                        }
                    }
                ) {
                    Text(if (serverUrlInput.isNotBlank()) "保存并连接" else "关闭")
                }
            },
            dismissButton = {
                if (editingServer != null) {
                    TextButton(
                        onClick = {
                            editingServer = null
                            serverNameInput = ""
                            serverUrlInput = ""
                        }
                    ) {
                        Text("取消编辑", color = theme.mutedForeground)
                    }
                }
            }
        )
    }

    // 主题选择器（内置 Falcon 两套 + Ghostty 463 套）
    if (showThemeDialog) {
        val themes = remember(themeSearchQuery) {
            JsonParsers.parseThemeList(FalconNative.listAvailableThemes(themeSearchQuery))
        }
        AlertDialog(
            onDismissRequest = { showThemeDialog = false },
            containerColor = theme.card,
            title = {
                Text("选择主题（Falcon & Ghostty）", color = theme.foreground, fontWeight = FontWeight.SemiBold)
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(
                        value = themeSearchQuery,
                        onValueChange = { themeSearchQuery = it },
                        placeholder = { Text("搜索 465 套主题（如 Falcon / Catppuccin / Tokyo）") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )
                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(300.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp)
                    ) {
                        items(themes, key = { it.name }) { item ->
                            val isSelected = item.name.equals(themeName, ignoreCase = true)
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clip(RoundedCornerShape(8.dp))
                                    .background(if (isSelected) theme.tint else theme.background)
                                    .clickable {
                                        onSelectTheme(item.name)
                                        showThemeDialog = false
                                    }
                                    .padding(horizontal = 10.dp, vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically
                            ) {
                                // 主题色块预览
                                Box(
                                    modifier = Modifier
                                        .size(22.dp)
                                        .clip(CircleShape)
                                        .background(parseHexColor(item.backgroundHex))
                                        .border(1.dp, parseHexColor(item.accentHex), CircleShape),
                                    contentAlignment = Alignment.Center
                                ) {
                                    Box(
                                        modifier = Modifier
                                            .size(8.dp)
                                            .clip(CircleShape)
                                            .background(parseHexColor(item.foregroundHex))
                                    )
                                }
                                Spacer(modifier = Modifier.width(10.dp))
                                Text(
                                    text = item.name,
                                    color = if (isSelected) theme.tintForeground else theme.foreground,
                                    fontSize = 14.sp,
                                    fontWeight = if (isSelected) FontWeight.Bold else FontWeight.Normal,
                                    modifier = Modifier.weight(1f)
                                )
                                Text(
                                    text = if (item.isDark) "深色" else "浅色",
                                    color = theme.mutedForeground,
                                    fontSize = 11.sp
                                )
                            }
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { showThemeDialog = false }) {
                    Text("关闭")
                }
            }
        )
    }

    // 重命名会话对话框
    val rTarget = renameTarget
    if (rTarget != null) {
        AlertDialog(
            onDismissRequest = { renameTarget = null },
            containerColor = theme.card,
            title = { Text("重命名会话", color = theme.foreground) },
            text = {
                OutlinedTextField(
                    value = renameInput,
                    onValueChange = { renameInput = it },
                    placeholder = { Text("留空则恢复自动标题") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth()
                )
            },
            confirmButton = {
                Button(
                    onClick = {
                        val name = renameInput.trim()
                        renameTarget = null
                        scope.launch {
                            FalconApiClient.renameSession(activeServer.url, rTarget.id, name)
                                .onSuccess { onRefresh() }
                                .onFailure { e -> onToast("重命名失败：${e.message}") }
                        }
                    }
                ) {
                    Text("保存")
                }
            },
            dismissButton = {
                TextButton(onClick = { renameTarget = null }) {
                    Text("取消", color = theme.mutedForeground)
                }
            }
        )
    }

    // 终止会话确认框
    val tTarget = terminateTarget
    if (tTarget != null) {
        AlertDialog(
            onDismissRequest = { terminateTarget = null },
            containerColor = theme.card,
            title = {
                Text(
                    text = if (terminateBusyCmd != null) {
                        "「${tTarget.displayLabel}」里有程序正在运行"
                    } else {
                        "终止会话「${tTarget.displayLabel}」？"
                    },
                    color = theme.foreground
                )
            },
            text = {
                Text(
                    text = if (terminateBusyCmd != null) {
                        "正在运行「$terminateBusyCmd」。终止会话会把正在运行的命令一并杀掉，此操作不可撤销。"
                    } else {
                        "该会话中正在运行的命令会被杀掉。此操作不可撤销。"
                    },
                    color = theme.mutedForeground,
                    fontSize = 13.sp
                )
            },
            confirmButton = {
                Button(
                    colors = ButtonDefaults.buttonColors(containerColor = theme.destructive),
                    onClick = {
                        terminateTarget = null
                        scope.launch {
                            FalconApiClient.terminateSession(activeServer.url, tTarget.id)
                                .onSuccess { onRefresh() }
                                .onFailure { e -> onToast("终止失败：${e.message}") }
                        }
                    }
                ) {
                    Text("终止会话", color = Color.White)
                }
            },
            dismissButton = {
                TextButton(onClick = { terminateTarget = null }) {
                    Text("取消", color = theme.mutedForeground)
                }
            }
        )
    }
}

private fun handleSessionClick(
    baseUrl: String,
    session: SessionUi,
    theme: FalconThemeTokens,
    scope: kotlinx.coroutines.CoroutineScope,
    onOpenSession: (SessionUi) -> Unit,
    onRefresh: () -> Unit,
    onToast: (String) -> Unit
) {
    when (session.state) {
        "dead" -> {
            // 已丢失会话点击时用同样配置新建
            scope.launch {
                FalconApiClient.createSession(
                    baseUrl = baseUrl,
                    projectId = session.projectId,
                    name = session.name,
                    agent = session.agent,
                    theme = theme
                ).onSuccess { created ->
                    onRefresh()
                    onOpenSession(created)
                }.onFailure { e ->
                    onToast("新建会话失败：${e.message}")
                }
            }
        }
        "unverified" -> {
            scope.launch {
                FalconApiClient.reattachSession(baseUrl, session.id)
                    .onSuccess { reattached ->
                        onRefresh()
                        onOpenSession(reattached)
                    }
                    .onFailure {
                        // 即使预接回报错也允许进入终端视图看原因或重试
                        onOpenSession(session)
                    }
            }
        }
        else -> onOpenSession(session)
    }
}

@Composable
private fun ProjectTreeList(
    servers: List<ServerGroupUi>,
    theme: FalconThemeTokens,
    onNewSession: (projectId: String, title: String) -> Unit,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit
) {
    if (servers.isEmpty()) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text(
                text = "暂无项目或会话，点击右上角「快速终端」可直接启动本机 Shell",
                color = theme.mutedForeground,
                fontSize = 13.sp,
                modifier = Modifier.padding(24.dp)
            )
        }
        return
    }

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        items(servers, key = { it.key }) { server ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(10.dp))
                    .background(theme.card)
                    .padding(10.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                // 机器分组标题行（本机 / 已保存 SSH Host）
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Icon(
                        imageVector = Icons.Default.Dns,
                        contentDescription = null,
                        tint = theme.tintStrong,
                        modifier = Modifier.size(16.dp)
                    )
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = server.name,
                        color = theme.foreground,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.Bold
                    )
                    server.conn?.let { conn ->
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = conn,
                            color = theme.mutedForeground,
                            fontSize = 11.sp,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f)
                        )
                    } ?: Spacer(modifier = Modifier.weight(1f))

                    server.standaloneId?.let { standaloneId ->
                        Text(
                            text = "＋ 独立终端",
                            color = theme.tintStrong,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Medium,
                            modifier = Modifier
                                .clip(RoundedCornerShape(6.dp))
                                .clickable {
                                    onNewSession(standaloneId, "${server.name} · 无项目")
                                }
                                .padding(horizontal = 6.dp, vertical = 2.dp)
                        )
                    }
                }

                // 无项目独立会话
                if (server.standaloneSessions.isNotEmpty()) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(8.dp))
                            .background(theme.background)
                            .padding(8.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp)
                    ) {
                        Text(
                            text = "无项目独立会话",
                            color = theme.mutedForeground,
                            fontSize = 11.sp
                        )
                        for (s in server.standaloneSessions) {
                            SessionItemRow(
                                session = s,
                                theme = theme,
                                onClick = { onClickSession(s) },
                                onRename = { onRenameSession(s) },
                                onTerminate = { onTerminateSession(s) },
                                onDeleteDead = { onDeleteDeadSession(s) }
                            )
                        }
                    }
                }

                // 各文件夹项目及其附属 worktree
                for (folder in server.folders) {
                    ProjectCardSection(
                        project = folder.project,
                        worktrees = folder.worktrees,
                        theme = theme,
                        onNewSession = onNewSession,
                        onClickSession = onClickSession,
                        onRenameSession = onRenameSession,
                        onTerminateSession = onTerminateSession,
                        onDeleteDeadSession = onDeleteDeadSession
                    )
                }
            }
        }
    }
}

@Composable
private fun ProjectCardSection(
    project: ProjectUi,
    worktrees: List<ProjectUi>,
    theme: FalconThemeTokens,
    onNewSession: (projectId: String, title: String) -> Unit,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(theme.background)
            .padding(10.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        ProjectHeaderRow(
            project = project,
            indent = false,
            theme = theme,
            onNewSession = { onNewSession(project.id, project.name) }
        )

        for (s in project.sessions) {
            SessionItemRow(
                session = s,
                theme = theme,
                onClick = { onClickSession(s) },
                onRename = { onRenameSession(s) },
                onTerminate = { onTerminateSession(s) },
                onDeleteDead = { onDeleteDeadSession(s) }
            )
        }

        // 单仓库附属项目（git worktree）缩进展示
        for (wt in worktrees) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 12.dp, top = 4.dp)
                    .clip(RoundedCornerShape(6.dp))
                    .background(theme.card.copy(alpha = 0.65f))
                    .padding(8.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp)
            ) {
                ProjectHeaderRow(
                    project = wt,
                    indent = true,
                    theme = theme,
                    onNewSession = { onNewSession(wt.id, "${project.name} · ${wt.branch ?: wt.name}") }
                )
                for (s in wt.sessions) {
                    SessionItemRow(
                        session = s,
                        theme = theme,
                        onClick = { onClickSession(s) },
                        onRename = { onRenameSession(s) },
                        onTerminate = { onTerminateSession(s) },
                        onDeleteDead = { onDeleteDeadSession(s) }
                    )
                }
            }
        }
    }
}

@Composable
private fun ProjectHeaderRow(
    project: ProjectUi,
    indent: Boolean,
    theme: FalconThemeTokens,
    onNewSession: () -> Unit
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Icon(
            imageVector = Icons.Default.Folder,
            contentDescription = null,
            tint = if (indent) theme.tintStrong else theme.mutedForeground,
            modifier = Modifier.size(15.dp)
        )
        Spacer(modifier = Modifier.width(6.dp))
        Column(modifier = Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = project.branch ?: project.name,
                    color = theme.foreground,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                if (project.isWorktree) {
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = "worktree",
                        color = theme.tintStrong,
                        fontSize = 10.sp,
                        fontFamily = FontFamily.Monospace
                    )
                }
            }
            if (project.workingDir.isNotBlank()) {
                Text(
                    text = project.workingDir,
                    color = theme.mutedForeground,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
            }
        }
        Row(
            modifier = Modifier
                .clip(RoundedCornerShape(6.dp))
                .background(theme.secondary)
                .clickable(onClick = onNewSession)
                .padding(horizontal = 8.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(
                imageVector = Icons.Default.Add,
                contentDescription = "新建终端",
                tint = theme.secondaryForeground,
                modifier = Modifier.size(13.dp)
            )
            Spacer(modifier = Modifier.width(2.dp))
            Text(
                text = "终端",
                color = theme.secondaryForeground,
                fontSize = 11.sp,
                fontWeight = FontWeight.Medium
            )
        }
    }
}

@Composable
private fun AllSessionsOverview(
    allSessions: List<SessionUi>,
    filter: String,
    onFilterChange: (String) -> Unit,
    theme: FalconThemeTokens,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit,
    onClearAllDead: () -> Unit
) {
    val filtered = remember(allSessions, filter) {
        if (filter == "all") allSessions else allSessions.filter { it.state == filter }
    }
    val hasDead = remember(allSessions) { allSessions.any { it.state == "dead" } }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            FilterChip("全部", filter == "all", theme) { onFilterChange("all") }
            FilterChip("运行中", filter == "active", theme) { onFilterChange("active") }
            FilterChip("待接回", filter == "unverified", theme) { onFilterChange("unverified") }
            FilterChip("已丢失", filter == "dead", theme) { onFilterChange("dead") }
            Spacer(modifier = Modifier.weight(1f))
            if (hasDead) {
                Text(
                    text = "清除已丢失",
                    color = theme.destructive,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.clickable(onClick = onClearAllDead)
                )
            }
        }

        if (filtered.isEmpty()) {
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("没有符合筛选的会话", color = theme.mutedForeground, fontSize = 13.sp)
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                items(filtered, key = { it.id }) { s ->
                    SessionItemRow(
                        session = s,
                        showProjectName = true,
                        theme = theme,
                        onClick = { onClickSession(s) },
                        onRename = { onRenameSession(s) },
                        onTerminate = { onTerminateSession(s) },
                        onDeleteDead = { onDeleteDeadSession(s) }
                    )
                }
            }
        }
    }
}

@Composable
private fun SessionItemRow(
    session: SessionUi,
    showProjectName: Boolean = false,
    theme: FalconThemeTokens,
    onClick: () -> Unit,
    onRename: () -> Unit,
    onTerminate: () -> Unit,
    onDeleteDead: () -> Unit
) {
    var menuExpanded by remember { mutableStateOf(false) }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(theme.card)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        val stateDotColor = when (session.state) {
            "active" -> theme.success
            "unverified" -> theme.warning
            else -> theme.destructive
        }
        Box(
            modifier = Modifier
                .size(8.dp)
                .clip(CircleShape)
                .background(stateDotColor)
        )
        Spacer(modifier = Modifier.width(8.dp))

        Icon(
            imageVector = Icons.Default.Terminal,
            contentDescription = null,
            tint = theme.mutedForeground,
            modifier = Modifier.size(15.dp)
        )
        Spacer(modifier = Modifier.width(6.dp))

        Column(modifier = Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = session.displayLabel,
                    color = theme.foreground,
                    fontSize = 13.sp,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                if (!session.durable) {
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = "非持久",
                        color = theme.mutedForeground,
                        fontSize = 10.sp
                    )
                }
            }
            val subLine = buildString {
                if (showProjectName) {
                    append(session.projectName)
                    append(" · ")
                }
                when (session.state) {
                    "active" -> append(if (session.durable) "运行中 · 持久" else "运行中")
                    "unverified" -> append("待接回（点击接回）")
                    "dead" -> append(session.deadReasonText ?: "已丢失")
                }
                session.badge?.detail?.let { detail ->
                    append(" · ")
                    append(detail)
                }
            }
            Text(
                text = subLine,
                color = theme.mutedForeground,
                fontSize = 11.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
        }

        // 程序状态徽标（OSC 7501）
        session.badge?.let { badge ->
            val badgeColor = when (badge.tone) {
                "blocked" -> theme.warning
                "error" -> theme.destructive
                "done" -> theme.success
                else -> theme.tintStrong
            }
            Box(
                modifier = Modifier
                    .padding(end = 4.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(badgeColor.copy(alpha = 0.16f))
                    .padding(horizontal = 7.dp, vertical = 2.dp)
            ) {
                Text(
                    text = badge.shortLabel,
                    color = badgeColor,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.SemiBold
                )
            }
        }

        Box {
            IconButton(
                onClick = { menuExpanded = true },
                modifier = Modifier.size(28.dp)
            ) {
                Icon(
                    imageVector = Icons.Default.MoreVert,
                    contentDescription = "会话操作",
                    tint = theme.mutedForeground,
                    modifier = Modifier.size(16.dp)
                )
            }
            DropdownMenu(
                expanded = menuExpanded,
                onDismissRequest = { menuExpanded = false },
                modifier = Modifier.background(theme.card)
            ) {
                if (session.state != "dead") {
                    DropdownMenuItem(
                        text = { Text("打开终端", color = theme.foreground) },
                        onClick = {
                            menuExpanded = false
                            onClick()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("重命名会话…", color = theme.foreground) },
                        onClick = {
                            menuExpanded = false
                            onRename()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("终止会话…", color = theme.destructive) },
                        onClick = {
                            menuExpanded = false
                            onTerminate()
                        }
                    )
                } else {
                    DropdownMenuItem(
                        text = { Text("用同样配置新建", color = theme.foreground) },
                        onClick = {
                            menuExpanded = false
                            onClick()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("清除记录", color = theme.destructive) },
                        onClick = {
                            menuExpanded = false
                            onDeleteDead()
                        }
                    )
                }
            }
        }
    }
}

@Composable
private fun TabPill(
    title: String,
    selected: Boolean,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(8.dp))
            .background(if (selected) theme.secondary else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp)
    ) {
        Text(
            text = title,
            color = if (selected) theme.secondaryForeground else theme.mutedForeground,
            fontSize = 12.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal
        )
    }
}

@Composable
private fun FilterChip(
    label: String,
    selected: Boolean,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(if (selected) theme.tint else theme.card)
            .clickable(onClick = onClick)
            .padding(horizontal = 9.dp, vertical = 4.dp)
    ) {
        Text(
            text = label,
            color = if (selected) theme.tintForeground else theme.mutedForeground,
            fontSize = 11.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal
        )
    }
}

@Composable
private fun AgentChoiceRow(
    label: String,
    subtitle: String,
    selected: Boolean,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(if (selected) theme.tint else theme.background)
            .border(
                width = 1.dp,
                color = if (selected) theme.tintStrong else Color.Transparent,
                shape = RoundedCornerShape(8.dp)
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = label,
                color = if (selected) theme.tintForeground else theme.foreground,
                fontSize = 13.sp,
                fontWeight = FontWeight.Medium
            )
            Text(
                text = subtitle,
                color = theme.mutedForeground,
                fontSize = 11.sp,
                fontFamily = FontFamily.Monospace
            )
        }
    }
}
