package com.falcon.android.ui

import android.content.Context
import android.graphics.Typeface
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import com.falcon.android.bridge.FalconNative
import org.json.JSONObject
import kotlin.math.abs

@Immutable
data class FalconThemeTokens(
    val name: String,
    val isDark: Boolean,
    val appearance: String,
    val backgroundHex: String,
    val foregroundHex: String,
    val app: Color,
    val background: Color,
    val foreground: Color,
    val card: Color,
    val cardForeground: Color,
    val primary: Color,
    val primaryForeground: Color,
    val secondary: Color,
    val secondaryForeground: Color,
    val muted: Color,
    val mutedForeground: Color,
    val accent: Color,
    val accentForeground: Color,
    val tint: Color,
    val tintForeground: Color,
    val tintStrong: Color,
    val destructive: Color,
    val success: Color,
    val warning: Color,
    val sidebar: Color,
    val sidebarForeground: Color,
    val terminalBackground: Color,
    val terminalForeground: Color
)

fun parseHexColor(hex: String, fallback: Color = Color(0xFF0A0A0A)): Color {
    val clean = hex.trim().removePrefix("#")
    return try {
        when (clean.length) {
            6 -> Color(0xFF000000 or clean.toLong(16))
            8 -> {
                val rrggbb = clean.substring(0, 6).toLong(16)
                val aa = clean.substring(6, 8).toLong(16)
                Color((aa shl 24) or rrggbb)
            }
            else -> fallback
        }
    } catch (_: Exception) {
        fallback
    }
}

/**
 * 与桌面端 `falcon_core::host_color::host_hue` 逐位一致的 FNV-1a UTF-16 哈希，
 * 确保同一台 SSH 主机在桌面端与安卓端呈现相同的身份色条。
 */
private val HOST_HUE_BANDS = intArrayOf(30, 50, 170, 190, 210, 230, 250, 270, 290, 310, 330)

fun hostHue(key: String): Int {
    if (key.isEmpty()) {
        return HOST_HUE_BANDS[(2_166_136_261L % HOST_HUE_BANDS.size).toInt()]
    }
    var h: Int = 2_166_136_261L.toInt()
    for (i in 0 until key.length) {
        h = h xor key[i].code
        h *= 16_777_619
    }
    val absVal = abs(h.toLong())
    return HOST_HUE_BANDS[(absVal % HOST_HUE_BANDS.size).toInt()]
}

fun FalconThemeTokens.hostBarColor(kind: String, conn: String?): Color {
    if (kind == "local") return mutedForeground.copy(alpha = 0.35f)
    if (conn.isNullOrBlank()) return muted
    val hue = hostHue(conn).toFloat()
    val sat = if (isDark) 0.62f else 0.58f
    val light = if (isDark) 0.62f else 0.46f
    return Color.hsl(hue, sat, light)
}

fun loadFalconThemeTokens(themeName: String): FalconThemeTokens {
    val raw = FalconNative.exportThemeTokens(themeName)
    val obj = JSONObject(raw)
    val ui = obj.getJSONObject("ui")
    val term = obj.getJSONObject("terminal")
    return FalconThemeTokens(
        name = obj.optString("name", "Falcon Dark"),
        isDark = obj.optBoolean("isDark", true),
        appearance = obj.optString("appearance", "dark"),
        backgroundHex = obj.optString("backgroundHex", "#0a0a0a"),
        foregroundHex = obj.optString("foregroundHex", "#fafafa"),
        app = parseHexColor(ui.getString("app")),
        background = parseHexColor(ui.getString("background")),
        foreground = parseHexColor(ui.getString("foreground")),
        card = parseHexColor(ui.getString("card")),
        cardForeground = parseHexColor(ui.getString("cardForeground")),
        primary = parseHexColor(ui.getString("primary")),
        primaryForeground = parseHexColor(ui.getString("primaryForeground")),
        secondary = parseHexColor(ui.getString("secondary")),
        secondaryForeground = parseHexColor(ui.getString("secondaryForeground")),
        muted = parseHexColor(ui.getString("muted")),
        mutedForeground = parseHexColor(ui.getString("mutedForeground")),
        accent = parseHexColor(ui.getString("accent")),
        accentForeground = parseHexColor(ui.getString("accentForeground")),
        tint = parseHexColor(ui.getString("tint")),
        tintForeground = parseHexColor(ui.getString("tintForeground")),
        tintStrong = parseHexColor(ui.getString("tintStrong")),
        destructive = parseHexColor(ui.getString("destructive")),
        success = parseHexColor(ui.getString("success")),
        warning = parseHexColor(ui.getString("warning")),
        sidebar = parseHexColor(ui.getString("sidebar")),
        sidebarForeground = parseHexColor(ui.getString("sidebarForeground")),
        terminalBackground = parseHexColor(term.getString("background")),
        terminalForeground = parseHexColor(term.getString("foreground"))
    )
}

val LocalFalconTheme = staticCompositionLocalOf {
    loadFalconThemeTokens("Falcon Dark")
}

val LocalFalconMonoFont = staticCompositionLocalOf<FontFamily> {
    FontFamily.Monospace
}

private fun loadBerkeleyMonoFontFamily(context: Context): FontFamily {
    return try {
        val tf = Typeface.createFromAsset(context.assets, "fonts/TX-02-Regular.ttf")
        FontFamily(tf)
    } catch (_: Exception) {
        FontFamily.Monospace
    }
}

@Composable
fun FalconTheme(
    themeName: String,
    content: @Composable () -> Unit
) {
    val context = LocalContext.current
    val tokens = remember(themeName) { loadFalconThemeTokens(themeName) }
    val monoFont = remember(context) { loadBerkeleyMonoFontFamily(context) }
    val colorScheme = if (tokens.isDark) {
        darkColorScheme(
            primary = tokens.tintStrong,
            onPrimary = tokens.primaryForeground,
            secondary = tokens.secondary,
            onSecondary = tokens.secondaryForeground,
            background = tokens.app,
            onBackground = tokens.foreground,
            surface = tokens.background,
            onSurface = tokens.foreground,
            surfaceVariant = tokens.card,
            onSurfaceVariant = tokens.mutedForeground,
            error = tokens.destructive
        )
    } else {
        lightColorScheme(
            primary = tokens.tintStrong,
            onPrimary = tokens.primaryForeground,
            secondary = tokens.secondary,
            onSecondary = tokens.secondaryForeground,
            background = tokens.app,
            onBackground = tokens.foreground,
            surface = tokens.background,
            onSurface = tokens.foreground,
            surfaceVariant = tokens.card,
            onSurfaceVariant = tokens.mutedForeground,
            error = tokens.destructive
        )
    }

    CompositionLocalProvider(
        LocalFalconTheme provides tokens,
        LocalFalconMonoFont provides monoFont
    ) {
        MaterialTheme(
            colorScheme = colorScheme,
            content = content
        )
    }
}
