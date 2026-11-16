package com.falcon.android.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import android.text.InputType
import android.util.TypedValue
import android.view.GestureDetector
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.OverScroller
import com.falcon.android.bridge.FalconNative
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

class TerminalCanvasView(context: Context) : View(context) {

    var sessionHandle: Long = 0L
        set(value) {
            if (field != value) {
                field = value
                lastSnapshotSeq = 0L
                scroller.forceFinished(true)
                invalidate()
            }
        }

    /** 发出一条或多条已序列化的 ClientMessage JSON 字符串到 WebSocket */
    var onSendWsMessages: ((List<String>) -> Unit)? = null

    /** 当视口行列数计算变化时回调 `(cols, rows)` */
    var onViewportGridChanged: ((Int, Int) -> Unit)? = null

    /** 当终端字号变化（如双指缩放）时回调 */
    var onFontSizeChanged: ((Float) -> Unit)? = null

    /** 当存在或清除选区文本时回调 */
    var onSelectionChanged: ((String?) -> Unit)? = null

    /** 当是否处于历史回滚状态或尺寸跟随状态变化时回调 `(isScrolledUp, isOwner, gridCols, gridRows)` */
    var onRenderStateUpdated: ((Boolean, Boolean, Int, Int) -> Unit)? = null

    /** 扩展栏上锁定的修饰键查询与消费 */
    var consumeStickyModifiers: (() -> Triple<Boolean, Boolean, Boolean>)? = null

    var outsideAreaColor: Int = 0xFF050505.toInt()

    private val density = resources.displayMetrics.density

    private var fontSizeSp: Float = 12.0f
    private var cellWidth: Float = 16f
    private var cellHeight: Float = 32f
    private var fontBaselineOffset: Float = 24f

    private var viewportCols: Int = 0
    private var viewportRows: Int = 0

    private val paddingX = 3f * density
    private val paddingY = 2f * density

    private val regularTypeface: Typeface
    private val boldTypeface: Typeface
    private val italicTypeface: Typeface
    private val boldItalicTypeface: Typeface

