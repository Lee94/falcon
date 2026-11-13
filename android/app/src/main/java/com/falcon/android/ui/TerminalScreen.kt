package com.falcon.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import android.view.HapticFeedbackConstants
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Refresh
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.falcon.android.R
import com.falcon.android.bridge.FalconNative
import com.falcon.android.model.AgentCatalog
import com.falcon.android.model.AgentModeTransition
import com.falcon.android.model.AgentViewStateUi
import com.falcon.android.model.AskpassPromptUi
import com.falcon.android.model.JsonParsers
import com.falcon.android.model.ProgramBadgeUi
import com.falcon.android.model.SessionUi
import com.falcon.android.net.AgentModeUnsupportedException
import com.falcon.android.net.FalconApiClient
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject

private const val TERM_PREFS = "falcon_android_prefs"
private const val KEY_FONT_SIZE_SP = "term_font_size_sp"
private const val KEY_EXTRA_KEYS_EXPANDED = "term_extra_keys_expanded"

@Composable
fun TerminalScreen(
    baseUrl: String,
    initialSession: SessionUi,
    themeName: String,
    onBackDetach: () -> Unit,
    onSessionTerminated: () -> Unit,
    onToast: (String) -> Unit
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    val theme = LocalFalconTheme.current
    val monoFont = LocalFalconMonoFont.current

    val prefs = remember(context) {
        context.getSharedPreferences(TERM_PREFS, Context.MODE_PRIVATE)
    }
    var extraKeysExpanded by remember {
        mutableStateOf(prefs.getBoolean(KEY_EXTRA_KEYS_EXPANDED, true))
    }

    var session by remember { mutableStateOf(initialSession) }
    var liveTitle by remember { mutableStateOf(initialSession.title) }
    var customName by remember { mutableStateOf(initialSession.name) }
    var sessionState by remember { mutableStateOf(initialSession.state) }
    var deadReason by remember { mutableStateOf(initialSession.deadReason) }
    var programBadge by remember { mutableStateOf<ProgramBadgeUi?>(initialSession.badge) }

    var wsConnected by remember { mutableStateOf(false) }
    var wsRetryAttempt by remember { mutableIntStateOf(0) }
    var sshReconnectAttempt by remember { mutableIntStateOf(0) }
    var reconnectTrigger by remember { mutableIntStateOf(0) }

    var askpassQueue by remember { mutableStateOf<List<AskpassPromptUi>>(emptyList()) }
    var askpassPassword by remember { mutableStateOf("") }

    var ctrlLocked by remember { mutableStateOf(false) }
    var altLocked by remember { mutableStateOf(false) }
    var shiftLocked by remember { mutableStateOf(false) }

    var selectedText by remember { mutableStateOf<String?>(null) }
    var isScrolledUp by remember { mutableStateOf(false) }
    var isSizeOwner by remember { mutableStateOf(true) }
    var gridSize by remember { mutableStateOf(80 to 24) }

    var showMoreMenu by remember { mutableStateOf(false) }
    var showRenameDialog by remember { mutableStateOf(false) }
    var renameInput by remember { mutableStateOf(initialSession.name) }
    var showTerminateDialog by remember { mutableStateOf(false) }
    var busyCommand by remember { mutableStateOf<String?>(null) }
    var uploadingImage by remember { mutableStateOf(false) }

    var agentState by remember { mutableStateOf<AgentViewStateUi?>(null) }
    // 紧凑/图形只改变排版；GUI/TUI 转换走服务端并以 WS 快照为准，不能靠本地标记隐藏 CLI。
    var agentTerminalStyle by remember(initialSession.id) { mutableStateOf(false) }
    var pendingAgentMode by remember(initialSession.id) { mutableStateOf<Boolean?>(null) }
    var agentTransition by remember(initialSession.id) { mutableStateOf(AgentModeTransition()) }
    var agentDraft by remember(initialSession.id) { mutableStateOf("") }
    val transitionLock = remember(initialSession.id) { Any() }
    // WS 回调与 HTTP 完成位于不同线程，原子更新避免快照与响应互相覆盖转换进度。
    fun updateAgentTransition(update: (AgentModeTransition) -> AgentModeTransition) {
        synchronized(transitionLock) { agentTransition = update(agentTransition) }
    }
    val agentModeSwitching = agentTransition.switching
    val agentComposerFocus = remember(initialSession.id) { androidx.compose.ui.focus.FocusRequester() }
    val hasAgentView = agentState?.interactive == true
    val sessionAvailable = wsConnected && agentTransition.synchronized && sessionState == "active" && sshReconnectAttempt == 0
    val canSwitchAgentMode = session.agent == "claude" && sessionAvailable && !agentModeSwitching &&
        (!hasAgentView || agentState?.status == "idle" || agentState?.status == "error")

    // 另一 Viewer 改变模式时撤销旧确认；迟到的 HTTP 响应不覆盖新的 WS 状态。
    LaunchedEffect(hasAgentView) {
        pendingAgentMode = null
        if (hasAgentView) agentTerminalStyle = false
    }
    LaunchedEffect(hasAgentView, agentModeSwitching, sessionAvailable, agentState?.status == "unknown") {
        if (hasAgentView && !agentModeSwitching && sessionAvailable && agentState?.status != "unknown") {
            agentComposerFocus.requestFocus()
        }
    }

    var canvasViewRef by remember { mutableStateOf<TerminalCanvasView?>(null) }
    var activeWebSocket by remember { mutableStateOf<WebSocket?>(null) }
    var nativeHandle by remember { mutableLongStateOf(0L) }

    // 创建与销毁 Rust 侧 TermCore 实例
    DisposableEffect(initialSession.id) {
        val handle = FalconNative.createSession(80, 24, initialSession.durable, themeName)
        nativeHandle = handle
        onDispose {
            activeWebSocket?.close(1000, "detach")
            activeWebSocket = null
            FalconNative.destroySession(handle)
            nativeHandle = 0L
        }
    }

    // 主题切换同步给 TermCore 与服务端
    LaunchedEffect(themeName, nativeHandle) {
        if (nativeHandle != 0L) {
            val appearanceJson = FalconNative.setSessionTheme(nativeHandle, themeName)
            if (appearanceJson.isNotEmpty()) {
                activeWebSocket?.send(appearanceJson)
            }
            canvasViewRef?.invalidate()
        }
    }

    val sendWsMessages: (List<String>) -> Unit = remember {
        { msgs ->
            val ws = activeWebSocket
            if (ws != null) {
                val inputAllowed = wsConnected && agentTransition.synchronized && sessionState == "active" && sshReconnectAttempt == 0 &&
                    !agentTransition.switching && agentState?.interactive != true
                for (m in msgs) {
                    // 转换期间保留尺寸与外观上报，但键盘、粘贴及终端查询回复不能进入控制通道。
                    if (inputAllowed || JSONObject(m).optString("type") != "input") ws.send(m)
                }
            }
        }
    }
    val sendAgentMessage: (JSONObject) -> Boolean = remember {
        { payload ->
            val current = agentState
            val configuring = payload.optString("type") in listOf("agent_set_model", "agent_set_reasoning_effort")
            val allowed = wsConnected && agentTransition.synchronized && sessionState == "active" &&
                sshReconnectAttempt == 0 && !agentTransition.switching && current?.interactive == true &&
                current.status != "unknown" && (!configuring || current.status == "idle" || current.status == "error")
            val sent = allowed && activeWebSocket?.send(payload.toString()) == true
            if (sent) activeWebSocket?.send("""{"type":"status_seen"}""")
            sent
        }
    }

    // 建立并维护 `/ws/sessions/:id` 连接
    LaunchedEffect(initialSession.id, reconnectTrigger, nativeHandle) {
        if (nativeHandle == 0L) return@LaunchedEffect

        wsConnected = false
        updateAgentTransition { it.disconnected() }
        if (agentState?.interactive == true) agentState = agentState?.copy(status = "unknown")
        val requiresModeSnapshot = if (AgentCatalog.supportsGui(initialSession.agent)) {
            FalconApiClient.agentModeSnapshotSupported(baseUrl, initialSession.id, initialSession.agent).getOrElse { error ->
                onToast(context.getString(R.string.agent_mode_sync_failed, error.message.orEmpty()))
                delay(1000)
                reconnectTrigger++
                return@LaunchedEffect
            } || agentState?.interactive == true
        } else {
            agentState?.interactive == true
        }
        var currentWs: WebSocket? = null
        var closedByEffect = false

        val listener = object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                if (closedByEffect) {
                    webSocket.close(1000, "Effect disposed")
                    return
                }
                updateAgentTransition { it.connected(requiresModeSnapshot) }
                wsConnected = true
                wsRetryAttempt = 0
                sshReconnectAttempt = 0
                val (cols, rows) = canvasViewRef?.currentViewportGrid() ?: (80 to 24)
                val handshakeJson = FalconNative.handshakeMessages(nativeHandle, cols, rows, true)
                val msgs = JsonParsers.parseStringArray(handshakeJson)
                for (m in msgs) {
                    webSocket.send(m)
                }
                webSocket.send("""{"type":"status_seen"}""")
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                if (closedByEffect) return
                val h = nativeHandle
                if (h == 0L) return
                val resultJson = FalconNative.onBinaryFrame(h, bytes.toByteArray())
                canvasViewRef?.requestRender()
                if (!resultJson.contains("\"type\"")) return
                try {
                    val obj = JSONObject(resultJson)
                    val effects = obj.optJSONArray("effects")
                    if (effects != null && effects.length() > 0) {
                        for (i in 0 until effects.length()) {
                            val eff = effects.optJSONObject(i) ?: continue
                            when (eff.optString("type")) {
                                "sendWs" -> {
                                    val json = eff.optString("json")
                                    if (json.isNotEmpty()) sendWsMessages(listOf(json))
                                }
                                "clipboard" -> {
                                    val text = eff.optString("text")
                                    val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                                    cm?.setPrimaryClip(ClipData.newPlainText("Falcon Terminal", text))
                                }
                                "bell" -> {
                                    canvasViewRef?.post {
                                        canvasViewRef?.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                                    }
                                }
                            }
                        }
                    }
                } catch (_: Exception) {
                }
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                if (closedByEffect) return
                val h = nativeHandle
                if (h == 0L) return
                val parsedStr = FalconNative.parseServerMessage(text)
                try {
                    val obj = JSONObject(parsedStr)
                    when (obj.optString("kind")) {
                        "state" -> {
                            sessionState = obj.optString("state", "active")
                            deadReason = if (obj.has("deadReason") && !obj.isNull("deadReason")) {
                                obj.optString("deadReason")
                            } else {
                                null
                            }
                            sshReconnectAttempt = 0
                        }
                        "reconnecting" -> {
                            sshReconnectAttempt = obj.optInt("attempt", 1)
                        }
                        "error" -> {
                            val errMsg = obj.optString("message")
                            if (errMsg.isNotBlank()) onToast(errMsg)
                        }
                        "title" -> {
                            liveTitle = if (obj.has("title") && !obj.isNull("title")) {
                                obj.optString("title")
                            } else {
                                null
                            }
                        }
                        "askpass" -> {
                            val id = obj.optString("id")
                            val prompt = obj.optString("prompt", "Password:")
                            if (id.isNotBlank()) {
                                askpassQueue = askpassQueue + AskpassPromptUi(id, prompt)
                            }
                        }
                        "programStatus" -> {
                            programBadge = JsonParsers.parseBadge(obj.optJSONObject("badge"))
                        }
                        "size" -> {
                            val c = obj.optInt("cols", 80)
                            val r = obj.optInt("rows", 24)
                            val owner = obj.optBoolean("owner", true)
                            FalconNative.onServerSize(h, c, r, owner)
                            canvasViewRef?.requestRender()
                        }
                        "agentSnapshot" -> {
                            JsonParsers.parseAgentSnapshot(obj.optJSONObject("state"))?.let { snap ->
                                agentState = snap
                                updateAgentTransition { it.snapshot(
                                    graphical = snap.interactive,
                                    ready = !snap.active || snap.terminalBacked || snap.status != "unknown"
                                ) }
                            }
                        }
                        "agentEntry" -> {
                            JsonParsers.parseAgentEntry(obj.optJSONObject("entry"))?.let { entry ->
                                agentState = agentState?.upsertEntry(entry)
                            }
                        }
                        "agentDelta" -> {
                            val id = obj.optString("id")
                            val dKind = obj.optString("deltaKind")
                            val delta = obj.optString("delta")
                            agentState = agentState?.applyDelta(id, dKind, delta)
                        }
                        "agentMeta" -> {
                            agentState?.let { cur ->
                                val model = if (obj.has("model") && !obj.isNull("model")) obj.optString("model") else cur.model
                                val perm = if (obj.has("permissionMode") && !obj.isNull("permissionMode")) obj.optString("permissionMode") else cur.permissionMode
                                val cwd = if (obj.has("cwd") && !obj.isNull("cwd")) obj.optString("cwd") else cur.cwd
                                val cmds = if (obj.has("commands") && !obj.isNull("commands")) {
                                    JsonParsers.parseSlashCommands(obj.optJSONArray("commands"))
                                } else {
                                    cur.commands
                                }
                                val models = if (obj.has("models") && !obj.isNull("models")) {
                                    JsonParsers.parseStringList(obj.optJSONArray("models"))
                                } else {
                                    cur.models
                                }
                                val options = if (obj.has("modelOptions") && !obj.isNull("modelOptions")) {
                                    JsonParsers.parseModelOptions(obj.optJSONArray("modelOptions"))
                                } else {
                                    cur.modelOptions
                                }
                                val nextActive = obj.optBoolean("active", cur.active)
                                val next = cur.copy(
                                    active = nextActive,
                                    status = obj.optString("status", cur.status),
                                    model = model,
                                    permissionMode = perm,
                                    cwd = cwd,
                                    commands = cmds,
                                    models = models,
                                    modelOptions = options,
                                    reasoningEffort = if (obj.isNull("reasoningEffort")) null else obj.optString("reasoningEffort")
                                )
                                agentState = next
                                if (agentTransition.synchronized) {
                                    updateAgentTransition { it.snapshot(
                                        graphical = next.interactive,
                                        ready = !next.active || next.terminalBacked || next.status != "unknown"
                                    ) }
                                }
                            }
                        }
                    }
                } catch (_: Exception) {
                }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(code, reason)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                if (closedByEffect) return
                wsConnected = false
                updateAgentTransition { it.disconnected() }
                if (!closedByEffect && sessionState != "dead" && code != 4401) {
                    scope.launch {
                        val nextAttempt = wsRetryAttempt + 1
                        wsRetryAttempt = nextAttempt
                        val waitMs = (1000L * (1 shl nextAttempt.coerceAtMost(4))).coerceAtMost(10000L)
                        delay(waitMs)
                        if (!closedByEffect) {
                            reconnectTrigger++
                        }
                    }
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                if (closedByEffect) return
                wsConnected = false
                updateAgentTransition { it.disconnected() }
                if (!closedByEffect && sessionState != "dead") {
                    scope.launch {
                        val nextAttempt = wsRetryAttempt + 1
                        wsRetryAttempt = nextAttempt
                        val waitMs = (1200L * (1 shl nextAttempt.coerceAtMost(4))).coerceAtMost(10000L)
                        delay(waitMs)
                        if (!closedByEffect) {
                            reconnectTrigger++
                        }
                    }
                }
            }
        }

        currentWs = FalconApiClient.openSessionWebSocket(baseUrl, initialSession.id, listener)
        activeWebSocket = currentWs

        try {
            kotlinx.coroutines.awaitCancellation()
        } finally {
            closedByEffect = true
            currentWs.close(1000, "Effect disposed")
        }
    }

    // 成功响应迟于/早于 WS 都不能放开旧模式。缺少目标快照时重连以重新取得权威状态，不超时解锁。
    LaunchedEffect(agentTransition.awaitingSnapshot, wsConnected) {
        if (agentTransition.awaitingSnapshot && wsConnected) {
            delay(5000)
            if (agentTransition.awaitingSnapshot) reconnectTrigger++
        }
    }

    // 前后台切换时同步 `visibility` 与 `status_seen`（ADR 0022 & ADR 0020 决定六）
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_RESUME -> {
                    activeWebSocket?.send("""{"type":"visibility","visible":true}""")
                    activeWebSocket?.send("""{"type":"status_seen"}""")
                }
                Lifecycle.Event.ON_PAUSE -> {
                    activeWebSocket?.send("""{"type":"visibility","visible":false}""")
                }
                else -> {}
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
        }
    }

    // 图片选择器：上传图片到会话宿主机并将返回路径粘进终端输入
    val imagePickerLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.GetContent()
    ) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            uploadingImage = true
            val mimeType = context.contentResolver.getType(uri) ?: "image/png"
            val bytes = runCatching {
                context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            }.getOrNull()
            if (bytes == null || bytes.isEmpty()) {
                uploadingImage = false
                onToast("读取图片失败")
                return@launch
            }
            val res = FalconApiClient.pasteImage(baseUrl, session.id, mimeType, bytes)
            uploadingImage = false
            res.onSuccess { remotePath ->
                canvasViewRef?.sendPaste("$remotePath ")
                onToast("已粘贴图片路径：$remotePath")
            }.onFailure { e ->
                onToast("图片粘贴失败：${e.message}")
            }
        }
    }

    val effectiveTitle = remember(customName, liveTitle, session) {
        when {
            customName.isNotBlank() -> customName
            !liveTitle.isNullOrBlank() -> liveTitle!!
            !session.agent.isNullOrBlank() -> session.agent!!
            else -> session.displayLabel
        }
    }

    val updateAndSaveFontSize: (Float) -> Unit = { newSp ->
        canvasViewRef?.let { view ->
            view.setFontSizeSp(newSp)
            prefs.edit().putFloat(KEY_FONT_SIZE_SP, view.getFontSizeSp()).apply()
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(theme.app)
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
    ) {
        // 顶部单行超紧凑标题栏（34dp + 1.5dp 状态色线）：严格固定高度，不随状态增删终端视图行高
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(theme.sidebar)
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(34.dp)
                    .padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(2.dp)
            ) {
                TerminalHeaderIconButton(
                    icon = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "断开视图并返回",
                    tint = theme.foreground,
                    onClick = onBackDetach
                )

                // 状态圆点 + 标题 + 项目名 + 网格/连接状态内联单行排布
                Row(
                    modifier = Modifier
                        .weight(1f)
                        .padding(horizontal = 2.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    val dotColor = when {
                        sessionState == "dead" -> theme.destructive
                        !wsConnected || sessionState == "unverified" || sshReconnectAttempt > 0 -> theme.warning
                        programBadge?.tone == "blocked" -> theme.warning
                        else -> theme.success
                    }
                    Box(
                        modifier = Modifier
                            .size(6.dp)
                            .clip(CircleShape)
                            .background(dotColor)
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    Text(
                        text = effectiveTitle,
                        color = theme.foreground,
                        fontSize = 12.5.sp,
                        fontWeight = FontWeight.SemiBold,
                        fontFamily = monoFont,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false)
                    )
                    Spacer(modifier = Modifier.width(5.dp))
                    val metaText = buildString {
                        append(session.projectName)
                        when {
                            !wsConnected -> append(" · 重连($wsRetryAttempt)")
                            sshReconnectAttempt > 0 -> append(" · SSH($sshReconnectAttempt)")
                            sessionState == "unverified" -> append(" · 待接回")
                            sessionState == "dead" -> append(" · 已结束")
                            !isSizeOwner -> append(" · 跟随 ${gridSize.first}×${gridSize.second}")
                            else -> append(" · ${gridSize.first}×${gridSize.second}")
                        }
                    }
                    Text(
                        text = metaText,
                        color = theme.mutedForeground,
                        fontSize = 10.5.sp,
                        fontFamily = monoFont,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                }

                // 程序状态胶囊（OSC 7501）
                programBadge?.let { badge ->
                    val badgeColor = when (badge.tone) {
                        "blocked" -> theme.warning
                        "error" -> theme.destructive
                        "done" -> theme.success
                        else -> theme.tintStrong
                    }
                    val badgeLabel = buildString {
                        append(badge.shortLabel)
                        if (badge.progress != null) {
                            append(" ${badge.progress}%")
                        }
                    }
                    Box(
                        modifier = Modifier
                            .height(19.dp)
                            .clip(RoundedCornerShape(5.dp))
                            .background(badgeColor.copy(alpha = 0.16f))
                            .border(0.8.dp, badgeColor.copy(alpha = 0.45f), RoundedCornerShape(5.dp))
                            .padding(horizontal = 6.dp),
                        contentAlignment = Alignment.Center
                    ) {
                        Text(
                            text = badgeLabel,
                            color = badgeColor,
                            fontSize = 10.sp,
                            fontFamily = monoFont,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1
                        )
                    }
                }

                // 若处于历史翻阅状态，提供一键回到底部按钮
                if (isScrolledUp) {
                    TerminalHeaderIconButton(
                        icon = Icons.Default.KeyboardArrowDown,
                        contentDescription = "回到底部",
                        tint = theme.tintStrong,
                        onClick = {
                            if (nativeHandle != 0L) {
                                FalconNative.scrollToBottom(nativeHandle)
                                canvasViewRef?.requestRender()
                            }
                        }
                    )
                }

                // 选区存在时显示复制按钮
                if (!selectedText.isNullOrEmpty()) {
                    TerminalHeaderIconButton(
                        icon = Icons.Default.ContentCopy,
                        contentDescription = "复制选中内容",
                        tint = theme.tintStrong,
                        onClick = {
                            val text = selectedText ?: return@TerminalHeaderIconButton
                            val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                            cm?.setPrimaryClip(ClipData.newPlainText("Falcon Terminal", text))
                            FalconNative.clearSelection(nativeHandle)
                            selectedText = null
                            canvasViewRef?.requestRender()
                            onToast("已复制选中文本")
                        }
                    )
                }

                if (session.agent == "claude") {
                    Box(
                        modifier = Modifier
                            .height(22.dp)
                            .clip(RoundedCornerShape(5.dp))
                            .background(if (hasAgentView) theme.tint else theme.secondary)
                            .clickable(enabled = canSwitchAgentMode) { pendingAgentMode = !hasAgentView }
                            .padding(horizontal = 6.dp),
                        contentAlignment = Alignment.Center
                    ) {
                        Text(
                            text = stringResource(
                                when {
                                    agentModeSwitching -> R.string.agent_mode_switching
                                    hasAgentView -> R.string.agent_mode_to_tui
                                    else -> R.string.agent_mode_to_gui
                                }
                            ),
                            color = if (canSwitchAgentMode) theme.tintStrong else theme.mutedForeground,
                            fontSize = 10.sp,
                            fontFamily = monoFont,
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                }

                Box(
                    modifier = Modifier
                        .size(26.dp)
                        .clip(RoundedCornerShape(5.dp))
                        .clickable(enabled = !uploadingImage && !hasAgentView && sessionAvailable && !agentModeSwitching) {
                            imagePickerLauncher.launch("image/*")
                        },
                    contentAlignment = Alignment.Center
                ) {
                    if (uploadingImage) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(13.dp),
                            strokeWidth = 1.6.dp,
                            color = theme.tintStrong
                        )
                    } else {
                        Icon(
                            imageVector = Icons.Default.Image,
                            contentDescription = "粘贴图片到终端",
                            tint = theme.mutedForeground,
                            modifier = Modifier.size(15.dp)
                        )
                    }
                }

                // 断线或 unverified 时提供立即重连/接回按钮
                if (!wsConnected || sessionState == "unverified") {
                    TerminalHeaderIconButton(
                        icon = Icons.Default.Refresh,
                        contentDescription = "立即接回",
                        tint = theme.warning,
                        onClick = {
                            scope.launch {
                                if (sessionState == "unverified") {
                                    FalconApiClient.reattachSession(baseUrl, session.id)
                                }
                                reconnectTrigger++
                            }
                        }
                    )
                }

                Box {
                    TerminalHeaderIconButton(
                        icon = Icons.Default.MoreHoriz,
                        contentDescription = "更多操作",
                        tint = theme.mutedForeground,
                        onClick = { showMoreMenu = true }
                    )
                    DropdownMenu(
                        expanded = showMoreMenu,
                        onDismissRequest = { showMoreMenu = false },
                        modifier = Modifier.background(theme.card)
                    ) {
                        DropdownMenuItem(
                            text = { Text("粘贴剪贴板文本", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                                val text = cm?.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString()
                                if (!text.isNullOrEmpty()) {
                                    canvasViewRef?.sendPaste(text)
                                } else {
                                    onToast("剪贴板为空")
                                }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("放大字号 (+)", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                canvasViewRef?.let { updateAndSaveFontSize(it.getFontSizeSp() + 1.0f) }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("缩小字号 (−)", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                canvasViewRef?.let { updateAndSaveFontSize(it.getFontSizeSp() - 1.0f) }
                            }
                        )
                        DropdownMenuItem(
                            text = {
                                Text(
                                    if (extraKeysExpanded) "切换为单排按键栏" else "展开双排按键栏",
                                    color = theme.foreground,
                                    fontSize = 13.sp
                                )
                            },
                            onClick = {
                                showMoreMenu = false
                                extraKeysExpanded = !extraKeysExpanded
                                prefs.edit().putBoolean(KEY_EXTRA_KEYS_EXPANDED, extraKeysExpanded).apply()
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("重命名会话…", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                renameInput = customName
                                showRenameDialog = true
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("重新连接 / 接回", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                scope.launch {
                                    FalconApiClient.reattachSession(baseUrl, session.id)
                                    reconnectTrigger++
                                }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("断开视图（后台保持运行）", color = theme.foreground, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                onBackDetach()
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("终止会话（Terminate）…", color = theme.destructive, fontSize = 13.sp) },
                            onClick = {
                                showMoreMenu = false
                                scope.launch {
                                    val (busy, cmd) = FalconApiClient.checkForegroundBusy(baseUrl, session.id)
                                    busyCommand = if (busy) cmd else null
                                    showTerminateDialog = true
                                }
                            }
                        )
                    }
                }
            }

            // 标题栏底边状态色线（与桌面端 terminal/title_status.rs 同口径，固定 1.5dp）
            val bottomLineColor = when (programBadge?.tone) {
                "blocked" -> theme.warning
                "error" -> theme.destructive
                "done" -> theme.success
                else -> theme.muted
            }
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(1.5.dp)
                    .background(bottomLineColor)
            )
        }

        if (hasAgentView) {
            val currentAgentState = agentState!!
            AgentSessionView(
                state = currentAgentState,
                terminalStyle = agentTerminalStyle,
                available = sessionAvailable && !agentModeSwitching,
                composerFocus = agentComposerFocus,
                theme = theme,
                monoFont = monoFont,
                inputText = agentDraft,
                onInputChange = { agentDraft = it },
                onSendPrompt = { prompt ->
                    val payload = JSONObject().apply {
                        put("type", "agent_prompt")
                        put("text", prompt)
                    }
                    sendAgentMessage(payload)
                },
                onInterrupt = {
                    sendAgentMessage(JSONObject().put("type", "agent_interrupt"))
                },
                onApprovePermission = { callId, decision ->
                    val payload = JSONObject().apply {
                        put("type", "agent_permission")
                        put("callId", callId)
                        put("decision", decision)
                    }
                    sendAgentMessage(payload)
                },
                onSelectModel = { model ->
                    val payload = JSONObject().apply {
                        put("type", "agent_set_model")
                        put("model", model)
                    }
                    sendAgentMessage(payload)
                },
                onSelectReasoningEffort = { effort ->
                    val payload = JSONObject().apply {
                        put("type", "agent_set_reasoning_effort")
                        put("effort", effort ?: JSONObject.NULL)
                    }
                    sendAgentMessage(payload)
                },
                onToggleStyle = {
                    agentTerminalStyle = !agentTerminalStyle
                    agentComposerFocus.requestFocus()
                },
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
            )
        } else {
            // 中部：终端网格 Canvas 视图
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .background(theme.terminalBackground)
            ) {
                AndroidView(
                    modifier = Modifier.fillMaxSize(),
                    factory = { ctx ->
                        TerminalCanvasView(ctx).apply {
                            val savedFontSp = prefs.getFloat(KEY_FONT_SIZE_SP, 12.0f)
                            this.setFontSizeSp(savedFontSp)
                            this.sessionHandle = nativeHandle
                            this.outsideAreaColor = theme.app.toArgb()
                            this.onSendWsMessages = sendWsMessages
                            this.onViewportGridChanged = { c, r ->
                                gridSize = c to r
                            }
                            this.onFontSizeChanged = { sp ->
                                prefs.edit().putFloat(KEY_FONT_SIZE_SP, sp).apply()
                            }
                            this.onSelectionChanged = { sel ->
                                selectedText = sel
                            }
                            this.onRenderStateUpdated = { scrolledUp, owner, gCols, gRows ->
                                post {
                                    isScrolledUp = scrolledUp
                                    isSizeOwner = owner
                                    gridSize = gCols to gRows
                                }
                            }
                            this.consumeStickyModifiers = {
                                val current = Triple(ctrlLocked, altLocked, shiftLocked)
                                if (ctrlLocked || altLocked || shiftLocked) {
                                    ctrlLocked = false
                                    altLocked = false
                                    shiftLocked = false
                                }
                                current
                            }
                            canvasViewRef = this
                            post { requestFocus() }
                        }
                    },
                    update = { view ->
                        view.sessionHandle = nativeHandle
                        view.outsideAreaColor = theme.app.toArgb()
                        view.onSendWsMessages = sendWsMessages
                    },
                    onRelease = { view ->
                        view.onSendWsMessages = null
                        if (canvasViewRef === view) canvasViewRef = null
                    }
                )
            }

            // 底部：高密度移动端扩展按键栏（支持单/双排折叠，右侧固定软键盘与折叠开关）
            MobileTerminalKeyBar(
                theme = theme,
                monoFont = monoFont,
                expanded = extraKeysExpanded,
                ctrlLocked = ctrlLocked,
                altLocked = altLocked,
                shiftLocked = shiftLocked,
                onToggleExpanded = {
                    extraKeysExpanded = !extraKeysExpanded
                    prefs.edit().putBoolean(KEY_EXTRA_KEYS_EXPANDED, extraKeysExpanded).apply()
                },
                onToggleCtrl = { ctrlLocked = !ctrlLocked },
                onToggleAlt = { altLocked = !altLocked },
                onToggleShift = { shiftLocked = !shiftLocked },
                onSendKey = { key, forceCtrl, forceAlt, forceShift ->
                    val c = forceCtrl || ctrlLocked
                    val a = forceAlt || altLocked
                    val s = forceShift || shiftLocked
                    ctrlLocked = false
                    altLocked = false
                    shiftLocked = false
                    canvasViewRef?.sendKey(key, ctrl = c, alt = a, shift = s)
                },
                onSendText = { txt ->
                    if (ctrlLocked || altLocked) {
                        val c = ctrlLocked
                        val a = altLocked
                        val s = shiftLocked
                        ctrlLocked = false
                        altLocked = false
                        shiftLocked = false
                        canvasViewRef?.sendKey(txt, ctrl = c, alt = a, shift = s)
                    } else {
                        val h = nativeHandle
                        if (h != 0L) {
                            val chunks = FalconNative.encodeText(h, txt)
                            sendWsMessages(JsonParsers.parseStringArray(chunks))
                            canvasViewRef?.requestRender()
                        }
                    }
                },
                onPasteClipboard = {
                    val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                    val text = cm?.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString()
                    if (!text.isNullOrEmpty()) {
                        canvasViewRef?.sendPaste(text)
                    } else {
                        onToast("剪贴板为空")
                    }
                },
                onToggleIme = {
                    canvasViewRef?.toggleSoftKeyboard()
                }
            )
        }
    }

    val targetAgentMode = pendingAgentMode
    if (targetAgentMode != null && askpassQueue.isEmpty()) {
        AlertDialog(
            onDismissRequest = { pendingAgentMode = null },
            containerColor = theme.card,
            title = {
                Text(
                    stringResource(if (targetAgentMode) R.string.agent_mode_gui_title else R.string.agent_mode_tui_title),
                    color = theme.foreground,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold
                )
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text(
                        stringResource(if (targetAgentMode) R.string.agent_mode_gui_body else R.string.agent_mode_tui_body),
                        color = theme.foreground,
                        fontSize = 13.sp
                    )
                    Text(stringResource(R.string.agent_mode_conditions), color = theme.mutedForeground, fontSize = 12.sp)
                }
            },
            confirmButton = {
                Button(
                    enabled = canSwitchAgentMode,
                    onClick = {
                        pendingAgentMode = null
                        if (!canSwitchAgentMode || agentTransition.switching || targetAgentMode == hasAgentView) return@Button
                        updateAgentTransition { it.start(targetAgentMode) }
                        scope.launch {
                            var succeeded = false
                            try {
                                val result = FalconApiClient.switchAgentMode(baseUrl, session.id, targetAgentMode)
                                succeeded = result.isSuccess
                                result.onFailure { error ->
                                        val message = if (error is AgentModeUnsupportedException) {
                                            context.getString(R.string.agent_mode_unsupported)
                                        } else {
                                            error.message.orEmpty()
                                        }
                                        onToast(context.getString(R.string.agent_mode_failed, message))
                                        reconnectTrigger++
                                    }
                            } finally {
                                updateAgentTransition { it.finish(succeeded) }
                            }
                        }
                    }
                ) {
                    Text(stringResource(R.string.agent_mode_confirm), fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingAgentMode = null }) {
                    Text(stringResource(R.string.agent_mode_cancel), color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }

    // Askpass 弹窗（sudo / SSH 密码请求）
    val currentAskpass = askpassQueue.firstOrNull()
    if (currentAskpass != null) {
        AlertDialog(
            onDismissRequest = {},
            containerColor = theme.card,
            title = {
                Text("需要密码", color = theme.foreground, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            },
            text = {
                Column {
                    Text(
                        text = currentAskpass.prompt,
                        color = theme.mutedForeground,
                        fontSize = 12.sp,
                        fontFamily = monoFont
                    )
                    Spacer(modifier = Modifier.height(8.dp))
                    OutlinedTextField(
                        value = askpassPassword,
                        onValueChange = { askpassPassword = it },
                        label = { Text("密码", fontSize = 12.sp) },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        modifier = Modifier.fillMaxWidth()
                    )
                }
            },
            confirmButton = {
                Button(
                    onClick = {
                        val pwd = askpassPassword
                        val id = currentAskpass.id
                        askpassPassword = ""
                        askpassQueue = askpassQueue.drop(1)
                        scope.launch {
                            FalconApiClient.answerAskpass(baseUrl, id, pwd, cancel = false)
                        }
                    }
                ) {
                    Text("确定", fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        val id = currentAskpass.id
                        askpassPassword = ""
                        askpassQueue = askpassQueue.drop(1)
                        scope.launch {
                            FalconApiClient.answerAskpass(baseUrl, id, null, cancel = true)
                        }
                    }
                ) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }

    // 重命名会话对话框
    if (showRenameDialog) {
        AlertDialog(
            onDismissRequest = { showRenameDialog = false },
            containerColor = theme.card,
            title = { Text("重命名会话", color = theme.foreground, fontSize = 15.sp, fontWeight = FontWeight.SemiBold) },
            text = {
                OutlinedTextField(
                    value = renameInput,
                    onValueChange = { renameInput = it },
                    placeholder = { Text("留空则使用自动标题", fontSize = 12.sp) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth()
                )
            },
            confirmButton = {
                Button(
                    onClick = {
                        val targetName = renameInput.trim()
                        showRenameDialog = false
                        scope.launch {
                            FalconApiClient.renameSession(baseUrl, session.id, targetName)
                                .onSuccess {
                                    customName = targetName
                                }
                                .onFailure { e ->
                                    onToast("重命名失败：${e.message}")
                                }
                        }
                    }
                ) {
                    Text("保存", fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { showRenameDialog = false }) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }

    // 终止会话确认框（含前台运行中命令检测提示）
    if (showTerminateDialog) {
        AlertDialog(
            onDismissRequest = { showTerminateDialog = false },
            containerColor = theme.card,
            title = {
                Text(
                    text = if (busyCommand != null) {
                        "「$effectiveTitle」正在运行程序"
                    } else {
                        "终止会话「$effectiveTitle」？"
                    },
                    color = theme.foreground,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold
                )
            },
            text = {
                Text(
                    text = if (busyCommand != null) {
                        "正在运行「$busyCommand」。终止会话会结束底层 shell 并杀掉正在运行的命令，不可撤销。\n\n若希望会话留在后台继续运行，请点取消并直接返回。"
                    } else {
                        "该会话底层 shell 及其中正在运行的命令会被结束，此操作不可撤销。"
                    },
                    color = theme.mutedForeground,
                    fontSize = 12.5.sp
                )
            },
            confirmButton = {
                Button(
                    colors = ButtonDefaults.buttonColors(containerColor = theme.destructive),
                    onClick = {
                        showTerminateDialog = false
                        scope.launch {
                            FalconApiClient.terminateSession(baseUrl, session.id)
                                .onSuccess {
                                    onSessionTerminated()
                                }
                                .onFailure { e ->
                                    onToast("终止会话失败：${e.message}")
                                }
                        }
                    }
                ) {
                    Text("终止会话", color = Color.White, fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { showTerminateDialog = false }) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }
}

@Composable
private fun TerminalHeaderIconButton(
    icon: ImageVector,
    contentDescription: String,
    tint: Color,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .size(26.dp)
            .clip(RoundedCornerShape(5.dp))
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
private fun MobileTerminalKeyBar(
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    expanded: Boolean,
    ctrlLocked: Boolean,
    altLocked: Boolean,
    shiftLocked: Boolean,
    onToggleExpanded: () -> Unit,
    onToggleCtrl: () -> Unit,
    onToggleAlt: () -> Unit,
    onToggleShift: () -> Unit,
    onSendKey: (key: String, ctrl: Boolean, alt: Boolean, shift: Boolean) -> Unit,
    onSendText: (String) -> Unit,
    onPasteClipboard: () -> Unit,
    onToggleIme: () -> Unit
) {
    val view = LocalView.current
    val hapticTap = {
        view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(theme.sidebar)
    ) {
        // 顶部分隔细线
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(0.8.dp)
                .background(theme.muted)
        )

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 4.dp, vertical = 2.5.dp),
            verticalArrangement = Arrangement.spacedBy(2.5.dp)
        ) {
            // 第一排：左侧高频修饰与方向键滚动区 + 右侧常驻固定软键盘与单/双排切换键
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(3.dp)
            ) {
                Row(
                    modifier = Modifier
                        .weight(1f)
                        .horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(3.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    KeyBarButton("ESC", theme, monoFont) {
                        hapticTap()
                        onSendKey("escape", false, false, false)
                    }
                    KeyBarButton("TAB", theme, monoFont) {
                        hapticTap()
                        onSendKey("tab", false, false, false)
                    }
                    KeyBarButton("CTRL", theme, monoFont, active = ctrlLocked) {
                        hapticTap()
                        onToggleCtrl()
                    }
                    KeyBarButton("ALT", theme, monoFont, active = altLocked) {
                        hapticTap()
                        onToggleAlt()
                    }
                    KeyBarButton("SHIFT", theme, monoFont, active = shiftLocked) {
                        hapticTap()
                        onToggleShift()
                    }
                    KeyBarButton("←", theme, monoFont) {
                        hapticTap()
                        onSendKey("left", false, false, false)
                    }
                    KeyBarButton("↓", theme, monoFont) {
                        hapticTap()
                        onSendKey("down", false, false, false)
                    }
                    KeyBarButton("↑", theme, monoFont) {
                        hapticTap()
                        onSendKey("up", false, false, false)
                    }
                    KeyBarButton("→", theme, monoFont) {
                        hapticTap()
                        onSendKey("right", false, false, false)
                    }
                    KeyBarButton("⏎", theme, monoFont) {
                        hapticTap()
                        onSendKey("enter", false, false, false)
                    }
                    if (!expanded) {
                        KeyBarButton("^C", theme, monoFont) {
                            hapticTap()
                            onSendKey("c", true, false, false)
                        }
                        KeyBarButton("/", theme, monoFont) {
                            hapticTap()
                            onSendText("/")
                        }
                        KeyBarButton("-", theme, monoFont) {
                            hapticTap()
                            onSendText("-")
                        }
                    }
                }

                // 右侧固定区：折叠/展开第二排 + 调出/收起软键盘（永远不需横向滚动即可触达）
                KeyBarIconButton(
                    icon = if (expanded) Icons.Default.KeyboardArrowDown else Icons.Default.KeyboardArrowUp,
                    contentDesc = if (expanded) "收起扩展行" else "展开扩展行",
                    theme = theme,
                    onClick = {
                        hapticTap()
                        onToggleExpanded()
                    }
                )
                KeyBarIconButton(
                    icon = Icons.Default.Keyboard,
                    contentDesc = "切换软键盘",
                    theme = theme,
                    onClick = {
                        hapticTap()
                        onToggleIme()
                    }
                )
            }

            // 第二排：终端高频信号键、跳转键与符号（可一键折叠）
            if (expanded) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(3.dp)
                ) {
                    Row(
                        modifier = Modifier
                            .weight(1f)
                            .horizontalScroll(rememberScrollState()),
                        horizontalArrangement = Arrangement.spacedBy(3.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        KeyBarButton("^C", theme, monoFont) {
                            hapticTap()
                            onSendKey("c", true, false, false)
                        }
                        KeyBarButton("^D", theme, monoFont) {
                            hapticTap()
                            onSendKey("d", true, false, false)
                        }
                        KeyBarButton("^Z", theme, monoFont) {
                            hapticTap()
                            onSendKey("z", true, false, false)
                        }
                        KeyBarButton("^L", theme, monoFont) {
                            hapticTap()
                            onSendKey("l", true, false, false)
                        }
                        KeyBarButton("^R", theme, monoFont) {
                            hapticTap()
                            onSendKey("r", true, false, false)
                        }
                        KeyBarButton("/", theme, monoFont) {
                            hapticTap()
                            onSendText("/")
                        }
                        KeyBarButton("-", theme, monoFont) {
                            hapticTap()
                            onSendText("-")
                        }
                        KeyBarButton("|", theme, monoFont) {
                            hapticTap()
                            onSendText("|")
                        }
                        KeyBarButton("~", theme, monoFont) {
                            hapticTap()
                            onSendText("~")
                        }
                        KeyBarButton("_", theme, monoFont) {
                            hapticTap()
                            onSendText("_")
                        }
                        KeyBarButton("HOME", theme, monoFont) {
                            hapticTap()
                            onSendKey("home", false, false, false)
                        }
                        KeyBarButton("END", theme, monoFont) {
                            hapticTap()
                            onSendKey("end", false, false, false)
                        }
                        KeyBarButton("PGUP", theme, monoFont) {
                            hapticTap()
                            onSendKey("pageup", false, false, false)
                        }
                        KeyBarButton("PGDN", theme, monoFont) {
                            hapticTap()
                            onSendKey("pagedown", false, false, false)
                        }
                    }

                    KeyBarIconButton(
                        icon = Icons.Default.ContentPaste,
                        contentDesc = "粘贴",
                        theme = theme,
                        onClick = {
                            hapticTap()
                            onPasteClipboard()
                        }
                    )
                }
            }
        }
    }
}

@Composable
private fun KeyBarButton(
    label: String,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    active: Boolean = false,
    onClick: () -> Unit
) {
    val bg = if (active) theme.tintStrong else theme.card
    val fg = if (active) theme.primaryForeground else theme.foreground
    Box(
        modifier = Modifier
            .height(24.dp)
            .clip(RoundedCornerShape(4.dp))
            .background(bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 7.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = label,
            color = fg,
            fontSize = 11.sp,
            fontFamily = monoFont,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1
        )
    }
}

@Composable
private fun KeyBarIconButton(
    icon: ImageVector,
    contentDesc: String,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .height(24.dp)
            .clip(RoundedCornerShape(4.dp))
            .background(theme.card)
            .clickable(onClick = onClick)
            .padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center
    ) {
        Icon(
            imageVector = icon,
            contentDescription = contentDesc,
            tint = theme.mutedForeground,
            modifier = Modifier.size(13.5.dp)
        )
    }
}
