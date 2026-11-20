plugins {
    id("com.android.library")
}

android {
    namespace = "com.google.vtfx.font.gate.android"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
}

dependencies {
    compileOnly(project(":vtfx-core"))
    api(project(":vtfx-font-gate"))
}
