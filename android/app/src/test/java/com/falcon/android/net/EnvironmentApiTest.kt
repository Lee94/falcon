package com.falcon.android.net

import com.falcon.android.model.RepoBranch
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EnvironmentApiTest {
    @Test fun threadCreationAcceptsTheNewProjectResponse() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""{"id":"thread","threadEnvironmentId":"node:host/env"}"""))
            FalconApiClient.createThread(server.url("/").toString(), "node:host/env", "thread", "task").getOrThrow()
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/api/projects/node%3Ahost%2Fenv/threads", request.path)
            assertEquals("task", JSONObject(request.body.readUtf8()).getString("task"))
        }
    }

    @Test fun branchLookupDistinguishesOrdinaryDirectoriesAndGitRepositories() = runBlocking {
        MockWebServer().use { server ->
            val baseUrl = server.url("/").toString()
            val directory = "/user files/A+B#C"
            server.enqueue(MockResponse().setBody("null"))
            assertNull(FalconApiClient.fetchEnvironmentBranches(baseUrl, null, directory).getOrThrow())
            val local = server.takeRequest().requestUrl!!
            assertEquals("/api/environments/branches", local.encodedPath)
            assertEquals(directory, local.queryParameter("path"))
            assertNull(local.queryParameter("hostId"))

            server.enqueue(MockResponse().setBody(
                """[{"name":"origin/main","remote":true,"localName":"main","head":false,"suggestedDir":"/repo-main","dirOccupied":false}]"""
            ))
            assertEquals(listOf(RepoBranch("origin/main", false)),
                FalconApiClient.fetchEnvironmentBranches(baseUrl, "host:a+b", directory).getOrThrow())
            assertEquals("host:a+b", server.takeRequest().requestUrl!!.queryParameter("hostId"))
        }
    }

    @Test fun environmentRequestsUseDirectoryForNewEnvsAndKeepLegacyRepoPayloads() = runBlocking {
        MockWebServer().use { server ->
            val baseUrl = server.url("/").toString()
            for (branch in listOf(null, "origin/main")) {
                server.enqueue(MockResponse().setBody("{}"))
                FalconApiClient.saveEnvironment(baseUrl, "host", null, "env", "/work", branch).getOrThrow()
                val request = server.takeRequest()
                val payload = JSONObject(request.body.readUtf8())
                assertEquals("POST", request.method)
                assertEquals("/api/environments", request.path)
                assertTrue(!request.getHeader("Idempotency-Key").isNullOrBlank())
                assertEquals("ssh", payload.getString("type"))
                assertEquals("host", payload.getString("hostId"))
                assertEquals("", payload.getString("environmentContext"))
                assertEquals("/work", payload.getString("workingDir"))
                assertFalse(payload.has("repos"))
                assertEquals(branch, payload.opt("defaultWorktreeBranch"))
            }

            server.enqueue(MockResponse().setBody("{}"))
            FalconApiClient.saveEnvironment(baseUrl, null, "env", "renamed").getOrThrow()
            val edit = server.takeRequest()
            assertEquals("PUT", edit.method)
            assertEquals("/api/projects/env", edit.path)
            val editPayload = JSONObject(edit.body.readUtf8())
            assertFalse(editPayload.has("workingDir"))
            assertFalse(editPayload.has("defaultWorktreeBranch"))

            server.enqueue(MockResponse().setBody("{}"))
            FalconApiClient.saveEnvironment(baseUrl, null, null, "legacy", legacyRepos = listOf("/one", "/two")).getOrThrow()
            val legacy = JSONObject(server.takeRequest().body.readUtf8())
            assertEquals("/two", legacy.getJSONArray("repos").getString(1))
            assertFalse(legacy.has("workingDir"))
        }
    }
}
