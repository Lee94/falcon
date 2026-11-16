package com.falcon.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.falcon.android.model.AgentCatalog
import com.falcon.android.model.AgentEntryUi
import com.falcon.android.model.AgentPlanItemUi
import com.falcon.android.model.AgentToolCallUi
import com.falcon.android.model.AgentToolContentUi
import com.falcon.android.model.AgentViewStateUi

@Composable
fun AgentSessionView(
    state: AgentViewStateUi,
    terminalStyle: Boolean,
    available: Boolean,
    composerFocus: FocusRequester,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    inputText: String,
    onInputChange: (String) -> Unit,
    onSendPrompt: (String) -> Boolean,
    onInterrupt: () -> Unit,
    onApprovePermission: (callId: String, decision: String) -> Unit,
    onSelectModel: (String) -> Unit,
    onSelectReasoningEffort: (String?) -> Unit,
    onToggleStyle: () -> Unit,
    modifier: Modifier = Modifier
) {
    val foldedMap = remember { mutableStateMapOf<String, Boolean>() }
    val listState = rememberLazyListState()

    val isRunning = state.status == "running" || state.status == "awaiting_permission"
    val enabled = available && state.interactive && state.status != "unknown"
    val canConfigure = enabled && !isRunning
    val selectedModel = state.selectedModelOption()
    val modelChoices = state.modelChoices()
    val legacyCatalog = state.modelOptions.isEmpty() && modelChoices.isNotEmpty()
    val approvePermission: (String, String) -> Unit = { callId, decision ->
        if (enabled) onApprovePermission(callId, decision)
    }
    val agentLabel = AgentCatalog.labelFor(state.agent)

    // 新增条目或流式输出时自动滚到底部
    val lastEntrySignature = state.entries.lastOrNull()?.let { e ->
        when (e) {
            is AgentEntryUi.Message -> "${e.id}:${e.text.length}"
            is AgentEntryUi.Thought -> "${e.id}:${e.text.length}"
            is AgentEntryUi.ToolCall -> "${e.id}:${e.call.status}:${e.call.permissionPending}:${e.call.content.size}"
            else -> e.id
        }
    }
    LaunchedEffect(state.entries.size, lastEntrySignature) {
        if (state.entries.isNotEmpty()) {
            listState.animateScrollToItem(state.entries.size - 1)
        }
    }

    val slashPrefix = remember(inputText) {
        if (inputText.startsWith("/") && !inputText.contains("\n") && !inputText.contains(" ")) {
            inputText.removePrefix("/").lowercase()
        } else {
            null
        }
    }
    val matchingCommands = remember(slashPrefix, state.commands) {
        if (slashPrefix == null) {
            emptyList()
        } else {
            state.commands.filter { it.name.lowercase().contains(slashPrefix) }.take(6)
        }
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .background(theme.background)
    ) {
        // 1. 时间线主体
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
        ) {
            if (state.entries.isEmpty()) {
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center
                ) {
                    Text(
                        text = "$agentLabel · 原生对话已就绪",
                        color = theme.foreground,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold,
                        fontFamily = monoFont
                    )
                    Spacer(modifier = Modifier.height(6.dp))
                    Text(
                        text = if (terminalStyle) "紧凑样式 · 输入指令开始对话" else "输入指令，查看 Markdown、思考过程与文件差异",
                        color = theme.mutedForeground,
                        fontSize = 12.sp
                    )
                }
            } else {
                SelectionContainer {
                    LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize(),
                        contentPadding = PaddingValues(horizontal = 12.dp, vertical = 10.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp)
                    ) {
                        itemsIndexed(
                            items = state.entries,
                            key = { idx, item -> "${idx}_${item.id}" }
                        ) { _, entry ->
                            if (terminalStyle) {
                                TerminalStyleEntry(entry, foldedMap, theme, monoFont, enabled, approvePermission)
                            } else when (entry) {
                                is AgentEntryUi.User -> UserBubble(entry.text, theme)
                                is AgentEntryUi.Thought -> {
                                    val isOpen = foldedMap[entry.id] ?: false
                                    ThoughtBlock(
                                        text = entry.text,
                                        isOpen = isOpen,
                                        theme = theme,
                                        onToggle = { foldedMap[entry.id] = !isOpen }
                                    )
                                }
                                is AgentEntryUi.Message -> MessageBlock(entry.text, theme, monoFont)
                                is AgentEntryUi.ToolCall -> {
                                    val tc = entry.call
                                    val defaultOpen = tc.permissionPending ||
                                        tc.status == "failed" ||
                                        tc.kind == "edit" ||
                                        tc.kind == "write"
                                    val isOpen = foldedMap[tc.id] ?: defaultOpen
                                    ToolCallCard(
                                        tc = tc,
                                        isOpen = isOpen,
                                        theme = theme,
                                        monoFont = monoFont,
                                        onToggle = { foldedMap[tc.id] = !isOpen },
                                        enabled = enabled,
                                        onApprovePermission = approvePermission
                                    )
                                }
                                is AgentEntryUi.Plan -> PlanCard(entry.items, theme)
                                is AgentEntryUi.Notice -> NoticeCard(entry.level, entry.text, theme)
                            }
                        }
                    }
                }
            }
        }

        // 2. 斜杠命令候选浮层
        if (enabled && matchingCommands.isNotEmpty()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 10.dp, vertical = 4.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .background(theme.card)
                    .border(0.8.dp, theme.muted, RoundedCornerShape(8.dp))
                    .padding(4.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp)
            ) {
                for (cmd in matchingCommands) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(5.dp))
                            .clickable(enabled = enabled) { onInputChange("/${cmd.name} ") }
                            .padding(horizontal = 8.dp, vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text(
                            text = "/${cmd.name}",
                            color = theme.tintStrong,
                            fontSize = 12.sp,
                            fontFamily = monoFont,
                            fontWeight = FontWeight.SemiBold
                        )
                        Spacer(modifier = Modifier.width(8.dp))
                        Text(
                            text = cmd.description,
                            color = theme.mutedForeground,
                            fontSize = 11.5.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )
                    }
                }
            }
        }

        // 3. 底部状态与输入栏
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(theme.sidebar)
                .padding(horizontal = 10.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            // 模型与思考程度来自真实能力目录；切换样式不影响正在运行的轮次。
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween
            ) {
                Row(
                    modifier = Modifier.weight(1f).horizontalScroll(rememberScrollState()),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    AgentChoiceMenu(
                        label = selectedModel?.label ?: state.model ?: "正在加载模型…",
                        selected = selectedModel?.id ?: state.model,
                        options = modelChoices.map { it.id to it.label },
                        enabled = canConfigure && modelChoices.isNotEmpty(),
                        theme = theme,
                        monoFont = monoFont,
                        onSelect = { model -> if (canConfigure && model != null) onSelectModel(model) }
                    )
                    val efforts = selectedModel?.reasoningEfforts.orEmpty()
                    val defaultEffort = selectedModel?.defaultReasoningEffort?.let { "默认（${effortLabel(it)}）" } ?: "默认"
                    AgentChoiceMenu(
                        label = if (legacyCatalog) "服务端未提供思考程度" else if (efforts.isEmpty()) "思考程度不可用" else "思考：${state.reasoningEffort?.let(::effortLabel) ?: defaultEffort}",
                        selected = state.reasoningEffort,
                        options = listOf(null to defaultEffort) + efforts.map { it to effortLabel(it) },
                        enabled = canConfigure && efforts.isNotEmpty(),
                        theme = theme,
                        monoFont = monoFont,
                        onSelect = { if (canConfigure) onSelectReasoningEffort(it) }
                    )

                    if (isRunning) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(5.dp)
                        ) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(11.dp),
                                strokeWidth = 1.5.dp,
                                color = theme.tintStrong
                            )
                            Text(
                                text = if (state.status == "awaiting_permission") "等待授权…" else "思考中…",
                                color = theme.tintStrong,
                                fontSize = 11.sp
                            )
                        }
                    }
                }

                Box(
                    modifier = Modifier
                        .clip(RoundedCornerShape(5.dp))
                        .clickable { onToggleStyle() }
                        .padding(horizontal = 6.dp, vertical = 3.dp)
                ) {
                    Text(
                        text = if (terminalStyle) "切换图形样式" else "切换紧凑样式",
                        color = theme.mutedForeground,
                        fontSize = 11.sp,
                        fontFamily = monoFont
                    )
                }
            }

            // 多行输入框 + 发送 / 终止按钮
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .then(if (terminalStyle) Modifier else Modifier.clip(RoundedCornerShape(8.dp)).background(theme.background))
                    .padding(horizontal = 10.dp, vertical = 6.dp),
                verticalAlignment = Alignment.Bottom,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                if (terminalStyle) Text("›", color = theme.tintStrong, fontFamily = monoFont, fontSize = 13.sp)
                Box(
                    modifier = Modifier
                        .weight(1f)
                        .heightIn(min = 24.dp, max = 110.dp),
                    contentAlignment = Alignment.CenterStart
                ) {
                    if (inputText.isEmpty()) {
                        Text(
                            text = "向 $agentLabel 发送消息，输入 / 调出命令…",
                            color = theme.mutedForeground,
                            fontSize = 13.sp
                        )
                    }
                    BasicTextField(
                        value = inputText,
                        onValueChange = onInputChange,
                        enabled = enabled,
                        textStyle = TextStyle(
                            color = theme.foreground,
                            fontSize = 13.sp,
                            lineHeight = 19.sp,
                            fontFamily = if (terminalStyle) monoFont else FontFamily.Default
                        ),
                        cursorBrush = SolidColor(theme.tintStrong),
                        modifier = Modifier.fillMaxWidth().focusRequester(composerFocus)
                    )
                }

                if (isRunning) {
                    Box(
                        modifier = Modifier
                            .size(28.dp)
                            .clip(RoundedCornerShape(6.dp))
                            .background(theme.destructive.copy(alpha = 0.18f))
                            .border(0.8.dp, theme.destructive.copy(alpha = 0.5f), RoundedCornerShape(6.dp))
                            .clickable(enabled = enabled) { onInterrupt() },
                        contentAlignment = Alignment.Center
                    ) {
                        Icon(
                            imageVector = Icons.Default.Stop,
                            contentDescription = "停止",
                            tint = theme.destructive,
                            modifier = Modifier.size(15.dp)
                        )
                    }
                } else {
                    val canSend = enabled && inputText.isNotBlank()
                    Box(
                        modifier = Modifier
                            .size(28.dp)
                            .clip(RoundedCornerShape(6.dp))
                            .background(if (canSend) theme.primary else theme.secondary)
                            .clickable(enabled = canSend) {
                                val prompt = inputText.trim()
                                if (prompt.isNotEmpty() && onSendPrompt(prompt)) {
                                    onInputChange("")
                                }
                            },
                        contentAlignment = Alignment.Center
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Filled.Send,
                            contentDescription = "发送",
                            tint = if (canSend) theme.background else theme.mutedForeground,
                            modifier = Modifier.size(14.dp)
                        )
                    }
                }
            }
        }
    }
}

