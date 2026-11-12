package com.falcon.android.model

import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentViewModeTest {
    @Test
    fun transcriptMirrorKeepsRealTerminalVisible() {
        val state = JsonParsers.parseAgentSnapshot(JSONObject("""{"active":true,"terminalBacked":true}"""))!!
        assertTrue(state.terminalBacked)
        assertFalse(state.interactive)
    }

    @Test
    fun structuredSessionWithoutLegacyFlagUsesGui() {
        val state = JsonParsers.parseAgentSnapshot(JSONObject("""{"active":true}"""))!!
        assertFalse(state.terminalBacked)
        assertTrue(state.interactive)
    }

    @Test
    fun inactiveSnapshotReturnsToTerminal() {
        val state = JsonParsers.parseAgentSnapshot(JSONObject("""{"active":false,"terminalBacked":false}"""))!!
        assertFalse(state.interactive)
    }
}
