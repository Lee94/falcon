package com.falcon.android.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class DashboardNodeStatusTest {
    private val dashboard = JsonParsers.parseDashboard(
        """{
            "servers": [
                {"key":"s:local","kind":"local"},
                {"key":"s:host:h1","kind":"host","hostId":"h1","standaloneId":"standalone:host:h1",
                    "standaloneSessions":[{"id":"standalone","hostId":"h1"}],
                    "folders":[{"project":{"id":"project","sessions":[{"id":"project-session","hostId":"h1"}]}}]},
                {"key":"s:host:h2","kind":"host","hostId":"h2",
                    "folders":[{"project":{"id":"other"}}]}
            ],
            "allSessions": [
                {"id":"local"}, {"id":"standalone","hostId":"h1"},
                {"id":"project-session","hostId":"h1"}, {"id":"archived","hostId":"h1"},
                {"id":"other-session","hostId":"h2"}
            ]
        }"""
    )

    @Test fun unavailableHostRemainsAsAHostRowAndItsCachedResourcesAreHidden() {
        val connecting = NodeStatus("h1", null, false, null)
        val offline = connecting.copy(error = "SSH connection lost")
        for (status in listOf(connecting, offline)) {
            val view = dashboard.withNodeStatuses(listOf(status))
            assertEquals(dashboard.servers.map { it.key }, view.servers.map { it.key })
            val host = view.servers[1]
            assertEquals(status, host.unavailableNode)
            assertEquals("standalone:host:h1", host.standaloneId)
            assertTrue(host.folders.isEmpty())
            assertTrue(host.standaloneSessions.isEmpty())
            assertEquals(dashboard.servers[2], view.servers[2])
            assertEquals(listOf("local", "other-session"), view.allSessions.map { it.id })
        }
    }

    @Test fun olderServersAndOnlineNodesKeepTheirLists() {
        assertSame(dashboard, dashboard.withNodeStatuses(emptyList()))
        val view = dashboard.withNodeStatuses(listOf(NodeStatus("h1", "0.4.12", true, null)))
        assertEquals(dashboard, view)
        assertNull(view.servers[1].unavailableNode)
    }
}