private fun effortLabel(value: String): String = when (value) {
    "off" -> "关闭"
    "minimal" -> "极少"
    "low" -> "低"
    "medium" -> "中"
    "high" -> "高"
    "xhigh" -> "极高"
    "max" -> "最高"
    else -> value
}

@Composable
private fun AgentChoiceMenu(
    label: String,
    selected: String?,
    options: List<Pair<String?, String>>,
    enabled: Boolean,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onSelect: (String?) -> Unit
) {
    var expanded by remember { mutableStateOf(false) }
    LaunchedEffect(enabled) { if (!enabled) expanded = false }
    Box {
        Row(
            modifier = Modifier.clip(RoundedCornerShape(5.dp)).background(theme.secondary)
                .clickable(enabled = enabled) { expanded = true }
                .padding(horizontal = 7.dp, vertical = 3.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = label,
                color = if (enabled) theme.foreground else theme.mutedForeground,
                fontSize = 11.sp,
                fontFamily = monoFont,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 190.dp)
            )
            if (enabled) Icon(Icons.Default.KeyboardArrowDown, null, tint = theme.mutedForeground, modifier = Modifier.size(13.dp))
        }
        DropdownMenu(expanded = expanded && enabled, onDismissRequest = { expanded = false }, modifier = Modifier.background(theme.card)) {
            for ((value, optionLabel) in options) {
                DropdownMenuItem(
                    text = { Text(optionLabel, color = if (value == selected) theme.tintStrong else theme.foreground, fontSize = 12.sp) },
                    enabled = enabled,
                    onClick = {
                        expanded = false
                        if (enabled) onSelect(value)
                    }
                )
            }
        }
    }
}

