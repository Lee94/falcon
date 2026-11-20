plugins {
    id("com.android.library")
}

android {
    namespace = "com.google.vtfx.gate.android"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
}

dependencies {
    api(project(":vtfx-gate"))
}
