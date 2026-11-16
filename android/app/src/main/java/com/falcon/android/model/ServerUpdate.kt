package com.falcon.android.model

import org.json.JSONObject

data class ServerUpdateCheck(
    val currentVersion: String,
    val latestVersion: String?,
    val available: Boolean,
    val platform: String,
    val error: String?
) {
    companion object {
        fun parse(json: JSONObject) = ServerUpdateCheck(
            json.getString("currentVersion"), json.optionalText("latestVersion"),
            json.getBoolean("available"), json.getString("platform"), json.optionalText("error")
        )
    }
}

data class ServerUpdateTask(val phase: String, val targetVersion: String?, val error: String?) {
    val running: Boolean get() = phase !in setOf("idle", "succeeded", "failed")
    val terminal: Boolean get() = phase == "succeeded" || phase == "failed"

    companion object {
        fun parse(json: JSONObject) = ServerUpdateTask(
            json.getString("phase"), json.optionalText("targetVersion"), json.optionalText("error")
        )
    }
}

data class NodeUpdateTarget(val hostId: String, val name: String, val address: String)
data class NodeStatus(val hostId: String?, val version: String?, val online: Boolean, val error: String?)

private fun JSONObject.optionalText(key: String): String? =
    if (isNull(key)) null else optString(key).takeIf { it.isNotBlank() }