@Composable
private fun TerminalStyleEntry(
    entry: AgentEntryUi,
    folded: MutableMap<String, Boolean>,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    enabled: Boolean,
    onApprove: (String, String) -> Unit
) {
    when (entry) {
        is AgentEntryUi.User -> TerminalLine("›", entry.text, theme.tintStrong, monoFont)
        is AgentEntryUi.Message -> TerminalLine("•", entry.text, theme.foreground, monoFont)
        is AgentEntryUi.Thought -> {
            val isOpen = folded[entry.id] ?: false
            Column(Modifier.fillMaxWidth()) {
                Text(
                    text = if (isOpen) "▾ 思考过程" else "▸ 思考过程 · ${entry.text.lineSequence().lastOrNull { it.isNotBlank() }.orEmpty().take(60)}",
                    color = theme.mutedForeground,
                    fontFamily = monoFont,
                    fontSize = 12.sp,
                    maxLines = if (isOpen) Int.MAX_VALUE else 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.clickable { folded[entry.id] = !isOpen }.padding(vertical = 3.dp)
                )
                if (isOpen) TerminalLine("·", entry.text, theme.mutedForeground, monoFont)
            }
        }
        is AgentEntryUi.ToolCall -> {
            val tc = entry.call
            val isOpen = folded[tc.id] ?: (tc.permissionPending || tc.status == "failed" || tc.kind == "edit" || tc.kind == "write")
            ToolCallCard(tc, isOpen, theme, monoFont, { folded[tc.id] = !isOpen }, onApprove, enabled, true)
        }
        is AgentEntryUi.Plan -> TerminalLine("≡", entry.items.joinToString("\n") {
            val mark = when (it.status) { "completed" -> "[x]"; "in_progress" -> "[>]"; else -> "[ ]" }
            "$mark ${it.content}"
        }, theme.foreground, monoFont)
        is AgentEntryUi.Notice -> TerminalLine("·", entry.text, when (entry.level) {
            "error" -> theme.destructive
            "warning" -> theme.warning
            else -> theme.mutedForeground
        }, monoFont)
    }
}

