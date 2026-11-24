package com.falcon.android.net

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.io.OutputStream
import java.security.MessageDigest

private const val REPO = "Lee94/falcon"
internal data class AndroidRelease(val version: String, val url: String, val size: Long, val sha256: String)

private fun versionParts(version: String): List<Long> {
    require(version.matches(Regex("[0-9]+\\.[0-9]+\\.[0-9]+"))) { "无法识别版本号 $version" }
    return version.split('.').map { it.toLong() }
}

internal fun newerAndroidRelease(json: JSONObject, current: String): AndroidRelease? {
    require(!json.getBoolean("draft") && !json.getBoolean("prerelease")) { "未找到稳定版本" }
    val tag = json.getString("tag_name")
    val version = tag.removePrefix("v")
    val comparison = versionParts(version).zip(versionParts(current))
        .firstOrNull { (latest, installed) -> latest != installed } ?: return null
    if (comparison.first < comparison.second) return null

    val name = "falcon-v$version-android.apk"
    val assets = json.getJSONArray("assets")
    val asset = (0 until assets.length()).map { assets.getJSONObject(it) }.singleOrNull { it.optString("name") == name }
        ?: error("此版本缺少 Android 安装包")
    val url = "https://github.com/$REPO/releases/download/$tag/$name"
    require(asset.getString("browser_download_url") == url) { "安装包下载地址与发布版本不符" }
    val digest = asset.getString("digest")
    require(digest.matches(Regex("sha256:[0-9a-f]{64}"))) { "缺少有效的安装包校验值" }
    val size = asset.getLong("size")
    require(size > 0) { "安装包大小无效" }
    return AndroidRelease(version, url, size, digest.substringAfter(':'))
}

internal fun copyVerified(input: InputStream, output: OutputStream, release: AndroidRelease, progress: (Int) -> Unit) {
    val digest = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(64 * 1024)
    var total = 0L
    var previous = -1
    while (true) {
        val n = input.read(buffer)
        if (n == -1) break
        total += n
        require(total <= release.size) { "安装包超过发布大小" }
        digest.update(buffer, 0, n)
        output.write(buffer, 0, n)
        val percent = (100.0 * total / release.size).toInt()
        if (percent != previous) {
            progress(percent)
            previous = percent
        }
    }
    require(total == release.size && digest.digest().joinToString("") { "%02x".format(it) } == release.sha256) {
        "安装包校验失败，请重新下载"
    }
}

internal sealed interface AppUpdateState {
    data object Checking : AppUpdateState
    data object Current : AppUpdateState
    data class Available(val release: AndroidRelease) : AppUpdateState
    data class Downloading(val percent: Int) : AppUpdateState
    data class Ready(val file: File, val version: String) : AppUpdateState
    data class Failed(val message: String) : AppUpdateState
}

internal object AppUpdates {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val client = OkHttpClient.Builder().followSslRedirects(false).build()
    private val status = MutableStateFlow<AppUpdateState>(AppUpdateState.Checking)
    val state = status.asStateFlow()
    private var job: Job? = null

    fun check(context: Context, current: String) {
        if (job?.isActive == true) return
        val dir = File(context.cacheDir, "app-updates")
        status.value = AppUpdateState.Checking
        job = scope.launch {
            try {
                status.value = withContext(Dispatchers.IO) {
                    val request = Request.Builder().url("https://api.github.com/repos/$REPO/releases/latest").build()
                    val release = client.newCall(request).execute().use { res ->
                        check(res.isSuccessful) { "检查更新失败（HTTP ${res.code}）" }
                        newerAndroidRelease(JSONObject(res.body.string()), current)
                    }
                    if (release == null) {
                        dir.listFiles()?.forEach { it.delete() }
                        AppUpdateState.Current
                    } else {
                        val cached = File(dir, "${release.sha256}.apk")
                        if (cached.isFile && cached.length() == release.size) AppUpdateState.Ready(cached, release.version)
                        else AppUpdateState.Available(release)
                    }
                }
            } catch (e: Exception) {
                status.value = AppUpdateState.Failed(e.localizedMessage ?: "检查更新失败")
            }
        }
    }

    fun download(context: Context) {
        val release = (status.value as? AppUpdateState.Available)?.release ?: return
        val app = context.applicationContext
        status.value = AppUpdateState.Downloading(0)
        job = scope.launch {
            try {
                val file = withContext(Dispatchers.IO) {
                    val dir = File(app.cacheDir, "app-updates")
                    check(dir.isDirectory || dir.mkdirs()) { "无法创建更新目录" }
                    val apk = File(dir, "${release.sha256}.apk")
                    // 临时文件和已完成的包在同一目录，只有校验成功后才交给安装器。
                    val tmp = File(dir, "download.part.apk")
                    try {
                        client.newCall(Request.Builder().url(release.url).build()).execute().use { res ->
                            check(res.isSuccessful) { "下载失败（HTTP ${res.code}）" }
                            tmp.outputStream().buffered().use { out ->
                                copyVerified(res.body.byteStream(), out, release) { status.value = AppUpdateState.Downloading(it) }
                            }
                        }
                        val pm = app.packageManager
                        val info = pm.getPackageArchiveInfo(tmp.path, 0)
                        val installed = pm.getPackageInfo(app.packageName, 0)
                        check(info?.packageName == app.packageName && info.versionName == release.version &&
                            info.longVersionCode > installed.longVersionCode) { "安装包与当前应用或目标版本不符" }
                        check(tmp.renameTo(apk)) { "无法保存安装包" }
                        dir.listFiles()?.filter { it != apk && it.extension == "apk" }?.forEach { it.delete() }
                        apk
                    } finally {
                        tmp.delete()
                    }
                }
                status.value = AppUpdateState.Ready(file, release.version)
            } catch (e: Exception) {
                status.value = AppUpdateState.Failed(e.localizedMessage ?: "下载失败")
            }
        }
    }
}
