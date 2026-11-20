package com.falcon.android.ui

import com.google.vtfx.session.TerminalSession
import org.json.JSONObject

/** Falcon input frames have a 1 MiB limit including JSON escapes. */
internal fun sendTerminalInput(bytes: ByteArray, send: (String) -> Boolean) {
    val data = bytes.decodeToString()
    var start = 0
    while (start < data.length) {
        var end = minOf(start + 65536, data.length)
        if (end < data.length && data[end - 1].isHighSurrogate()) end--
        val message = JSONObject().put("type", "input").put("data", data.substring(start, end))
        if (!send(message.toString())) return
        start = end
    }
}

/** The type byte is defined by falcon-proto. Replay replaces both parser state and scrollback. */
internal fun applyTerminalFrame(
    current: TerminalSession,
    frame: ByteArray,
    newSession: () -> TerminalSession,
): TerminalSession {
    val target = when (frame.firstOrNull()?.toInt()) {
        1 -> current
        2 -> newSession()
        else -> return current
    }
    target.feed(frame, 1)
    return target
}
