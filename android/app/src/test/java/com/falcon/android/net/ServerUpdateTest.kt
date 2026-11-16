package com.falcon.android.net

import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class ServerUpdateTest {
    private lateinit var server: MockWebServer

    @Before fun start() { server = MockWebServer(); server.start() }
    @After fun stop() { server.shutdown() }

    @Test fun currentServerCheckAndNodeInstallUseDifferentTargets() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"currentVersion":"1.0.0","latestVersion":"1.1.0","available":true,"platform":"linux","error":null}"""))
        val check = FalconApiClient.checkServerUpdate(server.url("/").toString(), null).getOrThrow()
        assertEquals("1.0.0", check.currentVersion)
        assertEquals("/api/system/update", server.takeRequest().path)
        server.enqueue(MockResponse().setBody("""{"phase":"downloading","targetVersion":"1.1.0","error":null}"""))
        val task = FalconApiClient.startServerUpdate(server.url("/").toString(), "host/id", "1.1.0").getOrThrow()
        assertTrue(task.running)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("/api/hosts/host%2Fid/node/system/update", request.path)
        assertEquals("1.1.0", JSONObject(request.body.readUtf8()).getString("version"))
    }

    @Test fun lostInstallResponseIsNotRetriedAndFinalStatusCanBeRead() = runBlocking {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST))
        assertTrue(FalconApiClient.startServerUpdate(server.url("/").toString(), null, "1.1.0").isFailure)
        server.enqueue(MockResponse().setBody("""{"phase":"succeeded","targetVersion":"1.1.0","error":null}"""))
        val task = FalconApiClient.serverUpdateStatus(server.url("/").toString(), null).getOrThrow()
        assertTrue(task.terminal)
        assertEquals(2, server.requestCount)
        assertEquals("POST", server.takeRequest().method)
        val status = server.takeRequest()
        assertEquals("GET", status.method)
        assertEquals("/api/system/update/status", status.path)
    }

    @Test fun unsupportedServerRejectsInstallAndMalformedStatusDoesNotLookIdle() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(404).setBody("""{"error":"unsupported"}"""))
        assertTrue(FalconApiClient.startServerUpdate(server.url("/").toString(), null, "1.1.0").exceptionOrNull()
            is FalconApiClient.UpdateRejectedException)
        server.enqueue(MockResponse().setBody("{}"))
        assertTrue(FalconApiClient.serverUpdateStatus(server.url("/").toString(), null).isFailure)
    }

    @Test fun newPhaseRemainsInProgressRatherThanUnlockingInstall() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"phase":"future","targetVersion":"1.1.0"}"""))
        val task = FalconApiClient.serverUpdateStatus(server.url("/").toString(), "n").getOrThrow()
        assertTrue(task.running)
        assertEquals("/api/hosts/n/node/system/update/status", server.takeRequest().path)
    }
}
