package com.falcon.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
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
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MoreHoriz
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
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
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
    val monoFont = LocalFalconMonoFont.current
    val scope = rememberCoroutineScope()

    var currentTab by remember { mutableIntStateOf(0) } // 0 = 项目树, 1 = 全部会话
    var sessionFilter by remember { mutableStateOf("all") } // all / active / unverified / dead

    var showServerDialog by remember { mutableStateOf(false) }
    var editingServer by remember { mutableStateOf<ServerProfile?>(null) }
    var serverNameInput by remember { mutableStateOf("") }
    var serverUrlInput by remember { mutableStateOf("") }

    var showThemeDialog by remember { mutableStateOf(false) }
    var themeSearchQuery by remember { mutableStateOf("") }
    var showSettingsMenu by remember { mutableStateOf(false) }
    var showAbout by remember(activeServer.url) { mutableStateOf(false) }
    var showNodeSettings by remember(activeServer.url) { mutableStateOf(false) }
    var updateTarget by remember { mutableStateOf<UpdateTarget?>(null) }

    // 折叠状态（默认全部展开，点击可收起）
    val collapsedServers = remember { mutableStateMapOf<String, Boolean>() }
    val collapsedProjects = remember { mutableStateMapOf<String, Boolean>() }

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

    val activeCount = remember(dashboard.allSessions) {
        dashboard.allSessions.count { it.state == "active" }
    }
    val blockedCount = remember(dashboard.allSessions) {
        dashboard.allSessions.count { it.state == "active" && it.badge?.tone == "blocked" }
    }
    val unverifiedCount = remember(dashboard.allSessions) {
        dashboard.allSessions.count { it.state == "unverified" }
    }
    val deadCount = remember(dashboard.allSessions) {
        dashboard.allSessions.count { it.state == "dead" }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(theme.app)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 6.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp)
    ) {
        // 顶部紧凑浮动岛：第一行服务器与快捷动作，第二行下沉分段切换与内联筛选/状态计数
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .background(theme.background)
                .padding(horizontal = 8.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp)
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(28.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(5.dp)
            ) {
                // 下沉式服务端切换胶囊
                val statusColor = when {
                    isLoading -> theme.warning
                    connectionError != null -> theme.destructive
                    else -> theme.success
                }
                val shortEndpoint = remember(activeServer.url) {
                    activeServer.url
                        .removePrefix("http://")
                        .removePrefix("https://")
                        .trimEnd('/')
                }
                Row(
                    modifier = Modifier
                        .weight(1f)
                        .height(28.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.app)
                        .clickable { showServerDialog = true }
                        .padding(horizontal = 8.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Box(
                        modifier = Modifier
                            .size(6.dp)
                            .clip(CircleShape)
                            .background(statusColor)
                    )
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = activeServer.name,
                        color = theme.foreground,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = shortEndpoint,
                        color = theme.mutedForeground,
                        fontSize = 10.5.sp,
                        fontFamily = monoFont,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false)
                    )
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        text = "▾",
                        color = theme.tintStrong,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.Bold
                    )
                }

                Box {
                    CompactIconBox(
                        icon = Icons.Default.MoreHoriz,
                        contentDescription = t("serverUpdate.settings"),
                        tint = theme.mutedForeground,
                        bgColor = theme.app,
                        onClick = { showSettingsMenu = true }
                    )
                    DropdownMenu(expanded = showSettingsMenu, onDismissRequest = { showSettingsMenu = false }) {
                        DropdownMenuItem(text = { Text(t("settings.aboutTitle")) }, onClick = {
                            showSettingsMenu = false
                            showAbout = true
                        })
                        DropdownMenuItem(text = { Text(t("settings.tab_hosts")) }, onClick = {
                            showSettingsMenu = false
                            showNodeSettings = true
                        })
                    }
                }

                // 主题按钮（紧凑 28dp，避免 Material IconButton 默认 48dp 撑高）
                CompactIconBox(
                    icon = Icons.Default.Palette,
                    contentDescription = "切换主题",
                    tint = theme.mutedForeground,
                    bgColor = theme.app,
                    onClick = { showThemeDialog = true }
                )

                // 刷新按钮
                Box(
                    modifier = Modifier
                        .size(28.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.app)
                        .clickable(onClick = onRefresh),
                    contentAlignment = Alignment.Center
                ) {
                    if (isLoading) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(13.dp),
                            strokeWidth = 1.8.dp,
                            color = theme.tintStrong
                        )
                    } else {
                        Icon(
                            imageVector = Icons.Default.Refresh,
                            contentDescription = "刷新",
                            tint = theme.mutedForeground,
                            modifier = Modifier.size(15.dp)
                        )
                    }
                }

                // 快速本机独立终端按钮
                Row(
                    modifier = Modifier
                        .height(28.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.tint)
                        .clickable {
                            openCreateSessionDialog("standalone:local", "本机 · 无项目")
                        }
                        .padding(horizontal = 8.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Icon(
                        imageVector = Icons.Default.Add,
                        contentDescription = null,
                        tint = theme.tintForeground,
                        modifier = Modifier.size(13.dp)
                    )
                    Spacer(modifier = Modifier.width(2.dp))
                    Text(
                        text = "终端",
                        color = theme.tintForeground,
                        fontSize = 11.5.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                }
            }

            if (connectionError != null) {
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.destructive.copy(alpha = 0.14f))
                        .clickable { showServerDialog = true }
                        .padding(horizontal = 8.dp, vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = "连接失败：$connectionError",
                        color = theme.destructive,
                        fontSize = 11.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f)
                    )
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = "切换/扫描 ▸",
                        color = theme.tintStrong,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                }
            }

            // 第二行：左侧下沉分段控件（项目树 / 全部会话），右侧状态摘要或内联过滤胶囊
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(26.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                // 下沉式分段槽（借窗口底色 theme.app）
                Row(
                    modifier = Modifier
                        .height(26.dp)
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.app)
                        .padding(2.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    SegmentedTabButton(
                        title = "项目树",
                        selected = currentTab == 0,
                        theme = theme,
                        onClick = { currentTab = 0 }
                    )
                    SegmentedTabButton(
                        title = "会话 (${dashboard.allSessions.size})",
                        badgeDotColor = if (blockedCount > 0) theme.warning else null,
                        selected = currentTab == 1,
                        theme = theme,
                        onClick = { currentTab = 1 }
                    )
                }

                Spacer(modifier = Modifier.weight(1f))

                if (currentTab == 0) {
                    // 项目树模式下展示紧凑状态速览芯片，点击直接切到对应筛选的会话列表
                    if (blockedCount > 0) {
                        StatusSummaryPill(
                            dotColor = theme.warning,
                            text = "$blockedCount 等你",
                            bgColor = theme.warning.copy(alpha = 0.15f),
                            textColor = theme.warning,
                            monoFont = monoFont,
                            onClick = {
                                sessionFilter = "active"
                                currentTab = 1
                            }
                        )
                    }
                    if (unverifiedCount > 0) {
                        StatusSummaryPill(
                            dotColor = theme.warning,
                            text = "$unverifiedCount 待接回",
                            bgColor = theme.app,
                            textColor = theme.warning,
                            monoFont = monoFont,
                            onClick = {
                                sessionFilter = "unverified"
                                currentTab = 1
                            }
                        )
                    }
                    if (activeCount > 0) {
                        StatusSummaryPill(
                            dotColor = theme.success,
                            text = "$activeCount 运行",
                            bgColor = theme.app,
                            textColor = theme.mutedForeground,
                            monoFont = monoFont,
                            onClick = {
                                sessionFilter = "active"
                                currentTab = 1
                            }
                        )
                    }
                } else {
                    // 全部会话模式下直接把筛选胶囊内联在同一行，省去额外工具栏行高
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(3.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        FilterChip("全部", sessionFilter == "all", theme) { sessionFilter = "all" }
                        FilterChip("运行", sessionFilter == "active", theme) { sessionFilter = "active" }
                        if (unverifiedCount > 0 || sessionFilter == "unverified") {
                            FilterChip("待接回", sessionFilter == "unverified", theme) { sessionFilter = "unverified" }
                        }
                        if (deadCount > 0 || sessionFilter == "dead") {
                            FilterChip("已丢失", sessionFilter == "dead", theme) { sessionFilter = "dead" }
                        }
                    }
                }
            }
        }

        // 主体内容浮动岛
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .background(theme.background)
        ) {
            if (currentTab == 0) {
                ProjectTreeList(
                    servers = dashboard.servers,
                    collapsedServers = collapsedServers,
                    collapsedProjects = collapsedProjects,
                    theme = theme,
                    monoFont = monoFont,
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
                    theme = theme,
                    monoFont = monoFont,
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

    // 新建会话弹窗（紧凑列表，支持单击 Agent 选中或直接启动）
    val targetPid = createTargetProjectId
    if (targetPid != null) {
        AlertDialog(
            onDismissRequest = { if (!creatingSession) createTargetProjectId = null },
            containerColor = theme.card,
            title = {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text(
                        text = "新建终端",
                        color = theme.foreground,
                        fontSize = 15.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                    Text(
                        text = createTargetTitle,
                        color = theme.mutedForeground,
                        fontSize = 11.5.sp,
                        fontFamily = monoFont,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                }
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(
                        value = customSessionName,
                        onValueChange = { customSessionName = it },
                        placeholder = { Text("自定义标题（可选，留空自动跟随命令）", fontSize = 12.sp) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )

                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween
                    ) {
                        Text(
                            text = "启动环境",
                            color = theme.mutedForeground,
                            fontSize = 11.5.sp,
                            fontWeight = FontWeight.Medium
                        )
                        if (loadingAgents) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(4.dp)
                            ) {
                                CircularProgressIndicator(
                                    modifier = Modifier.size(11.dp),
                                    strokeWidth = 1.5.dp,
                                    color = theme.tintStrong
                                )
                                Text("检测 CLI…", color = theme.mutedForeground, fontSize = 10.5.sp)
                            }
                        }
                    }

                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 240.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp)
                    ) {
                        item(key = "__shell__") {
                            AgentChoiceRow(
                                label = "交互式 Shell",
                                cliName = "shell",
                                selected = selectedAgent == null,
                                theme = theme,
                                monoFont = monoFont,
                                onClick = { selectedAgent = null }
                            )
                        }
                        items(detectedAgents, key = { it }) { agentId ->
                            AgentChoiceRow(
                                label = AgentCatalog.labelFor(agentId),
                                cliName = agentId,
                                selected = selectedAgent == agentId,
                                theme = theme,
                                monoFont = monoFont,
                                onClick = { selectedAgent = agentId }
                            )
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
                    Text(if (creatingSession) "创建中…" else "启动", fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(
                    enabled = !creatingSession,
                    onClick = { createTargetProjectId = null }
                ) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }

    if (showAbout) {
        ServerAboutDialog(activeServer, onDismiss = { showAbout = false }, onUpdate = { updateTarget = it })
    }
    if (showNodeSettings) {
        NodeSettingsDialog(activeServer, onDismiss = { showNodeSettings = false }, onUpdate = { updateTarget = it })
    }
    updateTarget?.let { target -> ServerUpdateDialog(target, onDismiss = { updateTarget = null }) }

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
                    Text("服务端连接", color = theme.foreground, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
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
                                    "未发现开启局域网访问的 Falcon (:4923)"
                                } else {
                                    "发现 ${found.size} 台局域网服务端，点击即连"
                                }
                            }
                        }
                    ) {
                        Text(if (scanningLan) "扫描中…" else "扫描局域网", fontSize = 12.sp)
                    }
                }
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
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
                                .clip(RoundedCornerShape(6.dp))
                                .background(theme.tint)
                                .clickable {
                                    val label = "局域网 ${disc.platform} (${disc.url.removePrefix("http://")})"
                                    onAddOrUpdateServer(label, disc.url, null)
                                    showServerDialog = false
                                }
                                .padding(horizontal = 8.dp, vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = "Falcon v${disc.version} (${disc.platform})",
                                    color = theme.tintForeground,
                                    fontSize = 12.5.sp,
                                    fontWeight = FontWeight.SemiBold
                                )
                                Text(
                                    text = disc.url,
                                    color = theme.mutedForeground,
                                    fontSize = 10.5.sp,
                                    fontFamily = monoFont
                                )
                            }
                            Text("连接", color = theme.tintForeground, fontSize = 11.5.sp, fontWeight = FontWeight.Medium)
                        }
                    }

                    for (profile in serverProfiles) {
                        val isCurrent = profile.id == activeServer.id
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clip(RoundedCornerShape(6.dp))
                                .background(if (isCurrent) theme.tint else theme.background)
                                .clickable {
                                    onSelectServer(profile)
                                    showServerDialog = false
                                }
                                .padding(horizontal = 8.dp, vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = profile.name,
                                    color = if (isCurrent) theme.tintForeground else theme.foreground,
                                    fontSize = 12.5.sp,
                                    fontWeight = FontWeight.SemiBold,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis
                                )
                                Text(
                                    text = profile.url,
                                    color = theme.mutedForeground,
                                    fontSize = 10.5.sp,
                                    fontFamily = monoFont,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis
                                )
                            }
                            Text(
                                text = "编辑",
                                color = theme.tintStrong,
                                fontSize = 11.5.sp,
                                modifier = Modifier
                                    .clip(RoundedCornerShape(4.dp))
                                    .clickable {
                                        editingServer = profile
                                        serverNameInput = profile.name
                                        serverUrlInput = profile.url
                                    }
                                    .padding(horizontal = 6.dp, vertical = 3.dp)
                            )
                            if (serverProfiles.size > 1) {
                                Box(
                                    modifier = Modifier
                                        .size(24.dp)
                                        .clip(RoundedCornerShape(4.dp))
                                        .clickable { onDeleteServer(profile.id) },
                                    contentAlignment = Alignment.Center
                                ) {
                                    Icon(
                                        imageVector = Icons.Default.DeleteOutline,
                                        contentDescription = "删除",
                                        tint = theme.destructive,
                                        modifier = Modifier.size(15.dp)
                                    )
                                }
                            }
                        }
                    }

                    Spacer(modifier = Modifier.height(2.dp))
                    Text(
                        text = if (editingServer != null) "编辑服务端" else "添加服务端",
                        color = theme.mutedForeground,
                        fontSize = 11.5.sp
                    )
                    OutlinedTextField(
                        value = serverNameInput,
                        onValueChange = { serverNameInput = it },
                        label = { Text("名称（如：办公 Mac / 实验机 212）", fontSize = 12.sp) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )
                    OutlinedTextField(
                        value = serverUrlInput,
                        onValueChange = { serverUrlInput = it },
                        label = { Text("地址（如 http://192.168.1.10:4923）", fontSize = 12.sp) },
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
                    Text(if (serverUrlInput.isNotBlank()) "保存并连接" else "关闭", fontSize = 13.sp)
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
                        Text("取消编辑", color = theme.mutedForeground, fontSize = 13.sp)
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
                Text("选择主题（465 套）", color = theme.foreground, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    OutlinedTextField(
                        value = themeSearchQuery,
                        onValueChange = { themeSearchQuery = it },
                        placeholder = { Text("搜索主题（Falcon / Catppuccin / Tokyo）", fontSize = 12.sp) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth()
                    )
                    LazyColumn(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(280.dp),
                        verticalArrangement = Arrangement.spacedBy(3.dp)
                    ) {
                        items(themes, key = { it.name }) { item ->
                            val isSelected = item.name.equals(themeName, ignoreCase = true)
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .height(34.dp)
                                    .clip(RoundedCornerShape(6.dp))
                                    .background(if (isSelected) theme.tint else theme.background)
                                    .clickable {
                                        onSelectTheme(item.name)
                                        showThemeDialog = false
                                    }
                                    .padding(horizontal = 8.dp),
                                verticalAlignment = Alignment.CenterVertically
                            ) {
                                Box(
                                    modifier = Modifier
                                        .size(16.dp)
                                        .clip(CircleShape)
                                        .background(parseHexColor(item.backgroundHex))
                                        .border(1.dp, parseHexColor(item.accentHex), CircleShape),
                                    contentAlignment = Alignment.Center
                                ) {
                                    Box(
                                        modifier = Modifier
                                            .size(6.dp)
                                            .clip(CircleShape)
                                            .background(parseHexColor(item.foregroundHex))
                                    )
                                }
                                Spacer(modifier = Modifier.width(8.dp))
                                Text(
                                    text = item.name,
                                    color = if (isSelected) theme.tintForeground else theme.foreground,
                                    fontSize = 12.5.sp,
                                    fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                    modifier = Modifier.weight(1f)
                                )
                                Text(
                                    text = if (item.isDark) "深" else "浅",
                                    color = theme.mutedForeground,
                                    fontSize = 10.5.sp
                                )
                            }
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { showThemeDialog = false }) {
                    Text("关闭", fontSize = 13.sp)
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
            title = { Text("重命名会话", color = theme.foreground, fontSize = 15.sp, fontWeight = FontWeight.SemiBold) },
            text = {
                OutlinedTextField(
                    value = renameInput,
                    onValueChange = { renameInput = it },
                    placeholder = { Text("留空则恢复自动标题", fontSize = 12.sp) },
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
                    Text("保存", fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { renameTarget = null }) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
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
                        "「${tTarget.displayLabel}」正在运行程序"
                    } else {
                        "终止会话「${tTarget.displayLabel}」？"
                    },
                    color = theme.foreground,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold
                )
            },
            text = {
                Text(
                    text = if (terminateBusyCmd != null) {
                        "正在运行「$terminateBusyCmd」。终止会话会把正在运行的命令一并结束，不可撤销。"
                    } else {
                        "该会话中正在运行的命令会被结束，此操作不可撤销。"
                    },
                    color = theme.mutedForeground,
                    fontSize = 12.5.sp
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
                    Text("终止会话", color = Color.White, fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { terminateTarget = null }) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
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
    collapsedServers: MutableMap<String, Boolean>,
    collapsedProjects: MutableMap<String, Boolean>,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onNewSession: (projectId: String, title: String) -> Unit,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit
) {
    if (servers.isEmpty()) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text(
                text = "暂无项目或会话，点击右上角「＋终端」直接启动本机 Shell",
                color = theme.mutedForeground,
                fontSize = 12.sp,
                modifier = Modifier.padding(16.dp)
            )
        }
        return
    }

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(horizontal = 6.dp, vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        items(servers, key = { it.key }) { server ->
            val isCollapsed = collapsedServers[server.key] == true
            val totalServerSessions = remember(server) {
                server.standaloneSessions.size +
                    server.folders.sumOf { f ->
                        f.project.sessions.size + f.worktrees.sumOf { it.sessions.size }
                    }
            }
            val barColor = theme.hostBarColor(server.kind, server.conn)

            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(8.dp))
                    .background(theme.app.copy(alpha = 0.55f))
                    .padding(horizontal = 6.dp, vertical = 5.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp)
            ) {
                // 机器分组标题行（对齐桌面侧栏：折叠箭头 + 3dp 主机身份色条 + 图标 + 主机名 + 连接串 + 独立终端）
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(26.dp)
                        .clip(RoundedCornerShape(5.dp))
                        .clickable {
                            collapsedServers[server.key] = !isCollapsed
                        }
                        .padding(horizontal = 4.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Icon(
                        imageVector = if (isCollapsed) Icons.AutoMirrored.Filled.KeyboardArrowRight else Icons.Default.KeyboardArrowDown,
                        contentDescription = null,
                        tint = theme.mutedForeground,
                        modifier = Modifier.size(14.dp)
                    )
                    Spacer(modifier = Modifier.width(3.dp))
                    // 3dp 宽主机色带（SSH 主机按 user@host:port 哈希着色，本机中性）
                    Box(
                        modifier = Modifier
                            .width(3.dp)
                            .height(14.dp)
                            .clip(CircleShape)
                            .background(barColor)
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    Icon(
                        imageVector = if (server.kind == "local") Icons.Default.Computer else Icons.Default.Dns,
                        contentDescription = null,
                        tint = theme.mutedForeground,
                        modifier = Modifier.size(13.dp)
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    Text(
                        text = server.name,
                        color = theme.foreground,
                        fontSize = 12.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                    server.conn?.let { conn ->
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = conn,
                            color = theme.mutedForeground,
                            fontSize = 10.5.sp,
                            fontFamily = monoFont,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f)
                        )
                    } ?: Spacer(modifier = Modifier.weight(1f))

                    if (isCollapsed && totalServerSessions > 0) {
                        Text(
                            text = "$totalServerSessions",
                            color = theme.mutedForeground,
                            fontSize = 10.5.sp,
                            fontFamily = monoFont,
                            modifier = Modifier
                                .padding(end = 6.dp)
                                .clip(RoundedCornerShape(4.dp))
                                .background(theme.card)
                                .padding(horizontal = 5.dp, vertical = 1.dp)
                        )
                    }

                    server.standaloneId?.let { standaloneId ->
                        Row(
                            modifier = Modifier
                                .height(20.dp)
                                .clip(RoundedCornerShape(4.dp))
                                .background(theme.card)
                                .clickable {
                                    onNewSession(standaloneId, "${server.name} · 无项目")
                                }
                                .padding(horizontal = 6.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Icon(
                                imageVector = Icons.Default.Add,
                                contentDescription = null,
                                tint = theme.tintStrong,
                                modifier = Modifier.size(11.dp)
                            )
                            Spacer(modifier = Modifier.width(2.dp))
                            Text(
                                text = "独立终端",
                                color = theme.tintStrong,
                                fontSize = 10.5.sp,
                                fontWeight = FontWeight.Medium
                            )
                        }
                    }
                }

                if (!isCollapsed) {
                    // 无项目独立会话（仅在非空时展示，不占空位）
                    if (server.standaloneSessions.isNotEmpty()) {
                        val stdKey = "std:${server.key}"
                        val stdCollapsed = collapsedProjects[stdKey] == true
                        CompactProjectBlock(
                            icon = Icons.Default.Terminal,
                            title = "无项目独立会话",
                            branchTag = null,
                            pathHint = null,
                            sessions = server.standaloneSessions,
                            isWorktree = false,
                            collapsed = stdCollapsed,
                            onToggleCollapse = { collapsedProjects[stdKey] = !stdCollapsed },
                            onNewSession = {
                                server.standaloneId?.let { sid ->
                                    onNewSession(sid, "${server.name} · 无项目")
                                }
                            },
                            theme = theme,
                            monoFont = monoFont,
                            onClickSession = onClickSession,
                            onRenameSession = onRenameSession,
                            onTerminateSession = onTerminateSession,
                            onDeleteDeadSession = onDeleteDeadSession
                        )
                    }

                    // 各项目文件夹及其附属 worktree
                    for (folder in server.folders) {
                        val proj = folder.project
                        val projCollapsed = collapsedProjects[proj.id] == true
                        CompactProjectBlock(
                            icon = Icons.Default.Folder,
                            title = proj.name,
                            branchTag = proj.branch,
                            pathHint = shortenPath(proj.workingDir),
                            sessions = proj.sessions,
                            isWorktree = false,
                            collapsed = projCollapsed,
                            onToggleCollapse = {
                                if (proj.sessions.isNotEmpty()) {
                                    collapsedProjects[proj.id] = !projCollapsed
                                } else {
                                    onNewSession(proj.id, proj.name)
                                }
                            },
                            onNewSession = { onNewSession(proj.id, proj.name) },
                            theme = theme,
                            monoFont = monoFont,
                            onClickSession = onClickSession,
                            onRenameSession = onRenameSession,
                            onTerminateSession = onTerminateSession,
                            onDeleteDeadSession = onDeleteDeadSession,
                            worktreesContent = if (folder.worktrees.isNotEmpty()) {
                                {
                                    for (wt in folder.worktrees) {
                                        val wtCollapsed = collapsedProjects[wt.id] == true
                                        CompactProjectBlock(
                                            icon = Icons.Default.AccountTree,
                                            title = wt.branch ?: wt.name,
                                            branchTag = "worktree",
                                            pathHint = shortenPath(wt.workingDir),
                                            sessions = wt.sessions,
                                            isWorktree = true,
                                            collapsed = wtCollapsed,
                                            onToggleCollapse = {
                                                if (wt.sessions.isNotEmpty()) {
                                                    collapsedProjects[wt.id] = !wtCollapsed
                                                } else {
                                                    onNewSession(wt.id, "${proj.name} · ${wt.branch ?: wt.name}")
                                                }
                                            },
                                            onNewSession = {
                                                onNewSession(wt.id, "${proj.name} · ${wt.branch ?: wt.name}")
                                            },
                                            theme = theme,
                                            monoFont = monoFont,
                                            onClickSession = onClickSession,
                                            onRenameSession = onRenameSession,
                                            onTerminateSession = onTerminateSession,
                                            onDeleteDeadSession = onDeleteDeadSession
                                        )
                                    }
                                }
                            } else null
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun CompactProjectBlock(
    icon: ImageVector,
    title: String,
    branchTag: String?,
    pathHint: String?,
    sessions: List<SessionUi>,
    isWorktree: Boolean,
    collapsed: Boolean,
    onToggleCollapse: () -> Unit,
    onNewSession: () -> Unit,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit,
    worktreesContent: (@Composable () -> Unit)? = null
) {
    val hasBlocked = remember(sessions) {
        sessions.any { it.state == "active" && it.badge?.tone == "blocked" }
    }
    val hasUnverified = remember(sessions) {
        sessions.any { it.state == "unverified" }
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = if (isWorktree) 10.dp else 0.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(if (isWorktree) theme.background.copy(alpha = 0.65f) else theme.background)
            .padding(horizontal = 6.dp, vertical = 3.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp)
    ) {
        // 单行高密度项目头（26dp）
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(26.dp)
                .clip(RoundedCornerShape(4.dp))
                .clickable(onClick = onToggleCollapse)
                .padding(horizontal = 2.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = if (isWorktree) theme.tintStrong else theme.mutedForeground,
                modifier = Modifier.size(13.dp)
            )
            Spacer(modifier = Modifier.width(5.dp))
            Text(
                text = title,
                color = theme.foreground,
                fontSize = 12.5.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis
            )
            if (!branchTag.isNullOrBlank()) {
                Spacer(modifier = Modifier.width(5.dp))
                Text(
                    text = branchTag,
                    color = theme.tintStrong,
                    fontSize = 9.5.sp,
                    fontFamily = monoFont,
                    maxLines = 1,
                    modifier = Modifier
                        .clip(RoundedCornerShape(3.dp))
                        .background(theme.tint.copy(alpha = 0.45f))
                        .padding(horizontal = 4.dp, vertical = 1.dp)
                )
            }
            if (!pathHint.isNullOrBlank()) {
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = pathHint,
                    color = theme.mutedForeground.copy(alpha = 0.8f),
                    fontSize = 10.5.sp,
                    fontFamily = monoFont,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f)
                )
            } else {
                Spacer(modifier = Modifier.weight(1f))
            }

            // 会话计数徽标（参考桌面端 session_count_badge：折叠或有异常时醒目提示）
            if (sessions.isNotEmpty()) {
                Row(
                    modifier = Modifier
                        .padding(end = 4.dp)
                        .height(18.dp)
                        .clip(RoundedCornerShape(4.dp))
                        .background(if (collapsed) theme.secondary else theme.app)
                        .padding(horizontal = 5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(3.dp)
                ) {
                    if (hasBlocked) {
                        Box(
                            modifier = Modifier
                                .size(5.dp)
                                .clip(CircleShape)
                                .background(theme.warning)
                        )
                    } else if (hasUnverified) {
                        Box(
                            modifier = Modifier
                                .size(5.dp)
                                .clip(CircleShape)
                                .background(theme.warning)
                        )
                    }
                    Text(
                        text = "${sessions.size}",
                        color = if (collapsed) theme.secondaryForeground else theme.mutedForeground,
                        fontSize = 10.sp,
                        fontFamily = monoFont,
                        fontWeight = FontWeight.Medium
                    )
                }
            }

            // 紧凑 `+` 新建会话按钮（22×20dp）
            Box(
                modifier = Modifier
                    .size(width = 22.dp, height = 20.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .background(theme.secondary)
                    .clickable(onClick = onNewSession),
                contentAlignment = Alignment.Center
            ) {
                Icon(
                    imageVector = Icons.Default.Add,
                    contentDescription = "新建终端",
                    tint = theme.secondaryForeground,
                    modifier = Modifier.size(12.dp)
                )
            }
        }

        // 会话列表（带左侧 1.5dp 树引导竖线，每行单行 30dp）
        if (!collapsed && sessions.isNotEmpty()) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(IntrinsicSize.Min)
                    .padding(start = 6.dp)
            ) {
                Box(
                    modifier = Modifier
                        .width(1.5.dp)
                        .fillMaxHeight()
                        .padding(vertical = 3.dp)
                        .background(theme.muted)
                )
                Spacer(modifier = Modifier.width(6.dp))
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(2.dp)
                ) {
                    for (s in sessions) {
                        SessionItemRow(
                            session = s,
                            showProjectName = false,
                            theme = theme,
                            monoFont = monoFont,
                            onClick = { onClickSession(s) },
                            onRename = { onRenameSession(s) },
                            onTerminate = { onTerminateSession(s) },
                            onDeleteDead = { onDeleteDeadSession(s) }
                        )
                    }
                }
            }
        }

        worktreesContent?.invoke()
    }
}

@Composable
private fun AllSessionsOverview(
    allSessions: List<SessionUi>,
    filter: String,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onClickSession: (SessionUi) -> Unit,
    onRenameSession: (SessionUi) -> Unit,
    onTerminateSession: (SessionUi) -> Unit,
    onDeleteDeadSession: (SessionUi) -> Unit,
    onClearAllDead: () -> Unit
) {
    val filtered = remember(allSessions, filter) {
        if (filter == "all") allSessions else allSessions.filter { it.state == filter }
    }
    val deadCount = remember(allSessions) { allSessions.count { it.state == "dead" } }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 6.dp, vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        if (deadCount > 0) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween
            ) {
                Text(
                    text = "共 ${filtered.size} 条会话",
                    color = theme.mutedForeground,
                    fontSize = 11.sp
                )
                Text(
                    text = "清除已丢失 ($deadCount)",
                    color = theme.destructive,
                    fontSize = 11.sp,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier
                        .clip(RoundedCornerShape(4.dp))
                        .clickable(onClick = onClearAllDead)
                        .padding(horizontal = 6.dp, vertical = 2.dp)
                )
            }
        }

        if (filtered.isEmpty()) {
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("没有符合筛选的会话", color = theme.mutedForeground, fontSize = 12.sp)
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(3.dp)
            ) {
                items(filtered, key = { it.id }) { s ->
                    SessionItemRow(
                        session = s,
                        showProjectName = true,
                        theme = theme,
                        monoFont = monoFont,
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

/**
 * 单行高密度会话行（30dp 高度，参考桌面端 `sidebar.rs::session_row` 与 Termius / Prompt 3 紧凑列表）：
 * `[状态点] [Agent胶囊/Shell图标] [会话标题] [项目名(总览)] [OSC 7501 徽标/状态] [相对时长] [⋯]`
 */
@Composable
private fun SessionItemRow(
    session: SessionUi,
    showProjectName: Boolean = false,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onClick: () -> Unit,
    onRename: () -> Unit,
    onTerminate: () -> Unit,
    onDeleteDead: () -> Unit
) {
    var menuExpanded by remember { mutableStateOf(false) }

    val stateDotColor = when (session.state) {
        "active" -> if (session.badge?.tone == "blocked") theme.warning else theme.success
        "unverified" -> theme.warning
        else -> theme.destructive
    }
    val isDead = session.state == "dead"
    val relativeAge = remember(session.lastActiveAt, session.createdAt) {
        val ts = if (session.lastActiveAt > 0L) session.lastActiveAt else session.createdAt
        formatRelativeTime(ts)
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(30.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(theme.card)
            .clickable(onClick = onClick)
            .padding(start = 8.dp, end = 4.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        // 1. 状态指示圆点（blocked 时为空心警示圈+中心点，对齐桌面端 needs_you_mark）
        if (session.state == "active" && session.badge?.tone == "blocked") {
            Box(
                modifier = Modifier
                    .size(9.dp)
                    .border(1.2.dp, theme.warning, CircleShape),
                contentAlignment = Alignment.Center
            ) {
                Box(
                    modifier = Modifier
                        .size(3.5.dp)
                        .clip(CircleShape)
                        .background(theme.warning)
                )
            }
        } else {
            Box(
                modifier = Modifier
                    .size(6.dp)
                    .clip(CircleShape)
                    .background(stateDotColor)
            )
        }
        Spacer(modifier = Modifier.width(6.dp))

        // 2. Agent 标识胶囊或终端图标
        if (!session.agent.isNullOrBlank()) {
            Text(
                text = session.agent,
                color = theme.secondaryForeground,
                fontSize = 9.5.sp,
                fontFamily = monoFont,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                modifier = Modifier
                    .clip(RoundedCornerShape(3.dp))
                    .background(theme.secondary)
                    .padding(horizontal = 4.dp, vertical = 1.dp)
            )
        } else {
            Icon(
                imageVector = Icons.Default.Terminal,
                contentDescription = null,
                tint = theme.mutedForeground,
                modifier = Modifier.size(12.dp)
            )
        }
        Spacer(modifier = Modifier.width(6.dp))

        // 3. 会话主标题（单行截断）
        Text(
            text = session.displayLabel,
            color = if (isDead) theme.mutedForeground else theme.foreground,
            fontSize = 12.sp,
            fontFamily = monoFont,
            fontWeight = if (session.badge?.tone == "done" || session.badge?.tone == "blocked") {
                FontWeight.SemiBold
            } else {
                FontWeight.Medium
            },
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f)
        )

        // 4. 总览模式下的所属项目短标签
        if (showProjectName) {
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = session.projectName,
                color = theme.mutedForeground,
                fontSize = 10.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier
                    .clip(RoundedCornerShape(3.dp))
                    .background(theme.app)
                    .padding(horizontal = 4.dp, vertical = 1.dp)
            )
        }

        // 5. 状态说明 / OSC 7501 程序状态胶囊
        Spacer(modifier = Modifier.width(4.dp))
        when {
            session.state == "dead" -> {
                Text(
                    text = session.deadReasonText ?: "已丢失",
                    color = theme.destructive,
                    fontSize = 10.sp,
                    maxLines = 1
                )
            }
            session.state == "unverified" -> {
                Text(
                    text = "待接回",
                    color = theme.warning,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1
                )
            }
            session.badge != null -> {
                val badge = session.badge
                val badgeColor = when (badge.tone) {
                    "blocked" -> theme.warning
                    "error" -> theme.destructive
                    "done" -> theme.success
                    else -> theme.tintStrong
                }
                val badgeText = buildString {
                    append(badge.shortLabel)
                    if (badge.progress != null) {
                        append(" ${badge.progress}%")
                    }
                }
                Box(
                    modifier = Modifier
                        .clip(RoundedCornerShape(4.dp))
                        .background(badgeColor.copy(alpha = 0.16f))
                        .padding(horizontal = 5.dp, vertical = 1.dp)
                ) {
                    Text(
                        text = badgeText,
                        color = badgeColor,
                        fontSize = 9.5.sp,
                        fontFamily = monoFont,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1
                    )
                }
            }
            !session.durable -> {
                Text(
                    text = "非持久",
                    color = theme.mutedForeground,
                    fontSize = 9.5.sp,
                    maxLines = 1
                )
            }
        }

        // 6. 相对活跃时间（对齐桌面端 age_text / idle_text）
        if (relativeAge.isNotEmpty()) {
            Spacer(modifier = Modifier.width(5.dp))
            Text(
                text = relativeAge,
                color = theme.mutedForeground.copy(alpha = 0.8f),
                fontSize = 10.sp,
                fontFamily = monoFont,
                maxLines = 1
            )
        }

        // 7. 紧凑更多菜单（22dp 点击热区，不使用撑高行距的 Material IconButton）
        Box {
            Box(
                modifier = Modifier
                    .size(22.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .clickable { menuExpanded = true },
                contentAlignment = Alignment.Center
            ) {
                Icon(
                    imageVector = Icons.Default.MoreHoriz,
                    contentDescription = "会话操作",
                    tint = theme.mutedForeground,
                    modifier = Modifier.size(14.dp)
                )
            }
            DropdownMenu(
                expanded = menuExpanded,
                onDismissRequest = { menuExpanded = false },
                modifier = Modifier.background(theme.card)
            ) {
                if (session.state != "dead") {
                    DropdownMenuItem(
                        text = { Text("打开终端", color = theme.foreground, fontSize = 13.sp) },
                        onClick = {
                            menuExpanded = false
                            onClick()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("重命名会话…", color = theme.foreground, fontSize = 13.sp) },
                        onClick = {
                            menuExpanded = false
                            onRename()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("终止会话…", color = theme.destructive, fontSize = 13.sp) },
                        onClick = {
                            menuExpanded = false
                            onTerminate()
                        }
                    )
                } else {
                    DropdownMenuItem(
                        text = { Text("用同样配置新建", color = theme.foreground, fontSize = 13.sp) },
                        onClick = {
                            menuExpanded = false
                            onClick()
                        }
                    )
                    DropdownMenuItem(
                        text = { Text("清除记录", color = theme.destructive, fontSize = 13.sp) },
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
private fun CompactIconBox(
    icon: ImageVector,
    contentDescription: String,
    tint: Color,
    bgColor: Color,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .size(28.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(bgColor)
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center
    ) {
        Icon(
            imageVector = icon,
            contentDescription = contentDescription,
            tint = tint,
            modifier = Modifier.size(15.dp)
        )
    }
}

@Composable
private fun SegmentedTabButton(
    title: String,
    badgeDotColor: Color? = null,
    selected: Boolean,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxHeight()
            .clip(RoundedCornerShape(4.dp))
            .background(if (selected) theme.secondary else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        Text(
            text = title,
            color = if (selected) theme.secondaryForeground else theme.mutedForeground,
            fontSize = 11.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal
        )
        if (badgeDotColor != null) {
            Box(
                modifier = Modifier
                    .size(5.dp)
                    .clip(CircleShape)
                    .background(badgeDotColor)
            )
        }
    }
}

@Composable
private fun StatusSummaryPill(
    dotColor: Color,
    text: String,
    bgColor: Color,
    textColor: Color,
    monoFont: FontFamily,
    onClick: () -> Unit
) {
    Row(
        modifier = Modifier
            .height(22.dp)
            .clip(RoundedCornerShape(5.dp))
            .background(bgColor)
            .clickable(onClick = onClick)
            .padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        Box(
            modifier = Modifier
                .size(5.dp)
                .clip(CircleShape)
                .background(dotColor)
        )
        Text(
            text = text,
            color = textColor,
            fontSize = 10.5.sp,
            fontFamily = monoFont,
            fontWeight = FontWeight.Medium
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
            .height(22.dp)
            .clip(RoundedCornerShape(5.dp))
            .background(if (selected) theme.tint else theme.app)
            .clickable(onClick = onClick)
            .padding(horizontal = 7.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = label,
            color = if (selected) theme.tintForeground else theme.mutedForeground,
            fontSize = 10.5.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal
        )
    }
}

@Composable
private fun AgentChoiceRow(
    label: String,
    cliName: String,
    selected: Boolean,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onClick: () -> Unit
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(34.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(if (selected) theme.tint else theme.background)
            .border(
                width = 1.dp,
                color = if (selected) theme.tintStrong else Color.Transparent,
                shape = RoundedCornerShape(6.dp)
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(
            text = label,
            color = if (selected) theme.tintForeground else theme.foreground,
            fontSize = 12.5.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
            modifier = Modifier.weight(1f)
        )
        Text(
            text = cliName,
            color = theme.mutedForeground,
            fontSize = 11.sp,
            fontFamily = monoFont
        )
        if (selected) {
            Spacer(modifier = Modifier.width(6.dp))
            Icon(
                imageVector = Icons.Default.Check,
                contentDescription = null,
                tint = theme.tintStrong,
                modifier = Modifier.size(14.dp)
            )
        }
    }
}

private fun shortenPath(path: String): String {
    if (path.isBlank()) return ""
    val normalized = path.trim()
    val homePrefixes = listOf("/Users/", "/home/")
    for (prefix in homePrefixes) {
        if (normalized.startsWith(prefix)) {
            val rest = normalized.removePrefix(prefix)
            val slashIdx = rest.indexOf('/')
            if (slashIdx >= 0) {
                return "~" + rest.substring(slashIdx)
            }
            return "~"
        }
    }
    return normalized
}

private fun formatRelativeTime(timestampMs: Long): String {
    if (timestampMs <= 0L) return ""
    val now = System.currentTimeMillis()
    val diffMin = ((now - timestampMs).coerceAtLeast(0L)) / 60_000L
    return when {
        diffMin < 1L -> "刚刚"
        diffMin < 60L -> "${diffMin}m"
        diffMin < 1440L -> "${diffMin / 60L}h"
        else -> "${diffMin / 1440L}d"
    }
}
