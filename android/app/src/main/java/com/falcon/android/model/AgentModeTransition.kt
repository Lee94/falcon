package com.falcon.android.model

// HTTP 只确认转换请求完成；输入必须等同一连接收到目标模式的可交互状态。
// 重连发生在 HTTP 完成之后时，新连接的首个快照就是当前事实，可包含其它 Viewer 的后续转换。
data class AgentModeTransition(
    val synchronized: Boolean = false,
    val requestInFlight: Boolean = false,
    val targetGraphical: Boolean? = null,
    val targetObserved: Boolean = false,
    val acceptCurrentMode: Boolean = false
) {
    val switching: Boolean get() = requestInFlight || targetGraphical != null
    val awaitingSnapshot: Boolean get() = !requestInFlight && targetGraphical != null

    fun connected(requiresSnapshot: Boolean): AgentModeTransition = copy(
        synchronized = !requiresSnapshot,
        acceptCurrentMode = awaitingSnapshot
    )

    fun disconnected(): AgentModeTransition = copy(synchronized = false, acceptCurrentMode = false)

    fun start(graphical: Boolean): AgentModeTransition = copy(
        requestInFlight = true,
        targetGraphical = graphical,
        targetObserved = false,
        acceptCurrentMode = false
    )

    fun snapshot(graphical: Boolean, ready: Boolean): AgentModeTransition {
        val observed = targetObserved || (ready && targetGraphical != null &&
            (graphical == targetGraphical || acceptCurrentMode))
        return copy(
            synchronized = true,
            targetObserved = observed,
            targetGraphical = if (observed && !requestInFlight) null else targetGraphical
        )
    }

    fun finish(succeeded: Boolean): AgentModeTransition = copy(
        requestInFlight = false,
        // 请求失败可能只是响应丢失，服务端实际已转换；旧快照不能重新开放输入，须重连同步。
        synchronized = synchronized && succeeded,
        targetGraphical = if (!succeeded || targetObserved) null else targetGraphical
    )
}
