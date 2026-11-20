package com.falcon.android.ui

import com.google.vtfx.session.TerminalSession
import com.google.vtfx.session.TerminalSessionListener
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class VtfxSessionTest {
    @Test fun durableSessionsLeaveCursorQueriesToHostAndStillSendInput() {
        for (durable in listOf(true, false)) {
            val sent = mutableListOf<String>()
            val session = TerminalSession(connection = { sent.add(it.decodeToString()) },
                answerHostQueries = !durable)
            session.feed("\u001b[6n".encodeToByteArray())
            assertEquals(if (durable) "" else "\u001b[1;1R", sent.joinToString(""))
            session.sendText("中文")
            assertTrue(sent.joinToString("").endsWith("中文"))
        }
    }

    @Test fun followingRemoteGridStillReportsLocalViewport() {
        val reports = mutableListOf<Pair<Int, Int>>()
        val session = TerminalSession(connection = {}, listener = object : TerminalSessionListener {
            override fun onResize(columns: Int, rows: Int) { reports.add(columns to rows) }
        })
        session.followGrid(120 to 40)
        session.resize(64, 22)
        assertEquals(120 to 40, session.snapshot().let { it.columns to it.rows })
        assertEquals(listOf(64 to 22), reports)
        session.followGrid(null)
        assertEquals(64 to 22, session.snapshot().let { it.columns to it.rows })
        assertEquals(1, reports.size)
    }

    @Test fun replayReplacesPartialEscapeSequenceAndOldScrollback() {
        fun create() = TerminalSession(connection = {}, rows = 2)
        val old = create()
        applyTerminalFrame(old, byteArrayOf(1) + "one\r\ntwo\r\nthree\u001b[".encodeToByteArray(), ::create)
        assertTrue(old.scrollbackSize > 0)
        val replay = applyTerminalFrame(old, byteArrayOf(2) + "fresh".encodeToByteArray(), ::create)
        assertEquals(0, replay.scrollbackSize)
        val live = applyTerminalFrame(replay, byteArrayOf(1) + "!".encodeToByteArray(), ::create)
        assertSame(replay, live)
        assertEquals("fresh!", live.snapshot().line(0).text().trimEnd())
    }

    @Test fun longInputPreservesUnicodeAcrossJsonFrames() {
        val input = "\u001b".repeat(65535) + "😀中文"
        val parts = mutableListOf<String>()
        sendTerminalInput(input.encodeToByteArray()) { json ->
            assertTrue(json.encodeToByteArray().size < 1024 * 1024)
            parts.add(JSONObject(json).getString("data"))
        }
        assertEquals(2, parts.size)
        assertEquals(input, parts.joinToString(""))
    }
}
