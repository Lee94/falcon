plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.google.vtfx.compose"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
    buildFeatures { compose = true }
}

dependencies {
    api(project(":vtfx-core"))
    api(project(":vtfx-gate"))
    api(project(":vtfx-session"))
    api(project(":vtfx-gate-android"))
    api(project(":vtfx-font"))
    api(project(":vtfx-font-gate"))
    implementation(project(":vtfx-font-gate-android"))
    implementation(project(":vtfx-input"))
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.ui:ui")
}
