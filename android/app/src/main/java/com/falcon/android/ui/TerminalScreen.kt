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
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import com.falcon.android.bridge.FalconNative
import com.falcon.android.model.AskpassPromptUi
import com.falcon.android.model.JsonParsers
import com.falcon.android.model.ProgramBadgeUi
import com.falcon.android.model.SessionUi
import com.falcon.android.net.FalconApiClient
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import org.json.JSONObject

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
    var displayOffset by remember { mutableIntStateOf(0) }
    var isSizeOwner by remember { mutableStateOf(true) }
    var gridSize by remember { mutableStateOf(80 to 24) }

    var showMoreMenu by remember { mutableStateOf(false) }
    var showRenameDialog by remember { mutableStateOf(false) }
    var renameInput by remember { mutableStateOf(initialSession.name) }
    var showTerminateDialog by remember { mutableStateOf(false) }
    var busyCommand by remember { mutableStateOf<String?>(null) }
    var uploadingImage by remember { mutableStateOf(false) }

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
                for (m in msgs) {
                    ws.send(m)
                }
            }
        }
    }

    // 建立并维护 `/ws/sessions/:id` 连接
    LaunchedEffect(initialSession.id, reconnectTrigger, nativeHandle) {
        if (nativeHandle == 0L) return@LaunchedEffect

        var currentWs: WebSocket? = null
        var closedByEffect = false

        val listener = object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
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
                val h = nativeHandle
                if (h == 0L) return
                val resultJson = FalconNative.onBinaryFrame(h, bytes.toByteArray())
                canvasViewRef?.requestRender()
                try {
                    val obj = JSONObject(resultJson)
                    val effects = obj.optJSONArray("effects")
                    if (effects != null && effects.length() > 0) {
                        for (i in 0 until effects.length()) {
                            val eff = effects.optJSONObject(i) ?: continue
                            when (eff.optString("type")) {
                                "sendWs" -> {
                                    val json = eff.optString("json")
                                    if (json.isNotEmpty()) webSocket.send(json)
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
                    }
                } catch (_: Exception) {
                }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(code, reason)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                wsConnected = false
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
                wsConnected = false
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

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(theme.app)
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
    ) {
        // 顶部标题栏：严格不随状态增删终端视图行高（程序状态胶囊与连接状态内嵌在固定高度标题栏内）
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(theme.sidebar)
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(48.dp)
                    .padding(horizontal = 4.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                IconButton(onClick = onBackDetach) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = "断开视图并返回",
                        tint = theme.foreground
                    )
                }

                Column(
                    modifier = Modifier
                        .weight(1f)
                        .padding(end = 6.dp)
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        // 连接与会话状态指示点
                        val dotColor = when {
                            sessionState == "dead" -> theme.destructive
                            !wsConnected || sessionState == "unverified" || sshReconnectAttempt > 0 -> theme.warning
                            else -> theme.success
                        }
                        Box(
                            modifier = Modifier
                                .size(7.dp)
                                .clip(CircleShape)
                                .background(dotColor)
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = effectiveTitle,
                            color = theme.foreground,
                            fontSize = 14.sp,
                            fontWeight = FontWeight.SemiBold,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )
                    }
                    val subText = buildString {
                        append(session.projectName)
                        if (!isSizeOwner) {
                            append(" · 跟随尺寸 ${gridSize.first}×${gridSize.second}")
                        } else if (!wsConnected) {
                            append(" · 重连中($wsRetryAttempt)")
                        } else if (sshReconnectAttempt > 0) {
                            append(" · SSH 重连中($sshReconnectAttempt)")
                        } else if (sessionState == "unverified") {
                            append(" · 待接回")
                        } else if (sessionState == "dead") {
                            append(" · 已结束")
                        } else {
                            append(" · ${gridSize.first}×${gridSize.second}")
                        }
                    }
                    Text(
                        text = subText,
                        color = theme.mutedForeground,
                        fontSize = 11.sp,
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
                    Box(
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .clip(RoundedCornerShape(10.dp))
                            .background(badgeColor.copy(alpha = 0.18f))
                            .border(1.dp, badgeColor.copy(alpha = 0.45f), RoundedCornerShape(10.dp))
                            .padding(horizontal = 8.dp, vertical = 3.dp)
                    ) {
                        Text(
                            text = badge.shortLabel,
                            color = badgeColor,
                            fontSize = 11.sp,
                            fontWeight = FontWeight.Medium
                        )
                    }
                }

                // 若处于历史翻阅状态，提供一键回到底部按钮
                if (displayOffset > 0) {
                    IconButton(
                        onClick = {
                            if (nativeHandle != 0L) {
                                FalconNative.scrollToBottom(nativeHandle)
                                canvasViewRef?.requestRender()
                            }
                        }
                    ) {
                        Icon(
                            imageVector = Icons.Default.KeyboardArrowDown,
                            contentDescription = "回到底部",
                            tint = theme.tintStrong
                        )
                    }
                }

                // 选区存在时显示复制按钮
                if (!selectedText.isNullOrEmpty()) {
                    IconButton(
                        onClick = {
                            val text = selectedText ?: return@IconButton
                            val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                            cm?.setPrimaryClip(ClipData.newPlainText("Falcon Terminal", text))
                            FalconNative.clearSelection(nativeHandle)
                            selectedText = null
                            canvasViewRef?.requestRender()
                            onToast("已复制选中文本")
                        }
                    ) {
                        Icon(
                            imageVector = Icons.Default.ContentCopy,
                            contentDescription = "复制选中内容",
                            tint = theme.tintStrong
                        )
                    }
                }

                // 粘贴图片按钮
                IconButton(
                    enabled = !uploadingImage,
                    onClick = { imagePickerLauncher.launch("image/*") }
                ) {
                    if (uploadingImage) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(16.dp),
                            strokeWidth = 2.dp,
                            color = theme.tintStrong
                        )
                    } else {
                        Icon(
                            imageVector = Icons.Default.Image,
                            contentDescription = "粘贴图片到终端",
                            tint = theme.foreground
                        )
                    }
                }

                // 断线或 unverified 时提供立即重连/接回按钮
                if (!wsConnected || sessionState == "unverified") {
                    IconButton(
                        onClick = {
                            scope.launch {
                                if (sessionState == "unverified") {
                                    FalconApiClient.reattachSession(baseUrl, session.id)
                                }
                                reconnectTrigger++
                            }
                        }
                    ) {
                        Icon(
                            imageVector = Icons.Default.Refresh,
                            contentDescription = "立即接回",
                            tint = theme.warning
                        )
                    }
                }

                Box {
                    IconButton(onClick = { showMoreMenu = true }) {
                        Icon(
                            imageVector = Icons.Default.MoreVert,
                            contentDescription = "更多操作",
                            tint = theme.foreground
                        )
                    }
                    DropdownMenu(
                        expanded = showMoreMenu,
                        onDismissRequest = { showMoreMenu = false },
                        modifier = Modifier.background(theme.card)
                    ) {
                        DropdownMenuItem(
                            text = { Text("粘贴剪贴板文本", color = theme.foreground) },
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
                            text = { Text("放大字号 (+)", color = theme.foreground) },
                            onClick = {
                                showMoreMenu = false
                                canvasViewRef?.let { it.setFontSizeSp(it.getFontSizeSp() + 1.5f) }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("缩小字号 (−)", color = theme.foreground) },
                            onClick = {
                                showMoreMenu = false
                                canvasViewRef?.let { it.setFontSizeSp(it.getFontSizeSp() - 1.5f) }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("重命名会话…", color = theme.foreground) },
                            onClick = {
                                showMoreMenu = false
                                renameInput = customName
                                showRenameDialog = true
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("重新连接 / 接回", color = theme.foreground) },
                            onClick = {
                                showMoreMenu = false
                                scope.launch {
                                    FalconApiClient.reattachSession(baseUrl, session.id)
                                    reconnectTrigger++
                                }
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("断开视图（后台保持运行）", color = theme.foreground) },
                            onClick = {
                                showMoreMenu = false
                                onBackDetach()
                            }
                        )
                        DropdownMenuItem(
                            text = { Text("终止会话（Terminate）…", color = theme.destructive) },
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

            // 标题栏底边状态色线（与桌面端 terminal/title_status.rs 同口径，占固定 2dp）
            val bottomLineColor = when (programBadge?.tone) {
                "blocked" -> theme.warning
                "error" -> theme.destructive
                "working" -> theme.tintStrong
                "done" -> theme.success
                else -> theme.muted
            }
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(2.dp)
                    .background(bottomLineColor)
            )
        }

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
                        this.sessionHandle = nativeHandle
                        this.outsideAreaColor = theme.app.toArgb()
                        this.onSendWsMessages = sendWsMessages
                        this.onViewportGridChanged = { c, r ->
                            gridSize = c to r
                        }
                        this.onSelectionChanged = { sel ->
                            selectedText = sel
                        }
                        this.onRenderStateUpdated = { offset, _, owner, gCols, gRows ->
                            displayOffset = offset
                            isSizeOwner = owner
                            gridSize = gCols to gRows
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
                }
            )
        }

        // 底部：移动端双排扩展按键栏
        MobileTerminalKeyBar(
            theme = theme,
            ctrlLocked = ctrlLocked,
            altLocked = altLocked,
            shiftLocked = shiftLocked,
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

    // Askpass 弹窗（sudo / SSH 密码请求）
    val currentAskpass = askpassQueue.firstOrNull()
    if (currentAskpass != null) {
        AlertDialog(
            onDismissRequest = {},
            containerColor = theme.card,
            title = {
                Text("需要密码", color = theme.foreground, fontWeight = FontWeight.SemiBold)
            },
            text = {
                Column {
                    Text(
                        text = currentAskpass.prompt,
                        color = theme.mutedForeground,
                        fontSize = 13.sp,
                        fontFamily = FontFamily.Monospace
                    )
                    Spacer(modifier = Modifier.height(10.dp))
                    OutlinedTextField(
                        value = askpassPassword,
                        onValueChange = { askpassPassword = it },
                        label = { Text("密码") },
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
                    Text("确定")
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
                    Text("取消", color = theme.mutedForeground)
                }
            }
        )
    }

    // 重命名会话对话框
    if (showRenameDialog) {
        AlertDialog(
            onDismissRequest = { showRenameDialog = false },
            containerColor = theme.card,
            title = { Text("重命名会话", color = theme.foreground) },
            text = {
                OutlinedTextField(
                    value = renameInput,
                    onValueChange = { renameInput = it },
                    placeholder = { Text("留空则使用自动标题") },
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
                    Text("保存")
                }
            },
            dismissButton = {
                TextButton(onClick = { showRenameDialog = false }) {
                    Text("取消", color = theme.mutedForeground)
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
                        "「$effectiveTitle」里有程序正在运行"
                    } else {
                        "终止会话「$effectiveTitle」？"
                    },
                    color = theme.foreground
                )
            },
            text = {
                Text(
                    text = if (busyCommand != null) {
                        "正在运行「$busyCommand」。终止会话会结束底层 shell 并杀掉正在运行的命令，此操作不可撤销。\n\n若希望会话留在后台继续运行，请点取消并直接返回。"
                    } else {
                        "该会话底层 shell 及其中正在运行的命令会被结束，此操作不可撤销。"
                    },
                    color = theme.mutedForeground,
                    fontSize = 13.sp
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
                    Text("终止会话", color = Color.White)
                }
            },
            dismissButton = {
                TextButton(onClick = { showTerminateDialog = false }) {
                    Text("取消", color = theme.mutedForeground)
                }
            }
        )
    }
}

