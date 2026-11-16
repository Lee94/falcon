package com.falcon.android.model

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentModeTransitionTest {
    private val synced = AgentModeTransition().connected(requiresSnapshot = true)
        .snapshot(graphical = true, ready = true)

    @Test
    fun freshConnectionWaitsForAuthoritativeSnapshot() {
        val connected = AgentModeTransition().connected(requiresSnapshot = true)
        assertFalse(connected.synchronized)
        assertTrue(connected.snapshot(graphical = false, ready = true).synchronized)
    }

    @Test
    fun legacyServerCanConnectWithoutNewSnapshotCapability() {
        assertTrue(AgentModeTransition().connected(requiresSnapshot = false).synchronized)
    }

    @Test
    fun httpSuccessBeforeSnapshotKeepsInputsBlocked() {
        val awaiting = synced.start(graphical = false).finish(succeeded = true)
        assertTrue(awaiting.switching)
        assertTrue(awaiting.awaitingSnapshot)
        assertFalse(awaiting.snapshot(graphical = false, ready = true).switching)
    }

    @Test
    fun snapshotBeforeHttpSuccessKeepsRequestLocked() {
        val converting = synced.start(graphical = false).snapshot(graphical = false, ready = true)
        assertTrue(converting.switching)
        assertFalse(converting.finish(succeeded = true).switching)
    }

    @Test
    fun unknownSnapshotDoesNotFinishGuiConversion() {
        val converting = synced.start(graphical = true).finish(succeeded = true)
            .snapshot(graphical = true, ready = false)
        assertTrue(converting.switching)
        assertFalse(converting.snapshot(graphical = true, ready = true).switching)
    }

    @Test
    fun staleOppositeModeDoesNotReleaseConversionLock() {
        val converting = synced.start(graphical = false).finish(succeeded = true)
            .snapshot(graphical = true, ready = true)
        assertTrue(converting.switching)
    }

    @Test
    fun lateResponseDoesNotWaitForAlreadyObservedTargetAgain() {
        val converting = synced.start(graphical = false)
            .snapshot(graphical = false, ready = true)
            .snapshot(graphical = true, ready = true)
        assertFalse(converting.finish(succeeded = true).switching)
    }

    @Test
    fun disconnectInvalidatesOldModeBeforeReconnection() {
        val disconnected = synced.disconnected()
        assertFalse(disconnected.synchronized)
        assertFalse(disconnected.connected(requiresSnapshot = true).synchronized)
    }

    @Test
    fun reconnectAfterSuccessAcceptsCurrentModeFromOtherViewer() {
        val reconnecting = synced.start(graphical = false).finish(succeeded = true)
            .disconnected().connected(requiresSnapshot = true)
        assertTrue(reconnecting.switching)
        assertFalse(reconnecting.synchronized)
        val current = reconnecting.snapshot(graphical = true, ready = true)
        assertFalse(current.switching)
        assertTrue(current.synchronized)
    }

    @Test
    fun reconnectDuringRequestDoesNotAcceptAnEarlyOppositeSnapshot() {
        val reconnecting = synced.start(graphical = false)
            .disconnected().connected(requiresSnapshot = true)
            .snapshot(graphical = true, ready = true)
        assertTrue(reconnecting.finish(succeeded = true).switching)
    }

    @Test
    fun failedRequestInvalidatesOldModeUntilReconnection() {
        val uncertain = synced.start(graphical = false).finish(succeeded = false)
        assertFalse(uncertain.switching)
        assertFalse(uncertain.synchronized)
        val reconnecting = uncertain.disconnected().connected(requiresSnapshot = true)
        assertFalse(reconnecting.synchronized)
        assertTrue(reconnecting.snapshot(graphical = false, ready = true).synchronized)
    }

    @Test
    fun lostResponseAfterTargetSnapshotStillRequiresFreshSynchronization() {
        val uncertain = synced.start(graphical = false)
            .snapshot(graphical = false, ready = true).finish(succeeded = false)
        assertFalse(uncertain.switching)
        assertFalse(uncertain.synchronized)
    }
}
