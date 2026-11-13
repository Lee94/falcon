package com.falcon.android.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import org.json.JSONObject

private object SharedStrings {
    private var loaded: JSONObject? = null
    fun load(context: android.content.Context): JSONObject = loaded ?: context.assets
        .open("zh-CN.json").bufferedReader().use { JSONObject(it.readText()) }.also { loaded = it }
}

/** 文案来自 falcon-ui/locales/zh-CN.json，Gradle 将原文件作为资产打包。 */
@Composable
internal fun t(key: String, vararg values: Pair<String, String>): String {
    var node: Any = SharedStrings.load(LocalContext.current)
    for (part in key.split('.')) node = (node as? JSONObject)?.opt(part) ?: return key
    var text = node as? String ?: return key
    for ((name, value) in values) text = text.replace("%{$name}", value)
    return text
}
