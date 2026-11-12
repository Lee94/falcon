package com.falcon.android.net

import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class AgentModeTest {
    private lateinit var server: MockWebServer
    private val capabilities = """{"preservesProcess":true,"interactiveGui":true,"interactiveTerminal":true}"""

    @Before
    fun startServer() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun stopServer() {
        server.shutdown()
    }

    private fun switch(graphical: Boolean) = runBlocking {
        FalconApiClient.switchAgentMode(server.url("/").toString(), "test-session", graphical)
    }

    @Test
    fun newServerRequiresSnapshotBeforeOpeningSessionInput() {
        server.enqueue(MockResponse().setBody("""{"agentModeSnapshot":true}"""))
        val supported = runBlocking {
            FalconApiClient.agentModeSnapshotSupported(server.url("/").toString(), "test-session").getOrThrow()
        }
        assertTrue(supported)
        assertEquals("GET", server.takeRequest().method)
    }

    @Test
    fun legacyServerWithoutSnapshotCapabilityRemainsCompatible() {
        server.enqueue(MockResponse().setBody("""{"interactiveGui":true}"""))
        val supported = runBlocking {
            FalconApiClient.agentModeSnapshotSupported(server.url("/").toString(), "test-session").getOrThrow()
        }
        assertEquals(false, supported)
    }

    @Test
    fun legacyServerWithoutCapabilityRouteRemainsCompatible() {
        server.enqueue(MockResponse().setResponseCode(404))
        val supported = runBlocking {
            FalconApiClient.agentModeSnapshotSupported(server.url("/").toString(), "test-session").getOrThrow()
        }
        assertEquals(false, supported)
    }

    @Test
    fun guiRequiresCapabilityAndPostsExplicitControlMode() {
        server.enqueue(MockResponse().setBody(capabilities))
        server.enqueue(MockResponse().setBody("""{"ok":true}"""))

        assertTrue(switch(graphical = true).isSuccess)
        assertEquals(2, server.requestCount)
        val probe = server.takeRequest()
        assertEquals("GET", probe.method)
        assertEquals("/api/sessions/test-session/agent-mode", probe.path)
        val conversion = server.takeRequest()
        assertEquals("POST", conversion.method)
        assertEquals(probe.path, conversion.path)
        assertEquals("gui-control", JSONObject(conversion.body.readUtf8()).getString("mode"))
    }

    @Test
    fun tuiRequiresItsOwnCapabilityAndPostsTerminalControl() {
        server.enqueue(MockResponse().setBody(capabilities))
        server.enqueue(MockResponse().setBody("""{"ok":true}"""))

        assertTrue(switch(graphical = false).isSuccess)
        assertEquals(2, server.requestCount)
        assertEquals("GET", server.takeRequest().method)
        val conversion = server.takeRequest()
        assertEquals("POST", conversion.method)
        assertEquals("terminal-control", JSONObject(conversion.body.readUtf8()).getString("mode"))
    }

    @Test
    fun mirrorOnlyServerNeverReceivesConversionPost() {
        for (graphical in listOf(true, false)) {
            server.enqueue(MockResponse().setBody("""{"preservesProcess":true}"""))
            assertTrue(switch(graphical).exceptionOrNull() is AgentModeUnsupportedException)
        }
        assertEquals(2, server.requestCount)
        repeat(2) { assertEquals("GET", server.takeRequest().method) }
    }

    @Test
    fun guiCapabilityDoesNotAuthorizeReturningToTui() {
        server.enqueue(MockResponse().setBody("""{"interactiveGui":true}"""))
        assertTrue(switch(graphical = false).exceptionOrNull() is AgentModeUnsupportedException)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun capabilityMustBeBooleanTrue() {
        server.enqueue(MockResponse().setBody("""{"interactiveGui":"true"}"""))
        assertTrue(switch(graphical = true).exceptionOrNull() is AgentModeUnsupportedException)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun missingCapabilityRouteNeverReceivesConversionPost() {
        for (status in listOf(404, 405)) {
            server.enqueue(MockResponse().setResponseCode(status))
            assertTrue(switch(graphical = true).exceptionOrNull() is AgentModeUnsupportedException)
        }
        assertEquals(2, server.requestCount)
        repeat(2) { assertEquals("GET", server.takeRequest().method) }
    }

    @Test
    fun capabilityFailurePreservesServerErrorWithoutPosting() {
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"error":"service unavailable"}"""))
        assertEquals("service unavailable", switch(graphical = true).exceptionOrNull()?.message)
        assertEquals(1, server.requestCount)
    }

    @Test
    fun rejectedConversionPreservesServerReason() {
        server.enqueue(MockResponse().setBody(capabilities))
        server.enqueue(MockResponse().setResponseCode(409).setBody("""{"error":"agent is running"}"""))
        assertEquals("agent is running", switch(graphical = false).exceptionOrNull()?.message)
        assertEquals(2, server.requestCount)
    }
}