@Composable
private fun MobileTerminalKeyBar(
    theme: FalconThemeTokens,
    ctrlLocked: Boolean,
    altLocked: Boolean,
    shiftLocked: Boolean,
    onToggleCtrl: () -> Unit,
    onToggleAlt: () -> Unit,
    onToggleShift: () -> Unit,
    onSendKey: (key: String, ctrl: Boolean, alt: Boolean, shift: Boolean) -> Unit,
    onSendText: (String) -> Unit,
    onPasteClipboard: () -> Unit,
    onToggleIme: () -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(theme.sidebar)
            .padding(vertical = 4.dp)
    ) {
        // 第一排：核心控制键与方向键
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            KeyBarButton("ESC", theme) { onSendKey("escape", false, false, false) }
            KeyBarButton("TAB", theme) { onSendKey("tab", false, false, false) }
            KeyBarButton("CTRL", theme, active = ctrlLocked, onClick = onToggleCtrl)
            KeyBarButton("ALT", theme, active = altLocked, onClick = onToggleAlt)
            KeyBarButton("SHIFT", theme, active = shiftLocked, onClick = onToggleShift)
            KeyBarButton("↑", theme) { onSendKey("up", false, false, false) }
            KeyBarButton("↓", theme) { onSendKey("down", false, false, false) }
            KeyBarButton("←", theme) { onSendKey("left", false, false, false) }
            KeyBarButton("→", theme) { onSendKey("right", false, false, false) }
            KeyBarButton("ENTER", theme) { onSendKey("enter", false, false, false) }
            KeyBarIconButton(
                icon = Icons.Default.Keyboard,
                contentDesc = "切换软键盘",
                theme = theme,
                onClick = onToggleIme
            )
        }

        Spacer(modifier = Modifier.height(4.dp))

        // 第二排：终端高频信号键、跳转键与符号
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            KeyBarButton("^C", theme) { onSendKey("c", true, false, false) }
            KeyBarButton("^D", theme) { onSendKey("d", true, false, false) }
            KeyBarButton("^Z", theme) { onSendKey("z", true, false, false) }
            KeyBarButton("^L", theme) { onSendKey("l", true, false, false) }
            KeyBarButton("^R", theme) { onSendKey("r", true, false, false) }
            KeyBarButton("HOME", theme) { onSendKey("home", false, false, false) }
            KeyBarButton("END", theme) { onSendKey("end", false, false, false) }
            KeyBarButton("PGUP", theme) { onSendKey("pageup", false, false, false) }
            KeyBarButton("PGDN", theme) { onSendKey("pagedown", false, false, false) }
            KeyBarButton("/", theme) { onSendText("/") }
            KeyBarButton("-", theme) { onSendText("-") }
            KeyBarButton("|", theme) { onSendText("|") }
            KeyBarButton("~", theme) { onSendText("~") }
            KeyBarButton("_", theme) { onSendText("_") }
            KeyBarIconButton(
                icon = Icons.Default.ContentPaste,
                contentDesc = "粘贴",
                theme = theme,
                onClick = onPasteClipboard
            )
        }
    }
}

@Composable
private fun KeyBarButton(
    label: String,
    theme: FalconThemeTokens,
    active: Boolean = false,
    onClick: () -> Unit
) {
    val bg = if (active) theme.tintStrong else theme.card
    val fg = if (active) theme.primaryForeground else theme.foreground
    Box(
        modifier = Modifier
            .height(32.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = label,
            color = fg,
            fontSize = 12.sp,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium
        )
    }
}

@Composable
private fun KeyBarIconButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    contentDesc: String,
    theme: FalconThemeTokens,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .height(32.dp)
            .clip(RoundedCornerShape(6.dp))
            .background(theme.card)
            .clickable(onClick = onClick)
            .padding(horizontal = 9.dp),
        contentAlignment = Alignment.Center
    ) {
        Icon(
            imageVector = icon,
            contentDescription = contentDesc,
            tint = theme.foreground,
            modifier = Modifier.size(16.dp)
        )
    }
}
