package com.falcon.android.net

import androidx.compose.ui.graphics.Color
import com.falcon.android.ui.FalconThemeTokens
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

    private val creationTheme = FalconThemeTokens(
        name = "Test", isDark = true, appearance = "dark",
        backgroundHex = "#102030", foregroundHex = "#abcdef",
        app = Color.Unspecified, background = Color.Unspecified, foreground = Color.Unspecified,
        card = Color.Unspecified, cardForeground = Color.Unspecified,
        primary = Color.Unspecified, primaryForeground = Color.Unspecified,
        secondary = Color.Unspecified, secondaryForeground = Color.Unspecified,
        muted = Color.Unspecified, mutedForeground = Color.Unspecified,
        accent = Color.Unspecified, accentForeground = Color.Unspecified,
        tint = Color.Unspecified, tintForeground = Color.Unspecified, tintStrong = Color.Unspecified,
        destructive = Color.Unspecified, success = Color.Unspecified, warning = Color.Unspecified,
        sidebar = Color.Unspecified, sidebarForeground = Color.Unspecified,
        terminalBackground = Color.Unspecified, terminalForeground = Color.Unspecified
    )

    @Test
    fun detectedAgentsUseTheServerArrayAndProjectScope() = runBlocking {
        server.enqueue(MockResponse().setBody("""["codex","claude","omp"]"""))
        assertEquals(listOf("codex", "claude", "omp"),
            FalconApiClient.fetchDetectedAgents(server.url("/").toString(), "node:remote/project").getOrThrow())
        assertEquals("node:remote/project", server.takeRequest().requestUrl!!.queryParameter("projectId"))
        server.enqueue(MockResponse().setBody("[]"))
        assertTrue(FalconApiClient.fetchDetectedAgents(server.url("/").toString(), "standalone:local").getOrThrow().isEmpty())
    }

    @Test
    fun claudeAndCodexDefaultToGuiWhileShellAndOtherCliKeepTerminal() = runBlocking {
        for (agent in listOf("claude", "codex", "grok", null)) {
            val graphical = agent == "claude" || agent == "codex"
            if (graphical) server.enqueue(MockResponse().setBody("""{"createGui":true}"""))
            // 同时验证创建被拒绝时不丢失错误；不需要在 JVM 测试中加载 Android JNI。
            server.enqueue(MockResponse().setResponseCode(502).setBody("""{"error":"isolated creation rejected"}"""))
            val result = FalconApiClient.createSession(server.url("/").toString(), "node:remote/project", "  task  ", agent, creationTheme)
            assertEquals("isolated creation rejected", result.exceptionOrNull()?.message)
            if (graphical) {
                val probe = server.takeRequest()
                assertEquals("GET", probe.method)
                assertEquals("/api/projects/node%3Aremote%2Fproject/sessions", probe.path)
            }
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/api/projects/node%3Aremote%2Fproject/sessions", request.path)
            assertTrue(!request.getHeader("Idempotency-Key").isNullOrBlank())
            val body = JSONObject(request.body.readUtf8())
            assertEquals("task", body.getString("name"))
            assertEquals(agent, body.opt("agent"))
            assertEquals(if (graphical) "gui" else null, body.opt("agentMode"))
            assertEquals("dark", body.getString("appearance"))
            assertEquals("#102030", body.getString("background"))
            assertEquals("#abcdef", body.getString("foreground"))
        }
        assertEquals(6, server.requestCount)
    }

    @Test
    fun legacyServerCannotSilentlyCreateTerminalInsteadOfGui() = runBlocking {
        for (response in listOf(
            MockResponse().setBody(capabilities),
            MockResponse().setBody("""{"createGui":"true"}"""),
            MockResponse().setResponseCode(404),
            MockResponse().setResponseCode(405)
        )) {
            server.enqueue(response)
            val result = FalconApiClient.createSession(server.url("/").toString(), "standalone:local", "", "claude", creationTheme)
            assertTrue(result.exceptionOrNull() is AgentModeUnsupportedException)
            assertEquals("GET", server.takeRequest().method)
        }
        assertEquals(4, server.requestCount)
    }

    @Test
    fun codexWaitsForModeSnapshotOnlyOnServersThatProvideIt() = runBlocking {
        for ((body, expected) in listOf(
            """{"agentModeSnapshot":true}""" to false,
            """{"agentModeSnapshot":true,"createGui":true}""" to true
        )) {
            server.enqueue(MockResponse().setBody(body))
            assertEquals(expected, FalconApiClient.agentModeSnapshotSupported(
                server.url("/").toString(), "codex-session", "codex").getOrThrow())
        }
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