@Composable
private fun TerminalLine(prefix: String, text: String, color: androidx.compose.ui.graphics.Color, monoFont: FontFamily) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(prefix, color = color, fontFamily = monoFont, fontSize = 12.sp, lineHeight = 19.sp)
        Text(text, color = color, fontFamily = monoFont, fontSize = 12.sp, lineHeight = 19.sp, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun UserBubble(text: String, theme: FalconThemeTokens) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End
    ) {
        Box(
            modifier = Modifier
                .widthIn(max = 310.dp)
                .clip(RoundedCornerShape(8.dp))
                .background(theme.tint)
                .border(0.8.dp, theme.tintStrong.copy(alpha = 0.35f), RoundedCornerShape(8.dp))
                .padding(horizontal = 11.dp, vertical = 7.dp)
        ) {
            Text(
                text = text,
                color = theme.foreground,
                fontSize = 13.sp,
                lineHeight = 19.sp
            )
        }
    }
}

@Composable
private fun ThoughtBlock(
    text: String,
    isOpen: Boolean,
    theme: FalconThemeTokens,
    onToggle: () -> Unit
) {
    val trimmed = text.trim()
    if (trimmed.isEmpty()) return
    val preview = remember(trimmed) {
        trimmed.lineSequence().lastOrNull { it.isNotBlank() }?.trim()?.take(60) ?: ""
    }

    Column(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(5.dp))
                .clickable { onToggle() }
                .padding(vertical = 3.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(
                imageVector = if (isOpen) Icons.Default.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null,
                tint = theme.mutedForeground,
                modifier = Modifier.size(15.dp)
            )
            Spacer(modifier = Modifier.width(4.dp))
            Text(
                text = "思考过程",
                color = theme.mutedForeground,
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium
            )
            if (!isOpen && preview.isNotEmpty()) {
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    text = "· $preview",
                    color = theme.mutedForeground.copy(alpha = 0.72f),
                    fontSize = 11.5.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f)
                )
            }
        }
        if (isOpen) {
            Box(
                modifier = Modifier
                    .padding(start = 7.dp, top = 3.dp)
                    .border(width = 1.dp, color = theme.muted, shape = RoundedCornerShape(4.dp))
                    .padding(horizontal = 9.dp, vertical = 6.dp)
            ) {
                Text(
                    text = trimmed,
                    color = theme.mutedForeground,
                    fontSize = 12.sp,
                    lineHeight = 17.5.sp
                )
            }
        }
    }
}