    private val bgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.FILL
    }

    private val textPaint = Paint(
        Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG or Paint.LINEAR_TEXT_FLAG
    )

    private val cursorPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val scrollbarPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.FILL
        color = 0x66FFFFFF
    }

    private val rectBuffer = RectF()
    private val charBuffer = CharArray(2)
    private var rowCharBuffer = CharArray(512)

    private var snapshotBuffer = IntArray(
        FalconNative.SNAPSHOT_HEADER_INTS + 120 * 60 * FalconNative.SNAPSHOT_CELL_INTS
    )
    private var lastSnapshotSeq: Long = 0L
    private var hasValidSnapshot: Boolean = false

    private val scroller = OverScroller(context)
    private var lastFlingY: Int = 0
    private var lastTouchX: Float = 0f
    private var lastTouchY: Float = 0f
    private var multiTouchActive: Boolean = false
    private var isDraggingScrollbar: Boolean = false

    private var isSelectingText: Boolean = false
    private var lastReportedScrolledUp: Boolean = false
    private var lastReportedOwner: Boolean = true
    private var lastReportedGridCols: Int = 0
    private var lastReportedGridRows: Int = 0

    // 当处于非 owner 跟随尺寸模式时的当前绘制缩放与偏移
    private var activeScale: Float = 1f
    private var activePanX: Float = 0f
    private var activePanY: Float = 0f

    private var lastPaintStyle: Int = -1
    private var lastPaintColor: Int = 0

    init {
        isFocusable = true
        isFocusableInTouchMode = true

        regularTypeface = loadAssetFont("fonts/TX-02-Regular.ttf", Typeface.MONOSPACE)
        boldTypeface = loadAssetFont("fonts/TX-02-Bold.ttf", Typeface.create(regularTypeface, Typeface.BOLD))
        italicTypeface = loadAssetFont("fonts/TX-02-Oblique.ttf", Typeface.create(regularTypeface, Typeface.ITALIC))
        boldItalicTypeface = loadAssetFont(
            "fonts/TX-02-BoldOblique.ttf",
            Typeface.create(regularTypeface, Typeface.BOLD_ITALIC)
        )

        updateFontMetrics()
    }

    private fun loadAssetFont(path: String, fallback: Typeface): Typeface {
        return try {
            Typeface.createFromAsset(context.assets, path)
        } catch (_: Exception) {
            fallback
        }
    }

    fun setFontSizeSp(sp: Float) {
        val clamped = sp.coerceIn(8.5f, 28f)
        if (abs(clamped - fontSizeSp) < 0.1f) return
        fontSizeSp = clamped
        updateFontMetrics()
        recomputeGridFromSize(width, height)
        onFontSizeChanged?.invoke(fontSizeSp)
        invalidate()
    }

    fun getFontSizeSp(): Float = fontSizeSp

    private fun updateFontMetrics() {
        textPaint.typeface = regularTypeface
        lastPaintStyle = 0
        textPaint.textSize = TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_SP,
            fontSizeSp,
            resources.displayMetrics
        )
        val fm = textPaint.fontMetrics
        cellWidth = max(1f, textPaint.measureText("M"))
        cellHeight = max(1f, (fm.descent - fm.ascent + fm.leading))
        fontBaselineOffset = -fm.ascent
    }

    private fun applyTextPaint(styleBits: Int, fgArgb: Int) {
        if (styleBits != lastPaintStyle) {
            lastPaintStyle = styleBits
            val isBold = (styleBits and FalconNative.CELL_FLAG_BOLD) != 0
            val isItalic = (styleBits and FalconNative.CELL_FLAG_ITALIC) != 0
            textPaint.typeface = when {
                isBold && isItalic -> boldItalicTypeface
                isBold -> boldTypeface
                isItalic -> italicTypeface
                else -> regularTypeface
            }
        }
        if (fgArgb != lastPaintColor) {
            lastPaintColor = fgArgb
            textPaint.color = fgArgb
        }
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        recomputeGridFromSize(w, h)
    }

    private fun recomputeGridFromSize(w: Int, h: Int) {
        if (w <= 0 || h <= 0) return
        val availW = max(1f, w - paddingX * 2f)
        val availH = max(1f, h - paddingY * 2f)
        val newCols = max(2, floor(availW / cellWidth).toInt())
        val newRows = max(1, floor(availH / cellHeight).toInt())
        if (newCols != viewportCols || newRows != viewportRows) {
            viewportCols = newCols
            viewportRows = newRows
            if (sessionHandle != 0L) {
                val resizeJson = FalconNative.onViewportResize(sessionHandle, newCols, newRows)
                if (resizeJson.isNotEmpty()) {
                    onSendWsMessages?.invoke(listOf(resizeJson))
                }
            }
            onViewportGridChanged?.invoke(newCols, newRows)
        }
    }

    fun currentViewportGrid(): Pair<Int, Int> =
        if (viewportCols >= 2 && viewportRows >= 1) viewportCols to viewportRows else 80 to 24

    fun requestRender() {
        postInvalidateOnAnimation()
    }

    private var softKeyboardShown = false

    fun showSoftKeyboard() {
        requestFocus()
        val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
        imm?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        softKeyboardShown = true
    }

    fun hideSoftKeyboard() {
        val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
        imm?.hideSoftInputFromWindow(windowToken, 0)
        softKeyboardShown = false
    }

    fun toggleSoftKeyboard() {
        if (softKeyboardShown) {
            hideSoftKeyboard()
        } else {
            showSoftKeyboard()
        }
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // 使用普通文本类型；密码类型会让部分系统切换到安全输入法。
        outAttrs.inputType =
            InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions =
            EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                EditorInfo.IME_FLAG_NO_FULLSCREEN or
                EditorInfo.IME_ACTION_NONE

        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                val str = text?.toString() ?: return true
                if (str.isNotEmpty()) {
                    handleCommittedText(str)
                }
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                if (beforeLength > 0) {
                    repeat(beforeLength) {
                        sendKey("backspace", ctrl = false, alt = false, shift = false)
                    }
                    return true
                }
                return super.deleteSurroundingText(beforeLength, afterLength)
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                if (event.action == KeyEvent.ACTION_DOWN) {
                    if (handleAndroidKeyDown(event.keyCode, event)) {
                        return true
                    }
                }
                return super.sendKeyEvent(event)
            }
        }
    }

    private fun handleCommittedText(text: String) {
        if (sessionHandle == 0L) return
        val (stickyCtrl, stickyAlt, stickyShift) =
            consumeStickyModifiers?.invoke() ?: Triple(false, false, false)
        if ((stickyCtrl || stickyAlt) && text.length == 1) {
            sendKey(text, ctrl = stickyCtrl, alt = stickyAlt, shift = stickyShift)
            return
        }
        val chunksJson = FalconNative.encodeText(sessionHandle, text)
        val list = com.falcon.android.model.JsonParsers.parseStringArray(chunksJson)
        if (list.isNotEmpty()) {
            onSendWsMessages?.invoke(list)
            requestRender()
        }
    }

    fun sendKey(key: String, ctrl: Boolean = false, alt: Boolean = false, shift: Boolean = false) {
        if (sessionHandle == 0L) return
        val chunksJson = FalconNative.encodeKey(sessionHandle, key, ctrl, alt, shift)
        val list = com.falcon.android.model.JsonParsers.parseStringArray(chunksJson)
        if (list.isNotEmpty()) {
            onSendWsMessages?.invoke(list)
            requestRender()
        }
    }

    fun sendPaste(text: String) {
        if (sessionHandle == 0L || text.isEmpty()) return
        val chunksJson = FalconNative.encodePaste(sessionHandle, text)
        val list = com.falcon.android.model.JsonParsers.parseStringArray(chunksJson)
        if (list.isNotEmpty()) {
            onSendWsMessages?.invoke(list)
            requestRender()
        }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (handleAndroidKeyDown(keyCode, event)) {
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    private fun handleAndroidKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (sessionHandle == 0L) return false
        val (stickyCtrl, stickyAlt, stickyShift) =
            consumeStickyModifiers?.invoke() ?: Triple(false, false, false)
        val ctrl = event.isCtrlPressed || stickyCtrl
        val alt = event.isAltPressed || stickyAlt
        val shift = event.isShiftPressed || stickyShift

        val mappedKey: String? = when (keyCode) {
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> "enter"
            KeyEvent.KEYCODE_DEL -> "backspace"
            KeyEvent.KEYCODE_FORWARD_DEL -> "delete"
            KeyEvent.KEYCODE_TAB -> "tab"
            KeyEvent.KEYCODE_ESCAPE -> "escape"
            KeyEvent.KEYCODE_DPAD_UP -> "up"
            KeyEvent.KEYCODE_DPAD_DOWN -> "down"
            KeyEvent.KEYCODE_DPAD_LEFT -> "left"
            KeyEvent.KEYCODE_DPAD_RIGHT -> "right"
            KeyEvent.KEYCODE_MOVE_HOME -> "home"
            KeyEvent.KEYCODE_MOVE_END -> "end"
            KeyEvent.KEYCODE_PAGE_UP -> "pageup"
            KeyEvent.KEYCODE_PAGE_DOWN -> "pagedown"
            KeyEvent.KEYCODE_INSERT -> "insert"
            KeyEvent.KEYCODE_SPACE -> if (ctrl) "space" else null
            KeyEvent.KEYCODE_F1 -> "f1"
            KeyEvent.KEYCODE_F2 -> "f2"
            KeyEvent.KEYCODE_F3 -> "f3"
            KeyEvent.KEYCODE_F4 -> "f4"
            KeyEvent.KEYCODE_F5 -> "f5"
            KeyEvent.KEYCODE_F6 -> "f6"
            KeyEvent.KEYCODE_F7 -> "f7"
            KeyEvent.KEYCODE_F8 -> "f8"
            KeyEvent.KEYCODE_F9 -> "f9"
            KeyEvent.KEYCODE_F10 -> "f10"
            KeyEvent.KEYCODE_F11 -> "f11"
            KeyEvent.KEYCODE_F12 -> "f12"
            else -> null
        }

        if (mappedKey != null) {
            sendKey(mappedKey, ctrl = ctrl, alt = alt, shift = shift)
            return true
        }

        val unicode = event.getUnicodeChar(event.metaState and KeyEvent.META_CTRL_MASK.inv())
        if (unicode != 0) {
            val ch = unicode.toChar().toString()
            if (ctrl || alt) {
                sendKey(ch, ctrl = ctrl, alt = alt, shift = shift)
            } else {
                handleCommittedText(ch)
            }
            return true
        }
        return false
    }

    private fun screenToCell(x: Float, y: Float, cols: Int, rows: Int): Triple<Int, Int, Boolean> {
        val localX = ((x - paddingX + activePanX) / activeScale).coerceAtLeast(0f)
        val localY = ((y - paddingY + activePanY) / activeScale).coerceAtLeast(0f)
        val col = floor(localX / cellWidth).toInt().coerceIn(0, max(0, cols - 1))
        val row = floor(localY / cellHeight).toInt().coerceIn(0, max(0, rows - 1))
        val rightHalf = ((localX / cellWidth) % 1f) >= 0.5f
        return Triple(row, col, rightHalf)
    }

    private val scaleDetector = ScaleGestureDetector(
        context,
        object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
            override fun onScale(detector: ScaleGestureDetector): Boolean {
                val factor = detector.scaleFactor
                if (abs(factor - 1f) > 0.015f) {
                    setFontSizeSp(fontSizeSp * factor)
                }
                return true
            }
        }
    )

    private fun dispatchScrollDelta(
        deltaY: Float,
        touchX: Float,
        touchY: Float,
        isFling: Boolean = false
    ): Boolean {
        val handle = sessionHandle
        if (handle == 0L) return true
        val cols = if (hasValidSnapshot) snapshotBuffer[0] else viewportCols
        val rows = if (hasValidSnapshot) snapshotBuffer[1] else viewportRows
        if (hasValidSnapshot) {
            val displayOffset = snapshotBuffer[6]
            val historySize = snapshotBuffer[7]
            val modeBits = snapshotBuffer[8]
            val isLocalScrollback =
                (modeBits and (FalconNative.MODE_BIT_ALT_SCREEN or FalconNative.MODE_BIT_MOUSE_ACTIVE)) == 0
            if (isLocalScrollback) {
                if ((deltaY > 0f && displayOffset <= 0) ||
                    (deltaY < 0f && displayOffset >= historySize)
                ) {
                    return true
                }
            } else if (isFling && (modeBits and FalconNative.MODE_BIT_ALT_SCREEN) != 0 &&
                (modeBits and FalconNative.MODE_BIT_MOUSE_ACTIVE) == 0
            ) {
                // 备用屏转上下方向键（如 shell 命令历史 / vim）不执行手指抬起后的惯性连发，避免误触一串 ↑/↓
                return true
            }
        }
        val (row, col, _) = screenToCell(touchX, touchY, cols, rows)
        val res = FalconNative.onScrollGesture(
            handle,
            deltaY.toDouble(),
            (cellHeight * activeScale).toDouble(),
            row,
            col
        )
        if (res == "SCROLL") {
            postInvalidateOnAnimation()
        } else if (res.isNotEmpty()) {
            onSendWsMessages?.invoke(listOf(res))
        }
        return false
    }

    private fun scrollByScrollbarTouch(touchY: Float) {
        val handle = sessionHandle
        if (handle == 0L || !hasValidSnapshot) return
        val rows = snapshotBuffer[1]
        val historySize = snapshotBuffer[7]
        if (historySize <= 0 || rows <= 0) return

        val totalLines = (historySize + rows).toFloat()
        val thumbRatio = (rows.toFloat() / totalLines).coerceIn(0.05f, 1f)
        val trackH = max(1f, height.toFloat() - paddingY * 2f)
        val thumbH = max(24f * density, trackH * thumbRatio).coerceAtMost(trackH)
        val span = trackH - thumbH
        val targetOffset = if (span <= 0f) {
            0
        } else {
            val topPx = (touchY - paddingY - thumbH * 0.5f).coerceIn(0f, span)
            val above = ((topPx / span) * historySize.toFloat()).roundToInt().coerceIn(0, historySize)
            historySize - above
        }
        if (FalconNative.scrollToOffset(handle, targetOffset)) {
            postInvalidateOnAnimation()
        }
    }

    private val gestureDetector = GestureDetector(
        context,
        object : GestureDetector.SimpleOnGestureListener() {
            override fun onSingleTapUp(e: MotionEvent): Boolean {
                if (sessionHandle != 0L) {
                    // 清空已有选区
                    val existing = FalconNative.selectionText(sessionHandle)
                    if (existing.isNotEmpty()) {
                        FalconNative.clearSelection(sessionHandle)
                        onSelectionChanged?.invoke(null)
                        invalidate()
                        return true
                    }
                    // 若程序开启了鼠标上报，发送按下与松开报文
                    if (hasValidSnapshot) {
                        val cols = snapshotBuffer[0]
                        val rows = snapshotBuffer[1]
                        val modeBits = snapshotBuffer[8]
                        if ((modeBits and FalconNative.MODE_BIT_MOUSE_ACTIVE) != 0) {
                            val (row, col, _) = screenToCell(e.x, e.y, cols, rows)
                            val px = (e.x - paddingX).toInt().coerceAtLeast(0)
                            val py = (e.y - paddingY).toInt().coerceAtLeast(0)
                            val downMsg = FalconNative.onMouseEvent(sessionHandle, 0, row, col, px, py)
                            val upMsg = FalconNative.onMouseEvent(sessionHandle, 2, row, col, px, py)
                            val msgs = listOf(downMsg, upMsg).filter { it.isNotEmpty() }
                            if (msgs.isNotEmpty()) {
                                onSendWsMessages?.invoke(msgs)
                            }
                        }
                    }
                }
                showSoftKeyboard()
                return true
            }

            override fun onScroll(
                e1: MotionEvent?,
                e2: MotionEvent,
                distanceX: Float,
                distanceY: Float
            ): Boolean {
                if (sessionHandle == 0L ||
                    isSelectingText ||
                    isDraggingScrollbar ||
                    multiTouchActive ||
                    scaleDetector.isInProgress
                ) {
                    return false
                }
                if (abs(distanceY) < abs(distanceX)) {
                    return false
                }
                parent?.requestDisallowInterceptTouchEvent(true)
                lastTouchX = e2.x
                lastTouchY = e2.y
                // distanceY > 0 为手指向上推（画面向下滚），distanceY < 0 为手指向下拖（向历史方向滚）
                dispatchScrollDelta(distanceY, e2.x, e2.y, isFling = false)
                return true
            }

            override fun onFling(
                e1: MotionEvent?,
                e2: MotionEvent,
                velocityX: Float,
                velocityY: Float
            ): Boolean {
                if (sessionHandle == 0L ||
                    isSelectingText ||
                    isDraggingScrollbar ||
                    multiTouchActive ||
                    scaleDetector.isInProgress
                ) {
                    return false
                }
                if (abs(velocityY) <= abs(velocityX)) {
                    return false
                }
                lastFlingY = 0
                lastTouchX = e2.x
                lastTouchY = e2.y
                scroller.fling(
                    0,
                    0,
                    0,
                    (-velocityY).roundToInt(),
                    0,
                    0,
                    -Int.MAX_VALUE / 4,
                    Int.MAX_VALUE / 4
                )
                postInvalidateOnAnimation()
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (sessionHandle == 0L || !hasValidSnapshot || isDraggingScrollbar || multiTouchActive) return
                val cols = snapshotBuffer[0]
                val rows = snapshotBuffer[1]
                val (row, col, rightHalf) = screenToCell(e.x, e.y, cols, rows)
                isSelectingText = true
                // 长按先按词（Semantic = 1）起选区，拖动时实时扩展
                FalconNative.startSelection(sessionHandle, 1, row, col, rightHalf)
                val sel = FalconNative.selectionText(sessionHandle).ifEmpty { null }
                onSelectionChanged?.invoke(sel)
                parent?.requestDisallowInterceptTouchEvent(true)
                invalidate()
            }
        }
    )

    override fun computeScroll() {
        super.computeScroll()
        if (scroller.computeScrollOffset()) {
            val currY = scroller.currY
            val dy = (currY - lastFlingY).toFloat()
            lastFlingY = currY
            if (dy != 0f && sessionHandle != 0L) {
                val hitBoundary = dispatchScrollDelta(dy, lastTouchX, lastTouchY, isFling = true)
                if (hitBoundary) {
                    scroller.forceFinished(true)
                    return
                }
            }
            if (!scroller.isFinished) {
                postInvalidateOnAnimation()
            }
        }
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                multiTouchActive = false
                if (!scroller.isFinished) {
                    scroller.forceFinished(true)
                }
                if (sessionHandle != 0L) {
                    FalconNative.resetScrollGesture(sessionHandle)
                }
                lastTouchX = event.x
                lastTouchY = event.y
                // 右侧历史滚动条拖拽热区（当处于历史翻阅状态且按在右边缘 28dp 内时可直接拖拽滑块）
                if (hasValidSnapshot &&
                    snapshotBuffer[7] > 0 &&
                    snapshotBuffer[6] > 0 &&
                    event.x >= width - 28f * density
                ) {
                    isDraggingScrollbar = true
                    parent?.requestDisallowInterceptTouchEvent(true)
                    scrollByScrollbarTouch(event.y)
                    return true
                }
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                multiTouchActive = true
                isDraggingScrollbar = false
                scroller.forceFinished(true)
                if (sessionHandle != 0L) {
                    FalconNative.resetScrollGesture(sessionHandle)
                }
            }
            MotionEvent.ACTION_POINTER_UP -> {
                scroller.forceFinished(true)
                if (sessionHandle != 0L) {
                    FalconNative.resetScrollGesture(sessionHandle)
                }
                if (event.pointerCount <= 2) {
                    multiTouchActive = false
                }
            }
        }

        if (isDraggingScrollbar) {
            when (event.actionMasked) {
                MotionEvent.ACTION_MOVE -> {
                    scrollByScrollbarTouch(event.y)
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    isDraggingScrollbar = false
                    parent?.requestDisallowInterceptTouchEvent(false)
                }
            }
            return true
        }

        scaleDetector.onTouchEvent(event)
        gestureDetector.onTouchEvent(event)

        if (isSelectingText && sessionHandle != 0L && hasValidSnapshot) {
            val cols = snapshotBuffer[0]
            val rows = snapshotBuffer[1]
            when (event.actionMasked) {
                MotionEvent.ACTION_MOVE -> {
                    val (row, col, rightHalf) = screenToCell(event.x, event.y, cols, rows)
                    FalconNative.updateSelection(sessionHandle, row, col, rightHalf)
                    val sel = FalconNative.selectionText(sessionHandle).ifEmpty { null }
                    onSelectionChanged?.invoke(sel)
                    invalidate()
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    isSelectingText = false
                    parent?.requestDisallowInterceptTouchEvent(false)
                    val sel = FalconNative.selectionText(sessionHandle).ifEmpty { null }
                    onSelectionChanged?.invoke(sel)
                }
            }
            return true
        }
        return true
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val handle = sessionHandle
        if (handle == 0L) return

        var seqOrNeed = FalconNative.packRenderSnapshotInto(handle, lastSnapshotSeq, snapshotBuffer)
        if (seqOrNeed < 0L) {
            val needed = (-seqOrNeed).toInt()
            snapshotBuffer = IntArray(max(needed, snapshotBuffer.size * 2))
            seqOrNeed = FalconNative.packRenderSnapshotInto(handle, 0L, snapshotBuffer)
        }
        if (seqOrNeed <= 0L) return
        lastSnapshotSeq = seqOrNeed
        hasValidSnapshot = true
        val snap = snapshotBuffer

        val cols = snap[0]
        val rows = snap[1]
        val cursorRow = snap[2]
        val cursorCol = snap[3]
        val cursorShape = snap[4]
        val cursorWide = snap[5] != 0
        val displayOffset = snap[6]
        val historySize = snap[7]
        val defaultBgArgb = snap[9]
        val cursorColorArgb = snap[11]
        val cursorTextArgb = snap[12]
        val isOwner = snap[14] != 0

        val isScrolledUp = displayOffset > 0
        if (isScrolledUp != lastReportedScrolledUp ||
            isOwner != lastReportedOwner ||
            cols != lastReportedGridCols ||
            rows != lastReportedGridRows
        ) {
            lastReportedScrolledUp = isScrolledUp
            lastReportedOwner = isOwner
            lastReportedGridCols = cols
            lastReportedGridRows = rows
            onRenderStateUpdated?.invoke(isScrolledUp, isOwner, cols, rows)
        }

        // 先铺终端默认底色
        bgPaint.color = defaultBgArgb
        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), bgPaint)

        val gridPixelW = cols * cellWidth
        val gridPixelH = rows * cellHeight
        val availW = max(1f, width - paddingX * 2f)
        val availH = max(1f, height - paddingY * 2f)

        // ADR 0022：当尺寸由其它端决定（!isOwner）且比当前视口大时，最多缩放到 0.5 倍并平移保证光标可见
        if (!isOwner && (gridPixelW > availW + 1f || gridPixelH > availH + 1f)) {
            val fitScale = min(availW / gridPixelW, availH / gridPixelH)
            activeScale = max(0.5f, min(1f, fitScale))
            val scaledW = gridPixelW * activeScale
            val scaledH = gridPixelH * activeScale
            activePanX = if (scaledW > availW && cursorCol >= 0) {
                val cursorCenterX = (cursorCol + 0.5f) * cellWidth * activeScale
                (cursorCenterX - availW * 0.5f).coerceIn(0f, scaledW - availW)
            } else {
                0f
            }
            activePanY = if (scaledH > availH && cursorRow >= 0) {
                val cursorCenterY = (cursorRow + 0.5f) * cellHeight * activeScale
                (cursorCenterY - availH * 0.5f).coerceIn(0f, scaledH - availH)
            } else {
                0f
            }
        } else {
            activeScale = 1f
            activePanX = 0f
            activePanY = 0f
        }

        // 若 !isOwner 且网格小于视口，网格外部铺窗口底色
        if (!isOwner) {
            val endX = paddingX + gridPixelW * activeScale
            val endY = paddingY + gridPixelH * activeScale
            bgPaint.color = outsideAreaColor
            if (endX < width) {
                canvas.drawRect(endX, 0f, width.toFloat(), height.toFloat(), bgPaint)
            }
            if (endY < height) {
                canvas.drawRect(0f, endY, width.toFloat(), height.toFloat(), bgPaint)
            }
        }

        canvas.save()
        canvas.translate(paddingX - activePanX, paddingY - activePanY)
        if (activeScale != 1f) {
            canvas.scale(activeScale, activeScale)
        }

        val headerOffset = FalconNative.SNAPSHOT_HEADER_INTS
        val cellStride = FalconNative.SNAPSHOT_CELL_INTS

        // 1. 绘制非默认背景色矩形（同行相邻同色合并）
        for (r in 0 until rows) {
            val top = r * cellHeight
            val bottom = top + cellHeight
            var runStartCol = -1
            var runBgColor = 0
            val rowCellOffset = headerOffset + r * cols * cellStride
            for (c in 0 until cols) {
                val base = rowCellOffset + c * cellStride
                val bg = snap[base + 2]
                val flags = snap[base + 3]
                val isDefaultBg = (flags and FalconNative.CELL_FLAG_DEFAULT_BG) != 0
                if (!isDefaultBg) {
                    if (runStartCol == -1) {
                        runStartCol = c
                        runBgColor = bg
                    } else if (bg != runBgColor) {
                        bgPaint.color = runBgColor
                        canvas.drawRect(
                            runStartCol * cellWidth,
                            top,
                            c * cellWidth,
                            bottom,
                            bgPaint
                        )
                        runStartCol = c
                        runBgColor = bg
                    }
                } else if (runStartCol != -1) {
                    bgPaint.color = runBgColor
                    canvas.drawRect(
                        runStartCol * cellWidth,
                        top,
                        c * cellWidth,
                        bottom,
                        bgPaint
                    )
                    runStartCol = -1
                }
            }
            if (runStartCol != -1) {
                bgPaint.color = runBgColor
                canvas.drawRect(
                    runStartCol * cellWidth,
                    top,
                    cols * cellWidth,
                    bottom,
                    bgPaint
                )
            }
        }

        if (rowCharBuffer.size < cols * 2) {
            rowCharBuffer = CharArray(max(cols * 2, 512))
        }
        val lineBuf = rowCharBuffer
        val styleMask = FalconNative.CELL_FLAG_BOLD or FalconNative.CELL_FLAG_ITALIC

        // 2. 绘制字符、块元素与修饰线（同行连续同样式 ASCII 字符合并为单次 drawText 调用）
        for (r in 0 until rows) {
            val top = r * cellHeight
            val baseline = top + fontBaselineOffset
            val rowCellOffset = headerOffset + r * cols * cellStride

            var asciiRunStartCol = -1
            var asciiRunLen = 0
            var asciiRunFg = 0
            var asciiRunStyle = 0
            var pendingSpaces = 0

            for (c in 0 until cols) {
                val base = rowCellOffset + c * cellStride
                val cp = snap[base]
                val fg = snap[base + 1]
                val flags = snap[base + 3]

                if ((flags and FalconNative.CELL_FLAG_WIDE_CHAR_SPACER) != 0) continue
                if ((flags and FalconNative.CELL_FLAG_HIDDEN) != 0) {
                    if (asciiRunStartCol != -1) {
                        pendingSpaces++
                    }
                    continue
                }

                val left = c * cellWidth
                val spanCols = if ((flags and FalconNative.CELL_FLAG_WIDE_CHAR) != 0) 2 else 1
                val right = left + spanCols * cellWidth

                // 修饰线（下划线 / 删除线）
                if ((flags and FalconNative.CELL_FLAG_UNDERLINE) != 0) {
                    bgPaint.color = fg
                    val uTop = top + cellHeight - max(1.5f, density)
                    canvas.drawRect(left, uTop, right, top + cellHeight, bgPaint)
                }
                if ((flags and FalconNative.CELL_FLAG_STRIKEOUT) != 0) {
                    bgPaint.color = fg
                    val sMid = top + cellHeight * 0.5f
                    canvas.drawRect(
                        left,
                        sMid - max(1f, density * 0.5f),
                        right,
                        sMid + max(1f, density * 0.5f),
                        bgPaint
                    )
                }

                if (cp <= 32) {
                    if (asciiRunStartCol != -1) {
                        pendingSpaces++
                    }
                    continue
                }

                val style = flags and styleMask
                // 可打印 ASCII（0x21..0x7E）：合并同行相邻同样式字符（允许中间夹 <= 4 个空格）
                if (cp in 0x21..0x7E) {
                    if (asciiRunStartCol != -1 &&
                        fg == asciiRunFg &&
                        style == asciiRunStyle &&
                        pendingSpaces <= 4
                    ) {
                        while (pendingSpaces > 0) {
                            lineBuf[asciiRunLen++] = ' '
                            pendingSpaces--
                        }
                        lineBuf[asciiRunLen++] = cp.toChar()
                    } else {
                        if (asciiRunStartCol != -1) {
                            applyTextPaint(asciiRunStyle, asciiRunFg)
                            canvas.drawText(
                                lineBuf,
                                0,
                                asciiRunLen,
                                asciiRunStartCol * cellWidth,
                                baseline,
                                textPaint
                            )
                        }
                        asciiRunStartCol = c
                        asciiRunLen = 1
                        asciiRunFg = fg
                        asciiRunStyle = style
                        pendingSpaces = 0
                        lineBuf[0] = cp.toChar()
                    }
                    continue
                }

                // 非 ASCII 字符或块元素：先冲刷已积累的 ASCII 连续段，再按精确列坐标独立绘制
                if (asciiRunStartCol != -1) {
                    applyTextPaint(asciiRunStyle, asciiRunFg)
                    canvas.drawText(
                        lineBuf,
                        0,
                        asciiRunLen,
                        asciiRunStartCol * cellWidth,
                        baseline,
                        textPaint
                    )
                    asciiRunStartCol = -1
                    asciiRunLen = 0
                    pendingSpaces = 0
                }

                // 常见块元素字符用几何矩形直接绘制，避免字形留缝
                if (drawBlockElement(canvas, cp, left, top, cellWidth * spanCols, cellHeight, fg)) {
                    continue
                }

                applyTextPaint(style, fg)
                val charCount = Character.toChars(cp, charBuffer, 0)
                canvas.drawText(charBuffer, 0, charCount, left, baseline, textPaint)
            }

            if (asciiRunStartCol != -1) {
                applyTextPaint(asciiRunStyle, asciiRunFg)
                canvas.drawText(
                    lineBuf,
                    0,
                    asciiRunLen,
                    asciiRunStartCol * cellWidth,
                    baseline,
                    textPaint
                )
            }
        }

        // 3. 绘制光标（历史偏移为 0 时光标才在可见区）
        if (cursorRow in 0 until rows && cursorCol in 0 until cols && cursorShape >= 0) {
            val cLeft = cursorCol * cellWidth
            val cTop = cursorRow * cellHeight
            val cSpan = if (cursorWide) 2 else 1
            val cWidth = cSpan * cellWidth
            rectBuffer.set(cLeft, cTop, cLeft + cWidth, cTop + cellHeight)

            val effectiveShape = if (!hasFocus() && cursorShape == 0) 3 else cursorShape
            when (effectiveShape) {
                0 -> {
                    // 实心块光标 + 反色绘制其上的字符
                    cursorPaint.style = Paint.Style.FILL
                    cursorPaint.color = cursorColorArgb
                    canvas.drawRect(rectBuffer, cursorPaint)

                    val base = headerOffset + (cursorRow * cols + cursorCol) * cellStride
                    val cp = snap[base]
                    if (cp > 32) {
                        applyTextPaint(0, cursorTextArgb)
                        val charCount = Character.toChars(cp, charBuffer, 0)
                        canvas.drawText(
                            charBuffer,
                            0,
                            charCount,
                            cLeft,
                            cTop + fontBaselineOffset,
                            textPaint
                        )
                    }
                }
                1 -> {
                    // 下划线光标
                    cursorPaint.style = Paint.Style.FILL
                    cursorPaint.color = cursorColorArgb
                    val h = max(2f * density, cellHeight * 0.12f)
                    canvas.drawRect(cLeft, cTop + cellHeight - h, cLeft + cWidth, cTop + cellHeight, cursorPaint)
                }
                2 -> {
                    // 竖线光标（Beam）
                    cursorPaint.style = Paint.Style.FILL
                    cursorPaint.color = cursorColorArgb
                    val w = max(2f * density, cellWidth * 0.18f)
                    canvas.drawRect(cLeft, cTop, cLeft + w, cTop + cellHeight, cursorPaint)
                }
                else -> {
                    // 空心框光标（失焦或 HollowBlock）
                    cursorPaint.style = Paint.Style.STROKE
                    cursorPaint.strokeWidth = max(1.5f, density)
                    cursorPaint.color = cursorColorArgb
                    canvas.drawRect(rectBuffer, cursorPaint)
                }
            }
        }

        canvas.restore()

        // 4. 历史滚动条指示器
        if (historySize > 0 && displayOffset > 0) {
            val totalLines = (historySize + rows).toFloat()
            val thumbRatio = (rows.toFloat() / totalLines).coerceIn(0.05f, 1f)
            val trackH = height.toFloat() - paddingY * 2f
            val thumbH = max(24f * density, trackH * thumbRatio)
            val progressFromTop =
                ((historySize - displayOffset).toFloat() / historySize.toFloat()).coerceIn(0f, 1f)
            val thumbTop = paddingY + (trackH - thumbH) * progressFromTop
            val barW = if (isDraggingScrollbar) 5.5f * density else 3.5f * density
            canvas.drawRoundRect(
                width - barW - 2f * density,
                thumbTop,
                width - 2f * density,
                thumbTop + thumbH,
                barW,
                barW,
                scrollbarPaint
            )
        }
    }

    private fun drawBlockElement(
        canvas: Canvas,
        cp: Int,
        left: Float,
        top: Float,
        w: Float,
        h: Float,
        fgArgb: Int
    ): Boolean {
        when (cp) {
            0x2580 -> { // ▀ 上半块
                bgPaint.color = fgArgb
                canvas.drawRect(left, top, left + w, top + h * 0.5f, bgPaint)
                return true
            }
            in 0x2581..0x2588 -> { // ▁..█ 下部八分块
                val ratio = (cp - 0x2580) / 8f
                bgPaint.color = fgArgb
                canvas.drawRect(left, top + h * (1f - ratio), left + w, top + h, bgPaint)
                return true
            }
            in 0x2589..0x258F -> { // ▉..▏ 左部八分块
                val ratio = (0x2590 - cp) / 8f
                bgPaint.color = fgArgb
                canvas.drawRect(left, top, left + w * ratio, top + h, bgPaint)
                return true
            }
            0x2590 -> { // ▐ 右半块
                bgPaint.color = fgArgb
                canvas.drawRect(left + w * 0.5f, top, left + w, top + h, bgPaint)
                return true
            }
            0x2591, 0x2592, 0x2593 -> { // ░ ▒ ▓ 阴影块
                val alphaFactor = when (cp) {
                    0x2591 -> 0.25f
                    0x2592 -> 0.50f
                    else -> 0.75f
                }
                val origAlpha = (fgArgb ushr 24) and 0xFF
                val newAlpha = (origAlpha * alphaFactor).toInt().coerceIn(0, 255)
                bgPaint.color = (newAlpha shl 24) or (fgArgb and 0x00FFFFFF)
                canvas.drawRect(left, top, left + w, top + h, bgPaint)
                return true
            }
        }
        return false
    }
}
