package com.falcon.android.bridge

object FalconNative {
    init {
        System.loadLibrary("falcon_android")
    }

    const val SNAPSHOT_HEADER_INTS = 16
    const val SNAPSHOT_CELL_INTS = 4

    const val CELL_FLAG_BOLD = 1 shl 0
    const val CELL_FLAG_ITALIC = 1 shl 1
    const val CELL_FLAG_UNDERLINE = 1 shl 2
    const val CELL_FLAG_STRIKEOUT = 1 shl 3
    const val CELL_FLAG_WIDE_CHAR = 1 shl 4
    const val CELL_FLAG_WIDE_CHAR_SPACER = 1 shl 5
    const val CELL_FLAG_SELECTED = 1 shl 6
    const val CELL_FLAG_DEFAULT_BG = 1 shl 7
    const val CELL_FLAG_HIDDEN = 1 shl 8

    const val MODE_BIT_ALT_SCREEN = 1 shl 0
    const val MODE_BIT_APP_CURSOR = 1 shl 1
    const val MODE_BIT_BRACKETED_PASTE = 1 shl 2
    const val MODE_BIT_FOCUS_IN_OUT = 1 shl 3
    const val MODE_BIT_MOUSE_ACTIVE = 1 shl 4

    @JvmStatic
    external fun createSession(cols: Int, rows: Int, durable: Boolean, themeName: String): Long

    @JvmStatic
    external fun destroySession(handle: Long)

    @JvmStatic
    external fun setSessionDurable(handle: Long, durable: Boolean)

    @JvmStatic
    external fun setSessionTheme(handle: Long, themeName: String): String

    @JvmStatic
    external fun handshakeMessages(handle: Long, cols: Int, rows: Int, visible: Boolean): String

    @JvmStatic
    external fun onViewportResize(handle: Long, cols: Int, rows: Int): String

    @JvmStatic
    external fun onServerSize(handle: Long, cols: Int, rows: Int, owner: Boolean)

    @JvmStatic
    external fun onBinaryFrame(handle: Long, frame: ByteArray): String

    @JvmStatic
    external fun packRenderSnapshot(handle: Long): IntArray

    @JvmStatic
    external fun packRenderSnapshotInto(handle: Long, lastSeq: Long, outArray: IntArray): Long

    @JvmStatic
    external fun encodeKey(
        handle: Long,
        key: String,
        ctrl: Boolean,
        alt: Boolean,
        shift: Boolean
    ): String

    @JvmStatic
    external fun encodeText(handle: Long, text: String): String

    @JvmStatic
    external fun encodePaste(handle: Long, text: String): String

    @JvmStatic
    external fun onScrollGesture(
        handle: Long,
        deltaYPixels: Double,
        cellHeight: Double,
        row: Int,
        col: Int
    ): String

    @JvmStatic
    external fun scrollToBottom(handle: Long)

    @JvmStatic
    external fun resetScrollGesture(handle: Long)

    @JvmStatic
    external fun scrollToOffset(handle: Long, offset: Int): Boolean

    @JvmStatic
    external fun onMouseEvent(
        handle: Long,
        action: Int,
        row: Int,
        col: Int,
        pixelX: Int,
        pixelY: Int
    ): String

    @JvmStatic
    external fun startSelection(handle: Long, kind: Int, row: Int, col: Int, rightHalf: Boolean)

    @JvmStatic
    external fun updateSelection(handle: Long, row: Int, col: Int, rightHalf: Boolean)

    @JvmStatic
    external fun clearSelection(handle: Long)

    @JvmStatic
    external fun selectionText(handle: Long): String

    @JvmStatic
    external fun parseServerMessage(rawJson: String): String

    @JvmStatic
    external fun buildDashboardView(
        projectsJson: String,
        sessionsJson: String,
        hostsJson: String
    ): String

    @JvmStatic
    external fun exportThemeTokens(themeName: String): String

    @JvmStatic
    external fun listAvailableThemes(query: String): String
}