@Composable
private fun MessageBlock(text: String, theme: FalconThemeTokens, monoFont: FontFamily) {
    val trimmed = text.trim()
    if (trimmed.isEmpty()) return
    val segments = remember(trimmed) { splitCodeFences(trimmed) }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        for (seg in segments) {
            if (seg.isCode) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(6.dp))
                        .background(theme.card)
                        .border(0.8.dp, theme.muted, RoundedCornerShape(6.dp))
                ) {
                    if (seg.lang.isNotEmpty()) {
                        Text(
                            text = seg.lang,
                            color = theme.mutedForeground,
                            fontSize = 10.5.sp,
                            fontFamily = monoFont,
                            modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp)
                        )
                    }
                    Box(
                        modifier = Modifier
                            .fillMaxWidth()
                            .horizontalScroll(rememberScrollState())
                            .padding(horizontal = 8.dp, vertical = 6.dp)
                    ) {
                        Text(
                            text = seg.content,
                            color = theme.foreground,
                            fontSize = 11.5.sp,
                            lineHeight = 16.5.sp,
                            fontFamily = monoFont
                        )
                    }
                }
            } else {
                Text(
                    text = seg.content,
                    color = theme.foreground,
                    fontSize = 13.sp,
                    lineHeight = 19.5.sp
                )
            }
        }
    }
}

private data class MdSegment(val isCode: Boolean, val lang: String, val content: String)

private fun splitCodeFences(raw: String): List<MdSegment> {
    val out = ArrayList<MdSegment>()
    val lines = raw.lines()
    var inFence = false
    var fenceLang = ""
    val buf = StringBuilder()

    for (line in lines) {
        if (line.trimStart().startsWith("```")) {
            if (!inFence) {
                if (buf.isNotEmpty()) {
                    out.add(MdSegment(false, "", buf.toString().trimEnd()))
                    buf.clear()
                }
                inFence = true
                fenceLang = line.trimStart().removePrefix("```").trim()
            } else {
                out.add(MdSegment(true, fenceLang, buf.toString().trimEnd()))
                buf.clear()
                inFence = false
                fenceLang = ""
            }
        } else {
            if (buf.isNotEmpty()) buf.append('\n')
            buf.append(line)
        }
    }
    if (buf.isNotEmpty()) {
        out.add(MdSegment(inFence, fenceLang, buf.toString().trimEnd()))
    }
    return out
}

