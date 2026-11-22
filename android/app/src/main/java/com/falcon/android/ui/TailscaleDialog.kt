package com.falcon.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.falcon.android.net.TailnetManager

@Composable
fun TailscaleSettingsDialog(
    onDismiss: () -> Unit,
    onToast: (String) -> Unit
) {
    val theme = LocalFalconTheme.current
    val monoFont = LocalFalconMonoFont.current
    val context = LocalContext.current
    val tn by TailnetManager.state.collectAsState()

    var hostnameInput by remember(tn.hostname) { mutableStateOf(tn.hostname) }
    var controlUrlInput by remember(tn.controlUrl) { mutableStateOf(tn.controlUrl) }
    var authKeyInput by remember { mutableStateOf("") }
    var advancedOpen by remember { mutableStateOf(false) }
    var confirmLogout by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = theme.card,
        title = {
            Text(
                text = "Tailscale",
                color = theme.foreground,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold
            )
        },
        text = {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(max = 460.dp)
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                Text(
                    text = "在客户端内置独立 Tailscale 节点，不占用系统 VPN 名额，此设置仅保存在本机。",
                    color = theme.mutedForeground,
                    fontSize = 11.5.sp
                )

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(8.dp))
                        .background(theme.background)
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text("启用内置 Tailscale", color = theme.foreground, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                        Text(
                            text = if (tn.libraryAvailable) "开启后自动加入 tailnet" else "此构建未包含 Tailscale",
                            color = theme.mutedForeground,
                            fontSize = 11.sp
                        )
                    }
                    Switch(
                        checked = tn.enabled,
                        enabled = tn.libraryAvailable && !tn.busy,
                        onCheckedChange = { TailnetManager.setEnabled(it) }
                    )
                }

                // 状态卡片
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(8.dp))
                        .background(theme.background)
                        .padding(10.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    if (!tn.libraryAvailable) {
                        Text("此构建未包含 Tailscale", color = theme.foreground, fontSize = 12.5.sp, fontWeight = FontWeight.Medium)
                        Text(
                            text = "当前安装包未附带 libfalcon_tailnet.so，其余功能不受影响。",
                            color = theme.mutedForeground,
                            fontSize = 11.sp
                        )
                    } else {
                        when (tn.status.state) {
                            "stopped" -> {
                                Text("未连接", color = theme.mutedForeground, fontSize = 12.sp)
                                if (!tn.lastError.isNullOrBlank()) {
                                    Text(tn.lastError!!, color = theme.destructive, fontSize = 11.sp)
                                }
                            }
                            "starting" -> {
                                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Box(Modifier.size(8.dp).clip(CircleShape).background(theme.warning))
                                    Text("正在连接 Tailscale…", color = theme.mutedForeground, fontSize = 12.sp)
                                }
                            }
                            "needs_login" -> {
                                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Box(Modifier.size(8.dp).clip(CircleShape).background(theme.warning))
                                    Text("需要登录 Tailscale", color = theme.foreground, fontSize = 12.5.sp, fontWeight = FontWeight.Medium)
                                }
                                val loginUrl = tn.status.loginUrl
                                if (!loginUrl.isNullOrBlank()) {
                                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                        Button(onClick = {
                                            runCatching {
                                                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(loginUrl)))
                                            }.onFailure {
                                                onToast("无法打开浏览器")
                                            }
                                        }) {
                                            Text("在浏览器中登录", fontSize = 12.sp)
                                        }
                                        OutlinedButton(onClick = {
                                            val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                                            cm?.setPrimaryClip(ClipData.newPlainText("Tailscale Login", loginUrl))
                                            onToast("已复制登录链接")
                                        }) {
                                            Text("复制链接", fontSize = 12.sp)
                                        }
                                    }
                                } else {
                                    Text("正在获取登录链接…", color = theme.mutedForeground, fontSize = 11.5.sp)
                                }
                            }
                            "needs_approval" -> {
                                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Box(Modifier.size(8.dp).clip(CircleShape).background(theme.warning))
                                    Text("等待管理员审批", color = theme.foreground, fontSize = 12.5.sp, fontWeight = FontWeight.Medium)
                                }
                                Text("在 Tailscale 管理后台批准此设备后会自动上线。", color = theme.mutedForeground, fontSize = 11.sp)
                            }
                            "running" -> {
                                val self = tn.status.selfNode
                                val onlineCount = tn.status.peers.count { it.online }
                                Row(
                                    modifier = Modifier.fillMaxWidth(),
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.SpaceBetween
                                ) {
                                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                        Box(Modifier.size(8.dp).clip(CircleShape).background(theme.success))
                                        Text("已连接", color = theme.foreground, fontSize = 12.5.sp, fontWeight = FontWeight.SemiBold)
                                    }
                                    Text("$onlineCount / ${tn.status.peers.size} 台在线", color = theme.mutedForeground, fontSize = 11.sp)
                                }
                                KeyValueRow("设备名", self?.name?.ifBlank { tn.effectiveHostname } ?: tn.effectiveHostname, monoFont)
                                KeyValueRow("Tailscale IP", self?.ipv4 ?: "—", monoFont)
                                KeyValueRow("Tailnet", self?.tailnet ?: self?.magicDnsSuffix ?: "—", monoFont)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    OutlinedButton(enabled = !tn.busy, onClick = { TailnetManager.setEnabled(false) }) {
                                        Text("断开连接", fontSize = 12.sp)
                                    }
                                    TextButton(enabled = !tn.busy, onClick = { confirmLogout = true }) {
                                        Text("退出登录", color = theme.destructive, fontSize = 12.sp)
                                    }
                                }
                            }
                            else -> {
                                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Box(Modifier.size(8.dp).clip(CircleShape).background(theme.destructive))
                                    Text("连接出错", color = theme.destructive, fontSize = 12.5.sp, fontWeight = FontWeight.Medium)
                                }
                                Text(tn.status.error ?: tn.lastError ?: "未知错误", color = theme.mutedForeground, fontSize = 11.sp)
                                OutlinedButton(enabled = !tn.busy, onClick = { TailnetManager.retry() }) {
                                    Text("重试", fontSize = 12.sp)
                                }
                            }
                        }
                    }
                }

                // 高级配置（默认折叠）
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(6.dp))
                        .clickable { advancedOpen = !advancedOpen }
                        .padding(vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    Text(
                        text = if (advancedOpen) "▾ 高级" else "▸ 高级",
                        color = theme.mutedForeground,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium
                    )
                }

                if (advancedOpen) {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedTextField(
                            value = hostnameInput,
                            onValueChange = { hostnameInput = it },
                            label = { Text("设备名（默认 ${tn.defaultHostname}）", fontSize = 11.5.sp) },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth()
                        )
                        OutlinedTextField(
                            value = controlUrlInput,
                            onValueChange = { controlUrlInput = it },
                            label = { Text("控制服务器（Headscale URL，留空官方）", fontSize = 11.5.sp) },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth()
                        )
                        OutlinedTextField(
                            value = authKeyInput,
                            onValueChange = { authKeyInput = it },
                            label = { Text("Auth Key（可选，仅写入不回显）", fontSize = 11.5.sp) },
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            modifier = Modifier.fillMaxWidth()
                        )
                        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                            OutlinedButton(
                                enabled = tn.libraryAvailable && !tn.busy,
                                onClick = {
                                    val key = authKeyInput.trim().takeIf { it.isNotEmpty() }
                                    authKeyInput = ""
                                    TailnetManager.updateSettings(hostnameInput, controlUrlInput, key)
                                    onToast("Tailscale 配置已保存")
                                }
                            ) {
                                Text("保存并应用", fontSize = 12.sp)
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text("关闭", fontSize = 13.sp)
            }
        }
    )

    if (confirmLogout) {
        AlertDialog(
            onDismissRequest = { confirmLogout = false },
            containerColor = theme.card,
            title = { Text("退出内置 Tailscale 登录？", color = theme.foreground, fontSize = 14.5.sp, fontWeight = FontWeight.SemiBold) },
            text = { Text("会清除本机保存的节点身份与状态，下次开启需要重新登录授权。", color = theme.mutedForeground, fontSize = 12.5.sp) },
            confirmButton = {
                Button(onClick = {
                    confirmLogout = false
                    TailnetManager.logout()
                }) {
                    Text("退出登录", fontSize = 13.sp)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmLogout = false }) {
                    Text("取消", color = theme.mutedForeground, fontSize = 13.sp)
                }
            }
        )
    }
}

@Composable
private fun KeyValueRow(label: String, value: String, monoFont: androidx.compose.ui.text.font.FontFamily) {
    val theme = LocalFalconTheme.current
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(label, color = theme.mutedForeground, fontSize = 11.5.sp)
        Text(value, color = theme.foreground, fontSize = 11.5.sp, fontFamily = monoFont)
    }
}
