package com.falcon.android.net

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream

class AppUpdatesTest {
    private val digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    private val release = AndroidRelease("0.10.0", "https://github.com/Lee94/falcon/releases/download/v0.10.0/falcon-v0.10.0-android.apk", 3, digest)
    private fun metadata() = JSONObject("""{"tag_name":"v0.10.0","draft":false,"prerelease":false,"assets":[
        {"name":"falcon-v0.10.0-android.apk","size":3,"digest":"sha256:$digest","browser_download_url":"${release.url}"}
    ]}""")

    @Test fun selectsOnlyNewerStableReleaseFromOfficialRepo() {
        assertEquals(release, newerAndroidRelease(metadata(), "0.9.99"))
        assertNull(newerAndroidRelease(metadata(), "0.10.0"))
        assertNull(newerAndroidRelease(metadata(), "1.0.0"))
        assertThrows(IllegalArgumentException::class.java) { newerAndroidRelease(metadata().put("prerelease", true), "0.9.0") }
        val wrongUrl = metadata()
        wrongUrl.getJSONArray("assets").getJSONObject(0).put("browser_download_url", "https://example.com/app.apk")
        assertThrows(IllegalArgumentException::class.java) { newerAndroidRelease(wrongUrl, "0.9.0") }
    }

    @Test fun verifiesBytesBeforeInstallation() {
        val out = ByteArrayOutputStream()
        copyVerified("abc".byteInputStream(), out, release) {}
        assertEquals("abc", out.toString("UTF-8"))
        for (invalid in listOf("ab", "abcd", "abd")) {
            assertThrows(IllegalArgumentException::class.java) {
                copyVerified(invalid.byteInputStream(), ByteArrayOutputStream(), release) {}
            }
        }
    }
}