@Composable
private fun ToolCallCard(
    tc: AgentToolCallUi,
    isOpen: Boolean,
    theme: FalconThemeTokens,
    monoFont: FontFamily,
    onToggle: () -> Unit,
    onApprovePermission: (callId: String, decision: String) -> Unit,
    enabled: Boolean,
    terminalStyle: Boolean = false
) {
    val borderColor = when {
        tc.permissionPending -> theme.warning.copy(alpha = 0.7f)
        tc.status == "failed" -> theme.destructive.copy(alpha = 0.5f)
        else -> theme.muted
    }
    val statusColor = when {
        tc.permissionPending -> theme.warning
        tc.status == "completed" -> theme.success
        tc.status == "failed" -> theme.destructive
        else -> theme.tintStrong
    }
    val hasBody = tc.content.isNotEmpty() || tc.permissionPending

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .then(if (terminalStyle) Modifier else Modifier.clip(RoundedCornerShape(8.dp)).background(theme.card).border(0.8.dp, borderColor, RoundedCornerShape(8.dp)))
    ) {
        // 卡片标题行
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(enabled = hasBody) { onToggle() }
                .padding(horizontal = 10.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween
        ) {
            Row(
                modifier = Modifier.weight(1f),
                verticalAlignment = Alignment.CenterVertically
            ) {
                Box(
                    modifier = Modifier
                        .size(7.dp)
                        .clip(CircleShape)
                        .background(statusColor)
                )
                Spacer(modifier = Modifier.width(7.dp))
                Text(
                    text = tc.title,
                    color = theme.foreground,
                    fontSize = 12.sp,
                    fontFamily = monoFont,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                if (!tc.description.isNullOrBlank()) {
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        text = "— ${tc.description}",
                        color = theme.mutedForeground,
                        fontSize = 11.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                }
            }
            if (hasBody) {
                Icon(
                    imageVector = if (isOpen) Icons.Default.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    contentDescription = null,
                    tint = theme.mutedForeground,
                    modifier = Modifier.size(16.dp)
                )
            }
        }

        if (isOpen && hasBody) {
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(0.8.dp)
                    .background(theme.muted)
            )
            for (c in tc.content) {
                when (c) {
                    is AgentToolContentUi.Text -> {
                        if (c.text.isNotBlank()) {
                            TerminalOutputBox(c.text, null, theme, monoFont)
                        }
                    }
                    is AgentToolContentUi.Terminal -> {
                        if (c.output.isNotBlank() || c.exitCode != null) {
                            TerminalOutputBox(c.output, c.exitCode, theme, monoFont)
                        }
                    }
                    is AgentToolContentUi.Diff -> {
                        if (c.unifiedDiff.isNotBlank()) {
                            DiffOutputBox(c.unifiedDiff, theme, monoFont)
                        }
                    }
                }
            }

            if (tc.permissionPending) {
                PermissionApprovalBar(
                    tc = tc,
                    theme = theme,
                    enabled = enabled,
                    onApprove = { decision -> if (enabled) onApprovePermission(tc.id, decision) }
                )
            }
        }
    }
}

@Composable
private fun TerminalOutputBox(
    output: String,
    exitCode: Int?,
    theme: FalconThemeTokens,
    monoFont: FontFamily
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(theme.terminalBackground.copy(alpha = 0.7f))
            .padding(horizontal = 8.dp, vertical = 6.dp)
    ) {
        if (output.isNotEmpty()) {
            Text(
                text = output.trimEnd(),
                color = theme.foreground,
                fontSize = 11.sp,
                lineHeight = 16.sp,
                fontFamily = monoFont
            )
        }
        if (exitCode != null && exitCode != 0) {
            Spacer(modifier = Modifier.height(3.dp))
            Text(
                text = "退出码 $exitCode",
                color = theme.destructive,
                fontSize = 10.5.sp,
                fontFamily = monoFont
            )
        }
    }
}

