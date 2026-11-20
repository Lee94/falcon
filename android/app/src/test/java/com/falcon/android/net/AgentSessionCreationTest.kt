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

class AgentSessionCreationTest {
    private lateinit var server: MockWebServer

    @Before
    fun startServer() {
        server = MockWebServer()
        server.start()
    }

    @After
    fun stopServer() {
        server.shutdown()
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
    fun allAgentsUseTerminalSessions() = runBlocking {
        for (agent in listOf("claude", "codex", "grok", null)) {
            // 同时验证创建被拒绝时不丢失错误；不需要在 JVM 测试中加载 Android JNI。
            server.enqueue(MockResponse().setResponseCode(502).setBody("""{"error":"isolated creation rejected"}"""))
            val result = FalconApiClient.createSession(server.url("/").toString(), "node:remote/project", "  task  ", agent, creationTheme)
            assertEquals("isolated creation rejected", result.exceptionOrNull()?.message)
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/api/projects/node%3Aremote%2Fproject/sessions", request.path)
            assertTrue(!request.getHeader("Idempotency-Key").isNullOrBlank())
            val body = JSONObject(request.body.readUtf8())
            assertEquals("task", body.getString("name"))
            assertEquals(agent, body.opt("agent"))
            assertTrue(!body.has("agentMode"))
            assertEquals("dark", body.getString("appearance"))
            assertEquals("#102030", body.getString("background"))
            assertEquals("#abcdef", body.getString("foreground"))
        }
        assertEquals(4, server.requestCount)
    }

}
