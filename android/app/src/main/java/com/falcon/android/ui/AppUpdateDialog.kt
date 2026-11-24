package com.falcon.android.ui

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import com.falcon.android.net.AppUpdateState
import com.falcon.android.net.AppUpdates

@Composable
internal fun AppUpdateDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val current = remember(context) { context.packageManager.getPackageInfo(context.packageName, 0).versionName.orEmpty() }
    val state by AppUpdates.state.collectAsState()
    var error by remember { mutableStateOf<String?>(null) }
    val theme = LocalFalconTheme.current

    fun install() {
        val ready = state as? AppUpdateState.Ready ?: return
        try {
            if (!ready.file.isFile) {
                AppUpdates.check(context, current)
                return
            }
            val uri = FileProvider.getUriForFile(context, "${context.packageName}.updates", ready.file)
            context.startActivity(Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            })
            error = null
        } catch (e: Exception) {
            error = e.localizedMessage ?: "无法打开系统安装器"
        }
    }

    val permission = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        if (context.packageManager.canRequestPackageInstalls()) install()
        else error = "需要在系统设置中允许 Falcon 安装应用后才能继续"
    }

    LaunchedEffect(Unit) {
        AppUpdates.check(context, current)
    }
    AlertDialog(
        onDismissRequest = onDismiss, containerColor = theme.card,
        title = { Text("客户端更新", color = theme.foreground) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("当前版本 $current", color = theme.foreground)
                val message = when (val s = state) {
                    AppUpdateState.Checking -> "正在检查更新…"
                    AppUpdateState.Current -> "当前已是最新版本"
                    is AppUpdateState.Available -> "发现新版本 ${s.release.version}"
                    is AppUpdateState.Downloading -> "正在下载 ${s.percent}%"
                    is AppUpdateState.Ready -> "版本 ${s.version} 已下载，可继续安装"
                    is AppUpdateState.Failed -> s.message
                }
                Text(message, color = if (state is AppUpdateState.Failed) theme.destructive else theme.mutedForeground)
                error?.let { Text(it, color = theme.destructive) }
            }
        },
        confirmButton = {
            when (state) {
                is AppUpdateState.Ready -> TextButton(onClick = {
                    error = null
                    if (context.packageManager.canRequestPackageInstalls()) install()
                    else try {
                        permission.launch(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}")))
                    } catch (e: Exception) {
                        error = e.localizedMessage ?: "无法打开系统安装权限设置"
                    }
                }) { Text("安装更新") }
                is AppUpdateState.Available -> TextButton(onClick = { AppUpdates.download(context) }) { Text("下载更新") }
                AppUpdateState.Current, is AppUpdateState.Failed -> TextButton(onClick = {
                    error = null
                    AppUpdates.check(context, current)
                }) { Text("重新检查") }
                else -> Unit
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("关闭") } }
    )
}