@Composable
private fun DiffOutputBox(
    unifiedDiff: String,
    theme: FalconThemeTokens,
    monoFont: FontFamily
) {
    val lines = remember(unifiedDiff) { unifiedDiff.trimEnd().lines() }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(theme.terminalBackground.copy(alpha = 0.7f))
            .padding(vertical = 4.dp)
    ) {
        for (line in lines) {
            val isAdd = line.startsWith("+")
            val isDel = line.startsWith("-")
            val bg = when {
                isAdd -> theme.success.copy(alpha = 0.14f)
                isDel -> theme.destructive.copy(alpha = 0.14f)
                else -> androidx.compose.ui.graphics.Color.Transparent
            }
            val fg = when {
                isAdd -> theme.success
                isDel -> theme.destructive
                else -> theme.mutedForeground
            }
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(bg)
                    .padding(horizontal = 8.dp, vertical = 1.dp)
            ) {
                Text(
                    text = line,
                    color = fg,
                    fontSize = 11.sp,
                    lineHeight = 15.5.sp,
                    fontFamily = monoFont
                )
            }
        }
    }
}

@Composable
private fun PermissionApprovalBar(
    tc: AgentToolCallUi,
    theme: FalconThemeTokens,
    enabled: Boolean,
    onApprove: (String) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(theme.warning.copy(alpha = 0.12f))
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        Text(
            text = tc.permissionReason ?: "需要授权执行此操作",
            color = theme.warning,
            fontSize = 11.5.sp,
            fontWeight = FontWeight.Medium
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            ApprovalButton(
                label = "允许",
                enabled = enabled,
                bg = theme.primary,
                fg = theme.background,
                onClick = { onApprove("allow") }
            )
            ApprovalButton(
                label = "本会话总是允许",
                enabled = enabled,
                bg = theme.secondary,
                fg = theme.foreground,
                onClick = { onApprove("allow_session") }
            )
            ApprovalButton(
                label = "拒绝",
                enabled = enabled,
                bg = theme.destructive.copy(alpha = 0.2f),
                fg = theme.destructive,
                onClick = { onApprove("deny") }
            )
        }
    }
}

@Composable
private fun ApprovalButton(
    label: String,
    enabled: Boolean,
    bg: androidx.compose.ui.graphics.Color,
    fg: androidx.compose.ui.graphics.Color,
    onClick: () -> Unit
) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(if (enabled) bg else bg.copy(alpha = 0.3f))
            .clickable(enabled = enabled) { onClick() }
            .padding(horizontal = 10.dp, vertical = 5.dp),
        contentAlignment = Alignment.Center
    ) {
        Text(
            text = label,
            color = fg,
            fontSize = 11.5.sp,
            fontWeight = FontWeight.SemiBold
        )
    }
}

@Composable
private fun PlanCard(items: List<AgentPlanItemUi>, theme: FalconThemeTokens) {
    if (items.isEmpty()) return
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(theme.card)
            .border(0.8.dp, theme.muted, RoundedCornerShape(8.dp))
            .padding(10.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp)
    ) {
        Text(
            text = "任务清单",
            color = theme.foreground,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold
        )
        for (item in items) {
            val done = item.status == "completed"
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Icon(
                    imageVector = if (done) Icons.Default.Check else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    contentDescription = null,
                    tint = if (done) theme.success else theme.tintStrong,
                    modifier = Modifier.size(13.dp)
                )
                Text(
                    text = item.content,
                    color = if (done) theme.mutedForeground else theme.foreground,
                    fontSize = 12.sp
                )
            }
        }
    }
}

@Composable
private fun NoticeCard(level: String, text: String, theme: FalconThemeTokens) {
    val tone = when (level) {
        "error" -> theme.destructive
        "warning" -> theme.warning
        else -> theme.mutedForeground
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(7.dp))
            .background(tone.copy(alpha = 0.1f))
            .border(0.8.dp, tone.copy(alpha = 0.4f), RoundedCornerShape(7.dp))
            .padding(horizontal = 10.dp, vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp)
    ) {
        if (level == "error") {
            Icon(
                imageVector = Icons.Default.Close,
                contentDescription = null,
                tint = tone,
                modifier = Modifier.size(14.dp)
            )
        }
        Text(
            text = text,
            color = tone,
            fontSize = 12.sp,
            modifier = Modifier.weight(1f)
        )
    }
}
