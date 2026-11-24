package com.falcon.android.net

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.FileOutputStream
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

class TailnetLibraryLookupTest {
    @get:Rule
    val tempFolder = TemporaryFolder()

    private fun createFakeApk(name: String, entries: List<String>): File {
        val apk = File(tempFolder.root, name)
        ZipOutputStream(FileOutputStream(apk)).use { zos ->
            for (entry in entries) {
                zos.putNextEntry(ZipEntry(entry))
                zos.write(byteArrayOf(0x7f, 'E'.code.toByte(), 'L'.code.toByte(), 'F'.code.toByte()))
                zos.closeEntry()
            }
        }
        return apk
    }

    @Test
    fun findsUnextractedLibraryInsideApkWhenNativeDirIsEmpty() {
        val emptyNativeDir = tempFolder.newFolder("empty-native-lib-dir")
        val apk = createFakeApk(
            "base.apk",
            listOf(
                "lib/arm64-v8a/libfalcon_android.so",
                "lib/arm64-v8a/libfalcon_tailnet.so"
            )
        )

        val resolved = findTailnetLibraryInPaths(
            nativeLibraryDir = emptyNativeDir.absolutePath,
            apkPaths = listOf(apk.absolutePath),
            abis = listOf("arm64-v8a")
        )

        assertEquals("${apk.absolutePath}!/lib/arm64-v8a/libfalcon_tailnet.so", resolved)
    }

    @Test
    fun prefersExtractedLibraryInNativeDirWhenPresent() {
        val nativeDir = tempFolder.newFolder("legacy-native-lib-dir")
        val extracted = File(nativeDir, "libfalcon_tailnet.so").apply { writeBytes(byteArrayOf(1, 2, 3, 4)) }
        val apk = createFakeApk("base.apk", listOf("lib/arm64-v8a/libfalcon_tailnet.so"))

        val resolved = findTailnetLibraryInPaths(
            nativeLibraryDir = nativeDir.absolutePath,
            apkPaths = listOf(apk.absolutePath),
            abis = listOf("arm64-v8a")
        )

        assertEquals(extracted.absolutePath, resolved)
    }

    @Test
    fun returnsNullWhenAbiDoesNotMatchOrLibraryIsAbsent() {
        val emptyNativeDir = tempFolder.newFolder("missing-native-lib-dir")
        val arm64OnlyApk = createFakeApk("arm64.apk", listOf("lib/arm64-v8a/libfalcon_tailnet.so"))
        val withoutTailnetApk = createFakeApk("dev.apk", listOf("lib/arm64-v8a/libfalcon_android.so"))

        assertNull(
            findTailnetLibraryInPaths(
                nativeLibraryDir = emptyNativeDir.absolutePath,
                apkPaths = listOf(arm64OnlyApk.absolutePath),
                abis = listOf("x86_64")
            )
        )
        assertNull(
            findTailnetLibraryInPaths(
                nativeLibraryDir = emptyNativeDir.absolutePath,
                apkPaths = listOf(withoutTailnetApk.absolutePath),
                abis = listOf("arm64-v8a")
            )
        )
    }
}
